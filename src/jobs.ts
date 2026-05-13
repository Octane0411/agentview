import { spawn } from "node:child_process";
import path from "node:path";
import { fileURLToPath } from "node:url";
import {
  appendJobEvent,
  appendJobInbox,
  getJob,
  putJob,
  removeJobFiles,
  updateJob,
} from "./store.js";
import { createWorktree, removeWorktree, worktreeHasChanges } from "./worktree.js";
import {
  commandExists,
  extractPrRefs,
  makeJobId,
  nowIso,
  titleFromPrompt,
} from "./util.js";
import type { Job } from "./schema.js";

const currentFile = fileURLToPath(import.meta.url);
const repoRoot = path.resolve(path.dirname(currentFile), "..");
const binPath = path.join(repoRoot, "bin", "agentview.js");

type DispatchOptions = {
  cwd?: string;
  title?: string;
  model?: string;
  profile?: string;
  approvalPolicy?: Job["approvalPolicy"];
  sandbox?: Job["sandbox"];
};

type RemoveOptions = {
  force?: boolean;
  purge?: boolean;
};

export async function dispatchJob(prompt: string, options: DispatchOptions = {}): Promise<Job> {
  const cwd = path.resolve(options.cwd || process.cwd());
  const parsed = parseDispatchPrompt(prompt, cwd);
  const title = options.title || titleFromPrompt(parsed.prompt);
  const jobId = makeJobId();
  const worktree = await createWorktree({ cwd: parsed.cwd, jobId, title });
  const now = nowIso();
  const job: Job = {
    id: jobId,
    provider: "codex",
    codexThreadId: null,
    title,
    initialPrompt: parsed.prompt,
    repoRoot: worktree.repoRoot,
    cwd: worktree.cwd,
    dispatchCwd: parsed.cwd,
    worktreePath: worktree.worktreePath,
    worktreeBranch: worktree.branch,
    model: options.model || parsed.model || null,
    profile: options.profile || parsed.profile || null,
    approvalPolicy: options.approvalPolicy || "never",
    sandbox: options.sandbox || "workspace-write",
    status: "working",
    processState: "alive",
    pid: null,
    activeWorkerPid: null,
    pinned: false,
    manualOrder: null,
    archived: false,
    deleted: false,
    lastSummary: worktree.warning || "queued",
    lastOutput: null,
    blockingRequest: null,
    prRefs: extractPrRefs(parsed.prompt),
    createdAt: now,
    updatedAt: now,
    completedAt: null,
    exitCode: null,
  };

  await putJob(job);
  await appendJobEvent(jobId, {
    type: "agentview_job_created",
    prompt: parsed.prompt,
    cwd: job.cwd,
    worktreePath: job.worktreePath,
    timestamp: now,
  });

  const child = spawn(process.execPath, [binPath, "__worker", jobId, "run"], {
    cwd: job.cwd,
    detached: true,
    stdio: "ignore",
    env: process.env,
  });
  child.unref();

  await updateJob(jobId, () => ({
    pid: child.pid ?? null,
    processState: "alive",
    lastSummary: worktree.warning || "starting Codex",
  }));

  return { ...job, pid: child.pid ?? null, processState: "alive" };
}

export async function replyToJob(jobId: string, prompt: string): Promise<number | null> {
  const job = await requireJob(jobId);
  if (job.processState === "alive" && job.pid) {
    await appendJobInbox(jobId, { type: "reply", prompt });
    await updateJob(jobId, () => ({
      lastSummary: "reply queued",
      blockingRequest: null,
      status: "working",
    }));
    return job.pid;
  }
  if (!job.codexThreadId) throw new Error(`Job ${jobId} has no Codex thread id yet`);
  const child = spawn(process.execPath, [binPath, "__worker", jobId, "reply", prompt], {
    cwd: job.cwd,
    detached: true,
    stdio: "ignore",
    env: process.env,
  });
  child.unref();
  await updateJob(jobId, () => ({
    status: "working",
    processState: "alive",
    pid: child.pid,
    activeWorkerPid: child.pid,
    completedAt: null,
    lastSummary: "reply sent",
    blockingRequest: null,
  }));
  return child.pid ?? null;
}

