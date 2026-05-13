import test from "node:test";
import assert from "node:assert/strict";
import { parseFlags } from "../src/cli.js";
import { buildCodexExecArgs } from "../src/codex.js";
import { parseDispatchPrompt } from "../src/jobs.js";
import {
  eventNeedsInput,
  extractPrRefs,
  extractThreadId,
  summarizeEvent,
  truncate,
} from "../src/util.js";

test("parseFlags separates flags and positional arguments", () => {
  const parsed = parseFlags(["--cwd", "/tmp/repo", "--attach", "fix", "tests"], {
    boolean: ["attach"],
    string: ["cwd"],
  });
  assert.deepEqual(parsed.flags, { cwd: "/tmp/repo", attach: true });
  assert.deepEqual(parsed.positional, ["fix", "tests"]);
});

test("parseDispatchPrompt supports model and repo prefixes", () => {
  const parsed = parseDispatchPrompt("model:gpt-5.2-codex @api fix auth", "/projects/app");
  assert.equal(parsed.model, "gpt-5.2-codex");
  assert.equal(parsed.cwd, "/projects/api");
  assert.equal(parsed.prompt, "fix auth");
});

test("event helpers extract thread id, summary, input requests, and PR refs", () => {
  const event = {
    method: "item/tool/requestUserInput",
    params: {
      threadId: "123e4567-e89b-12d3-a456-426614174000",
      message: "Approve migration?",
      output: "Opened https://github.com/acme/app/pull/42",
    },
  };
  assert.equal(extractThreadId(event), "123e4567-e89b-12d3-a456-426614174000");
  assert.equal(eventNeedsInput(event), true);
  assert.equal(summarizeEvent(event), "Approve migration?");
  assert.deepEqual(extractPrRefs(JSON.stringify(event)), [
    {
      url: "https://github.com/acme/app/pull/42",
      owner: "acme",
      repo: "app",
      number: 42,
      status: "unknown",
    },
  ]);
});

test("truncate uses ASCII ellipsis", () => {
  assert.equal(truncate("abcdefghijklmnopqrstuvwxyz", 10), "abcdefg...");
});

test("buildCodexExecArgs uses different option shapes for run and resume", () => {
  const job = {
    cwd: "/repo/worktree",
    worktreePath: "/repo/worktree",
    sandbox: "workspace-write",
    model: "gpt-5.2-codex",
    codexThreadId: "123e4567-e89b-12d3-a456-426614174000",
  };
  assert.deepEqual(buildCodexExecArgs(job, "do task"), [
    "exec",
    "--json",
    "--cd",
    "/repo/worktree",
    "--sandbox",
    "workspace-write",
    "--model",
    "gpt-5.2-codex",
    "do task",
  ]);
  assert.deepEqual(buildCodexExecArgs(job, "continue", { resume: true }), [
    "exec",
    "resume",
    "--json",
    "--model",
    "gpt-5.2-codex",
    "123e4567-e89b-12d3-a456-426614174000",
    "continue",
  ]);
});
