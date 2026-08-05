//! Durable managed-worktree registry, startup recovery, and GC (ARC-320).
//!
//! Layout under a registry root:
//!
//! ```text
//! <root>/
//!   registry/<id>.json     one atomic record per worktree
//!   worktrees/<id>/        the materialized worktree directory
//! ```
//!
//! Records are written atomically (temp file + rename). Startup recovery
//! removes interrupted creations and stale records; orphan directories with
//! no record are reported but never deleted, because their identity cannot be
//! verified. GC only ever deletes directories that match a validated record.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use super::git::run_git;
use super::{ManagedWorktree, WorkingTreeMode, WorktreeBuilder, WorktreeCreationMode};
use crate::contract::{
    WorkspaceHandle, WorkspaceKind, WorkspaceLifecycle, valid_lifecycle_transition,
};

const REGISTRY_DIRECTORY: &str = "registry";
const WORKTREES_DIRECTORY: &str = "worktrees";
const ID_HASH_BYTES: usize = 16;

static WORKTREE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Durable facts of one managed worktree.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorktreeRecord {
    /// WorkspaceId display form (kind-prefixed, bounded ASCII).
    pub id: String,
    pub kind: WorkspaceKind,
    /// Parent workspace root (used to unregister git worktree metadata).
    pub source: PathBuf,
    /// Absolute child worktree directory.
    pub dest: PathBuf,
    pub owner_operation: String,
    pub parent_session: Option<String>,
    pub base_revision: Option<String>,
    pub creation_mode: WorktreeCreationMode,
    pub lifecycle: WorkspaceLifecycle,
    /// Unix seconds.
    pub created_at: u64,
    pub updated_at: u64,
}

impl WorktreeRecord {
    pub fn from_managed(managed: &ManagedWorktree, source: &Path, now: u64) -> Self {
        Self {
            id: managed.lease().handle().id().as_str().to_owned(),
            kind: managed.lease().handle().kind(),
            source: source.to_path_buf(),
            dest: managed.root().to_path_buf(),
            owner_operation: managed.lease().owner_operation().to_owned(),
            parent_session: managed.lease().parent_session().map(str::to_owned),
            base_revision: managed.lease().base_revision().map(str::to_owned),
            creation_mode: managed.creation_mode(),
            lifecycle: managed.lease().lifecycle(),
            created_at: now,
            updated_at: now,
        }
    }

    /// Advance the record lifecycle through the same table the lease uses.
    pub fn transition(&mut self, next: WorkspaceLifecycle, now: u64) -> Result<(), RegistryError> {
        if !valid_lifecycle_transition(self.lifecycle, next) {
            return Err(RegistryError::InvalidTransition {
                from: self.lifecycle,
                to: next,
            });
        }
        self.lifecycle = next;
        self.updated_at = now;
        Ok(())
    }
}

/// File-based registry of managed worktrees.
#[derive(Debug, Clone)]
pub struct WorktreeRegistry {
    root: PathBuf,
}

