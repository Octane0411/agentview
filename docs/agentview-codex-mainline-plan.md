# AgentView Codex Mainline Plan

Status: active execution plan
Date: 2026-05-14
Observed Codex CLI: `codex-cli 0.130.0`
Observed Codex source tag: `rust-v0.130.0`

This plan replaces the earlier app-server spike plan for implementation
decisions. The product goal is not to wrap `codex resume`; it is to make
AgentView the session controller and reuse Codex source for the native session
view.

## Non-Negotiable Mainline

The normal user-visible flow must become:

```text
agentview TUI
  -> AgentView runtime / supervisor
  -> Codex app-server
  -> thread/start
  -> turn/start
  -> app-server notifications
  -> AgentView list rows

Enter selected row
  -> hosted Codex session view
  -> same app-server thread
  -> Left Arrow returns HostedSessionExit::Detached
  -> AgentView list redraws
```

The normal user-visible flow must not be:

```text
agentview TUI
  -> codex exec --json

Enter selected row
  -> spawn codex resume
```

`codex exec` and `codex resume` may remain only as explicit fallback/debug
paths. They are not acceptable for Claude Agent View parity because AgentView
cannot own detach keys once a child `codex resume` process owns the terminal.

## Current State

Already implemented:

- Rust workspace for AgentView.
- Local job store and list TUI.
- Fallback dispatch through `codex exec --json`.
- Fallback attach through `codex resume`, now guarded for active jobs.
- `crates/agentview-codex-app-server`, a minimal stdio JSONL app-server client.
- Hidden smoke command: `agentview __app-server-smoke`.

Still missing from the normal path:

- `agentview run` and TUI dispatch do not use app-server yet.
- AgentView does not keep a long-lived app-server runtime alive yet.
- `Enter` still uses fallback attach for completed jobs.
- No hosted Codex TUI entrypoint is wired into AgentView yet.

## How AgentView Wraps Codex Source

AgentView stays the top-level product and process boundary. Codex source is
vendored only to reuse the app-server protocol/client and native TUI rendering.

Repository layout:

```text
agentview/
  crates/
    agentview-core/
    agentview-tui/
    agentview-cli/
    agentview-codex-app-server/
    agentview-codex-runtime/        # planned: supervisor-facing runtime API
    agentview-codex-hosted/         # planned: thin wrapper over patched Codex TUI
  third_party/
    codex/                          # git submodule: openai/codex
  patches/
    codex/
      0001-expose-hosted-session-view.patch
      0002-add-host-detach-event.patch
  tools/
    update-codex.sh
    check-codex-patches.sh
```

Pinned source:

```text
third_party/codex -> openai/codex tag rust-v0.130.0
commit: 58573da43ab697e8b79f152c53df4b42230395a8
```

Codex crates we expect to consume by path:

```toml
codex-app-server-protocol = { path = "third_party/codex/codex-rs/app-server-protocol" }
codex-app-server-client = { path = "third_party/codex/codex-rs/app-server-client" }
codex-tui = { path = "third_party/codex/codex-rs/tui" }
```

The exact dependency set may grow because `codex-tui` has internal workspace
dependencies. That is acceptable as long as Codex remains isolated behind
AgentView adapter crates.

AgentView must not import Codex internals directly from general app code.
Only these bridge crates may touch Codex crates:

- `agentview-codex-runtime`
- `agentview-codex-hosted`
- `agentview-codex-app-server` while the bridge is still being migrated

## Verified Codex Source Seams

These seams were verified against `third_party/codex` at `rust-v0.130.0`.
They define the smallest place where AgentView should attach to Codex source.

- `codex-rs/tui/src/lib.rs` exposes `run_main(...)`.
  This already supports a remote app-server descriptor and a direct
  `resume_session_id`, so hosted mode does not need Codex's resume picker.
- `codex-rs/tui/src/resume_picker.rs` defines `SessionSelection::Resume` and
  `SessionTarget { thread_id, path }`.
  Hosted mode should construct this selection directly for the selected
  AgentView job.
- `codex-rs/tui/src/app.rs` defines `App::run(...)`, `AppExitInfo`, and
  `ExitReason`.
  This is the boundary that needs a hosted run option and a detached exit
  reason.