export async function respawnJob(jobId: string, prompt = "Continue the previous task."): Promise<number | null> {
  const job = await requireJob(jobId);
  if (!job.codexThreadId) throw new Error(`Job ${jobId} has no Codex thread id yet`);
  const child = spawn(process.execPath, [binPath, "__worker", jobId, "resume", prompt], {
    cwd: job.cwd,
    detached: true,
    stdio: "ignore",
    env: process.env,
  });
  child.unref();
  await updateJob(jobId, () => ({
    status: "working",
    processState: "alive",
    pid: child.pid,
    activeWorkerPid: child.pid,
    completedAt: null,
    lastSummary: "respawned",
    blockingRequest: null,
  }));
  return child.pid ?? null;
}

export async function stopJob(jobId: string): Promise<void> {
  const job = await requireJob(jobId);
  if (job.pid) {
    try {
      process.kill(job.pid, "SIGTERM");
    } catch {
      // Already gone.
    }
  }
  await updateJob(jobId, () => ({
    status: "stopped",
    processState: "exited",
    pid: null,
    activeWorkerPid: null,
    completedAt: nowIso(),
    lastSummary: "stopped",
  }));
}

export async function removeJob(jobId: string, options: RemoveOptions = {}): Promise<void> {
  const job = await requireJob(jobId);
  if (job.pid) await stopJob(jobId);
  if (job.worktreePath && (await worktreeHasChanges(job.worktreePath)) && !options.force) {
    throw new Error(
      `Worktree has uncommitted changes; refusing to remove ${job.worktreePath}. Use --force to override.`,
    );
  }
  if (job.worktreePath) await removeWorktree(job.worktreePath, { force: options.force });
  await updateJob(jobId, () => ({
    deleted: true,
    archived: true,
    status: "stopped",
    processState: "exited",
    pid: null,
    activeWorkerPid: null,
    lastSummary: "deleted",
  }));
  if (options.purge) await removeJobFiles(jobId);
}

export async function archiveJob(jobId: string, archived = true): Promise<void> {
  await requireJob(jobId);
  await updateJob(jobId, () => ({ archived }));
}

export async function renameJob(jobId: string, title: string): Promise<void> {
  await requireJob(jobId);
  await updateJob(jobId, () => ({ title }));
}

export async function pinJob(jobId: string, pinned?: boolean): Promise<void> {
  const job = await requireJob(jobId);
  await updateJob(jobId, () => ({ pinned: pinned ?? !job.pinned }));
}

export async function requireJob(jobId: string): Promise<Job> {
  const job = await getJob(jobId);
  if (!job || job.deleted) throw new Error(`Unknown job: ${jobId}`);
  return job;
}

export async function doctor(): Promise<{ codex: boolean; node: string }> {
  return {
    codex: await commandExists("codex"),
    node: process.version,
  };
}

export function parseDispatchPrompt(input: string, cwd: string): { prompt: string; cwd: string; model: string | null; profile: string | null } {
  let prompt = String(input || "").trim();
  let model = null;
  let profile = null;
  let targetCwd = cwd;

  const modelMatch = prompt.match(/^model:([^\s]+)\s+([\s\S]*)$/);
  if (modelMatch) {
    model = modelMatch[1];
    prompt = modelMatch[2].trim();
  }

  const profileMatch = prompt.match(/^profile:([^\s]+)\s+([\s\S]*)$/);
  if (profileMatch) {
    profile = profileMatch[1];
    prompt = profileMatch[2].trim();
  }

  const repoMatch = prompt.match(/^@([^\s]+)\s+([\s\S]*)$/);
  if (repoMatch) {
    targetCwd = path.resolve(cwd, "..", repoMatch[1]);
    prompt = repoMatch[2].trim();
  }

  return { prompt, cwd: targetCwd, model, profile };
}
