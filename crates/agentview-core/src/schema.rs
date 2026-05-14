use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobStatus {
    Working,
    NeedsInput,
    Idle,
    Completed,
    Failed,
    Stopped,
}

impl JobStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Working => "working",
            Self::NeedsInput => "needs_input",
            Self::Idle => "idle",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Stopped => "stopped",
        }
    }
}

impl std::fmt::Display for JobStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProcessState {
    Alive,
    Exited,
    Sleeping,
    Unknown,
}

impl ProcessState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Alive => "alive",
            Self::Exited => "exited",
            Self::Sleeping => "sleeping",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobBackend {
    #[default]
    FallbackExec,
    AppServer,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrRef {
    pub url: String,
    pub owner: String,
    pub repo: String,
    pub number: u64,
    pub status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockingRequest {
    #[serde(rename = "type")]
    pub kind: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub event: Option<Value>,
    #[serde(rename = "createdAt")]
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Job {
    pub id: String,
    pub provider: String,
    #[serde(default)]
    pub backend: JobBackend,
    #[serde(rename = "codexThreadId")]
    pub codex_thread_id: Option<String>,
    #[serde(
        rename = "codexTurnId",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub codex_turn_id: Option<String>,
    pub title: String,
    #[serde(rename = "initialPrompt")]
    pub initial_prompt: String,
    #[serde(rename = "repoRoot")]
    pub repo_root: String,
    pub cwd: String,
    #[serde(rename = "dispatchCwd")]
    pub dispatch_cwd: String,
    #[serde(rename = "worktreePath")]
    pub worktree_path: Option<String>,
    #[serde(rename = "worktreeBranch")]
    pub worktree_branch: Option<String>,
    pub model: Option<String>,
    pub profile: Option<String>,
    #[serde(rename = "approvalPolicy")]
    pub approval_policy: String,
    pub sandbox: String,
    pub status: JobStatus,
    #[serde(rename = "processState")]
    pub process_state: ProcessState,
    pub pid: Option<u32>,
    #[serde(rename = "activeWorkerPid")]
    pub active_worker_pid: Option<u32>,
    pub pinned: bool,
    #[serde(rename = "manualOrder")]
    pub manual_order: Option<i64>,
    pub archived: bool,
    pub deleted: bool,
    #[serde(rename = "lastSummary")]
    pub last_summary: Option<String>,
    #[serde(rename = "lastOutput")]
    pub last_output: Option<String>,
    #[serde(rename = "blockingRequest")]
    pub blocking_request: Option<BlockingRequest>,
    #[serde(rename = "prRefs")]
    pub pr_refs: Vec<PrRef>,
    #[serde(rename = "createdAt")]
    pub created_at: String,
    #[serde(rename = "updatedAt")]
    pub updated_at: String,
    #[serde(rename = "completedAt")]
    pub completed_at: Option<String>,
    #[serde(rename = "exitCode")]
    pub exit_code: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Store {
    pub version: u32,
    pub jobs: BTreeMap<String, Job>,
    #[serde(default)]
    pub preferences: BTreeMap<String, Value>,
}

impl Default for Store {
    fn default() -> Self {
        Self {
            version: 1,
            jobs: BTreeMap::new(),
            preferences: BTreeMap::new(),
        }
    }
}
