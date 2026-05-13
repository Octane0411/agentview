import { attachCodex } from "./codex.js";
import { dispatchJob, pinJob, removeJob, renameJob, replyToJob, stopJob } from "./jobs.js";
import { listJobs, readJobLast } from "./store.js";
import { relativeTime, truncate } from "./util.js";
import type { Job, JobStatus } from "./schema.js";

type Row = { type: "header"; label: string } | { type: "job"; job: Job };
type Group = { label: string; jobs: Job[] };
type LastDelete = { jobId: string; at: number } | null;

export async function runTui() {
  if (!process.stdin.isTTY || !process.stdout.isTTY) {
    console.error("agentview TUI requires an interactive terminal. Use `agentview list` in non-TTY contexts.");
    process.exitCode = 1;
    return;
  }
  const tui = new Tui();
  await tui.start();
}

class Tui {
  jobs: Job[];
  rows: Row[];
  selected: number;
  input: string;
  message: string;
  peek: boolean;
  help: boolean;
  groupBy: "state" | "cwd";
  lastDelete: LastDelete;
  refreshTimer: NodeJS.Timeout | null;
  originalRawMode: boolean | undefined;
  suspended: boolean;

  constructor() {
    this.jobs = [];
    this.rows = [];
    this.selected = 0;
    this.input = "";
    this.message = "";
    this.peek = false;
    this.help = false;
    this.groupBy = "state";
    this.lastDelete = null;
    this.refreshTimer = null;
    this.suspended = false;
  }

  async start() {
    this.originalRawMode = process.stdin.isRaw;
    this.suspended = false;
    process.stdin.setRawMode(true);
    process.stdin.resume();
    process.stdin.setEncoding("utf8");
    process.stdout.write("\x1b[?25l");
    process.stdin.on("data", this.onData);
    process.on("SIGWINCH", this.onResize);
    this.startRefreshTimer();
    await this.refresh();
  }

  stop() {
    this.suspended = true;
    this.stopRefreshTimer();
    process.stdin.off("data", this.onData);
    process.off("SIGWINCH", this.onResize);
    process.stdin.setRawMode(Boolean(this.originalRawMode));
    process.stdin.pause();
    process.stdout.write("\x1b[?25h\x1b[0m\x1b[2J\x1b[H");
  }

  onResize = () => {
    void this.render();
  };

  onData = (chunk: Buffer | string) => {
    void this.handleKey(String(chunk));
  };

  async refresh() {
    if (this.suspended) return;
    this.jobs = await listJobs();
    this.buildRows();
    if (this.selected >= this.rows.length) this.selected = Math.max(0, this.rows.length - 1);
    await this.render();
  }

  buildRows() {
    const groups = this.groupBy === "cwd" ? groupJobsByCwd(this.jobs) : groupJobsByState(this.jobs);
    const rows: Row[] = [];
    for (const group of groups) {
      if (!group.jobs.length) continue;
      rows.push({ type: "header", label: group.label });
      for (const job of group.jobs) rows.push({ type: "job", job });
    }
    this.rows = rows;
    if (this.rows[this.selected]?.type === "header") {
      const next = this.rows.findIndex((row, index) => index >= this.selected && row.type === "job");
      if (next >= 0) this.selected = next;
    }
  }

  selectedJob(): Job | null {
    const row = this.rows[this.selected];
    return row?.type === "job" ? row.job : null;
  }

  async handleKey(key: string): Promise<void> {
    if (key === "\u0003") {
      if (this.input) {
        this.input = "";
        this.message = "";
        return this.render();
      }
      this.stop();
      return;
    }
    if (key === "\u001b") {
      if (this.help || this.peek || this.input) {
        this.help = false;
        this.peek = false;
        this.input = "";
        return this.render();
      }
      this.stop();
      return;
    }
    if (key === "\u001b[A") return this.move(-1);
    if (key === "\u001b[B") return this.move(1);
    if (key === "\u001b[C") return this.attachSelected();
    if (key === "\u0018") return this.stopOrDelete();
    if (key === "\u0014") return this.togglePin();
    if (key === "\u0013") {
      this.groupBy = this.groupBy === "state" ? "cwd" : "state";
      return this.refresh();
    }
    if (key === "?") {
      this.help = !this.help;
      return this.render();
    }
    if (key === " ") {
      if (this.input) {
        this.input += " ";
      } else {
        this.peek = !this.peek;
      }
      return this.render();
    }
    if (key === "\r" || key === "\n") return this.submit();
    if (key === "\u007f") {
      this.input = this.input.slice(0, -1);
      return this.render();
    }
    if (key >= " " && key !== "\u007f") {
      this.input += key;
      return this.render();
    }
  }

