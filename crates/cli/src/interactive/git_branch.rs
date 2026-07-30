//! Git branch resolution for the footer.
//!
//! Mirrors the branch logic of the TypeScript `FooterDataProvider` in
//! `pi/packages/coding-agent/src/core/footer-data-provider.ts`.
//!
//! Unlike the TypeScript provider, this does not install filesystem watchers
//! (the `notify` crate is not a workspace dependency). Instead it re-reads
//! `.git/HEAD` on each `branch()` call, which is cheap and always reflects the
//! current checkout at render time.

use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Clone)]
struct GitPaths {
    repo_dir: PathBuf,
    head_path: PathBuf,
}

/// Resolves the current git branch for display in the footer.
#[derive(Debug, Clone, Default)]
pub(super) struct GitBranchProvider {
    paths: Option<GitPaths>,
}

impl GitBranchProvider {
    pub(super) fn new(cwd: &Path) -> Self {
        Self {
            paths: find_git_paths(cwd),
        }
    }

    pub(super) fn set_cwd(&mut self, cwd: &Path) {
        self.paths = find_git_paths(cwd);
    }

    /// Current git branch, or `None` if not inside a git repo. Returns
    /// `"detached"` for a detached HEAD, matching the TypeScript provider.
    pub(super) fn branch(&self) -> Option<String> {
        let paths = self.paths.as_ref()?;
        let content = std::fs::read_to_string(&paths.head_path)
            .ok()?
            .trim()
            .to_string();
        if let Some(branch) = content.strip_prefix("ref: refs/heads/") {
            let branch = branch.to_string();
            // A `.invalid` ref marker means the branch ref is broken; fall back
            // to asking git directly (mirrors the TypeScript provider).
            if branch == ".invalid" {
                return resolve_branch_with_git(&paths.repo_dir)
                    .or_else(|| Some("detached".to_string()));
            }
            return Some(branch);
        }
        Some("detached".to_string())
    }
}

/// Walk up from `start` looking for `.git`, handling both regular repos (`.git`
/// is a directory) and worktrees (`.git` is a file pointing at the gitdir).
fn find_git_paths(start: &Path) -> Option<GitPaths> {
    let mut dir = start.to_path_buf();
    loop {
        let git_path = dir.join(".git");
        if let Ok(meta) = std::fs::metadata(&git_path) {
            if meta.is_file() {
                // Worktree: `.git` contains `gitdir: <path>`.
                let content = std::fs::read_to_string(&git_path).ok()?.trim().to_string();
                let gitdir = content
                    .strip_prefix("gitdir: ")
                    .map(str::trim)
                    .map(|p| dir.join(p))
                    .unwrap_or_else(|| dir.join(&content));
                let head_path = gitdir.join("HEAD");
                if head_path.exists() {
                    return Some(GitPaths {
                        repo_dir: dir,
                        head_path,
                    });
                }
            } else if meta.is_dir() {
                let head_path = git_path.join("HEAD");
                if head_path.exists() {
                    return Some(GitPaths {
                        repo_dir: dir,
                        head_path,
                    });
                }
            }
        }
        if !dir.pop() {
            return None;
        }
    }
}

/// Ask git for the current branch synchronously (mirrors `spawnSync` in the
/// TypeScript provider). Returns `None` on detached HEAD or if git is
/// unavailable.
fn resolve_branch_with_git(repo_dir: &Path) -> Option<String> {
    let output = Command::new("git")
        .args([
            "--no-optional-locks",
            "symbolic-ref",
            "--quiet",
            "--short",
            "HEAD",
        ])
        .current_dir(repo_dir)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let branch = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if branch.is_empty() {
        None
    } else {
        Some(branch)
    }
}
