use serde_json::Value;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
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
            "--fallback-exec",
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
    let list = env.agentview(&store, &codex).arg("list").output().unwrap();
    assert!(list.status.success());
    assert!(String::from_utf8_lossy(&list.stdout).contains("pr:green"));

    let peek = env
        .agentview(&store, &codex)
        .args(["peek", &job_id])
        .output()
        .unwrap();
    assert!(peek.status.success());
    let peek_stdout = String::from_utf8_lossy(&peek.stdout);
    assert!(peek_stdout.contains(THREAD_ID));
    assert!(peek_stdout.contains("completed fake app-server"));
    assert!(peek_stdout.contains("https://github.com/acme/app/pull/42 [green]"));

    let logs = env
        .agentview(&store, &codex)
        .args(["logs", &job_id])
        .output()
        .unwrap();
    assert!(logs.status.success());
    let logs_stdout = String::from_utf8_lossy(&logs.stdout);
    assert!(logs_stdout.contains("supervisor_app_server_turn_queued"));
    assert!(logs_stdout.contains("app_server_thread_started"));
    assert!(logs_stdout.contains("app_server_turn_started"));
    assert!(!logs_stdout.contains("started fake codex"));

    let reply = env
        .agentview(&store, &codex)
        .args(["reply", &job_id, "follow up through app-server"])
        .output()
        .unwrap();
    assert!(
        reply.status.success(),
        "{}",
        String::from_utf8_lossy(&reply.stderr)
    );

    wait_until(Duration::from_secs(5), || {
        let output = env
            .agentview(&store, &codex)
            .args(["peek", &job_id])
            .output()
            .unwrap();
        String::from_utf8_lossy(&output.stdout).contains("completed fake app-server reply")
    });

    let logs = env
        .agentview(&store, &codex)
        .args(["logs", &job_id])
        .output()
        .unwrap();
    assert!(logs.status.success());
    let logs_stdout = String::from_utf8_lossy(&logs.stdout);
    assert!(logs_stdout.contains("app_server_thread_resumed"));
    assert!(!logs_stdout.contains("resumed fake codex"));

    let store_json: Value =
        serde_json::from_str(&fs::read_to_string(store.path().join("agentview.json")).unwrap())
            .unwrap();
    assert_eq!(store_json["jobs"][&job_id]["backend"], "app_server");
    assert_eq!(store_json["jobs"][&job_id]["prRefs"][0]["number"], 42);
    assert_eq!(store_json["jobs"][&job_id]["prRefs"][0]["status"], "green");
    let worktree_path = store_json["jobs"][&job_id]["worktreePath"]
        .as_str()
        .expect("worktree path");

    let (hosted_helper, hosted_log) = env.fake_hosted_helper();
    let hosted = env
        .agentview(&store, &codex)
        .env("AGENTVIEW_CODEX_HOSTED", hosted_helper)
        .args(["attach", &job_id])
        .output()
        .unwrap();
    assert!(
        hosted.status.success(),
        "{}",
        String::from_utf8_lossy(&hosted.stderr)
    );
    wait_until(Duration::from_secs(5), || hosted_log.exists());
    let hosted_args = fs::read_to_string(hosted_log).unwrap();
    assert!(hosted_args.contains(&format!("--thread-id {THREAD_ID}")));
    assert!(hosted_args.contains(&format!("--cwd {worktree_path}")));
    assert!(!hosted_args.contains("--no-alt-screen"));

    let logs = env
        .agentview(&store, &codex)
        .args(["logs", &job_id])
        .output()
        .unwrap();
    assert!(logs.status.success());
    assert!(String::from_utf8_lossy(&logs.stdout).contains("hosted_attach_detached"));

    let shutdown = env
        .agentview(&store, &codex)
        .arg("__supervisor-shutdown")
        .output()
        .unwrap();
    assert!(
        shutdown.status.success(),
        "{}",
        String::from_utf8_lossy(&shutdown.stderr)
    );
}

#[test]
fn respawn_all_restarts_stopped_app_server_jobs() {
    let env = TestEnv::new();
    let repo = env.git_repo();
    let codex = env.fake_codex();
    let store = TempDir::new().unwrap();

    let first = run_app_server_job(&env, &store, &codex, &repo, "first stopped task");
    let second = run_app_server_job(&env, &store, &codex, &repo, "second stopped task");

    for job_id in [&first, &second] {
        wait_until(Duration::from_secs(5), || {
            let output = env.agentview(&store, &codex).arg("list").output().unwrap();
            let stdout = String::from_utf8_lossy(&output.stdout);
            stdout.contains(job_id) && stdout.contains("completed")
        });
        let stop = env
            .agentview(&store, &codex)
            .args(["stop", job_id])
            .output()
            .unwrap();
        assert!(
            stop.status.success(),
            "{}",
            String::from_utf8_lossy(&stop.stderr)
        );
    }

    wait_until(Duration::from_secs(5), || {
        [&first, &second].iter().all(|job_id| {
            let output = env
                .agentview(&store, &codex)
                .args(["peek", job_id])
                .output()
                .unwrap();
            String::from_utf8_lossy(&output.stdout).contains(&format!("{job_id}  stopped"))
        })
    });

    let respawn = env
        .agentview(&store, &codex)
        .args(["respawn", "--all"])
        .output()
        .unwrap();
    assert!(
        respawn.status.success(),
        "{}",
        String::from_utf8_lossy(&respawn.stderr)
    );
    let stdout = String::from_utf8_lossy(&respawn.stdout);
    assert!(stdout.contains(&format!("respawned {first}")));
    assert!(stdout.contains(&format!("respawned {second}")));

    for job_id in [&first, &second] {
        wait_until(Duration::from_secs(5), || {
            let output = env
                .agentview(&store, &codex)
                .args(["peek", job_id])
                .output()
                .unwrap();
            String::from_utf8_lossy(&output.stdout).contains("completed fake app-server reply")
        });
    }

    let shutdown = env
        .agentview(&store, &codex)
        .arg("__supervisor-shutdown")
        .output()
        .unwrap();
    assert!(
        shutdown.status.success(),
        "{}",
        String::from_utf8_lossy(&shutdown.stderr)
    );
}

