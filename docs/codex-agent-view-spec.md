# Codex Agent View Parity Specification

Status: Draft parity contract
Date: 2026-05-13
Primary goal: expose exactly the same user-visible feature set as Claude Code
Agent View, adapted to Codex sessions.

Reference behavior:
- Claude Code Agent View documentation:
  https://code.claude.com/docs/en/agent-view
- Reference date checked: 2026-05-13
- Claude reference version floor in the documentation: Claude Code v2.1.139
  or later.
- Local Codex CLI version observed during planning: `codex-cli 0.130.0`

This document is the product contract. If an item below cannot be implemented
with the current Codex integration, the implementation is incomplete. Do not
replace it with a smaller MVP behavior and still call it parity.

## 1. Parity Rule

AgentView must match Claude Code Agent View exactly at the user-visible
behavior level:

- Same capabilities.
- Same state model.
- Same grouping, ordering, filtering, and keyboard behavior.
- Same dispatch, background, attach, detach, stop, remove, respawn, worktree,
  model, permission, supervisor, storage, disable, troubleshooting, and
  limitation semantics.

AgentView may use Codex-specific internal APIs and storage, but provider
differences must stay behind adapters and must not add or remove user-visible
features.

The following are not part of the parity surface and must not be added as
user-visible features unless Claude Agent View adds an equivalent:

- Archive or unarchive flows.
- Text search outside the documented filter syntax.
- Model filters such as `m:<model>`.
- Codex profile prefixes such as `profile:<name>`.
- Shell commands named `peek`, `list`, `interrupt`, `doctor`, `repair-index`,
  or `cleanup-worktrees`.
- Web UI, cloud hosting, multi-user collaboration, or agent-to-agent messaging.
- A different safe-delete mode that changes the documented remove behavior.

## 2. Product Goal

AgentView is a local terminal UI for dispatching and managing many background
Codex sessions from one screen.

The core loop is:

1. Open AgentView.
2. Dispatch a new background session from the input.
3. Monitor every background session grouped by state.
4. Peek at recent output, blocking questions, and pull requests.
5. Reply without leaving AgentView.
6. Attach to the full conversation.
7. Detach back to AgentView without stopping the session.
8. Bring an existing interactive session into AgentView.
9. Keep parallel file edits isolated with git worktrees when possible.

Every background session is a full Codex conversation owned by Codex. It keeps
running without a terminal attached when the local supervisor and machine are
running.

## 3. Command Mapping

AgentView uses its own command names, but each command must map one-to-one to
the Claude Agent View behavior.

| Claude command | AgentView command | Purpose |
| --- | --- | --- |
| `claude agents` | `agentview` | Open the full-screen AgentView table. |
| `claude --bg "<prompt>"` | `agentview run "<prompt>"` | Start a session directly in the background. |
| `claude attach <id>` | `agentview attach <id>` | Attach to a background session in this terminal. |
| `claude logs <id>` | `agentview logs <id>` | Print recent session output. |
| `claude stop <id>` | `agentview stop <id>` | Stop a session. |
| `claude kill <id>` | `agentview kill <id>` | Alias for stop. |
| `claude respawn <id>` | `agentview respawn <id>` | Restart a stopped session with conversation intact. |
| `claude respawn --all` | `agentview respawn --all` | Restart every stopped session. |
| `claude rm <id>` | `agentview rm <id>` | Remove a session from the list. |

No additional shell management commands are in scope.

## 4. Opening AgentView

Command:

```bash
agentview
```

Behavior:

- Opens a full-screen terminal table with an input at the bottom.
- Starts the per-user supervisor if it is not already running.
- Shows every background session the user has started, across all projects.
- Sessions from other repositories and worktrees appear regardless of the
  directory where `agentview` was opened.
- Interactive sessions open in other terminals do not appear until they are
  backgrounded.
- Sessions spawned by a session as subagents, teammates, tools, workflows, or
  equivalent Codex-internal workers are not listed as separate rows.
- `Esc` exits the UI and returns to the shell. Sessions keep running and
  reappear the next time AgentView opens.

The header shows:

- AgentView version.
- Default dispatch model.
- Current dispatch directory.
- Summary counts.

The main table shows grouped sessions. The footer shows keyboard hints.

Example shape:

```text
AgentView v0.1.0    model: gpt-5.2-codex    cwd: ~/repo    2 working, 1 needs input

Pinned
  * auth refactor          Edit src/auth/session.ts                     3m

Ready for review
  . checkout validation    github.com/acme/app/pull/248            o    2h

Needs input
  * db migration           needs input: apply generated migration?       1m

Working
  * flaky settings test    Run npm test -- SettingsChangeDetector        7m

Completed
  . docs update            result: updated docs/api.md                   18m
  x dead-code cleanup      failed: test command exited 1                 24m
  ... 6 more

> investigate the flaky billing test

Enter attach/dispatch  Space peek  Ctrl+S group  Ctrl+T pin  Ctrl+X stop  ? help
```

## 5. Session State

Each row starts with an icon. Icon color and animation show session state.
Icon shape shows process state.

Session states:

| State | Meaning |
| --- | --- |
| Working | Codex is actively running tools or generating a response. |
| Needs input | Codex is waiting on a specific question or permission decision from the user. |
| Idle | The session has nothing to do and is ready for the next prompt. |
| Completed | The task finished successfully. |
| Failed | The task ended with an error. |
| Stopped | The session was stopped with `Ctrl+X`, `agentview stop`, or `agentview kill`. |

Process shapes:

| Shape | Meaning |
| --- | --- |
| Alive | The session process is alive and replies immediately. |
| Exited | The process has exited. The user can still peek, reply, or attach, and AgentView restarts it from where it left off. |
| Sleeping | A loop session is sleeping between iterations. The row shows run count and countdown. |

Background sessions do not need any terminal open to keep working. They are
hosted by the supervisor. If the machine sleeps or shuts down, running sessions
stop and appear failed when the machine wakes; the user can attach, peek, reply,
or run `agentview respawn --all` to restart them from where they left off.

## 6. Row Summaries

Each row displays:

- State/process icon.
- Session name.
- One-line generated summary of current activity, need, or result.
- Pull request status indicator when applicable.
- Last changed time.

The one-line summary must be generated by a small summarization model, not only
by raw event text. While a session is actively working, the summary refreshes at
most once every 15 seconds, plus once when each turn ends. Summary calls use the
same account/provider policy as the session itself.

## 7. Pull Request Status

When a session opens a pull request, a status dot appears at the right edge of
the row and links to the pull request in terminals that support hyperlinks.

When a session opens more than one pull request, the count appears before the
dot, and the dot color reflects whichever pull request most needs attention.

| Dot color | Pull request status |
| --- | --- |
| Yellow | Waiting on checks or review, or checks failed. |
| Green | Checks passed and no review is blocking. |
| Purple | Merged. |
| Grey | Draft or closed. |

Unknown status is shown as grey only when the pull request is known to be draft
or closed. If status cannot be resolved, the implementation must keep resolving
until it can show the documented status or clearly mark the session state as
unable to determine PR status; it must not invent a fifth dot color.

## 8. Peek and Reply

`Space` on a selected row opens or closes the peek panel.

The peek panel shows:

- What the session needs from the user, if anything.
- The session's most recent output.
- Pull requests opened by the session.

Reply behavior:

- Typing a reply in the peek panel and pressing `Enter` sends it to the selected
  session without leaving AgentView.
- Multiple-choice questions show their options; pressing the matching number key
  chooses an option.
- For other blocked sessions, `Tab` fills the input with a suggested reply that
  the user can edit before sending.
- Prefixing a reply with `!` sends a Bash command to the session.
- `Up` and `Down` move the peek panel to adjacent sessions without closing it.
- `Right Arrow` attaches to the peeked session.

## 9. Attach and Detach

`Enter` or `Right Arrow` on a selected row attaches to the full interactive
Codex conversation.

Required behavior:

- The attached view replaces AgentView in the terminal.
- The attached view behaves exactly as if the user had started Codex directly in
  that directory.
- The user sees the full conversation, not only a log tail.
- The user can type follow-up prompts.
- The same Codex conversation/thread is used.
- On attach, Codex posts a short recap of what happened while the user was away.
- `Left Arrow` on an empty prompt detaches and returns to AgentView.
- `Ctrl+Z` detaches immediately if a dialog has focus or `Left Arrow` does not
  respond.
- Detaching never stops a background session.
- `Left Arrow`, `Ctrl+C`, `Ctrl+D`, `Ctrl+Z`, and `/exit` all leave the
  background session running.
- To end the session from inside it, the user runs `/stop`.

After a session has been dispatched or backgrounded, `Left Arrow` on an empty
prompt works from any Codex session, not only sessions attached from AgentView.
It backgrounds the current session and opens AgentView with that session
pre-selected. This shortcut can be disabled in configuration.

## 10. Organizing the List

Default grouping:

1. Pinned
2. Ready for review
3. Needs input
4. Working
5. Completed

Group names do not map one-to-one to the state model:

- A session moves to `Ready for review` when it has an open pull request.
- `Completed` contains completed, failed, and stopped sessions.

`Ctrl+S` switches grouping between state and directory. The choice persists
across runs.

Within a group:

| Shortcut | Action |
| --- | --- |
| `Ctrl+T` | Pin or unpin the selected session. |
| `Shift+Up` | Move the selected session up. |
| `Shift+Down` | Move the selected session down. |
| `Ctrl+R` | Rename the selected session. |
| `Enter` on group header | Collapse or expand the group. |

Older completed sessions fold into a `... N more` row to keep the list short.
Failures and sessions with an open pull request always stay visible.

## 11. Stop and Remove

`Ctrl+X` behavior:

1. First press stops the selected session.
2. Pressing `Ctrl+X` again within two seconds removes the session.

`Ctrl+X` on a group header removes every session in that group after
confirmation.

Remove behavior from the UI:

- Removes the session from AgentView.
- Stops any live process owned by the supervisor.
- Removes the worktree created for that session, including uncommitted changes.
- Keeps the Codex transcript on disk and resumable through Codex-owned resume
  mechanisms.

The UI must make the destructive worktree consequence clear before deleting, but
it must not introduce a different default behavior from Claude Agent View.

Shell remove behavior:

```bash
agentview rm <id>
```

The shell remove command removes the session from the list and cleans up its
worktree if there are no uncommitted changes.

## 12. Filtering

Typing in the dispatch input filters instead of dispatching only when the input
matches documented filter syntax.

| Filter | Shows |
| --- | --- |
| `a:<name>` | Sessions running the named agent. |
| `s:<state>` | Sessions in the given state, such as `s:working`. |
| `s:blocked` | Sessions waiting on the user. |
| `#<number>` | The session working on that pull request. |
| Pull request URL | The session working on that pull request. |

No other filters are in scope.

## 13. Keyboard Shortcuts

`?` shows every shortcut in context.

| Shortcut | Action |
| --- | --- |
| `Up` / `Down` | Move between rows. |
| `Enter` | Attach to the selected session, or dispatch if there is text in the input. |
| `Space` | Open or close the peek panel for the selected session. |
| `Shift+Enter` | Dispatch and attach immediately. |
| `Right Arrow` | Attach to the selected session. |
| `Alt+1`..`Alt+9` | Attach to sessions 1 through 9 in the current group. |
| `Tab` on empty input | Browse all agents. |
| `Tab` on non-empty input | Apply the highlighted suggestion. |
| `Ctrl+S` | Switch grouping between state and directory. |
| `Ctrl+T` | Pin or unpin the selected session. |
| `Ctrl+R` | Rename the selected session. |
| `Ctrl+G` | Open the dispatch prompt in `$EDITOR`. |
| `Ctrl+X` | Stop the session; press again within two seconds to remove it. |
| `Shift+Up` / `Shift+Down` | Reorder the selected session. |
| `Esc` | Close the peek panel, clear the input, or exit. |
| `Ctrl+C` | Clear the input; press twice to exit. |
| `?` | Show shortcut help. |

## 14. Dispatching Sessions

### 14.1 From AgentView

Typing a task prompt in the bottom input and pressing `Enter` starts a new
background session.

Every prompt entered in the dispatch input starts its own new session. It does
not send a follow-up to the selected session. Follow-up replies are sent from
the peek panel or attached conversation.

The new session:

- Uses the default dispatch model shown in the header.
- Uses the same permission mode that Codex would use if started in that
  directory.
- Is named automatically from the prompt.
- Can be renamed later with `Ctrl+R`.

Prompt shorter than four characters is rejected with a `Too short` hint.

Dispatch control syntax:

| Input | Effect |
| --- | --- |
| `<agent-name> <prompt>` | If the first word matches a configured agent, that agent runs as the session's main agent with its configuration. |
| `@<agent-name>` | Mention a configured agent anywhere in the prompt to run it as the main agent. |
| `@<repo>` | Mention a repository under the directory where AgentView opened to run the session there. |
| `/<skill>` | Suggest skills to dispatch as the prompt. |
| `#<number>` or pull request URL | Select an existing session for that pull request if one exists; otherwise dispatch with pull request context. |
| `Shift+Enter` | Dispatch and immediately attach to the new session. |

Pasting an image into the prompt includes the image with the task.

If the same `@name` matches both an agent and a sibling repository, the agent
takes precedence. A bare first-word agent match also takes precedence over
treating that word as plain prompt text.

### 14.2 Dispatch to a Specific Directory

A new session runs in the directory where AgentView was opened unless a
directory target is selected.