- `codex-rs/tui/src/app/input.rs` handles top-level key events before they are
  forwarded to `ChatWidget`.
  This is where hosted mode should intercept Left Arrow before normal composer
  cursor movement.
- `codex-rs/tui/src/chatwidget.rs` already exposes checks for an empty
  composer and inactive modal/popup state.
  Hosted detach should only fire when those checks say the key is safe to
  steal.
- `codex-rs/tui/src/app_server_session.rs` owns shutdown of the app-server
  client.
  Hosted detach must not call the shutdown path for an AgentView-owned
  app-server connection.

The important consequence: AgentView does not need to rewrite or mirror the
Codex TUI. It needs a narrow hosted-session extension point in Codex TUI.

## Preferred Integration Shape

Preferred shape: library-hosted Codex TUI in the AgentView process.

```text
agentview-tui
  -> agentview-codex-hosted
    -> codex_tui::hosted::run_hosted_session_view(...)
      -> codex-app-server-client
      -> AgentView supervisor app-server connection
```

Why this is preferred:

- AgentView can keep the outer event loop and terminal lifecycle coherent.
- Hosted view can return `HostedSessionExit::Detached`.
- Left Arrow can be handled by the hosted view and returned to AgentView.
- No `codex resume` child process is involved.

Required Codex source patch surface:

1. Export a hosted-session module from `codex-tui`.
2. Accept an app-server connection descriptor first; later accept an existing
   app-server client if the client type can be shared cleanly.
3. Open a specific thread id by constructing `SessionSelection::Resume`
   directly, without showing Codex's resume picker.
4. Reuse Codex transcript cells, composer, approvals, diffs, command output, and
   request-user-input UI unchanged.
5. Add host-owned detach binding support.
6. Return `HostedSessionExit::Detached` without interrupting the active turn and
   without shutting down the AgentView-owned app-server.
7. Render a host footer hint such as `Left Arrow AgentView`.

Concrete patch behavior:

- Add a hosted config/run mode to Codex TUI.
- Add a detached exit reason or hosted-only exit result.
- In hosted mode, intercept Left Arrow only when:
  - no overlay is open;
  - no modal or popup is active;
  - composer text is empty;
  - the key event is a press/repeat, not release.
- In hosted mode, detach exits the view loop but does not send
  `ShutdownFirst`, does not interrupt the turn, and does not close the
  app-server owned by AgentView.
- Outside hosted mode, Codex key behavior remains unchanged.

Target hosted API:

```rust
pub struct HostedSessionConfig {
    pub thread_id: String,
    pub cwd: PathBuf,
    pub detach_keys: Vec<HostDetachKey>,
    pub footer_hint: String,
}

pub enum HostedSessionExit {
    Detached,
    Quit,
    Fatal(String),
}

pub async fn run_hosted_session_view(
    terminal: &mut ratatui::Terminal<impl ratatui::backend::Backend>,
    app_server: HostedAppServerConnection,
    config: HostedSessionConfig,
) -> anyhow::Result<HostedSessionExit>;
```

Codex patch rules:

- Patch only library boundary and detach behavior.
- Do not fork Codex rendering into AgentView.
- Do not patch model/tool/session semantics.
- Do not add AgentView-specific product behavior inside Codex beyond generic
  host/detach extension points.
- Keep every patch rebased and stored under `patches/codex`.

## Fallback Integration Shape

If `codex-tui` cannot be cleanly linked into the AgentView workspace quickly,
use a patched hosted helper binary as an intermediate step.

```text
agentview TUI
  -> spawn agentview-codex-hosted helper
    -> patched Codex hosted view
    -> same AgentView supervisor app-server thread
    -> Left Arrow exits helper with Detached
  -> agentview TUI redraws list
```

This is not the same as `codex resume`:

- The helper is built from patched Codex source specifically for hosted mode.
- The helper connects to the AgentView-managed app-server/thread.
- Left Arrow is handled by the helper as detach and exits cleanly.
- Detach does not interrupt the turn.

