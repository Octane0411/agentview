# Rust AgentView + Codex TUI Reuse Plan

Status: Rust MVP implemented; hosted Codex TUI reuse still planned
Date: 2026-05-13

## Decision Context

AgentView is still early enough that a Rust rewrite is realistic.

The product goal is no longer just a CLI fallback around `codex resume`. We want
Claude Agent View-style switching between a session list and attached sessions,
while keeping the attached Codex session experience as close as possible to the
native Codex CLI/TUI.

The important technical facts are:

- Codex CLI already exposes `codex app-server`.
- Codex TUI is a Rust/ratatui app-server client.
- Codex app-server exports typed protocol bindings with `codex app-server generate-ts`.
- The Codex repository is Apache-2.0.
- Codex TUI is not currently exposed as a clean embedded widget API; many
  relevant pieces are `pub(crate)` and tied to the full TUI application.

## Recommended Direction

Rewrite AgentView as a Rust workspace and integrate Codex through app-server.

Do not keep the TypeScript TUI as the long-term architecture. It was useful for
proving the workflow and E2E harness, but it makes native Codex session parity
harder because Codex TUI is Rust/ratatui.

Do not rely on spawning `codex resume` for parity. That is a fallback attach
mode, not a native Agent View mode.

Do not copy Codex TUI source into this repository and fork it manually. That
will diverge quickly.

Instead, keep Codex as an upstream dependency with a minimal patch surface that
exposes a hosted session view.

## Target Architecture

```text
agentview
  crates/agentview-cli
    - command parsing
    - terminal entrypoint

  crates/agentview-core
    - job/session metadata
    - state machine
    - worktree manager
    - persistence

  crates/agentview-tui
    - Agent View list UI
    - grouping/filtering/keyboard routing
    - peek panel
    - hosted Codex session container

  third_party/codex
    - openai/codex git submodule or subtree
    - patched only where needed to expose hosted session APIs

  patches/codex
    - minimal patch queue against upstream Codex
```

Runtime shape:

```text
AgentView TUI process
  -> owns list view and key routing
  -> starts/connects to Codex app-server
  -> creates/resumes Codex threads
  -> switches into hosted Codex session view
  -> detaches back to list without stopping the thread

Codex app-server
  -> owns thread, turn, transcript, approval, tool, command, and diff state
```

Ownership:

- Codex owns conversation/thread/turn state.
- AgentView owns job metadata, list ordering/grouping, worktree mapping, and
  Agent View UI state.
- Hosted Codex session view owns the native-feeling conversation UI while
  attached.

## Rust Package Management Model

Rust package units are crates. A repository can contain a workspace with many
crates.

`Cargo.toml` at the workspace root lists member crates:

```toml
[workspace]
members = [
  "crates/agentview-cli",
  "crates/agentview-core",
  "crates/agentview-tui",
]
resolver = "2"
```

Each crate has its own `Cargo.toml`, and dependencies can be:

- crates.io packages, for example `ratatui = "0.x"`.
- local path dependencies, for example
  `agentview-core = { path = "../agentview-core" }`.
- git dependencies, for example
  `codex-app-server-protocol = { git = "...", rev = "..." }`.

For Codex, a plain git dependency is unlikely to be enough because the Codex
workspace uses many internal path crates. We need one of the integration
strategies below.

## Codex Integration Options

### Option A: Git Submodule

Keep `openai/codex` as `third_party/codex`.

Pros:

- Easy to pin exact upstream commit.
- Easy to inspect and build against Codex crates locally.
- Easy to keep our patch queue separate.
- Familiar update workflow.

Cons:

- Requires submodule setup for contributors.
- We need scripts to apply/reapply patches.

Recommended for the first serious implementation.

### Option B: Git Subtree

Vendor Codex into this repo as a subtree.

Pros:

- No submodule checkout friction.
- Single repository contains everything.

Cons:

- Larger repository.
- Upstream updates are noisier.
- Patch boundaries are less obvious.

Good later if submodule friction is painful.

### Option C: Fork Codex

Maintain a fork of `openai/codex` and build AgentView inside or next to that
fork.

Pros:

- Easiest way to modify private Codex TUI internals.
- Most native Codex TUI reuse.

Cons:

- Highest long-term maintenance cost.
- Product becomes coupled to Codex fork lifecycle.
- Harder to keep AgentView as a standalone project.

Useful only if hosted session APIs require too much upstream patching.

### Option D: Consume Released Codex Crates

