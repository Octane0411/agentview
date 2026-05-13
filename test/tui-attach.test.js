import test from "node:test";
import assert from "node:assert/strict";
import { chmod, mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { execFile } from "node:child_process";
import { promisify } from "node:util";
import { runCommand } from "../dist/src/util.js";

const execFileAsync = promisify(execFile);

test("TUI attach suspends Agent View rendering while Codex owns the terminal", async (t) => {
  const expectPath = await commandPath("expect");
  if (!expectPath) {
    t.skip("expect is required for PTY TUI smoke tests");
    return;
  }

  const root = await mkdtemp(path.join(os.tmpdir(), "agentview-tui-attach-"));
  const home = path.join(root, "home");
  const bin = path.join(root, "bin");
  const cwd = path.join(root, "repo");
  await mkdir(bin, { recursive: true });
  await mkdir(cwd, { recursive: true });
  assert.equal((await runCommand("git", ["init"], { cwd })).code, 0);
  await runCommand("git", ["config", "user.email", "test@example.com"], { cwd });
  await runCommand("git", ["config", "user.name", "Agent View Test"], { cwd });
  await writeFile(path.join(cwd, "README.md"), "hello\n", "utf8");
  assert.equal((await runCommand("git", ["add", "README.md"], { cwd })).code, 0);
  assert.equal((await runCommand("git", ["commit", "-m", "init"], { cwd })).code, 0);

  const fakeCodex = path.join(bin, "codex");
  await writeFile(
    fakeCodex,
    `#!/usr/bin/env node
const args = process.argv.slice(2);
if (args[0] === "exec") {
  const threadId = "123e4567-e89b-12d3-a456-426614174000";
  console.log(JSON.stringify({ type: "thread.started", thread_id: threadId }));
  console.log(JSON.stringify({ type: "item.completed", item: { type: "agent_message", text: "ready to attach" } }));
  process.exit(0);
}
if (args[0] === "resume") {
  console.log("FAKE_CODEX_RESUME_START");
  setTimeout(() => {
    console.log("FAKE_CODEX_RESUME_END");
    process.exit(0);
  }, 2200);
  return;
}
process.exit(2);
`,
    "utf8",
  );
  await chmod(fakeCodex, 0o755);

  const env = {
    ...process.env,
    AGENTVIEW_HOME: home,
    PATH: `${bin}:${process.env.PATH}`,
    NO_COLOR: "1",
  };

  try {
    const launched = await execFileAsync(process.execPath, ["dist/bin/agentview.js", "run", "--cwd", cwd, "fake task"], {
      cwd: process.cwd(),
      env,
    });
    const jobId = launched.stdout.match(/backgrounded\s+(av_[^\s]+)/)?.[1];
    assert.ok(jobId);

    await waitFor(async () => {
      const store = JSON.parse(await readFile(path.join(home, "agentview.json"), "utf8"));
      return store.jobs[jobId]?.status === "completed";
    });

    const expectScript = path.join(root, "attach.exp");
    await writeFile(
      expectScript,
      [
        "set timeout 12",
        `set env(AGENTVIEW_HOME) ${expectQuote(home)}`,
        `set env(PATH) ${expectQuote(`${bin}:${process.env.PATH}`)}`,
        "set env(NO_COLOR) 1",
        `spawn ${expectQuote(process.execPath)} ${expectQuote(path.join(process.cwd(), "dist/bin/agentview.js"))}`,
        'expect -re "Codex Agent View"',
        'send "\\r"',
        'expect "FAKE_CODEX_RESUME_START"',
        'expect "FAKE_CODEX_RESUME_END"',
        'expect -re "Codex Agent View"',
        'send "\\033"',
        "expect eof",
      ].join("\n"),
      "utf8",
    );

    const result = await execFileAsync(expectPath, [expectScript], {
      cwd: process.cwd(),
      env,
      maxBuffer: 1024 * 1024,
    });
    const duringAttach = result.stdout.split("FAKE_CODEX_RESUME_START")[1]?.split("FAKE_CODEX_RESUME_END")[0] || "";
    assert.doesNotMatch(duringAttach, /Codex Agent View/);
  } finally {
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

async function commandPath(command) {
  const result = await runCommand("sh", ["-lc", `command -v ${shellQuote(command)}`]);
  return result.code === 0 ? result.stdout.trim() : "";
}

function shellQuote(value) {
  return `'${String(value).replaceAll("'", "'\\''")}'`;
}

function expectQuote(value) {
  return `{${String(value).replaceAll("}", "\\}")}}`;
}
