//! Reviewable and recoverable merge protocol for managed child worktrees.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio_util::sync::CancellationToken;

use super::git::git_capture;
use super::registry::{RegistryError, WorktreeRecord, WorktreeRegistry, write_record_atomic};
use crate::contract::WorkspaceLifecycle;

/// Review and apply are both bounded. Exceeding the bound fails closed and
/// retains the proposal instead of silently dropping changes.
pub const MAX_CHANGESET_ENTRIES: usize = 4096;
const COPY_BUFFER_BYTES: usize = 64 * 1024;
static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeKind {
    Added,
    Modified,
    Deleted,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangeEntry {
    /// Workspace-relative path.
    pub path: PathBuf,
    pub kind: ChangeKind,
    pub additions: u64,
    pub deletions: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangeSet {
    pub base_revision: Option<String>,
    pub entries: Vec<ChangeEntry>,
}

impl ChangeSet {
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MergeProposal {
    pub worktree_id: String,
    pub child_operation_id: String,
    pub changeset: ChangeSet,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MergeError {
    #[error("worktree is not mergeable: {message}")]
    NotMergeable { message: String },
    #[error("changeset exceeds the safe limit of {limit} entries")]
    ChangeSetTooLarge { limit: usize },
    #[error(
        "parent workspace moved past the child base revision (expected {expected:?}, found {actual:?})"
    )]
    StaleParent {
        expected: Option<String>,
        actual: Option<String>,
    },
    #[error("merge conflicts on {paths:?}")]
    Conflict { paths: Vec<PathBuf> },
    #[error("merge was cancelled")]
    Cancelled,
    #[error("cannot apply change {path}: {message}")]
    ApplyFailed { path: PathBuf, message: String },
    #[error("merge recovery failed: {message}")]
    RecoveryFailed { message: String },
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

#[derive(Debug, Clone, PartialEq, Eq)]
enum EntryIdentity {
    File { digest: [u8; 32], bytes: u64 },
    Symlink(PathBuf),
    Directory,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum PathState {
    Missing,
    Entry(EntryIdentity),
    Blocked {
        ancestor: PathBuf,
        identity: EntryIdentity,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum TransactionPhase {
    Prepared,
    Applied,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct MergeTransaction {
    worktree_id: String,
    source: PathBuf,
    phase: TransactionPhase,
}

/// Snapshot the exact materialized child baseline. This captures dirty tracked
/// and untracked source state and also gives copy-mode worktrees a real base.
pub(super) fn create_baseline(
    registry: &WorktreeRegistry,
    record: &WorktreeRecord,
    cancellation: &CancellationToken,
) -> Result<(), String> {
    let baseline = registry.baseline_dir(&record.id);
    let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let temp = baseline.with_extension(format!("tmp.{}.{}", std::process::id(), sequence));
    remove_path(&temp)?;
    let result = copy_workspace_contents(&record.dest, &temp, cancellation);
    if let Err(error) = result {
        let _ = remove_path(&temp);
        return Err(error);
    }
    if cancellation.is_cancelled() {
        let _ = remove_path(&temp);
        return Err("baseline creation was cancelled".into());
    }
    fs::rename(&temp, &baseline)
        .map_err(|error| format!("cannot commit baseline {}: {error}", baseline.display()))?;
    sync_directory(
        baseline
            .parent()
            .ok_or_else(|| "baseline has no parent directory".to_string())?,
    )
    .map_err(|error| format!("cannot sync baseline directory: {error}"))
}

pub fn build_changeset(registry: &WorktreeRegistry, id: &str) -> Result<ChangeSet, MergeError> {
    build_changeset_cancellable(registry, id, &CancellationToken::new())
}

pub fn build_changeset_cancellable(
    registry: &WorktreeRegistry,
    id: &str,
    cancellation: &CancellationToken,
) -> Result<ChangeSet, MergeError> {
    let record = registry.load(id)?.ok_or_else(|| MergeError::NotMergeable {
        message: format!("worktree {id} is not registered"),
    })?;
    ensure_merge_pending(&record)?;
    build_changeset_for_record(registry, &record, cancellation)
}

fn build_changeset_for_record(
    registry: &WorktreeRegistry,
    record: &WorktreeRecord,
    cancellation: &CancellationToken,
) -> Result<ChangeSet, MergeError> {
    let baseline = registry.baseline_dir(&record.id);
    if !baseline.is_dir() {
        return Err(MergeError::NotMergeable {
            message: format!("worktree {} has no creation baseline", record.id),
        });
    }
    let base = collect_entries(&baseline, cancellation)?;
    let child = collect_entries(&record.dest, cancellation)?;
    let paths = base
        .keys()
        .chain(child.keys())
        .cloned()
        .collect::<BTreeSet<_>>();
    let mut entries = Vec::new();
    for path in paths {
        check_cancelled(cancellation)?;
        let before = base.get(&path);
        let after = child.get(&path);
        if before == after {
            continue;
        }
        if entries.len() == MAX_CHANGESET_ENTRIES {
            return Err(MergeError::ChangeSetTooLarge {
                limit: MAX_CHANGESET_ENTRIES,
            });
        }
        let kind = match (before, after) {
            (None, Some(_)) => ChangeKind::Added,
            (Some(_), None) => ChangeKind::Deleted,
            (Some(_), Some(_)) => ChangeKind::Modified,
            (None, None) => unreachable!("union path exists in at least one snapshot"),
        };
        let (additions, deletions) = match kind {
            ChangeKind::Added => (text_line_count(&record.dest.join(&path)), 0),
            ChangeKind::Deleted => (0, text_line_count(&baseline.join(&path))),
            ChangeKind::Modified => (
                text_line_count(&record.dest.join(&path)),
                text_line_count(&baseline.join(&path)),
            ),
        };
        entries.push(ChangeEntry {
            path,
            kind,
            additions,
            deletions,
        });
    }
    Ok(ChangeSet {
        base_revision: record.base_revision.clone(),
        entries,
    })
}

fn text_line_count(path: &Path) -> u64 {
    const MAX_TEXT_STAT_BYTES: u64 = 8 * 1024 * 1024;
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return 0;
    };
    if !metadata.is_file() || metadata.len() > MAX_TEXT_STAT_BYTES {
        return 0;
    }
    let Ok(bytes) = fs::read(path) else {
        return 0;
    };
    if std::str::from_utf8(&bytes).is_err() || bytes.contains(&0) {
        return 0;
    }
    if bytes.is_empty() {
        0
    } else {
        bytes.iter().filter(|byte| **byte == b'\n').count() as u64
            + u64::from(bytes.last() != Some(&b'\n'))
    }
}

pub fn apply_merge(registry: &WorktreeRegistry, id: &str) -> Result<MergeReport, MergeError> {
    apply_merge_cancellable(registry, id, &CancellationToken::new())
}

pub fn apply_merge_cancellable(
    registry: &WorktreeRegistry,
    id: &str,
    cancellation: &CancellationToken,
) -> Result<MergeReport, MergeError> {
    let _writer = registry.acquire_writer()?;
    let mut record = registry
        .load_unlocked(id)?
        .ok_or_else(|| MergeError::NotMergeable {
            message: format!("worktree {id} is not registered"),
        })?;
    ensure_merge_pending(&record)?;
    check_parent_revision(&record, cancellation)?;
    let changeset = build_changeset_for_record(registry, &record, cancellation)?;
    let conflicts = parent_conflicts(registry, &record, &changeset, cancellation)?;
    if !conflicts.is_empty() {
        return Err(MergeError::Conflict { paths: conflicts });
    }
    check_cancelled(cancellation)?;

    prepare_transaction(registry, &record, cancellation)?;
    if let Err(error) = apply_entries(&record, &changeset.entries, cancellation) {
        rollback_transaction(registry, &record).map_err(|rollback| MergeError::RecoveryFailed {
            message: format!("{error}; rollback also failed: {rollback}"),
        })?;
        return Err(error);
    }
    if let Err(error) = mark_transaction_applied(registry, &record) {
        rollback_transaction(registry, &record).map_err(|rollback| MergeError::RecoveryFailed {
            message: format!("{error}; rollback also failed: {rollback}"),
        })?;
        return Err(error);
    }
    record.transition(WorkspaceLifecycle::Merged, unix_seconds())?;
    write_record_atomic(&registry.record_path(&record.id), &record)?;
    remove_path(&registry.transaction_dir(&record.id)).map_err(|message| {
        MergeError::RecoveryFailed {
            message: format!("merge committed but transaction cleanup failed: {message}"),
        }
    })?;
    Ok(MergeReport {
        worktree_id: record.id,
        base_revision: record.base_revision,
        applied: changeset.entries.len(),
        entries: changeset.entries,
    })
}

fn ensure_merge_pending(record: &WorktreeRecord) -> Result<(), MergeError> {
    if record.lifecycle != WorkspaceLifecycle::MergePending {
        return Err(MergeError::NotMergeable {
            message: format!(
                "worktree {} is {:?}, not MergePending",
                record.id, record.lifecycle
            ),
        });
    }
    Ok(())
}

fn check_parent_revision(
    record: &WorktreeRecord,
    cancellation: &CancellationToken,
) -> Result<(), MergeError> {
    let Some(expected) = record.base_revision.as_ref() else {
        return Ok(());
    };
    let actual = String::from_utf8(git_capture(
        &record.source,
        &["rev-parse", "HEAD"],
        cancellation,
        256,
    )?)
    .map_err(|_| MergeError::GitFailed {
        message: "parent HEAD is not UTF-8".into(),
    })?
    .trim()
    .to_owned();
    if &actual != expected {
        return Err(MergeError::StaleParent {
            expected: Some(expected.clone()),
            actual: Some(actual),
        });
    }
    Ok(())
}

fn parent_conflicts(
    registry: &WorktreeRegistry,
    record: &WorktreeRecord,
    changeset: &ChangeSet,
    cancellation: &CancellationToken,
) -> Result<Vec<PathBuf>, MergeError> {
    let baseline = registry.baseline_dir(&record.id);
    let mut conflicts = Vec::new();
    for entry in &changeset.entries {
        check_cancelled(cancellation)?;
        let base = path_state(&baseline, &entry.path, cancellation)?;
        let parent = path_state(&record.source, &entry.path, cancellation)?;
        if base != parent {
            conflicts.push(entry.path.clone());
        }
    }
    conflicts.sort();
    conflicts.dedup();
    Ok(conflicts)
}

fn apply_entries(
    record: &WorktreeRecord,
    entries: &[ChangeEntry],
    cancellation: &CancellationToken,
) -> Result<(), MergeError> {
    let mut removals = entries
        .iter()
        .filter(|entry| entry.kind == ChangeKind::Deleted)
        .collect::<Vec<_>>();
    removals.sort_by_key(|entry| std::cmp::Reverse(entry.path.components().count()));
    let mut writes = entries
        .iter()
        .filter(|entry| entry.kind != ChangeKind::Deleted)
        .collect::<Vec<_>>();
    writes.sort_by_key(|entry| entry.path.components().count());

    for entry in removals {
        check_cancelled(cancellation)?;
        #[cfg(test)]
        fault_injection::maybe_fail_apply(&entry.path)?;
        remove_path(&record.source.join(&entry.path)).map_err(|message| {
            MergeError::ApplyFailed {
                path: entry.path.clone(),
                message,
            }
        })?;
    }
    for entry in writes {
        check_cancelled(cancellation)?;
        #[cfg(test)]
        fault_injection::maybe_fail_apply(&entry.path)?;
        replace_from_child(&record.source, &record.dest, &entry.path).map_err(|message| {
            MergeError::ApplyFailed {
                path: entry.path.clone(),
                message,
            }
        })?;
    }
    Ok(())
}

fn replace_from_child(
    parent_root: &Path,
    child_root: &Path,
    relative: &Path,
) -> Result<(), String> {
    validate_relative(relative)?;
    let child = child_root.join(relative);
    let parent = parent_root.join(relative);
    ensure_safe_parent(parent_root, relative)?;
    let metadata = fs::symlink_metadata(&child)
        .map_err(|error| format!("cannot inspect child path {}: {error}", child.display()))?;
    let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let temp = parent.with_extension(format!("evo-merge-tmp.{}.{}", std::process::id(), sequence));
    remove_path(&temp)?;
    if metadata.file_type().is_symlink() {
        let target = fs::read_link(&child)
            .map_err(|error| format!("cannot read child symlink {}: {error}", child.display()))?;
        create_symlink(&target, &temp)?;
    } else if metadata.is_file() {
        reflink_copy::reflink_or_copy(&child, &temp)
            .map_err(|error| format!("cannot stage {}: {error}", child.display()))?;
        File::open(&temp)
            .and_then(|file| file.sync_all())
            .map_err(|error| format!("cannot sync staged file {}: {error}", temp.display()))?;
    } else {
        return Err(format!("unsupported child path type: {}", child.display()));
    }
    remove_path(&parent)?;
    fs::rename(&temp, &parent)
        .map_err(|error| format!("cannot install {}: {error}", parent.display()))?;
    sync_directory(
        parent
            .parent()
            .ok_or_else(|| format!("change path has no parent: {}", parent.display()))?,
    )
    .map_err(|error| format!("cannot sync merge parent directory: {error}"))
}

fn prepare_transaction(
    registry: &WorktreeRegistry,
    record: &WorktreeRecord,
    cancellation: &CancellationToken,
) -> Result<(), MergeError> {
    let transaction = registry.transaction_dir(&record.id);
    remove_path(&transaction).map_err(|message| MergeError::RecoveryFailed { message })?;
    let backup = transaction.join("backup");
    copy_workspace_contents(&record.source, &backup, cancellation)
        .map_err(|message| MergeError::RecoveryFailed { message })?;
    write_transaction(
        &transaction,
        &MergeTransaction {
            worktree_id: record.id.clone(),
            source: record.source.clone(),
            phase: TransactionPhase::Prepared,
        },
    )
}

fn mark_transaction_applied(
    registry: &WorktreeRegistry,
    record: &WorktreeRecord,
) -> Result<(), MergeError> {
    write_transaction(
        &registry.transaction_dir(&record.id),
        &MergeTransaction {
            worktree_id: record.id.clone(),
            source: record.source.clone(),
            phase: TransactionPhase::Applied,
        },
    )
}

fn write_transaction(directory: &Path, transaction: &MergeTransaction) -> Result<(), MergeError> {
    fs::create_dir_all(directory).map_err(|error| MergeError::RecoveryFailed {
        message: format!("cannot create merge transaction: {error}"),
    })?;
    let path = directory.join("journal.json");
    let temp = directory.join("journal.json.tmp");
    let bytes = serde_json::to_vec(transaction).map_err(|error| MergeError::RecoveryFailed {
        message: format!("cannot encode merge transaction: {error}"),
    })?;
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(&temp)
        .map_err(|error| MergeError::RecoveryFailed {
            message: format!("cannot create merge journal: {error}"),
        })?;
    #[cfg(test)]
    fault_injection::maybe_fail_journal_write(transaction.phase)?;
    file.write_all(&bytes)
        .and_then(|()| file.sync_all())
        .map_err(|error| MergeError::RecoveryFailed {
            message: format!("cannot sync merge journal: {error}"),
        })?;
    fs::rename(&temp, &path).map_err(|error| MergeError::RecoveryFailed {
        message: format!("cannot commit merge journal: {error}"),
    })?;
    sync_directory(directory).map_err(|error| MergeError::RecoveryFailed {
        message: format!("cannot sync merge transaction directory: {error}"),
    })
}

fn rollback_transaction(
    registry: &WorktreeRegistry,
    record: &WorktreeRecord,
) -> Result<(), String> {
    let transaction = registry.transaction_dir(&record.id);
    let backup = transaction.join("backup");
    if !backup.is_dir() {
        return Err(format!("merge backup is missing: {}", backup.display()));
    }
    remove_workspace_contents(&record.source)?;
    copy_workspace_contents(&backup, &record.source, &CancellationToken::new())?;
    remove_path(&transaction)
}

/// Recover transactions before normal registry reconciliation. Prepared
/// transactions are rolled back; Applied transactions complete the durable
/// lifecycle transition. The operation may have died at any instruction.
pub(super) fn recover_transactions(
    registry: &WorktreeRegistry,
) -> Result<(Vec<String>, Vec<String>), RegistryError> {
    let root = registry.root().join("transactions");
    let mut rolled_back = Vec::new();
    let mut completed = Vec::new();
    let entries = fs::read_dir(&root).map_err(|error| RegistryError::Io {
        message: format!("cannot read merge transactions: {error}"),
    })?;
    for entry in entries {
        let entry = entry.map_err(|error| RegistryError::Io {
            message: format!("cannot read merge transaction entry: {error}"),
        })?;
        if !entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false) {
            continue;
        }
        let journal = entry.path().join("journal.json");
        let bytes = match fs::read(&journal) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                // The journal is committed only after the backup is complete and
                // before the first parent mutation. A journal-less directory is
                // therefore an incomplete, non-authoritative backup.
                remove_path(&entry.path()).map_err(|message| RegistryError::Io { message })?;
                continue;
            }
            Err(error) => {
                return Err(RegistryError::Io {
                    message: format!("cannot read merge journal {}: {error}", journal.display()),
                });
            }
        };
        let transaction: MergeTransaction =
            serde_json::from_slice(&bytes).map_err(|error| RegistryError::InvalidRecord {
                message: format!("cannot decode merge journal {}: {error}", journal.display()),
            })?;
        let mut record = registry
            .load_unlocked(&transaction.worktree_id)?
            .ok_or_else(|| RegistryError::UnknownWorktree {
                id: transaction.worktree_id.clone(),
            })?;
        if record.source != transaction.source
            || entry.file_name().to_string_lossy() != transaction.worktree_id
        {
            return Err(RegistryError::InvalidRecord {
                message: format!("merge transaction identity mismatch for {}", record.id),
            });
        }
        match transaction.phase {
            TransactionPhase::Prepared => {
                rollback_transaction(registry, &record).map_err(|message| RegistryError::Io {
                    message: format!("cannot roll back merge {}: {message}", record.id),
                })?;
                rolled_back.push(record.id);
            }
            TransactionPhase::Applied => {
                if record.lifecycle == WorkspaceLifecycle::MergePending {
                    record.transition(WorkspaceLifecycle::Merged, unix_seconds())?;
                    write_record_atomic(&registry.record_path(&record.id), &record)?;
                }
                remove_path(&entry.path()).map_err(|message| RegistryError::Io { message })?;
                completed.push(record.id);
            }
        }
    }
    Ok((rolled_back, completed))
}

fn collect_entries(
    root: &Path,
    cancellation: &CancellationToken,
) -> Result<BTreeMap<PathBuf, EntryIdentity>, MergeError> {
    let mut entries = BTreeMap::new();
    let mut pending = vec![PathBuf::new()];
    while let Some(relative) = pending.pop() {
        check_cancelled(cancellation)?;
        let absolute = root.join(&relative);
        let metadata =
            fs::symlink_metadata(&absolute).map_err(|error| MergeError::NotMergeable {
                message: format!("cannot inspect {}: {error}", absolute.display()),
            })?;
        if metadata.file_type().is_symlink() {
            let target = fs::read_link(&absolute).map_err(|error| MergeError::NotMergeable {
                message: format!("cannot read symlink {}: {error}", absolute.display()),
            })?;
            entries.insert(relative, EntryIdentity::Symlink(target));
        } else if metadata.is_dir() {
            let directory = fs::read_dir(&absolute).map_err(|error| MergeError::NotMergeable {
                message: format!("cannot read directory {}: {error}", absolute.display()),
            })?;
            for child in directory {
                let child = child.map_err(|error| MergeError::NotMergeable {
                    message: format!("cannot read directory {}: {error}", absolute.display()),
                })?;
                if relative.as_os_str().is_empty() && child.file_name() == ".git" {
                    continue;
                }
                pending.push(relative.join(child.file_name()));
            }
        } else if metadata.is_file() {
            entries.insert(relative, hash_file(&absolute, cancellation)?);
        } else {
            return Err(MergeError::NotMergeable {
                message: format!("unsupported filesystem entry: {}", absolute.display()),
            });
        }
    }
    Ok(entries)
}

fn path_state(
    root: &Path,
    relative: &Path,
    cancellation: &CancellationToken,
) -> Result<PathState, MergeError> {
    validate_relative(relative).map_err(|message| MergeError::NotMergeable { message })?;
    let mut current = root.to_path_buf();
    let components = relative.components().collect::<Vec<_>>();
    for (index, component) in components.iter().enumerate() {
        check_cancelled(cancellation)?;
        let Component::Normal(name) = component else {
            unreachable!("relative path was validated")
        };
        current.push(name);
        let metadata = match fs::symlink_metadata(&current) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(PathState::Missing),
            Err(error) => {
                return Err(MergeError::NotMergeable {
                    message: format!("cannot inspect {}: {error}", current.display()),
                });
            }
        };
        let identity = identity_from_metadata(&current, &metadata, cancellation)?;
        if index + 1 == components.len() {
            return Ok(PathState::Entry(identity));
        }
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Ok(PathState::Blocked {
                ancestor: current.strip_prefix(root).unwrap_or(&current).to_path_buf(),
                identity,
            });
        }
    }
    Ok(PathState::Entry(EntryIdentity::Directory))
}

fn identity_from_metadata(
    path: &Path,
    metadata: &fs::Metadata,
    cancellation: &CancellationToken,
) -> Result<EntryIdentity, MergeError> {
    if metadata.file_type().is_symlink() {
        return fs::read_link(path)
            .map(EntryIdentity::Symlink)
            .map_err(|error| MergeError::NotMergeable {
                message: format!("cannot read symlink {}: {error}", path.display()),
            });
    }
    if metadata.is_dir() {
        return Ok(EntryIdentity::Directory);
    }
    if metadata.is_file() {
        return hash_file(path, cancellation);
    }
    Err(MergeError::NotMergeable {
        message: format!("unsupported filesystem entry: {}", path.display()),
    })
}

fn hash_file(path: &Path, cancellation: &CancellationToken) -> Result<EntryIdentity, MergeError> {
    let mut file = File::open(path).map_err(|error| MergeError::NotMergeable {
        message: format!("cannot open {}: {error}", path.display()),
    })?;
    let mut digest = Sha256::new();
    let mut buffer = vec![0; COPY_BUFFER_BYTES];
    let mut bytes = 0u64;
    loop {
        check_cancelled(cancellation)?;
        let read = file
            .read(&mut buffer)
            .map_err(|error| MergeError::NotMergeable {
                message: format!("cannot read {}: {error}", path.display()),
            })?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
        bytes = bytes.saturating_add(read as u64);
    }
    Ok(EntryIdentity::File {
        digest: digest.finalize().into(),
        bytes,
    })
}

fn copy_workspace_contents(
    source: &Path,
    dest: &Path,
    cancellation: &CancellationToken,
) -> Result<(), String> {
    fs::create_dir_all(dest)
        .map_err(|error| format!("cannot create snapshot {}: {error}", dest.display()))?;
    let mut pending = vec![PathBuf::new()];
    while let Some(relative) = pending.pop() {
        if cancellation.is_cancelled() {
            return Err("snapshot copy was cancelled".into());
        }
        let current = source.join(&relative);
        for entry in fs::read_dir(&current)
            .map_err(|error| format!("cannot read {}: {error}", current.display()))?
        {
            let entry =
                entry.map_err(|error| format!("cannot read {}: {error}", current.display()))?;
            if relative.as_os_str().is_empty() && entry.file_name() == ".git" {
                continue;
            }
            let child_relative = relative.join(entry.file_name());
            let child_source = source.join(&child_relative);
            let child_dest = dest.join(&child_relative);
            let metadata = fs::symlink_metadata(&child_source)
                .map_err(|error| format!("cannot inspect {}: {error}", child_source.display()))?;
            if metadata.file_type().is_symlink() {
                if let Some(parent) = child_dest.parent() {
                    fs::create_dir_all(parent).map_err(|error| error.to_string())?;
                }
                let target = fs::read_link(&child_source).map_err(|error| error.to_string())?;
                create_symlink(&target, &child_dest)?;
            } else if metadata.is_dir() {
                fs::create_dir_all(&child_dest).map_err(|error| error.to_string())?;
                pending.push(child_relative);
            } else if metadata.is_file() {
                if let Some(parent) = child_dest.parent() {
                    fs::create_dir_all(parent).map_err(|error| error.to_string())?;
                }
                reflink_copy::reflink_or_copy(&child_source, &child_dest)
                    .map_err(|error| format!("cannot copy {}: {error}", child_source.display()))?;
            } else {
                return Err(format!(
                    "unsupported snapshot entry: {}",
                    child_source.display()
                ));
            }
        }
    }
    sync_directory(dest).map_err(|error| format!("cannot sync snapshot: {error}"))
}

fn remove_workspace_contents(root: &Path) -> Result<(), String> {
    for entry in fs::read_dir(root)
        .map_err(|error| format!("cannot read workspace {}: {error}", root.display()))?
    {
        let entry = entry.map_err(|error| error.to_string())?;
        if entry.file_name() == ".git" {
            continue;
        }
        remove_path(&entry.path())?;
    }
    Ok(())
}

fn ensure_safe_parent(root: &Path, relative: &Path) -> Result<(), String> {
    validate_relative(relative)?;
    let parent = relative
        .parent()
        .ok_or_else(|| format!("change path has no parent: {}", relative.display()))?;
    let mut current = root.to_path_buf();
    for component in parent.components() {
        let Component::Normal(name) = component else {
            return Err(format!("invalid change path: {}", relative.display()));
        };
        current.push(name);
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
            Ok(_) => {
                return Err(format!(
                    "merge parent is not a real directory: {}",
                    current.display()
                ));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                fs::create_dir(&current)
                    .map_err(|error| format!("cannot create {}: {error}", current.display()))?;
            }
            Err(error) => return Err(format!("cannot inspect {}: {error}", current.display())),
        }
    }
    Ok(())
}

