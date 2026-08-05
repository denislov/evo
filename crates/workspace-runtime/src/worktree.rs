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

use std::ffi::OsString;
use std::io;
use std::path::{Component, Path, PathBuf};

use tokio_util::sync::CancellationToken;

use crate::contract::{
    WorkspaceHandle, WorkspaceId, WorkspaceIdentityError, WorkspaceKind, WorkspaceLease,
    WorkspaceLeaseError, WorkspaceLifecycle,
};

const CANCEL_POLL_ENTRIES: usize = 64;
const MAX_STATUS_BYTES: usize = 8 * 1024 * 1024;
const MAX_REVISION_BYTES: usize = 1024;

mod git;
pub(crate) mod registry;
use git::{git_capture, run_git};

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
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorktreeCreationMode {
    /// `git worktree add` on the source repository, plus dirty sync.
    GitLinked,
    /// Full tree copy (reflink where supported) of a non-git source.
    Copy,
}

/// Result of a successful [`WorktreeBuilder::create`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeReport {
    /// HEAD commit of the source repository, when the source is git-managed.
    commit: Option<String>,
    /// How the worktree was materialized.
    creation_mode: WorktreeCreationMode,
    files_copied: u64,
    dirs_created: u64,
    symlinks_copied: u64,
    files_deleted: u64,
}

impl WorktreeReport {
    pub fn commit(&self) -> Option<&str> {
        self.commit.as_deref()
    }

    pub const fn creation_mode(&self) -> WorktreeCreationMode {
        self.creation_mode
    }

    pub const fn files_copied(&self) -> u64 {
        self.files_copied
    }

    pub const fn dirs_created(&self) -> u64 {
        self.dirs_created
    }

    pub const fn symlinks_copied(&self) -> u64 {
        self.symlinks_copied
    }

    pub const fn files_deleted(&self) -> u64 {
        self.files_deleted
    }
}

/// A materialized child workspace whose report and lifecycle identity cannot
/// drift apart.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagedWorktree {
    lease: WorkspaceLease,
    report: WorktreeReport,
}

impl ManagedWorktree {
    pub fn lease(&self) -> &WorkspaceLease {
        &self.lease
    }

    pub fn report(&self) -> &WorktreeReport {
        &self.report
    }

    pub fn root(&self) -> &Path {
        self.lease.handle().root()
    }

    pub const fn creation_mode(&self) -> WorktreeCreationMode {
        self.report.creation_mode
    }

    pub fn transition(&mut self, next: WorkspaceLifecycle) -> Result<(), WorkspaceLeaseError> {
        self.lease.transition(next)
    }
}

/// Builder for one managed child worktree.
///
/// Heavy work runs inside [`WorktreeBuilder::create`]; async callers should
/// run it with `spawn_blocking`.
#[derive(Clone)]
pub struct WorktreeBuilder {
    source: WorkspaceHandle,
    dest: PathBuf,
    owner_operation: String,
    parent_session: Option<String>,
    worktree_id: Option<WorkspaceId>,
    mode: WorkingTreeMode,
    cancellation: CancellationToken,
}

impl WorktreeBuilder {
    pub fn new(
        source: WorkspaceHandle,
        dest: impl Into<PathBuf>,
        owner_operation: impl Into<String>,
    ) -> Self {
        Self {
            source,
            dest: dest.into(),
            owner_operation: owner_operation.into(),
            parent_session: None,
            worktree_id: None,
            mode: WorkingTreeMode::PreserveWorkingTree,
            cancellation: CancellationToken::new(),
        }
    }

    pub fn parent_session(mut self, parent_session: Option<String>) -> Self {
        self.parent_session = parent_session;
        self
    }

