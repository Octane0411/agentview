use anyhow::{Context, Result, bail};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InitializeResponse {
    pub user_agent: String,
    pub codex_home: PathBuf,
    pub platform_family: String,
    pub platform_os: String,
}

#[derive(Debug, Clone, Default)]
pub struct ThreadStartOptions {
    pub cwd: Option<PathBuf>,
    pub model: Option<String>,
    pub approval_policy: Option<String>,
    pub sandbox: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadStartResponse {
    pub thread: ThreadSummary,
    pub model: String,
    pub model_provider: String,
    pub service_tier: Option<String>,
    pub cwd: PathBuf,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadSummary {
    pub id: String,
    pub session_id: String,
    #[serde(default)]
    pub preview: String,
    pub status: Value,
    pub cwd: PathBuf,
    #[serde(default)]
    pub name: Option<String>,
}

impl ThreadSummary {
    pub fn status_label(&self) -> &str {
        status_label(&self.status)
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct TurnStartResponse {
    pub turn: TurnSummary,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnSummary {
    pub id: String,
    pub status: Value,
    pub started_at: Option<i64>,
    pub completed_at: Option<i64>,
    pub duration_ms: Option<i64>,
}

impl TurnSummary {
    pub fn status_label(&self) -> &str {
        status_label(&self.status)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Notification {
    pub method: String,
    pub params: Value,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ServerRequest {
    pub id: Value,
    pub method: String,
    pub params: Value,
}

#[derive(Debug, Clone, PartialEq)]
pub enum AppServerEvent {
    Notification(Notification),
    ServerRequest(ServerRequest),
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
    #[serde(default)]
    pub data: Option<Value>,
}

#[derive(Debug)]
pub struct AppServerClient {
    child: Option<Child>,
    stdin: Option<ChildStdin>,
    receiver: Receiver<ReaderEvent>,
    stdout_thread: Option<JoinHandle<()>>,
    stderr_thread: Option<JoinHandle<()>>,
    stderr_lines: Arc<Mutex<Vec<String>>>,
    next_id: u64,
    pending_notifications: Vec<Notification>,
    pending_server_requests: Vec<ServerRequest>,
    pending_responses: Vec<ResponseMessage>,
}

impl AppServerClient {
    pub fn spawn_stdio() -> Result<Self> {
        let mut command = Command::new("codex");
        command.args(["app-server", "--listen", "stdio://"]);
        Self::spawn_with_command(command)
    }

    pub fn spawn_with_command(mut command: Command) -> Result<Self> {
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let mut child = command
            .spawn()
            .context("failed to spawn codex app-server")?;
        let stdin = child
            .stdin
            .take()
            .context("codex app-server stdin was not piped")?;
        let stdout = child
            .stdout
            .take()
            .context("codex app-server stdout was not piped")?;
        let stderr = child
            .stderr
            .take()
            .context("codex app-server stderr was not piped")?;

        let (sender, receiver) = mpsc::channel();
        let stdout_thread = thread::spawn(move || read_stdout(stdout, sender));

        let stderr_lines = Arc::new(Mutex::new(Vec::new()));
        let stderr_thread = {
            let stderr_lines = Arc::clone(&stderr_lines);
            thread::spawn(move || read_stderr(stderr, stderr_lines))
        };

        Ok(Self {
            child: Some(child),
            stdin: Some(stdin),
            receiver,
            stdout_thread: Some(stdout_thread),
            stderr_thread: Some(stderr_thread),
            stderr_lines,
            next_id: 0,
            pending_notifications: Vec::new(),
            pending_server_requests: Vec::new(),
            pending_responses: Vec::new(),
        })
    }

    pub fn initialize(&mut self) -> Result<InitializeResponse> {
        let result = self.request(
            "initialize",
            json!({
                "clientInfo": {
                    "name": "agentview",
                    "title": "AgentView",
                    "version": env!("CARGO_PKG_VERSION"),
                },
                "capabilities": {
                    "experimentalApi": true,
                },
            }),
        )?;
        self.notify("initialized", None)?;
        serde_json::from_value(result).context("failed to parse initialize response")
    }

    pub fn start_thread(&mut self, options: ThreadStartOptions) -> Result<ThreadStartResponse> {
        let mut params = Map::new();
        params.insert("experimentalRawEvents".to_string(), Value::Bool(false));
        params.insert("persistExtendedHistory".to_string(), Value::Bool(false));
        if let Some(cwd) = options.cwd {
            params.insert("cwd".to_string(), path_value(&cwd));
        }
        if let Some(model) = options.model {
            params.insert("model".to_string(), Value::String(model));
        }
        if let Some(approval_policy) = options.approval_policy {
            params.insert("approvalPolicy".to_string(), Value::String(approval_policy));
        }
        if let Some(sandbox) = options.sandbox {
            params.insert("sandbox".to_string(), Value::String(sandbox));
        }

        let result = self.request("thread/start", Value::Object(params))?;
        serde_json::from_value(result).context("failed to parse thread/start response")
    }

    pub fn start_text_turn(&mut self, thread_id: &str, prompt: &str) -> Result<TurnStartResponse> {
        let result = self.request(
            "turn/start",
            json!({
                "threadId": thread_id,
                "input": [{
                    "type": "text",
                    "text": prompt,
                    "text_elements": [],
                }],
            }),
        )?;
        serde_json::from_value(result).context("failed to parse turn/start response")
    }

    pub fn request(&mut self, method: &str, params: Value) -> Result<Value> {
        self.request_with_timeout(method, params, DEFAULT_REQUEST_TIMEOUT)
    }

    pub fn request_with_timeout(
        &mut self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value> {
        let id = Value::from(self.next_id);
        self.next_id += 1;
        self.write_json(&json!({
            "id": id,
            "method": method,
            "params": params,
        }))?;
        self.wait_for_response(&id, timeout)
    }

    pub fn notify(&mut self, method: &str, params: Option<Value>) -> Result<()> {
        let mut message = Map::new();
        message.insert("method".to_string(), Value::String(method.to_string()));
        if let Some(params) = params {
            message.insert("params".to_string(), params);
        }
        self.write_json(&Value::Object(message))
    }

    pub fn next_event(&mut self, timeout: Duration) -> Result<Option<AppServerEvent>> {
        if !self.pending_notifications.is_empty() {
            let notification = self.pending_notifications.remove(0);
            return Ok(Some(AppServerEvent::Notification(notification)));
        }
        if !self.pending_server_requests.is_empty() {
            let request = self.pending_server_requests.remove(0);
            return Ok(Some(AppServerEvent::ServerRequest(request)));
        }

        match self.receiver.recv_timeout(timeout) {
            Ok(ReaderEvent::Message(WireMessage::Notification(notification))) => {
                Ok(Some(AppServerEvent::Notification(notification)))
            }
            Ok(ReaderEvent::Message(WireMessage::Request(request))) => {
                Ok(Some(AppServerEvent::ServerRequest(request)))
            }
            Ok(ReaderEvent::Message(WireMessage::Response(response))) => {
                self.pending_responses.push(response);
                Ok(None)
            }
            Ok(ReaderEvent::InvalidLine { line, error }) => {
                bail!("invalid app-server JSON line: {error}: {line}")
            }
            Ok(ReaderEvent::IoError(error)) => bail!("app-server stdout read failed: {error}"),
            Ok(ReaderEvent::Eof) => Ok(None),
            Err(RecvTimeoutError::Timeout) => Ok(None),
            Err(RecvTimeoutError::Disconnected) => Ok(None),
        }
    }

    pub fn drain_notifications(&mut self) -> Vec<Notification> {
        std::mem::take(&mut self.pending_notifications)
    }

    pub fn drain_server_requests(&mut self) -> Vec<ServerRequest> {
        std::mem::take(&mut self.pending_server_requests)
    }

    pub fn stderr_tail(&self) -> String {
        let Ok(lines) = self.stderr_lines.lock() else {
            return String::new();
        };
        let start = lines.len().saturating_sub(20);
        lines[start..].join("\n")
    }

    pub fn shutdown(mut self) -> Result<()> {
        self.terminate_child()
    }

    fn wait_for_response(&mut self, expected_id: &Value, timeout: Duration) -> Result<Value> {
        if let Some(index) = self
            .pending_responses
            .iter()
            .position(|response| &response.id == expected_id)
        {
            return response_result(self.pending_responses.remove(index));
        }

        let deadline = Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                let stderr = self.stderr_tail();
                if stderr.is_empty() {
                    bail!("timed out waiting for app-server response id {expected_id}");
                }
                bail!(
                    "timed out waiting for app-server response id {expected_id}\nstderr:\n{stderr}"
                );
            }

            match self.receiver.recv_timeout(remaining) {
                Ok(ReaderEvent::Message(message)) => match message {
                    WireMessage::Response(response) if &response.id == expected_id => {
                        return response_result(response);
                    }
                    WireMessage::Response(response) => self.pending_responses.push(response),
                    WireMessage::Notification(notification) => {
                        self.pending_notifications.push(notification);
                    }
                    WireMessage::Request(request) => self.pending_server_requests.push(request),
                },
                Ok(ReaderEvent::InvalidLine { line, error }) => {
                    bail!("invalid app-server JSON line: {error}: {line}")
                }
                Ok(ReaderEvent::IoError(error)) => bail!("app-server stdout read failed: {error}"),
                Ok(ReaderEvent::Eof) => {
                    let stderr = self.stderr_tail();
                    if stderr.is_empty() {
                        bail!("app-server exited before response id {expected_id}");
                    }
                    bail!("app-server exited before response id {expected_id}\nstderr:\n{stderr}");
                }
                Err(RecvTimeoutError::Timeout) => continue,
                Err(RecvTimeoutError::Disconnected) => {
                    bail!("app-server stdout reader disconnected")
                }
            }
        }
    }

    fn write_json(&mut self, value: &Value) -> Result<()> {
        let stdin = self
            .stdin
            .as_mut()
            .context("codex app-server stdin is closed")?;
        serde_json::to_writer(&mut *stdin, value).context("failed to write app-server JSON")?;
        stdin
            .write_all(b"\n")
            .context("failed to write app-server newline")?;
        stdin.flush().context("failed to flush app-server stdin")
    }

    fn terminate_child(&mut self) -> Result<()> {
        self.stdin.take();
        if let Some(mut child) = self.child.take() {
            if child.try_wait()?.is_none() {
                let _ = child.kill();
            }
            let _ = child.wait();
        }
        if let Some(thread) = self.stdout_thread.take() {
            let _ = thread.join();
        }
        if let Some(thread) = self.stderr_thread.take() {
            let _ = thread.join();
        }
        Ok(())
    }
}

impl Drop for AppServerClient {
    fn drop(&mut self) {
        let _ = self.terminate_child();
    }
}

#[derive(Debug, Clone)]
enum WireMessage {
    Response(ResponseMessage),
    Notification(Notification),
    Request(ServerRequest),
}

#[derive(Debug, Clone)]
struct ResponseMessage {
    id: Value,
    result: Option<Value>,
    error: Option<RpcError>,
}

#[derive(Debug)]
enum ReaderEvent {
    Message(WireMessage),
    InvalidLine { line: String, error: String },
    IoError(String),
    Eof,
}

fn response_result(response: ResponseMessage) -> Result<Value> {
    if let Some(error) = response.error {
        if let Some(data) = error.data {
            bail!(
                "app-server error {}: {}\ndata: {}",
                error.code,
                error.message,
                data
            );
        }
        bail!("app-server error {}: {}", error.code, error.message);
    }
    Ok(response.result.unwrap_or(Value::Null))
}

fn read_stdout(stdout: std::process::ChildStdout, sender: mpsc::Sender<ReaderEvent>) {
    let mut reader = BufReader::new(stdout);
    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => {
                let _ = sender.send(ReaderEvent::Eof);
                return;
            }
            Ok(_) => {
                let trimmed = line.trim_end_matches(&['\r', '\n'][..]);
                if trimmed.is_empty() {
                    continue;
                }
                let event = match serde_json::from_str::<Value>(trimmed) {
                    Ok(value) => match wire_message_from_value(value) {
                        Ok(message) => ReaderEvent::Message(message),
                        Err(error) => ReaderEvent::InvalidLine {
                            line: trimmed.to_string(),
                            error: error.to_string(),
                        },
                    },
                    Err(error) => ReaderEvent::InvalidLine {
                        line: trimmed.to_string(),
                        error: error.to_string(),
                    },
                };
                if sender.send(event).is_err() {
                    return;
                }
            }
            Err(error) => {
                let _ = sender.send(ReaderEvent::IoError(error.to_string()));
                return;
            }
        }
    }
}

fn read_stderr(stderr: std::process::ChildStderr, lines: Arc<Mutex<Vec<String>>>) {
    let reader = BufReader::new(stderr);
    for line in reader.lines().map_while(std::result::Result::ok) {
        if let Ok(mut lines) = lines.lock() {
            lines.push(line);
        }
    }
}

fn wire_message_from_value(mut value: Value) -> Result<WireMessage> {
    let object = value
        .as_object_mut()
        .context("app-server message is not an object")?;

    let id = object.remove("id");
    let method = object
        .remove("method")
        .and_then(|value| value.as_str().map(ToOwned::to_owned));
    let params = object.remove("params").unwrap_or(Value::Null);

    match (id, method) {
        (Some(id), Some(method)) => Ok(WireMessage::Request(ServerRequest { id, method, params })),
        (None, Some(method)) => Ok(WireMessage::Notification(Notification { method, params })),
        (Some(id), None) => {
            let result = object.remove("result");
            let error = object
                .remove("error")
                .map(serde_json::from_value)
                .transpose()
                .context("failed to parse app-server error")?;
            Ok(WireMessage::Response(ResponseMessage { id, result, error }))
        }
        (None, None) => bail!("app-server message has neither method nor id"),
    }
}

fn path_value(path: &Path) -> Value {
    Value::String(path.to_string_lossy().into_owned())
}

fn status_label(status: &Value) -> &str {
    status
        .as_str()
        .or_else(|| status.get("type").and_then(Value::as_str))
        .unwrap_or("unknown")
}
