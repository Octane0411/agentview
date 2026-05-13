# Codex Agent View MVP Specification

Status: Draft
Date: 2026-05-13
Primary goal: Match the core user experience of Claude Code Agent View for Codex sessions.

Reference behavior:
- Claude Code Agent View: https://code.claude.com/docs/en/agent-view
- Local Codex CLI version observed during planning: `codex-cli 0.130.0`

## 1. Product Goal

Codex Agent View is a local terminal workspace for dispatching, monitoring, entering, replying to, and stopping multiple Codex coding sessions from one screen.

The MVP must preserve the main Claude Agent View loop:

1. Open a full-screen session list.
2. Dispatch a new background agent from the input.
3. See each session grouped by state.
4. Peek at recent output or a blocking question.
5. Reply without leaving the list.
6. Attach to the full conversation.
7. Detach back to the list without stopping the session.
8. Keep file edits isolated across parallel sessions.

## 2. Non-Goals

The MVP does not need to:

- Run Claude Code sessions.
- Register Codex sessions inside `claude agents`.
- Provide cloud-hosted execution.
- Support multi-user collaboration.
- Implement agent-to-agent messaging.
- Replace the Codex CLI or Codex app-server protocol.
- Provide a polished web UI.

## 3. Target User Experience

Command:

```bash
agentview
```

This opens a full-screen terminal UI with:

- Header: app version, default model, current dispatch directory, summary counts.
- Grouped session list.
- Peek panel, opened on demand.
- Dispatch input at the bottom.
- Footer with keyboard hints.

Example:

```text
Codex Agent View v0.1.0    model: gpt-5.2-codex    cwd: ~/repo    2 working, 1 needs input

Pinned
  * auth refactor          Edit src/auth/session.ts                     3m

Ready for review
  . checkout validation    github.com/acme/app/pull/248          yellow  2h

Needs input
  * db migration           needs input: apply generated migration?       1m

Working
  * flaky settings test    Run npm test -- SettingsChangeDetector        7m

Completed
  . docs update            result: updated docs/api.md                   18m
  x dead-code cleanup      failed: test command exited 1                 24m

> investigate the flaky billing test

Enter attach/dispatch  Space peek  Ctrl+S group  Ctrl+T pin  Ctrl+X stop  ? help
```

## 4. Architecture

### 4.1 Components

```text
agentview                 Terminal UI process
agentview-daemon          Per-user supervisor process
job-store                 Local SQLite state
worktree-manager          Creates and cleans per-job git worktrees
codex-adapter             Talks to Codex app-server or Codex CLI
event-normalizer          Converts Codex events into Agent View states
pty-bridge                Attaches full interactive sessions to terminal
```

### 4.2 Recommended Codex Integration

Use `codex app-server` as the primary backend for parity.

Required protocol capabilities:

- `thread/start`
- `thread/resume`
- `thread/list`
- `thread/read`
- `turn/start`
- `turn/steer`
- `turn/interrupt`
- `thread/status/changed`
- `turn/started`
- `turn/completed`
- `turn/diff/updated`
- `item/agentMessage/delta`
- `item/commandExecution/requestApproval`
- `item/fileChange/requestApproval`
- `item/tool/requestUserInput`

Fallback integration:

- `codex exec --json`
- `codex resume <session_id>`
- `codex exec resume <session_id> --json`

The fallback is acceptable for an internal prototype, but it cannot fully match live attach/detach behavior unless wrapped by a PTY session manager.

## 5. State Ownership

Codex owns conversation state:

- Thread id
- Turns
- Transcript
- Context
- Resume semantics

Agent View owns job state:

- Job id
- Codex thread id
- Display name
- Pinned state
- Manual order
- Grouping preference
- Worktree path
- Repo root
- Dispatch prompt
- Process id, if the job is running under the supervisor
- Normalized status
- Last activity summary
- Last update timestamp
- PR metadata
- Stop/delete/archive state

### 5.1 Job Record

