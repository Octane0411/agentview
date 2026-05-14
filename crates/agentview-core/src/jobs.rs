use crate::schema::{Job, JobBackend, JobStatus, ProcessState};
use crate::store::{
    append_job_event, put_job, remove_job_files, require_job, update_job, with_store,
};
use crate::supervisor::{
    supervisor_resolve_server_request, supervisor_start_app_server_turn,
    supervisor_stop_app_server_turn,
};
use crate::util::{command_exists, extract_pr_refs, make_job_id, now_iso, title_from_prompt};
use crate::worktree::{create_worktree, remove_worktree, worktree_has_changes};
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

#[derive(Debug, Clone, Default)]
pub struct DispatchOptions {
    pub cwd: Option<PathBuf>,
    pub title: Option<String>,
    pub model: Option<String>,
    pub profile: Option<String>,
    pub approval_policy: Option<String>,
    pub sandbox: Option<String>,
    pub backend: DispatchBackend,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum DispatchBackend {
    FallbackExec,
    #[default]
    AppServer,
}

#[derive(Debug, Clone, Default)]
pub struct RemoveOptions {
    pub force: bool,
    pub purge: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedDispatchPrompt {
    pub prompt: String,
    pub cwd: PathBuf,
    pub model: Option<String>,
    pub profile: Option<String>,
}

#[derive(Debug, Clone)]
pub struct DoctorReport {
    pub codex: bool,
    pub rustc: Option<String>,
}

pub fn dispatch_job(prompt: &str, options: DispatchOptions) -> Result<Job> {
    let cwd = absolute_path(options.cwd.unwrap_or(std::env::current_dir()?))?;
    let parsed = parse_dispatch_prompt(prompt, &cwd);
    if parsed.prompt.trim().is_empty() {
        bail!("Prompt is empty");
    }

    let title = options
        .title
        .clone()
        .unwrap_or_else(|| title_from_prompt(&parsed.prompt));
    let job_id = make_job_id();
    let worktree = create_worktree(&parsed.cwd, &job_id, &title)?;
    let now = now_iso();
    let backend = match options.backend {
        DispatchBackend::FallbackExec => JobBackend::FallbackExec,
        DispatchBackend::AppServer => JobBackend::AppServer,
    };
    let job = Job {
        id: job_id.clone(),
        provider: "codex".to_string(),
        backend,
        codex_thread_id: None,
        codex_turn_id: None,
        title,
        initial_prompt: parsed.prompt.clone(),
        repo_root: worktree.repo_root.to_string_lossy().to_string(),
        cwd: worktree.cwd.to_string_lossy().to_string(),
        dispatch_cwd: parsed.cwd.to_string_lossy().to_string(),
        worktree_path: worktree
            .worktree_path
            .as_ref()
            .map(|path| path.to_string_lossy().to_string()),
        worktree_branch: worktree.branch.clone(),
        model: options.model.or(parsed.model),
        profile: options.profile.or(parsed.profile),
        approval_policy: options
            .approval_policy
            .unwrap_or_else(|| "never".to_string()),
        sandbox: options
            .sandbox
            .unwrap_or_else(|| "workspace-write".to_string()),
        status: JobStatus::Working,
        process_state: ProcessState::Alive,
        pid: None,
        active_worker_pid: None,
        pinned: false,
        manual_order: None,
        archived: false,
        deleted: false,
        last_summary: Some(
            worktree
                .warning
                .clone()
                .unwrap_or_else(|| "queued".to_string()),
        ),
        last_output: None,
        blocking_request: None,
        pr_refs: extract_pr_refs(&parsed.prompt),
        created_at: now.clone(),
        updated_at: now.clone(),
        completed_at: None,
        exit_code: None,
        error: None,
    };

    put_job(job.clone())?;
    append_job_event(
        &job_id,
        &json!({
            "type": "agentview_job_created",
            "prompt": parsed.prompt,
            "cwd": job.cwd,
            "worktreePath": job.worktree_path,
            "timestamp": now
        }),
    )?;

    let pid = match backend {
        JobBackend::FallbackExec => spawn_worker(&job_id, "run", None, &job.cwd)?.id(),
        JobBackend::AppServer => supervisor_start_app_server_turn(&job_id, &parsed.prompt, false)?,
    };

    update_job(&job_id, |job| {
        if job.process_state == ProcessState::Alive {
            job.pid = Some(pid);
            job.active_worker_pid = Some(pid);
            job.last_summary = Some(
                worktree
                    .warning
                    .clone()
                    .unwrap_or_else(|| "starting Codex".to_string()),
            );
        }
        Ok(())
    })
}

pub fn reply_to_job(job_id: &str, prompt: &str) -> Result<Option<u32>> {
    let job = require_job(job_id)?;
    if job.process_state == ProcessState::Alive {
        match job.backend {
            JobBackend::AppServer => return reply_to_running_app_server_job(&job, prompt),
            JobBackend::FallbackExec => bail!(
                "Live replies to a running Codex exec session require the app-server backend. Wait for this turn to finish, or stop it and resume."
            ),
        }
    }
    if job.codex_thread_id.is_none() {
        bail!("Job {job_id} has no Codex thread id yet");
    }
    let pid = match job.backend {
        JobBackend::FallbackExec => spawn_worker(job_id, "reply", Some(prompt), &job.cwd)?.id(),
        JobBackend::AppServer => supervisor_start_app_server_turn(job_id, prompt, true)?,
    };
    update_job(job_id, |job| {
        job.status = JobStatus::Working;
        job.process_state = ProcessState::Alive;
        job.pid = Some(pid);
        job.active_worker_pid = Some(pid);
        job.completed_at = None;
        job.last_summary = Some("reply sent".to_string());
        job.blocking_request = None;
        Ok(())
    })?;
    Ok(Some(pid))
}

fn reply_to_running_app_server_job(job: &Job, prompt: &str) -> Result<Option<u32>> {
    let blocking_request = job.blocking_request.as_ref().with_context(|| {
        format!(
            "Job {} is still running and has no pending request. Enter the session to continue the live conversation.",
            job.id
        )
    })?;
    let event = blocking_request.event.as_ref().with_context(|| {
        format!(
            "Job {} has a pending request without structured event metadata. Enter the session to answer it.",
            job.id
        )
    })?;
    let request_id = event
        .get("id")
        .cloned()
        .with_context(|| format!("Job {} pending request has no id", job.id))?;
    let method = event
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or(&blocking_request.kind);
    let params = event.get("params").cloned().unwrap_or(Value::Null);
    let result = server_request_response(method, &params, prompt)?;
    supervisor_resolve_server_request(&job.id, request_id.clone(), result.clone())?;
    append_job_event(
        &job.id,
        &json!({
            "type": "agentview_server_request_reply_sent",
            "requestId": request_id,
            "method": method,
            "result": result,
            "timestamp": now_iso()
        }),
    )?;
    update_job(&job.id, |job| {
        job.status = JobStatus::Working;
        job.last_summary = Some("reply sent".to_string());
        job.blocking_request = None;
        Ok(())
    })?;
    Ok(None)
}

fn server_request_response(method: &str, params: &Value, prompt: &str) -> Result<Value> {
    match method {
        "item/tool/requestUserInput" => user_input_response(params, prompt),
        "item/commandExecution/requestApproval" => {
            Ok(json!({ "decision": v2_approval_decision(prompt)? }))
        }
        "item/fileChange/requestApproval" => {
            Ok(json!({ "decision": v2_approval_decision(prompt)? }))
        }
        "item/permissions/requestApproval" => permissions_response(params, prompt),
        "applyPatchApproval" | "execCommandApproval" => {
            Ok(json!({ "decision": v1_review_decision(prompt)? }))
        }
        _ => bail!(
            "Pending Codex request `{method}` is not supported from the AgentView list yet. Enter the session to answer it."
        ),
    }
}

fn user_input_response(params: &Value, prompt: &str) -> Result<Value> {
    let answer = prompt.trim();
    if answer.is_empty() {
        bail!("Reply is empty");
    }
    let questions = params
        .get("questions")
        .and_then(Value::as_array)
        .context("request-user-input params are missing questions")?;
    if questions.is_empty() {
        bail!("request-user-input has no questions");
    }
    let mut answers = serde_json::Map::new();
    for question in questions {
        let id = question
            .get("id")
            .and_then(Value::as_str)
            .context("request-user-input question is missing id")?;
        answers.insert(id.to_string(), json!({ "answers": [answer] }));
    }
    Ok(json!({ "answers": answers }))
}

fn permissions_response(params: &Value, prompt: &str) -> Result<Value> {
    if is_approve_text(prompt) {
        let permissions = params
            .get("permissions")
            .cloned()
            .unwrap_or_else(|| json!({}));
        return Ok(json!({
            "permissions": permissions,
            "scope": "turn",
        }));
    }
    if is_decline_text(prompt) {
        return Ok(json!({
            "permissions": {},
            "scope": "turn",
        }));
    }
    bail!(
        "Permission requests require `agentview approve <job_id>` or `agentview decline <job_id>`"
    )
}

fn v2_approval_decision(prompt: &str) -> Result<&'static str> {
    if is_approve_text(prompt) {
        return Ok("accept");
    }
    if is_decline_text(prompt) {
        return Ok("decline");
    }
    bail!("Approval requests require `agentview approve <job_id>` or `agentview decline <job_id>`")
}

fn v1_review_decision(prompt: &str) -> Result<&'static str> {
    if is_approve_text(prompt) {
        return Ok("approved");
    }
    if is_decline_text(prompt) {
        return Ok("denied");
    }
    bail!("Approval requests require `agentview approve <job_id>` or `agentview decline <job_id>`")
}

