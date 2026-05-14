use crate::codex::{run_codex_app_server_reply, run_codex_app_server_turn};
use crate::schema::{JobStatus, ProcessState};
use crate::store::{agentview_home, append_job_event, update_job};
use crate::util::now_iso;
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::collections::hash_map::DefaultHasher;
use std::fs;
use std::hash::{Hash, Hasher};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum SupervisorRequest {
    Ping,
    Shutdown,
    RunAppServerTurn {
        job_id: String,
        prompt: String,
        resume: bool,
    },
}

#[derive(Debug, Deserialize, Serialize)]
struct SupervisorResponse {
    ok: bool,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pid: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SupervisorAction {
    Continue,
    Shutdown,
}

pub fn supervisor_socket_path() -> PathBuf {
    let mut hasher = DefaultHasher::new();
    agentview_home().hash(&mut hasher);
    std::env::temp_dir().join(format!("agentview-{:x}.sock", hasher.finish()))
}

pub fn supervisor_pid_path() -> PathBuf {
    agentview_home().join("supervisor.pid")
}

#[cfg(unix)]
pub fn run_supervisor(once: bool) -> Result<()> {
    use std::os::unix::net::{UnixListener, UnixStream};

    fs::create_dir_all(agentview_home())?;
    let socket_path = supervisor_socket_path();
    if socket_path.exists() {
        match UnixStream::connect(&socket_path) {
            Ok(_) => bail!("AgentView supervisor is already running"),
            Err(_) => fs::remove_file(&socket_path).with_context(|| {
                format!("failed to remove stale socket {}", socket_path.display())
            })?,
        }
    }

    let listener = UnixListener::bind(&socket_path)
        .with_context(|| format!("failed to bind {}", socket_path.display()))?;
    fs::write(supervisor_pid_path(), std::process::id().to_string())?;
    let _guard = SupervisorFiles;

    if once {
        let (stream, _) = listener.accept()?;
        handle_stream(stream)?;
        return Ok(());
    }

    for stream in listener.incoming() {
        if handle_stream(stream?)? == SupervisorAction::Shutdown {
            break;
        }
    }
    Ok(())
}

#[cfg(not(unix))]
pub fn run_supervisor(_once: bool) -> Result<()> {
    bail!("AgentView supervisor IPC currently requires Unix domain sockets")
}

#[cfg(unix)]
pub fn supervisor_ping(timeout: Duration) -> Result<String> {
    Ok(send_supervisor_request(&serde_json::json!({ "type": "ping" }), timeout)?.message)
}

#[cfg(not(unix))]
pub fn supervisor_ping(_timeout: Duration) -> Result<String> {
    bail!("AgentView supervisor IPC currently requires Unix domain sockets")
}

#[cfg(unix)]
pub fn supervisor_start_app_server_turn(job_id: &str, prompt: &str, resume: bool) -> Result<u32> {
    let pid = ensure_supervisor()?;
    let response = send_supervisor_request(
        &serde_json::json!({
            "type": "run_app_server_turn",
            "job_id": job_id,
            "prompt": prompt,
            "resume": resume,
        }),
        Duration::from_secs(2),
    )?;
    Ok(response.pid.unwrap_or(pid))
}

#[cfg(not(unix))]
pub fn supervisor_start_app_server_turn(
    _job_id: &str,
    _prompt: &str,
    _resume: bool,
) -> Result<u32> {
    bail!("AgentView supervisor IPC currently requires Unix domain sockets")
}

#[cfg(unix)]
pub fn supervisor_shutdown() -> Result<()> {
    let _ = send_supervisor_request(
        &serde_json::json!({ "type": "shutdown" }),
        Duration::from_secs(2),
    )?;
    Ok(())
}

#[cfg(not(unix))]
pub fn supervisor_shutdown() -> Result<()> {
    bail!("AgentView supervisor IPC currently requires Unix domain sockets")
}

#[cfg(unix)]
fn ensure_supervisor() -> Result<u32> {
    if supervisor_ping(Duration::from_millis(100)).is_ok() {
        return Ok(read_supervisor_pid().unwrap_or(0));
    }

    let mut child = Command::new(supervisor_binary()?);
    child
        .arg("__supervisor")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let pid = child
        .spawn()
        .context("failed to spawn AgentView supervisor")?
        .id();

    for _ in 0..50 {
        if supervisor_ping(Duration::from_millis(100)).is_ok() {
            return Ok(pid);
        }
        std::thread::sleep(Duration::from_millis(40));
    }
    bail!("timed out waiting for AgentView supervisor to start")
}

fn supervisor_binary() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("AGENTVIEW_BIN") {
        return Ok(PathBuf::from(path));
    }
    std::env::current_exe().context("failed to locate current executable")
}

