# agentview

`agentview` is a local Agent View-style controller for Codex sessions.

The current implementation is a Codex-only MVP. It uses the Codex CLI fallback path from the spec:

- `codex exec --json` for background turns
- `codex resume <thread_id>` for full interactive attach
- one local job store under `~/.agentview`
- one git worktree per dispatched job when the target directory is inside a git repo

The full product spec lives in `docs/codex-agent-view-spec.md`.

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
agentview rm <job_id>
```

Check dependencies:

```bash
agentview doctor
```

## TUI Shortcuts

- `Up` / `Down`: select a session
- `Enter`: attach selected session, or dispatch/send typed input
- `Space`: peek selected session; when peek is open, typed input replies to that session
- `Ctrl+X`: stop selected session; press again within two seconds to delete
- `Ctrl+T`: pin/unpin
- `Ctrl+S`: switch grouping between state and directory
- `?`: help
- `Esc`: close panels or exit

## Local State

Agent View metadata is stored under:

```text
~/.agentview/
```

Codex owns the conversation transcript and resume state. Agent View stores only job metadata, normalized event logs, and worktree mappings.

## Current Limitations

- This MVP does not use `codex app-server` yet.
- Attach is implemented by suspending Agent View and running `codex resume <thread_id>`.
- Approval handling is detected from JSON events, but full in-panel approval routing requires the app-server backend.
- The supervisor is implemented as detached per-job worker processes rather than a persistent daemon.

## Development

Run tests:

```bash
npm test
```

Run without installing globally:

```bash
node ./bin/agentview.js help
```
