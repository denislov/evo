//! Durable managed-worktree registry, startup recovery, and GC (ARC-320).
//!
//! Layout under a registry root:
//!
//! ```text
//! <root>/
//!   registry/<id>.json     one atomic record per worktree
//!   worktrees/<id>/        the materialized worktree directory
//!   baselines/<id>/        immutable creation-time merge baseline
//!   transactions/<id>/     recoverable merge journal and backups
//! ```
//!
//! Records are written atomically (temp file + rename). Startup recovery
//! removes interrupted creations and stale records; orphan directories with
//! no record are reported but never deleted, because their identity cannot be
//! verified. GC only ever deletes directories that match a validated record.

use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use super::{ManagedWorktree, WorkingTreeMode, WorktreeBuilder, WorktreeCreationMode};
use crate::contract::{
    WorkspaceHandle, WorkspaceId, WorkspaceKind, WorkspaceLifecycle, valid_lifecycle_transition,
};

mod cleanup;
mod storage;
use cleanup::{dir_size, remove_materialization};
pub(super) use storage::write_record_atomic;
use storage::{deserialize_record, read_directory};

const REGISTRY_DIRECTORY: &str = "registry";
const WORKTREES_DIRECTORY: &str = "worktrees";
const BASELINES_DIRECTORY: &str = "baselines";
const TRANSACTIONS_DIRECTORY: &str = "transactions";
const WRITER_LOCK_FILE: &str = ".writer.lock";
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
    /// Process that created the record; used to identify owners after restart.
    pub owner_pid: u32,
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
            owner_pid: std::process::id(),
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
///
/// The registry enforces a concurrency budget: at most `capacity` managed
/// worktrees may be live (`Ready`/`Active`/`MergePending`) at any moment.
/// `capacity = None` disables the limit.
#[derive(Debug, Clone)]
pub struct WorktreeRegistry {
    root: PathBuf,
    capacity: Option<usize>,
}

impl WorktreeRegistry {
    /// Open (creating on first use) a registry at `root` with no capacity limit.
    pub fn open(root: impl Into<PathBuf>) -> Result<Self, RegistryError> {
        Self::open_with_capacity(root, None)
    }

