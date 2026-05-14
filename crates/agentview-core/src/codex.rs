use crate::schema::{BlockingRequest, Job, JobBackend, JobStatus, ProcessState};
use crate::store::{
    append_job_event, get_job, read_job_events, require_job, update_job, write_job_last,
};
use crate::supervisor::supervisor_app_server_endpoint;
use crate::util::{
    command_exists, event_failed, event_needs_input, extract_pr_refs, extract_thread_id, home_dir,
    merge_pr_refs, now_iso, path_exists, strip_ansi, summarize_event, truncate,
};
use agentview_codex_hosted::{HostedHelper, HostedSessionConfig, HostedSessionExit};
use agentview_codex_runtime::{
    CodexRuntime, Notification, RuntimeEvent, RuntimeTurnOptions, ServerRequest,
};
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::fs;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;

pub fn assert_codex_available() -> Result<()> {
    if command_exists("codex") {
        Ok(())
    } else {
        bail!("Codex CLI not found. Install or expose `codex` on PATH.")
    }
}

pub fn build_codex_exec_args(job: &Job, prompt: &str, resume: bool) -> Vec<String> {
    if resume {
        let mut args = vec![
            "exec".to_string(),
            "resume".to_string(),
            "--json".to_string(),
        ];
        if let Some(model) = &job.model {
            args.extend(["--model".to_string(), model.clone()]);
        }
        if job.worktree_path.is_none() {
            args.push("--skip-git-repo-check".to_string());
        }
        if let Some(thread_id) = &job.codex_thread_id {
            args.push(thread_id.clone());
        }
        args.push(prompt.to_string());
        return args;
    }

    let mut args = vec![
        "exec".to_string(),
        "--json".to_string(),
        "--cd".to_string(),
        job.cwd.clone(),
        "--sandbox".to_string(),
        job.sandbox.clone(),
    ];
    if let Some(model) = &job.model {
        args.extend(["--model".to_string(), model.clone()]);
    }
    if let Some(profile) = &job.profile {
        args.extend(["--profile".to_string(), profile.clone()]);
    }
    if job.worktree_path.is_none() {
        args.push("--skip-git-repo-check".to_string());
    }
    args.push(prompt.to_string());
    args
}

pub fn build_codex_resume_args(job: &Job) -> Vec<String> {
    let mut args = vec![
        "resume".to_string(),
        "--include-non-interactive".to_string(),
        "--cd".to_string(),
        job.cwd.clone(),
        "--sandbox".to_string(),
        job.sandbox.clone(),
    ];
    if let Some(model) = &job.model {
        args.extend(["--model".to_string(), model.clone()]);
    }
    if let Some(profile) = &job.profile {
        args.extend(["--profile".to_string(), profile.clone()]);
    }
    if let Some(thread_id) = &job.codex_thread_id {
        args.push(thread_id.clone());
    }
    args
}