#[test]
fn app_server_stop_routes_turn_interrupt_through_supervisor() {
    let env = TestEnv::new();
    let repo = env.git_repo();
    let codex = env.fake_slow_app_server_codex();
    let store = TempDir::new().unwrap();

    let output = env
        .agentview(&store, &codex)
        .args([
            "run",
            "--cwd",
            repo.to_str().unwrap(),
            "start a slow fake app-server task",
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
        let output = env
            .agentview(&store, &codex)
            .args(["peek", &job_id])
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        stdout.contains("slow fake app-server running") && stdout.contains("turn: turn-1")
    });

    let stop = env
        .agentview(&store, &codex)
        .args(["stop", &job_id])
        .output()
        .unwrap();
    assert!(
        stop.status.success(),
        "{}",
        String::from_utf8_lossy(&stop.stderr)
    );

    wait_until(Duration::from_secs(5), || {
        let output = env.agentview(&store, &codex).arg("list").output().unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        stdout.contains(&job_id) && stdout.contains("stopped")
    });

    wait_until(Duration::from_secs(5), || {
        let output = env
            .agentview(&store, &codex)
            .args(["logs", &job_id])
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        stdout.contains("supervisor_app_server_turn_interrupt_requested")
            && stdout.contains("supervisor_app_server_turn_interrupt_sent")
    });

    let logs = env
        .agentview(&store, &codex)
        .args(["logs", &job_id])
        .output()
        .unwrap();
    assert!(logs.status.success());
    let logs_stdout = String::from_utf8_lossy(&logs.stdout);
    assert!(logs_stdout.contains("supervisor_app_server_turn_interrupt_requested"));
    assert!(logs_stdout.contains("supervisor_app_server_turn_interrupt_sent"));

    let shutdown = env
        .agentview(&store, &codex)
        .arg("__supervisor-shutdown")
        .output()
        .unwrap();
    assert!(
        shutdown.status.success(),
        "{}",
        String::from_utf8_lossy(&shutdown.stderr)
    );
}

#[test]
fn running_app_server_request_can_be_answered_from_list() {
    let env = TestEnv::new();
    let repo = env.git_repo();
    let codex = env.fake_input_app_server_codex();
    let store = TempDir::new().unwrap();

    let output = env
        .agentview(&store, &codex)
        .args([
            "run",
            "--cwd",
            repo.to_str().unwrap(),
            "ask for confirmation through app-server",
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
        stdout.contains(&job_id) && stdout.contains("needs_input")
    });

    let peek = env
        .agentview(&store, &codex)
        .args(["peek", &job_id])
        .output()
        .unwrap();
    assert!(peek.status.success());
    let peek_stdout = String::from_utf8_lossy(&peek.stdout);
    assert!(peek_stdout.contains("needs input: Continue?"));

    let reply = env
        .agentview(&store, &codex)
        .args(["reply", &job_id, "yes"])
        .output()
        .unwrap();
    assert!(
        reply.status.success(),
        "{}",
        String::from_utf8_lossy(&reply.stderr)
    );

    wait_until(Duration::from_secs(5), || {
        let output = env
            .agentview(&store, &codex)
            .args(["peek", &job_id])
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        stdout.contains("completed after answer yes") && stdout.contains("completed")
    });

    let logs = env
        .agentview(&store, &codex)
        .args(["logs", &job_id])
        .output()
        .unwrap();
    assert!(logs.status.success());
    let logs_stdout = String::from_utf8_lossy(&logs.stdout);
    assert!(logs_stdout.contains("app_server_request"));
    assert!(logs_stdout.contains("agentview_server_request_reply_sent"));
    assert!(logs_stdout.contains("supervisor_server_request_resolve_requested"));
    assert!(logs_stdout.contains("supervisor_server_request_resolved"));
    assert!(logs_stdout.contains("serverRequest/resolved"));

    let shutdown = env
        .agentview(&store, &codex)
        .arg("__supervisor-shutdown")
        .output()
        .unwrap();
    assert!(
        shutdown.status.success(),
        "{}",
        String::from_utf8_lossy(&shutdown.stderr)
    );
}

#[test]
fn running_app_server_approval_can_be_accepted_from_list() {
    let env = TestEnv::new();
    let repo = env.git_repo();
    let codex = env.fake_approval_app_server_codex();
    let store = TempDir::new().unwrap();

    let output = env
        .agentview(&store, &codex)
        .args([
            "run",
            "--cwd",
            repo.to_str().unwrap(),
            "ask for command approval through app-server",
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
        stdout.contains(&job_id) && stdout.contains("needs_input")
    });

    let approve = env
        .agentview(&store, &codex)
        .args(["approve", &job_id])
        .output()
        .unwrap();
    assert!(
        approve.status.success(),
        "{}",
        String::from_utf8_lossy(&approve.stderr)
    );

    wait_until(Duration::from_secs(5), || {
        let output = env
            .agentview(&store, &codex)
            .args(["peek", &job_id])
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        stdout.contains("command approved from list") && stdout.contains("completed")
    });

    let logs = env
        .agentview(&store, &codex)
        .args(["logs", &job_id])
        .output()
        .unwrap();
    assert!(logs.status.success());
    let logs_stdout = String::from_utf8_lossy(&logs.stdout);
    assert!(logs_stdout.contains("item/commandExecution/requestApproval"));
    assert!(logs_stdout.contains("\"decision\":\"accept\""));

    let shutdown = env
        .agentview(&store, &codex)
        .arg("__supervisor-shutdown")
        .output()
        .unwrap();
    assert!(
        shutdown.status.success(),
        "{}",
        String::from_utf8_lossy(&shutdown.stderr)
    );
}