fn is_approve_text(prompt: &str) -> bool {
    matches!(
        normalize_decision_text(prompt).as_str(),
        "approve" | "approved" | "accept" | "accepted" | "yes" | "y"
    )
}

fn is_decline_text(prompt: &str) -> bool {
    matches!(
        normalize_decision_text(prompt).as_str(),
        "decline" | "declined" | "deny" | "denied" | "reject" | "rejected" | "no" | "n"
    )
}

fn normalize_decision_text(prompt: &str) -> String {
    prompt.trim().to_ascii_lowercase()
}

pub fn respawn_job(job_id: &str, prompt: &str) -> Result<Option<u32>> {
    let job = require_job(job_id)?;
    if job.process_state == ProcessState::Alive {
        bail!("Job {job_id} is already running");
    }
    if job.codex_thread_id.is_none() {
        bail!("Job {job_id} has no Codex thread id yet");
    }
    let pid = match job.backend {
        JobBackend::FallbackExec => spawn_worker(job_id, "resume", Some(prompt), &job.cwd)?.id(),
        JobBackend::AppServer => supervisor_start_app_server_turn(job_id, prompt, true)?,
    };
    update_job(job_id, |job| {
        job.status = JobStatus::Working;
        job.process_state = ProcessState::Alive;
        job.pid = Some(pid);
        job.active_worker_pid = Some(pid);
        job.completed_at = None;
        job.last_summary = Some("respawned".to_string());
        job.blocking_request = None;
        Ok(())
    })?;
    Ok(Some(pid))
}

