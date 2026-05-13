import { spawn } from "node:child_process";
import { readFile } from "node:fs/promises";
import { attachCodex, tailJobEvents } from "./codex.js";
import {
  archiveJob,
  dispatchJob,
  doctor,
  pinJob,
  removeJob,
  renameJob,
  replyToJob,
  requireJob,
  respawnJob,
  stopJob,
} from "./jobs.js";
import { listJobs, readJobLast } from "./store.js";
import { runTui } from "./tui.js";
import { relativeTime, truncate } from "./util.js";
import { workerMain } from "./worker.js";
import type { Job, JobStatus } from "./schema.js";

export async function main(argv: string[]): Promise<void> {
  const args = argv.slice(2);
  const command = args[0];

  if (command === "__worker") {
    await workerMain(argv);
    return;
  }

  if (!command) {
    await runTui();
    return;
  }

  if (command === "help" || command === "--help" || command === "-h") {
    printHelp();
    return;
  }

  if (command === "--version" || command === "-v") {
    const pkg = JSON.parse(await readFile(new URL("../../package.json", import.meta.url), "utf8"));
    console.log(pkg.version);
    return;
  }

  const rest = args.slice(1);
  switch (command) {
    case "run":
      await cmdRun(rest);
      break;
    case "list":
    case "ls":
      await cmdList(rest);
      break;
    case "peek":
      await cmdPeek(rest);
      break;
    case "logs":
      await cmdLogs(rest);
      break;
    case "attach":
      await cmdAttach(rest);
      break;
    case "reply":
      await cmdReply(rest);
      break;
    case "approve":
    case "accept":
      await cmdDecision(rest, "approved");
      break;
    case "decline":
    case "deny":
      await cmdDecision(rest, "declined");
      break;
    case "stop":
      await cmdStop(rest);
      break;
    case "rm":
    case "remove":
      await cmdRemove(rest);
      break;
    case "archive":
      await cmdArchive(rest, true);
      break;
    case "unarchive":
      await cmdArchive(rest, false);
      break;
    case "rename":
      await cmdRename(rest);
      break;
    case "pin":
      await cmdPin(rest);
      break;
    case "respawn":
      await cmdRespawn(rest);
      break;
    case "interrupt":
      await cmdStop(rest);
      break;
    case "doctor":
      await cmdDoctor();
      break;
    default:
      throw new Error(`Unknown command: ${command}. Run agentview help.`);
  }
}

async function cmdRun(args: string[]): Promise<void> {
  const { flags, positional } = parseFlags(args, {
    boolean: ["attach"],
    string: ["cwd", "model", "sandbox"],
  });
  const prompt = positional.join(" ").trim();
  if (!prompt) throw new Error("Usage: agentview run [--cwd DIR] [--model MODEL] [--attach] \"task\"");
  const job = await dispatchJob(prompt, {
    cwd: typeof flags.cwd === "string" ? flags.cwd : undefined,
    model: typeof flags.model === "string" ? flags.model : undefined,
    sandbox: typeof flags.sandbox === "string" ? flags.sandbox as Job["sandbox"] : undefined,
  });
  console.log(`backgrounded  ${job.id}`);
  console.log("  agentview                 list sessions");
  console.log(`  agentview attach ${job.id}  open in this terminal`);
  console.log(`  agentview logs ${job.id}    show recent output`);
  console.log(`  agentview stop ${job.id}    stop this session`);
  if (Boolean(flags.attach)) await cmdAttach([job.id]);
}

async function cmdList(args: string[]): Promise<void> {
  const { flags } = parseFlags(args, { boolean: ["all"] });
  const jobs = await listJobs({ all: Boolean(flags.all) });
  for (const job of jobs) {
    console.log(formatJobLine(job));
  }
}

async function cmdPeek(args: string[]): Promise<void> {
  const jobId = args[0];
  if (!jobId) throw new Error("Usage: agentview peek <job_id>");
  const job = await requireJob(jobId);
  const last = await readJobLast(jobId);
  console.log(`${job.id}  ${job.status}  ${job.title}`);
  console.log(`cwd: ${job.cwd}`);
  if (job.codexThreadId) console.log(`thread: ${job.codexThreadId}`);
  if (job.worktreePath) console.log(`worktree: ${job.worktreePath}`);
  if (job.blockingRequest) console.log(`needs input: ${job.blockingRequest.message}`);
  if (job.prRefs?.length) console.log(`prs: ${job.prRefs.map((ref) => ref.url).join(", ")}`);
  console.log("");
  console.log(last || job.lastOutput || job.lastSummary || "(no output yet)");
}

async function cmdLogs(args: string[]): Promise<void> {
  const jobId = args[0];
  if (!jobId) throw new Error("Usage: agentview logs <job_id>");
  await requireJob(jobId);
  const lines = await tailJobEvents(jobId, Number(args[1]) || 80);
  for (const line of lines) console.log(line);
}

async function cmdAttach(args: string[]): Promise<void> {
  const jobId = args[0];
  if (!jobId) throw new Error("Usage: agentview attach <job_id>");
  const job = await requireJob(jobId);
  await attachCodex(job);
}

async function cmdReply(args: string[]): Promise<void> {
  const jobId = args[0];
  const prompt = args.slice(1).join(" ").trim();
  if (!jobId || !prompt) throw new Error("Usage: agentview reply <job_id> \"message\"");
  const pid = await replyToJob(jobId, prompt);
  console.log(`reply sent  ${jobId}  pid ${pid}`);
}