To target a different directory:

- Open `agentview` in that directory.
- Open `agentview` in a parent directory that holds several repositories and
  mention one with `@<repo>` in the prompt.
- From the shell, `cd` into the directory and run `agentview run "<prompt>"`.

When AgentView is grouped by directory, the highlighted row's directory becomes
the dispatch target, so the user can scroll to a group and dispatch there
without retyping the path.

### 14.3 From Inside a Session

Inside an interactive Codex session, `/background` and `/bg` move the current
conversation into a background session.

The user can pass one more instruction:

```text
/bg run the test suite and fix any failures
```

Backgrounding from an interactive session starts a fresh background process that
resumes from the saved conversation. Running subagents, background commands, or
equivalent in-flight work do not transfer to it. Codex asks the user to confirm
before backgrounding when in-flight work is running.

Once in the background, the session can start new subagents, tools, background
commands, or equivalent Codex work; those keep running across later detach and
reattach.

### 14.4 From the Shell

Command:

```bash
agentview run "investigate the flaky SettingsChangeDetector test"
```

To run a specific agent as the session's main agent:

```bash
agentview run --agent code-reviewer "address review comments on PR 1234"
```

After backgrounding, AgentView prints the short id and management commands:

```text
backgrounded - av_7c5dcf5d
  agentview                 list sessions
  agentview attach av_7c5dcf5d    open in this terminal
  agentview logs av_7c5dcf5d      show recent output
  agentview stop av_7c5dcf5d      stop this session
```

## 15. Worktree Isolation

Every background session, whether started from AgentView, `/bg`, or
`agentview run`, starts in the user's working directory. Before editing files,
AgentView moves the session into an isolated git worktree under:

```text
<repo>/.agentview/worktrees/<id>/
```

This allows parallel sessions to read the same checkout while writing to
separate worktrees.

AgentView skips worktree creation when:

- The session is already under `.agentview/worktrees/`.
- The working directory is not a git repository.
- The write is outside the working directory.

Outside a git repository, sessions write directly to the working directory and
are not isolated from each other. AgentView must warn the user not to dispatch
parallel sessions that edit the same files.

The worktree is removed when the user removes the session according to the
remove behavior in this spec. To find a session's worktree path, peek the
session or attach and check its working directory.

To make an agent always run in its own worktree regardless of how it was
started, set `isolation: worktree` in that agent's configuration.

## 16. Model and Permission Settings

The model name shown in the AgentView header is the dispatch default. New
sessions started from the input use this model.

Each background session can run on a different model. To override a session's
model:

- From the shell, pass `--model` with `agentview run`.
- Attach to a running session and change the model there. The change persists
  if the session is respawned.
- Dispatch an agent whose configuration sets a model.

A dispatched session reads settings and permission mode from the directory it
runs in, the same as if the user had started Codex there directly.

Dispatching from the AgentView input does not pass a permission mode. The
session uses the directory default or the dispatched agent's configured
permission mode.

To set permission mode from the shell, pass `--permission-mode` with
`agentview run`.

Dangerous automatic permission modes must be refused until the user has accepted
that mode once interactively, because background sessions can act without being
watched.

## 17. Shell Management

Every background session has a short id. The id is printed when a shell command
starts a background session and is used by the management commands below.

| Command | Purpose |
| --- | --- |
| `agentview` | Open AgentView. |
| `agentview attach <id>` | Attach to a session in this terminal. |
| `agentview logs <id>` | Print recent output. |
| `agentview stop <id>` | Stop a session. |
| `agentview kill <id>` | Alias for stop. |
| `agentview respawn <id>` | Restart a stopped session with conversation intact. |
| `agentview respawn --all` | Restart every stopped session. |
| `agentview rm <id>` | Remove a session from the list and clean its worktree if there are no uncommitted changes. |

## 18. Hosting and Supervisor

Every session listed in AgentView is considered a background session, whether or
not the user is currently attached to it.

By contrast, a session started by running Codex directly is tied to that
terminal and ends when it closes, unless the user sends it to the background.

Background sessions are hosted by a per-user supervisor process, separate from
the terminal and from the AgentView UI.

Supervisor behavior:

- Starts automatically the first time the user backgrounds a session or opens
  AgentView.
- Is not managed directly by the user.
- Authenticates with the same credentials as interactive Codex sessions.
- Makes no additional network connections beyond the model/provider API.
- Runs each background session as its own Codex process managed by the
  supervisor.
- Keeps a session process running while it is working, waiting for input, or has
  a terminal attached.
- Stops a finished and unattached session process after about one hour to free
  resources.