#[test]
fn running_app_server_attach_passes_supervisor_websocket_endpoint() {
    let env = TestEnv::new();
    let repo = env.git_repo();
    let codex = env.fake_websocket_slow_app_server_codex();
    let store = TempDir::new().unwrap();

    let output = env
        .agentview_default_transport(&store, &codex)
        .args([
            "run",
            "--cwd",
            repo.to_str().unwrap(),
            "start a websocket fake app-server task",
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
        let output = env
            .agentview_default_transport(&store, &codex)
            .args(["peek", &job_id])
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        stdout.contains("websocket fake app-server running") && stdout.contains("turn: turn-ws-1")
    });

    let (hosted_helper, hosted_log) = env.fake_hosted_helper();
    let hosted = env
        .agentview_default_transport(&store, &codex)
        .env("AGENTVIEW_CODEX_HOSTED", hosted_helper)
        .args(["attach", &job_id])
        .output()
        .unwrap();
    assert!(
        hosted.status.success(),
        "{}",
        String::from_utf8_lossy(&hosted.stderr)
    );
    let hosted_args = fs::read_to_string(hosted_log).unwrap();
    assert!(hosted_args.contains(&format!("--thread-id {THREAD_ID}")));
    assert!(hosted_args.contains("--app-server-url ws://127.0.0.1:"));
    assert!(!hosted_args.contains("--remote-url"));

    let stop = env
        .agentview_default_transport(&store, &codex)
        .args(["stop", &job_id])
        .output()
        .unwrap();
    assert!(
        stop.status.success(),
        "{}",
        String::from_utf8_lossy(&stop.stderr)
    );
    wait_until(Duration::from_secs(5), || {
        let output = env
            .agentview_default_transport(&store, &codex)
            .arg("list")
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        stdout.contains(&job_id) && stdout.contains("stopped")
    });

    let shutdown = env
        .agentview_default_transport(&store, &codex)
        .arg("__supervisor-shutdown")
        .output()
        .unwrap();
    assert!(
        shutdown.status.success(),
        "{}",
        String::from_utf8_lossy(&shutdown.stderr)
    );
}

#[test]
fn running_needs_input_attach_uses_hosted_endpoint_and_can_still_reply() {
    let env = TestEnv::new();
    let repo = env.git_repo();
    let codex = env.fake_websocket_input_app_server_codex();
    let store = TempDir::new().unwrap();

    let output = env
        .agentview_default_transport(&store, &codex)
        .args([
            "run",
            "--cwd",
            repo.to_str().unwrap(),
            "start a websocket request-user-input task",
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
        let output = env
            .agentview_default_transport(&store, &codex)
            .arg("list")
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        stdout.contains(&job_id) && stdout.contains("needs_input")
    });

    let (hosted_helper, hosted_log) = env.fake_hosted_helper();
    let hosted = env
        .agentview_default_transport(&store, &codex)
        .env("AGENTVIEW_CODEX_HOSTED", hosted_helper)
        .args(["attach", &job_id])
        .output()
        .unwrap();
    assert!(
        hosted.status.success(),
        "{}",
        String::from_utf8_lossy(&hosted.stderr)
    );
    let hosted_args = fs::read_to_string(hosted_log).unwrap();
    assert!(hosted_args.contains(&format!("--thread-id {THREAD_ID}")));
    assert!(hosted_args.contains("--app-server-url ws://127.0.0.1:"));

    let reply = env
        .agentview_default_transport(&store, &codex)
        .args(["reply", &job_id, "yes"])
        .output()
        .unwrap();
    assert!(
        reply.status.success(),
        "{}",
        String::from_utf8_lossy(&reply.stderr)
    );

    wait_until(Duration::from_secs(5), || {
        let output = env
            .agentview_default_transport(&store, &codex)
            .args(["peek", &job_id])
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        stdout.contains("websocket completed after answer yes") && stdout.contains("completed")
    });

    let logs = env
        .agentview_default_transport(&store, &codex)
        .args(["logs", &job_id])
        .output()
        .unwrap();
    assert!(logs.status.success());
    let logs_stdout = String::from_utf8_lossy(&logs.stdout);
    assert!(logs_stdout.contains("hosted_attach_detached"));
    assert!(logs_stdout.contains("agentview_server_request_reply_sent"));
    assert!(!logs_stdout.contains("hosted_attach_quit"));
    assert!(!logs_stdout.contains("conversation interrupted"));

    let shutdown = env
        .agentview_default_transport(&store, &codex)
        .arg("__supervisor-shutdown")
        .output()
        .unwrap();
    assert!(
        shutdown.status.success(),
        "{}",
        String::from_utf8_lossy(&shutdown.stderr)
    );
}

#[test]
fn running_attach_defers_while_mcp_startup_is_pending() {
    let env = TestEnv::new();
    let repo = env.git_repo();
    let codex = env.fake_websocket_input_app_server_codex_with_pending_mcp();
    let store = TempDir::new().unwrap();

    let output = env
        .agentview_default_transport(&store, &codex)
        .args([
            "run",
            "--cwd",
            repo.to_str().unwrap(),
            "start a websocket request-user-input task while MCP is booting",
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
        let output = env
            .agentview_default_transport(&store, &codex)
            .arg("list")
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        stdout.contains(&job_id) && stdout.contains("needs_input")
    });

    let (hosted_helper, hosted_log) = env.fake_hosted_helper();
    let hosted = env
        .agentview_default_transport(&store, &codex)
        .env("AGENTVIEW_CODEX_HOSTED", hosted_helper)
        .args(["attach", &job_id])
        .output()
        .unwrap();
    assert!(!hosted.status.success());
    assert!(
        String::from_utf8_lossy(&hosted.stderr)
            .contains("Session is still booting MCP server: codex_apps")
    );
    assert!(!hosted_log.exists());

    let reply = env
        .agentview_default_transport(&store, &codex)
        .args(["reply", &job_id, "yes"])
        .output()
        .unwrap();
    assert!(
        reply.status.success(),
        "{}",
        String::from_utf8_lossy(&reply.stderr)
    );

    wait_until(Duration::from_secs(5), || {
        let output = env
            .agentview_default_transport(&store, &codex)
            .args(["peek", &job_id])
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        stdout.contains("websocket completed after answer yes") && stdout.contains("completed")
    });

    let logs = env
        .agentview_default_transport(&store, &codex)
        .args(["logs", &job_id])
        .output()
        .unwrap();
    assert!(logs.status.success());
    let logs_stdout = String::from_utf8_lossy(&logs.stdout);
    assert!(logs_stdout.contains("hosted_attach_deferred"));
    assert!(!logs_stdout.contains("hosted_attach_started"));

    let shutdown = env
        .agentview_default_transport(&store, &codex)
        .arg("__supervisor-shutdown")
        .output()
        .unwrap();
    assert!(
        shutdown.status.success(),
        "{}",
        String::from_utf8_lossy(&shutdown.stderr)
    );
}

