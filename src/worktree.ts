import path from "node:path";
import { rm } from "node:fs/promises";
import { runCommand, slugify, pathExists } from "./util.js";

export type WorktreeInfo = {
  repoRoot: string;
  cwd: string;
  worktreePath: string | null;
  branch: string | null;
  isolated: boolean;
  warning: string | null;
};

export async function findGitRoot(cwd: string): Promise<string | null> {
  const result = await runCommand("git", ["rev-parse", "--show-toplevel"], { cwd });
  if (result.code !== 0) return null;
  return result.stdout.trim() || null;
}

export async function createWorktree({ cwd, jobId, title }: { cwd: string; jobId: string; title: string }): Promise<WorktreeInfo> {
  const repoRoot = await findGitRoot(cwd);
  if (!repoRoot) {
    return {
      repoRoot: cwd,
      cwd,
      worktreePath: null,
      branch: null,
      isolated: false,
      warning: "Not inside a git repository; running directly in the selected directory.",
    };
  }

  const worktreePath = path.join(repoRoot, ".agentview", "worktrees", jobId);
  const branch = `agentview/${jobId}-${slugify(title)}`;
  if (await pathExists(worktreePath)) {
    return { repoRoot, cwd: worktreePath, worktreePath, branch, isolated: true, warning: null };
  }

  const result = await runCommand("git", ["worktree", "add", "-b", branch, worktreePath, "HEAD"], {
    cwd: repoRoot,
  });
  if (result.code !== 0) {
    throw new Error(`Cannot create worktree: ${result.stderr || result.stdout}`);
  }

  return { repoRoot, cwd: worktreePath, worktreePath, branch, isolated: true, warning: null };
}

export async function worktreeHasChanges(worktreePath: string | null): Promise<boolean> {
  if (!worktreePath) return false;
  const result = await runCommand("git", ["status", "--porcelain"], { cwd: worktreePath });
  if (result.code !== 0) return true;
  return result.stdout.trim().length > 0;
}

export async function removeWorktree(worktreePath: string | null, { force = false }: { force?: boolean } = {}): Promise<void> {
  if (!worktreePath) return;
  const args = ["worktree", "remove"];
  if (force) args.push("--force");
  args.push(worktreePath);
  const result = await runCommand("git", args, { cwd: path.dirname(path.dirname(worktreePath)) });
  if (result.code !== 0) {
    if (!force) throw new Error(result.stderr || result.stdout || "git worktree remove failed");
    await rm(worktreePath, { recursive: true, force: true });
  }
}
