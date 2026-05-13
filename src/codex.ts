import { spawn } from "node:child_process";
import { readFile } from "node:fs/promises";
import path from "node:path";
import type { ChildProcessWithoutNullStreams } from "node:child_process";
import {
  commandExists,
  eventFailed,
  eventNeedsInput,
  extractPrRefs,
  extractThreadId,
  mergePrRefs,
  pathExists,
  readJsonLines,
  resolveHome,
  stripAnsi,
  summarizeEvent,
  truncate,
} from "./util.js";
import {
  appendJobEvent,
  getJob,
  getJobInboxPath,
  getJobEventsPath,
  updateJob,
  writeJobLast,
} from "./store.js";
import { InboxMessageSchema } from "./schema.js";
import type { Job } from "./schema.js";

export async function assertCodexAvailable() {
  if (!(await commandExists("codex"))) {
    throw new Error("Codex CLI not found. Install or expose `codex` on PATH.");
  }
}

export function buildCodexExecArgs(job: Job, prompt: string, options: { resume?: boolean } = {}): string[] {
  if (options.resume) {
    const args = ["exec", "resume", "--json"];
    if (job.model) args.push("--model", job.model);
    if (!job.worktreePath) args.push("--skip-git-repo-check");
    if (job.codexThreadId) args.push(job.codexThreadId);
    args.push(prompt);
    return args;
  }
  const args = ["exec", "--json", "--cd", job.cwd, "--sandbox", job.sandbox || "workspace-write"];
  if (job.model) args.push("--model", job.model);
  if (!job.worktreePath) args.push("--skip-git-repo-check");
  args.push(prompt);
  return args;
}

export function buildCodexResumeArgs(job: Job): string[] {
  const args = ["resume"];
  if (job.codexThreadId) args.push(job.codexThreadId);
  return args;
}

export async function runCodexTurn(jobId: string, prompt: string, options: { resume?: boolean } = {}): Promise<void> {
  await assertCodexAvailable();
  const job = await getJob(jobId);
  if (!job) throw new Error(`Unknown job: ${jobId}`);
  const args = buildCodexExecArgs(job, prompt, options);

  await updateJob(jobId, () => ({
    status: "working",
    processState: "alive",
    pid: process.pid,
    activeWorkerPid: process.pid,
    lastSummary: options.resume ? "Resuming Codex thread" : "Starting Codex session",
    blockingRequest: null,
    error: null,
  }));

  const child = spawn("codex", args, {
    cwd: job.cwd,
    env: process.env,
    stdio: ["pipe", "pipe", "pipe"],
  });

  await updateJob(jobId, () => ({
    pid: child.pid,
    activeWorkerPid: process.pid,
    processState: "alive",
  }));

  let stdoutBuffer = "";
  let stderrBuffer = "";
  let eventQueue = Promise.resolve();
  const stopInboxPump = pumpInboxToStdin(jobId, child);

  const enqueueLine = (line: string) => {
    eventQueue = eventQueue.then(() => handleCodexLine(jobId, line));
    return eventQueue;
  };

  child.stdout?.on("data", (chunk) => {
    stdoutBuffer += chunk.toString("utf8");
    const lines = stdoutBuffer.split(/\r?\n/);
    stdoutBuffer = lines.pop() || "";
    for (const line of lines) {
      enqueueLine(line);
    }
  });

  child.stderr?.on("data", (chunk) => {
    const text = chunk.toString("utf8");
    stderrBuffer += text;
    void appendJobEvent(jobId, {
      type: "stderr",
      text: stripAnsi(text),
      timestamp: new Date().toISOString(),
    });
    void updateJob(jobId, () => ({
      lastOutput: truncate(stripAnsi(text), 200),
      lastSummary: truncate(stripAnsi(text), 120),
    }));
  });

  const exitCode = await new Promise<number>((resolve) => {
    child.on("error", async (error) => {
      await appendJobEvent(jobId, {
        type: "process_error",
        error: error.message,
        timestamp: new Date().toISOString(),
      });
      resolve(127);
    });
    child.on("close", (code) => resolve(code ?? 0));
  });
  stopInboxPump();

  if (stdoutBuffer.trim()) enqueueLine(stdoutBuffer);
  await eventQueue;
  const finalThreadId = await discoverCodexThreadId(jobId);

  const latest = await getJob(jobId);
  const stopped = latest?.status === "stopped";
  const failed = exitCode !== 0 && !stopped;
  const finalOutput = failed ? truncate(stripAnsi(stderrBuffer), 240) : latest?.lastOutput;

  await updateJob(jobId, () => ({
    status: stopped ? "stopped" : failed ? "failed" : "completed",
    processState: "exited",
    pid: null,
    activeWorkerPid: null,
    exitCode,
    codexThreadId: latest?.codexThreadId || finalThreadId || null,
    completedAt: new Date().toISOString(),
    lastSummary: stopped
      ? "stopped"
      : failed
        ? `failed: codex exited ${exitCode}`
        : latest?.lastSummary || "completed",
    lastOutput: finalOutput,
    blockingRequest: null,
  }));
}