  async move(delta: number): Promise<void> {
    if (!this.rows.length) return;
    let next = this.selected;
    do {
      next = Math.max(0, Math.min(this.rows.length - 1, next + delta));
      if (this.rows[next]?.type === "job") break;
      if (next === 0 || next === this.rows.length - 1) break;
    } while (true);
    this.selected = next;
    await this.render();
  }

  async submit() {
    const text = this.input.trim();
    const job = this.selectedJob();
    this.input = "";

    if (text.startsWith("/rename ") && job) {
      await renameJob(job.id, text.slice(8).trim());
      this.message = `renamed ${job.id}`;
      return this.refresh();
    }

    if (text) {
      if (this.peek && job) {
        try {
          await replyToJob(job.id, text);
          this.message = `reply sent to ${job.id}`;
        } catch (error) {
          this.message = error instanceof Error ? error.message : String(error);
        }
        return this.refresh();
      }
      try {
        const created = await dispatchJob(text);
        this.message = `backgrounded ${created.id}`;
      } catch (error) {
        this.message = error instanceof Error ? error.message : String(error);
      }
      return this.refresh();
    }

    return this.attachSelected();
  }

  async attachSelected() {
    const job = this.selectedJob();
    if (!job) return;
    this.suspend();
    try {
      await attachCodex(job);
    } catch (error) {
      console.error(error instanceof Error ? error.message : String(error));
      console.error("Press Enter to return to Agent View.");
      await waitForEnter();
    } finally {
      this.resume();
      await this.refresh();
    }
  }

  suspend() {
    this.suspended = true;
    this.stopRefreshTimer();
    process.stdin.off("data", this.onData);
    process.off("SIGWINCH", this.onResize);
    process.stdin.setRawMode(false);
    process.stdin.pause();
    process.stdout.write("\x1b[?25h\x1b[0m\x1b[2J\x1b[H");
  }

  resume() {
    this.suspended = false;
    process.stdin.setRawMode(true);
    process.stdin.resume();
    process.stdin.on("data", this.onData);
    process.on("SIGWINCH", this.onResize);
    this.startRefreshTimer();
    process.stdout.write("\x1b[?25l");
  }

  startRefreshTimer() {
    if (this.refreshTimer) return;
    this.refreshTimer = setInterval(() => {
      void this.refresh();
    }, 1500);
  }

  stopRefreshTimer() {
    if (!this.refreshTimer) return;
    clearInterval(this.refreshTimer);
    this.refreshTimer = null;
  }

  async stopOrDelete() {
    const job = this.selectedJob();
    if (!job) return;
    const now = Date.now();
    const sameTarget = this.lastDelete?.jobId === job.id && now - this.lastDelete.at < 2000;
    if (sameTarget) {
      try {
        await removeJob(job.id);
        this.message = `removed ${job.id}`;
      } catch (error) {
        this.message = error instanceof Error ? error.message : String(error);
      }
      this.lastDelete = null;
      return this.refresh();
    }
    await stopJob(job.id);
    this.lastDelete = { jobId: job.id, at: now };
    this.message = `stopped ${job.id}; press Ctrl+X again to delete`;
    await this.refresh();
  }

  async togglePin() {
    const job = this.selectedJob();
    if (!job) return;
    await pinJob(job.id);
    await this.refresh();
  }

  async render() {
    if (this.suspended) return;
    const width = process.stdout.columns || 100;
    const height = process.stdout.rows || 30;
    const lines = [];
    const counts = countJobs(this.jobs);
    lines.push(`Codex Agent View v0.1.0  cwd: ${shortCwd(process.cwd())}`);
    lines.push(`${counts.needs_input} awaiting input . ${counts.working} working . ${counts.completed} completed`);
    lines.push("");

    const reserved = this.peek || this.help ? 12 : 4;
    const maxRows = Math.max(1, height - reserved - 4);
    const start = Math.max(0, Math.min(this.selected - Math.floor(maxRows / 2), Math.max(0, this.rows.length - maxRows)));
    const visibleRows = this.rows.slice(start, start + maxRows);
    for (let offset = 0; offset < visibleRows.length; offset += 1) {
      const row = visibleRows[offset];
      const index = start + offset;
      if (row.type === "header") {
        lines.push("");
        lines.push(truncate(row.label, width));
        continue;
      }
      const selected = index === this.selected;
      lines.push(renderJobRow(row.job, { selected, width }));
    }

    while (lines.length < height - reserved) lines.push("");
    if (this.help) {
      lines.push(...renderHelp(width));
    } else if (this.peek) {
      lines.push(...(await renderPeek(this.selectedJob(), width)));
    }

    lines.push("".padEnd(width, "-"));
    if (this.message) lines.push(truncate(this.message, width));
    const prompt = this.peek && this.selectedJob() ? "reply" : "describe a task for a new session";
    lines.push(`> ${this.input || prompt}`.slice(0, width));
    lines.push("enter to open/send . space to reply . ctrl+x to stop/delete . ctrl+s group . ctrl+t pin . ? help");
    process.stdout.write(`\x1b[2J\x1b[H${lines.slice(0, height).join("\n")}\x1b[0m`);
  }
}

