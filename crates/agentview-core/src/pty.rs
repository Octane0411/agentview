use crate::codex::hosted_session_config;
use crate::schema::Job;
use crate::store::{append_job_event, job_dir, require_job};
use crate::util::{now_iso, path_exists};
use agentview_codex_hosted::HostedHelper;
use anyhow::{Context, Result, bail};
use serde_json::json;
use std::ffi::CStr;
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Write};
use std::mem::MaybeUninit;
use std::os::fd::{AsRawFd, FromRawFd, RawFd};
use std::os::unix::net::{UnixListener, UnixStream};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const DETACH_SEQUENCE: &[u8] = b"\x1b]777;agentview-detach\x07";
const BUFFER_LIMIT: usize = 96 * 1024;
const ATTACH_HEADER_PREFIX: &str = "AGENTVIEW_PTY_ATTACH ";
const PING_HEADER: &str = "AGENTVIEW_PTY_PING";

#[derive(Debug, Clone, Copy)]
struct TerminalSize {
    rows: u16,
    cols: u16,
}

impl Default for TerminalSize {
    fn default() -> Self {
        Self { rows: 24, cols: 80 }
    }
}

struct ClientState {
    stream: Option<UnixStream>,
}

struct SharedState {
    client: Mutex<ClientState>,
    buffer: Mutex<Vec<u8>>,
}

pub fn persistent_tui_enabled() -> bool {
    std::env::var("AGENTVIEW_PERSISTENT_CODEX_TUI")
        .map(|value| value != "0" && value != "false")
        .unwrap_or(true)
}

pub fn prewarm_hosted_pty(job: &Job) -> Result<()> {
    ensure_hosted_pty_host(job)
}

pub fn attach_hosted_pty(job: &Job, no_alt_screen: bool) -> Result<i32> {
    if no_alt_screen {
        bail!("persistent hosted PTY attach requires the alternate screen");
    }
    ensure_hosted_pty_host(job)?;
    append_job_event(
        &job.id,
        &json!({
            "type": "hosted_pty_attach_started",
            "timestamp": now_iso()
        }),
    )?;
    let status = attach_client(&job.id);
    append_job_event(
        &job.id,
        &json!({
            "type": "hosted_pty_attach_finished",
            "ok": status.is_ok(),
            "timestamp": now_iso()
        }),
    )?;
    status.map(|()| 0)
}

pub fn hosted_pty_host_main(job_id: &str) -> Result<()> {
    let job = require_job(job_id)?;
    let socket_path = hosted_pty_socket_path(job_id);
    let pid_path = hosted_pty_pid_path(job_id);
    fs::create_dir_all(job_dir(job_id))?;
    bind_fresh_socket(&socket_path)?;
    fs::write(&pid_path, std::process::id().to_string())?;

    let listener = UnixListener::bind(&socket_path)
        .with_context(|| format!("failed to bind {}", socket_path.display()))?;
    listener.set_nonblocking(true)?;

    let pty = open_pty()?;
    let size = TerminalSize::default();
    set_pty_size_path(&pty.slave_path, size)?;
    let mut child = spawn_hosted_helper_in_pty(&job, &pty.slave_path)?;
    append_job_event(
        job_id,
        &json!({
            "type": "hosted_pty_started",
            "pid": child.id(),
            "socket": socket_path,
            "timestamp": now_iso()
        }),
    )?;

    let shared = Arc::new(SharedState {
        client: Mutex::new(ClientState { stream: None }),
        buffer: Mutex::new(Vec::new()),
    });
    let reader_shared = Arc::clone(&shared);
    let job_id_for_reader = job_id.to_string();
    let mut reader = pty.master.try_clone()?;
    let reader_thread = std::thread::spawn(move || {
        let _ = read_pty_output(&job_id_for_reader, &mut reader, reader_shared);
    });

    let result = accept_clients(
        job_id,
        &listener,
        &pty.slave_path,
        pty.master.as_raw_fd(),
        &mut child,
        &shared,
    );
    let _ = child.kill();
    let _ = reader_thread.join();
    let _ = fs::remove_file(&socket_path);
    let _ = fs::remove_file(&pid_path);
    result
}