fn validate_relative(path: &Path) -> Result<(), String> {
    if path.as_os_str().is_empty()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(format!(
            "change path is not workspace-relative: {}",
            path.display()
        ));
    }
    Ok(())
}

fn remove_path(path: &Path) -> Result<(), String> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
            fs::remove_dir_all(path)
                .map_err(|error| format!("cannot remove directory {}: {error}", path.display()))
        }
        Ok(_) => fs::remove_file(path)
            .map_err(|error| format!("cannot remove file {}: {error}", path.display())),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("cannot inspect {}: {error}", path.display())),
    }
}

#[cfg(unix)]
fn create_symlink(target: &Path, path: &Path) -> Result<(), String> {
    std::os::unix::fs::symlink(target, path)
        .map_err(|error| format!("cannot create symlink {}: {error}", path.display()))
}

#[cfg(windows)]
fn create_symlink(target: &Path, path: &Path) -> Result<(), String> {
    use std::os::windows::fs::{symlink_dir, symlink_file};
    let create = if target.is_dir() {
        symlink_dir
    } else {
        symlink_file
    };
    create(target, path)
        .map_err(|error| format!("cannot create symlink {}: {error}", path.display()))
}

fn sync_directory(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        File::open(path)?.sync_all()
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Ok(())
    }
}

fn check_cancelled(cancellation: &CancellationToken) -> Result<(), MergeError> {
    if cancellation.is_cancelled() {
        Err(MergeError::Cancelled)
    } else {
        Ok(())
    }
}

fn unix_seconds() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod fault_injection;

#[cfg(test)]
#[path = "merge_tests.rs"]
mod merge_tests;