fn read_supervisor_pid() -> Option<u32> {
    fs::read_to_string(supervisor_pid_path())
        .ok()
        .and_then(|text| text.trim().parse().ok())
}

#[cfg(unix)]
fn send_supervisor_request(
    value: &serde_json::Value,
    timeout: Duration,
) -> Result<SupervisorResponse> {
    use std::os::unix::net::UnixStream;

    let mut stream = UnixStream::connect(supervisor_socket_path())
        .context("failed to connect to AgentView supervisor")?;
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(timeout))?;
    serde_json::to_writer(&mut stream, value)?;
    stream.write_all(b"\n")?;
    stream.flush()?;

    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line)?;
    let response: SupervisorResponse =
        serde_json::from_str(line.trim()).context("failed to parse supervisor response")?;
    if !response.ok {
        bail!("supervisor error: {}", response.message);
    }
    Ok(response)
}

#[cfg(unix)]
fn handle_stream(mut stream: std::os::unix::net::UnixStream) -> Result<SupervisorAction> {
    let mut line = String::new();
    BufReader::new(stream.try_clone()?).read_line(&mut line)?;
    let (response, action) = match serde_json::from_str::<SupervisorRequest>(line.trim()) {
        Ok(SupervisorRequest::Ping) => (
            SupervisorResponse {
                ok: true,
                message: "pong".to_string(),
                pid: Some(std::process::id()),
            },
            SupervisorAction::Continue,
        ),
        Ok(SupervisorRequest::Shutdown) => (
            SupervisorResponse {
                ok: true,
                message: "shutdown".to_string(),
                pid: Some(std::process::id()),
            },
            SupervisorAction::Shutdown,
        ),
        Ok(SupervisorRequest::RunAppServerTurn {
            job_id,
            prompt,
            resume,
        }) => {
            start_app_server_turn_thread(job_id, prompt, resume);
            (
                SupervisorResponse {
                    ok: true,
                    message: "started".to_string(),
                    pid: Some(std::process::id()),
                },
                SupervisorAction::Continue,
            )
        }
        Err(error) => (
            SupervisorResponse {
                ok: false,
                message: error.to_string(),
                pid: Some(std::process::id()),
            },
            SupervisorAction::Continue,
        ),
    };
    serde_json::to_writer(&mut stream, &response)?;
    stream.write_all(b"\n")?;
    stream.flush()?;
    Ok(action)
}

fn start_app_server_turn_thread(job_id: String, prompt: String, resume: bool) {
    let _ = append_job_event(
        &job_id,
        &serde_json::json!({
            "type": "supervisor_app_server_turn_queued",
            "resume": resume,
            "timestamp": now_iso()
        }),
    );
    thread::spawn(move || {
        let result = if resume {
            run_codex_app_server_reply(&job_id, &prompt)
        } else {
            run_codex_app_server_turn(&job_id, &prompt)
        };
        if let Err(error) = result {
            record_turn_error(&job_id, error.to_string());
        }
    });
}

fn record_turn_error(job_id: &str, message: String) {
    let _ = append_job_event(
        job_id,
        &serde_json::json!({
            "type": "supervisor_error",
            "error": message,
            "timestamp": now_iso()
        }),
    );
    let _ = update_job(job_id, |job| {
        job.status = JobStatus::Failed;
        job.process_state = ProcessState::Exited;
        job.pid = None;
        job.active_worker_pid = None;
        job.codex_turn_id = None;
        job.completed_at = Some(now_iso());
        job.last_summary = Some(format!("failed: {message}"));
        job.error = Some(message.clone());
        Ok(())
    });
}

struct SupervisorFiles;

impl Drop for SupervisorFiles {
    fn drop(&mut self) {
        let _ = fs::remove_file(supervisor_socket_path());
        let _ = fs::remove_file(supervisor_pid_path());
    }
}
