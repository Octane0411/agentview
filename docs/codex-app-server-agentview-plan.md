# Codex App-Server AgentView Plan

Status: active architecture plan
Date: 2026-05-14
Observed Codex CLI: `codex-cli 0.130.0`
Observed Codex source tag: `rust-v0.130.0`

This plan supersedes `docs/rust-codex-tui-plan.md` for the next implementation
phase.

## First Principles

AgentView is not a wrapper around terminal processes. AgentView is a session
controller.

To match Claude Agent View, AgentView must own the list, dispatch, grouping,
status, attach, detach, and routing experience. Codex must own the actual
conversation, turn execution, approvals, tool calls, transcript, and native
session rendering.

That split implies one architectural boundary:

- AgentView talks to Codex through `codex app-server`.
- AgentView does not treat `codex resume` as the main attach mechanism.
- AgentView does not parse terminal output for session state when app-server
  events are available.
- AgentView may temporarily keep `codex exec` and `codex resume` only as a
  compatibility fallback.

## Current Fallback Problems

The Rust MVP currently dispatches with `codex exec --json` and attaches with
`codex resume --include-non-interactive <thread_id>`.

That proves the job store, worktree isolation, and list UI, but it cannot reach
Agent View parity:

- Once AgentView launches `codex resume`, terminal input is owned by Codex.
  AgentView cannot capture left arrow or any other detach shortcut.
- `codex resume` is a new TUI process over a recorded thread, not the same
  hosted live view as the list controller.
- Attaching to a running non-interactive `codex exec` thread can show interrupted
  state or create confusing session behavior.
- List-level reply, approval, and request-user-input cannot be reliable without
  structured app-server request ids.

Near-term guardrail:

- Disable fallback attach while the job is `working`.
- Make fallback attach copy explicit: "opens Codex resume; exit Codex to return
  to AgentView".

## Target Runtime

```text
AgentView TUI process
  - owns the Agent View list
  - owns row selection, grouping, filtering, footer hints
  - owns dispatch input and job metadata
  - owns worktree mapping
  - starts or connects to a Codex app-server
  - switches between list view and hosted Codex session view

Codex app-server
  - owns threads
  - owns turns
  - owns approvals and user-input requests
  - owns tool execution state
  - owns transcript and event stream
  - keeps sessions alive without a terminal attached

Hosted Codex TUI view
  - reuses Codex native rendering
  - renders one selected thread
  - sends prompts and approvals to app-server
  - detaches back to AgentView without interrupting the thread
```

## Target User Flow

1. User opens `agentview`.
2. AgentView starts or connects to app-server.
3. User dispatches a task.
4. AgentView creates a worktree and starts a Codex thread through app-server.
5. AgentView subscribes to app-server notifications and updates the row.
6. User presses `Enter` on a row.
7. AgentView switches to hosted Codex TUI view in the same terminal process.
8. User presses left arrow or the configured detach key.
9. Hosted view returns `Detached`.
10. AgentView list redraws immediately. The Codex turn keeps running.

This is the behavior we want. Anything based on exiting AgentView and spawning
`codex resume` is a fallback, not parity.

## Ownership Model

AgentView-owned state:

- job id
- display title
- pinned/manual order
- group state
- dispatch cwd
- worktree path and branch
- app-server thread id
- latest normalized summary
- current pending request id and type
- archived/deleted metadata if kept for internal cleanup

Codex-owned state:

- conversation transcript
- thread lifecycle
- turn lifecycle
- tool calls
- command output
- file diffs
- approval prompts
- request-user-input prompts
- model/provider runtime state

The AgentView store should be reconstructable from app-server thread state plus
worktree metadata. It must not become a second transcript store.

## App-Server Adapter

Create `crates/agentview-codex-app-server`.

Responsibilities:

- spawn `codex app-server --listen stdio://`, `unix://`, or `ws://127.0.0.1:PORT`
- perform protocol initialization
- start a new thread
- resume/load a thread
- start a turn
- interrupt a turn
- submit user text
- submit approval decisions
- submit request-user-input answers
- subscribe to notifications
- normalize app-server events into AgentView row state

