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

## Concrete Chain To Build

The earlier plans were directionally correct, but the attach transport was not
explicit enough. The chain we need to build is:

```text
AgentView list TUI
  -> agentview-core
  -> AgentView supervisor IPC
  -> supervisor-owned Codex app-server endpoint
  -> Codex thread/turn state

Enter selected row
  -> AgentView resolves job id to Codex thread id
  -> AgentView launches or enters hosted Codex TUI
  -> hosted Codex TUI connects to the same app-server endpoint
  -> hosted Codex TUI opens the selected thread id
  -> Left Arrow returns Detached to AgentView
  -> AgentView redraws the list
```

The important correction is the app-server endpoint. A private `stdio://`
app-server child is enough for background dispatch, but it is not enough for an
out-of-process hosted helper because the helper cannot share the supervisor's
private stdin/stdout pipes. Hosted attach therefore needs one of these two
transport shapes:

1. Preferred long-term: library-hosted Codex TUI in the AgentView process,
   sharing an app-server client handle owned by the host.
2. MVP-compatible: supervisor starts Codex app-server on a connectable local
   endpoint and passes that endpoint to the hosted helper.

For the current Codex pin, the practical MVP endpoint is loopback websocket:

```text
codex app-server --listen ws://127.0.0.1:<reserved-port>
```

Codex app-server also supports `unix://`, but Codex TUI's remote app-server
client at `rust-v0.130.0` is websocket-shaped. Until upstream exposes a Unix
socket remote client or a clean in-process hosted handle, AgentView should use a
loopback websocket endpoint for hosted attach and keep it local-only.

This is the link to change. The target is not to make `codex resume` easier to
exit from; the target is to remove `codex resume` from the normal attach path.

## Current State

Already implemented:

- Rust workspace for AgentView.
- Local job store and list TUI.
- Fallback dispatch through `codex exec --json`.
- Fallback attach through `codex resume`, now guarded for active jobs.
- `crates/agentview-codex-app-server`, a minimal stdio JSONL app-server client.
- Hidden smoke command: `agentview __app-server-smoke`.
- Default app-server dispatch through supervisor for `agentview run` and TUI
  submit.
- Hidden fallback exec path: `agentview run --fallback-exec ...`.
- `crates/agentview-codex-runtime`, the first supervisor-facing runtime bridge.
- Hidden supervisor IPC with app-server run/reply/stop routing.
- Codex source submodule pinned to `rust-v0.130.0`.
- Hosted Codex TUI patch stored under `patches/codex`.
- `crates/agentview-codex-hosted`, the AgentView-side hosted-session contract
  and helper invocation shape.
- App-server-backed attach routes through the hosted helper contract.
- Supervisor app-server dispatch now uses a local websocket endpoint by
  default so a hosted helper can reconnect to the same running thread.
- Patched Codex helper build script produces `target/debug/agentview-codex-hosted`.

Still missing from the normal path:

- Live reply to a running app-server turn is not wired yet.
- Helper packaging/version-update workflow is still manual.
- Full list TUI PTY automation still needs hardening. The hosted attach path is
  covered by a real PTY E2E, and the list Enter path has been manually verified,
  but the expect harness for exiting the outer list TUI is not yet robust enough
  to make it the primary regression script.
- Direct library-hosted Codex TUI remains the preferred long-term shape; the
  current MVP uses the helper-process bridge.

## How AgentView Wraps Codex Source

AgentView stays the top-level product, process boundary, and user-visible
controller. Codex source is vendored only to reuse two things:

1. Codex's structured session runtime boundary: app-server protocol, thread
   lifecycle, turn lifecycle, approvals, and request-user-input events.
2. Codex's native interactive session UI: transcript cells, composer, diffs,
   approvals, command output, and key handling.

AgentView must not become a forked Codex CLI. The integration is a shell around
Codex's supported or minimally patched library seams:

