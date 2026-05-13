import test from "node:test";
import assert from "node:assert/strict";
import { chmod, mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { execFile } from "node:child_process";
import { promisify } from "node:util";
import { loadStore } from "../src/store.js";
import { runCommand } from "../src/util.js";

const execFileAsync = promisify(execFile);

test("CLI run/list/peek/attach work with a fake codex executable", async () => {
  const root = await mkdtemp(path.join(os.tmpdir(), "agentview-cli-"));
  const home = path.join(root, "home");
  const bin = path.join(root, "bin");
  const cwd = path.join(root, "repo");
  await writeFile(path.join(root, "placeholder"), "", "utf8");
  await mkdir(bin, { recursive: true });
  await mkdir(cwd, { recursive: true });
  assert.equal((await runCommand("git", ["init"], { cwd })).code, 0);
  await runCommand("git", ["config", "user.email", "test@example.com"], { cwd });
  await runCommand("git", ["config", "user.name", "Agent View Test"], { cwd });
  await writeFile(path.join(cwd, "README.md"), "hello\n", "utf8");
  assert.equal((await runCommand("git", ["add", "README.md"], { cwd })).code, 0);
  assert.equal((await runCommand("git", ["commit", "-m", "init"], { cwd })).code, 0);

  const fakeCodex = path.join(bin, "codex");
  const callsFile = path.join(root, "calls.log");
  await writeFile(
    fakeCodex,
    `#!/usr/bin/env node
const fs = require("fs");
const args = process.argv.slice(2);
fs.appendFileSync(${JSON.stringify(callsFile)}, args.join(" ") + "\\n");
if (args[0] === "exec") {
  const threadId = "123e4567-e89b-12d3-a456-426614174000";
  console.log(JSON.stringify({ method: "thread/started", params: { threadId } }));
  console.log(JSON.stringify({ method: "item/agentMessage/delta", params: { threadId, delta: "fake complete https://github.com/acme/app/pull/42" } }));
  process.exit(0);
}
if (args[0] === "resume") {
  console.log("resumed " + args[1]);
  process.exit(0);
}
process.exit(2);
`,
    "utf8",
  );
  await chmod(fakeCodex, 0o755);

  const previousHome = process.env.AGENTVIEW_HOME;
  process.env.AGENTVIEW_HOME = home;
  const env = {
    ...process.env,
    AGENTVIEW_HOME: home,
    PATH: `${bin}:${process.env.PATH}`,
  };

  try {
    const run = await execFileAsync(process.execPath, ["bin/agentview.js", "run", "--cwd", cwd, "fake task"], {
      cwd: process.cwd(),
      env,
    });
    const jobId = run.stdout.match(/backgrounded\s+(av_[^\s]+)/)?.[1];
    assert.ok(jobId);

    await waitFor(async () => {
      const store = await loadStore();
      return store.jobs[jobId]?.status === "completed";
    });

    const store = await loadStore();
    const job = store.jobs[jobId];
    assert.equal(job.codexThreadId, "123e4567-e89b-12d3-a456-426614174000");
    assert.equal(job.prRefs.length, 1);
    assert.match(job.worktreePath, /\.agentview\/worktrees\/av_/);
    assert.notEqual(job.cwd, cwd);

    const list = await execFileAsync(process.execPath, ["bin/agentview.js", "list"], {
      cwd: process.cwd(),
      env,
    });
    assert.match(list.stdout, /completed/);
    assert.match(list.stdout, /fake task/);

    const peek = await execFileAsync(process.execPath, ["bin/agentview.js", "peek", jobId], {
      cwd: process.cwd(),
      env,
    });
    assert.match(peek.stdout, /fake complete/);

    const attach = await execFileAsync(process.execPath, ["bin/agentview.js", "attach", jobId], {
      cwd: process.cwd(),
      env,
    });
    assert.match(attach.stdout, /resumed 123e4567/);

    const stop = await execFileAsync(process.execPath, ["bin/agentview.js", "stop", jobId], {
      cwd: process.cwd(),
      env,
    });
    assert.match(stop.stdout, /stopped/);

    const remove = await execFileAsync(process.execPath, ["bin/agentview.js", "rm", jobId], {
      cwd: process.cwd(),
      env,
    });
    assert.match(remove.stdout, /removed/);

    const afterRemove = await execFileAsync(process.execPath, ["bin/agentview.js", "list"], {
      cwd: process.cwd(),
      env,
    });
    assert.doesNotMatch(afterRemove.stdout, new RegExp(jobId));

    const calls = await readFile(callsFile, "utf8");
    assert.match(calls, /exec --json/);
    assert.match(calls, new RegExp(`--cd ${escapeRegExp(job.worktreePath)}`));
    assert.match(calls, /resume 123e4567/);
  } finally {
    process.env.AGENTVIEW_HOME = previousHome;
    await rm(root, { recursive: true, force: true });
  }
});

test("CLI marks a non-zero codex exit as failed", async () => {
  const root = await mkdtemp(path.join(os.tmpdir(), "agentview-cli-fail-"));
  const home = path.join(root, "home");
  const bin = path.join(root, "bin");
  const cwd = path.join(root, "repo");
  await mkdir(bin, { recursive: true });
  await mkdir(cwd, { recursive: true });

  const fakeCodex = path.join(bin, "codex");
  await writeFile(
    fakeCodex,
    `#!/usr/bin/env node
if (process.argv.slice(2)[0] === "exec") {
  console.error("fake codex failed");
  process.exit(7);
}
process.exit(2);
`,
    "utf8",
  );
  await chmod(fakeCodex, 0o755);

  const previousHome = process.env.AGENTVIEW_HOME;
  process.env.AGENTVIEW_HOME = home;
  const env = {
    ...process.env,
    AGENTVIEW_HOME: home,
    PATH: `${bin}:${process.env.PATH}`,
  };

  try {
    const run = await execFileAsync(process.execPath, ["bin/agentview.js", "run", "--cwd", cwd, "failing task"], {
      cwd: process.cwd(),
      env,
    });
    const jobId = run.stdout.match(/backgrounded\s+(av_[^\s]+)/)?.[1];
    assert.ok(jobId);

    await waitFor(async () => {
      const store = await loadStore();
      return store.jobs[jobId]?.status === "failed";
    });

    const store = await loadStore();
    const job = store.jobs[jobId];
    assert.equal(job.exitCode, 7);
    assert.match(job.lastSummary, /failed/);
  } finally {
    process.env.AGENTVIEW_HOME = previousHome;
    await rm(root, { recursive: true, force: true });
  }
});

async function waitFor(predicate) {
  const deadline = Date.now() + 3000;
  while (Date.now() < deadline) {
    if (await predicate()) return;
    await new Promise((resolve) => setTimeout(resolve, 50));
  }
  assert.fail("condition was not met before timeout");
}

function escapeRegExp(value) {
  return String(value).replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}
