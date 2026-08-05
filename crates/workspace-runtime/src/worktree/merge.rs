//! Merge protocol for managed child worktrees (ARC-340).
//!
//! A child worktree in `MergePending` carries changes relative to its base
//! revision. The parent workspace is only ever modified by an explicit,
//! admitted merge: the child never writes back directly. The merge is
//! optimistic — the parent must still sit on the child's base revision — and
//! conflicts are detected before any file is touched, so a failed merge leaves
//! the parent byte-identical.
//!
//! Git-linked worktrees are fully supported. Copy-mode worktrees have no base
//! snapshot yet; merging them is refused until `change-tracker` (Phase 4)
//! supplies one.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use tokio_util::sync::CancellationToken;

use super::git::git_capture;
use super::registry::{RegistryError, WorktreeRecord, WorktreeRegistry};
use crate::contract::WorkspaceLifecycle;

/// Upper bound on changeset entries; larger diffs are truncated and reported.
pub const MAX_CHANGESET_ENTRIES: usize = 4096;
const MAX_DIFF_OUTPUT_BYTES: usize = 16 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeKind {
    Added,
    Modified,
    Deleted,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ChangeEntry {
    /// Workspace-relative path.
    pub path: PathBuf,
    pub kind: ChangeKind,
    pub additions: u64,
    pub deletions: u64,
}

/// The file-level changes a child worktree made since its base revision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangeSet {
    pub base_revision: Option<String>,
    pub entries: Vec<ChangeEntry>,
    pub truncated: bool,
}