fn ensure_hosted_pty_host(job: &Job) -> Result<()> {
    if hosted_pty_ping(&job.id) {
        return Ok(());
    }
    cleanup_stale_host_files(&job.id);

    let log_path = hosted_pty_log_path(&job.id);
    let log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .with_context(|| format!("failed to open {}", log_path.display()))?;
    let err = log.try_clone()?;
    let mut child = Command::new(agentview_binary()?);
    child
        .arg("__hosted-pty-host")
        .arg(&job.id)
        .stdin(Stdio::null())
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(err));
    unsafe {
        child.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let pid = child
        .spawn()
        .context("failed to spawn hosted PTY host")?
        .id();
    append_job_event(
        &job.id,
        &json!({
            "type": "hosted_pty_host_spawned",
            "pid": pid,
            "timestamp": now_iso()
        }),
    )?;

    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(5) {
        if hosted_pty_ping(&job.id) {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    bail!("timed out waiting for hosted PTY host to start")
}

fn hosted_pty_ping(job_id: &str) -> bool {
    let Ok(mut stream) = UnixStream::connect(hosted_pty_socket_path(job_id)) else {
        return false;
    };
    if stream
        .write_all(format!("{PING_HEADER}\n").as_bytes())
        .is_err()
    {
        return false;
    }
    let _ = stream.set_read_timeout(Some(Duration::from_millis(250)));
    let mut line = String::new();
    BufReader::new(stream)
        .read_line(&mut line)
        .map(|_| line.trim() == "OK")
        .unwrap_or(false)
}

fn attach_client(job_id: &str) -> Result<()> {
    let mut stream = UnixStream::connect(hosted_pty_socket_path(job_id))
        .context("failed to connect to hosted PTY")?;
    let size = terminal_size(std::io::stdout().as_raw_fd()).unwrap_or_default();
    stream.write_all(format!("{ATTACH_HEADER_PREFIX}{} {}\n", size.rows, size.cols).as_bytes())?;
    stream.flush()?;

    let _guard = RawTerminalGuard::new(std::io::stdin().as_raw_fd())?;
    relay_terminal(stream)
}

fn accept_clients(
    job_id: &str,
    listener: &UnixListener,
    slave_path: &Path,
    master_fd: RawFd,
    child: &mut Child,
    shared: &SharedState,
) -> Result<()> {
    loop {
        if let Some(status) = child.try_wait()? {
            append_job_event(
                job_id,
                &json!({
                    "type": "hosted_pty_exited",
                    "status": status.code(),
                    "timestamp": now_iso()
                }),
            )?;
            return Ok(());
        }
        match listener.accept() {
            Ok((stream, _)) => handle_client(job_id, stream, slave_path, master_fd, child, shared)?,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(error) => return Err(error.into()),
        }
    }
}

fn handle_client(
    job_id: &str,
    stream: UnixStream,
    slave_path: &Path,
    master_fd: RawFd,
    child: &Child,
    shared: &SharedState,
) -> Result<()> {
    let mut reader = BufReader::new(stream);
    let mut header = String::new();
    reader.read_line(&mut header)?;
    let mut stream = reader.into_inner();
    let header = header.trim();
    if header == PING_HEADER {
        stream.write_all(b"OK\n")?;
        return Ok(());
    }
    let Some(rest) = header.strip_prefix(ATTACH_HEADER_PREFIX) else {
        return Ok(());
    };
    let mut parts = rest.split_whitespace();
    let size = TerminalSize {
        rows: parts
            .next()
            .and_then(|value| value.parse().ok())
            .unwrap_or(24),
        cols: parts
            .next()
            .and_then(|value| value.parse().ok())
            .unwrap_or(80),
    };
    set_pty_size_path(slave_path, size)?;
    signal_window_change(child.id());

    {
        let buffer = shared.buffer.lock().expect("buffer lock poisoned");
        if !buffer.is_empty() {
            let _ = stream.write_all(&buffer);
            let _ = stream.flush();
        }
    }

    let client_writer = stream.try_clone()?;
    {
        let mut client = shared.client.lock().expect("client lock poisoned");
        if let Some(old) = client.stream.take() {
            let _ = old.shutdown(std::net::Shutdown::Both);
        }
        client.stream = Some(client_writer);
    }
    append_job_event(
        job_id,
        &json!({
            "type": "hosted_pty_client_attached",
            "timestamp": now_iso()
        }),
    )?;

    let master_dup = unsafe { libc::dup(master_fd) };
    if master_dup < 0 {
        return Err(std::io::Error::last_os_error()).context("failed to clone PTY master");
    }
    let mut master = unsafe { File::from_raw_fd(master_dup) };
    std::thread::spawn(move || {
        let mut input = stream;
        let mut buf = [0_u8; 4096];
        loop {
            match input.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    if master.write_all(&buf[..n]).is_err() {
                        break;
                    }
                    let _ = master.flush();
                }
                Err(_) => break,
            }
        }
    });
    Ok(())
}

