use agentview_codex_app_server::{
    AppServerClient, AppServerEvent, ThreadResumeOptions, ThreadStartOptions,
};
pub use agentview_codex_app_server::{Notification, ServerRequest};
use anyhow::{Context, Result, bail};
use codex_app_server_protocol::{
    ApplyPatchApprovalResponse, CommandExecutionApprovalDecision,
    CommandExecutionRequestApprovalResponse, ExecCommandApprovalResponse,
    FileChangeApprovalDecision, FileChangeRequestApprovalResponse, GrantedPermissionProfile,
    PermissionGrantScope, PermissionsRequestApprovalResponse, ToolRequestUserInputAnswer,
    ToolRequestUserInputParams, ToolRequestUserInputResponse,
};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct RuntimeTurnOptions {
    pub cwd: PathBuf,
    pub model: Option<String>,
    pub approval_policy: String,
    pub sandbox: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum RuntimeEvent {
    Initialized {
        user_agent: String,
        codex_home: PathBuf,
    },
    ThreadStarted {
        thread_id: String,
    },
    ThreadResumed {
        thread_id: String,
    },
    TurnStarted {
        thread_id: String,
        turn_id: String,
    },
    Notification(Notification),
    ServerRequest(ServerRequest),
}

#[derive(Debug, Clone, PartialEq)]
pub struct RuntimeInitialized {
    pub user_agent: String,
    pub codex_home: PathBuf,
}

#[derive(Debug, Clone)]
pub struct CodexRuntime {
    poll_interval: Duration,
    codex_binary: Option<PathBuf>,
    listen_url: Option<String>,
}

#[derive(Debug)]
pub struct CodexRuntimeSession {
    client: AppServerClient,
    poll_interval: Duration,
    initialized: RuntimeInitialized,
}

pub fn server_request_response(method: &str, params: &Value, prompt: &str) -> Result<Value> {
    match method {
        "item/tool/requestUserInput" => user_input_response(params, prompt),
        "item/commandExecution/requestApproval" => {
            let response = CommandExecutionRequestApprovalResponse {
                decision: v2_command_approval_decision(prompt)?,
            };
            serde_json::to_value(response).context("failed to serialize command approval response")
        }
        "item/fileChange/requestApproval" => {
            let response = FileChangeRequestApprovalResponse {
                decision: v2_file_change_approval_decision(prompt)?,
            };
            serde_json::to_value(response)
                .context("failed to serialize file-change approval response")
        }
        "item/permissions/requestApproval" => permissions_response(params, prompt),
        "applyPatchApproval" => v1_apply_patch_response(prompt),
        "execCommandApproval" => v1_exec_command_response(prompt),
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
    let params: ToolRequestUserInputParams = serde_json::from_value(params.clone())
        .context("failed to parse request-user-input params")?;
    if params.questions.is_empty() {
        bail!("request-user-input has no questions");
    }
    let answers = params
        .questions
        .into_iter()
        .map(|question| {
            (
                question.id,
                ToolRequestUserInputAnswer {
                    answers: vec![answer.to_string()],
                },
            )
        })
        .collect::<HashMap<_, _>>();
    let response = ToolRequestUserInputResponse { answers };
    serde_json::to_value(response).context("failed to serialize request-user-input response")
}

fn permissions_response(params: &Value, prompt: &str) -> Result<Value> {
    let permissions = if is_approve_text(prompt) {
        serde_json::from_value(
            params
                .get("permissions")
                .cloned()
                .unwrap_or_else(|| json!({})),
        )
        .context("failed to parse permissions request profile")?
    } else if is_decline_text(prompt) {
        GrantedPermissionProfile::default()
    } else {
        bail!(
            "Permission requests require `agentview approve <job_id>` or `agentview decline <job_id>`"
        );
    };
    let response = PermissionsRequestApprovalResponse {
        permissions,
        scope: PermissionGrantScope::Turn,
        strict_auto_review: None,
    };
    serde_json::to_value(response).context("failed to serialize permissions approval response")
}

fn v2_command_approval_decision(prompt: &str) -> Result<CommandExecutionApprovalDecision> {
    if is_approve_text(prompt) {
        return Ok(CommandExecutionApprovalDecision::Accept);
    }
    if is_decline_text(prompt) {
        return Ok(CommandExecutionApprovalDecision::Decline);
    }
    bail!("Approval requests require `agentview approve <job_id>` or `agentview decline <job_id>`")
}

fn v2_file_change_approval_decision(prompt: &str) -> Result<FileChangeApprovalDecision> {
    if is_approve_text(prompt) {
        return Ok(FileChangeApprovalDecision::Accept);
    }
    if is_decline_text(prompt) {
        return Ok(FileChangeApprovalDecision::Decline);
    }
    bail!("Approval requests require `agentview approve <job_id>` or `agentview decline <job_id>`")
}

fn v1_apply_patch_response(prompt: &str) -> Result<Value> {
    let response: ApplyPatchApprovalResponse =
        serde_json::from_value(json!({ "decision": v1_review_decision(prompt)? }))
            .context("failed to build v1 apply-patch approval response")?;
    serde_json::to_value(response).context("failed to serialize v1 apply-patch approval response")
}

fn v1_exec_command_response(prompt: &str) -> Result<Value> {
    let response: ExecCommandApprovalResponse =
        serde_json::from_value(json!({ "decision": v1_review_decision(prompt)? }))
            .context("failed to build v1 exec-command approval response")?;
    serde_json::to_value(response).context("failed to serialize v1 exec-command approval response")
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

impl Default for CodexRuntime {
    fn default() -> Self {
        Self {
            poll_interval: Duration::from_millis(250),
            codex_binary: None,
            listen_url: None,
        }
    }
}

impl CodexRuntime {
    pub fn with_codex_binary(mut self, codex_binary: impl Into<PathBuf>) -> Self {
        self.codex_binary = Some(codex_binary.into());
        self
    }

    pub fn with_listen_url(mut self, listen_url: impl Into<String>) -> Self {
        self.listen_url = Some(listen_url.into());
        self
    }

    pub fn run_text_turn(
        &self,
        options: RuntimeTurnOptions,
        prompt: &str,
        mut on_event: impl FnMut(RuntimeEvent) -> Result<()>,
    ) -> Result<()> {
        let mut session = self.spawn_session()?;
        on_event(session.initialized_event())?;

        let thread_id = session.start_thread(options)?;
        on_event(RuntimeEvent::ThreadStarted {
            thread_id: thread_id.clone(),
        })?;

        self.start_turn_and_drain(&mut session, thread_id, prompt, &mut on_event)?;

        session.shutdown()?;
        Ok(())
    }

    pub fn run_text_turn_on_thread(
        &self,
        thread_id: &str,
        options: RuntimeTurnOptions,
        prompt: &str,
        mut on_event: impl FnMut(RuntimeEvent) -> Result<()>,
    ) -> Result<()> {
        let mut session = self.spawn_session()?;
        on_event(session.initialized_event())?;

        let thread_id = session.resume_thread(thread_id, options)?;
        on_event(RuntimeEvent::ThreadResumed {
            thread_id: thread_id.clone(),
        })?;

        self.start_turn_and_drain(&mut session, thread_id, prompt, &mut on_event)?;

        session.shutdown()?;
        Ok(())
    }

    pub fn spawn_session(&self) -> Result<CodexRuntimeSession> {
        let mut client = self.spawn_client()?;
        let initialized = client.initialize()?;
        Ok(CodexRuntimeSession {
            client,
            poll_interval: self.poll_interval,
            initialized: RuntimeInitialized {
                user_agent: initialized.user_agent,
                codex_home: initialized.codex_home.into_path_buf(),
            },
        })
    }

    fn start_turn_and_drain(
        &self,
        session: &mut CodexRuntimeSession,
        thread_id: String,
        prompt: &str,
        on_event: &mut impl FnMut(RuntimeEvent) -> Result<()>,
    ) -> Result<()> {
        let turn_id = session.start_text_turn(&thread_id, prompt)?;
        on_event(RuntimeEvent::TurnStarted {
            thread_id: thread_id.clone(),
            turn_id,
        })?;

        loop {
            match session.next_event()? {
                Some(RuntimeEvent::Notification(notification)) => {
                    let completed = notification.method == "turn/completed";
                    on_event(RuntimeEvent::Notification(notification))?;
                    if completed {
                        break;
                    }
                }
                Some(RuntimeEvent::ServerRequest(request)) => {
                    on_event(RuntimeEvent::ServerRequest(request))?;
                }
                Some(event) => on_event(event)?,
                None => {}
            }
        }

        Ok(())
    }

    fn spawn_client(&self) -> Result<AppServerClient> {
        if let Some(listen_url) = &self.listen_url {
            if let Some(codex_binary) = &self.codex_binary {
                let mut command = Command::new(codex_binary);
                command.args(["app-server", "--listen", listen_url]);
                return AppServerClient::spawn_websocket_with_command(command, listen_url);
            }
            return AppServerClient::spawn_websocket(listen_url);
        }
        if let Some(codex_binary) = &self.codex_binary {
            let mut command = Command::new(codex_binary);
            command.args(["app-server", "--listen", "stdio://"]);
            return AppServerClient::spawn_with_command(command);
        }
        AppServerClient::spawn_stdio()
    }
}

impl CodexRuntimeSession {
    pub fn initialized(&self) -> &RuntimeInitialized {
        &self.initialized
    }

    pub fn initialized_event(&self) -> RuntimeEvent {
        RuntimeEvent::Initialized {
            user_agent: self.initialized.user_agent.clone(),
            codex_home: self.initialized.codex_home.clone(),
        }
    }

    pub fn start_thread(&mut self, options: RuntimeTurnOptions) -> Result<String> {
        let started = self.client.start_thread(ThreadStartOptions {
            cwd: Some(options.cwd),
            model: options.model,
            approval_policy: Some(options.approval_policy),
            sandbox: Some(options.sandbox),
        })?;
        Ok(started.thread.id)
    }

    pub fn resume_thread(
        &mut self,
        thread_id: &str,
        options: RuntimeTurnOptions,
    ) -> Result<String> {
        let resumed = self.client.resume_thread(ThreadResumeOptions {
            thread_id: thread_id.to_string(),
            cwd: Some(options.cwd),
            model: options.model,
            approval_policy: Some(options.approval_policy),
            sandbox: Some(options.sandbox),
        })?;
        Ok(resumed.thread.id)
    }

    pub fn start_text_turn(&mut self, thread_id: &str, prompt: &str) -> Result<String> {
        let turn = self.client.start_text_turn(thread_id, prompt)?;
        Ok(turn.turn.id)
    }

    pub fn interrupt_turn(&mut self, thread_id: &str, turn_id: &str) -> Result<()> {
        self.client.interrupt_turn(thread_id, turn_id)
    }

    pub fn resolve_server_request(&mut self, request_id: &Value, result: Value) -> Result<()> {
        self.client.resolve_server_request(request_id, result)
    }

    pub fn next_event(&mut self) -> Result<Option<RuntimeEvent>> {
        match self.client.next_event(self.poll_interval)? {
            Some(AppServerEvent::Notification(notification)) => {
                Ok(Some(RuntimeEvent::Notification(notification)))
            }
            Some(AppServerEvent::ServerRequest(request)) => {
                Ok(Some(RuntimeEvent::ServerRequest(request)))
            }
            None => Ok(None),
        }
    }

    pub fn shutdown(self) -> Result<()> {
        self.client.shutdown()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use tempfile::TempDir;

    #[test]
    fn run_text_turn_emits_thread_turn_and_notifications() {
        let temp = TempDir::new().unwrap();
        let codex = fake_codex(&temp);

        let mut events = Vec::new();
        CodexRuntime::default()
            .with_codex_binary(codex)
            .run_text_turn(
                RuntimeTurnOptions {
                    cwd: temp.path().to_path_buf(),
                    model: None,
                    approval_policy: "never".to_string(),
                    sandbox: "workspace-write".to_string(),
                },
                "hello",
                |event| {
                    events.push(event);
                    Ok(())
                },
            )
            .unwrap();
        assert!(matches!(events[0], RuntimeEvent::Initialized { .. }));
        assert!(matches!(
            events[1],
            RuntimeEvent::ThreadStarted { ref thread_id } if thread_id == "thread-1"
        ));
        assert!(matches!(
            events[2],
            RuntimeEvent::TurnStarted { ref thread_id, ref turn_id }
                if thread_id == "thread-1" && turn_id == "turn-1"
        ));
        assert!(events.iter().any(|event| matches!(
            event,
            RuntimeEvent::Notification(notification)
                if notification.method == "item/agentMessage/delta"
        )));
        assert!(events.iter().any(|event| matches!(
            event,
            RuntimeEvent::Notification(notification)
                if notification.method == "turn/completed"
        )));
    }

    #[test]
    fn run_text_turn_on_thread_resumes_then_starts_turn() {
        let temp = TempDir::new().unwrap();
        let codex = fake_codex(&temp);

        let mut events = Vec::new();
        CodexRuntime::default()
            .with_codex_binary(codex)
            .run_text_turn_on_thread(
                "thread-1",
                RuntimeTurnOptions {
                    cwd: temp.path().to_path_buf(),
                    model: None,
                    approval_policy: "never".to_string(),
                    sandbox: "workspace-write".to_string(),
                },
                "follow up",
                |event| {
                    events.push(event);
                    Ok(())
                },
            )
            .unwrap();

        assert!(matches!(events[0], RuntimeEvent::Initialized { .. }));
        assert!(matches!(
            events[1],
            RuntimeEvent::ThreadResumed { ref thread_id } if thread_id == "thread-1"
        ));
        assert!(matches!(
            events[2],
            RuntimeEvent::TurnStarted { ref thread_id, ref turn_id }
                if thread_id == "thread-1" && turn_id == "turn-1"
        ));
    }

    #[test]
    fn request_user_input_response_answers_each_question_with_protocol_shape() {
        let response = server_request_response(
            "item/tool/requestUserInput",
            &json!({
                "threadId": "thread-1",
                "turnId": "turn-1",
                "itemId": "item-1",
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
    fn approval_responses_serialize_through_codex_protocol_types() {
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
    fn permissions_response_grants_or_denies_requested_profile_with_protocol_shape() {
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

    fn fake_codex(temp: &TempDir) -> PathBuf {
        let bin = temp.path().join("bin");
        fs::create_dir_all(&bin).unwrap();
        let codex = bin.join("codex");
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
printf '%s\n' '{{"id":0,"result":{{"userAgent":"fake-codex/0.0.0","codexHome":"{codex_home}","platformFamily":"unix","platformOs":"macos"}}}}'
IFS= read -r initialized
IFS= read -r thread_request
case "$thread_request" in
  *'"method":"thread/start"'*) thread_method="started" ;;
  *'"method":"thread/resume"'*) thread_method="resumed" ;;
  *) printf '%s\n' "expected thread start/resume, got: $thread_request" >&2; exit 3 ;;
esac
printf '%s\n' '{{"id":1,"result":{{"thread":{{"id":"thread-1","sessionId":"thread-1","preview":"","status":"running","cwd":"{cwd}","name":null}},"model":"fake-model","modelProvider":"fake-provider","serviceTier":null,"cwd":"{cwd}"}}}}'
IFS= read -r turn_start
printf '%s\n' '{{"id":2,"result":{{"turn":{{"id":"turn-1","status":"running","startedAt":0,"completedAt":null,"durationMs":null}}}}}}'
printf '%s\n' "{{\"method\":\"item/agentMessage/delta\",\"params\":{{\"threadId\":\"thread-1\",\"turnId\":\"turn-1\",\"itemId\":\"item-1\",\"delta\":\"$thread_method\"}}}}"
printf '%s\n' '{{"method":"turn/completed","params":{{"threadId":"thread-1","turn":{{"id":"turn-1","status":"completed","startedAt":0,"completedAt":1,"durationMs":1}}}}}}'
"#,
                codex_home = codex_home.display(),
                cwd = temp.path().display()
            ),
        )
        .unwrap();
        let mut permissions = fs::metadata(&codex).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&codex, permissions).unwrap();
        codex
    }
}
