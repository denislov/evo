//! Minimal managed worktree creation (ARC-310).
//!
//! A source workspace is materialized as a child worktree through one of two
//! strategies:
//!
//! - Git-linked: `git worktree add` on the source repository, then sync the
//!   source's dirty tracked files and untracked files into the worktree.
//! - Copy fallback: full tree copy (reflink when the filesystem supports it,
//!   plain copy otherwise) for sources that are not git repositories.
//!
//! Ignored files are never synced: dirty sync derives its file list from
//! `git status`, which excludes ignored paths by default.
//!
//! Creation is cancellable; a cancelled or failed create removes the
//! destination so no half-materialized worktree survives.

use std::io;
use std::path::{Path, PathBuf};

use tokio_util::sync::CancellationToken;

const CANCEL_POLL_ENTRIES: usize = 64;
const MAX_STATUS_BYTES: usize = 8 * 1024 * 1024;

/// How much of the source working tree the child worktree should preserve.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkingTreeMode {
    /// Mirror the source working tree: dirty tracked modifications, staged
    /// changes, and untracked files are all carried into the worktree.
    PreserveWorkingTree,
    /// Materialize only the tracked checkout at `HEAD`. Local modifications
    /// and untracked files from the source are not copied.
    CleanTracked,
}

/// How a child worktree was materialized.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorktreeCreationMode {
    /// `git worktree add` on the source repository, plus dirty sync.
    GitLinked,
    /// Full tree copy (reflink where supported) of a non-git source.
    Copy,
}

/// Result of a successful [`WorktreeBuilder::create`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeReport {
    /// Absolute path of the materialized child worktree.
    pub worktree_path: PathBuf,
    /// HEAD commit of the source repository, when the source is git-managed.
    pub commit: Option<String>,
    /// How the worktree was materialized.
    pub creation_mode: WorktreeCreationMode,
    pub files_copied: u64,
    pub dirs_created: u64,
    pub symlinks_copied: u64,
    pub files_deleted: u64,
    /// Non-fatal issues encountered while syncing (e.g. a file that vanished
    /// between status and copy).
    pub issues: Vec<String>,
}

/// Builder for one managed child worktree.
///
/// Heavy work runs inside [`WorktreeBuilder::create`]; async callers should
/// run it with `spawn_blocking`.
#[derive(Clone)]
pub struct WorktreeBuilder {
    source: PathBuf,
    dest: PathBuf,
    mode: WorkingTreeMode,
    cancellation: CancellationToken,
}

impl WorktreeBuilder {
    pub fn new(source: impl Into<PathBuf>, dest: impl Into<PathBuf>) -> Self {
        Self {
            source: source.into(),
            dest: dest.into(),
            mode: WorkingTreeMode::PreserveWorkingTree,
            cancellation: CancellationToken::new(),
        }
    }

    pub fn working_tree_mode(mut self, mode: WorkingTreeMode) -> Self {
        self.mode = mode;
        self
    }

    pub fn cancellation_token(mut self, token: CancellationToken) -> Self {
        self.cancellation = token;
        self
    }

    /// Create the child worktree at `dest`.
    ///
    /// The destination must not exist and must not be inside the source
    /// workspace (a nested destination would be picked up by dirty sync as an
    /// untracked directory). On failure or cancellation the destination is
    /// removed before the error is returned.
    pub fn create(self) -> Result<WorktreeReport, WorktreeError> {
        validate_source(&self.source)?;
        if self.dest.exists() {
            return Err(WorktreeError::DestinationExists {
                path: self.dest.clone(),
            });
        }
        if self.dest.starts_with(&self.source) {
            return Err(WorktreeError::DestinationInsideSource {
                source_root: self.source.clone(),
                dest: self.dest.clone(),
            });
        }
        let result = if source_is_git_repository(&self.source) {
            self.create_git_worktree()
        } else {
            self.create_copy_worktree()
        };
        if result.is_err() && self.dest.exists() {
            // A git-linked worktree is registered in the source repository's
            // worktree metadata; remove the registration before deleting the
            // directory so no orphan record survives.
            if source_is_git_repository(&self.source) {
                let _ = std::process::Command::new("git")
                    .args(["worktree", "remove", "--force"])
                    .arg(&self.dest)
                    .current_dir(&self.source)
                    .output();
            }
            let _ = remove_tree_best_effort(&self.dest);
        }
        result
    }