fn read_pty_output(job_id: &str, master: &mut File, shared: Arc<SharedState>) -> Result<()> {
    let mut filter = DetachFilter::default();
    let mut chunk = [0_u8; 8192];
    loop {
        match master.read(&mut chunk) {
            Ok(0) => return Ok(()),
            Ok(n) => {
                let filtered = filter.push(&chunk[..n]);
                if !filtered.output.is_empty() {
                    append_buffer(shared.as_ref(), &filtered.output);
                    write_to_client(shared.as_ref(), &filtered.output);
                }
                if filtered.detach {
                    detach_client(job_id, shared.as_ref());
                }
            }
            Err(error) if error.raw_os_error() == Some(libc::EIO) => return Ok(()),
            Err(error) => return Err(error.into()),
        }
    }
}

fn append_buffer(shared: &SharedState, bytes: &[u8]) {
    let mut buffer = shared.buffer.lock().expect("buffer lock poisoned");
    buffer.extend_from_slice(bytes);
    if buffer.len() > BUFFER_LIMIT {
        let drain = buffer.len() - BUFFER_LIMIT;
        buffer.drain(0..drain);
    }
}

fn write_to_client(shared: &SharedState, bytes: &[u8]) {
    let mut client = shared.client.lock().expect("client lock poisoned");
    let Some(stream) = client.stream.as_mut() else {
        return;
    };
    if stream
        .write_all(bytes)
        .and_then(|_| stream.flush())
        .is_err()
    {
        client.stream = None;
    }
}

fn detach_client(job_id: &str, shared: &SharedState) {
    let mut client = shared.client.lock().expect("client lock poisoned");
    if let Some(stream) = client.stream.take() {
        let _ = stream.shutdown(std::net::Shutdown::Both);
    }
    let _ = append_job_event(
        job_id,
        &json!({
            "type": "hosted_pty_detached",
            "timestamp": now_iso()
        }),
    );
}

#[derive(Default)]
struct DetachFilter {
    pending: Vec<u8>,
}

struct FilteredOutput {
    output: Vec<u8>,
    detach: bool,
}

impl DetachFilter {
    fn push(&mut self, bytes: &[u8]) -> FilteredOutput {
        self.pending.extend_from_slice(bytes);
        let mut output = Vec::new();
        let mut detach = false;

        while let Some(index) = find_subsequence(&self.pending, DETACH_SEQUENCE) {
            output.extend_from_slice(&self.pending[..index]);
            self.pending.drain(..index + DETACH_SEQUENCE.len());
            detach = true;
        }

        let keep = detach_prefix_suffix_len(&self.pending);
        if self.pending.len() > keep {
            let emit = self.pending.len() - keep;
            output.extend_from_slice(&self.pending[..emit]);
            self.pending.drain(..emit);
        }

        FilteredOutput { output, detach }
    }
}