```ts
type JobStatus =
  | "working"
  | "needs_input"
  | "idle"
  | "completed"
  | "failed"
  | "stopped";

type ProcessState =
  | "alive"
  | "exited"
  | "sleeping"
  | "unknown";

type Job = {
  id: string;
  provider: "codex";
  codexThreadId: string | null;
  title: string;
  initialPrompt: string;
  repoRoot: string;
  cwd: string;
  worktreePath: string | null;
  model: string | null;
  approvalPolicy: "untrusted" | "on-request" | "never";
  sandbox: "read-only" | "workspace-write" | "danger-full-access";
  status: JobStatus;
  processState: ProcessState;
  pinned: boolean;
  manualOrder: number | null;
  archived: boolean;
  lastSummary: string | null;
  lastOutput: string | null;
  blockingRequest: BlockingRequest | null;
  prRefs: PullRequestRef[];
  createdAt: string;
  updatedAt: string;
  completedAt: string | null;
};
```

## 6. Session States

Agent View must show the following normalized states.

| State | Meaning |
| --- | --- |
| Working | Codex is generating, running tools, applying patches, or executing commands. |
| Needs input | Codex is waiting for user input, command approval, file-change approval, or permission approval. |
| Idle | The thread is loaded and ready for the next prompt. |
| Completed | The last requested task finished successfully. |
| Failed | The session ended with an error. |
| Stopped | The user stopped the job. |

Process state must be displayed separately:

| Process state | Meaning |
| --- | --- |
| Alive | A Codex process or app-server thread is active and can respond immediately. |
| Exited | No live process is attached, but the thread can be resumed. |
| Sleeping | Reserved for future scheduled or loop jobs. |
| Unknown | State could not be verified. |

## 7. Row Summaries

Each row displays:

- State/process icon.
- Session title.
- One-line current activity, need, or result.
- PR status dot when applicable.
- Last changed time.

Summary source priority:

1. Explicit blocking request text.
2. Current command or tool event.
3. Last assistant message delta.
4. Last completed result.
5. Generated summary from a small model, optional after MVP.

MVP should not require an extra model call for summaries. It can derive summaries from event text.

## 8. Pull Request Status

If Codex opens or references a pull request, the row displays a status dot.

| Dot | Meaning |
| --- | --- |
| Yellow | Checks pending, checks failed, or review is blocking. |
| Green | Checks passed and no blocking review is known. |
| Purple | Merged. |
| Grey | Draft, closed, or status unknown. |

MVP requirement:

- Detect PR URLs from assistant output.
- Store PR refs on the job.
- Link to the PR in terminals that support hyperlinks.

Post-MVP:

- Use GitHub API or `gh` to resolve check/review/merge state.

## 9. Opening Agent View

Command:

```bash
agentview
```

Behavior:

- Starts `agentview-daemon` if not already running.
- Connects to existing daemon.
- Loads all non-archived jobs from local store.
- Shows jobs from all projects by default.
- Dispatch target defaults to the current directory.
- `Esc` exits the UI while jobs keep running.

## 10. Dispatching Sessions

### 10.1 From Agent View

Typing a prompt in the bottom input and pressing `Enter` starts a new Codex session.

Default behavior:

1. Create job record.
2. Create isolated worktree if the dispatch directory is inside a git repository.
3. Start Codex thread in the job worktree.
4. Start a turn with the input prompt.
5. Stream events into the job row and peek buffer.

Input prefixes:

| Input | Effect |
| --- | --- |
| `@<repo> <prompt>` | Dispatch into a sibling repo under the Agent View root. |
| `/skill <prompt>` | Attach a Codex skill reference when supported. |
| `#<number>` or PR URL | Select existing job for that PR if found, otherwise dispatch with PR context. |
| `model:<name> <prompt>` | Override model for this job. |
| `profile:<name> <prompt>` | Use a Codex config profile. |

MVP can support only `@<repo>`, `model:<name>`, and PR URL detection.

### 10.2 Dispatch and Attach

`Shift+Enter` dispatches a new job and immediately attaches to the full conversation.

### 10.3 From Shell

Commands:

```bash
agentview run "investigate the flaky checkout test"
agentview run --model gpt-5.2-codex "fix billing validation"
agentview run --cwd ~/repo "review auth middleware"
agentview run --attach "implement the migration"
```

Output:

```text
backgrounded  av_01jz...
  agentview                 list sessions
  agentview attach av_01jz  open in this terminal
  agentview logs av_01jz    show recent output
  agentview stop av_01jz    stop this session
```

## 11. Peek and Reply

`Space` opens the peek panel for the selected row.

Peek panel shows:

- Latest assistant output.
- Current command/tool activity.
- Blocking request, if any.
- Approval options, if any.
- PR links, if any.
- Worktree path.

Reply behavior:

- Typing a normal message and pressing `Enter` sends a new user input to the selected Codex thread.
- If the session is waiting for a specific approval, the reply resolves that approval.
- Number keys choose displayed multiple-choice options when present.
- `Tab` fills a suggested reply when available.
- Prefixing with `!` runs a shell command in the job worktree, subject to sandbox/approval policy.

MVP requirement:

- Normal reply.
- Approval accept/decline.
- Show latest output.

Post-MVP:

- Suggested replies.
- Number shortcuts.
- `!` shell command.

## 12. Attach and Detach

`Enter` or `Right Arrow` on a selected row attaches to the full Codex conversation.

Required behavior:

- The user sees the full conversation, not only a log tail.
- The user can type follow-up prompts.
- The session keeps its original Codex thread id.
- Detaching returns to Agent View.
- Detaching does not stop the session.

Implementation options:

1. Preferred: render the attached conversation through Codex app-server inside Agent View.
2. Acceptable MVP fallback: suspend Agent View and run `codex resume <thread_id>` in the terminal.
3. PTY fallback: launch `codex resume <thread_id>` under a PTY and return to Agent View when the child exits.

Parity requirement:

- If using the fallback Codex TUI, document that detach key behavior may differ from Claude Agent View.
- For full parity, Agent View must own the attached UI through app-server or a PTY bridge that can detach without killing the Codex session.

Detach shortcuts:

| Shortcut | Action |
| --- | --- |
| `Left Arrow` on empty prompt | Detach to Agent View. |
| `Ctrl+Z` | Force detach to Agent View. |
| `Ctrl+C` | Interrupt current turn, not delete the session. |
| `/stop` or `agentview stop` | Stop the job. |

## 13. Organizing the List

Default grouping:

1. Pinned
2. Ready for review
3. Needs input
4. Working
5. Completed

Alternative grouping:

- `Ctrl+S` toggles grouping by directory.
- Preference persists across runs.

Within a group:

| Shortcut | Action |
| --- | --- |
| `Ctrl+T` | Pin or unpin selected session. |
| `Shift+Up` | Move selected session up. |
| `Shift+Down` | Move selected session down. |
| `Ctrl+R` | Rename selected session. |
| `Enter` on group header | Collapse or expand group. |

Completed rows:

- Older completed rows collapse into a `... N more` row.
- Failed rows remain visible.
- Rows with open PR refs remain visible.

## 14. Stop, Delete, and Archive

`Ctrl+X` behavior:

1. First press stops the selected job.
2. Second press within two seconds deletes the job after confirmation.

Delete behavior:

- Removes job from Agent View.
- Stops any live process owned by the daemon.
- Cleans up the job worktree only if safe.
- Does not delete Codex transcript/session data.

Worktree cleanup safety:

- If uncommitted changes exist, refuse deletion unless `--force` is used.
- Show the worktree path and suggested commands.

Shell commands:

```bash
agentview stop <job_id>
agentview rm <job_id>
agentview rm --force <job_id>
agentview archive <job_id>
agentview unarchive <job_id>
```

## 15. Filtering

Typing in the dispatch input filters rows when it matches filter syntax.

| Filter | Shows |
| --- | --- |
| `s:<state>` | Sessions in a state, for example `s:working` or `s:blocked`. |
| `#<number>` | Session associated with that PR number. |
| PR URL | Session associated with that PR URL. |
| `@<repo>` | Sessions in that repo. |
| `m:<model>` | Sessions using that model. |
| `text` | Sessions whose title, prompt, summary, or cwd contains text. |