pub fn stop_job(job_id: &str) -> Result<()> {
    let job = require_job(job_id)?;
    if job.backend == JobBackend::AppServer && job.process_state == ProcessState::Alive {
        supervisor_stop_app_server_turn(job_id)?;
        mark_job_stopped(job_id)?;
        return Ok(());
    }
    if let Some(pid) = job.pid {
        signal_term(pid);
    }
    mark_job_stopped(job_id)?;
    Ok(())
}

fn mark_job_stopped(job_id: &str) -> Result<Job> {
    update_job(job_id, |job| {
        job.status = JobStatus::Stopped;
        job.process_state = ProcessState::Exited;
        job.pid = None;
        job.active_worker_pid = None;
        job.codex_turn_id = None;
        job.completed_at = Some(now_iso());
        job.last_summary = Some("stopped".to_string());
        Ok(())
    })
}

pub fn remove_job(job_id: &str, options: RemoveOptions) -> Result<()> {
    let job = require_job(job_id)?;
    if job.pid.is_some() {
        stop_job(job_id)?;
    }
    if worktree_has_changes(job.worktree_path.as_deref())? && !options.force {
        bail!(
            "Worktree has uncommitted changes; refusing to remove {}. Use --force to override.",
            job.worktree_path.unwrap_or_default()
        );
    }
    remove_worktree(job.worktree_path.as_deref(), options.force)?;
    update_job(job_id, |job| {
        job.deleted = true;
        job.archived = true;
        job.status = JobStatus::Stopped;
        job.process_state = ProcessState::Exited;
        job.pid = None;
        job.active_worker_pid = None;
        job.codex_turn_id = None;
        job.last_summary = Some("deleted".to_string());
        Ok(())
    })?;
    if options.purge {
        remove_job_files(job_id)?;
    }
    Ok(())
}