    /// Open a registry at `root` with an explicit live-worktree capacity.
    ///
    /// `capacity` bounds how many managed worktrees can exist concurrently;
    /// `None` means unlimited. This is the budget consumers rely on to bound
    /// parallel child agents without hard-coded product constants.
    pub fn open_with_capacity(
        root: impl Into<PathBuf>,
        capacity: Option<usize>,
    ) -> Result<Self, RegistryError> {
        let root = root.into();
        fs::create_dir_all(&root).map_err(|error| RegistryError::Io {
            message: format!("cannot create registry root: {error}"),
        })?;
        let root = fs::canonicalize(&root).map_err(|error| RegistryError::Io {
            message: format!("cannot resolve registry root: {error}"),
        })?;
        fs::create_dir_all(Self::registry_dir(&root)).map_err(|error| RegistryError::Io {
            message: format!("cannot create registry directory: {error}"),
        })?;
        fs::create_dir_all(Self::worktrees_dir(&root)).map_err(|error| RegistryError::Io {
            message: format!("cannot create worktrees directory: {error}"),
        })?;
        fs::create_dir_all(Self::baselines_dir(&root)).map_err(|error| RegistryError::Io {
            message: format!("cannot create baselines directory: {error}"),
        })?;
        fs::create_dir_all(Self::transactions_dir(&root)).map_err(|error| RegistryError::Io {
            message: format!("cannot create transactions directory: {error}"),
        })?;
        Ok(Self { root, capacity })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The live-worktree capacity budget, or `None` when unlimited.
    pub fn capacity(&self) -> Option<usize> {
        self.capacity
    }

    /// The directory under which managed worktrees are materialized.
    pub fn worktrees_root(&self) -> PathBuf {
        Self::worktrees_dir(&self.root)
    }

    /// The directory a worktree with a validated identity occupies.
    fn worktree_dir(&self, id: &WorkspaceId) -> PathBuf {
        self.worktrees_root().join(id.as_str())
    }

    /// Create and register a managed child worktree in one step.
    ///
    /// A unique identity is derived from the source and owner, the destination
    /// is allocated under this registry's worktrees root. A durable `Creating`
    /// record is written before materialization and replaced by `Ready` only
    /// after the builder succeeds.
    pub fn create_managed(
        &self,
        source: &WorkspaceHandle,
        owner_operation: &str,
        parent_session: Option<&str>,
        mode: WorkingTreeMode,
        cancellation: &CancellationToken,
    ) -> Result<WorktreeRecord, RegistryError> {
        let _writer = self.acquire_writer()?;
        self.check_capacity_unlocked()?;
        let value = unique_worktree_value(source, owner_operation);
        let id = crate::contract::WorkspaceId::user_supplied(WorkspaceKind::ManagedChild, value)
            .map_err(|error| RegistryError::InvalidRecord {
                message: format!("cannot construct worktree id: {error}"),
            })?;
        let dest = self.worktree_dir(&id);
        let source_root = fs::canonicalize(source.root()).map_err(|error| RegistryError::Io {
            message: format!("cannot resolve source workspace: {error}"),
        })?;
        let now = unix_seconds();
        let creating = WorktreeRecord {
            id: id.as_str().to_owned(),
            kind: WorkspaceKind::ManagedChild,
            source: source_root.clone(),
            dest: dest.clone(),
            owner_operation: owner_operation.to_owned(),
            owner_pid: std::process::id(),
            parent_session: parent_session.map(str::to_owned),
            base_revision: None,
            creation_mode: if fs::symlink_metadata(source.root().join(".git")).is_ok() {
                WorktreeCreationMode::GitLinked
            } else {
                WorktreeCreationMode::Copy
            },
            lifecycle: WorkspaceLifecycle::Creating,
            created_at: now,
            updated_at: now,
        };
        self.register_unlocked(&creating)?;
        let managed = WorktreeBuilder::new(source.clone(), &dest, owner_operation)
            .parent_session(parent_session.map(str::to_owned))
            .worktree_id(id)
            .working_tree_mode(mode)
            .cancellation_token(cancellation.clone())
            .create();
        let managed = match managed {
            Ok(managed) => managed,
            Err(error @ super::WorktreeError::Cancelled) => {
                // A normal cancellation has already cleaned the materialization;
                // remove its durable Creating record immediately. If the process
                // crashes instead, startup recovery still handles the record.
                self.remove_unlocked(&creating.id)?;
                return Err(RegistryError::Worktree(error));
            }
            Err(error) => return Err(RegistryError::Worktree(error)),
        };
        let record = WorktreeRecord::from_managed(&managed, &source_root, now);
        if let Err(error) = super::merge::create_baseline(self, &record, cancellation) {
            let _ = remove_materialization(self, &record);
            let _ = self.remove_unlocked(&creating.id);
            return Err(RegistryError::Io {
                message: format!("cannot create merge baseline: {error}"),
            });
        }
        self.register_unlocked(&record)?;
        Ok(record)
    }

    #[cfg(test)]
    pub(crate) fn register(&self, record: &WorktreeRecord) -> Result<(), RegistryError> {
        let _writer = self.acquire_writer()?;
        validate_record(record, &self.root)?;
        if self.record_path(&record.id).exists() {
            return Err(RegistryError::AlreadyRegistered {
                id: record.id.clone(),
            });
        }
        self.register_unlocked(record)
    }

    /// Remove a managed worktree and its record after re-validating identity.
    ///
    /// The materialization is removed (git registration, directory, prune) and
    /// the durable record is deleted only when identity checks pass. Missing
    /// records are idempotent no-ops; anything else is fail-closed.
    pub fn discard(&self, id: &str) -> Result<(), RegistryError> {
        let _writer = self.acquire_writer()?;
        let parsed = parse_registry_id(id)?;
        let Some(mut record) = self.load_unlocked(parsed.as_str())? else {
            return Ok(());
        };
        validate_record(&record, &self.root)?;
        if record.lifecycle == WorkspaceLifecycle::Removed {
            return self.remove_unlocked(&record.id);
        }
        if matches!(
            record.lifecycle,
            WorkspaceLifecycle::Ready
                | WorkspaceLifecycle::Active
                | WorkspaceLifecycle::MergePending
        ) {
            record.transition(WorkspaceLifecycle::Discarded, unix_seconds())?;
            write_record_atomic(&self.record_path(&record.id), &record)?;
        }
        if matches!(
            record.lifecycle,
            WorkspaceLifecycle::Discarded | WorkspaceLifecycle::Merged
        ) {
            record.transition(WorkspaceLifecycle::Cleaning, unix_seconds())?;
            write_record_atomic(&self.record_path(&record.id), &record)?;
        }
        remove_materialization(self, &record)?;
        record.transition(WorkspaceLifecycle::Removed, unix_seconds())?;
        write_record_atomic(&self.record_path(&record.id), &record)?;
        self.remove_unlocked(&record.id)
    }

    fn check_capacity_unlocked(&self) -> Result<(), RegistryError> {
        let Some(capacity) = self.capacity else {
            return Ok(());
        };
        let active = self
            .load_all()?
            .into_iter()
            .filter(|record| {
                matches!(
                    record.lifecycle,
                    WorkspaceLifecycle::Ready
                        | WorkspaceLifecycle::Active
                        | WorkspaceLifecycle::MergePending
                )
            })
            .count();
        if active >= capacity {
            return Err(RegistryError::CapacityExhausted { active, capacity });
        }
        Ok(())
    }

    pub fn load(&self, id: &str) -> Result<Option<WorktreeRecord>, RegistryError> {
        let parsed = parse_registry_id(id)?;
        self.load_unlocked(parsed.as_str())
    }

    pub(super) fn load_unlocked(&self, id: &str) -> Result<Option<WorktreeRecord>, RegistryError> {
        let parsed = parse_registry_id(id)?;
        let path = self.record_path(parsed.as_str());
        match fs::read(&path) {
            Ok(bytes) => {
                let record = deserialize_record(&bytes)?;
                validate_record(&record, &self.root)?;
                if record.id != parsed.as_str() {
                    return Err(RegistryError::InvalidRecord {
                        message: format!("record id {} does not match filename {}", record.id, id),
                    });
                }
                Ok(Some(record))
            }
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
            records.push(self.load_unlocked(id)?.ok_or_else(|| RegistryError::Io {
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
        let _writer = self.acquire_writer()?;
        let parsed = parse_registry_id(id)?;
        let mut record = self
            .load_unlocked(parsed.as_str())?
            .ok_or_else(|| RegistryError::UnknownWorktree { id: id.to_owned() })?;
        record.transition(next, now)?;
        write_record_atomic(&self.record_path(parsed.as_str()), &record)?;
        Ok(record)
    }

    #[cfg(test)]
    pub(crate) fn remove(&self, id: &str) -> Result<(), RegistryError> {
        let _writer = self.acquire_writer()?;
        self.remove_unlocked(id)
    }

    pub(super) fn remove_unlocked(&self, id: &str) -> Result<(), RegistryError> {
        let parsed = parse_registry_id(id)?;
        match fs::remove_file(self.record_path(parsed.as_str())) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(RegistryError::Io {
                message: format!("cannot remove record {id}: {error}"),
            }),
        }
    }

    pub(super) fn record_path(&self, id: &str) -> PathBuf {
        WorktreeRegistry::registry_dir(&self.root).join(format!("{id}.json"))
    }

    fn register_unlocked(&self, record: &WorktreeRecord) -> Result<(), RegistryError> {
        validate_record(record, &self.root)?;
        write_record_atomic(&self.record_path(&record.id), record)
    }

    fn registry_dir(root: &Path) -> PathBuf {
        root.join(REGISTRY_DIRECTORY)
    }

    fn worktrees_dir(root: &Path) -> PathBuf {
        root.join(WORKTREES_DIRECTORY)
    }

    fn baselines_dir(root: &Path) -> PathBuf {
        root.join(BASELINES_DIRECTORY)
    }

    fn transactions_dir(root: &Path) -> PathBuf {
        root.join(TRANSACTIONS_DIRECTORY)
    }

    pub(super) fn baseline_dir(&self, id: &str) -> PathBuf {
        Self::baselines_dir(&self.root).join(id)
    }

    pub(super) fn transaction_dir(&self, id: &str) -> PathBuf {
        Self::transactions_dir(&self.root).join(id)
    }

    pub(super) fn acquire_writer(&self) -> Result<RegistryWriteGuard, RegistryError> {
        let path = Self::registry_dir(&self.root).join(WRITER_LOCK_FILE);
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&path)
            .map_err(|error| RegistryError::Io {
                message: format!("cannot open registry writer lock: {error}"),
            })?;
        file.lock().map_err(|error| RegistryError::Io {
            message: format!("cannot acquire registry writer lock: {error}"),
        })?;
        Ok(RegistryWriteGuard { _file: file })
    }
}

#[derive(Debug)]
pub(super) struct RegistryWriteGuard {
    _file: File,
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
    /// Prepared merge transactions restored to their pre-merge parent bytes.
    pub merges_rolled_back: Vec<String>,
    /// Fully applied transactions whose durable Merged transition was completed.
    pub merges_completed: Vec<String>,
}

/// Result of the safe startup maintenance pass.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StartupMaintenanceReport {
    pub recovery: RecoveryReport,
    pub gc: GcReport,
}

impl WorktreeRegistry {
    /// Reconcile the registry after startup: remove interrupted worktrees and
    /// stale records, and report orphan directories for human review.
    ///
    /// The returned report describes what was found before recovery, so
    /// callers can audit which records were cleaned.
    pub fn recover(&self) -> Result<RecoveryReport, RegistryError> {
        let _writer = self.acquire_writer()?;
        let (merges_rolled_back, merges_completed) = super::merge::recover_transactions(self)?;
        let mut report = self.scan_unlocked()?;
        report.merges_rolled_back = merges_rolled_back;
        report.merges_completed = merges_completed;
        for record in &report.interrupted {
            remove_materialization(self, record)?;
            self.remove_unlocked(&record.id)?;
        }
        for record in &report.stale_records {
            remove_materialization(self, record)?;
            self.remove_unlocked(&record.id)?;
        }
        Ok(report)
    }

    /// Reconcile interrupted state and collect records owned by dead
    /// processes. Live-process records are retained because another session
    /// may still be using the shared registry.
    pub fn startup_maintenance(&self) -> Result<StartupMaintenanceReport, RegistryError> {
        let recovery = self.recover()?;
        let gc = self.gc(&GcOptions {
            now: unix_seconds(),
            max_age_seconds: 0,
            disk_budget_bytes: None,
            owner_liveness: Box::new(owner_process_is_alive),
            dry_run: false,
        })?;
        Ok(StartupMaintenanceReport { recovery, gc })
    }

    fn scan_unlocked(&self) -> Result<RecoveryReport, RegistryError> {
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
    /// Returns whether the worktree's owner is still alive. A worktree whose
    /// owner is alive is never a candidate.
    pub owner_liveness: Box<dyn Fn(&WorktreeRecord) -> bool + Send + Sync>,
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
        let _writer = self.acquire_writer()?;
        let records = self.load_all()?;
        let mut eligible = records
            .into_iter()
            .filter(|record| {
                matches!(
                    record.lifecycle,
                    WorkspaceLifecycle::Ready
                        | WorkspaceLifecycle::Active
                        | WorkspaceLifecycle::Merged
                        | WorkspaceLifecycle::Discarded
                )
            })
            .filter(|record| !(options.owner_liveness)(record))
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
            let size = dir_size(&record.dest).map_err(|error| RegistryError::Io {
                message: format!(
                    "cannot calculate worktree size {}: {error}",
                    record.dest.display()
                ),
            })?;
            remove_materialization(self, &record)?;
            self.remove_unlocked(&record.id)?;
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
    #[error("worktree is already registered: {id}")]
    AlreadyRegistered { id: String },
    #[error("invalid lifecycle transition: {from:?} -> {to:?}")]
    InvalidTransition {
        from: WorkspaceLifecycle,
        to: WorkspaceLifecycle,
    },
    #[error("managed worktree capacity exhausted: {active}/{capacity} live")]
    CapacityExhausted { active: usize, capacity: usize },
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
    digest.update(unix_nanos().to_le_bytes());
    digest.update(std::process::id().to_le_bytes());
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

fn unix_nanos() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0)
}

fn owner_process_is_alive(record: &WorktreeRecord) -> bool {
    process_is_alive(record.owner_pid)
}

#[cfg(unix)]
fn process_is_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    let result = unsafe { libc::kill(pid as libc::pid_t, 0) };
    result == 0 || io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(windows)]
fn process_is_alive(pid: u32) -> bool {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
    if pid == 0 {
        return false;
    }
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if handle.is_null() {
        return false;
    }
    unsafe { CloseHandle(handle) };
    true
}

fn parse_registry_id(id: &str) -> Result<WorkspaceId, RegistryError> {
    let parsed = WorkspaceId::parse(id).map_err(|error| RegistryError::InvalidRecord {
        message: format!("invalid worktree id {id}: {error}"),
    })?;
    if !parsed
        .as_str()
        .starts_with(&format!("{}-", WorkspaceKind::ManagedChild.tag()))
    {
        return Err(RegistryError::InvalidRecord {
            message: format!("registry id {id} is not a managed child id"),
        });
    }
    Ok(parsed)
}

fn validate_record(record: &WorktreeRecord, root: &Path) -> Result<(), RegistryError> {
    let parsed_id = parse_registry_id(&record.id)?;
    if record.kind != WorkspaceKind::ManagedChild
        || !record.id.starts_with(&format!("{}-", record.kind.tag()))
        || parsed_id.as_str() != record.id
    {
        return Err(RegistryError::InvalidRecord {
            message: format!("id {} does not identify a managed child", record.id),
        });
    }
    if !record.source.is_absolute() {
        return Err(RegistryError::InvalidRecord {
            message: format!("source is not absolute: {}", record.source.display()),
        });
    }
    match fs::canonicalize(&record.source) {
        Ok(canonical_source) if canonical_source == record.source => {}
        Ok(_) => {
            return Err(RegistryError::InvalidRecord {
                message: format!("source is not canonical: {}", record.source.display()),
            });
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(RegistryError::InvalidRecord {
                message: format!("cannot resolve source {}: {error}", record.source.display()),
            });
        }
    }
    if !record.dest.is_absolute() {
        return Err(RegistryError::InvalidRecord {
            message: format!("dest is not absolute: {}", record.dest.display()),
        });
    }
    let normalized_dest = normalize_registry_path(&record.dest)?;
    let worktrees_dir = WorktreeRegistry::worktrees_dir(root);
    if normalized_dest.parent() != Some(worktrees_dir.as_path())
        || normalized_dest
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
    let resolved_dest = resolve_registry_path(&normalized_dest)?;
    let canonical_worktrees =
        fs::canonicalize(&worktrees_dir).map_err(|error| RegistryError::InvalidRecord {
            message: format!("cannot resolve worktrees root: {error}"),
        })?;
    if !resolved_dest.starts_with(&canonical_worktrees) {
        return Err(RegistryError::InvalidRecord {
            message: format!(
                "destination {} resolves outside the registry worktrees root",
                record.dest.display()
            ),
        });
    }
    Ok(())
}

fn normalize_registry_path(path: &Path) -> Result<PathBuf, RegistryError> {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            std::path::Component::RootDir => normalized.push(component.as_os_str()),
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                if !normalized.pop() {
                    return Err(RegistryError::InvalidRecord {
                        message: format!("destination escapes filesystem root: {}", path.display()),
                    });
                }
            }
            std::path::Component::Normal(part) => normalized.push(part),
        }
    }
    Ok(normalized)
}

fn resolve_registry_path(path: &Path) -> Result<PathBuf, RegistryError> {
    let mut ancestor = path.to_path_buf();
    let mut suffix = PathBuf::new();
    loop {
        match fs::symlink_metadata(&ancestor) {
            Ok(_) => {
                let canonical =
                    fs::canonicalize(&ancestor).map_err(|error| RegistryError::InvalidRecord {
                        message: format!("cannot resolve destination ancestor: {error}"),
                    })?;
                return Ok(canonical.join(suffix));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let name = ancestor
                    .file_name()
                    .ok_or_else(|| RegistryError::InvalidRecord {
                        message: format!(
                            "destination has no existing ancestor: {}",
                            path.display()
                        ),
                    })?;
                let mut next = PathBuf::from(name);
                next.push(&suffix);
                suffix = next;
                ancestor = ancestor
                    .parent()
                    .ok_or_else(|| RegistryError::InvalidRecord {
                        message: format!("destination has no parent: {}", path.display()),
                    })?
                    .to_path_buf();
            }
            Err(error) => {
                return Err(RegistryError::InvalidRecord {
                    message: format!("cannot inspect destination ancestor: {error}"),
                });
            }
        }
    }
}

#[cfg(test)]
mod tests;