Depend on published Codex crates from crates.io.

Pros:

- Clean Cargo dependency model.
- No vendored upstream source.

Cons:

- Not viable unless the needed Codex crates are published with stable public
  APIs.
- Codex TUI currently does not expose a hosted session widget API.

Not recommended right now.

## Recommended Initial Integration

Use Option A: Codex as a git submodule plus a small patch queue.

Initial layout:

```text
third_party/codex     # git submodule pinned to upstream commit
patches/codex/*.patch # our minimal hosted-mode patches
tools/update-codex.sh # update/rebase/apply-patches helper
```

Root workspace can either:

1. Keep AgentView as its own workspace and depend on Codex crates by path into
   `third_party/codex/codex-rs/...`.
2. Generate a temporary combined workspace for local development.

Start with path dependencies. If Cargo workspace inheritance causes friction
with Codex's own workspace dependencies, switch to a combined workspace helper
script or make AgentView a workspace member inside `third_party/codex/codex-rs`
for the spike only.

## Hosted Session View Goal

We need a thin API in Codex TUI roughly shaped like:

```rust
pub struct HostedSessionConfig {
    pub thread_id: String,
    pub cwd: PathBuf,
    pub detach_keys: Vec<KeyBinding>,
}

pub enum HostedSessionExit {
    Detached,
    Closed,
    Fatal(String),
}

pub async fn run_hosted_session_view(
    app_server: AppServerClient,
    config: HostedSessionConfig,
    terminal: &mut Terminal,
) -> Result<HostedSessionExit>;
```

The actual API can differ, but it must support:

- render a specific existing Codex thread
- send user prompts
- handle approval requests
- handle request-user-input
- show command output and file diffs with native Codex rendering
- detach without stopping the turn or thread
- return control to AgentView list view

Patch principle:

- expose the smallest API needed
- avoid modifying Codex rendering logic
- avoid duplicating Codex event handling
- keep detach behavior as a host-level escape hatch

## AgentView List View Responsibilities

AgentView still implements:

- session table
- grouping and ordering
- pinned sessions
- ready-for-review group
- job/session mapping
- worktree creation and cleanup
- summary rows
- PR status resolution
- CLI commands
- supervisor lifecycle
- persistence

Codex TUI hosted view implements:

- full conversation rendering
- composer behavior while attached
- approval panels
- command output rendering
- diff rendering
- Codex-specific session shortcuts where compatible

## App-Server Responsibilities

AgentView should talk to app-server directly for list-level behavior:

- `thread/start`
- `thread/resume`
- `thread/read`
- `thread/list`
- `turn/start`
- `turn/steer`
- `turn/interrupt`

AgentView should subscribe to and normalize:

- `thread/status/changed`
- `turn/started`
- `turn/completed`
- `turn/diff/updated`
- `item/agentMessage/delta`
- `item/commandExecution/requestApproval`
- `item/fileChange/requestApproval`
- `item/tool/requestUserInput`
- `item/permissions/requestApproval`

This enables the Agent View list to show `needs input` and handle peek/reply
without entering the session.

## Migration Plan

### Phase 0: Retire TS Prototype

The TypeScript implementation was useful as a behavioral prototype, but the
runtime has moved to Rust.

Current state:

- Rust workspace exists under `crates/`.
- CLI, local store, worker execution, worktree isolation, fallback attach, and
  a ratatui list TUI are implemented in Rust.
- The JS/TS runtime has been removed.
- Fake-Codex integration coverage verifies dispatch, list, peek, attach, and
  conservative worktree deletion.
- Real-Codex E2E and app-server/hosted Codex TUI reuse are still pending.

### Phase 1: Rust Workspace Spike

Goal: prove dependency and app-server wiring.

Tasks:

1. Create Rust workspace under the existing repo.
2. Add Codex as `third_party/codex` submodule pinned to a known commit.
3. Build a small Rust binary that starts `codex app-server --listen stdio://`
   or uses Codex app-server client directly if path dependencies work cleanly.
4. Send `initialize`, `thread/start`, and `turn/start`.
5. Print normalized notifications to stdout.
6. Verify with a real Codex turn in a temporary git repo.

Exit criteria:

- A real Codex thread can be started from Rust.
- Thread id is captured.
- Turn events stream into our code.
- No dependency approach blocker remains.

### Phase 2: AgentView Rust List MVP

Goal: replace TS list/dispatch with Rust list over app-server.

Tasks:

