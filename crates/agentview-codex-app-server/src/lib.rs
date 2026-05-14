use anyhow::{Context, Result, bail};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
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

#[derive(Debug, Clone)]
pub struct ThreadResumeOptions {
    pub thread_id: String,
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
pub struct ThreadResumeResponse {
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
    connection: Option<AppServerConnection>,
    receiver: Receiver<ReaderEvent>,
    reader_thread: Option<JoinHandle<()>>,
    stderr_thread: Option<JoinHandle<()>>,
    stderr_lines: Arc<Mutex<Vec<String>>>,
    next_id: u64,
    pending_notifications: Vec<Notification>,
    pending_server_requests: Vec<ServerRequest>,
    pending_responses: Vec<ResponseMessage>,
}

#[derive(Debug)]
enum AppServerConnection {
    Stdio(ChildStdin),
    WebSocket(Arc<Mutex<TcpStream>>),
}

impl AppServerClient {
    pub fn spawn_stdio() -> Result<Self> {
        let mut command = Command::new("codex");
        command.args(["app-server", "--listen", "stdio://"]);
        Self::spawn_with_command(command)
    }

    pub fn spawn_websocket(listen_url: &str) -> Result<Self> {
        let mut command = Command::new("codex");
        command.args(["app-server", "--listen", listen_url]);
        Self::spawn_websocket_with_command(command, listen_url)
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
        let reader_thread = thread::spawn(move || read_stdout(stdout, sender));

        let stderr_lines = Arc::new(Mutex::new(Vec::new()));
        let stderr_thread = {
            let stderr_lines = Arc::clone(&stderr_lines);
            thread::spawn(move || read_stderr(stderr, stderr_lines))
        };

        Ok(Self {
            child: Some(child),
            connection: Some(AppServerConnection::Stdio(stdin)),
            receiver,
            reader_thread: Some(reader_thread),
            stderr_thread: Some(stderr_thread),
            stderr_lines,
            next_id: 0,
            pending_notifications: Vec::new(),
            pending_server_requests: Vec::new(),
            pending_responses: Vec::new(),
        })
    }

    pub fn spawn_websocket_with_command(mut command: Command, listen_url: &str) -> Result<Self> {
        command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());

        let mut child = command
            .spawn()
            .context("failed to spawn codex app-server websocket")?;
        let stderr = child
            .stderr
            .take()
            .context("codex app-server stderr was not piped")?;

        let stderr_lines = Arc::new(Mutex::new(Vec::new()));
        let stderr_thread = {
            let stderr_lines = Arc::clone(&stderr_lines);
            thread::spawn(move || read_stderr(stderr, stderr_lines))
        };

        let stream =
            match connect_websocket_with_retry(listen_url, Duration::from_secs(5), &stderr_lines) {
                Ok(stream) => stream,
                Err(error) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    let _ = stderr_thread.join();
                    return Err(error);
                }
            };
        let reader_stream = stream
            .try_clone()
            .context("failed to clone app-server websocket stream")?;
        let writer_stream = Arc::new(Mutex::new(stream));

        let (sender, receiver) = mpsc::channel();
        let reader_thread = thread::spawn(move || read_websocket(reader_stream, sender));

