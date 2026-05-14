# agentview

`agentview` is a local Agent View-style controller for Codex sessions.

The current implementation is a Codex-only MVP. The normal path is:

- AgentView starts `codex app-server` under a local supervisor.
- AgentView stores local job metadata under `~/.agentview`.
- Codex owns thread, turn, transcript, tool, and approval state.
- Entering a row opens a hosted Codex session view built from the pinned Codex
  source under `third_party/codex`.
- `codex exec` / `codex resume` remain fallback/debug paths only.

The product spec lives in `docs/codex-agent-view-spec.md`. The current
execution plan lives in `docs/agentview-codex-mainline-plan.md`.

The implementation is Rust-first and uses Serde schemas for persisted job/store
boundaries.

## Usage

Run the TUI:

```bash
agentview
```

Dispatch a job:

```bash
agentview run "investigate the flaky checkout test"
agentview run --cwd ~/repo --model gpt-5.2-codex "fix auth validation"
```

Inspect and manage jobs:

```bash
agentview list
agentview peek <job_id>
agentview logs <job_id>
agentview attach <job_id>
agentview reply <job_id> "continue and run tests"
agentview approve <job_id>
agentview decline <job_id>
agentview stop <job_id>
agentview respawn <job_id>
agentview respawn --all
agentview rm <job_id>
```

Check dependencies:

```bash
agentview doctor
```

## TUI Shortcuts

- `Up` / `Down`: select a session
- `Enter`: attach selected session, collapse/expand a group header, or
  dispatch/send typed input
- `Space`: peek selected session; when peek is open, typed input replies to that session
- `Shift+Up` / `Shift+Down`: reorder selected session within its group
- `Ctrl+X`: stop selected session; press again within two seconds to delete
- `Ctrl+R`: rename selected session
- `Ctrl+T`: pin/unpin
- `Ctrl+S`: switch grouping between state and directory; the choice persists
- `Ctrl+C`: clear input or panels; press twice to exit
- `?`: help
- `Esc`: close panels, cancel rename, clear input, or exit

## Local State

Agent View metadata is stored under:

```text
~/.agentview/
```

Codex owns the conversation transcript and resume state. Agent View stores only
job metadata, normalized event logs, worktree mappings, and local UI
preferences.

## Current Limitations

- The attached session UI currently uses a patched helper process,
  `agentview-codex-hosted`, built from `third_party/codex` plus
  `patches/codex`.
- Direct library-hosted Codex TUI is still the target shape.
- The fallback `codex exec` backend cannot receive live replies while running.
- PR status extraction and final Claude Agent View grouping parity are not
  complete yet.

## Development

Build development binaries:

```bash
tools/build-dev.sh
```

This builds:

```text
target/debug/agentview
target/debug/agentview-codex-hosted
```

Run tests:

```bash
cargo test --workspace
```

Run without installing globally:

```bash
cargo run -p agentview-cli -- help
```

Build only the hosted helper from the pinned Codex source:

```bash
tools/build-codex-hosted-helper.sh
```

Install locally:

```bash
cargo install --path crates/agentview-cli --force
```

## Codex Source Updates

Codex is pinned as a submodule:

```text
third_party/codex
```

AgentView changes to Codex live in:

```text
patches/codex/*.patch
```

Do not edit the submodule in place for normal AgentView work. To verify the
patch queue:

```bash
tools/check-codex-patches.sh
```

To update the Codex pin and rebuild the hosted helper:

```bash
tools/update-codex.sh rust-v0.130.0
```

Real-Codex PTY verification:

```bash
tools/e2e-hosted-detach.sh
```

The integration tests use fake `codex` executables to verify dispatch, list,
peek, attach, needs-input reply/approval, stop, and conservative worktree
deletion. Run real-Codex E2E before relying on a release build.
