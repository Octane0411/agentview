use agentview_codex_app_server::{AppServerClient, AppServerEvent, ThreadStartOptions};
use std::fs;
use std::net::TcpListener;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;
use tempfile::TempDir;

#[test]
fn websocket_app_server_client_drives_turn() {
    let temp = TempDir::new().unwrap();
    let codex = fake_websocket_codex(&temp);
    let listen_url = reserve_listen_url();

    let mut command = Command::new(codex);
    command.args(["app-server", "--listen", &listen_url]);
    let mut client = AppServerClient::spawn_websocket_with_command(command, &listen_url).unwrap();

    let initialized = client.initialize().unwrap();
    assert_eq!(initialized.user_agent, "fake-codex-ws/0.0.0");

    let thread = client
        .start_thread(ThreadStartOptions {
            cwd: Some(temp.path().to_path_buf()),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(thread.thread.id, "thread-ws-1");

    let turn = client
        .start_text_turn("thread-ws-1", "hello over websocket")
        .unwrap();
    assert_eq!(turn.turn.id, "turn-ws-1");

    let first = client.next_event(Duration::from_secs(1)).unwrap();
    assert!(matches!(
        first,
        Some(AppServerEvent::Notification(notification))
            if notification.method == "item/agentMessage/delta"
    ));
    let second = client.next_event(Duration::from_secs(1)).unwrap();
    assert!(matches!(
        second,
        Some(AppServerEvent::Notification(notification))
            if notification.method == "turn/completed"
    ));

    client.shutdown().unwrap();
}

fn reserve_listen_url() -> String {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let addr = listener.local_addr().unwrap();
    format!("ws://{addr}")
}

fn fake_websocket_codex(temp: &TempDir) -> PathBuf {
    let bin = temp.path().join("bin");
    fs::create_dir_all(&bin).unwrap();
    let codex = bin.join("codex");
    let codex_home = temp.path().join("codex-home");
    fs::create_dir_all(&codex_home).unwrap();
    let script = r#"#!/bin/sh
set -eu
if [ "${1:-}" != "app-server" ] || [ "${2:-}" != "--listen" ]; then
  printf '%s\n' "unexpected args: $*" >&2
  exit 2
fi
python3 - "$3" "__CODEX_HOME__" <<'PY'
import base64
import hashlib
import json
import socket
import sys
from urllib.parse import urlparse

url = urlparse(sys.argv[1])
codex_home = sys.argv[2]
server = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
server.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
server.bind((url.hostname, url.port))
server.listen(1)
conn, _ = server.accept()

request = b""
while b"\r\n\r\n" not in request:
    chunk = conn.recv(1)
    if not chunk:
        raise SystemExit("handshake closed")
    request += chunk
key = ""
for line in request.decode().split("\r\n"):
    if line.lower().startswith("sec-websocket-key:"):
        key = line.split(":", 1)[1].strip()
accept = base64.b64encode(hashlib.sha1((key + "258EAFA5-E914-47DA-95CA-C5AB0DC85B11").encode()).digest()).decode()
conn.sendall((
    "HTTP/1.1 101 Switching Protocols\r\n"
    "Upgrade: websocket\r\n"
    "Connection: Upgrade\r\n"
    f"Sec-WebSocket-Accept: {{accept}}\r\n"
    "\r\n"
).encode())

def recv_frame():
    header = conn.recv(2)
    if not header:
        return None
    length = header[1] & 0x7F
    masked = header[1] & 0x80
    if length == 126:
        length = int.from_bytes(conn.recv(2), "big")
    elif length == 127:
        length = int.from_bytes(conn.recv(8), "big")
    mask = conn.recv(4) if masked else b"\x00\x00\x00\x00"
    payload = bytearray()
    while len(payload) < length:
        payload.extend(conn.recv(length - len(payload)))
    if masked:
        for index in range(len(payload)):
            payload[index] ^= mask[index % 4]
    return json.loads(payload.decode())

def send_frame(value):
    payload = json.dumps(value, separators=(",", ":")).encode()
    frame = bytearray([0x81])
    if len(payload) < 126:
        frame.append(len(payload))
    elif len(payload) <= 65535:
        frame.append(126)
        frame.extend(len(payload).to_bytes(2, "big"))
    else:
        frame.append(127)
        frame.extend(len(payload).to_bytes(8, "big"))
    frame.extend(payload)
    conn.sendall(frame)

init = recv_frame()
send_frame({"id": init["id"], "result": {"userAgent": "fake-codex-ws/0.0.0", "codexHome": codex_home, "platformFamily": "unix", "platformOs": "macos"}})
recv_frame()
thread = recv_frame()
send_frame({"id": thread["id"], "result": {"thread": {"id": "thread-ws-1", "sessionId": "thread-ws-1", "preview": "", "status": "running", "cwd": codex_home, "name": None}, "model": "fake-model", "modelProvider": "fake-provider", "serviceTier": None, "cwd": codex_home}})
turn = recv_frame()
send_frame({"id": turn["id"], "result": {"turn": {"id": "turn-ws-1", "status": "running", "startedAt": 0, "completedAt": None, "durationMs": None}}})
send_frame({"method": "item/agentMessage/delta", "params": {"threadId": "thread-ws-1", "turnId": "turn-ws-1", "itemId": "item-1", "delta": "completed over websocket"}})
send_frame({"method": "turn/completed", "params": {"threadId": "thread-ws-1", "turn": {"id": "turn-ws-1", "status": "completed", "startedAt": 0, "completedAt": 1, "durationMs": 1}}})
conn.close()
server.close()
PY
"#
    .replace("__CODEX_HOME__", &codex_home.to_string_lossy());
    fs::write(&codex, script).unwrap();
    let mut permissions = fs::metadata(&codex).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&codex, permissions).unwrap();
    codex
}