impl WorktreeRegistry {
    /// Open (creating on first use) a registry at `root`.
    pub fn open(root: impl Into<PathBuf>) -> Result<Self, RegistryError> {
        let root = root.into();
        fs::create_dir_all(Self::registry_dir(&root)).map_err(|error| RegistryError::Io {
            message: format!("cannot create registry directory: {error}"),
        })?;
        fs::create_dir_all(Self::worktrees_dir(&root)).map_err(|error| RegistryError::Io {
            message: format!("cannot create worktrees directory: {error}"),
        })?;
        Ok(Self { root })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The directory under which managed worktrees are materialized.
    pub fn worktrees_root(&self) -> PathBuf {
        Self::worktrees_dir(&self.root)
    }

    /// The directory a worktree with `id` occupies.
    pub fn worktree_dir(&self, id: &str) -> PathBuf {
        self.worktrees_root().join(id)
    }

    /// Create and register a managed child worktree in one step.
    ///
    /// A unique identity is derived from the source and owner, the destination
    /// is allocated under this registry's worktrees root, and the resulting
    /// record is registered as `Ready` only after materialization succeeded.
    pub fn create_managed(
        &self,
        source: &WorkspaceHandle,
        owner_operation: &str,
        parent_session: Option<&str>,
        mode: WorkingTreeMode,
        cancellation: &CancellationToken,
    ) -> Result<WorktreeRecord, RegistryError> {
        let value = unique_worktree_value(source, owner_operation);
        let id = crate::contract::WorkspaceId::user_supplied(WorkspaceKind::ManagedChild, value)
            .map_err(|error| RegistryError::InvalidRecord {
                message: format!("cannot construct worktree id: {error}"),
            })?;
        let dest = self.worktree_dir(id.as_str());
        let managed = WorktreeBuilder::new(source.clone(), &dest, owner_operation)
            .parent_session(parent_session.map(str::to_owned))
            .worktree_id(id)
            .working_tree_mode(mode)
            .cancellation_token(cancellation.clone())
            .create()
            .map_err(RegistryError::Worktree)?;
        let now = unix_seconds();
        let record = WorktreeRecord::from_managed(&managed, source.root(), now);
        self.register(&record)?;
        Ok(record)
    }

    pub fn register(&self, record: &WorktreeRecord) -> Result<(), RegistryError> {
        validate_record(record, &self.root)?;
        write_record_atomic(&self.record_path(&record.id), record)
    }

    pub fn load(&self, id: &str) -> Result<Option<WorktreeRecord>, RegistryError> {
        let path = self.record_path(id);
        match fs::read(&path) {
            Ok(bytes) => deserialize_record(&bytes).map(Some),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(RegistryError::Io {
                message: format!("cannot read record {id}: {error}"),
            }),
        }
    }

    pub fn load_all(&self) -> Result<Vec<WorktreeRecord>, RegistryError> {
        let mut records = Vec::new();
        for entry in read_directory(&WorktreeRegistry::registry_dir(&self.root))? {
            let name = entry.file_name();
            let Some(id) = name.to_str().and_then(|name| name.strip_suffix(".json")) else {
                continue;
            };
            records.push(self.load(id)?.ok_or_else(|| RegistryError::Io {
                message: format!("record {id} vanished while loading"),
            })?);
        }
        records.sort_by(|left, right| left.id.cmp(&right.id));
        Ok(records)
    }

    pub fn transition(
        &self,
        id: &str,
        next: WorkspaceLifecycle,
        now: u64,
    ) -> Result<WorktreeRecord, RegistryError> {
        let mut record = self
            .load(id)?
            .ok_or_else(|| RegistryError::UnknownWorktree { id: id.to_owned() })?;
        record.transition(next, now)?;
        write_record_atomic(&self.record_path(id), &record)?;
        Ok(record)
    }

    pub fn remove(&self, id: &str) -> Result<(), RegistryError> {
        match fs::remove_file(self.record_path(id)) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(RegistryError::Io {
                message: format!("cannot remove record {id}: {error}"),
            }),
        }
    }

    fn record_path(&self, id: &str) -> PathBuf {
        WorktreeRegistry::registry_dir(&self.root).join(format!("{id}.json"))
    }

    fn registry_dir(root: &Path) -> PathBuf {
        root.join(REGISTRY_DIRECTORY)
    }

    fn worktrees_dir(root: &Path) -> PathBuf {
        root.join(WORKTREES_DIRECTORY)
    }
}

/// Startup view of registry/disk inconsistency.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RecoveryReport {
    /// Directories under the worktrees root with no record. Never deleted
    /// automatically: their identity cannot be verified.
    pub orphans: Vec<PathBuf>,
    /// Records whose worktree directory is missing; removable.
    pub stale_records: Vec<WorktreeRecord>,
    /// Records stuck in Creating/Cleaning (a previous run crashed mid-way).
    pub interrupted: Vec<WorktreeRecord>,
}

impl WorktreeRegistry {
    /// Reconcile the registry after startup: remove interrupted worktrees and
    /// stale records, and report orphan directories for human review.
    ///
    /// The returned report describes what was found before recovery, so
    /// callers can audit which records were cleaned.
    pub fn recover(&self) -> Result<RecoveryReport, RegistryError> {
        let report = self.scan()?;
        for record in &report.interrupted {
            remove_materialization(self, record)?;
            self.remove(&record.id)?;
        }
        for record in &report.stale_records {
            self.remove(&record.id)?;
        }
        Ok(report)
    }