MVP requirement:

- `s:<state>`
- `#<number>`
- text search

## 16. Keyboard Shortcuts

| Shortcut | Action |
| --- | --- |
| `Up` / `Down` | Move between rows. |
| `Enter` | Attach selected session, or dispatch if input has text. |
| `Space` | Open or close peek panel. |
| `Shift+Enter` | Dispatch and attach immediately. |
| `Right Arrow` | Attach selected session. |
| `Alt+1`..`Alt+9` | Attach to session 1 through 9 in current group. |
| `Tab` | Apply highlighted suggestion or cycle completions. |
| `Ctrl+S` | Switch grouping between state and directory. |
| `Ctrl+T` | Pin or unpin selected session. |
| `Ctrl+R` | Rename selected session. |
| `Ctrl+G` | Open dispatch prompt in `$EDITOR`. |
| `Ctrl+X` | Stop session, then delete on second press. |
| `Shift+Up` / `Shift+Down` | Reorder selected session. |
| `Esc` | Close peek panel, clear input, or exit. |
| `Ctrl+C` | Clear input, interrupt attached turn, or exit on second press. |
| `?` | Show shortcut help. |

## 17. Worktree Isolation

Every dispatched job should run in an isolated git worktree when possible.

Default path:

```text
<repo>/.agentview/worktrees/<job_id>/
```

Behavior:

- If the dispatch directory is inside a git repo, create a worktree before Codex can edit files.
- If the dispatch directory is already under `.agentview/worktrees`, reuse it.
- If not inside a git repo, run directly in the chosen directory and show an isolation warning.
- Store worktree path on the job record.
- Remove worktree only when the job is deleted and cleanup is safe.

Branch naming:

```text
agentview/<job_id>-<slug>
```

## 18. Model and Permission Settings

Header shows the dispatch default model.

Model selection:

- Default from Codex config.
- Override globally through Agent View settings.
- Override per job using dispatch prefix or CLI flag.

CLI:

```bash
agentview --model gpt-5.2-codex
agentview run --model gpt-5.2-codex "task"
```

Permission mapping:

| Agent View setting | Codex setting |
| --- | --- |
| Approval policy | `--ask-for-approval` or app-server approval policy |
| Sandbox | `--sandbox` or app-server sandbox policy |
| Danger bypass | `--dangerously-bypass-approvals-and-sandbox` |

Danger bypass must be disabled by default and require explicit opt-in.

## 19. Shell Management Commands

Required CLI:

```bash
agentview
agentview run "task"
agentview attach <job_id>
agentview peek <job_id>
agentview logs <job_id>
agentview list
agentview stop <job_id>
agentview interrupt <job_id>
agentview respawn <job_id>
agentview rm <job_id>
agentview archive <job_id>
```

Command behavior:

| Command | Purpose |
| --- | --- |
| `agentview` | Open full-screen Agent View. |
| `agentview run` | Dispatch a background Codex job. |
| `agentview attach` | Attach to the full conversation. |
| `agentview peek` | Print latest summary and blocking state. |
| `agentview logs` | Print recent output. |
| `agentview list` | Print jobs without TUI. |
| `agentview stop` | Stop a job. |
| `agentview interrupt` | Interrupt current active turn. |
| `agentview respawn` | Restart a stopped/exited job from its Codex thread. |
| `agentview rm` | Remove job and optionally clean worktree. |
| `agentview archive` | Hide job from default list without deleting. |

## 20. Daemon Behavior

`agentview-daemon` is a per-user local supervisor.

Responsibilities:

- Own long-running Codex app-server connection.
- Start jobs.
- Track live turns.
- Normalize events.
- Route approvals and user replies.
- Track process liveness.
- Persist job metadata.
- Manage worktrees.

Lifecycle:

- Starts automatically when `agentview` or `agentview run` is called.
- Exits when no jobs are active and no UI is connected for a configurable idle timeout.
- Restarts without losing job metadata.
- Reconnects to Codex threads after restart.