    fn create_git_worktree(&self) -> Result<WorktreeReport, WorktreeError> {
        check_cancelled(&self.cancellation)?;
        let commit = git_capture(&self.source, &["rev-parse", "HEAD"])
            .ok()
            .map(|output| String::from_utf8_lossy(&output).trim().to_owned());
        run_git(&self.source, &["worktree", "add", "--detach"], &self.dest)?;
        let mut report = WorktreeReport {
            worktree_path: self.dest.clone(),
            commit,
            creation_mode: WorktreeCreationMode::GitLinked,
            files_copied: 0,
            dirs_created: 0,
            symlinks_copied: 0,
            files_deleted: 0,
            issues: Vec::new(),
        };
        if matches!(self.mode, WorkingTreeMode::PreserveWorkingTree) {
            self.sync_working_tree(&mut report)?;
        }
        Ok(report)
    }

    fn create_copy_worktree(&self) -> Result<WorktreeReport, WorktreeError> {
        check_cancelled(&self.cancellation)?;
        if matches!(self.mode, WorkingTreeMode::CleanTracked) {
            return Err(WorktreeError::CopyUnavailable {
                message: "CleanTracked requires a git source".into(),
            });
        }
        let mut report = WorktreeReport {
            worktree_path: self.dest.clone(),
            commit: None,
            creation_mode: WorktreeCreationMode::Copy,
            files_copied: 0,
            dirs_created: 0,
            symlinks_copied: 0,
            files_deleted: 0,
            issues: Vec::new(),
        };
        copy_tree(&self.source, &self.dest, &self.cancellation, &mut report)?;
        Ok(report)
    }