fn find_subsequence(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn detach_prefix_suffix_len(bytes: &[u8]) -> usize {
    let max = bytes.len().min(DETACH_SEQUENCE.len().saturating_sub(1));
    (1..=max)
        .rev()
        .find(|len| bytes[bytes.len() - len..] == DETACH_SEQUENCE[..*len])
        .unwrap_or(0)
}

fn relay_terminal(mut stream: UnixStream) -> Result<()> {
    let stdin_fd = std::io::stdin().as_raw_fd();
    let socket_fd = stream.as_raw_fd();
    let mut stdout = std::io::stdout();
    let mut stdin_buf = [0_u8; 4096];
    let mut socket_buf = [0_u8; 8192];

    loop {
        let mut fds = [
            libc::pollfd {
                fd: stdin_fd,
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: socket_fd,
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        let result = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, -1) };
        if result < 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error.into());
        }
        if fds[1].revents & (libc::POLLIN | libc::POLLHUP | libc::POLLERR) != 0 {
            match stream.read(&mut socket_buf) {
                Ok(0) => break,
                Ok(n) => {
                    stdout.write_all(&socket_buf[..n])?;
                    stdout.flush()?;
                }
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                Err(error) => return Err(error.into()),
            }
        }
        if fds[0].revents & libc::POLLIN != 0 {
            let n = unsafe {
                libc::read(
                    stdin_fd,
                    stdin_buf.as_mut_ptr().cast::<libc::c_void>(),
                    stdin_buf.len(),
                )
            };
            if n <= 0 {
                break;
            }
            stream.write_all(&stdin_buf[..n as usize])?;
            stream.flush()?;
        }
    }
    Ok(())
}

struct RawTerminalGuard {
    fd: RawFd,
    original: libc::termios,
}

impl RawTerminalGuard {
    fn new(fd: RawFd) -> Result<Self> {
        let mut original = MaybeUninit::<libc::termios>::uninit();
        if unsafe { libc::tcgetattr(fd, original.as_mut_ptr()) } != 0 {
            return Err(std::io::Error::last_os_error()).context("failed to read terminal mode");
        }
        let original = unsafe { original.assume_init() };
        let mut raw = original;
        unsafe { libc::cfmakeraw(&mut raw) };
        if unsafe { libc::tcsetattr(fd, libc::TCSANOW, &raw) } != 0 {
            return Err(std::io::Error::last_os_error()).context("failed to enter raw mode");
        }
        Ok(Self { fd, original })
    }
}

impl Drop for RawTerminalGuard {
    fn drop(&mut self) {
        let _ = unsafe { libc::tcsetattr(self.fd, libc::TCSANOW, &self.original) };
    }
}

struct Pty {
    master: File,
    slave_path: PathBuf,
}

fn open_pty() -> Result<Pty> {
    let master_fd = unsafe { libc::posix_openpt(libc::O_RDWR | libc::O_NOCTTY) };
    if master_fd < 0 {
        return Err(std::io::Error::last_os_error()).context("failed to open PTY master");
    }
    if unsafe { libc::grantpt(master_fd) } != 0 {
        return Err(std::io::Error::last_os_error()).context("failed to grant PTY");
    }
    if unsafe { libc::unlockpt(master_fd) } != 0 {
        return Err(std::io::Error::last_os_error()).context("failed to unlock PTY");
    }
    let slave_name = unsafe { libc::ptsname(master_fd) };
    if slave_name.is_null() {
        return Err(std::io::Error::last_os_error()).context("failed to resolve PTY slave");
    }
    let slave_path = PathBuf::from(
        unsafe { CStr::from_ptr(slave_name) }
            .to_string_lossy()
            .into_owned(),
    );
    let master = unsafe { File::from_raw_fd(master_fd) };
    Ok(Pty { master, slave_path })
}