## 21. Storage

Default config directory:

```text
~/.agentview/
```

Files:

| Path | Contents |
| --- | --- |
| `~/.agentview/agentview.db` | Job metadata and UI preferences. |
| `~/.agentview/daemon.log` | Supervisor logs. |
| `~/.agentview/jobs/<job_id>/events.jsonl` | Normalized event log. |
| `~/.agentview/jobs/<job_id>/last.txt` | Last rendered output snapshot. |

Codex transcripts remain under Codex-owned storage.

## 22. Error Handling

Required visible errors:

- Codex app-server not found.
- Codex login/auth failure.
- Version unsupported.
- Cannot create worktree.
- Worktree has uncommitted changes during delete.
- Codex thread cannot be resumed.
- Approval request timed out.
- Session failed after machine sleep or daemon restart.

Recovery commands:

```bash
agentview doctor
agentview respawn <job_id>
agentview repair-index
agentview cleanup-worktrees
```

## 23. MVP Scope

### Must Have

- Full-screen TUI.
- Dispatch Codex job.
- List jobs grouped by state.
- Store job metadata.
- Use per-job worktree when in git repo.
- Stream output into row and peek panel.
- Normalize working, needs input, completed, failed, stopped.
- Peek latest output.
- Reply to session.
- Attach to full conversation.
- Detach without deleting job.
- Stop job.
- Delete job with worktree safety check.
- Resume job after Agent View exits and reopens.
- Shell commands: `run`, `list`, `attach`, `logs`, `stop`, `rm`.

### Should Have

- Rename.
- Pin.
- Reorder.
- Directory grouping.
- Text and state filters.
- PR URL detection.
- Model override.
- Approval accept/decline in peek panel.

### Later

- Generated row summaries.
- GitHub check/review status.
- Images in dispatch prompt.
- Skill picker.
- `!` shell command from peek.
- Scheduled loop jobs.
- Web UI.
- Multi-provider Claude plus Codex support.

## 24. Acceptance Criteria

### Dispatch

Given a git repo, when the user runs:

```bash
agentview run "change the README title"
```

Then:

- A job appears in Agent View.
- The job has a Codex thread id.
- The job runs in a separate worktree.
- The row shows working state while Codex is active.
- Output is visible from peek.

### Peek and Reply

Given a job in needs input state, when the user presses `Space`, types a reply, and presses `Enter`, then:

- The reply is sent to the same Codex thread.
- The job leaves needs input if Codex resumes work.
- The event log records the reply.

### Attach and Detach

Given any non-deleted job, when the user presses `Enter` on its row, then:

- The full conversation opens.
- The user can send a follow-up prompt.
- The same Codex thread id is used.
- Detaching returns to the list.
- The job remains available after Agent View exits and reopens.

### Worktree Safety

Given a job worktree with uncommitted changes, when the user deletes the job, then:

- Agent View refuses to remove the worktree by default.
- The UI shows the worktree path.
- The Codex transcript remains resumable.

### Restart

Given Agent View and daemon are stopped, when the user runs `agentview`, then:

- Previous jobs are listed from local store.
- Codex threads can be resumed.
- Completed and failed states remain visible.

## 25. Known Risks

- `codex app-server` is experimental and may change. Pin Codex CLI version for MVP.
- `codex resume` fallback may not support Claude-like detach behavior.
- Worktree cleanup can lose uncommitted work if implemented carelessly.
- Running many Codex sessions in parallel consumes usage quota quickly.
- Outside git repos, edit isolation cannot be guaranteed.

## 26. Implementation Recommendation

Build in two stages.

Stage 1:

- TUI.
- SQLite job store.
- Worktree manager.
- Codex CLI fallback with `codex exec --json` and `codex resume`.
- Basic attach by suspending Agent View and launching `codex resume`.

Stage 2:

- Switch Codex backend to `codex app-server`.
- Implement native attach/detach inside Agent View.
- Implement approval routing in peek panel.
- Add event-driven status updates.

The product should not claim full Claude Agent View parity until Stage 2 is complete.
