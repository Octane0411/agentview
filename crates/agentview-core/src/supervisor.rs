use crate::store::agentview_home;
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::collections::hash_map::DefaultHasher;
use std::fs;
use std::hash::{Hash, Hasher};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::time::Duration;

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum SupervisorRequest {
    Ping,
}

#[derive(Debug, Deserialize, Serialize)]
struct SupervisorResponse {
    ok: bool,
    message: String,
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
        handle_stream(stream?)?;
    }
    Ok(())
}

#[cfg(not(unix))]
pub fn run_supervisor(_once: bool) -> Result<()> {
    bail!("AgentView supervisor IPC currently requires Unix domain sockets")
}

#[cfg(unix)]
pub fn supervisor_ping(timeout: Duration) -> Result<String> {
    use std::os::unix::net::UnixStream;

    let mut stream = UnixStream::connect(supervisor_socket_path())
        .context("failed to connect to AgentView supervisor")?;
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(timeout))?;
    serde_json::to_writer(&mut stream, &serde_json::json!({ "type": "ping" }))?;
    stream.write_all(b"\n")?;
    stream.flush()?;

    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line)?;
    let response: SupervisorResponse =
        serde_json::from_str(line.trim()).context("failed to parse supervisor response")?;
    if !response.ok {
        bail!("supervisor error: {}", response.message);
    }
    Ok(response.message)
}

#[cfg(not(unix))]
pub fn supervisor_ping(_timeout: Duration) -> Result<String> {
    bail!("AgentView supervisor IPC currently requires Unix domain sockets")
}

#[cfg(unix)]
fn handle_stream(mut stream: std::os::unix::net::UnixStream) -> Result<()> {
    let mut line = String::new();
    BufReader::new(stream.try_clone()?).read_line(&mut line)?;
    let response = match serde_json::from_str::<SupervisorRequest>(line.trim()) {
        Ok(SupervisorRequest::Ping) => SupervisorResponse {
            ok: true,
            message: "pong".to_string(),
        },
        Err(error) => SupervisorResponse {
            ok: false,
            message: error.to_string(),
        },
    };
    serde_json::to_writer(&mut stream, &response)?;
    stream.write_all(b"\n")?;
    stream.flush()?;
    Ok(())
}

struct SupervisorFiles;

impl Drop for SupervisorFiles {
    fn drop(&mut self) {
        let _ = fs::remove_file(supervisor_socket_path());
        let _ = fs::remove_file(supervisor_pid_path());
    }
}