#[test]
fn persistent_hosted_pty_attach_detaches_without_exiting_helper_immediately() {
    if Command::new("expect").arg("-v").output().is_err() {
        eprintln!("skipping persistent PTY integration: expect is not installed");
        return;
    }

    let env = TestEnv::new();
    let repo = env.git_repo();
    let codex = env.fake_codex();
    let store = TempDir::new().unwrap();

    let job_id = run_app_server_job(
        &env,
        &store,
        &codex,
        &repo,
        "complete a fake app-server task before persistent attach",
    );
    wait_until(Duration::from_secs(5), || {
        let output = env.agentview(&store, &codex).arg("list").output().unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        stdout.contains(&job_id) && stdout.contains("completed")
    });

    let (hosted_helper, hosted_log) = env.fake_persistent_hosted_helper();
    let expect_script = env.root.path().join("persistent-pty-attach.exp");
    let expect_output = env.root.path().join("persistent-pty-attach.out");
    fs::write(
        &expect_script,
        r#"set timeout 15
match_max 200000
log_user 0
log_file -noappend $env(AGENTVIEW_EXPECT_OUTPUT)
stty rows 42 columns 132
spawn $env(AGENTVIEW_TEST_BIN) attach $env(AGENTVIEW_TEST_JOB)
expect {
  eof { exit 0 }
  timeout { exit 11 }
}
"#,
    )
    .unwrap();

    let fake_bin = codex.parent().unwrap();
    let path = format!(
        "{}:{}",
        fake_bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let attach = Command::new("expect")
        .arg(&expect_script)
        .env("AGENTVIEW_HOME", store.path())
        .env("AGENTVIEW_CODEX_HOSTED", hosted_helper)
        .env("AGENTVIEW_EXPECT_OUTPUT", &expect_output)
        .env("AGENTVIEW_TEST_BIN", env!("CARGO_BIN_EXE_agentview"))
        .env("AGENTVIEW_TEST_JOB", &job_id)
        .env("AGENTVIEW_PERSISTENT_CODEX_TUI", "1")
        .env("COLUMNS", "132")
        .env("LINES", "42")
        .env("NO_COLOR", "1")
        .env("PATH", path)
        .output()
        .unwrap();
    assert!(
        attach.status.success(),
        "persistent PTY attach failed\nstdout:\n{}\nstderr:\n{}\npty:\n{}",
        String::from_utf8_lossy(&attach.stdout),
        String::from_utf8_lossy(&attach.stderr),
        fs::read_to_string(&expect_output).unwrap_or_default()
    );

    if !hosted_log.exists() {
        let logs = env
            .agentview(&store, &codex)
            .args(["logs", &job_id, "200"])
            .output()
            .unwrap();
        panic!(
            "hosted helper was not invoked\npty:\n{}\nlogs:\n{}\nhost log:\n{}\nstderr:\n{}",
            fs::read_to_string(&expect_output).unwrap_or_default(),
            String::from_utf8_lossy(&logs.stdout),
            fs::read_to_string(
                store
                    .path()
                    .join("jobs")
                    .join(&job_id)
                    .join("hosted-pty.log")
            )
            .unwrap_or_default(),
            String::from_utf8_lossy(&logs.stderr)
        );
    }
    let hosted_args = fs::read_to_string(hosted_log).unwrap();
    assert!(hosted_args.contains(&format!("--thread-id {THREAD_ID}")));

    wait_until(Duration::from_secs(5), || {
        let logs = env
            .agentview(&store, &codex)
            .args(["logs", &job_id])
            .output()
            .unwrap();
        let logs_stdout = String::from_utf8_lossy(&logs.stdout);
        logs_stdout.contains("hosted_pty_detached") && logs_stdout.contains("hosted_pty_exited")
    });
}

#[test]
fn remove_purges_persistent_hosted_pty_processes() {
    let env = TestEnv::new();
    let repo = env.git_repo();
    let codex = env.fake_codex();
    let store = TempDir::new().unwrap();

    let (hosted_helper, hosted_log) = env.fake_long_running_hosted_helper();
    let output = env
        .agentview(&store, &codex)
        .env("AGENTVIEW_CODEX_HOSTED", &hosted_helper)
        .env("AGENTVIEW_PERSISTENT_CODEX_TUI", "1")
        .args([
            "run",
            "--cwd",
            repo.to_str().unwrap(),
            "complete a fake app-server task before purge",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let job_id = String::from_utf8_lossy(&output.stdout)
        .lines()
        .find_map(|line| line.strip_prefix("backgrounded"))
        .and_then(|line| line.split_whitespace().next())
        .expect("job id in run output")
        .to_string();

    let job_dir = store.path().join("jobs").join(&job_id);
    let host_pid_path = job_dir.join("hosted-pty.pid");
    let child_pid_path = job_dir.join("hosted-pty-child.pid");
    wait_until(Duration::from_secs(5), || {
        let output = env.agentview(&store, &codex).arg("list").output().unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        stdout.contains(&job_id) && stdout.contains("completed")
    });

    let mut host = env
        .agentview(&store, &codex)
        .env("AGENTVIEW_CODEX_HOSTED", &hosted_helper)
        .args(["__hosted-pty-host", &job_id])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();

    wait_until(Duration::from_secs(5), || {
        hosted_log.exists() && host_pid_path.exists() && child_pid_path.exists()
    });

    let host_pid = read_pid(&host_pid_path);
    let child_pid = read_pid(&child_pid_path);
    assert!(
        process_alive(host_pid),
        "host pid {host_pid} should be alive"
    );
    assert!(
        process_alive(child_pid),
        "child pid {child_pid} should be alive"
    );

    let remove = env
        .agentview(&store, &codex)
        .args(["rm", "--force", "--purge", &job_id])
        .output()
        .unwrap();
    assert!(
        remove.status.success(),
        "{}",
        String::from_utf8_lossy(&remove.stderr)
    );

    let _ = host.wait();
    wait_until(Duration::from_secs(5), || {
        !process_alive(host_pid) && !process_alive(child_pid)
    });
    assert!(!job_dir.exists());
}

#[test]
fn tui_enter_on_needs_input_job_uses_hosted_endpoint_and_can_still_reply() {
    if Command::new("expect").arg("-v").output().is_err() {
        eprintln!("skipping TUI PTY integration: expect is not installed");
        return;
    }

    let env = TestEnv::new();
    let repo = env.git_repo();
    let codex = env.fake_websocket_input_app_server_codex();
    let store = TempDir::new().unwrap();

    let output = env
        .agentview_default_transport(&store, &codex)
        .args([
            "run",
            "--cwd",
            repo.to_str().unwrap(),
            "start a websocket request-user-input task for TUI attach",
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
        let output = env
            .agentview_default_transport(&store, &codex)
            .arg("list")
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        stdout.contains(&job_id) && stdout.contains("needs_input")
    });

    let (hosted_helper, hosted_log) = env.fake_hosted_helper();
    let expect_script = env.root.path().join("tui-needs-input-attach.exp");
    let expect_output = env.root.path().join("tui-needs-input-attach.out");
    fs::write(
        &expect_script,
        r#"set timeout 30
match_max 200000
log_user 0
log_file -noappend $env(AGENTVIEW_EXPECT_OUTPUT)
stty rows 42 columns 132
spawn $env(AGENTVIEW_TEST_BIN)
after 1000
send "\r"
expect {
  eof { exit 0 }
  timeout { exit 11 }
}
"#,
    )
    .unwrap();

    let fake_bin = codex.parent().unwrap();
    let path = format!(
        "{}:{}",
        fake_bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let tui = Command::new("expect")
        .arg(&expect_script)
        .env("AGENTVIEW_HOME", store.path())
        .env("AGENTVIEW_CODEX_HOSTED", hosted_helper)
        .env("AGENTVIEW_EXPECT_OUTPUT", &expect_output)
        .env("AGENTVIEW_TEST_BIN", env!("CARGO_BIN_EXE_agentview"))
        .env("AGENTVIEW_PERSISTENT_CODEX_TUI", "0")
        .env("AGENTVIEW_TUI_EXIT_AFTER_ATTACH", "1")
        .env("COLUMNS", "132")
        .env("LINES", "42")
        .env("NO_COLOR", "1")
        .env("PATH", path)
        .output()
        .unwrap();
    assert!(
        tui.status.success(),
        "TUI attach failed\nstdout:\n{}\nstderr:\n{}\npty:\n{}",
        String::from_utf8_lossy(&tui.stdout),
        String::from_utf8_lossy(&tui.stderr),
        fs::read_to_string(&expect_output).unwrap_or_default()
    );

    let hosted_args = fs::read_to_string(hosted_log).unwrap();
    assert!(hosted_args.contains(&format!("--thread-id {THREAD_ID}")));
    assert!(hosted_args.contains("--app-server-url ws://127.0.0.1:"));

    let reply = env
        .agentview_default_transport(&store, &codex)
        .args(["reply", &job_id, "yes"])
        .output()
        .unwrap();
    assert!(
        reply.status.success(),
        "{}",
        String::from_utf8_lossy(&reply.stderr)
    );

    wait_until(Duration::from_secs(5), || {
        let output = env
            .agentview_default_transport(&store, &codex)
            .args(["peek", &job_id])
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        stdout.contains("websocket completed after answer yes") && stdout.contains("completed")
    });

    let logs = env
        .agentview_default_transport(&store, &codex)
        .args(["logs", &job_id])
        .output()
        .unwrap();
    assert!(logs.status.success());
    let logs_stdout = String::from_utf8_lossy(&logs.stdout);
    assert!(logs_stdout.contains("hosted_attach_detached"));
    assert!(logs_stdout.contains("agentview_list_returned_from_attach"));
    assert!(logs_stdout.contains("agentview_server_request_reply_sent"));
    assert!(!logs_stdout.contains("hosted_attach_quit"));
    assert!(!logs_stdout.contains("conversation interrupted"));

    let shutdown = env
        .agentview_default_transport(&store, &codex)
        .arg("__supervisor-shutdown")
        .output()
        .unwrap();
    assert!(
        shutdown.status.success(),
        "{}",
        String::from_utf8_lossy(&shutdown.stderr)
    );
}

#[test]
fn hidden_supervisor_accepts_ping_over_local_socket() {
    let env = TestEnv::new();
    let codex = env.fake_codex();
    let store = TempDir::new().unwrap();

    let mut child = env
        .agentview(&store, &codex)
        .args(["__supervisor", "--once"])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    let mut pong = String::new();
    wait_until(Duration::from_secs(5), || {
        let output = env
            .agentview(&store, &codex)
            .arg("__supervisor-ping")
            .output()
            .unwrap();
        if output.status.success() {
            pong = String::from_utf8_lossy(&output.stdout).trim().to_string();
            true
        } else {
            false
        }
    });

    assert_eq!(pong, "pong");
    let status = child.wait().unwrap();
    assert!(
        status.success(),
        "supervisor exited with {status}; stderr:\n{}",
        child
            .stderr
            .take()
            .map(|mut stderr| {
                let mut text = String::new();
                let _ = std::io::Read::read_to_string(&mut stderr, &mut text);
                text
            })
            .unwrap_or_default()
    );
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
        let mut command = self.agentview_default_transport(store, codex);
        command.env("AGENTVIEW_APP_SERVER_TRANSPORT", "stdio");
        command
    }

    fn agentview_default_transport(&self, store: &TempDir, codex: &Path) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_agentview"));
        let fake_bin = codex.parent().unwrap();
        let path = format!(
            "{}:{}",
            fake_bin.display(),
            std::env::var("PATH").unwrap_or_default()
        );
        command
            .env("AGENTVIEW_HOME", store.path())
            .env("AGENTVIEW_PERSISTENT_CODEX_TUI", "0")
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

  IFS= read -r thread_request
  case "$thread_request" in
    *'"method":"thread/start"'*) thread_mode="start"; delta="completed fake app-server https://github.com/acme/app/pull/42" ;;
    *'"method":"thread/resume"'*) thread_mode="resume"; delta="completed fake app-server reply" ;;
    *) printf '%s\n' "expected thread/start or thread/resume, got: $thread_request" >&2; exit 5 ;;
  esac
  if [ "$thread_mode" = "start" ]; then
    printf '%s\n' "{{\"method\":\"thread/started\",\"params\":{{\"thread\":{{\"id\":\"{THREAD_ID}\",\"sessionId\":\"{THREAD_ID}\",\"preview\":\"\",\"status\":\"running\",\"cwd\":\"$cwd\",\"name\":null}}}}}}"
  fi
  printf '%s\n' "{{\"id\":1,\"result\":{{\"thread\":{{\"id\":\"{THREAD_ID}\",\"sessionId\":\"{THREAD_ID}\",\"preview\":\"\",\"status\":\"running\",\"cwd\":\"$cwd\",\"name\":null}},\"model\":\"fake-model\",\"modelProvider\":\"fake-provider\",\"serviceTier\":null,\"cwd\":\"$cwd\"}}}}"

  IFS= read -r turn_start
  case "$turn_start" in
    *'"method":"turn/start"'*) ;;
    *) printf '%s\n' "expected turn/start, got: $turn_start" >&2; exit 6 ;;
  esac
  printf '%s\n' '{{"id":2,"result":{{"turn":{{"id":"turn-1","status":"running","startedAt":0,"completedAt":null,"durationMs":null}}}}}}'
  printf '%s\n' '{{"method":"turn/started","params":{{"threadId":"{THREAD_ID}","turn":{{"id":"turn-1","status":"running","startedAt":0,"completedAt":null,"durationMs":null}}}}}}'
  printf '%s\n' "{{\"method\":\"item/agentMessage/delta\",\"params\":{{\"threadId\":\"{THREAD_ID}\",\"turnId\":\"turn-1\",\"itemId\":\"item-1\",\"delta\":\"$delta\"}}}}"
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
        self.write_fake_gh(&bin);
        codex
    }

    fn write_fake_gh(&self, bin: &Path) {
        let gh = bin.join("gh");
        fs::write(
            &gh,
            r#"#!/bin/sh
set -eu
if [ "${1:-}" = "pr" ] && [ "${2:-}" = "view" ]; then
  printf '%s\n' '{"state":"OPEN","isDraft":false,"closed":false,"mergedAt":null,"mergeStateStatus":"CLEAN","reviewDecision":"APPROVED","statusCheckRollup":[{"state":"SUCCESS"}],"url":"https://github.com/acme/app/pull/42"}'
  exit 0
fi
printf '%s\n' "unknown fake gh command: $*" >&2
exit 2
"#,
        )
        .unwrap();
        let mut permissions = fs::metadata(&gh).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&gh, permissions).unwrap();
    }

    fn fake_slow_app_server_codex(&self) -> PathBuf {
        let bin = self.root.path().join("slow-bin");
        fs::create_dir_all(&bin).unwrap();
        let codex = bin.join("codex");
        let codex_home = self.root.path().join("slow-codex-home");
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
printf '%s\n' '{{"method":"item/agentMessage/delta","params":{{"threadId":"{THREAD_ID}","turnId":"turn-1","itemId":"item-1","delta":"slow fake app-server running"}}}}'

IFS= read -r interrupt
case "$interrupt" in
  *'"method":"turn/interrupt"'*'"threadId":"{THREAD_ID}"'*'"turnId":"turn-1"'*) ;;
  *) printf '%s\n' "expected turn/interrupt, got: $interrupt" >&2; exit 7 ;;