pub fn archive_job(job_id: &str, archived: bool) -> Result<()> {
    require_job(job_id)?;
    update_job(job_id, |job| {
        job.archived = archived;
        Ok(())
    })?;
    Ok(())
}

pub fn rename_job(job_id: &str, title: &str) -> Result<()> {
    require_job(job_id)?;
    update_job(job_id, |job| {
        job.title = title.to_string();
        Ok(())
    })?;
    Ok(())
}

pub fn reorder_jobs(job_ids: &[String]) -> Result<()> {
    if job_ids.is_empty() {
        return Ok(());
    }
    with_store(|store| {
        for (index, job_id) in job_ids.iter().enumerate() {
            let Some(job) = store.jobs.get_mut(job_id) else {
                bail!("Unknown job: {job_id}");
            };
            job.manual_order = Some(index as i64);
        }
        Ok(())
    })
}

pub fn pin_job(job_id: &str, pinned: Option<bool>) -> Result<()> {
    let current = require_job(job_id)?;
    update_job(job_id, |job| {
        job.pinned = pinned.unwrap_or(!current.pinned);
        Ok(())
    })?;
    Ok(())
}

pub fn doctor() -> DoctorReport {
    DoctorReport {
        codex: command_exists("codex"),
        rustc: command_version("rustc"),
    }
}

pub fn parse_dispatch_prompt(input: &str, cwd: &Path) -> ParsedDispatchPrompt {
    let mut prompt = input.trim().to_string();
    let mut model = None;
    let mut profile = None;
    let mut target_cwd = cwd.to_path_buf();

    if let Some(rest) = prompt.strip_prefix("model:") {
        if let Some((value, remaining)) = split_first_token(rest) {
            model = Some(value.to_string());
            prompt = remaining.trim().to_string();
        }
    }

    if let Some(rest) = prompt.strip_prefix("profile:") {
        if let Some((value, remaining)) = split_first_token(rest) {
            profile = Some(value.to_string());
            prompt = remaining.trim().to_string();
        }
    }

    if let Some(rest) = prompt.strip_prefix('@') {
        if let Some((repo, remaining)) = split_first_token(rest) {
            target_cwd = cwd.parent().unwrap_or(cwd).join(repo);
            prompt = remaining.trim().to_string();
        }
    }

    ParsedDispatchPrompt {
        prompt,
        cwd: absolute_path(target_cwd).unwrap_or_else(|_| cwd.to_path_buf()),
        model,
        profile,
    }
}

fn split_first_token(input: &str) -> Option<(&str, &str)> {
    let trimmed = input.trim_start();
    if trimmed.is_empty() {
        return None;
    }
    match trimmed.find(char::is_whitespace) {
        Some(index) => Some((&trimmed[..index], &trimmed[index..])),
        None => Some((trimmed, "")),
    }
}