pub fn run_codex_turn(job_id: &str, prompt: &str, resume: bool) -> Result<()> {
    assert_codex_available()?;
    let job = require_job(job_id)?;
    let args = build_codex_exec_args(&job, prompt, resume);

    update_job(job_id, |job| {
        job.status = JobStatus::Working;
        job.process_state = ProcessState::Alive;
        job.pid = Some(std::process::id());
        job.active_worker_pid = Some(std::process::id());
        job.codex_turn_id = None;
        job.completed_at = None;
        job.last_summary = Some(if resume {
            "Resuming Codex thread".to_string()
        } else {
            "Starting Codex session".to_string()
        });
        job.blocking_request = None;
        job.error = None;
        Ok(())
    })?;

    let mut child = Command::new("codex")
        .args(&args)
        .current_dir(Path::new(&job.cwd))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("failed to spawn codex {}", args.join(" ")))?;

    let codex_pid = child.id();
    update_job(job_id, |job| {
        job.pid = Some(codex_pid);
        job.active_worker_pid = Some(std::process::id());
        job.process_state = ProcessState::Alive;
        Ok(())
    })?;

    let stderr_text = Arc::new(Mutex::new(String::new()));
    let stderr_handle = child.stderr.take().map(|stderr| {
        let job_id = job_id.to_string();
        let stderr_text = Arc::clone(&stderr_text);
        thread::spawn(move || {
            let mut reader = BufReader::new(stderr);
            let mut buf = String::new();
            loop {
                buf.clear();
                let bytes = reader.read_line(&mut buf).unwrap_or(0);
                if bytes == 0 {
                    break;
                }
                let text = strip_ansi(&buf);
                if let Ok(mut locked) = stderr_text.lock() {
                    locked.push_str(&text);
                }
                let _ = append_job_event(
                    &job_id,
                    &json!({
                        "type": "stderr",
                        "text": text,
                        "timestamp": now_iso()
                    }),
                );
                let _ = update_job(&job_id, |job| {
                    job.last_output = Some(truncate(&text, 200));
                    job.last_summary = Some(truncate(&text, 120));
                    Ok(())
                });
            }
        })
    });

    if let Some(stdout) = child.stdout.take() {
        let mut reader = BufReader::new(stdout);
        let mut line = String::new();
        loop {
            line.clear();
            let bytes = reader.read_line(&mut line)?;
            if bytes == 0 {
                break;
            }
            handle_codex_line(job_id, &line)?;
        }
    }

    let status = child.wait()?;
    if let Some(handle) = stderr_handle {
        let _ = handle.join();
    }
    let exit_code = status.code().unwrap_or(1);
    let final_thread_id = discover_codex_thread_id(job_id)?;
    let latest = get_job(job_id)?.context("job disappeared while Codex turn was running")?;
    let stopped = latest.status == JobStatus::Stopped;
    let failed = exit_code != 0 && !stopped;
    let final_stderr = stderr_text
        .lock()
        .map(|text| text.clone())
        .unwrap_or_default();

    update_job(job_id, |job| {
        job.status = if stopped {
            JobStatus::Stopped
        } else if failed {
            JobStatus::Failed
        } else {
            JobStatus::Completed
        };
        job.process_state = ProcessState::Exited;
        job.pid = None;
        job.active_worker_pid = None;
        job.codex_turn_id = None;
        job.exit_code = Some(exit_code);
        if job.codex_thread_id.is_none() {
            job.codex_thread_id = final_thread_id;
        }
        job.completed_at = Some(now_iso());
        job.last_summary = Some(if stopped {
            "stopped".to_string()
        } else if failed {
            format!("failed: codex exited {exit_code}")
        } else {
            job.last_summary
                .clone()
                .unwrap_or_else(|| "completed".to_string())
        });
        if failed {
            job.last_output = Some(truncate(strip_ansi(&final_stderr), 240));
        }
        job.blocking_request = None;
        Ok(())
    })?;

    Ok(())
}

pub fn run_codex_app_server_turn(job_id: &str, prompt: &str) -> Result<()> {
    run_codex_app_server_turn_inner(job_id, prompt, None)
}

pub fn run_codex_app_server_reply(job_id: &str, prompt: &str) -> Result<()> {
    let job = require_job(job_id)?;
    let thread_id = job
        .codex_thread_id
        .clone()
        .with_context(|| format!("Job {job_id} has no Codex thread id yet"))?;
    run_codex_app_server_turn_inner(job_id, prompt, Some(thread_id))
}