- Leaves transcript and state on disk when a process is stopped.
- Starts a fresh process from saved state when the user later attaches, peeks,
  or replies.
- Exits when every session has finished and no terminal is connected.
- Starts again the next time a user needs it.
- Watches the installed Codex binary on disk and restarts into the new version
  after the normal updater replaces it.
- Reconnects to detached background session processes after supervisor restart.

## 19. State Ownership and Storage

Codex owns conversation state:

- Conversation/thread id.
- Turns.
- Transcript.
- Context.
- Resume semantics.

AgentView owns only Agent View metadata:

- Background session id.
- Codex conversation/thread id.
- Display name.
- Pinned state.
- Manual order.
- Grouping preference.
- Worktree path.
- Repo root and current directory.
- Dispatch prompt.
- Process id when supervised.
- Normalized state and process shape.
- Generated row summary.
- Last update timestamp.
- Pull request references.
- Stop and remove state.

Default config directory:

```text
~/.agentview/
```

If `AGENTVIEW_CONFIG_DIR` is set, the supervisor uses that directory instead of
`~/.agentview` and runs as a separate instance with its own sessions.

Required files:

| Path | Contents |
| --- | --- |
| `~/.agentview/daemon.log` | Supervisor log. |
| `~/.agentview/daemon/roster.json` | List of running background sessions, used to reconnect after restart. |
| `~/.agentview/jobs/<id>/state.json` | Per-session state shown in AgentView. |

Codex transcripts remain under Codex-owned storage.

## 20. Disable AgentView

Users can turn off background agents and AgentView entirely with either:

- `disableAgentView` setting.
- `AGENTVIEW_DISABLE_AGENT_VIEW` environment variable.

Administrators can enforce the same through managed settings if AgentView gains
managed settings support. Until that exists, there is no admin-specific disable
surface.

## 21. Troubleshooting Requirements

The product must expose user-visible behavior for the documented cases below.

### AgentView opens with no sessions

AgentView is empty until the user dispatches the first session. The UI tells the
user to type a task prompt in the bottom input and press `Enter`.

### Cannot open AgentView because background tasks are running

If `Left Arrow` tries to background the current session while in-flight work is
running, the shortcut must not silently abandon that work. The UI shows the
number of running background tasks, tells the user how to inspect them, and
requires `/bg` to confirm backgrounding.

### Prompt rejected as too short

The dispatch input expects a task description. Prompts shorter than four
characters are rejected with a `Too short` hint.

### Sessions show as failed after waking the machine

Background sessions do not survive sleep or shutdown. Running sessions show as
failed after wake. Attach, peek, reply, or `agentview respawn --all` restarts
them from where they left off.

### Session is slow to respond after attaching

Once a session has finished and sat unattached for about an hour, the supervisor
may stop its process to free resources. Attaching starts a fresh process from
where it left off. Sessions that are working or waiting on the user are never
stopped this way.

### Worktrees are filling up

Worktrees are removed when the session that created them is removed. If a
session ended without cleanup, the user can inspect leftovers with
`git worktree list` in the project directory and remove each with
`git worktree remove <path>`.

## 22. Limitations

AgentView has the same limitations as Claude Agent View:

- Rate limits and usage apply independently to each background session. Running
  many sessions in parallel consumes quota roughly proportionally.
- Sessions are local. Background sessions run on the user's machine and stop if
  it sleeps or shuts down.
- Worktrees are removed with the session. Users must merge or push changes they
  want to keep before removing a session that edited files in its own worktree.

## 23. Codex Integration Boundary

The Codex adapter must provide enough structured behavior to implement this
parity contract. Required capabilities include:

- Start a background conversation in a directory.
- Resume an existing conversation by id.
- Read recent output and full transcript.
- Stream assistant output and tool activity.
- Detect working, needs-input, idle, completed, failed, stopped, exited, alive,
  and sleeping states.
- Route user replies from the peek panel.
- Route multiple-choice selections and suggested replies.
- Route permission decisions.
- Interrupt and stop a running turn without deleting the session.
- Attach to a full interactive session.
- Detach from that interactive session without stopping it.
- Background an existing interactive session.
- Execute `!` Bash replies under the session's permission model.
- Detect and report pull request references and status.
- Preserve Codex-owned transcript and resume state outside AgentView metadata.

`codex app-server` or another structured Codex API is preferred. A raw
`codex exec --json` plus `codex resume` fallback is acceptable only for internal
development while it preserves this user-visible behavior. It must not be used
to claim parity if attach/detach, backgrounding, replies, approvals, summaries,
or state transitions differ from this spec.