```text
AgentView CLI/TUI
  -> agentview-core
    -> job store, worktrees, grouping, row state
  -> agentview-codex-runtime
    -> Codex app-server protocol
    -> thread/start, thread/resume, turn/start, notifications
  -> agentview-codex-hosted
    -> patched codex_tui hosted entrypoint
    -> native Codex session view for one thread
```

The job identity is AgentView-owned. The conversation identity is Codex-owned:

```text
AgentView job id <-> Codex app-server thread id <-> Codex transcript/session
```

AgentView stores enough metadata to show and reopen the list. It does not copy
or reinterpret the full Codex transcript.

Repository layout:

```text
agentview/
  crates/
    agentview-core/
    agentview-tui/
    agentview-cli/
    agentview-codex-app-server/
    agentview-codex-runtime/        # supervisor-facing Codex runtime API
    agentview-codex-hosted/         # thin wrapper over patched Codex TUI
  third_party/
    codex/                          # git submodule: openai/codex
  patches/
    codex/
      0001-expose-hosted-session-view.patch
      0002-add-agentview-hosted-helper-bin.patch
  tools/
    update-codex.sh
    check-codex-patches.sh
    build-codex-hosted-helper.sh
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

Put differently, AgentView "wraps" Codex source by pinning upstream Codex as a
submodule, applying a small patch queue that exposes hosted-session seams, and
calling those seams only from AgentView adapter crates. AgentView does not copy
Codex TUI files into its own TUI, and it does not become a long-lived fork of
Codex behavior.

Current MVP source wrapping chain:

```text
third_party/codex pinned to rust-v0.130.0
  -> patches/codex/*.patch expose hosted Codex TUI seams
  -> tools/build-codex-hosted-helper.sh applies patches to target/agentview-codex-patched
  -> cargo builds codex-tui --bin agentview-codex-hosted
  -> helper is copied to target/debug/agentview-codex-hosted
  -> AgentView attach resolves helper from AGENTVIEW_CODEX_HOSTED, then sibling binary, then PATH
  -> helper calls codex_tui::hosted::run_hosted_session_view(...)
  -> Codex TUI renders the selected AgentView job's Codex thread
```

This lets AgentView track upstream Codex by rebasing a small patch queue instead
of copying the Codex TUI implementation.

### Source Integration Contract

There are three allowed ways for AgentView to use Codex source:

1. Runtime bridge.
   `agentview-codex-runtime` owns the app-server client lifecycle exposed to
   AgentView. Today it can spawn `codex app-server --listen stdio://`; the
   target shape is to replace hand-written JSON with Codex protocol/client
   crates where the dependency graph is stable enough.
2. Hosted TUI bridge.
   `agentview-codex-hosted` is the only crate allowed to open the native Codex
   session view. It passes an app-server connection descriptor and a selected
   thread id into a patched hosted entrypoint in `codex-tui`.
3. Patch management.
   Any Codex source change must live as a small patch under `patches/codex`.
   The patch must expose generic host/detach behavior, not AgentView product
   logic. `tools/check-codex-patches.sh` must apply the patches cleanly against
   the pinned submodule.

Everything else in AgentView talks to these bridge crates, not to Codex
internals.

### Attach Transport Contract

Hosted attach needs three values from AgentView:

```text
Codex app-server endpoint
Codex thread id
working directory / worktree path
```

For a library-hosted view, the endpoint may eventually be an in-process
`AppServerClient` handle. For the helper-process MVP, the endpoint must be a
connectable URL. At the current Codex pin that means `ws://127.0.0.1:<port>`.

AgentView therefore needs to persist or query this per-supervisor endpoint, not
per-job terminal state. A job row stores the selected Codex thread id and
worktree path; the supervisor owns the app-server endpoint and lifetime.

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

If direct library hosting is not ready, the helper-process MVP must still use
the same app-server thread through a connectable endpoint. It may be a separate
process for terminal isolation, but it is not allowed to create a fresh resume
session.

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

For this helper path, the concrete invocation shape should be:

```text
agentview-codex-hosted
  --app-server-url ws://127.0.0.1:<port>
  --thread-id <codex-thread-id>
  --cwd <job-worktree>
```

The hosted helper is built from the patched Codex source. It opens the thread
through Codex's remote app-server client and exits with a distinct detached
status when Left Arrow is pressed in a safe state.

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
ensure connectable app-server endpoint for hosted attach
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
query supervisor for app-server endpoint
supervisor app-server remains alive
agentview_tui switches from list to hosted view
agentview-codex-hosted calls patched codex_tui hosted API with endpoint + thread id
hosted view renders the selected thread
Left Arrow when composer is empty returns Detached
agentview_tui redraws the list
```

No `codex resume` is used here. If the job was created by the old fallback path,
AgentView may show an explicit fallback action, but it must not present it as
the normal attach experience.

## Implementation Phases

### Phase 0: Freeze The Contract

Status: active architecture contract.

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

- `agentview run` creates an app-server-backed job through supervisor
  `thread/start` and `turn/start` instead of `codex exec`.
- `crates/agentview-codex-runtime` owns app-server process startup,
  `thread/start`, `thread/resume`, `turn/start`, and runtime event delivery.
- `crates/agentview-codex-runtime` now exposes an incremental
  `CodexRuntimeSession` API so the upcoming supervisor can hold an app-server
  session instead of only calling blocking one-shot helpers.
- App-server-backed jobs persist the active Codex turn id while a turn is
  running, preparing the stop path for `turn/interrupt`.
- `agentview reply` / `agentview respawn` on app-server-backed jobs now send
  the follow-up turn through `thread/resume` plus `turn/start`, instead of
  falling back to `codex exec resume`.
- The worker maps app-server notifications into AgentView job metadata and row
  state.
- Fake app-server integration tests cover app-server dispatch and follow-up
  replies, including guards against fallback `codex exec` / `codex resume`
  output on that path.
- The fallback `codex exec` path is now behind hidden `--fallback-exec`.

### Phase 3: Supervisor

Status: started.

Tasks:

1. [x] Add hidden supervisor command or daemon mode.
2. Supervisor owns the app-server runtime and job/thread mapping.
3. TUI and CLI talk to supervisor over local IPC.
4. Closing AgentView list does not kill running Codex turns.
5. Reopening AgentView reconstructs rows from store plus app-server thread state.
6. [x] Replace private stdio-only supervisor app-server with a connectable loopback
   websocket endpoint for hosted attach, or expose an equivalent brokered
   app-server connection.
7. [x] Reserve the loopback port before spawning Codex app-server and record the
   resulting `ws://127.0.0.1:<port>` endpoint in supervisor state.

Exit criteria:

- Running turn survives list detach/reopen.
- No running session depends on a visible terminal.
- Hosted attach can connect to the same app-server thread without using
  `codex resume`.

Current checkpoint:

- Hidden `agentview __supervisor` starts a local Unix socket supervisor process.
- Hidden `agentview __supervisor-ping` verifies the local IPC path.
- Hidden `agentview __supervisor-shutdown` stops the local supervisor.
- App-server-backed `agentview run`, `agentview reply`, and `agentview respawn`
  now submit execution requests through supervisor IPC instead of spawning an
  AgentView worker process.
- The supervisor starts the Codex app-server turn on a background thread and
  keeps the app-server child under the supervisor process while the turn runs.
- By default, the supervisor now reserves a loopback port and starts Codex with
  `codex app-server --listen ws://127.0.0.1:<port>`. The previous stdio
  transport remains available for tests through `AGENTVIEW_APP_SERVER_TRANSPORT=stdio`.
- The supervisor keeps an addressable running-session map for active app-server
  turns, including the command channel and app-server websocket URL.
- The supervisor process now detaches into its own session and writes stderr /
  IPC failures to `supervisor.log`, so closing an attach PTY or one AgentView
  command does not tear down active Codex turns.
- `agentview attach` on a running app-server-backed job queries the supervisor
  endpoint and passes `--app-server-url ws://127.0.0.1:<port>` to the hosted
  helper contract instead of falling back to `codex resume`.
- `agentview stop` on a running app-server-backed job routes through supervisor
  IPC and sends Codex `turn/interrupt` instead of killing the supervisor
  process.
- Live reply to an already running app-server turn is still pending.

### Phase 4: Hosted Codex Source Spike

Status: in progress.

Tasks:

1. [x] Create the Codex patch in `patches/codex` instead of carrying untracked
   submodule edits.
2. [x] Patch `codex-tui` to expose `run_hosted_session_view`.
3. [x] Build `agentview-codex-hosted` as a bridge crate.
4. [x] If direct library hosting is blocked by terminal ownership or workspace
   dependency shape, build a temporary hosted helper binary from the same patch.
5. [x] Add a hidden AgentView command that opens an app-server-created thread
   by id through the hosted helper contract.
6. [x] Pass `--app-server-url`, `--thread-id`, and `--cwd` into the hosted
   helper contract.
7. [x] Verify Codex native conversation UI renders through the helper in a PTY.
8. [x] Capture Left Arrow as detach when safe.
9. [x] Return to AgentView list without interrupting the turn in a real PTY E2E.

Current checkpoint:

- `patches/codex/0001-expose-hosted-session-view.patch` adds hosted detach
  support to `codex-tui`.
- `patches/codex/0002-add-agentview-hosted-helper-bin.patch` adds the
  patched Codex-side `agentview-codex-hosted` helper binary.
- `tools/check-codex-patches.sh` verifies that the patch applies cleanly to the
  pinned Codex submodule.
- `tools/build-codex-hosted-helper.sh` builds the helper from a patched Codex
  source copy under `target/agentview-codex-patched` and installs the binary at
  `target/debug/agentview-codex-hosted`.
- `crates/agentview-codex-hosted` owns the AgentView-side hosted session
  contract and temporary helper invocation shape.
- `HostedHelper::from_env_or_default()` resolves the helper from
  `AGENTVIEW_CODEX_HOSTED`, then from a sibling `agentview-codex-hosted` binary
  next to the current `agentview` executable, then from `PATH`.
- Hidden `agentview __hosted-attach <job_id>` resolves an app-server-backed
  job to its Codex thread id and invokes the hosted helper with `--thread-id`
  and `--cwd`. When the job is actively running under the supervisor websocket
  transport, it also invokes the helper with `--app-server-url`.
- Public `agentview attach <job_id>` and TUI Enter now route app-server-backed
  jobs to the hosted helper contract instead of `codex resume`.
- `tools/build-codex-hosted-helper.sh` was verified on 2026-05-14 and produced
  `target/debug/agentview-codex-hosted`.
- `target/debug/agentview-codex-hosted --help` was verified on 2026-05-14 and
  exposes `--thread-id`, `--cwd`, `--app-server-url`,
  `--app-server-auth-token`, and `--no-alt-screen`.
- Focused patched Codex test was verified on 2026-05-14:
  `cargo test --manifest-path target/agentview-codex-patched/codex/codex-rs/Cargo.toml -p codex-tui hosted_detach --lib`.
- `tools/e2e-hosted-detach.sh` now runs a real Codex PTY regression:
  dispatch a websocket app-server job, open the same thread through
  `agentview __hosted-attach --no-alt-screen`, detach with Left Arrow, verify
  the same turn is still running, re-enter and detach again, reject
  `conversation interrupted` / `hosted_attach_quit` markers, then wait for the
  marker response. Verified on 2026-05-14 with job `av_mp59vsfi_177l`, thread
  `019e25c4-8f80-7920-98db-613f709f0256`, turn
  `019e25c4-8fc5-73a3-ba06-a71c2e2c3014`, marker
  `AGENTVIEW_HOSTED_DETACH_E2E_1778750099_OK`.
- The full list TUI path was manually verified on 2026-05-14 with job
  `av_mp59omyw_16cy`: pressing Enter from `agentview` opened the hosted Codex
  UI, Left Arrow emitted `hosted_attach_detached`, the same turn continued, and
  the marker `AGENTVIEW_LIST_TUI_E2E_1778749765_OK` completed. The automated
  expect harness for cleanly exiting the outer list TUI after detach is still
  pending, so the committed repeatable script currently covers the hidden
  hosted attach command rather than the full-screen list wrapper.

Exit criteria:

- `Enter` on an app-server-backed job opens native Codex UI.
- Left Arrow returns to AgentView.
- The active Codex turn continues after detach.
- Re-entering shows the same live thread.

### Phase 5: Cut Over Normal UX

Tasks:

1. [x] Make app-server-backed dispatch the default for `agentview run` and TUI
   input.
2. [x] Make hosted view the default `Enter` behavior for app-server-backed jobs.
3. [x] Hide fallback `codex exec/resume` behind an explicit debug flag.
4. [x] Remove misleading copy that suggests fallback attach has Agent View
   parity.
5. [x] Update tests to fail if normal path invokes `codex exec` or
   `codex resume`.

Current checkpoint:

- `DispatchOptions::default()` now selects the app-server backend.
- CLI `agentview run` defaults to supervisor/app-server dispatch.
- TUI submit uses `DispatchOptions::default()`, so it also defaults to
  supervisor/app-server dispatch.
- Hidden `agentview run --fallback-exec ...` keeps the old `codex exec --json`
  path available for regression coverage and emergency debugging.
- The normal app-server integration test dispatches without `--app-server` and
  asserts fallback `codex exec` / `codex resume` output is absent.
- CLI command descriptions say `attach` opens a conversation; fallback
  `codex resume` copy is kept only for fallback implementation internals and
  tests.
- Real Codex app-server dispatch smoke was verified on 2026-05-14 with
  `codex-cli 0.130.0`: default `agentview run` created a supervisor/app-server
  job, reached `completed`, and `peek` returned `AGENTVIEW_REAL_E2E_OK`. Logs
  showed `thread/start`, `turn/start`, agent message deltas, and
  `turn/completed`. A non-fatal `codex_apps` MCP startup notification was
  observed.
- Real Codex websocket dispatch smoke was verified on 2026-05-14 with
  `codex-cli 0.130.0`: default `agentview run` created a supervisor websocket
  app-server job, recorded `appServerUrl: ws://127.0.0.1:<port>`, reached
  `completed`, and `peek` returned `AGENTVIEW_WS_E2E_OK`.

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

Current repeatable real Codex E2E:

```text
tools/e2e-hosted-detach.sh
```

This script consumes real Codex tokens. It requires `codex`, `expect`, a built
`target/debug/agentview`, and a built
`target/debug/agentview-codex-hosted`. It validates the hosted session core
with a PTY, but it does not yet drive the outer list TUI as the default
regression path.

Regression tests:

- `Enter` on normal app-server job does not execute `codex resume`.
- Detach does not emit interruption/aborted transcript markers.
- Closing hosted view does not shut down app-server.

## Immediate Next Step

The next implementation work should follow this order:

1. Harden the full list TUI PTY regression.
   - Drive `agentview` with a selected running row.
   - Press Enter to open the hosted Codex view.
   - Detach with Left Arrow and assert the outer list redraws.
   - Exit the outer list cleanly in the harness without relying on PTY close.
2. Wire live reply/approval for already running app-server turns.
   - `needs_input` rows should expose list-level reply/approval where possible.
   - Entering the hosted Codex view remains the canonical full-session approval
     path.
3. Formalize helper packaging and Codex update flow.
   - Keep `tools/check-codex-patches.sh` and `tools/build-codex-hosted-helper.sh`
     as the required checks when bumping `third_party/codex`.
   - Document the tested Codex CLI/source version after every bump.
4. Then fill the remaining parity gaps.
   - Live reply/approval while a turn is in `needs_input`.
   - Dirty worktree cleanup protection.
   - Completed/failed grouping and PR status extraction.