fn spawn_worker(
    job_id: &str,
    mode: &str,
    prompt: Option<&str>,
    cwd: &str,
) -> Result<std::process::Child> {
    let mut command = Command::new(worker_binary()?);
    command
        .arg("__worker")
        .arg(job_id)
        .arg(mode)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if let Some(prompt) = prompt {
        command.arg(prompt);
    }
    command.spawn().context("failed to spawn agentview worker")
}

fn worker_binary() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("AGENTVIEW_BIN") {
        return Ok(PathBuf::from(path));
    }
    std::env::current_exe().context("failed to locate current executable")
}

fn absolute_path(path: PathBuf) -> Result<PathBuf> {
    if path.is_absolute() {
        Ok(path)
    } else {
        Ok(std::env::current_dir()?.join(path))
    }
}

fn command_version(command: &str) -> Option<String> {
    let output = Command::new(command).arg("--version").output().ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

#[cfg(unix)]
fn signal_term(pid: u32) {
    unsafe {
        libc::kill(pid as i32, libc::SIGTERM);
    }
}

#[cfg(not(unix))]
fn signal_term(_pid: u32) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_prompt_extracts_model_profile_and_repo() {
        let cwd = PathBuf::from("/tmp/current");
        let parsed = parse_dispatch_prompt("model:gpt-5.2-codex @other fix tests", &cwd);
        assert_eq!(parsed.model.as_deref(), Some("gpt-5.2-codex"));
        assert_eq!(parsed.prompt, "fix tests");
        assert_eq!(parsed.cwd, PathBuf::from("/tmp/other"));

        let parsed = parse_dispatch_prompt("profile:fast summarize", &cwd);
        assert_eq!(parsed.profile.as_deref(), Some("fast"));
        assert_eq!(parsed.prompt, "summarize");
    }

    #[test]
    fn default_dispatch_backend_is_app_server() {
        assert_eq!(
            DispatchOptions::default().backend,
            DispatchBackend::AppServer
        );
    }

    #[test]
    fn request_user_input_response_answers_each_question() {
        let response = server_request_response(
            "item/tool/requestUserInput",
            &json!({
                "questions": [
                    { "id": "confirm_path", "header": "Confirm", "question": "Use this path?" },
                    { "id": "reason", "header": "Reason", "question": "Why?" }
                ]
            }),
            "yes",
        )
        .unwrap();

        assert_eq!(
            response,
            json!({
                "answers": {
                    "confirm_path": { "answers": ["yes"] },
                    "reason": { "answers": ["yes"] }
                }
            })
        );
    }

    #[test]
    fn approval_responses_use_codex_protocol_decisions() {
        assert_eq!(
            server_request_response(
                "item/commandExecution/requestApproval",
                &json!({}),
                "approved"
            )
            .unwrap(),
            json!({ "decision": "accept" })
        );
        assert_eq!(
            server_request_response("item/fileChange/requestApproval", &json!({}), "declined")
                .unwrap(),
            json!({ "decision": "decline" })
        );
        assert_eq!(
            server_request_response("execCommandApproval", &json!({}), "approved").unwrap(),
            json!({ "decision": "approved" })
        );
        assert_eq!(
            server_request_response("applyPatchApproval", &json!({}), "declined").unwrap(),
            json!({ "decision": "denied" })
        );
    }

    #[test]
    fn permissions_response_grants_or_denies_requested_profile() {
        let params = json!({
            "permissions": {
                "network": { "enabled": true },
                "fileSystem": { "read": ["/tmp/read"], "write": ["/tmp/write"] }
            }
        });

        assert_eq!(
            server_request_response("item/permissions/requestApproval", &params, "approve")
                .unwrap(),
            json!({
                "permissions": {
                    "network": { "enabled": true },
                    "fileSystem": { "read": ["/tmp/read"], "write": ["/tmp/write"] }
                },
                "scope": "turn"
            })
        );
        assert_eq!(
            server_request_response("item/permissions/requestApproval", &params, "decline")
                .unwrap(),
            json!({ "permissions": {}, "scope": "turn" })
        );
    }
}
