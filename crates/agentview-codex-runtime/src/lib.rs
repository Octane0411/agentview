use agentview_codex_app_server::{
    AppServerClient, AppServerEvent, ThreadResumeOptions, ThreadStartOptions,
};
pub use agentview_codex_app_server::{Notification, ServerRequest};
use anyhow::Result;
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
}

#[derive(Debug)]
pub struct CodexRuntimeSession {
    client: AppServerClient,
    poll_interval: Duration,
    initialized: RuntimeInitialized,
}

impl Default for CodexRuntime {
    fn default() -> Self {
        Self {
            poll_interval: Duration::from_millis(250),
            codex_binary: None,
        }
    }
}

impl CodexRuntime {
    pub fn with_codex_binary(mut self, codex_binary: impl Into<PathBuf>) -> Self {
        self.codex_binary = Some(codex_binary.into());
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
                codex_home: initialized.codex_home,
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
