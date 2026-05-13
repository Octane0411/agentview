import test from "node:test";
import assert from "node:assert/strict";
import { mkdir, mkdtemp, rm, writeFile } from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { createWorktree, removeWorktree, worktreeHasChanges } from "../dist/src/worktree.js";
import { runCommand } from "../dist/src/util.js";
import { putJob } from "../dist/src/store.js";
import { removeJob } from "../dist/src/jobs.js";

test("createWorktree creates an isolated git worktree and detects changes", async () => {
  const root = await mkdtemp(path.join(os.tmpdir(), "agentview-git-"));
  try {
    assert.equal((await runCommand("git", ["init"], { cwd: root })).code, 0);
    await runCommand("git", ["config", "user.email", "test@example.com"], { cwd: root });
    await runCommand("git", ["config", "user.name", "Agent View Test"], { cwd: root });
    await writeFile(path.join(root, "README.md"), "hello\n", "utf8");
    assert.equal((await runCommand("git", ["add", "README.md"], { cwd: root })).code, 0);
    assert.equal((await runCommand("git", ["commit", "-m", "init"], { cwd: root })).code, 0);

    const result = await createWorktree({ cwd: root, jobId: "av_test", title: "Fix README" });
    assert.equal(result.isolated, true);
    assert.match(result.worktreePath, /\.agentview\/worktrees\/av_test$/);
    assert.equal(await worktreeHasChanges(result.worktreePath), false);
    await writeFile(path.join(result.worktreePath, "README.md"), "changed\n", "utf8");
    assert.equal(await worktreeHasChanges(result.worktreePath), true);

    await removeWorktree(result.worktreePath, { force: true });
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});

test("removeJob refuses to delete a dirty worktree without force", async () => {
  const root = await mkdtemp(path.join(os.tmpdir(), "agentview-dirty-"));
  const home = path.join(root, "home");
  const repo = path.join(root, "repo");
  const previousHome = process.env.AGENTVIEW_HOME;
  process.env.AGENTVIEW_HOME = home;
  try {
    await mkdir(repo, { recursive: true });
    assert.equal((await runCommand("git", ["init"], { cwd: repo })).code, 0);
    await runCommand("git", ["config", "user.email", "test@example.com"], { cwd: repo });
    await runCommand("git", ["config", "user.name", "Agent View Test"], { cwd: repo });
    await writeFile(path.join(repo, "README.md"), "hello\n", "utf8");
    assert.equal((await runCommand("git", ["add", "README.md"], { cwd: repo })).code, 0);
    assert.equal((await runCommand("git", ["commit", "-m", "init"], { cwd: repo })).code, 0);

    const worktree = await createWorktree({ cwd: repo, jobId: "av_dirty", title: "Dirty Test" });
    await writeFile(path.join(worktree.worktreePath, "README.md"), "dirty\n", "utf8");
    const now = new Date().toISOString();
    await putJob({
      id: "av_dirty",
      provider: "codex",
      codexThreadId: null,
      title: "Dirty Test",
      initialPrompt: "dirty",
      repoRoot: repo,
      cwd: worktree.cwd,
      dispatchCwd: repo,
      worktreePath: worktree.worktreePath,
      worktreeBranch: worktree.branch,
      model: null,
      profile: null,
      approvalPolicy: "never",
      sandbox: "workspace-write",
      status: "completed",
      processState: "exited",
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
      completedAt: now,
      exitCode: 0,
    });

    await assert.rejects(() => removeJob("av_dirty"), /uncommitted changes/);
    await removeJob("av_dirty", { force: true });
  } finally {
    process.env.AGENTVIEW_HOME = previousHome;
    await rm(root, { recursive: true, force: true });
  }
});