function groupJobsByState(jobs: Job[]): Group[] {
  const pinned = jobs.filter((job) => job.pinned);
  const rest = jobs.filter((job) => !job.pinned);
  return [
    { label: "Pinned", jobs: pinned },
    { label: "Needs input", jobs: rest.filter((job) => job.status === "needs_input") },
    { label: "Working", jobs: rest.filter((job) => job.status === "working") },
    { label: "Completed", jobs: rest.filter((job) => job.status === "completed") },
    { label: "Failed", jobs: rest.filter((job) => job.status === "failed") },
    { label: "Stopped", jobs: rest.filter((job) => job.status === "stopped") },
  ];
}

function groupJobsByCwd(jobs: Job[]): Group[] {
  const byCwd = new Map();
  for (const job of jobs) {
    const key = shortCwd(job.dispatchCwd || job.cwd);
    if (!byCwd.has(key)) byCwd.set(key, []);
    byCwd.get(key).push(job);
  }
  return [...byCwd.entries()].map(([label, grouped]) => ({ label, jobs: grouped }));
}

function renderJobRow(job: Job, { selected, width }: { selected: boolean; width: number }): string {
  const icon = statusIcon(job.status);
  const marker = selected ? "> " : "  ";
  const title = truncate(job.title, 32).padEnd(34, " ");
  const summary = truncate(job.blockingRequest?.message || job.lastSummary || "", Math.max(20, width - 55));
  const time = relativeTime(job.updatedAt).padStart(4, " ");
  const line = `${marker}${icon} ${title} ${summary} ${time}`;
  return selected ? `\x1b[7m${line.padEnd(width)}\x1b[0m` : line.slice(0, width);
}

async function renderPeek(job: Job | null, width: number): Promise<string[]> {
  if (!job) return ["No session selected."];
  const last = await readJobLast(job.id);
  const lines = [
    "".padEnd(width, "-"),
    `${job.id}  ${job.status}  ${job.title}`,
    `cwd: ${job.cwd}`,
  ];
  if (job.codexThreadId) lines.push(`thread: ${job.codexThreadId}`);
  if (job.worktreePath) lines.push(`worktree: ${job.worktreePath}`);
  if (job.blockingRequest) lines.push(`needs input: ${job.blockingRequest.message}`);
  if (job.prRefs?.length) lines.push(`prs: ${job.prRefs.map((ref) => ref.url).join(", ")}`);
  lines.push(truncate(last || job.lastOutput || job.lastSummary || "(no output yet)", width));
  return lines.map((line) => truncate(line, width));
}

function renderHelp(width: number): string[] {
  return [
    "".padEnd(width, "-"),
    "Shortcuts",
    "up/down select . enter open or send . space peek/reply . right attach",
    "ctrl+x stop, press again to delete . ctrl+t pin . ctrl+s group by state/directory",
    "type a prompt to dispatch . with peek open, typed text replies to selected session",
    "type /rename <title> while a row is selected to rename it . esc exits",
  ].map((line) => truncate(line, width));
}

function statusIcon(status: JobStatus): string {
  switch (status) {
    case "working":
      return "*";
    case "needs_input":
      return "?";
    case "completed":
      return ".";
    case "failed":
      return "x";
    case "stopped":
      return "#";
    default:
      return "-";
  }
}

function countJobs(jobs: Job[]): { needs_input: number; working: number; completed: number } {
  return {
    needs_input: jobs.filter((job) => job.status === "needs_input").length,
    working: jobs.filter((job) => job.status === "working").length,
    completed: jobs.filter((job) => job.status === "completed").length,
  };
}

function shortCwd(cwd: string): string {
  const home = process.env.HOME;
  if (home && cwd.startsWith(home)) return `~${cwd.slice(home.length)}`;
  return cwd;
}

function waitForEnter(): Promise<void> {
  return new Promise((resolve) => {
    const onData = (chunk: Buffer | string) => {
      if (chunk.toString("utf8").includes("\n") || chunk.toString("utf8").includes("\r")) {
        process.stdin.off("data", onData);
        process.stdin.pause();
        resolve(undefined);
      }
    };
    process.stdin.resume();
    process.stdin.on("data", onData);
  });
}