Initial transport choice:

- Prefer `stdio://` for an embedded child process in local development.
- Support `unix://` for a longer-lived per-user supervisor.
- Defer websocket unless needed for remote clients or external hosted TUI
  experiments.

Protocol source:

- For the spike, generate bindings with `codex app-server generate-json-schema`
  or `generate-ts` and hand-map only the needed subset in Rust.
- For durable integration, depend on Codex protocol crates by path from a pinned
  Codex submodule if Cargo workspace integration is practical.

## Session State Machine

AgentView row state is derived from app-server thread and turn state:

```text
new
  -> starting
  -> working
  -> needs_input
  -> idle
  -> completed
  -> failed
  -> stopped
```

Mapping rules:

- `working`: active Codex turn is running.
- `needs_input`: app-server has an unresolved approval, permission, or
  request-user-input request for the thread.
- `idle`: thread is live and ready for a prompt, but no turn is active.
- `completed`: task-level completion marker is observed or the turn completed
  successfully and the session has no pending request.
- `failed`: app-server reports a turn error or the supervisor loses a running
  session unexpectedly.
- `stopped`: user explicitly stops the thread/turn from AgentView.

Important distinction:

- `needs_input` is blocked on the user.
- `completed` is not blocked and should not render reply/approval controls as
  if the session is waiting.

## Hosted Codex TUI

Codex TUI currently has native rendering, app-server client logic, and keymaps,
but no stable public hosted-session API.

We need a small upstream patch surface, not a forked copy.

Target API shape:

```rust
pub struct HostedSessionConfig {
    pub thread_id: ThreadId,
    pub cwd: PathBuf,
    pub detach_bindings: Vec<KeyBinding>,
    pub footer_hint: String,
}

pub enum HostedSessionExit {
    Detached,
    Quit,
    Fatal(String),
}

pub async fn run_hosted_session_view<B: Backend>(
    terminal: &mut Terminal<B>,
    app_server: AppServerClient,
    config: HostedSessionConfig,
) -> Result<HostedSessionExit>;
```

Hosted mode behavior:

- Render the selected thread using Codex's native conversation cells.
- Keep Codex composer, approvals, request-user-input, command output, and diff
  rendering intact.
- Add a host-level detach key, initially left arrow when the composer is empty
  and no modal is active.
- Also support a less ambiguous fallback detach key such as `Ctrl+B`.
- Show footer hint: `← AgentView` or `Ctrl+B AgentView`.
- Detach must not call app-server shutdown.
- Detach must not call turn interrupt.
- Detach must not write `<turn_aborted>` or equivalent into the transcript.

Patch rule:

- Patch Codex to expose a hosted entrypoint and host-detach event only.
- Do not reimplement Codex rendering in AgentView.
- Keep patch queue small enough that we can rebase it on Codex updates.

## Codex Dependency Strategy

Use a pinned Codex submodule first.

```text
third_party/codex
patches/codex/*.patch
tools/update-codex.sh
tools/check-codex-patches.sh
```

Pin to the local tested version first:

```text
openai/codex tag: rust-v0.130.0
```

Rationale:

- We need source-level access to `codex-rs/tui` internals.
- Codex TUI crates are not currently exposed as a clean stable hosted widget
  dependency.
- A submodule plus patch queue keeps upstream drift visible.

Fallback if Cargo workspace integration is painful:

- Build a patched Codex binary separately for hosted mode experiments.
- Keep AgentView core app-server adapter independent.
- Move to path dependencies only after the hosted API shape is proven.

## Implementation Phases

### Phase 1: App-Server Spike

Goal: prove AgentView can create and drive a real Codex app-server thread.

Tasks:

1. [x] Add `crates/agentview-codex-app-server`.
2. [x] Start `codex app-server --listen stdio://`.
3. [x] Implement minimal JSON-RPC transport.
4. [x] Initialize the server.
5. [x] Start a thread through `thread/start`.
6. [ ] Start a thread in a temporary git worktree.
7. [ ] Start a turn and keep app-server alive until completion or interruption.
8. [ ] Consume notifications and write normalized events to the AgentView job
   log.
