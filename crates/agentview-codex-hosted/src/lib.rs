use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostedSessionConfig {
    pub thread_id: String,
    pub cwd: PathBuf,
    pub remote_url: Option<String>,
    pub remote_auth_token: Option<String>,
    pub no_alt_screen: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostedSessionExit {
    Detached,
    Quit(i32),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostedHelper {
    binary: PathBuf,
}

impl HostedHelper {
    pub fn new(binary: impl Into<PathBuf>) -> Self {
        Self {
            binary: binary.into(),
        }
    }

    pub fn from_env_or_default() -> Self {
        let binary = std::env::var_os("AGENTVIEW_CODEX_HOSTED")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                std::env::current_exe()
                    .ok()
                    .and_then(|path| {
                        path.parent()
                            .map(|parent| parent.join("agentview-codex-hosted"))
                    })
                    .filter(|path| path.exists())
                    .unwrap_or_else(|| PathBuf::from("agentview-codex-hosted"))
            });
        Self::new(binary)
    }

    pub fn binary(&self) -> &Path {
        &self.binary
    }

    pub fn build_args(config: &HostedSessionConfig) -> Vec<String> {
        let mut args = vec![
            "--thread-id".to_string(),
            config.thread_id.clone(),
            "--cwd".to_string(),
            config.cwd.to_string_lossy().into_owned(),
        ];

        if let Some(remote_url) = &config.remote_url {
            args.extend(["--app-server-url".to_string(), remote_url.clone()]);
        }
        if let Some(remote_auth_token) = &config.remote_auth_token {
            args.extend([
                "--app-server-auth-token".to_string(),
                remote_auth_token.clone(),
            ]);
        }
        if config.no_alt_screen {
            args.push("--no-alt-screen".to_string());
        }

        args
    }

    pub fn run(&self, config: &HostedSessionConfig) -> Result<HostedSessionExit> {
        if config.thread_id.trim().is_empty() {
            bail!("hosted Codex thread id is empty");
        }

        let status = Command::new(&self.binary)
            .args(Self::build_args(config))
            .current_dir(&config.cwd)
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .status()
            .with_context(|| {
                format!(
                    "failed to start hosted Codex helper {}",
                    self.binary.display()
                )
            })?;

        let code = status.code().unwrap_or(1);
        if code == 0 {
            Ok(HostedSessionExit::Detached)
        } else {
            Ok(HostedSessionExit::Quit(code))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn helper_args_include_required_thread_and_cwd() {
        let config = HostedSessionConfig {
            thread_id: "thread-123".to_string(),
            cwd: PathBuf::from("/tmp/worktree"),
            remote_url: None,
            remote_auth_token: None,
            no_alt_screen: false,
        };

        assert_eq!(
            HostedHelper::build_args(&config),
            vec!["--thread-id", "thread-123", "--cwd", "/tmp/worktree",]
        );
    }

    #[test]
    fn helper_args_include_remote_and_alt_screen_options() {
        let config = HostedSessionConfig {
            thread_id: "thread-123".to_string(),
            cwd: PathBuf::from("/tmp/worktree"),
            remote_url: Some("ws://127.0.0.1:1234".to_string()),
            remote_auth_token: Some("token".to_string()),
            no_alt_screen: true,
        };

        assert_eq!(
            HostedHelper::build_args(&config),
            vec![
                "--thread-id",
                "thread-123",
                "--cwd",
                "/tmp/worktree",
                "--app-server-url",
                "ws://127.0.0.1:1234",
                "--app-server-auth-token",
                "token",
                "--no-alt-screen",
            ]
        );
    }
}
