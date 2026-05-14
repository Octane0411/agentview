use serde_json::Value;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};
use tempfile::TempDir;

const THREAD_ID: &str = "019e21d5-4369-7010-b2f7-fcc3b2b66ca9";

#[test]
fn dispatch_peek_attach_and_remove_with_fake_codex() {
    let env = TestEnv::new();
    let repo = env.git_repo();
    let codex = env.fake_codex();
    let store = TempDir::new().unwrap();

    let output = env
        .agentview(&store, &codex)
        .args([
            "run",
            "--cwd",
            repo.to_str().unwrap(),
            "write a fake summary",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let job_id = stdout
        .lines()
        .find_map(|line| line.strip_prefix("backgrounded"))
        .and_then(|line| line.split_whitespace().next())
        .expect("job id in run output")
        .to_string();

    wait_until(Duration::from_secs(5), || {
        let output = env.agentview(&store, &codex).arg("list").output().unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        stdout.contains(&job_id) && stdout.contains("completed")
    });

    let peek = env
        .agentview(&store, &codex)
        .args(["peek", &job_id])
        .output()
        .unwrap();
    assert!(peek.status.success());
    let peek_stdout = String::from_utf8_lossy(&peek.stdout);
    assert!(peek_stdout.contains(THREAD_ID));
    assert!(peek_stdout.contains("completed fake codex"));

    let attach = env
        .agentview(&store, &codex)
        .args(["attach", &job_id])
        .output()
        .unwrap();
    assert!(attach.status.success());
    assert!(String::from_utf8_lossy(&attach.stdout).contains("interactive resume"));

    let store_json: Value =
        serde_json::from_str(&fs::read_to_string(store.path().join("agentview.json")).unwrap())
            .unwrap();
    let worktree_path = store_json["jobs"][&job_id]["worktreePath"]
        .as_str()
        .expect("worktree path");
    assert!(Path::new(worktree_path).join("codex-output.txt").exists());

    let remove_dirty = env
        .agentview(&store, &codex)
        .args(["rm", &job_id])
        .output()
        .unwrap();
    assert!(!remove_dirty.status.success());
    assert!(String::from_utf8_lossy(&remove_dirty.stderr).contains("uncommitted changes"));

    let remove_force = env
        .agentview(&store, &codex)
        .args(["rm", "--force", "--purge", &job_id])
        .output()
        .unwrap();
    assert!(
        remove_force.status.success(),
        "{}",
        String::from_utf8_lossy(&remove_force.stderr)
    );
}

#[test]
fn app_server_dispatch_uses_thread_and_turn_start() {
    let env = TestEnv::new();
    let repo = env.git_repo();
    let codex = env.fake_codex();
    let store = TempDir::new().unwrap();

    let output = env
        .agentview(&store, &codex)
        .args([
            "run",
            "--app-server",
            "--cwd",
            repo.to_str().unwrap(),
            "write a fake app-server summary",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let job_id = stdout
        .lines()
        .find_map(|line| line.strip_prefix("backgrounded"))
        .and_then(|line| line.split_whitespace().next())
        .expect("job id in run output")
        .to_string();

    wait_until(Duration::from_secs(5), || {
        let output = env.agentview(&store, &codex).arg("list").output().unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        stdout.contains(&job_id) && stdout.contains("completed")
    });

    let peek = env
        .agentview(&store, &codex)
        .args(["peek", &job_id])
        .output()
        .unwrap();
    assert!(peek.status.success());
    let peek_stdout = String::from_utf8_lossy(&peek.stdout);
    assert!(peek_stdout.contains(THREAD_ID));
    assert!(peek_stdout.contains("completed fake app-server"));

    let logs = env
        .agentview(&store, &codex)
        .args(["logs", &job_id])
        .output()
        .unwrap();
    assert!(logs.status.success());
    let logs_stdout = String::from_utf8_lossy(&logs.stdout);
    assert!(logs_stdout.contains("app_server_thread_started"));
    assert!(logs_stdout.contains("app_server_turn_started"));
    assert!(!logs_stdout.contains("started fake codex"));

    let store_json: Value =
        serde_json::from_str(&fs::read_to_string(store.path().join("agentview.json")).unwrap())
            .unwrap();
    assert_eq!(store_json["jobs"][&job_id]["backend"], "app_server");
}

struct TestEnv {
    root: TempDir,
}

impl TestEnv {
    fn new() -> Self {
        Self {
            root: TempDir::new().unwrap(),
        }
    }

    fn agentview(&self, store: &TempDir, codex: &Path) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_agentview"));
        let fake_bin = codex.parent().unwrap();
        let path = format!(
            "{}:{}",
            fake_bin.display(),
            std::env::var("PATH").unwrap_or_default()
        );
        command
            .env("AGENTVIEW_HOME", store.path())
            .env("PATH", path)
            .env("NO_COLOR", "1");
        command
    }

    fn git_repo(&self) -> PathBuf {
        let repo = self.root.path().join("repo");
        fs::create_dir_all(&repo).unwrap();
        fs::write(repo.join("README.md"), "test repo\n").unwrap();
        run("git", &["init"], &repo);
        run("git", &["config", "user.email", "test@example.com"], &repo);
        run("git", &["config", "user.name", "Test User"], &repo);
        run("git", &["add", "README.md"], &repo);
        run("git", &["commit", "-m", "init"], &repo);
        repo
    }

    fn fake_codex(&self) -> PathBuf {
        let bin = self.root.path().join("bin");
        fs::create_dir_all(&bin).unwrap();
        let codex = bin.join("codex");
        let codex_home = self.root.path().join("codex-home");
        fs::create_dir_all(&codex_home).unwrap();
        fs::write(
            &codex,
            format!(
                r#"#!/bin/sh
set -eu
if [ "${{1:-}}" = "app-server" ]; then
  if [ "${{2:-}}" != "--listen" ] || [ "${{3:-}}" != "stdio://" ]; then
    printf '%s\n' "unexpected app-server args: $*" >&2
    exit 2
  fi
  cwd="$(pwd)"
  IFS= read -r init
  case "$init" in
    *'"method":"initialize"'*) ;;
    *) printf '%s\n' "expected initialize, got: $init" >&2; exit 3 ;;
  esac
  printf '%s\n' '{{"id":0,"result":{{"userAgent":"fake-codex/0.0.0","codexHome":"{codex_home}","platformFamily":"unix","platformOs":"macos"}}}}'

  IFS= read -r initialized
  case "$initialized" in
    *'"method":"initialized"'*) ;;
    *) printf '%s\n' "expected initialized, got: $initialized" >&2; exit 4 ;;
  esac

  IFS= read -r thread_start
  case "$thread_start" in
    *'"method":"thread/start"'*) ;;
    *) printf '%s\n' "expected thread/start, got: $thread_start" >&2; exit 5 ;;
  esac
  printf '%s\n' "{{\"method\":\"thread/started\",\"params\":{{\"thread\":{{\"id\":\"{THREAD_ID}\",\"sessionId\":\"{THREAD_ID}\",\"preview\":\"\",\"status\":\"running\",\"cwd\":\"$cwd\",\"name\":null}}}}}}"
  printf '%s\n' "{{\"id\":1,\"result\":{{\"thread\":{{\"id\":\"{THREAD_ID}\",\"sessionId\":\"{THREAD_ID}\",\"preview\":\"\",\"status\":\"running\",\"cwd\":\"$cwd\",\"name\":null}},\"model\":\"fake-model\",\"modelProvider\":\"fake-provider\",\"serviceTier\":null,\"cwd\":\"$cwd\"}}}}"

  IFS= read -r turn_start
  case "$turn_start" in
    *'"method":"turn/start"'*) ;;
    *) printf '%s\n' "expected turn/start, got: $turn_start" >&2; exit 6 ;;
  esac
  printf '%s\n' '{{"id":2,"result":{{"turn":{{"id":"turn-1","status":"running","startedAt":0,"completedAt":null,"durationMs":null}}}}}}'
  printf '%s\n' '{{"method":"turn/started","params":{{"threadId":"{THREAD_ID}","turn":{{"id":"turn-1","status":"running","startedAt":0,"completedAt":null,"durationMs":null}}}}}}'
  printf '%s\n' '{{"method":"item/agentMessage/delta","params":{{"threadId":"{THREAD_ID}","turnId":"turn-1","itemId":"item-1","delta":"completed fake app-server"}}}}'
  printf '%s\n' '{{"method":"turn/completed","params":{{"threadId":"{THREAD_ID}","turn":{{"id":"turn-1","status":"completed","startedAt":0,"completedAt":1,"durationMs":1}}}}}}'
  exit 0
