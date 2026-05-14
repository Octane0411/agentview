use crate::util::{path_exists, run_command, slugify};
use anyhow::{Context, Result, bail};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct WorktreeInfo {
    pub repo_root: PathBuf,
    pub cwd: PathBuf,
    pub worktree_path: Option<PathBuf>,
    pub branch: Option<String>,
    pub warning: Option<String>,
}

pub fn find_git_root(cwd: &Path) -> Result<Option<PathBuf>> {
    let output = run_command("git", &["rev-parse", "--show-toplevel"], Some(cwd))?;
    if output.code != 0 {
        return Ok(None);
    }
    let root = output.stdout.trim();
    if root.is_empty() {
        Ok(None)
    } else {
        Ok(Some(PathBuf::from(root)))
    }
}

pub fn create_worktree(cwd: &Path, job_id: &str, title: &str) -> Result<WorktreeInfo> {
    let Some(repo_root) = find_git_root(cwd)? else {
        return Ok(WorktreeInfo {
            repo_root: cwd.to_path_buf(),
            cwd: cwd.to_path_buf(),
            worktree_path: None,
            branch: None,
            warning: Some(
                "Not inside a git repository; running directly in the selected directory."
                    .to_string(),
            ),
        });
    };

    let worktree_path = repo_root.join(".agentview").join("worktrees").join(job_id);
    let branch = format!("agentview/{job_id}-{}", slugify(title));
    if path_exists(&worktree_path) {
        return Ok(WorktreeInfo {
            repo_root,
            cwd: worktree_path.clone(),
            worktree_path: Some(worktree_path),
            branch: Some(branch),
            warning: None,
        });
    }

    let worktree_str = worktree_path.to_string_lossy().to_string();
    let output = run_command(
        "git",
        &["worktree", "add", "-b", &branch, &worktree_str, "HEAD"],
        Some(&repo_root),
    )?;
    if output.code != 0 {
        bail!(
            "Cannot create worktree: {}",
            if output.stderr.trim().is_empty() {
                output.stdout.trim()
            } else {
                output.stderr.trim()
            }
        );
    }

    Ok(WorktreeInfo {
        repo_root,
        cwd: worktree_path.clone(),
        worktree_path: Some(worktree_path),
        branch: Some(branch),
        warning: None,
    })
}

pub fn worktree_has_changes(worktree_path: Option<&str>) -> Result<bool> {
    let Some(worktree_path) = worktree_path else {
        return Ok(false);
    };
    let path = Path::new(worktree_path);
    if !path_exists(path) {
        return Ok(false);
    }
    let output = run_command("git", &["status", "--porcelain"], Some(path))?;
    if output.code != 0 {
        return Ok(true);
    }
    Ok(!output.stdout.trim().is_empty())
}

pub fn remove_worktree(worktree_path: Option<&str>, force: bool) -> Result<()> {
    let Some(worktree_path) = worktree_path else {
        return Ok(());
    };
    let path = PathBuf::from(worktree_path);
    let cwd = path
        .parent()
        .and_then(Path::parent)
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    if !path_exists(&path) {
        if path_exists(&cwd) {
            let _ = run_command("git", &["worktree", "prune"], Some(&cwd));
        }
        return Ok(());
    }
    let mut args = vec!["worktree", "remove"];
    if force {
        args.push("--force");
    }
    args.push(worktree_path);

    let output = run_command("git", &args, Some(&cwd)).context("failed to remove git worktree")?;
    if output.code == 0 {
        return Ok(());
    }
    if !force {
        bail!(
            "{}",
            if output.stderr.trim().is_empty() {
                output.stdout.trim()
            } else {
                output.stderr.trim()
            }
        );
    }
    if path_exists(&path) {
        fs::remove_dir_all(path)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn missing_worktree_is_not_dirty() {
        let temp = TempDir::new().unwrap();
        let missing = temp.path().join("missing");

        assert!(!worktree_has_changes(Some(missing.to_str().unwrap())).unwrap());
    }

    #[test]
    fn removing_missing_worktree_is_ok() {
        let temp = TempDir::new().unwrap();
        let missing = temp
            .path()
            .join(".agentview")
            .join("worktrees")
            .join("missing");

        remove_worktree(Some(missing.to_str().unwrap()), false).unwrap();
    }
}
