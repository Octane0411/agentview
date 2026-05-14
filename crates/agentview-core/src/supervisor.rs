use crate::codex::handle_app_server_runtime_event;
use crate::schema::{JobStatus, ProcessState};
use crate::store::{agentview_home, append_job_event, require_job, update_job};
use crate::util::now_iso;
use agentview_codex_runtime::{CodexRuntime, RuntimeEvent, RuntimeTurnOptions};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::collections::hash_map::DefaultHasher;
use std::fs;
use std::hash::{Hash, Hasher};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, mpsc};
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
    StopAppServerTurn {
        job_id: String,
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

#[derive(Clone, Default)]
struct SupervisorState {
    running: Arc<Mutex<HashMap<String, mpsc::Sender<RunningCommand>>>>,
}

#[derive(Debug, Clone, Copy)]
enum RunningCommand {
    Interrupt,
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
    let state = SupervisorState::default();

    if once {
        let (stream, _) = listener.accept()?;
        handle_stream(stream, &state)?;
        return Ok(());
    }

    for stream in listener.incoming() {
        if handle_stream(stream?, &state)? == SupervisorAction::Shutdown {
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
pub fn supervisor_stop_app_server_turn(job_id: &str) -> Result<()> {
    let _ = send_supervisor_request(
        &serde_json::json!({
            "type": "stop_app_server_turn",
            "job_id": job_id,
        }),
        Duration::from_secs(2),
    )?;
    Ok(())
}

#[cfg(not(unix))]
pub fn supervisor_stop_app_server_turn(_job_id: &str) -> Result<()> {
    bail!("AgentView supervisor IPC currently requires Unix domain sockets")
}

#[cfg(unix)]
fn handle_stream(
    mut stream: std::os::unix::net::UnixStream,
    state: &SupervisorState,
) -> Result<SupervisorAction> {
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
            let response = match start_app_server_turn_thread(state, job_id, prompt, resume) {
                Ok(()) => SupervisorResponse {
                    ok: true,
                    message: "started".to_string(),
                    pid: Some(std::process::id()),
                },
                Err(error) => SupervisorResponse {
                    ok: false,
                    message: error.to_string(),
                    pid: Some(std::process::id()),
                },
            };
            (response, SupervisorAction::Continue)
        }
        Ok(SupervisorRequest::StopAppServerTurn { job_id }) => {
            let response = match stop_app_server_turn(state, &job_id) {
                Ok(()) => SupervisorResponse {
                    ok: true,
                    message: "interrupt_sent".to_string(),
                    pid: Some(std::process::id()),
                },
                Err(error) => SupervisorResponse {
                    ok: false,
                    message: error.to_string(),
                    pid: Some(std::process::id()),
                },
            };
            (response, SupervisorAction::Continue)
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

fn start_app_server_turn_thread(
    state: &SupervisorState,
    job_id: String,
    prompt: String,
    resume: bool,
) -> Result<()> {
    let (sender, receiver) = mpsc::channel();
    {
        let mut running = state
            .running
            .lock()
            .map_err(|_| anyhow::anyhow!("supervisor running map is poisoned"))?;
        if running.contains_key(&job_id) {
            bail!("Job {job_id} is already running");
        }
        running.insert(job_id.clone(), sender);
    }

    append_job_event(
        &job_id,
        &serde_json::json!({
            "type": "supervisor_app_server_turn_queued",
            "resume": resume,
            "timestamp": now_iso()
        }),
    )?;
    let state = state.clone();
    thread::spawn(move || {
        let result = run_app_server_turn(&job_id, &prompt, resume, receiver);
        if let Ok(mut running) = state.running.lock() {
            running.remove(&job_id);
        }
        if let Err(error) = result {
            record_turn_error(&job_id, error.to_string());
        }
    });
    Ok(())
}

fn stop_app_server_turn(state: &SupervisorState, job_id: &str) -> Result<()> {
    let sender = {
        let running = state
            .running
            .lock()
            .map_err(|_| anyhow::anyhow!("supervisor running map is poisoned"))?;
        running
            .get(job_id)
            .cloned()
            .with_context(|| format!("Job {job_id} is not running under this supervisor"))?
    };
    sender
        .send(RunningCommand::Interrupt)
        .context("failed to send interrupt command to running turn")?;
    append_job_event(
        job_id,
        &serde_json::json!({
            "type": "supervisor_app_server_turn_interrupt_requested",
            "timestamp": now_iso()
        }),
    )?;
    update_job(job_id, |job| {
        job.last_summary = Some("stopping".to_string());
        Ok(())
    })?;
    Ok(())
}

fn run_app_server_turn(
    job_id: &str,
    prompt: &str,
    resume: bool,
    commands: mpsc::Receiver<RunningCommand>,
) -> Result<()> {
    let job = require_job(job_id)?;
    update_job(job_id, |job| {
        job.status = JobStatus::Working;
        job.process_state = ProcessState::Alive;
        job.pid = Some(std::process::id());
        job.active_worker_pid = Some(std::process::id());
        job.codex_turn_id = None;
        job.completed_at = None;
        job.last_summary = Some(if resume {
            "Resuming Codex app-server thread".to_string()
        } else {
            "Starting Codex app-server session".to_string()
        });
        job.blocking_request = None;
        job.error = None;
        Ok(())
    })?;

    let options = RuntimeTurnOptions {
        cwd: PathBuf::from(&job.cwd),
        model: job.model.clone(),
        approval_policy: job.approval_policy.clone(),
        sandbox: job.sandbox.clone(),
    };
    let mut session = CodexRuntime::default().spawn_session()?;
    let mut latest_text = String::new();
    handle_app_server_runtime_event(job_id, session.initialized_event(), &mut latest_text)?;

    let thread_id = if resume {
        let thread_id = job
            .codex_thread_id
            .clone()
            .with_context(|| format!("Job {job_id} has no Codex thread id yet"))?;
        let thread_id = session.resume_thread(&thread_id, options)?;
        handle_app_server_runtime_event(
            job_id,
            RuntimeEvent::ThreadResumed {
                thread_id: thread_id.clone(),
            },
            &mut latest_text,
        )?;
        thread_id
    } else {
        let thread_id = session.start_thread(options)?;
        handle_app_server_runtime_event(
            job_id,
            RuntimeEvent::ThreadStarted {
                thread_id: thread_id.clone(),
            },
            &mut latest_text,
        )?;
        thread_id
    };

    let turn_id = session.start_text_turn(&thread_id, prompt)?;
    handle_app_server_runtime_event(
        job_id,
        RuntimeEvent::TurnStarted {
            thread_id: thread_id.clone(),
            turn_id: turn_id.clone(),
        },
        &mut latest_text,
    )?;

    let mut interrupt_sent = false;
    loop {
        while let Ok(command) = commands.try_recv() {
            match command {
                RunningCommand::Interrupt if !interrupt_sent => {
                    session.interrupt_turn(&thread_id, &turn_id)?;
                    interrupt_sent = true;
                    append_job_event(
                        job_id,
                        &serde_json::json!({
                            "type": "supervisor_app_server_turn_interrupt_sent",
                            "threadId": thread_id,
                            "turnId": turn_id,
                            "timestamp": now_iso()
                        }),
                    )?;
                }
                RunningCommand::Interrupt => {}
            }
        }

        match session.next_event()? {
            Some(RuntimeEvent::Notification(notification)) => {
                let completed = notification.method == "turn/completed";
                handle_app_server_runtime_event(
                    job_id,
                    RuntimeEvent::Notification(notification),
                    &mut latest_text,
                )?;
                if completed {
                    break;
                }
            }
            Some(event) => handle_app_server_runtime_event(job_id, event, &mut latest_text)?,
            None => {}
        }
    }

    session.shutdown()?;
    Ok(())
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
