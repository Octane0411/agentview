use agentview_codex_app_server::AppServerClient;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::Command;
use tempfile::TempDir;

#[test]
fn initializes_over_stdio_jsonl() {
    let temp = TempDir::new().unwrap();
    let codex = fake_codex(&temp);

    let mut command = Command::new(&codex);
    command.args(["app-server", "--listen", "stdio://"]);

    let mut client = AppServerClient::spawn_with_command(command).unwrap();
    let initialized = client.initialize().unwrap();

    assert_eq!(initialized.user_agent, "fake-codex/0.0.0");
    assert_eq!(initialized.platform_family, "unix");
    assert_eq!(initialized.platform_os, "macos");

    let notifications = client.drain_notifications();
    assert_eq!(notifications.len(), 1);
    assert_eq!(notifications[0].method, "thread/status/changed");

    client.shutdown().unwrap();
}

#[test]
fn interrupt_turn_sends_thread_and_turn_ids() {
    let temp = TempDir::new().unwrap();
    let codex = fake_interrupt_codex(&temp);

    let mut command = Command::new(&codex);
    command.args(["app-server", "--listen", "stdio://"]);

    let mut client = AppServerClient::spawn_with_command(command).unwrap();
    client.initialize().unwrap();
    client.interrupt_turn("thread-1", "turn-1").unwrap();
    client.shutdown().unwrap();
}

fn fake_codex(temp: &TempDir) -> PathBuf {
    let codex = temp.path().join("codex");
    let codex_home = temp.path().join("codex-home");
    fs::create_dir_all(&codex_home).unwrap();
    fs::write(
        &codex,
        format!(
            r#"#!/bin/sh
set -eu
if [ "${{1:-}}" != "app-server" ] || [ "${{2:-}}" != "--listen" ] || [ "${{3:-}}" != "stdio://" ]; then
  printf '%s\n' "unexpected args: $*" >&2
  exit 2
fi

IFS= read -r init
case "$init" in
  *'"method":"initialize"'*) ;;
  *) printf '%s\n' "expected initialize, got: $init" >&2; exit 3 ;;
esac

printf '%s\n' '{{"method":"thread/status/changed","params":{{"threadId":"fake-thread","status":"running"}}}}'
printf '%s\n' '{{"id":0,"result":{{"userAgent":"fake-codex/0.0.0","codexHome":"{}","platformFamily":"unix","platformOs":"macos"}}}}'

IFS= read -r initialized
case "$initialized" in
  *'"method":"initialized"'*) ;;
  *) printf '%s\n' "expected initialized, got: $initialized" >&2; exit 4 ;;
esac
"#,
            codex_home.display()
        ),
    )
    .unwrap();
    let mut permissions = fs::metadata(&codex).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&codex, permissions).unwrap();
    codex
}

fn fake_interrupt_codex(temp: &TempDir) -> PathBuf {
    let codex = temp.path().join("codex-interrupt");
    let codex_home = temp.path().join("codex-home");
    fs::create_dir_all(&codex_home).unwrap();
    fs::write(
        &codex,
        format!(
            r#"#!/bin/sh
set -eu
if [ "${{1:-}}" != "app-server" ] || [ "${{2:-}}" != "--listen" ] || [ "${{3:-}}" != "stdio://" ]; then
  printf '%s\n' "unexpected args: $*" >&2
  exit 2
fi

IFS= read -r init
case "$init" in
  *'"method":"initialize"'*) ;;
  *) printf '%s\n' "expected initialize, got: $init" >&2; exit 3 ;;
esac
printf '%s\n' '{{"id":0,"result":{{"userAgent":"fake-codex/0.0.0","codexHome":"{}","platformFamily":"unix","platformOs":"macos"}}}}'

IFS= read -r initialized
case "$initialized" in
  *'"method":"initialized"'*) ;;
  *) printf '%s\n' "expected initialized, got: $initialized" >&2; exit 4 ;;
esac

IFS= read -r interrupt
case "$interrupt" in
  *'"method":"turn/interrupt"'*'"threadId":"thread-1"'*'"turnId":"turn-1"'*) ;;
  *) printf '%s\n' "expected turn/interrupt, got: $interrupt" >&2; exit 5 ;;
esac
printf '%s\n' '{{"id":1,"result":{{}}}}'
"#,
            codex_home.display()
        ),
    )
    .unwrap();
    let mut permissions = fs::metadata(&codex).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&codex, permissions).unwrap();
    codex
}