fi
if [ "${{1:-}}" = "exec" ]; then
  if [ "${{2:-}}" = "resume" ]; then
    printf '%s\n' '{{"type":"thread.started","thread_id":"{THREAD_ID}","message":"resumed fake codex"}}'
    printf '%s\n' '{{"type":"message","message":"resume complete"}}'
    exit 0
  fi
  printf '%s\n' '{{"type":"thread.started","thread_id":"{THREAD_ID}","message":"started fake codex"}}'
  printf '%s\n' '{{"type":"message","message":"completed fake codex"}}'
  printf '%s\n' fake-output > codex-output.txt
  exit 0
fi
if [ "${{1:-}}" = "resume" ]; then
  printf '%s\n' "interactive resume $*"
  exit 0
fi
printf '%s\n' "unknown fake codex command: $*" >&2
exit 2
"#,
                codex_home = codex_home.display()
            ),
        )
        .unwrap();
        let mut permissions = fs::metadata(&codex).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&codex, permissions).unwrap();
        codex
    }
}

fn run(command: &str, args: &[&str], cwd: &Path) {
    let output = Command::new(command)
        .args(args)
        .current_dir(cwd)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{} {} failed\nstdout:\n{}\nstderr:\n{}",
        command,
        args.join(" "),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn wait_until(timeout: Duration, mut predicate: impl FnMut() -> bool) {
    let started = Instant::now();
    while started.elapsed() < timeout {
        if predicate() {
            return;
        }
        thread::sleep(Duration::from_millis(100));
    }
    panic!("condition was not met within {:?}", timeout);
}
