import test from "node:test";
import assert from "node:assert/strict";
import { mkdtemp, rm } from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { handleCodexLine } from "../src/codex.js";
import { getJob, putJob, readJobLast } from "../src/store.js";

function sampleJob(overrides = {}) {
  const now = new Date().toISOString();
  return {
    id: "av_test",
    provider: "codex",
    codexThreadId: null,
    title: "test job",
    initialPrompt: "test prompt",
    repoRoot: process.cwd(),
    cwd: process.cwd(),
    dispatchCwd: process.cwd(),
    worktreePath: null,
    worktreeBranch: null,
    model: null,
    profile: null,
    approvalPolicy: "never",
    sandbox: "workspace-write",
    status: "working",
    processState: "alive",
    pid: null,
    activeWorkerPid: null,
    pinned: false,
    manualOrder: null,
    archived: false,
    deleted: false,
    lastSummary: null,
    lastOutput: null,
    blockingRequest: null,
    prRefs: [],
    createdAt: now,
    updatedAt: now,
    completedAt: null,
    exitCode: null,
    ...overrides,
  };
}

test("handleCodexLine updates job metadata from JSON events", async () => {
  const home = await mkdtemp(path.join(os.tmpdir(), "agentview-test-"));
  const previousHome = process.env.AGENTVIEW_HOME;
  process.env.AGENTVIEW_HOME = home;
  try {
    await putJob(sampleJob());
    await handleCodexLine(
      "av_test",
      JSON.stringify({
        method: "item/agentMessage/delta",
        params: {
          threadId: "123e4567-e89b-12d3-a456-426614174000",
          delta: "Implemented change in https://github.com/acme/app/pull/42",
        },
      }),
    );
    const job = await getJob("av_test");
    assert.equal(job.codexThreadId, "123e4567-e89b-12d3-a456-426614174000");
    assert.equal(job.status, "working");
    assert.equal(job.prRefs.length, 1);
    assert.match(await readJobLast("av_test"), /Implemented change/);
  } finally {
    process.env.AGENTVIEW_HOME = previousHome;
    await rm(home, { recursive: true, force: true });
  }
});

test("handleCodexLine marks blocking requests as needs_input", async () => {
  const home = await mkdtemp(path.join(os.tmpdir(), "agentview-test-"));
  const previousHome = process.env.AGENTVIEW_HOME;
  process.env.AGENTVIEW_HOME = home;
  try {
    await putJob(sampleJob());
    await handleCodexLine(
      "av_test",
      JSON.stringify({
        method: "item/tool/requestUserInput",
        params: {
          threadId: "123e4567-e89b-12d3-a456-426614174000",
          message: "Approve command?",
        },
      }),
    );
    const job = await getJob("av_test");
    assert.equal(job.status, "needs_input");
    assert.equal(job.blockingRequest.message, "Approve command?");
  } finally {
    process.env.AGENTVIEW_HOME = previousHome;
    await rm(home, { recursive: true, force: true });
  }
});