This fallback is acceptable for MVP if library linking is too slow, because it
still gives the user the Agent View behavior. It should be replaced by the
library-hosted path once the Codex TUI API is stable enough.

## Runtime Ownership

AgentView owns:

- job id and display title
- grouping, pinning, archiving, filtering
- worktree creation and cleanup policy
- app-server thread id to job id mapping
- latest row summary and pending request metadata
- terminal transitions between list and hosted view

Codex owns:

- transcript persistence
- thread and turn execution
- tool calls and command output
- approvals and request-user-input protocol
- model/provider state
- native session rendering

AgentView must not duplicate Codex transcript state. It stores only enough
metadata to reconstruct the Agent View list.

## Target Dispatch Chain

New `agentview run` / TUI submit path:

```text
create job metadata
create git worktree
supervisor.ensure_app_server()
thread/start { cwd: worktree, model, approvalPolicy, sandbox/permissions }
persist codexThreadId
turn/start { threadId, input: initial prompt }
event loop maps notifications to job rows
```

State mapping:

- `turn/started` -> `working`
- `serverRequest` approval/user input -> `needs_input`
- `serverRequest/resolved` -> previous running/idle state
- `item/agentMessage/delta` and `item/completed` -> row summary/log
- `turn/completed` success -> `completed` or `idle` depending task semantics
- `turn/completed` failed/error -> `failed`
- explicit AgentView stop -> `turn/interrupt` then `stopped`

## Target Attach Chain

New `Enter` path:

```text
selected job
require codexThreadId
supervisor connection remains alive
agentview_tui switches from list to hosted view
agentview-codex-hosted calls patched codex_tui hosted API
hosted view renders the selected thread
Left Arrow when composer is empty returns Detached
agentview_tui redraws the list
```

No `codex resume` is used here. If the job was created by the old fallback path,
AgentView may show an explicit fallback action, but it must not present it as
the normal attach experience.

## Implementation Phases

### Phase 0: Freeze The Contract

Status: current planning step.

Tasks:

1. Keep this document as the active plan.
2. Keep `codex exec/resume` labeled as fallback only.
3. Do not spend more time making fallback attach feel like parity.

Exit criteria:

- The team agrees the normal path is app-server plus hosted Codex source.

### Phase 1: Vendor Codex Source

Status: implemented for the initial pin.

Tasks:

1. [x] Add `third_party/codex` as a git submodule pinned to `rust-v0.130.0`.
2. [x] Add `patches/codex`.
3. [x] Add `tools/update-codex.sh` to move the submodule, reapply patches, and run
   focused checks.
4. [x] Add `tools/check-codex-patches.sh` to verify patches apply cleanly.
5. [x] Document the pinned Codex commit in this file.

Exit criteria:

- Fresh clone plus submodule init can build the current AgentView workspace.
- Patch check passes with no local manual steps.

### Phase 2: Typed Runtime Bridge

Status: in progress.

Tasks:

1. [x] Add `crates/agentview-codex-runtime`.
2. Replace hand-mapped app-server JSON where practical with
   `codex-app-server-protocol` types.
3. Decide runtime mode:
   - in-process app-server through `codex-app-server-client`, preferred if clean;
   - stdio app-server child as fallback, retaining current adapter.
4. Add supervisor-facing API:
   - `start_job`
   - `start_turn`
   - `interrupt_turn`
   - `reply`
   - `approve`
   - `answer_user_input`
   - `subscribe_events`
5. Map structured events into AgentView job state.

Exit criteria:

- `agentview run` can create an app-server-backed job without `codex exec`.
- The row reaches `working`, then `completed`, `failed`, or `needs_input` from
  structured events.

Current checkpoint:

- Hidden `agentview run --app-server ...` creates a job through
  `thread/start` and `turn/start` instead of `codex exec`.
- `crates/agentview-codex-runtime` owns app-server process startup,
  `thread/start`, `turn/start`, and runtime event delivery.
- The worker maps app-server notifications into AgentView job metadata and row
  state.
- The default user path still uses fallback `codex exec` until the supervisor
  and hosted attach path are ready.

### Phase 3: Supervisor

Tasks:

1. Add hidden supervisor command or daemon mode.
2. Supervisor owns the app-server runtime and job/thread mapping.
3. TUI and CLI talk to supervisor over local IPC.
4. Closing AgentView list does not kill running Codex turns.
5. Reopening AgentView reconstructs rows from store plus app-server thread state.

Exit criteria:

- Running turn survives list detach/reopen.
- No running session depends on a visible terminal.

### Phase 4: Hosted Codex Source Spike

Status: in progress.

Tasks:

1. [x] Create the Codex patch in `patches/codex` instead of carrying untracked
   submodule edits.
2. [x] Patch `codex-tui` to expose `run_hosted_session_view`.
3. [x] Build `agentview-codex-hosted` as a bridge crate.
4. If direct library hosting is blocked by terminal ownership or workspace
   dependency shape, build a temporary hosted helper binary from the same patch.
5. Open an app-server-created thread by id.
6. Render Codex native conversation UI.
7. Capture Left Arrow as detach when safe.
8. Return to AgentView list without interrupting the turn.

Current checkpoint:

- `patches/codex/0001-expose-hosted-session-view.patch` adds hosted detach
  support to `codex-tui`.
- `tools/check-codex-patches.sh` verifies that the patch applies cleanly to the
  pinned Codex submodule.
- `crates/agentview-codex-hosted` owns the AgentView-side hosted session
  contract and temporary helper invocation shape.
- Full `cargo test -p codex-tui hosted_detach --lib` still needs a successful
  Codex dependency fetch; the first attempt was blocked in dependency download
  by external network failures.

Exit criteria:

- `Enter` on an app-server-backed job opens native Codex UI.
- Left Arrow returns to AgentView.
- The active Codex turn continues after detach.
- Re-entering shows the same live thread.

### Phase 5: Cut Over Normal UX

Tasks:

1. Make app-server-backed dispatch the default for `agentview run` and TUI input.
2. Make hosted view the default `Enter` behavior.
3. Hide fallback `codex exec/resume` behind an explicit flag or debug command.
4. Remove misleading copy that suggests fallback attach has Agent View parity.
5. Update tests to fail if normal path invokes `codex exec` or `codex resume`.

Exit criteria:

- Normal usage never shells out to `codex exec` or `codex resume`.
- The user can dispatch, monitor, enter, detach, re-enter, reply, and stop from
  AgentView.

### Phase 6: Parity And Hardening

Tasks:

1. Worktree cleanup and dirty-worktree protection.
2. `needs_input` list-level reply/approval.
3. Completed/failed/ready-for-review grouping.
4. PR status extraction.
5. PTY and real Codex E2E tests.
6. Codex update workflow.

Exit criteria:

- `docs/codex-agent-view-spec.md` acceptance criteria pass.
- Codex submodule can be updated with a documented patch rebase flow.

## Testing Requirements

Unit tests:

- app-server event to AgentView state mapping
- pending request lifecycle
- fallback attach guard
- worktree cleanup policy

Fake app-server integration tests:

- thread start
- turn completion
- approval request and resolution
- request-user-input answer

Real Codex E2E:

1. Start AgentView.
2. Dispatch app-server-backed job.
3. Observe `working`.
4. Enter hosted Codex view.
5. Detach with Left Arrow during active turn.
6. Confirm list still shows the turn running.
7. Re-enter the same thread.
8. Complete the task.
9. Verify file edits in the worktree.
10. Remove job with dirty-worktree protection.

Regression tests:

- `Enter` on normal app-server job does not execute `codex resume`.
- Detach does not emit interruption/aborted transcript markers.
- Closing hosted view does not shut down app-server.

## Immediate Next Step

Phase 1 is done. The next implementation step is the Phase 4 spike:

1. Add a patch file under `patches/codex` that introduces hosted detach support
   in `codex-tui`.
2. Add `crates/agentview-codex-hosted` as the only AgentView crate allowed to
   call the hosted Codex TUI API.
3. Wire a hidden AgentView command that opens one app-server thread id through
   hosted Codex TUI.
4. Prove that Left Arrow returns to AgentView without a `conversation
   interrupted` marker.
5. After that works, move dispatch from `codex exec --json` to
   app-server-backed runtime.