esac
printf '%s\n' '{{"id":3,"result":{{}}}}'
printf '%s\n' '{{"method":"turn/completed","params":{{"threadId":"{THREAD_ID}","turn":{{"id":"turn-1","status":"interrupted","startedAt":0,"completedAt":1,"durationMs":1}}}}}}'
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

    fn fake_input_app_server_codex(&self) -> PathBuf {
        let bin = self.root.path().join("input-bin");
        fs::create_dir_all(&bin).unwrap();
        let codex = bin.join("codex");
        let codex_home = self.root.path().join("input-codex-home");
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
printf '%s\n' '{{"id":"req-1","method":"item/tool/requestUserInput","params":{{"threadId":"{THREAD_ID}","turnId":"turn-1","itemId":"call1","questions":[{{"id":"confirm_path","header":"Confirm","question":"Continue?","isOther":false,"isSecret":false,"options":null}}]}}}}'

IFS= read -r answer
case "$answer" in
  *'"id":"req-1"'*'"result"'*'"confirm_path"'*'"answers":["yes"]'*) ;;
  *) printf '%s\n' "expected request-user-input response, got: $answer" >&2; exit 7 ;;
esac
printf '%s\n' '{{"method":"serverRequest/resolved","params":{{"requestId":"req-1"}}}}'
printf '%s\n' '{{"method":"item/agentMessage/delta","params":{{"threadId":"{THREAD_ID}","turnId":"turn-1","itemId":"item-1","delta":"completed after answer yes"}}}}'
printf '%s\n' '{{"method":"turn/completed","params":{{"threadId":"{THREAD_ID}","turn":{{"id":"turn-1","status":"completed","startedAt":0,"completedAt":1,"durationMs":1}}}}}}'
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

    fn fake_approval_app_server_codex(&self) -> PathBuf {
        let bin = self.root.path().join("approval-bin");
        fs::create_dir_all(&bin).unwrap();
        let codex = bin.join("codex");
        let codex_home = self.root.path().join("approval-codex-home");
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
printf '%s\n' '{{"id":"req-approval","method":"item/commandExecution/requestApproval","params":{{"threadId":"{THREAD_ID}","turnId":"turn-1","itemId":"cmd1","startedAtMs":0,"command":"npm test","cwd":"/tmp","availableDecisions":["accept","decline"]}}}}'

IFS= read -r answer
case "$answer" in
  *'"id":"req-approval"'*'"result"'*'"decision":"accept"'*) ;;
  *) printf '%s\n' "expected approval accept response, got: $answer" >&2; exit 7 ;;