fn run_codex_app_server_turn_inner(
    job_id: &str,
    prompt: &str,
    resume_thread_id: Option<String>,
) -> Result<()> {
    assert_codex_available()?;
    let job = require_job(job_id)?;
    update_job(job_id, |job| {
        job.status = JobStatus::Working;
        job.process_state = ProcessState::Alive;
        job.pid = Some(std::process::id());
        job.active_worker_pid = Some(std::process::id());
        job.codex_turn_id = None;
        job.completed_at = None;
        job.last_summary = Some(if resume_thread_id.is_some() {
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

    let mut latest_text = String::new();
    let runtime = CodexRuntime::default();
    if let Some(thread_id) = resume_thread_id {
        runtime.run_text_turn_on_thread(&thread_id, options, prompt, |event| {
            handle_app_server_runtime_event(job_id, event, &mut latest_text)
        })
    } else {
        runtime.run_text_turn(options, prompt, |event| {
            handle_app_server_runtime_event(job_id, event, &mut latest_text)
        })
    }
}

pub(crate) fn handle_app_server_runtime_event(
    job_id: &str,
    event: RuntimeEvent,
    latest_text: &mut String,
) -> Result<()> {
    match event {
        RuntimeEvent::Initialized {
            user_agent,
            codex_home,
        } => append_job_event(
            job_id,
            &json!({
                "type": "app_server_initialized",
                "userAgent": user_agent,
                "codexHome": codex_home,
                "timestamp": now_iso()
            }),
        ),
        RuntimeEvent::ThreadStarted { thread_id } => {
            update_job(job_id, |job| {
                job.codex_thread_id = Some(thread_id.clone());
                job.last_summary = Some("Codex thread started".to_string());
                Ok(())
            })?;
            append_job_event(
                job_id,
                &json!({
                    "type": "app_server_thread_started",
                    "threadId": thread_id,
                    "timestamp": now_iso()
                }),
            )
        }
        RuntimeEvent::ThreadResumed { thread_id } => {
            update_job(job_id, |job| {
                job.codex_thread_id = Some(thread_id.clone());
                job.last_summary = Some("Codex thread resumed".to_string());
                Ok(())
            })?;
            append_job_event(
                job_id,
                &json!({
                    "type": "app_server_thread_resumed",
                    "threadId": thread_id,
                    "timestamp": now_iso()
                }),
            )
        }
        RuntimeEvent::TurnStarted { thread_id, turn_id } => {
            append_job_event(
                job_id,
                &json!({
                    "type": "app_server_turn_started",
                    "threadId": thread_id,
                    "turnId": turn_id.clone(),
                    "timestamp": now_iso()
                }),
            )?;
            update_job(job_id, |job| {
                job.status = JobStatus::Working;
                job.codex_turn_id = Some(turn_id);
                job.last_summary = Some("Codex turn started".to_string());
                Ok(())
            })?;
            Ok(())
        }
        RuntimeEvent::Notification(notification) => {
            handle_app_server_notification(job_id, notification, latest_text).map(|_| ())
        }
        RuntimeEvent::ServerRequest(request) => handle_app_server_request(job_id, request),
    }
}

fn handle_app_server_notification(
    job_id: &str,
    notification: Notification,
    latest_text: &mut String,
) -> Result<bool> {
    let method = notification.method.clone();
    let params = notification.params.clone();
    append_job_event(
        job_id,
        &json!({
            "type": "app_server_notification",
            "method": method.clone(),
            "params": params.clone(),
            "timestamp": now_iso()
        }),
    )?;

    match method.as_str() {
        "thread/started" => {
            if let Some(thread_id) = params
                .get("thread")
                .and_then(|thread| thread.get("id"))
                .and_then(Value::as_str)
            {
                update_job(job_id, |job| {
                    job.codex_thread_id = Some(thread_id.to_string());
                    Ok(())
                })?;
            }
        }
        "turn/started" => {
            update_job(job_id, |job| {
                job.status = JobStatus::Working;
                job.process_state = ProcessState::Alive;
                job.last_summary = Some("working".to_string());
                job.blocking_request = None;
                Ok(())
            })?;
        }
        "item/agentMessage/delta" | "process/outputDelta" | "item/commandExecution/outputDelta" => {
            if let Some(delta) = params
                .get("delta")
                .or_else(|| params.get("text"))
                .and_then(Value::as_str)
            {
                latest_text.push_str(delta);
                let summary = truncate(latest_text.trim(), 200);
                update_job(job_id, |job| {
                    job.last_output = Some(summary.clone());
                    job.last_summary = Some(truncate(&summary, 120));
                    Ok(())
                })?;
                write_job_last(job_id, latest_text.trim())?;
            }
        }
        "turn/completed" => {
            let status = params
                .get("turn")
                .and_then(|turn| turn.get("status"))
                .and_then(app_server_status_label)
                .unwrap_or("completed");
            let interrupted = matches!(status, "interrupted");
            let failed = matches!(status, "failed" | "error" | "cancelled" | "canceled");
            update_job(job_id, |job| {
                job.status = if interrupted {
                    JobStatus::Stopped
                } else if failed {
                    JobStatus::Failed
                } else {
                    JobStatus::Completed
                };
                job.process_state = ProcessState::Exited;
                job.pid = None;
                job.active_worker_pid = None;
                job.codex_turn_id = None;
                job.exit_code = Some(if failed || interrupted { 1 } else { 0 });
                job.completed_at = Some(now_iso());
                job.blocking_request = None;
                job.last_summary = Some(if latest_text.trim().is_empty() {
                    if interrupted {
                        "stopped".to_string()
                    } else if failed {
                        format!("failed: {status}")
                    } else {
                        "completed".to_string()
                    }
                } else {
                    truncate(latest_text.trim(), 120)
                });
                Ok(())
            })?;
            return Ok(true);
        }
        "serverRequest/resolved" => {
            update_job(job_id, |job| {
                job.status = JobStatus::Working;
                job.blocking_request = None;
                Ok(())
            })?;
        }
        _ => {}
    }

    Ok(false)
}

fn handle_app_server_request(job_id: &str, request: ServerRequest) -> Result<()> {
    let id = request.id.clone();
    let method = request.method.clone();
    let params = request.params.clone();
    let message = app_server_request_message(&method, &params);
    let summary = format!("needs input: {message}");
    append_job_event(
        job_id,
        &json!({
            "type": "app_server_request",
            "id": id.clone(),
            "method": method.clone(),
            "params": params.clone(),
            "timestamp": now_iso()
        }),
    )?;
    update_job(job_id, |job| {
        job.status = JobStatus::NeedsInput;
        job.last_summary = Some(summary.clone());
        job.blocking_request = Some(BlockingRequest {
            kind: method.clone(),
            message: message.clone(),
            event: Some(json!({
                "id": id,
                "method": method,
                "params": params
            })),
            created_at: now_iso(),
        });
        Ok(())
    })?;
    Ok(())
}

fn app_server_request_message(method: &str, params: &Value) -> String {
    match method {
        "item/tool/requestUserInput" => params
            .get("questions")
            .and_then(Value::as_array)
            .and_then(|questions| questions.first())
            .and_then(|question| {
                question
                    .get("question")
                    .or_else(|| question.get("header"))
                    .and_then(Value::as_str)
            })
            .unwrap_or("Codex is waiting for input")
            .to_string(),
        "item/commandExecution/requestApproval" => params
            .get("command")
            .and_then(Value::as_str)
            .map(|command| format!("approve command: {command}"))
            .or_else(|| {
                params
                    .get("reason")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned)
            })
            .unwrap_or_else(|| "approve command execution?".to_string()),
        "item/fileChange/requestApproval" | "applyPatchApproval" => params
            .get("reason")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| "approve file changes?".to_string()),
        "item/permissions/requestApproval" => params
            .get("reason")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| "approve requested permissions?".to_string()),
        "execCommandApproval" => params
            .get("command")
            .and_then(Value::as_array)
            .map(|parts| {
                parts
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .filter(|command| !command.is_empty())
            .map(|command| format!("approve command: {command}"))
            .unwrap_or_else(|| "approve command execution?".to_string()),
        _ => method.to_string(),
    }
}

fn app_server_status_label(status: &Value) -> Option<&str> {
    status
        .as_str()
        .or_else(|| status.get("type").and_then(Value::as_str))
}

pub fn handle_codex_line(job_id: &str, raw_line: &str) -> Result<()> {
    let line = raw_line.trim();
    if line.is_empty() {
        return Ok(());
    }

    let mut event: Value = match serde_json::from_str(line) {
        Ok(value) => value,
        Err(_) => json!({ "type": "text", "text": strip_ansi(line) }),
    };
    ensure_event_timestamp(&mut event);
    append_job_event(job_id, &event)?;

    let summary = summarize_event(&event);
    let thread_id = extract_thread_id(&event);
    let refs = extract_pr_refs(&event.to_string());
    let needs_input = event_needs_input(&event);
    let failed = event_failed(&event);

    update_job(job_id, |job| {
        if job.codex_thread_id.is_none() {
            job.codex_thread_id = thread_id.clone();
        }
        job.status = if needs_input {
            JobStatus::NeedsInput
        } else if failed {
            JobStatus::Failed
        } else {
            JobStatus::Working
        };
        if needs_input {
            job.blocking_request = Some(BlockingRequest {
                kind: "codex_request".to_string(),
                message: summary
                    .clone()
                    .unwrap_or_else(|| "Codex is waiting for input".to_string()),
                event: Some(event.clone()),
                created_at: now_iso(),
            });
        }
        if let Some(summary) = &summary {
            job.last_summary = Some(summary.clone());
            job.last_output = Some(summary.clone());
        }
        job.pr_refs = merge_pr_refs(&job.pr_refs, &refs);
        Ok(())
    })?;

    if let Some(summary) = summary {
        write_job_last(job_id, summary)?;
    }
    Ok(())
}

pub fn discover_codex_thread_id(job_id: &str) -> Result<Option<String>> {
    let Some(job) = get_job(job_id)? else {
        return Ok(None);
    };
    if job.codex_thread_id.is_some() {
        return Ok(job.codex_thread_id);
    }

    for event in read_job_events(job_id)?.iter().rev() {
        if let Some(thread_id) = extract_thread_id(event) {
            return Ok(Some(thread_id));
        }
    }

    find_recent_codex_session_id(&job)
}

pub fn find_recent_codex_session_id(job: &Job) -> Result<Option<String>> {
    let index_path = home_dir().join(".codex").join("session_index.jsonl");
    if !path_exists(&index_path) {
        return Ok(None);
    }
    let content = fs::read_to_string(index_path)?;
    let lines: Vec<_> = content
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect();
    for line in lines.iter().rev().take(300) {
        let Ok(entry) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let text = entry.to_string();
        let matches_cwd = text.contains(&job.cwd) || text.contains(&job.repo_root);
        if matches_cwd {
            if let Some(thread_id) = extract_thread_id(&entry) {
                return Ok(Some(thread_id));
            }
        }
    }
    Ok(None)
}

pub fn attach_codex(job: &Job) -> Result<i32> {
    if job.backend == JobBackend::AppServer {
        return attach_hosted_codex(job, false);
    }
    assert_codex_available()?;
    if job.process_state == ProcessState::Alive
        || matches!(job.status, JobStatus::Working | JobStatus::NeedsInput)
    {
        bail!(
            "Live attach to a running fallback Codex exec session requires the app-server backend. Wait for this turn to finish, or stop it and resume after completion."
        );
    }
    if job.codex_thread_id.is_none() {
        bail!(
            "Job {} does not have a Codex thread id yet. Try again after the first Codex event arrives.",
            job.id
        );
    }
    let status = Command::new("codex")
        .args(build_codex_resume_args(job))
        .current_dir(PathBuf::from(&job.cwd))
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .context("failed to attach Codex session")?;
    Ok(status.code().unwrap_or(0))
}

pub fn attach_hosted_codex(job: &Job, no_alt_screen: bool) -> Result<i32> {
    if job.backend != JobBackend::AppServer {
        bail!("Hosted attach requires an app-server-backed Codex job");
    }
    let thread_id = job.codex_thread_id.clone().with_context(|| {
        format!(
            "Job {} does not have a Codex thread id yet. Try again after the first Codex event arrives.",
            job.id
        )
    })?;
    append_job_event(
        &job.id,
        &json!({
            "type": "hosted_attach_started",
            "threadId": thread_id.clone(),
            "timestamp": now_iso()
        }),
    )?;
    let remote_url = supervisor_app_server_endpoint(&job.id)?;
    if matches!(job.process_state, ProcessState::Alive)
        && remote_url.as_deref().is_none_or(|url| url == "stdio://")
    {
        bail!(
            "Hosted attach to a running app-server job requires a connectable supervisor app-server endpoint."
        );
    }
    if let Some(remote_url) = remote_url.as_deref().filter(|url| *url != "stdio://") {
        append_job_event(
            &job.id,
            &json!({
                "type": "hosted_attach_endpoint_resolved",
                "threadId": thread_id.clone(),
                "appServerUrl": remote_url,
                "timestamp": now_iso()
            }),
        )?;
    }
    let config = HostedSessionConfig {
        thread_id: thread_id.clone(),
        cwd: PathBuf::from(&job.cwd),
        remote_url: remote_url.filter(|url| url != "stdio://"),
        remote_auth_token: None,
        no_alt_screen,
    };
    match HostedHelper::from_env_or_default().run(&config)? {
        HostedSessionExit::Detached => {
            append_job_event(
                &job.id,
                &json!({
                    "type": "hosted_attach_detached",
                    "threadId": thread_id,
                    "timestamp": now_iso()
                }),
            )?;
            Ok(0)
        }
        HostedSessionExit::Quit(code) => {
            append_job_event(
                &job.id,
                &json!({
                    "type": "hosted_attach_quit",
                    "threadId": thread_id,
                    "exitCode": code,
                    "timestamp": now_iso()
                }),
            )?;
            Ok(code)
        }
    }
}

pub fn cwd_for_display(cwd: &str) -> String {
    let home = home_dir();
    let path = Path::new(cwd);
    if let Ok(stripped) = path.strip_prefix(&home) {
        let suffix = stripped.to_string_lossy();
        return if suffix.is_empty() {
            "~".to_string()
        } else {
            format!("~/{}", suffix)
        };
    }
    cwd.to_string()
}

fn ensure_event_timestamp(event: &mut Value) {
    match event {
        Value::Object(map) => {
            map.entry("timestamp".to_string())
                .or_insert_with(|| json!(now_iso()));
        }
        _ => {
            *event = json!({
                "type": "value",
                "value": event.take(),
                "timestamp": now_iso()
            });
        }
    }
}

#[allow(dead_code)]
fn read_all(mut reader: impl Read) -> String {
    let mut text = String::new();
    let _ = reader.read_to_string(&mut text);
    text
}