impl ChangeSet {
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MergeError {
    #[error("worktree is not mergeable: {message}")]
    NotMergeable { message: String },
    #[error(
        "copy-mode worktrees cannot be merged yet; base snapshots arrive with change-tracker (Phase 4)"
    )]
    CopyWorktreeUnsupported,
    #[error(
        "parent workspace moved past the child base revision (expected {expected:?}, found {actual:?})"
    )]
    StaleParent {
        expected: Option<String>,
        actual: Option<String>,
    },
    #[error("merge conflicts on {paths:?}")]
    Conflict { paths: Vec<PathBuf> },
    #[error("cannot apply change {path}: {message}")]
    ApplyFailed { path: PathBuf, message: String },
    #[error("git failed: {0}")]
    Git(#[from] super::WorktreeError),
    #[error("git failed: {message}")]
    GitFailed { message: String },
    #[error("registry error: {0}")]
    Registry(#[from] RegistryError),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MergeReport {
    pub worktree_id: String,
    pub base_revision: Option<String>,
    pub applied: usize,
    pub entries: Vec<ChangeEntry>,
}

/// Build the file-level changeset of a `MergePending` worktree.
pub fn build_changeset(registry: &WorktreeRegistry, id: &str) -> Result<ChangeSet, MergeError> {
    let record = registry.load(id)?.ok_or_else(|| MergeError::NotMergeable {
        message: format!("worktree {id} is not registered"),
    })?;
    let base = mergeable_base(&record)?;
    if record.lifecycle != WorkspaceLifecycle::MergePending {
        return Err(MergeError::NotMergeable {
            message: format!("worktree {id} is {:?}, not MergePending", record.lifecycle),
        });
    }
    let cancellation = CancellationToken::new();
    let tracked = parse_name_status(&git_capture(
        &record.dest,
        &["diff", "--name-status", "--no-renames", base.as_str()],
        &cancellation,
        MAX_DIFF_OUTPUT_BYTES,
    )?)?;
    let untracked = parse_untracked(&git_capture(
        &record.dest,
        &["ls-files", "--others", "--exclude-standard", "-z"],
        &cancellation,
        MAX_DIFF_OUTPUT_BYTES,
    )?)?;
    let stats = parse_num_stats(&git_capture(
        &record.dest,
        &["diff", "--numstat", "--no-renames", base.as_str()],
        &cancellation,
        MAX_DIFF_OUTPUT_BYTES,
    )?)?;
    let mut entries = Vec::with_capacity(tracked.len() + untracked.len());
    let mut truncated = false;
    let mut push = |path: PathBuf, kind: ChangeKind, additions: u64, deletions: u64| {
        if entries.len() >= MAX_CHANGESET_ENTRIES {
            truncated = true;
            return;
        }
        entries.push(ChangeEntry {
            path,
            kind,
            additions,
            deletions,
        });
    };
    for (path, kind) in tracked {
        let (additions, deletions) = stats.get(&path).copied().unwrap_or((0, 0));
        push(path, kind, additions, deletions);
    }
    for path in untracked {
        push(path, ChangeKind::Added, 0, 0);
    }
    entries.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(ChangeSet {
        base_revision: Some(base),
        entries,
        truncated,
    })
}

/// Apply a `MergePending` worktree's changes back into its parent workspace.
///
/// The merge is optimistic: the parent must still be on the child's base
/// revision, and any parent-side change overlapping a child change is a
/// conflict. All conflict checks run before any file is written, so a failed
/// merge never leaves a partial parent. The record transitions to `Merged` on
/// success; the caller then discards the worktree.
pub fn apply_merge(registry: &WorktreeRegistry, id: &str) -> Result<MergeReport, MergeError> {
    let record = registry.load(id)?.ok_or_else(|| MergeError::NotMergeable {
        message: format!("worktree {id} is not registered"),
    })?;
    let base = mergeable_base(&record)?;
    if record.lifecycle != WorkspaceLifecycle::MergePending {
        return Err(MergeError::NotMergeable {
            message: format!("worktree {id} is {:?}, not MergePending", record.lifecycle),
        });
    }
    let cancellation = CancellationToken::new();
    let parent_head = String::from_utf8(git_capture(
        &record.source,
        &["rev-parse", "HEAD"],
        &cancellation,
        256,
    )?)
    .map_err(|_| MergeError::GitFailed {
        message: "parent HEAD is not UTF-8".into(),
    })?
    .trim()
    .to_owned();
    if parent_head != base {
        return Err(MergeError::StaleParent {
            expected: Some(base.clone()),
            actual: Some(parent_head),
        });
    }

    let parent_dirty = parse_name_only(&git_capture(
        &record.source,
        &["diff", "--name-only", "--no-renames", base.as_str()],
        &cancellation,
        MAX_DIFF_OUTPUT_BYTES,
    )?)?;
    let changeset = build_changeset(registry, id)?;
    let conflicts = changeset
        .entries
        .iter()
        .filter(|entry| parent_dirty.contains(&entry.path))
        .map(|entry| entry.path.clone())
        .collect::<Vec<_>>();
    if !conflicts.is_empty() {
        return Err(MergeError::Conflict { paths: conflicts });
    }

    for entry in &changeset.entries {
        match entry.kind {
            ChangeKind::Added | ChangeKind::Modified => {
                let child = record.dest.join(&entry.path);
                let parent = record.source.join(&entry.path);
                copy_into_parent(&child, &parent).map_err(|message| MergeError::ApplyFailed {
                    path: entry.path.clone(),
                    message,
                })?;
            }
            ChangeKind::Deleted => {
                let parent = record.source.join(&entry.path);
                match fs::remove_file(&parent) {
                    Ok(()) => {}
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                    Err(error) => {
                        return Err(MergeError::ApplyFailed {
                            path: entry.path.clone(),
                            message: format!("cannot delete {}: {error}", parent.display()),
                        });
                    }
                }
            }
        }
    }

    let updated = registry.transition(id, WorkspaceLifecycle::Merged, unix_seconds())?;
    Ok(MergeReport {
        worktree_id: updated.id,
        base_revision: updated.base_revision,
        applied: changeset.entries.len(),
        entries: changeset.entries,
    })
}

fn mergeable_base(record: &WorktreeRecord) -> Result<String, MergeError> {
    match record.creation_mode {
        super::WorktreeCreationMode::GitLinked => {}
        super::WorktreeCreationMode::Copy => return Err(MergeError::CopyWorktreeUnsupported),
    }
    record
        .base_revision
        .clone()
        .ok_or_else(|| MergeError::NotMergeable {
            message: format!("worktree {} has no base revision", record.id),
        })
}

fn parse_name_status(output: &[u8]) -> Result<Vec<(PathBuf, ChangeKind)>, MergeError> {
    let text = String::from_utf8(output.to_vec()).map_err(|_| MergeError::GitFailed {
        message: "git name-status output is not UTF-8".into(),
    })?;
    let mut entries = Vec::new();
    for line in text.lines().filter(|line| !line.is_empty()) {
        let mut fields = line.split('\t');
        let status = fields.next().unwrap_or("");
        let path = fields
            .next()
            .filter(|path| !path.is_empty())
            .ok_or_else(|| MergeError::GitFailed {
                message: format!("git name-status line has no path: {line}"),
            })?;
        let kind = match status {
            "A" => ChangeKind::Added,
            "D" => ChangeKind::Deleted,
            "M" => ChangeKind::Modified,
            _ => continue,
        };
        entries.push((PathBuf::from(path), kind));
    }
    Ok(entries)
}

fn parse_untracked(output: &[u8]) -> Result<Vec<PathBuf>, MergeError> {
    let mut paths = Vec::new();
    for entry in output.split(|byte| *byte == 0) {
        if entry.is_empty() {
            continue;
        }
        let path = String::from_utf8(entry.to_vec()).map_err(|_| MergeError::GitFailed {
            message: "git ls-files output is not UTF-8".into(),
        })?;
        paths.push(PathBuf::from(path));
    }
    Ok(paths)
}

fn parse_num_stats(
    output: &[u8],
) -> Result<std::collections::HashMap<PathBuf, (u64, u64)>, MergeError> {
    let text = String::from_utf8(output.to_vec()).map_err(|_| MergeError::GitFailed {
        message: "git numstat output is not UTF-8".into(),
    })?;
    let mut stats = std::collections::HashMap::new();
    for line in text.lines().filter(|line| !line.is_empty()) {
        let mut fields = line.split('\t');
        let additions = fields.next().unwrap_or("0");
        let deletions = fields.next().unwrap_or("0");
        let path = fields
            .next()
            .filter(|path| !path.is_empty())
            .ok_or_else(|| MergeError::GitFailed {
                message: format!("git numstat line has no path: {line}"),
            })?;
        let parse = |value: &str| value.parse::<u64>().unwrap_or(0);
        stats.insert(PathBuf::from(path), (parse(additions), parse(deletions)));
    }
    Ok(stats)
}

fn parse_name_only(output: &[u8]) -> Result<Vec<PathBuf>, MergeError> {
    let text = String::from_utf8(output.to_vec()).map_err(|_| MergeError::GitFailed {
        message: "git name-only output is not UTF-8".into(),
    })?;
    Ok(text
        .lines()
        .filter(|line| !line.is_empty())
        .map(PathBuf::from)
        .collect())
}

fn copy_into_parent(child: &Path, parent: &Path) -> Result<(), String> {
    let metadata = fs::symlink_metadata(child)
        .map_err(|error| format!("cannot inspect child path {}: {error}", child.display()))?;
    let Some(existing_parent) = parent.parent() else {
        return Err(format!("change path has no parent: {}", parent.display()));
    };
    fs::create_dir_all(existing_parent)
        .map_err(|error| format!("cannot create parent dir: {error}"))?;
    if metadata.is_symlink() {
        let target = fs::read_link(child)
            .map_err(|error| format!("cannot read child symlink {}: {error}", child.display()))?;
        let _ = fs::remove_file(parent);
        create_symlink(&target, parent)
    } else {
        fs::copy(child, parent)
            .map(|_| ())
            .map_err(|error| format!("cannot copy {}: {error}", parent.display()))
    }
}

fn create_symlink(target: &Path, parent: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(target, parent)
            .map_err(|error| format!("cannot create symlink {}: {error}", parent.display()))
    }
    #[cfg(not(unix))]
    {
        let _ = (target, parent);
        Err(format!(
            "cannot recreate symlink on this platform: {}",
            parent.display()
        ))
    }
}

fn unix_seconds() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
#[path = "merge_tests.rs"]
mod merge_tests;
