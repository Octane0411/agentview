import { mkdir, open, readFile, rename, rm, writeFile } from "node:fs/promises";
import path from "node:path";
import { ensureDir, nowIso, pathExists } from "./util.js";

const STORE_VERSION = 1;

export function getAgentviewHome() {
  return process.env.AGENTVIEW_HOME || path.join(process.env.HOME || process.cwd(), ".agentview");
}

export function getStorePath() {
  return path.join(getAgentviewHome(), "agentview.json");
}

export function getJobsDir() {
  return path.join(getAgentviewHome(), "jobs");
}

export function getJobDir(jobId) {
  return path.join(getJobsDir(), jobId);
}

export function getJobEventsPath(jobId) {
  return path.join(getJobDir(jobId), "events.jsonl");
}

export function getJobLastPath(jobId) {
  return path.join(getJobDir(jobId), "last.txt");
}

export async function initStore() {
  await ensureDir(getAgentviewHome());
  await ensureDir(getJobsDir());
  if (!(await pathExists(getStorePath()))) {
    await saveStore({ version: STORE_VERSION, jobs: {}, preferences: {} });
  }
}

export async function loadStore() {
  await initStore();
  const content = await readFile(getStorePath(), "utf8");
  const store = JSON.parse(content);
  store.jobs ||= {};
  store.preferences ||= {};
  return store;
}

export async function saveStore(store) {
  await ensureDir(getAgentviewHome());
  const target = getStorePath();
  const temp = `${target}.${process.pid}.${Date.now()}.tmp`;
  const normalized = {
    version: STORE_VERSION,
    jobs: store.jobs || {},
    preferences: store.preferences || {},
  };
  await writeFile(temp, `${JSON.stringify(normalized, null, 2)}\n`, "utf8");
  await rename(temp, target);
}

async function acquireLock() {
  const lockDir = path.join(getAgentviewHome(), "agentview.lock");
  await ensureDir(getAgentviewHome());
  const deadline = Date.now() + 5000;
  while (true) {
    try {
      await mkdir(lockDir);
      return async () => {
        await rm(lockDir, { recursive: true, force: true });
      };
    } catch (error) {
      if (Date.now() > deadline) throw new Error(`Timed out waiting for store lock: ${error.message}`);
      await new Promise((resolve) => setTimeout(resolve, 40));
    }
  }
}

export async function withStore(mutator) {
  const release = await acquireLock();
  try {
    const store = await loadStore();
    const result = await mutator(store);
    await saveStore(store);
    return result;
  } finally {
    await release();
  }
}

export async function getJob(jobId) {
  const store = await loadStore();
  return store.jobs[jobId] || null;
}

export async function listJobs(options = {}) {
  const store = await loadStore();
  const jobs = Object.values(store.jobs);
  return jobs
    .filter((job) => (options.all ? true : !job.archived && !job.deleted))
    .sort((a, b) => {
      if (a.pinned !== b.pinned) return a.pinned ? -1 : 1;
      return Date.parse(b.updatedAt || b.createdAt) - Date.parse(a.updatedAt || a.createdAt);
    });
}

export async function updateJob(jobId, updater) {
  return withStore(async (store) => {
    const job = store.jobs[jobId];
    if (!job) throw new Error(`Unknown job: ${jobId}`);
    const patch = await updater({ ...job });
    const next = patch ? { ...job, ...patch } : job;
    next.updatedAt = nowIso();
    store.jobs[jobId] = next;
    return next;
  });
}

export async function putJob(job) {
  return withStore(async (store) => {
    store.jobs[job.id] = job;
    await ensureDir(getJobDir(job.id));
    return job;
  });
}

export async function appendJobEvent(jobId, event) {
  await ensureDir(getJobDir(jobId));
  const handle = await open(getJobEventsPath(jobId), "a");
  try {
    await handle.write(`${JSON.stringify(event)}\n`);
  } finally {
    await handle.close();
  }
}

export async function writeJobLast(jobId, text) {
  await ensureDir(getJobDir(jobId));
  await writeFile(getJobLastPath(jobId), text || "", "utf8");
}

export async function readJobLast(jobId) {
  if (!(await pathExists(getJobLastPath(jobId)))) return "";
  return readFile(getJobLastPath(jobId), "utf8");
}

export async function removeJobFiles(jobId) {
  await rm(getJobDir(jobId), { recursive: true, force: true });
}