fn spawn_hosted_helper_in_pty(job: &Job, slave_path: &Path) -> Result<Child> {
    let config = hosted_session_config(job, false)?;
    let helper = HostedHelper::from_env_or_default();
    let mut command = Command::new(helper.binary());
    command
        .args(HostedHelper::build_args(&config))
        .env(
            "AGENTVIEW_HOST_DETACH_SEQUENCE",
            String::from_utf8_lossy(DETACH_SEQUENCE).to_string(),
        )
        .current_dir(&config.cwd);

    let stdin = OpenOptions::new().read(true).write(true).open(slave_path)?;
    let stdout = OpenOptions::new().read(true).write(true).open(slave_path)?;
    let stderr = OpenOptions::new().read(true).write(true).open(slave_path)?;
    command
        .stdin(Stdio::from(stdin))
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr));

    let slave = slave_path.to_path_buf();
    unsafe {
        command.pre_exec(move || {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            let c_path = std::ffi::CString::new(slave.to_string_lossy().as_bytes())
                .map_err(std::io::Error::other)?;
            let fd = libc::open(c_path.as_ptr(), libc::O_RDWR);
            if fd == -1 {
                return Err(std::io::Error::last_os_error());
            }
            if libc::ioctl(fd, libc::TIOCSCTTY as libc::c_ulong, 0) == -1 {
                return Err(std::io::Error::last_os_error());
            }
            libc::close(fd);
            Ok(())
        });
    }

    command
        .spawn()
        .context("failed to spawn hosted Codex in PTY")
}

fn set_pty_size(fd: RawFd, size: TerminalSize) -> Result<()> {
    let winsize = libc::winsize {
        ws_row: size.rows,
        ws_col: size.cols,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    if unsafe { libc::ioctl(fd, libc::TIOCSWINSZ, &winsize) } != 0 {
        return Err(std::io::Error::last_os_error()).context("failed to set PTY size");
    }
    Ok(())
}

fn set_pty_size_path(path: &Path, size: TerminalSize) -> Result<()> {
    let file = OpenOptions::new().read(true).write(true).open(path)?;
    set_pty_size(file.as_raw_fd(), size)
}

fn terminal_size(fd: RawFd) -> Option<TerminalSize> {
    let mut winsize = MaybeUninit::<libc::winsize>::uninit();
    if unsafe { libc::ioctl(fd, libc::TIOCGWINSZ, winsize.as_mut_ptr()) } != 0 {
        return None;
    }
    let winsize = unsafe { winsize.assume_init() };
    Some(TerminalSize {
        rows: winsize.ws_row.max(1),
        cols: winsize.ws_col.max(1),
    })
}

fn signal_window_change(pid: u32) {
    let _ = unsafe { libc::kill(pid as libc::pid_t, libc::SIGWINCH) };
}

fn bind_fresh_socket(path: &Path) -> Result<()> {
    if path_exists(path) {
        match UnixStream::connect(path) {
            Ok(_) => bail!("hosted PTY host is already running"),
            Err(_) => fs::remove_file(path)
                .with_context(|| format!("failed to remove stale socket {}", path.display()))?,
        }
    }
    Ok(())
}

fn cleanup_stale_host_files(job_id: &str) {
    let socket = hosted_pty_socket_path(job_id);
    if path_exists(&socket) && UnixStream::connect(&socket).is_err() {
        let _ = fs::remove_file(socket);
    }
}

fn hosted_pty_socket_path(job_id: &str) -> PathBuf {
    job_dir(job_id).join("hosted-pty.sock")
}

fn hosted_pty_pid_path(job_id: &str) -> PathBuf {
    job_dir(job_id).join("hosted-pty.pid")
}

fn hosted_pty_log_path(job_id: &str) -> PathBuf {
    job_dir(job_id).join("hosted-pty.log")
}

fn agentview_binary() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("AGENTVIEW_BIN") {
        return Ok(PathBuf::from(path));
    }
    std::env::current_exe().context("failed to locate current executable")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detach_filter_strips_sequence_from_single_chunk() {
        let mut filter = DetachFilter::default();
        let output = filter.push(b"before\x1b]777;agentview-detach\x07after");

        assert!(output.detach);
        assert_eq!(output.output, b"beforeafter");
        assert!(filter.push(b"").output.is_empty());
    }

    #[test]
    fn detach_filter_handles_split_sequence() {
        let mut filter = DetachFilter::default();
        let first = filter.push(b"before\x1b]777;agent");
        assert!(!first.detach);
        assert_eq!(first.output, b"before");

        let second = filter.push(b"view-detach\x07after");
        assert!(second.detach);
        assert_eq!(second.output, b"after");
        assert!(filter.push(b"").output.is_empty());
    }
}
