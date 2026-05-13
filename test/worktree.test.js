import test from "node:test";
import assert from "node:assert/strict";
import { mkdtemp, rm, writeFile } from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { createWorktree, removeWorktree, worktreeHasChanges } from "../src/worktree.js";
import { runCommand } from "../src/util.js";

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