function pumpInboxToStdin(jobId: string, child: ChildProcessWithoutNullStreams): () => void {
  const inboxPath = getJobInboxPath(jobId);
  let offset = 0;
  let stopped = false;
  const timer = setInterval(async () => {
    if (stopped || child.stdin.destroyed || child.exitCode !== null) return;
    try {
      const content = await readFile(inboxPath, "utf8");
      if (content.length <= offset) return;
      const next = content.slice(offset);
      offset = content.length;
      for (const line of next.split(/\r?\n/).filter(Boolean)) {
        let message;
        try {
          message = InboxMessageSchema.parse(JSON.parse(line));
        } catch {
          continue;
        }
        if (message.type === "reply" && message.prompt) {
          child.stdin.write(`${message.prompt}\n`);
          await appendJobEvent(jobId, {
            type: "agentview_reply_sent",
            prompt: message.prompt,
            timestamp: new Date().toISOString(),
          });
        }
      }
    } catch {
      // The inbox file is created lazily on first reply.
    }
  }, 200);
  return () => {
    stopped = true;
    clearInterval(timer);
    try {
      child.stdin.end();
    } catch {
      // ignore
    }
  };
}

export async function handleCodexLine(jobId: string, rawLine: string): Promise<void> {
  const line = rawLine.trim();
  if (!line) return;
  let event;
  try {
    event = JSON.parse(line);
  } catch {
    event = { type: "text", text: stripAnsi(line) };
  }
  event.timestamp ||= new Date().toISOString();
  await appendJobEvent(jobId, event);

  const summary = summarizeEvent(event);
  const threadId = extractThreadId(event);
  const refs = extractPrRefs(JSON.stringify(event));
  const needsInput = eventNeedsInput(event);
  const failed = eventFailed(event);

  await updateJob(jobId, (job) => ({
    codexThreadId: job.codexThreadId || threadId || null,
    status: needsInput ? "needs_input" : failed ? "failed" : "working",
    blockingRequest: needsInput
      ? {
          type: "codex_request",
          message: summary || "Codex is waiting for input",
          event,
          createdAt: new Date().toISOString(),
        }
      : job.blockingRequest,
    lastSummary: summary || job.lastSummary,
    lastOutput: summary || job.lastOutput,
    prRefs: mergePrRefs(job.prRefs, refs),
  }));
  if (summary) await writeJobLast(jobId, summary);
}

export async function discoverCodexThreadId(jobId: string): Promise<string | null> {
  const job = await getJob(jobId);
  if (!job) return null;
  if (job.codexThreadId) return job.codexThreadId;

  const events = await readJsonLines(getJobEventsPath(jobId));
  for (let index = events.length - 1; index >= 0; index -= 1) {
    const threadId = extractThreadId(events[index]);
    if (threadId) return threadId;
  }

  return findRecentCodexSessionId(job);
}

export async function findRecentCodexSessionId(job: Job): Promise<string | null> {
  const indexPath = resolveHome(".codex/session_index.jsonl");
  if (!(await pathExists(indexPath))) return null;
  const content = await readFile(indexPath, "utf8");
  const lines = content.split(/\r?\n/).filter(Boolean).slice(-300);
  const candidates = [];
  for (const line of lines) {
    let entry;
    try {
      entry = JSON.parse(line);
    } catch {
      continue;
    }
    const asText = JSON.stringify(entry);
    const matchesCwd = asText.includes(job.cwd) || (job.repoRoot && asText.includes(job.repoRoot));
    if (!matchesCwd) continue;
    const threadId = extractThreadId(entry);
    if (!threadId) continue;
    candidates.push(threadId);
  }
  return candidates.at(-1) || null;
}

export async function attachCodex(job: Job): Promise<number> {
  await assertCodexAvailable();
  if (!job.codexThreadId) {
    throw new Error(`Job ${job.id} does not have a Codex thread id yet. Try again after the first Codex event arrives.`);
  }
  const child = spawn("codex", buildCodexResumeArgs(job), {
    cwd: job.cwd,
    stdio: "inherit",
    env: process.env,
  });
  return new Promise((resolve, reject) => {
    child.on("error", reject);
    child.on("exit", (code) => resolve(code ?? 0));
  });
}

export async function tailJobEvents(jobId: string, limit = 80): Promise<string[]> {
  const file = getJobEventsPath(jobId);
  if (!(await pathExists(file))) return [];
  const content = await readFile(file, "utf8");
  return content.split(/\r?\n/).filter(Boolean).slice(-limit);
}

export function cwdForDisplay(cwd: string): string {
  const home = process.env.HOME;
  if (home && cwd.startsWith(home)) return `~${cwd.slice(home.length)}`;
  return path.relative(process.cwd(), cwd) || cwd;
}