    /// Pin the managed worktree identity. When omitted, the identity is
    /// derived from the destination directory name (with a `ManagedChild`
    /// prefix). The registry uses this to keep the record id, the directory
    /// name, and the handle id aligned.
    pub fn worktree_id(mut self, id: WorkspaceId) -> Self {
        self.worktree_id = Some(id);
        self
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
    /// untracked directory). On failure or cancellation, any destination
    /// materialized by this attempt is removed before the error is returned.
    pub fn create(self) -> Result<ManagedWorktree, WorktreeError> {
        if self.owner_operation.is_empty() {
            return Err(WorkspaceLeaseError::MissingOwner.into());
        }
        check_cancelled(&self.cancellation)?;
        let paths = validate_paths(self.source.root(), &self.dest)?;
        let git_linked = source_is_git_repository(&paths.source);
        let mut git_add_attempted = false;
        let mut materialization_attempted = false;
        let result = if git_linked {
            self.create_git_worktree(
                &paths,
                &mut git_add_attempted,
                &mut materialization_attempted,
            )
        } else {
            self.create_copy_worktree(&paths, &mut materialization_attempted)
        }
        .and_then(|report| {
            check_cancelled(&self.cancellation)?;
            self.finish_managed_worktree(&paths.dest, report)
        });

        match result {
            Ok(managed) => Ok(managed),
            Err(cause) if !materialization_attempted => Err(cause),
            Err(cause) => match cleanup_failed_creation(&paths, git_add_attempted) {
                Ok(()) => Err(cause),
                Err(cleanup_issues) => Err(WorktreeError::CleanupFailed {
                    cause: Box::new(cause),
                    cleanup_issues,
                }),
            },
        }
    }

    fn finish_managed_worktree(
        &self,
        dest: &Path,
        report: WorktreeReport,
    ) -> Result<ManagedWorktree, WorktreeError> {
        let handle = match &self.worktree_id {
            Some(id) => {
                WorkspaceHandle::with_explicit_id(id.clone(), WorkspaceKind::ManagedChild, dest)?
            }
            None => {
                // The destination's directory name is the worktree identity
                // when no explicit id was pinned.
                let value = dest
                    .file_name()
                    .and_then(|name| name.to_str())
                    .ok_or_else(|| WorktreeError::InvalidDestinationName {
                        message: format!(
                            "worktree destination has no valid id name: {}",
                            dest.display()
                        ),
                    })?;
                WorkspaceHandle::with_user_id(WorkspaceKind::ManagedChild, value, dest)?
            }
        };
        let mut lease = WorkspaceLease::new(
            handle,
            self.owner_operation.clone(),
            self.parent_session.clone(),
            report.commit.clone(),
        )?;
        lease.transition(WorkspaceLifecycle::Ready)?;
        Ok(ManagedWorktree { lease, report })
    }

    fn create_git_worktree(
        &self,
        paths: &ValidatedPaths,
        git_add_attempted: &mut bool,
        materialization_attempted: &mut bool,
    ) -> Result<WorktreeReport, WorktreeError> {
        check_cancelled(&self.cancellation)?;
        let commit = String::from_utf8(git_capture(
            &paths.source,
            &["rev-parse", "HEAD"],
            &self.cancellation,
            MAX_REVISION_BYTES,
        )?)
        .map_err(|_| WorktreeError::GitFailed {
            message: "git rev-parse returned a non-UTF-8 revision".into(),
        })?
        .trim()
        .to_owned();
        *git_add_attempted = true;
        *materialization_attempted = true;
        run_git(
            &paths.source,
            &["worktree", "add", "--detach"],
            Some(&paths.dest),
            &self.cancellation,
        )?;
        check_cancelled(&self.cancellation)?;
        let mut report = WorktreeReport {
            commit: Some(commit),
            creation_mode: WorktreeCreationMode::GitLinked,
            files_copied: 0,
            dirs_created: 0,
            symlinks_copied: 0,
            files_deleted: 0,
        };
        if matches!(self.mode, WorkingTreeMode::PreserveWorkingTree) {
            self.sync_working_tree(paths, &mut report)?;
        }
        Ok(report)
    }

    fn create_copy_worktree(
        &self,
        paths: &ValidatedPaths,
        materialization_attempted: &mut bool,
    ) -> Result<WorktreeReport, WorktreeError> {
        check_cancelled(&self.cancellation)?;
        if matches!(self.mode, WorkingTreeMode::CleanTracked) {
            return Err(WorktreeError::CopyUnavailable {
                message: "CleanTracked requires a git source".into(),
            });
        }
        let mut report = WorktreeReport {
            commit: None,
            creation_mode: WorktreeCreationMode::Copy,
            files_copied: 0,
            dirs_created: 0,
            symlinks_copied: 0,
            files_deleted: 0,
        };
        *materialization_attempted = true;
        copy_tree(&paths.source, &paths.dest, &self.cancellation, &mut report)?;
        Ok(report)
    }

    /// Carry dirty tracked modifications and untracked files from the source
    /// into the (already checked-out) worktree.
    fn sync_working_tree(
        &self,
        paths: &ValidatedPaths,
        report: &mut WorktreeReport,
    ) -> Result<(), WorktreeError> {
        check_cancelled(&self.cancellation)?;
        let status = git_capture(
            &paths.source,
            &["status", "--porcelain", "-z", "--untracked-files=all"],
            &self.cancellation,
            MAX_STATUS_BYTES,
        )?;
        let entries = parse_status_entries(&status)?;

        // Apply removals first so swaps and directory/file type changes cannot
        // overwrite a newly copied destination later in the same status set.
        for (index, entry) in entries.iter().enumerate() {
            if index.is_multiple_of(CANCEL_POLL_ENTRIES) {
                check_cancelled(&self.cancellation)?;
            }
            if let Some(previous_path) = &entry.previous_path
                && entry.renamed
            {
                remove_synced_path(&paths.dest, previous_path, report)?;
            }
            if entry.deleted {
                remove_synced_path(&paths.dest, &entry.path, report)?;
            }
        }
        for (index, entry) in entries.iter().filter(|entry| !entry.deleted).enumerate() {
            if index.is_multiple_of(CANCEL_POLL_ENTRIES) {
                check_cancelled(&self.cancellation)?;
            }
            copy_entry(&paths.source, &paths.dest, &entry.path, report).map_err(|message| {
                WorktreeError::CopyFailed {
                    path: paths.source.join(&entry.path),
                    message,
                }
            })?;
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
    #[error("destination must be an absolute path: {path}")]
    DestinationMustBeAbsolute { path: PathBuf },
    #[error("destination cannot be resolved safely: {path}: {message}")]
    DestinationUnavailable { path: PathBuf, message: String },
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
    #[error("worktree destination name is invalid: {message}")]
    InvalidDestinationName { message: String },
    #[error("workspace identity is invalid: {0}")]
    Identity(#[from] WorkspaceIdentityError),
    #[error("workspace lease is invalid: {0}")]
    Lease(#[from] WorkspaceLeaseError),
    #[error("worktree creation failed ({cause}); cleanup was incomplete: {cleanup_issues:?}")]
    CleanupFailed {
        cause: Box<WorktreeError>,
        cleanup_issues: Vec<String>,
    },
}

#[derive(Debug)]
struct StatusEntry {
    path: PathBuf,
    previous_path: Option<PathBuf>,
    renamed: bool,
    deleted: bool,
}

/// Parse `git status --porcelain -z` output. Rename/copy pairs contribute
/// their destination path; deletions are flagged so the worktree mirrors them.
fn parse_status_entries(output: &[u8]) -> Result<Vec<StatusEntry>, WorktreeError> {
    let mut entries = Vec::new();
    let mut tokens = output.split(|byte| *byte == 0);
    while let Some(header) = tokens.next() {
        if header.is_empty() {
            continue;
        }
        if header.len() < 3 {
            return Err(invalid_status("entry header is shorter than three bytes"));
        }
        if header[2] != b' ' {
            return Err(invalid_status("entry header is missing its path separator"));
        }
        let xy = &header[..2];
        let path = status_path(&header[3..])?;
        let deleted = xy[0] == b'D' || xy[1] == b'D';
        let renamed = xy.contains(&b'R');
        let copied = xy.contains(&b'C');
        let previous_path = if renamed || copied {
            let previous = tokens
                .next()
                .ok_or_else(|| invalid_status("rename/copy entry is missing its source path"))?;
            Some(status_path(previous)?)
        } else {
            None
        };
        entries.push(StatusEntry {
            path,
            previous_path,
            renamed,
            deleted: deleted && !renamed && !copied,
        });
    }
    Ok(entries)
}

fn invalid_status(message: impl Into<String>) -> WorktreeError {
    WorktreeError::GitFailed {
        message: format!("invalid git status output: {}", message.into()),
    }
}

fn status_path(bytes: &[u8]) -> Result<PathBuf, WorktreeError> {
    let path = PathBuf::from(git_os_string(bytes)?);
    if path.as_os_str().is_empty()
        || path.is_absolute()
        || !path
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
    {
        return Err(invalid_status(format!(
            "path is not a bounded repository-relative path: {}",
            path.display()
        )));
    }
    Ok(path)
}

#[cfg(unix)]
fn git_os_string(bytes: &[u8]) -> Result<OsString, WorktreeError> {
    use std::os::unix::ffi::OsStringExt;
    Ok(OsString::from_vec(bytes.to_vec()))
}

#[cfg(not(unix))]
fn git_os_string(bytes: &[u8]) -> Result<OsString, WorktreeError> {
    String::from_utf8(bytes.to_vec())
        .map(OsString::from)
        .map_err(|_| invalid_status("path is not valid UTF-8 on this platform"))
}

fn check_cancelled(token: &CancellationToken) -> Result<(), WorktreeError> {
    if token.is_cancelled() {
        Err(WorktreeError::Cancelled)
    } else {
        Ok(())
    }
}

struct ValidatedPaths {
    source: PathBuf,
    dest: PathBuf,
}

fn validate_paths(source: &Path, dest: &Path) -> Result<ValidatedPaths, WorktreeError> {
    let source_metadata =
        std::fs::metadata(source).map_err(|_| WorktreeError::SourceUnavailable {
            path: source.to_path_buf(),
        })?;
    if !source_metadata.is_dir() {
        return Err(WorktreeError::SourceNotDirectory {
            path: source.to_path_buf(),
        });
    }
    let source = std::fs::canonicalize(source).map_err(|_| WorktreeError::SourceUnavailable {
        path: source.to_path_buf(),
    })?;
    if !dest.is_absolute() {
        return Err(WorktreeError::DestinationMustBeAbsolute {
            path: dest.to_path_buf(),
        });
    }
    let dest = normalize_absolute_path(dest)?;
    match std::fs::symlink_metadata(&dest) {
        Ok(_) => {
            return Err(WorktreeError::DestinationExists { path: dest });
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(WorktreeError::DestinationUnavailable {
                path: dest,
                message: error.to_string(),
            });
        }
    }

    let (existing_ancestor, missing_suffix) = nearest_existing_ancestor(&dest)?;
    let canonical_ancestor = std::fs::canonicalize(&existing_ancestor).map_err(|error| {
        WorktreeError::DestinationUnavailable {
            path: existing_ancestor.clone(),
            message: error.to_string(),
        }
    })?;
    let resolved_dest = canonical_ancestor.join(missing_suffix);
    if resolved_dest.starts_with(&source) {
        return Err(WorktreeError::DestinationInsideSource {
            source_root: source,
            dest: resolved_dest,
        });
    }
    Ok(ValidatedPaths {
        source,
        dest: resolved_dest,
    })
}

fn normalize_absolute_path(path: &Path) -> Result<PathBuf, WorktreeError> {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir => normalized.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                if !normalized.pop() {
                    return Err(WorktreeError::DestinationUnavailable {
                        path: path.to_path_buf(),
                        message: "path escapes its filesystem root".into(),
                    });
                }
            }
            Component::Normal(part) => normalized.push(part),
        }
    }
    Ok(normalized)
}

fn nearest_existing_ancestor(dest: &Path) -> Result<(PathBuf, PathBuf), WorktreeError> {
    let mut ancestor = dest.to_path_buf();
    let mut suffix = PathBuf::new();
    loop {
        match std::fs::symlink_metadata(&ancestor) {
            Ok(_) => return Ok((ancestor, suffix)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let name =
                    ancestor
                        .file_name()
                        .ok_or_else(|| WorktreeError::DestinationUnavailable {
                            path: dest.to_path_buf(),
                            message: "destination has no existing filesystem ancestor".into(),
                        })?;
                let mut next_suffix = PathBuf::from(name);
                next_suffix.push(&suffix);
                suffix = next_suffix;
                ancestor = ancestor
                    .parent()
                    .ok_or_else(|| WorktreeError::DestinationUnavailable {
                        path: dest.to_path_buf(),
                        message: "destination has no parent".into(),
                    })?
                    .to_path_buf();
            }
            Err(error) => {
                return Err(WorktreeError::DestinationUnavailable {
                    path: ancestor,
                    message: error.to_string(),
                });
            }
        }
    }
}

fn source_is_git_repository(source: &Path) -> bool {
    std::fs::symlink_metadata(source.join(".git")).is_ok()
}

fn cleanup_failed_creation(
    paths: &ValidatedPaths,
    git_add_attempted: bool,
) -> Result<(), Vec<String>> {
    let mut issues = Vec::new();
    if git_add_attempted {
        let cleanup_token = CancellationToken::new();
        let _ = run_git(
            &paths.source,
            &["worktree", "remove", "--force"],
            Some(&paths.dest),
            &cleanup_token,
        );
    }
    if let Err(message) = remove_path(&paths.dest) {
        issues.push(format!(
            "cannot remove destination {}: {message}",
            paths.dest.display()
        ));
    }
    if git_add_attempted {
        let cleanup_token = CancellationToken::new();
        if let Err(error) = run_git(
            &paths.source,
            &["worktree", "prune", "--expire", "now"],
            None,
            &cleanup_token,
        ) {
            issues.push(format!("cannot prune git worktree metadata: {error}"));
        }
        match git_worktree_registration_exists(&paths.source, &paths.dest) {
            Ok(true) => issues.push(format!(
                "git worktree registration still exists for {}",
                paths.dest.display()
            )),
            Ok(false) => {}
            Err(error) => issues.push(format!("cannot verify git worktree cleanup: {error}")),
        }
    }
    match std::fs::symlink_metadata(&paths.dest) {
        Ok(_) => issues.push(format!(
            "destination still exists after cleanup: {}",
            paths.dest.display()
        )),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => issues.push(format!(
            "cannot verify destination cleanup {}: {error}",
            paths.dest.display()
        )),
    }
    if issues.is_empty() {
        Ok(())
    } else {
        Err(issues)
    }
}

pub(crate) fn git_worktree_registration_exists(
    source: &Path,
    dest: &Path,
) -> Result<bool, WorktreeError> {
    let output = git_capture(
        source,
        &["worktree", "list", "--porcelain", "-z"],
        &CancellationToken::new(),
        MAX_STATUS_BYTES,
    )?;
    for token in output.split(|byte| *byte == 0) {
        if let Some(path) = token.strip_prefix(b"worktree ") {
            let registered = PathBuf::from(git_os_string(path)?);
            if registered == dest {
                return Ok(true);
            }
        }
    }
    Ok(false)
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
            for entry in read_dir {
                let entry = entry.map_err(|error| WorktreeError::CopyFailed {
                    path: source_entry.clone(),
                    message: format!("cannot read directory entry: {error}"),
                })?;
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
    // reflink_or_copy requires a missing target. Removing any checked-out
    // file/directory first also supports dirty file-type changes.
    remove_path(dest).map_err(|message| WorktreeError::CopyFailed {
        path: source.to_path_buf(),
        message: format!("cannot replace existing copy target: {message}"),
    })?;
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
    remove_path(dest).map_err(|message| WorktreeError::CopyFailed {
        path: dest.to_path_buf(),
        message: format!("cannot replace existing symlink target: {message}"),
    })?;
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
    _report: &mut WorktreeReport,
) -> Result<(), WorktreeError> {
    Err(WorktreeError::CopyFailed {
        path: source.to_path_buf(),
        message: "symlink replication is unsupported on this platform".into(),
    })
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

fn remove_synced_path(
    dest_root: &Path,
    relative: &Path,
    report: &mut WorktreeReport,
) -> Result<(), WorktreeError> {
    let target = dest_root.join(relative);
    if remove_path(&target).map_err(|message| WorktreeError::CopyFailed {
        path: target.clone(),
        message: format!("cannot remove path from child worktree: {message}"),
    })? {
        report.files_deleted += 1;
    }
    Ok(())
}

#[cfg(test)]
mod tests;
