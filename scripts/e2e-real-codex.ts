#!/usr/bin/env node
import { spawn } from "node:child_process";
import { chmod, mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { z } from "zod";
import { StoreSchema } from "../src/schema.js";
import type { Job } from "../src/schema.js";

const EnvSchema = z
  .object({
    AGENTVIEW_REAL_CODEX_E2E: z.string().optional(),
    AGENTVIEW_E2E_KEEP: z.string().optional(),
    AGENTVIEW_E2E_MODEL: z.string().optional(),
    AGENTVIEW_E2E_TIMEOUT_MS: z.coerce.number().int().positive().default(10 * 60 * 1000),
  })
  .passthrough();

type CommandResult = {
  code: number;
  stdout: string;
  stderr: string;
};

const terminalStatuses = new Set(["completed", "failed", "stopped"]);

async function main(): Promise<void> {
  const envConfig = EnvSchema.parse(process.env);
  const explicit = truthy(envConfig.AGENTVIEW_REAL_CODEX_E2E) || process.argv.includes("--yes");
  if (!explicit) {
    console.log("skipped real Codex E2E; set AGENTVIEW_REAL_CODEX_E2E=1 or pass --yes");
    return;
  }

  const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..", "..");
  const cliPath = path.join(repoRoot, "dist", "bin", "agentview.js");
  const root = await mkdtemp(path.join(os.tmpdir(), "agentview-real-codex-"));
  const agentviewHome = path.join(root, "agentview-home");
  const targetRepo = path.join(root, "repo");
  const runEnv = {
    ...process.env,
    AGENTVIEW_HOME: agentviewHome,
    NO_COLOR: "1",
  };
  let keep = truthy(envConfig.AGENTVIEW_E2E_KEEP);

  try {
    await assertCommand("codex");
    await assertCommand("git");
    await setupRepo(targetRepo);

    const doctor = await agentview(cliPath, ["doctor"], { env: runEnv });
    assertIncludes(doctor.stdout, "codex: ok", "agentview doctor did not find Codex");

    const prompt =
      "Edit README.md by adding exactly one new line after the heading: Real Codex E2E passed. " +
      "Do not modify any other file. Do not commit. Do not run network commands. " +
      "Finish with a concise summary mentioning Real Codex E2E passed.";
    const runArgs = ["run", "--cwd", targetRepo];
    if (envConfig.AGENTVIEW_E2E_MODEL) runArgs.push("--model", envConfig.AGENTVIEW_E2E_MODEL);
    runArgs.push(prompt);

    const launched = await agentview(cliPath, runArgs, { env: runEnv, timeoutMs: 30_000 });
    const jobId = parseJobId(launched.stdout);
    console.log(`job: ${jobId}`);

    let job = await waitForTerminalJob(agentviewHome, jobId, envConfig.AGENTVIEW_E2E_TIMEOUT_MS);
    if (job.status !== "completed") {
      await printDiagnostics(cliPath, runEnv, jobId);
      throw new Error(`expected initial job to complete, got ${job.status}: ${job.lastSummary || ""}`);
    }
    if (!job.codexThreadId) throw new Error("expected Codex thread id after real run");
    if (!job.worktreePath) throw new Error("expected an isolated git worktree");

    await assertWorktreeEdit(targetRepo, job.worktreePath);
    await assertCliSurfaces(cliPath, runEnv, jobId);

    const resumePrompt =
      'Read README.md and answer exactly "E2E resume saw the README line." Do not edit files.';
    const respawn = await agentview(cliPath, ["respawn", jobId, resumePrompt], {
      env: runEnv,
      timeoutMs: 30_000,
    });
    assertIncludes(respawn.stdout, "respawned", "respawn command did not enqueue a resume turn");

    job = await waitForTerminalJob(agentviewHome, jobId, envConfig.AGENTVIEW_E2E_TIMEOUT_MS);
    if (job.status !== "completed") {
      await printDiagnostics(cliPath, runEnv, jobId);
      throw new Error(`expected resumed job to complete, got ${job.status}: ${job.lastSummary || ""}`);
    }

    await smokeTui(cliPath, runEnv);
    await smokeAttach(cliPath, runEnv, jobId);

    const removeWithoutForce = await agentview(cliPath, ["rm", jobId], {
      env: runEnv,
      allowFailure: true,
      timeoutMs: 30_000,
    });
    if (removeWithoutForce.code === 0) {
      console.log("remove guard: worktree was clean, non-force remove succeeded");
    } else {
      assertIncludes(
        `${removeWithoutForce.stdout}${removeWithoutForce.stderr}`,
        "uncommitted changes",
        "non-force remove failed for an unexpected reason",
      );
      const forced = await agentview(cliPath, ["rm", "--force", jobId], { env: runEnv, timeoutMs: 30_000 });
      assertIncludes(forced.stdout, "removed", "force remove did not complete");
    }

    console.log("real Codex E2E passed");
    if (!keep) console.log(`temp root cleaned: ${root}`);
  } catch (error) {
    keep = true;
    throw error;
  } finally {
    if (keep) {
      console.log(`kept temp root: ${root}`);
    } else {
      await rm(root, { recursive: true, force: true });
    }
  }
}

async function setupRepo(repo: string): Promise<void> {
  await mkdir(repo, { recursive: true });
  await run("git", ["init", "-b", "main"], { cwd: repo });
  await run("git", ["config", "user.email", "agentview-e2e@example.com"], { cwd: repo });
  await run("git", ["config", "user.name", "AgentView E2E"], { cwd: repo });
  await writeFile(path.join(repo, "README.md"), "# AgentView Real Codex E2E\n\nbaseline\n", "utf8");
  await run("git", ["add", "README.md"], { cwd: repo });
  await run("git", ["commit", "-m", "init"], { cwd: repo });
}

async function assertWorktreeEdit(targetRepo: string, worktreePath: string): Promise<void> {
  const worktreeReadme = await readFile(path.join(worktreePath, "README.md"), "utf8");
  assertIncludes(worktreeReadme, "Real Codex E2E passed.", "Codex did not edit README.md in the worktree");

  const originalReadme = await readFile(path.join(targetRepo, "README.md"), "utf8");
  if (originalReadme.includes("Real Codex E2E passed.")) {
    throw new Error("Codex edit leaked into the original checkout instead of the Agent View worktree");
  }
}

async function assertCliSurfaces(cliPath: string, env: NodeJS.ProcessEnv, jobId: string): Promise<void> {
  const list = await agentview(cliPath, ["list"], { env });
  assertIncludes(list.stdout, jobId, "list output did not include the job id");
  assertIncludes(list.stdout, "completed", "list output did not show completed status");

  const peek = await agentview(cliPath, ["peek", jobId], { env });
  assertIncludes(peek.stdout, jobId, "peek output did not include the job id");
  assertIncludes(peek.stdout, "thread:", "peek output did not include the Codex thread id");
  assertIncludes(peek.stdout, "worktree:", "peek output did not include the worktree path");

  const logs = await agentview(cliPath, ["logs", jobId, "30"], { env });
  assertIncludes(logs.stdout, "thread", "logs did not include Codex JSON events");
}

async function smokeTui(cliPath: string, env: NodeJS.ProcessEnv): Promise<void> {
  const expect = await hasCommand("expect");
  if (!expect) {
    console.log("tui smoke: skipped because `expect` is not installed");
    return;
  }

  const script = [
    "set timeout 15",
    `set env(AGENTVIEW_HOME) ${expectQuote(env.AGENTVIEW_HOME || "")}`,
    "set env(NO_COLOR) 1",
    `spawn ${expectQuote(process.execPath)} ${expectQuote(cliPath)}`,
    'expect -re "Codex Agent View"',
    'send "\\033"',
    "expect eof",
  ].join("\n");
  const file = await writeTempScript(script);
  try {
    await run("expect", [file], { env, timeoutMs: 20_000 });
    console.log("tui smoke: passed");
  } finally {
    await rm(path.dirname(file), { recursive: true, force: true });
  }
}

async function smokeAttach(cliPath: string, env: NodeJS.ProcessEnv, jobId: string): Promise<void> {
  const expect = await hasCommand("expect");
  if (!expect) {
    console.log("attach smoke: skipped because `expect` is not installed");
    return;
  }

  const script = [
    "set timeout 20",
    `set env(AGENTVIEW_HOME) ${expectQuote(env.AGENTVIEW_HOME || "")}`,
    "set env(NO_COLOR) 1",
    `spawn ${expectQuote(process.execPath)} ${expectQuote(cliPath)} attach ${expectQuote(jobId)}`,
    "expect {",
    '  -re "does not have a Codex thread id|Unknown job|error|Error" { exit 2 }',
    '  -re "Codex|codex|resume|>|›" { send "\\003"; exp_continue }',
    '  timeout { send "\\003"; sleep 1; send "\\003"; exit 0 }',
    "  eof { exit 0 }",
    "}",
  ].join("\n");
  const file = await writeTempScript(script);
  try {
    const result = await run("expect", [file], { env, allowFailure: true, timeoutMs: 30_000 });
    if (result.code !== 0) {
      throw new Error(`attach smoke failed\nstdout:\n${result.stdout}\nstderr:\n${result.stderr}`);
    }
    console.log("attach smoke: passed");
  } finally {
    await rm(path.dirname(file), { recursive: true, force: true });
  }
}

async function waitForTerminalJob(home: string, jobId: string, timeoutMs: number): Promise<Job> {
  const deadline = Date.now() + timeoutMs;
  let lastStatus = "";
  let lastSummary = "";
  while (Date.now() < deadline) {
    const store = await loadStore(home);
    const job = store.jobs[jobId];
    if (job && (job.status !== lastStatus || job.lastSummary !== lastSummary)) {
      lastStatus = job.status;
      lastSummary = job.lastSummary || "";
      console.log(`status: ${job.status}${job.lastSummary ? ` - ${job.lastSummary}` : ""}`);
    }
    if (job && terminalStatuses.has(job.status)) return job;
    if (job?.status === "needs_input") return job;
    await sleep(1_500);
  }
  throw new Error(`timed out waiting for ${jobId}`);
}

async function loadStore(home: string) {
  const content = await readFile(path.join(home, "agentview.json"), "utf8");
  return StoreSchema.parse(JSON.parse(content));
}

async function printDiagnostics(cliPath: string, env: NodeJS.ProcessEnv, jobId: string): Promise<void> {
  const peek = await agentview(cliPath, ["peek", jobId], { env, allowFailure: true });
  const logs = await agentview(cliPath, ["logs", jobId, "80"], { env, allowFailure: true });
  console.error("peek diagnostics:\n" + peek.stdout + peek.stderr);
  console.error("log diagnostics:\n" + logs.stdout + logs.stderr);
}

async function writeTempScript(content: string): Promise<string> {
  const dir = await mkdtemp(path.join(os.tmpdir(), "agentview-expect-"));
  const file = path.join(dir, "script.exp");
  await writeFile(file, `${content}\n`, "utf8");
  await chmod(file, 0o700);
  return file;
}

async function assertCommand(command: string): Promise<void> {
  if (!(await hasCommand(command))) throw new Error(`missing required command: ${command}`);
}

async function hasCommand(command: string): Promise<boolean> {
  const result = await run("sh", ["-lc", `command -v ${shellQuote(command)}`], { allowFailure: true });
  return result.code === 0;
}

function parseJobId(stdout: string): string {
  const jobId = stdout.match(/backgrounded\s+(av_[^\s]+)/)?.[1];
  if (!jobId) throw new Error(`could not parse job id from output:\n${stdout}`);
  return jobId;
}

function assertIncludes(value: string, expected: string, message: string): void {
  if (!value.includes(expected)) {
    throw new Error(`${message}; expected ${JSON.stringify(expected)} in:\n${value}`);
  }
}

function agentview(
  cliPath: string,
  args: string[],
  options: { env: NodeJS.ProcessEnv; timeoutMs?: number; allowFailure?: boolean },
): Promise<CommandResult> {
  return run(process.execPath, [cliPath, ...args], options);
}

function run(
  command: string,
  args: string[],
  options: { cwd?: string; env?: NodeJS.ProcessEnv; timeoutMs?: number; allowFailure?: boolean } = {},
): Promise<CommandResult> {
  return new Promise((resolve, reject) => {
    const child = spawn(command, args, {
      cwd: options.cwd,
      env: options.env || process.env,
      stdio: ["ignore", "pipe", "pipe"],
    });
    let stdout = "";
    let stderr = "";
    let killed = false;
    const timeout = setTimeout(() => {
      killed = true;
      child.kill("SIGTERM");
      setTimeout(() => child.kill("SIGKILL"), 2_000).unref();
    }, options.timeoutMs || 120_000);
    child.stdout?.on("data", (chunk) => {
      stdout += chunk.toString("utf8");
    });
    child.stderr?.on("data", (chunk) => {
      stderr += chunk.toString("utf8");
    });
    child.on("error", (error) => {
      clearTimeout(timeout);
      reject(error);
    });
    child.on("close", (code) => {
      clearTimeout(timeout);
      const result = { code: code ?? 0, stdout, stderr };
      if (killed) {
        reject(new Error(`timed out running ${command} ${args.join(" ")}\nstdout:\n${stdout}\nstderr:\n${stderr}`));
        return;
      }
      if (result.code !== 0 && !options.allowFailure) {
        reject(new Error(`command failed: ${command} ${args.join(" ")}\nstdout:\n${stdout}\nstderr:\n${stderr}`));
        return;
      }
      resolve(result);
    });
  });
}

function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

function truthy(value: string | undefined): boolean {
  return ["1", "true", "yes", "on"].includes(String(value || "").toLowerCase());
}

function shellQuote(value: string): string {
  return `'${String(value).replaceAll("'", "'\\''")}'`;
}

function expectQuote(value: string): string {
  return `{${String(value).replaceAll("}", "\\}")}}`;
}

main().catch((error) => {
  console.error(error instanceof Error ? error.stack || error.message : String(error));
  process.exitCode = 1;
});