esac
printf '%s\n' '{{"method":"serverRequest/resolved","params":{{"requestId":"req-approval"}}}}'
printf '%s\n' '{{"method":"item/agentMessage/delta","params":{{"threadId":"{THREAD_ID}","turnId":"turn-1","itemId":"item-1","delta":"command approved from list"}}}}'
printf '%s\n' '{{"method":"turn/completed","params":{{"threadId":"{THREAD_ID}","turn":{{"id":"turn-1","status":"completed","startedAt":0,"completedAt":1,"durationMs":1}}}}}}'
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

    fn fake_websocket_slow_app_server_codex(&self) -> PathBuf {
        let bin = self.root.path().join("ws-slow-bin");
        fs::create_dir_all(&bin).unwrap();
        let codex = bin.join("codex");
        let codex_home = self.root.path().join("ws-slow-codex-home");
        fs::create_dir_all(&codex_home).unwrap();
        let script = r#"#!/bin/sh
set -eu
if [ "${1:-}" != "app-server" ] || [ "${2:-}" != "--listen" ]; then
  printf '%s\n' "unexpected args: $*" >&2
  exit 2
fi
python3 - "$3" "__CODEX_HOME__" "__THREAD_ID__" <<'PY'
import base64
import hashlib
import json
import socket
import sys
from urllib.parse import urlparse

url = urlparse(sys.argv[1])
codex_home = sys.argv[2]
thread_id = sys.argv[3]
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
    f"Sec-WebSocket-Accept: {accept}\r\n"
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
        chunk = conn.recv(length - len(payload))
        if not chunk:
            raise SystemExit("frame closed")
        payload.extend(chunk)
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
send_frame({"method": "thread/started", "params": {"thread": {"id": thread_id, "sessionId": thread_id, "preview": "", "status": "running", "cwd": codex_home, "name": None}}})
send_frame({"id": thread["id"], "result": {"thread": {"id": thread_id, "sessionId": thread_id, "preview": "", "status": "running", "cwd": codex_home, "name": None}, "model": "fake-model", "modelProvider": "fake-provider", "serviceTier": None, "cwd": codex_home}})
turn = recv_frame()
send_frame({"id": turn["id"], "result": {"turn": {"id": "turn-ws-1", "status": "running", "startedAt": 0, "completedAt": None, "durationMs": None}}})
send_frame({"method": "turn/started", "params": {"threadId": thread_id, "turn": {"id": "turn-ws-1", "status": "running", "startedAt": 0, "completedAt": None, "durationMs": None}}})
send_frame({"method": "item/agentMessage/delta", "params": {"threadId": thread_id, "turnId": "turn-ws-1", "itemId": "item-1", "delta": "websocket fake app-server running"}})

interrupt = recv_frame()
if not interrupt or interrupt.get("method") != "turn/interrupt":
    raise SystemExit(f"expected turn/interrupt, got {interrupt}")
send_frame({"id": interrupt["id"], "result": {}})
send_frame({"method": "turn/completed", "params": {"threadId": thread_id, "turn": {"id": "turn-ws-1", "status": "interrupted", "startedAt": 0, "completedAt": 1, "durationMs": 1}}})
conn.close()
server.close()
PY
"#
        .replace("__CODEX_HOME__", &codex_home.to_string_lossy())
        .replace("__THREAD_ID__", THREAD_ID);
        fs::write(&codex, script).unwrap();
        let mut permissions = fs::metadata(&codex).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&codex, permissions).unwrap();
        codex
    }

    fn fake_websocket_input_app_server_codex(&self) -> PathBuf {
        self.fake_websocket_input_app_server_codex_with_mcp_startup(false)
    }

    fn fake_websocket_input_app_server_codex_with_pending_mcp(&self) -> PathBuf {
        self.fake_websocket_input_app_server_codex_with_mcp_startup(true)
    }

    fn fake_websocket_input_app_server_codex_with_mcp_startup(&self, pending_mcp: bool) -> PathBuf {
        let bin = self.root.path().join(if pending_mcp {
            "ws-input-mcp-bin"
        } else {
            "ws-input-bin"
        });
        fs::create_dir_all(&bin).unwrap();
        let codex = bin.join("codex");
        let codex_home = self.root.path().join(if pending_mcp {
            "ws-input-mcp-codex-home"
        } else {
            "ws-input-codex-home"
        });
        fs::create_dir_all(&codex_home).unwrap();
        let script = r#"#!/bin/sh
set -eu
if [ "${1:-}" != "app-server" ] || [ "${2:-}" != "--listen" ]; then
  printf '%s\n' "unexpected args: $*" >&2
  exit 2
fi
python3 - "$3" "__CODEX_HOME__" "__THREAD_ID__" <<'PY'
import base64
import hashlib
import json
import socket
import sys
from urllib.parse import urlparse

url = urlparse(sys.argv[1])
codex_home = sys.argv[2]
thread_id = sys.argv[3]
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
    f"Sec-WebSocket-Accept: {accept}\r\n"
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
        chunk = conn.recv(length - len(payload))
        if not chunk:
            raise SystemExit("frame closed")
        payload.extend(chunk)
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
send_frame({"method": "thread/started", "params": {"thread": {"id": thread_id, "sessionId": thread_id, "preview": "", "status": "running", "cwd": codex_home, "name": None}}})
send_frame({"id": thread["id"], "result": {"thread": {"id": thread_id, "sessionId": thread_id, "preview": "", "status": "running", "cwd": codex_home, "name": None}, "model": "fake-model", "modelProvider": "fake-provider", "serviceTier": None, "cwd": codex_home}})
turn = recv_frame()
send_frame({"id": turn["id"], "result": {"turn": {"id": "turn-ws-1", "status": "running", "startedAt": 0, "completedAt": None, "durationMs": None}}})
send_frame({"method": "turn/started", "params": {"threadId": thread_id, "turn": {"id": "turn-ws-1", "status": "running", "startedAt": 0, "completedAt": None, "durationMs": None}}})
__MCP_STARTUP__
send_frame({"id": "req-ws-1", "method": "item/tool/requestUserInput", "params": {"threadId": thread_id, "turnId": "turn-ws-1", "itemId": "call1", "questions": [{"id": "confirm_path", "header": "Confirm", "question": "Continue?", "isOther": False, "isSecret": False, "options": None}]}})

answer = recv_frame()
if not answer or answer.get("id") != "req-ws-1":
    raise SystemExit(f"expected request-user-input response id, got {answer}")
answers = answer.get("result", {}).get("answers", {})
if answers.get("confirm_path", {}).get("answers") != ["yes"]:
    raise SystemExit(f"expected answer yes, got {answer}")
send_frame({"method": "serverRequest/resolved", "params": {"requestId": "req-ws-1"}})
send_frame({"method": "item/agentMessage/delta", "params": {"threadId": thread_id, "turnId": "turn-ws-1", "itemId": "item-1", "delta": "websocket completed after answer yes"}})
send_frame({"method": "turn/completed", "params": {"threadId": thread_id, "turn": {"id": "turn-ws-1", "status": "completed", "startedAt": 0, "completedAt": 1, "durationMs": 1}}})
conn.close()
server.close()
PY
"#
        .replace("__CODEX_HOME__", &codex_home.to_string_lossy())
        .replace("__THREAD_ID__", THREAD_ID)
        .replace(
            "__MCP_STARTUP__",
            if pending_mcp {
                r#"send_frame({"method": "mcpServer/startupStatus/updated", "params": {"name": "codex_apps", "status": "starting"}})"#
            } else {
                ""
            },
        );
        fs::write(&codex, script).unwrap();
        let mut permissions = fs::metadata(&codex).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&codex, permissions).unwrap();
        codex
    }

    fn fake_hosted_helper(&self) -> (PathBuf, PathBuf) {
        let bin = self.root.path().join("hosted-bin");
        fs::create_dir_all(&bin).unwrap();
        let helper = bin.join("agentview-codex-hosted");
        let log = self.root.path().join("hosted-helper.args");
        fs::write(
            &helper,
            format!(
                r#"#!/bin/sh
set -eu
printf '%s\n' "$*" > "{}"
exit 0
"#,
                log.display()
            ),
        )
        .unwrap();
        let mut permissions = fs::metadata(&helper).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&helper, permissions).unwrap();
        (helper, log)
    }

    fn fake_persistent_hosted_helper(&self) -> (PathBuf, PathBuf) {
        let bin = self.root.path().join("persistent-hosted-bin");
        fs::create_dir_all(&bin).unwrap();
        let helper = bin.join("agentview-codex-hosted");
        let log = self.root.path().join("persistent-hosted-helper.args");
        fs::write(
            &helper,
            format!(
                r#"#!/bin/sh
set -eu
printf '%s\n' "$*" > "{}"
printf 'persistent hosted helper ready\r\n'
sleep 1
printf '\033]777;agentview-detach\a'
sleep 1
exit 0
"#,
                log.display()
            ),
        )
        .unwrap();
        let mut permissions = fs::metadata(&helper).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&helper, permissions).unwrap();
        (helper, log)
    }

    fn fake_long_running_hosted_helper(&self) -> (PathBuf, PathBuf) {
        let bin = self.root.path().join("long-hosted-bin");
        fs::create_dir_all(&bin).unwrap();
        let helper = bin.join("agentview-codex-hosted");
        let log = self.root.path().join("long-hosted-helper.args");
        fs::write(
            &helper,
            format!(
                r#"#!/bin/sh
set -eu
printf '%s\n' "$*" > "{}"
printf 'persistent hosted helper ready\r\n'
sleep 30
"#,
                log.display()
            ),
        )
        .unwrap();
        let mut permissions = fs::metadata(&helper).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&helper, permissions).unwrap();
        (helper, log)
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

fn run_app_server_job(
    env: &TestEnv,
    store: &TempDir,
    codex: &Path,
    repo: &Path,
    prompt: &str,
) -> String {
    let output = env
        .agentview(store, codex)
        .args(["run", "--cwd", repo.to_str().unwrap(), prompt])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .find_map(|line| line.strip_prefix("backgrounded"))
        .and_then(|line| line.split_whitespace().next())
        .expect("job id in run output")
        .to_string()
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

fn read_pid(path: &Path) -> u32 {
    fs::read_to_string(path).unwrap().trim().parse().unwrap()
}

fn process_alive(pid: u32) -> bool {
    let output = Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "stat="])
        .output()
        .unwrap();
    if !output.status.success() {
        return false;
    }
    let state = String::from_utf8_lossy(&output.stdout);
    let state = state.trim();
    !state.is_empty() && !state.starts_with('Z')
}