        Ok(Self {
            child: Some(child),
            connection: Some(AppServerConnection::WebSocket(writer_stream)),
            receiver,
            reader_thread: Some(reader_thread),
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

    pub fn resume_thread(&mut self, options: ThreadResumeOptions) -> Result<ThreadResumeResponse> {
        let mut params = Map::new();
        params.insert("threadId".to_string(), Value::String(options.thread_id));
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

        let result = self.request("thread/resume", Value::Object(params))?;
        serde_json::from_value(result).context("failed to parse thread/resume response")
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

    pub fn interrupt_turn(&mut self, thread_id: &str, turn_id: &str) -> Result<()> {
        self.request(
            "turn/interrupt",
            json!({
                "threadId": thread_id,
                "turnId": turn_id,
            }),
        )?;
        Ok(())
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
        let connection = self
            .connection
            .as_mut()
            .context("codex app-server connection is closed")?;
        match connection {
            AppServerConnection::Stdio(stdin) => {
                serde_json::to_writer(&mut *stdin, value)
                    .context("failed to write app-server JSON")?;
                stdin
                    .write_all(b"\n")
                    .context("failed to write app-server newline")?;
                stdin.flush().context("failed to flush app-server stdin")
            }
            AppServerConnection::WebSocket(stream) => {
                let payload =
                    serde_json::to_string(value).context("failed to encode app-server JSON")?;
                let mut stream = stream
                    .lock()
                    .map_err(|_| anyhow::anyhow!("app-server websocket stream is poisoned"))?;
                write_websocket_text(&mut stream, &payload)
                    .context("failed to write app-server websocket frame")
            }
        }
    }

    fn terminate_child(&mut self) -> Result<()> {
        self.connection.take();
        if let Some(mut child) = self.child.take() {
            if child.try_wait()?.is_none() {
                let _ = child.kill();
            }
            let _ = child.wait();
        }
        if let Some(thread) = self.reader_thread.take() {
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

fn read_websocket(mut stream: TcpStream, sender: mpsc::Sender<ReaderEvent>) {
    loop {
        match read_websocket_text(&mut stream) {
            Ok(Some(text)) => {
                if text.trim().is_empty() {
                    continue;
                }
                let event = match serde_json::from_str::<Value>(&text) {
                    Ok(value) => match wire_message_from_value(value) {
                        Ok(message) => ReaderEvent::Message(message),
                        Err(error) => ReaderEvent::InvalidLine {
                            line: text,
                            error: error.to_string(),
                        },
                    },
                    Err(error) => ReaderEvent::InvalidLine {
                        line: text,
                        error: error.to_string(),
                    },
                };
                if sender.send(event).is_err() {
                    return;
                }
            }
            Ok(None) => {
                let _ = sender.send(ReaderEvent::Eof);
                return;
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

fn connect_websocket_with_retry(
    listen_url: &str,
    timeout: Duration,
    stderr_lines: &Arc<Mutex<Vec<String>>>,
) -> Result<TcpStream> {
    let deadline = Instant::now() + timeout;
    let mut last_error = None;
    while Instant::now() < deadline {
        match connect_websocket_once(listen_url) {
            Ok(stream) => return Ok(stream),
            Err(error) => {
                last_error = Some(error.to_string());
                thread::sleep(Duration::from_millis(50));
            }
        }
    }

    let stderr = match stderr_lines.lock() {
        Ok(lines) => {
            let start = lines.len().saturating_sub(20);
            lines[start..].join("\n")
        }
        Err(_) => String::new(),
    };
    let message = last_error.unwrap_or_else(|| "timed out".to_string());
    if stderr.is_empty() {
        bail!("failed to connect app-server websocket {listen_url}: {message}");
    }
    bail!("failed to connect app-server websocket {listen_url}: {message}\nstderr:\n{stderr}")
}

fn connect_websocket_once(listen_url: &str) -> Result<TcpStream> {
    let target = parse_ws_url(listen_url)?;
    let mut stream = TcpStream::connect((target.host.as_str(), target.port))
        .with_context(|| format!("failed to connect TCP socket for {listen_url}"))?;
    stream
        .set_nodelay(true)
        .context("failed to configure app-server websocket socket")?;
    let request = format!(
        "GET {path} HTTP/1.1\r\nHost: {host}:{port}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\r\n",
        path = target.path,
        host = target.host,
        port = target.port
    );
    stream
        .write_all(request.as_bytes())
        .context("failed to write websocket handshake")?;
    stream
        .flush()
        .context("failed to flush websocket handshake")?;

    let response = read_http_response_header(&mut stream)?;
    let status_line = response.lines().next().unwrap_or_default();
    if !status_line.contains(" 101 ") && !status_line.ends_with(" 101 Switching Protocols") {
        bail!("websocket handshake failed: {status_line}");
    }
    Ok(stream)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct WsTarget {
    host: String,
    port: u16,
    path: String,
}

fn parse_ws_url(url: &str) -> Result<WsTarget> {
    let rest = url
        .strip_prefix("ws://")
        .with_context(|| format!("unsupported app-server websocket URL `{url}`"))?;
    let (authority, path) = match rest.split_once('/') {
        Some((authority, path)) => (authority, format!("/{path}")),
        None => (rest, "/".to_string()),
    };
    let (host, port) = authority
        .rsplit_once(':')
        .with_context(|| format!("websocket URL must include an explicit port: `{url}`"))?;
    if host.is_empty() {
        bail!("websocket URL host is empty: `{url}`");
    }
    let port = port
        .parse::<u16>()
        .with_context(|| format!("invalid websocket URL port in `{url}`"))?;
    Ok(WsTarget {
        host: host.trim_matches(['[', ']']).to_string(),
        port,
        path,
    })
}

fn read_http_response_header(stream: &mut TcpStream) -> Result<String> {
    let mut bytes = Vec::new();
    let mut buf = [0_u8; 1];
    while !bytes.ends_with(b"\r\n\r\n") {
        let read = stream
            .read(&mut buf)
            .context("failed to read websocket handshake")?;
        if read == 0 {
            bail!("websocket handshake closed before response headers");
        }
        bytes.push(buf[0]);
        if bytes.len() > 16 * 1024 {
            bail!("websocket handshake response header is too large");
        }
    }
    String::from_utf8(bytes).context("websocket handshake response is not UTF-8")
}

fn read_websocket_text(stream: &mut TcpStream) -> Result<Option<String>> {
    loop {
        let mut header = [0_u8; 2];
        match stream.read_exact(&mut header) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
            Err(error) => return Err(error).context("failed to read websocket frame header"),
        }

        let opcode = header[0] & 0x0f;
        let masked = (header[1] & 0x80) != 0;
        let mut len = u64::from(header[1] & 0x7f);
        if len == 126 {
            let mut extended = [0_u8; 2];
            stream
                .read_exact(&mut extended)
                .context("failed to read websocket frame length")?;
            len = u64::from(u16::from_be_bytes(extended));
        } else if len == 127 {
            let mut extended = [0_u8; 8];
            stream
                .read_exact(&mut extended)
                .context("failed to read websocket frame length")?;
            len = u64::from_be_bytes(extended);
        }
        if len > 128 * 1024 * 1024 {
            bail!("websocket frame is too large: {len} bytes");
        }

        let mut mask = [0_u8; 4];
        if masked {
            stream
                .read_exact(&mut mask)
                .context("failed to read websocket frame mask")?;
        }
        let mut payload = vec![0_u8; len as usize];
        stream
            .read_exact(&mut payload)
            .context("failed to read websocket frame payload")?;
        if masked {
            for (index, byte) in payload.iter_mut().enumerate() {
                *byte ^= mask[index % 4];
            }
        }

        match opcode {
            0x1 => {
                return String::from_utf8(payload)
                    .map(Some)
                    .context("websocket text frame is not UTF-8");
            }
            0x2 => {
                return String::from_utf8(payload)
                    .map(Some)
                    .context("websocket binary frame is not UTF-8");
            }
            0x8 => return Ok(None),
            0x9 | 0xA => continue,
            _ => continue,
        }
    }
}

fn write_websocket_text(stream: &mut TcpStream, text: &str) -> Result<()> {
    let payload = text.as_bytes();
    let mut frame = Vec::with_capacity(payload.len() + 14);
    frame.push(0x81);
    if payload.len() < 126 {
        frame.push(0x80 | payload.len() as u8);
    } else if payload.len() <= u16::MAX as usize {
        frame.push(0x80 | 126);
        frame.extend_from_slice(&(payload.len() as u16).to_be_bytes());
    } else {
        frame.push(0x80 | 127);
        frame.extend_from_slice(&(payload.len() as u64).to_be_bytes());
    }
    let mask = [0x12, 0x34, 0x56, 0x78];
    frame.extend_from_slice(&mask);
    frame.extend(
        payload
            .iter()
            .enumerate()
            .map(|(index, byte)| byte ^ mask[index % 4]),
    );
    stream.write_all(&frame)?;
    stream.flush()?;
    Ok(())
}