    /// Carry dirty tracked modifications and untracked files from the source
    /// into the (already checked-out) worktree.
    fn sync_working_tree(&self, report: &mut WorktreeReport) -> Result<(), WorktreeError> {
        check_cancelled(&self.cancellation)?;
        let status = git_capture(
            &self.source,
            &["status", "--porcelain", "-z", "--untracked-files=all"],
        )
        .map_err(|error| WorktreeError::GitFailed {
            message: format!("cannot read source status: {error}"),
        })?;
        let entries = parse_status_entries(&status);
        for (index, entry) in entries.iter().enumerate() {
            if index % CANCEL_POLL_ENTRIES == 0 && self.cancellation.is_cancelled() {
                return Err(WorktreeError::Cancelled);
            }
            let relative = Path::new(&entry.path);
            if entry.deleted {
                let target = self.dest.join(relative);
                match remove_path(&target) {
                    Ok(removed) if removed => report.files_deleted += 1,
                    Ok(_) => {}
                    Err(error) => report.issues.push(format!(
                        "cannot delete {} in worktree: {error}",
                        target.display()
                    )),
                }
                continue;
            }
            if let Err(error) = copy_entry(&self.source, &self.dest, relative, report) {
                report
                    .issues
                    .push(format!("cannot sync {}: {error}", relative.display()));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum WorktreeError {
    #[error("source workspace is unavailable: {path}")]
    SourceUnavailable { path: PathBuf },
    #[error("source workspace is not a directory: {path}")]
    SourceNotDirectory { path: PathBuf },
    #[error("destination already exists: {path}")]
    DestinationExists { path: PathBuf },
    #[error("destination {dest} must not be inside the source workspace {source_root}")]
    DestinationInsideSource { source_root: PathBuf, dest: PathBuf },
    #[error("copy worktree unavailable: {message}")]
    CopyUnavailable { message: String },
    #[error("git command failed: {message}")]
    GitFailed { message: String },
    #[error("cannot copy {path}: {message}")]
    CopyFailed { path: PathBuf, message: String },
    #[error("worktree creation was cancelled")]
    Cancelled,
}

struct StatusEntry {
    path: String,
    deleted: bool,
}

/// Parse `git status --porcelain -z` output. Rename/copy pairs contribute
/// their destination path; deletions are flagged so the worktree mirrors them.
fn parse_status_entries(output: &[u8]) -> Vec<StatusEntry> {
    let mut entries = Vec::new();
    let mut tokens = output.split(|byte| *byte == 0);
    while let Some(header) = tokens.next() {
        if header.len() < 3 {
            continue;
        }
        let xy = &header[..2];
        let path = String::from_utf8_lossy(&header[3..]).into_owned();
        if path.is_empty() {
            continue;
        }
        let deleted = xy[0] == b'D' || xy[1] == b'D';
        if xy == b"R " || xy == b"C " {
            if let Some(new_path) = tokens.next() {
                entries.push(StatusEntry {
                    path: String::from_utf8_lossy(new_path).into_owned(),
                    deleted: false,
                });
            }
            continue;
        }
        entries.push(StatusEntry { path, deleted });
    }
    entries
}

fn check_cancelled(token: &CancellationToken) -> Result<(), WorktreeError> {
    if token.is_cancelled() {
        Err(WorktreeError::Cancelled)
    } else {
        Ok(())
    }
}

fn run_git(source: &Path, args: &[&str], dest: &Path) -> Result<(), WorktreeError> {
    let output = std::process::Command::new("git")
        .args(args)
        .arg(dest)
        .current_dir(source)
        .output()
        .map_err(|error| WorktreeError::GitFailed {
            message: format!("cannot run git: {error}"),
        })?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(WorktreeError::GitFailed {
            message: format!(
                "git {} failed: {}",
                args.join(" "),
                stderr.trim().lines().last().unwrap_or("unknown error")
            ),
        });
    }
    Ok(())
}

fn git_capture(source: &Path, args: &[&str]) -> Result<Vec<u8>, String> {
    let output = std::process::Command::new("git")
        .args(args)
        .current_dir(source)
        .output()
        .map_err(|error| format!("cannot run git: {error}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).into_owned());
    }
    if output.stdout.len() > MAX_STATUS_BYTES {
        return Err("git output exceeds the status budget".into());
    }
    Ok(output.stdout)
}

fn validate_source(source: &Path) -> Result<(), WorktreeError> {
    let metadata =
        std::fs::symlink_metadata(source).map_err(|_| WorktreeError::SourceUnavailable {
            path: source.to_path_buf(),
        })?;
    if !metadata.is_dir() {
        return Err(WorktreeError::SourceNotDirectory {
            path: source.to_path_buf(),
        });
    }
    Ok(())
}

fn source_is_git_repository(source: &Path) -> bool {
    let git_dir = source.join(".git");
    match std::fs::symlink_metadata(&git_dir) {
        Ok(_) => true,
        Err(error) if error.kind() == io::ErrorKind::NotFound => false,
        Err(_) => false,
    }
}

fn copy_tree(
    source: &Path,
    dest: &Path,
    cancellation: &CancellationToken,
    report: &mut WorktreeReport,
) -> Result<(), WorktreeError> {
    let mut pending = vec![PathBuf::from("")];
    let mut visited = 0usize;
    while let Some(relative) = pending.pop() {
        if visited.is_multiple_of(CANCEL_POLL_ENTRIES) && cancellation.is_cancelled() {
            return Err(WorktreeError::Cancelled);
        }
        visited += 1;
        let source_entry = source.join(&relative);
        let metadata = std::fs::symlink_metadata(&source_entry).map_err(|error| {
            WorktreeError::CopyFailed {
                path: source_entry.clone(),
                message: format!("cannot inspect source entry: {error}"),
            }
        })?;
        if metadata.file_type().is_symlink() {
            copy_symlink(&source_entry, &dest.join(&relative), report)?;
            continue;
        }
        if metadata.is_dir() {
            let target = dest.join(&relative);
            std::fs::create_dir_all(&target).map_err(|error| WorktreeError::CopyFailed {
                path: target.clone(),
                message: format!("cannot create directory: {error}"),
            })?;
            report.dirs_created += 1;
            let read_dir =
                std::fs::read_dir(&source_entry).map_err(|error| WorktreeError::CopyFailed {
                    path: source_entry.clone(),
                    message: format!("cannot read directory: {error}"),
                })?;
            for entry in read_dir.flatten() {
                pending.push(relative.join(entry.file_name()));
            }
        } else {
            copy_file(&source_entry, &dest.join(&relative), report)?;
        }
    }
    Ok(())
}

fn copy_entry(
    source_root: &Path,
    dest_root: &Path,
    relative: &Path,
    report: &mut WorktreeReport,
) -> Result<(), String> {
    let source_entry = source_root.join(relative);
    let metadata = std::fs::symlink_metadata(&source_entry)
        .map_err(|error| format!("cannot inspect source entry: {error}"))?;
    if metadata.file_type().is_symlink() {
        copy_symlink(&source_entry, &dest_root.join(relative), report)
            .map_err(|error| error.to_string())
    } else if metadata.is_dir() {
        std::fs::create_dir_all(dest_root.join(relative))
            .map_err(|error| format!("cannot create directory: {error}"))?;
        report.dirs_created += 1;
        Ok(())
    } else {
        copy_file(&source_entry, &dest_root.join(relative), report)
            .map_err(|error| error.to_string())
    }
}

fn copy_file(source: &Path, dest: &Path, report: &mut WorktreeReport) -> Result<(), WorktreeError> {
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(|error| WorktreeError::CopyFailed {
            path: parent.to_path_buf(),
            message: format!("cannot create parent directory: {error}"),
        })?;
    }
    // reflink_or_copy fails with AlreadyExists when the target exists (e.g. a
    // file already checked out by `git worktree add`), so clear it first.
    match std::fs::symlink_metadata(dest) {
        Ok(metadata) if metadata.file_type().is_dir() => {
            return Err(WorktreeError::CopyFailed {
                path: source.to_path_buf(),
                message: format!("copy target is an existing directory: {}", dest.display()),
            });
        }
        Ok(_) => std::fs::remove_file(dest).map_err(|error| WorktreeError::CopyFailed {
            path: source.to_path_buf(),
            message: format!("cannot replace existing copy target: {error}"),
        })?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(WorktreeError::CopyFailed {
                path: source.to_path_buf(),
                message: format!("cannot inspect copy target: {error}"),
            });
        }
    }
    reflink_copy::reflink_or_copy(source, dest).map_err(|error| WorktreeError::CopyFailed {
        path: source.to_path_buf(),
        message: format!("cannot copy: {error}"),
    })?;
    report.files_copied += 1;
    Ok(())
}

#[cfg(unix)]
fn copy_symlink(
    source: &Path,
    dest: &Path,
    report: &mut WorktreeReport,
) -> Result<(), WorktreeError> {
    use std::os::unix::fs::symlink;
    let target = std::fs::read_link(source).map_err(|error| WorktreeError::CopyFailed {
        path: source.to_path_buf(),
        message: format!("cannot read symlink: {error}"),
    })?;
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(|error| WorktreeError::CopyFailed {
            path: parent.to_path_buf(),
            message: format!("cannot create parent directory: {error}"),
        })?;
    }
    if dest.symlink_metadata().is_ok() {
        std::fs::remove_file(dest).map_err(|error| WorktreeError::CopyFailed {
            path: dest.to_path_buf(),
            message: format!("cannot replace existing symlink target: {error}"),
        })?;
    }
    symlink(&target, dest).map_err(|error| WorktreeError::CopyFailed {
        path: source.to_path_buf(),
        message: format!("cannot create symlink: {error}"),
    })?;
    report.symlinks_copied += 1;
    Ok(())
}

#[cfg(not(unix))]
fn copy_symlink(
    source: &Path,
    _dest: &Path,
    report: &mut WorktreeReport,
) -> Result<(), WorktreeError> {
    report.issues.push(format!(
        "symlink {} is not replicated on this platform",
        source.display()
    ));
    Ok(())
}

fn remove_path(target: &Path) -> Result<bool, String> {
    match std::fs::symlink_metadata(target) {
        Ok(metadata) if metadata.is_dir() => {
            std::fs::remove_dir_all(target).map_err(|error| error.to_string())?;
            Ok(true)
        }
        Ok(_) => {
            std::fs::remove_file(target).map_err(|error| error.to_string())?;
            Ok(true)
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.to_string()),
    }
}

fn remove_tree_best_effort(target: &Path) -> Result<(), String> {
    if let Ok(metadata) = std::fs::symlink_metadata(target) {
        if metadata.is_dir() {
            std::fs::remove_dir_all(target).map_err(|error| error.to_string())
        } else {
            std::fs::remove_file(target).map_err(|error| error.to_string())
        }
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests;