1. Implement job store.
2. Implement worktree manager.
3. Implement app-server adapter.
4. Implement list TUI in ratatui.
5. Implement dispatch from input.
6. Implement grouped rows and latest summaries from events.
7. Implement stop/rm/respawn.

Exit criteria:

- `agentview` opens Rust TUI.
- `agentview run` dispatches background Codex sessions through app-server.
- Rows update from app-server events.
- Worktree isolation works.
- Existing real Codex E2E flow is ported.

### Phase 3: Hosted Codex Session View Spike

Goal: prove Codex TUI session view reuse.

Tasks:

1. Patch Codex TUI to expose a hosted-session entrypoint.
2. Add detach key handling that returns to host instead of exiting Codex.
3. Enter hosted view from AgentView selected row.
4. Detach back to AgentView without stopping active thread.
5. Preserve Codex-native rendering for messages, approvals, commands, and diffs.

Exit criteria:

- Entering a row feels like Codex CLI session UI.
- Detach returns to AgentView list.
- Active turn continues after detach.
- Re-entering shows the same thread state.

### Phase 4: List-Level Needs Input

Goal: match Claude Agent View's main loop.

Tasks:

1. Store pending server request ids per job.
2. Render `needs input` rows from structured app-server requests.
3. Show request details in peek.
4. Reply from peek to `tool/requestUserInput`.
5. Approve/decline command, file change, and permission requests.
6. Clear pending request when app-server emits `serverRequest/resolved`.

Exit criteria:

- User can handle common approval/input cases without entering session view.
- Session resumes and row state updates correctly.

### Phase 5: Parity Polish

Goal: close user-visible parity gaps in `docs/codex-agent-view-spec.md`.

Tasks:

1. Ready-for-review group.
2. PR status dot with GitHub/`gh` resolution.
3. Generated row summaries.
4. Filtering syntax.
5. Reorder and collapse behavior.
6. Disable/troubleshooting semantics if still in spec.
7. `respawn --all`.

Exit criteria:

- Spec acceptance criteria pass.
- Product no longer labels app-server/session behavior as fallback.

## Codex Update Workflow

Pin upstream Codex:

```text
third_party/codex @ <commit-sha>
```

Update steps:

1. Fetch latest upstream Codex.
2. Move submodule to target commit.
3. Reapply `patches/codex`.
4. Regenerate any protocol bindings if we vendor generated types.
5. Run Codex TUI tests touched by our hosted-session patch.
6. Run AgentView unit tests.
7. Run real Codex AgentView E2E.
8. Commit with message like `Update Codex upstream to <sha>`.

Automation:

```bash
tools/update-codex.sh <sha>
tools/check-codex-patches.sh
```

Patch budget:

- Target fewer than 5 patch files.
- Target fewer than 500 changed lines against Codex upstream.
- If patch grows beyond that, reconsider whether we need an upstream PR or a
  Codex fork.

## Test Strategy

Required tests:

- Rust unit tests for state machine and worktree manager.
- App-server adapter tests with mocked JSON-RPC.
- PTY tests for AgentView list navigation.
- Hosted session attach/detach test.
- Real Codex E2E using temporary git repo and isolated Codex/AgentView state.
- Codex update smoke test against pinned upstream.

Real E2E must cover:

- dispatch
- row status updates
- peek
- list-level reply or approval
- attach hosted session
- detach during active turn
- reattach same thread
- worktree dirty delete protection

## Risks

### Codex TUI Internals Change

Mitigation:

- Keep hosted-session patch minimal.
- Prefer upstreaming the hosted API once proven.
- Pin and update deliberately.

### Cargo Workspace Friction

Mitigation:

- Start with submodule path dependencies.
- If workspace inheritance blocks clean builds, create a temporary combined
  workspace for the spike.

### App-Server Protocol Changes

Mitigation:

- Pin Codex commit.
- Generate protocol artifacts in update workflow.
- Add adapter contract tests.

### Native Session View Takes Longer Than Expected

Mitigation:

- Keep CLI fallback attach as temporary escape hatch.
- Ship list-level app-server features independently.

## Recommended Immediate Next Step

Implement Phase 1 app-server wiring on top of the Rust workspace.

The first remaining decision point is whether we can depend on Codex crates
cleanly and drive a real app-server session from Rust.

If Phase 1 succeeds, proceed to Phase 3 hosted session spike before rebuilding
all list polish. The hosted session view is the highest-risk and highest-value
part of the rewrite.