9. [ ] Verify the row reaches `working`, then `completed` or `needs_input`.

Current progress:

- `agentview __app-server-smoke` initializes a real local Codex app-server.
- `agentview __app-server-smoke --cwd <repo>` initializes app-server and starts
  a real Codex thread for that cwd.
- Real smoke was verified against `codex-cli 0.130.0`.
- Turn start is implemented in the adapter, but the diagnostic command does not
  expose it yet because closing the smoke client immediately after `turn/start`
  would intentionally interrupt the turn. The next slice needs a supervisor loop
  that keeps the app-server process alive and drains notifications.

Exit criteria:

- No `codex exec` is used for the spike.
- A real Codex task can complete through app-server.
- Thread id and turn state are captured from structured events.

### Phase 2: App-Server Job Model

Goal: replace fallback job dispatch with app-server-backed sessions.

Tasks:

1. Add app-server thread id to persisted jobs.
2. Replace `codex exec --json` dispatch with app-server `thread/start` and
   `turn/start`.
3. Derive row status from app-server notifications.
4. Implement list-level reply as app-server user prompt submission.
5. Implement list-level approval and request-user-input handling.
6. Keep old `codex exec` path behind `--fallback-exec` or a feature flag only.

Exit criteria:

- `agentview run` creates app-server sessions.
- `agentview` rows update while the user remains in the list.
- Reply and approval do not require entering the session view.

### Phase 3: Supervisor

Goal: keep sessions alive without an AgentView TUI attached.

Tasks:

1. Add per-user supervisor process.
2. Supervisor owns the app-server child process or connection.
3. AgentView TUI connects to supervisor.
4. Supervisor persists job/thread mapping.
5. Supervisor recovers cleanly after AgentView exits.

Exit criteria:

- Closing AgentView does not stop running sessions.
- Reopening AgentView shows current app-server-backed jobs.

### Phase 4: Hosted Codex TUI Patch

Goal: switch into a native Codex session view and detach back.

Tasks:

1. Add Codex submodule pinned to `rust-v0.130.0`.
2. Identify the minimum Codex TUI public API patch.
3. Add hosted view entrypoint.
4. Add host detach event and footer hint.
5. Wire AgentView `Enter` to hosted view instead of `codex resume`.
6. Add detach key handling with left arrow when safe.
7. Add `Ctrl+B` or equivalent fallback detach key.

Exit criteria:

- Entering a row feels like native Codex TUI.
- Left arrow or fallback detach key returns to AgentView.
- Active turn continues after detach.
- Re-entering shows the same live thread.

### Phase 5: Parity Cleanup

Goal: close gaps against `docs/codex-agent-view-spec.md`.

Tasks:

1. Ready-for-review group.
2. Pull request status resolution.
3. Filtering and grouping parity.
4. `respawn --all`.
5. Stop/remove semantics matching the spec.
6. Bring existing interactive Codex session into AgentView if app-server exposes
   enough thread listing metadata.

Exit criteria:

- Spec acceptance criteria pass.
- Fallback `codex resume` is no longer part of the normal user flow.

## Testing Strategy

Required tests before replacing fallback dispatch:

- Unit tests for state mapping from app-server notifications.
- JSON-RPC transport tests with fixture messages.
- Fake app-server integration tests for pending approvals and user input.
- Real app-server E2E in a temporary git repo.
- PTY test for AgentView list navigation.
- Hosted-view detach test after the Codex patch exists.
- Regression test that detach does not interrupt a running turn.

Required real E2E:

1. Start AgentView app-server session.
2. Dispatch task.
3. Observe working row.
4. Enter hosted Codex view.
5. Detach during active turn.
6. Observe row still working.
7. Re-enter hosted view.
8. Complete task.
9. Verify worktree changes.
10. Remove job with dirty-worktree protection.

## Immediate Next Step

Do Phase 1 only.

Do not patch Codex TUI before proving app-server control from AgentView. The
hosted TUI patch is important, but the first hard boundary is structured
session ownership through app-server.

After Phase 1 succeeds, do Phase 4 as a focused spike before spending time on
parity polish.