    fn scan(&self) -> Result<RecoveryReport, RegistryError> {
        let records = self.load_all()?;
        let mut report = RecoveryReport::default();
        let mut recorded_ids = std::collections::HashSet::new();
        for record in records {
            recorded_ids.insert(record.id.clone());
            let dir_missing = match fs::symlink_metadata(&record.dest) {
                Ok(metadata) => !metadata.is_dir(),
                Err(_) => true,
            };
            if dir_missing {
                report.stale_records.push(record.clone());
            }
            if matches!(
                record.lifecycle,
                WorkspaceLifecycle::Creating | WorkspaceLifecycle::Cleaning
            ) {
                report.interrupted.push(record);
            }
        }
        for entry in read_directory(&self.worktrees_root())? {
            let name = entry.file_name();
            if !recorded_ids.contains(&name.to_string_lossy().to_string())
                && entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false)
            {
                report.orphans.push(entry.path());
            }
        }
        report.orphans.sort();
        Ok(report)
    }
}

/// GC policy for managed worktrees.
pub struct GcOptions {
    /// Seconds used as "now" for age checks (injectable for tests).
    pub now: u64,
    /// Only remove worktrees untouched for at least this many seconds.
    pub max_age_seconds: u64,
    /// Stop removing once at least this many bytes are reclaimed.
    pub disk_budget_bytes: Option<u64>,
    /// Returns whether the worktree's owner operation is still alive. A
    /// worktree whose owner is alive is never a candidate.
    pub owner_liveness: Box<dyn Fn(&str) -> bool + Send + Sync>,
    /// Report candidates without removing anything.
    pub dry_run: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GcRemovedWorktree {
    pub id: String,
    pub dest: PathBuf,
    pub reason: String,
    pub bytes_reclaimed: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GcReport {
    pub removed: Vec<GcRemovedWorktree>,
    pub candidates: Vec<String>,
}

impl WorktreeRegistry {
    /// Collect stale managed worktrees: dead owners past `max_age_seconds`,
    /// oldest first, until `disk_budget_bytes` is reclaimed.
    pub fn gc(&self, options: &GcOptions) -> Result<GcReport, RegistryError> {
        let records = self.load_all()?;
        let mut eligible = records
            .into_iter()
            .filter(|record| !(options.owner_liveness)(&record.owner_operation))
            .filter(|record| {
                options.now.saturating_sub(record.updated_at) >= options.max_age_seconds
            })
            .collect::<Vec<_>>();
        eligible.sort_by_key(|record| record.updated_at);

        let mut report = GcReport {
            removed: Vec::new(),
            candidates: eligible.iter().map(|record| record.id.clone()).collect(),
        };
        let mut reclaimed = 0u64;
        for record in eligible {
            if let Some(budget) = options.disk_budget_bytes
                && reclaimed >= budget
            {
                break;
            }
            if options.dry_run {
                continue;
            }
            let size = dir_size(&record.dest).unwrap_or(0);
            remove_materialization(self, &record)?;
            self.remove(&record.id)?;
            reclaimed = reclaimed.saturating_add(size);
            report.removed.push(GcRemovedWorktree {
                id: record.id,
                dest: record.dest,
                reason: "expired owner past max age".into(),
                bytes_reclaimed: size,
            });
        }
        Ok(report)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RegistryError {
    #[error("registry io error: {message}")]
    Io { message: String },
    #[error("registry record is invalid: {message}")]
    InvalidRecord { message: String },
    #[error("worktree is not registered: {id}")]
    UnknownWorktree { id: String },
    #[error("invalid lifecycle transition: {from:?} -> {to:?}")]
    InvalidTransition {
        from: WorkspaceLifecycle,
        to: WorkspaceLifecycle,
    },
    #[error("worktree creation failed: {0}")]
    Worktree(#[from] super::WorktreeError),
}

fn unique_worktree_value(source: &WorkspaceHandle, owner_operation: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut digest = Sha256::new();
    digest.update(b"evo-managed-worktree-v1\0");
    digest.update(source.id().as_str().as_bytes());
    digest.update([0]);
    digest.update(owner_operation.as_bytes());
    digest.update([0]);
    digest.update(unix_seconds().to_le_bytes());
    digest.update(
        WORKTREE_SEQUENCE
            .fetch_add(1, Ordering::Relaxed)
            .to_le_bytes(),
    );
    let encoded = format!("{:x}", digest.finalize());
    encoded[..ID_HASH_BYTES].to_owned()
}

fn unix_seconds() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

fn validate_record(record: &WorktreeRecord, root: &Path) -> Result<(), RegistryError> {
    if record.id.is_empty()
        || !record
            .id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(RegistryError::InvalidRecord {
            message: format!("id is not bounded ASCII: {}", record.id),
        });
    }
    if !record.dest.is_absolute() {
        return Err(RegistryError::InvalidRecord {
            message: format!("dest is not absolute: {}", record.dest.display()),
        });
    }
    if !record
        .dest
        .starts_with(WorktreeRegistry::worktrees_dir(root))
        || record
            .dest
            .file_name()
            .map(|name| name.to_string_lossy())
            .as_deref()
            != Some(record.id.as_str())
    {
        return Err(RegistryError::InvalidRecord {
            message: format!(
                "dest {} does not match id {} under the registry worktrees root",
                record.dest.display(),
                record.id
            ),
        });
    }
    Ok(())
}

fn deserialize_record(bytes: &[u8]) -> Result<WorktreeRecord, RegistryError> {
    serde_json::from_slice(bytes).map_err(|error| RegistryError::InvalidRecord {
        message: format!("cannot decode record: {error}"),
    })
}

fn write_record_atomic(path: &Path, record: &WorktreeRecord) -> Result<(), RegistryError> {
    let bytes = serde_json::to_vec(record).map_err(|error| RegistryError::Io {
        message: format!("cannot encode record: {error}"),
    })?;
    let tmp = path.with_extension(format!("json.tmp.{}", std::process::id()));
    fs::write(&tmp, &bytes).map_err(|error| RegistryError::Io {
        message: format!("cannot write record temp file: {error}"),
    })?;
    fs::rename(&tmp, path).map_err(|error| {
        let _ = fs::remove_file(&tmp);
        RegistryError::Io {
            message: format!("cannot commit record: {error}"),
        }
    })
}

fn read_directory(path: &Path) -> Result<Vec<fs::DirEntry>, RegistryError> {
    let mut entries = fs::read_dir(path)
        .map_err(|error| RegistryError::Io {
            message: format!("cannot read directory {}: {error}", path.display()),
        })?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| RegistryError::Io {
            message: format!("cannot read directory entry: {error}"),
        })?;
    entries.sort_by_key(|entry| entry.file_name());
    Ok(entries)
}

/// Remove one recorded worktree's materialization, verifying its identity
/// against the record before touching the directory.
fn remove_materialization(
    registry: &WorktreeRegistry,
    record: &WorktreeRecord,
) -> Result<(), RegistryError> {
    validate_record(record, registry.root())?;
    if record.creation_mode == WorktreeCreationMode::GitLinked && record.source.is_dir() {
        let _ = run_git(
            &record.source,
            &["worktree", "remove", "--force"],
            Some(&record.dest),
            &CancellationToken::new(),
        );
    }
    match fs::symlink_metadata(&record.dest) {
        Ok(metadata) if metadata.is_dir() => {
            fs::remove_dir_all(&record.dest).map_err(|error| RegistryError::Io {
                message: format!(
                    "cannot remove worktree directory {}: {error}",
                    record.dest.display()
                ),
            })?;
        }
        Ok(_) => {
            fs::remove_file(&record.dest).map_err(|error| RegistryError::Io {
                message: format!(
                    "cannot remove worktree path {}: {error}",
                    record.dest.display()
                ),
            })?;
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(RegistryError::Io {
                message: format!(
                    "cannot inspect worktree directory {}: {error}",
                    record.dest.display()
                ),
            });
        }
    }
    if record.creation_mode == WorktreeCreationMode::GitLinked && record.source.is_dir() {
        let _ = run_git(
            &record.source,
            &["worktree", "prune", "--expire", "now"],
            None,
            &CancellationToken::new(),
        );
    }
    Ok(())
}

/// Recursive byte size of `path`; missing paths count as zero.
fn dir_size(path: &Path) -> Result<u64, io::Error> {
    let mut total = 0u64;
    let mut pending = vec![path.to_path_buf()];
    while let Some(current) = pending.pop() {
        for entry in fs::read_dir(&current)? {
            let entry = entry?;
            let metadata = entry.metadata()?;
            if metadata.is_dir() {
                pending.push(entry.path());
            } else {
                total = total.saturating_add(metadata.len());
            }
        }
    }
    Ok(total)
}

#[cfg(test)]
mod tests;