async function cmdDecision(args: string[], decision: "approved" | "declined"): Promise<void> {
  const jobId = args[0];
  if (!jobId) throw new Error(`Usage: agentview ${decision === "approved" ? "approve" : "decline"} <job_id>`);
  const pid = await replyToJob(jobId, decision);
  console.log(`${decision}  ${jobId}  pid ${pid}`);
}

async function cmdStop(args: string[]): Promise<void> {
  const jobId = args[0];
  if (!jobId) throw new Error("Usage: agentview stop <job_id>");
  await stopJob(jobId);
  console.log(`stopped ${jobId}`);
}

async function cmdRemove(args: string[]): Promise<void> {
  const { flags, positional } = parseFlags(args, { boolean: ["force", "purge"] });
  const jobId = positional[0];
  if (!jobId) throw new Error("Usage: agentview rm [--force] [--purge] <job_id>");
  await removeJob(jobId, { force: Boolean(flags.force), purge: Boolean(flags.purge) });
  console.log(`removed ${jobId}`);
}

async function cmdArchive(args: string[], archived: boolean): Promise<void> {
  const jobId = args[0];
  if (!jobId) throw new Error(`Usage: agentview ${archived ? "archive" : "unarchive"} <job_id>`);
  await archiveJob(jobId, archived);
  console.log(`${archived ? "archived" : "unarchived"} ${jobId}`);
}

async function cmdRename(args: string[]): Promise<void> {
  const jobId = args[0];
  const title = args.slice(1).join(" ").trim();
  if (!jobId || !title) throw new Error("Usage: agentview rename <job_id> \"title\"");
  await renameJob(jobId, title);
  console.log(`renamed ${jobId}`);
}

async function cmdPin(args: string[]): Promise<void> {
  const jobId = args[0];
  if (!jobId) throw new Error("Usage: agentview pin <job_id>");
  await pinJob(jobId);
  console.log(`toggled pin ${jobId}`);
}

async function cmdRespawn(args: string[]): Promise<void> {
  const jobId = args[0];
  const prompt = args.slice(1).join(" ").trim() || "Continue the previous task.";
  if (!jobId) throw new Error("Usage: agentview respawn <job_id> [prompt]");
  const pid = await respawnJob(jobId, prompt);
  console.log(`respawned ${jobId}  pid ${pid}`);
}

async function cmdDoctor(): Promise<void> {
  const result = await doctor();
  console.log(`codex: ${result.codex ? "ok" : "missing"}`);
  console.log(`node: ${result.node}`);
}

type FlagSchema = { boolean?: string[]; string?: string[] };

export function parseFlags(args: string[], schema: FlagSchema = {}): { flags: Record<string, string | boolean>; positional: string[] } {
  const flags: Record<string, string | boolean> = {};
  const positional = [];
  const boolean = new Set(schema.boolean || []);
  const string = new Set(schema.string || []);

  for (let index = 0; index < args.length; index += 1) {
    const arg = args[index];
    if (arg === "--") {
      positional.push(...args.slice(index + 1));
      break;
    }
    if (!arg.startsWith("--")) {
      positional.push(arg);
      continue;
    }
    const [rawName, inline] = arg.slice(2).split("=", 2);
    if (boolean.has(rawName)) {
      flags[rawName] = inline === undefined ? true : inline !== "false";
      continue;
    }
    if (string.has(rawName)) {
      const value = inline ?? args[++index];
      if (!value) throw new Error(`Missing value for --${rawName}`);
      flags[rawName] = value;
      continue;
    }
    throw new Error(`Unknown flag: --${rawName}`);
  }
  return { flags, positional };
}

export function formatJobLine(job: Job): string {
  const icon = statusIcon(job.status);
  const time = relativeTime(job.updatedAt);
  const title = truncate(job.title, 28).padEnd(30, " ");
  const summary = truncate(job.blockingRequest?.message || job.lastSummary || "", 72);
  return `${icon} ${job.id.padEnd(16, " ")} ${job.status.padEnd(12, " ")} ${title} ${summary.padEnd(74, " ")} ${time}`;
}

export function statusIcon(status: JobStatus): string {
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
      return ".";
  }
}

function printHelp() {
  console.log(`agentview

Usage:
  agentview                         Open the TUI
  agentview run [options] "task"     Dispatch a Codex job
  agentview list [--all]             List jobs
  agentview peek <job_id>            Show latest output
  agentview logs <job_id> [N]        Show normalized event log
  agentview attach <job_id>          Resume full Codex conversation
  agentview reply <job_id> "msg"     Send a follow-up turn
  agentview approve <job_id>         Send an approval reply
  agentview decline <job_id>         Send a decline reply
  agentview stop <job_id>            Stop a running job
  agentview rm [--force] <job_id>    Remove a job
  agentview archive <job_id>         Hide a job
  agentview doctor                   Check local dependencies

Options for run:
  --cwd DIR
  --model MODEL
  --sandbox MODE
  --attach
`);
}

export function spawnInteractive(command: string, args: string[], options: { cwd?: string } = {}): Promise<number> {
  const child = spawn(command, args, {
    cwd: options.cwd,
    stdio: "inherit",
    env: process.env,
  });
  return new Promise((resolve, reject) => {
    child.on("error", reject);
    child.on("exit", (code) => resolve(code ?? 0));
  });
}
