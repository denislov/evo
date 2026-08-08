//! Background task registry: long-running processes with cursor-based output
//! spools, explicit gap reporting, owner-scoped termination, and wait/cancel
//! handles.
//!
//! Foreground execution is the one-shot [`run`](super::run): it awaits the
//! child, applies the hard timeout, and renders a bounded tail. Background
//! execution shares the same spawn/collect/terminate core (`SpawnedProcess`)
//! but runs in a detached driver task, keeps every byte of output in a bounded
//! spool with an explicit gap marker when bytes must be dropped, and is
//! terminated by its owner, by cancellation, or by registry shutdown instead
//! of a per-tool timeout.

use std::collections::HashMap;
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use tokio::sync::Notify;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use super::{OutputSink, PendingTermination, ProcessSpec, ProcessUpdateCallback, SpawnedProcess};

const DRIVER_JOIN_TIMEOUT: Duration = Duration::from_secs(10);

/// Opaque, process-unique identifier for one background task.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TaskId(u64);

impl TaskId {
    pub const fn from_u64(value: u64) -> Self {
        Self(value)
    }

    pub fn get(self) -> u64 {
        self.0
    }
}

impl fmt::Display for TaskId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

/// The product entity that owns a background task: an operation, a session, or
/// a managed worktree. Owners group tasks for listing and terminate en masse;
/// the owner string is intentionally opaque (an id, not a path or secret).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum TaskOwner {
    Operation(String),
    Session(String),
    Worktree(String),
}

impl TaskOwner {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Operation(_) => "operation",
            Self::Session(_) => "session",
            Self::Worktree(_) => "worktree",
        }
    }

    pub fn id(&self) -> &str {
        match self {
            Self::Operation(id) | Self::Session(id) | Self::Worktree(id) => id,
        }
    }
}

impl fmt::Display for TaskOwner {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}:{}", self.kind(), self.id())
    }
}

/// Terminal (or running) state of a background task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskState {
    Running,
    Completed { exit_code: Option<i32> },
    TimedOut,
    Cancelled,
    Failed { message: String },
}

impl TaskState {
    pub fn is_running(&self) -> bool {
        matches!(self, Self::Running)
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Completed { .. } => "completed",
            Self::TimedOut => "timed_out",
            Self::Cancelled => "cancelled",
            Self::Failed { .. } => "failed",
        }
    }
}

/// Explicit marker for output that was dropped before the reader observed it.
///
/// The spool is bounded: when the process produces more bytes than the budget
/// keeps, the oldest bytes are dropped and the gap is reported here so no
/// caller can mistake retained output for the complete stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OutputGap {
    pub dropped_bytes: u64,
}

/// Point-in-time view of one background task, without output contents.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskSnapshot {
    pub task_id: TaskId,
    pub owner: TaskOwner,
    pub spawned_at: SystemTime,
    pub state: TaskState,
    pub total_bytes: u64,
    pub gap: Option<OutputGap>,
}

/// Incremental output read from a cursor, with the next cursor position.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskOutputChunk {
    pub text: String,
    pub next_cursor: u64,
    pub gap: Option<OutputGap>,
}

/// Final report returned by `wait` once the task left the `Running` state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskReport {
    pub state: TaskState,
    pub output: String,
    pub total_bytes: u64,
    pub stdout_bytes: u64,
    pub stderr_bytes: u64,
    pub gap: Option<OutputGap>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskSpawnError {
    pub message: String,
}

impl fmt::Display for TaskSpawnError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for TaskSpawnError {}

/// Append-only, bounded spool of the merged stdout+stderr byte stream.
///
/// Bytes are written by the driver task and read by cursor. When the buffer
/// exceeds its budget the oldest bytes are dropped at a UTF-8 boundary and the
/// dropped count is tracked; readers that ask for a cursor before the oldest
/// retained byte receive an explicit [`OutputGap`].
struct OutputSpool {
    buffer: Vec<u8>,
    limit: usize,
    total_bytes: u64,
    dropped_bytes: u64,
    stdout_bytes: u64,
    stderr_bytes: u64,
}

impl OutputSpool {
    fn new(limit: usize) -> Self {
        Self {
            buffer: Vec::new(),
            limit,
            total_bytes: 0,
            dropped_bytes: 0,
            stdout_bytes: 0,
            stderr_bytes: 0,
        }
    }

    fn push_stdout(&mut self, data: &[u8], on_update: Option<&ProcessUpdateCallback>) {
        self.stdout_bytes = self.stdout_bytes.saturating_add(data.len() as u64);
        self.push_merged(data, on_update);
    }

    fn push_stderr(&mut self, data: &[u8], on_update: Option<&ProcessUpdateCallback>) {
        self.stderr_bytes = self.stderr_bytes.saturating_add(data.len() as u64);
        self.push_merged(data, on_update);
    }

    fn push_merged(&mut self, data: &[u8], on_update: Option<&ProcessUpdateCallback>) {
        self.total_bytes = self.total_bytes.saturating_add(data.len() as u64);
        self.buffer.extend_from_slice(data);
        if self.buffer.len() > self.limit {
            let drop = self.buffer.len() - self.limit;
            let mut split = drop;
            while split < self.buffer.len() && (self.buffer[split] & 0xC0) == 0x80 {
                split += 1;
            }
            self.dropped_bytes = self.dropped_bytes.saturating_add(split as u64);
            self.buffer.drain(..split);
        }
        if let Some(on_update) = on_update {
            on_update(String::from_utf8_lossy(&self.buffer).into_owned());
        }
    }

    /// Cursor semantics: `cursor` is the global byte offset the reader last
    /// consumed. The oldest retained byte sits at `dropped_bytes`, so a cursor
    /// below it means the reader missed bytes and receives an explicit gap.
    fn read_from(&self, cursor: u64) -> TaskOutputChunk {
        let base = self.dropped_bytes;
        let end = base + self.buffer.len() as u64;
        let (text, gap) = if cursor < base {
            (
                String::from_utf8_lossy(&self.buffer).into_owned(),
                Some(OutputGap {
                    dropped_bytes: base - cursor,
                }),
            )
        } else if cursor >= end {
            (String::new(), None)
        } else {
            (
                String::from_utf8_lossy(&self.buffer[(cursor - base) as usize..]).into_owned(),
                None,
            )
        };
        TaskOutputChunk {
            text,
            next_cursor: end,
            gap,
        }
    }

    fn render(&self) -> String {
        String::from_utf8_lossy(&self.buffer).into_owned()
    }

    fn gap(&self) -> Option<OutputGap> {
        (self.dropped_bytes > 0).then_some(OutputGap {
            dropped_bytes: self.dropped_bytes,
        })
    }
}

struct TaskShared {
    id: TaskId,
    owner: TaskOwner,
    spawned_at: SystemTime,
    cancel_token: CancellationToken,
    state: Mutex<TaskState>,
    spool: Mutex<OutputSpool>,
    notify: Notify,
}

impl TaskShared {
    fn finish(&self, state: TaskState) {
        *lock_shared(&self.state) = state;
        self.notify.notify_waiters();
    }

    fn snapshot(&self) -> TaskSnapshot {
        let spool = lock_shared(&self.spool);
        TaskSnapshot {
            task_id: self.id,
            owner: self.owner.clone(),
            spawned_at: self.spawned_at,
            state: lock_shared(&self.state).clone(),
            total_bytes: spool.total_bytes,
            gap: spool.gap(),
        }
    }

    fn report(&self) -> TaskReport {
        let spool = lock_shared(&self.spool);
        TaskReport {
            state: lock_shared(&self.state).clone(),
            output: spool.render(),
            total_bytes: spool.total_bytes,
            stdout_bytes: spool.stdout_bytes,
            stderr_bytes: spool.stderr_bytes,
            gap: spool.gap(),
        }
    }
}

fn lock_shared<T>(guard: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    guard
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Handle for one running or finished background task.
#[derive(Clone)]
pub struct TaskHandle {
    shared: Arc<TaskShared>,
}

impl fmt::Debug for TaskHandle {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TaskHandle")
            .field("task_id", &self.shared.id)
            .field("owner", &self.shared.owner)
            .finish()
    }
}

impl TaskHandle {
    pub fn task_id(&self) -> TaskId {
        self.shared.id
    }

    pub fn owner(&self) -> &TaskOwner {
        &self.shared.owner
    }

    pub fn snapshot(&self) -> TaskSnapshot {
        self.shared.snapshot()
    }

    /// Cheap state query without output or timing data.
    pub fn status(&self) -> TaskState {
        lock_shared(&self.shared.state).clone()
    }

    /// Incremental output since `cursor`. Bytes read are never returned twice;
    /// if the reader lagged behind the spool bound the chunk carries a gap.
    pub fn output(&self, cursor: u64) -> TaskOutputChunk {
        lock_shared(&self.shared.spool).read_from(cursor)
    }

    /// Request process-tree termination. Returns false when the task already
    /// left the `Running` state.
    pub fn cancel(&self) -> bool {
        let running = self.status().is_running();
        if running {
            self.shared.cancel_token.cancel();
        }
        running
    }

    /// Resolves when the task leaves the `Running` state, then returns the
    /// final report including the retained output and any gap marker.
    pub async fn wait(&self) -> TaskReport {
        loop {
            if !self.status().is_running() {
                return self.shared.report();
            }
            self.shared.notify.notified().await;
        }
    }
}

async fn background_driver(
    shared: Arc<TaskShared>,
    mut process: SpawnedProcess,
    timeout: Option<Duration>,
    on_update: Option<ProcessUpdateCallback>,
) {
    let mut writer = SpoolWriter(shared.clone());
    let timeout_sleep =
        timeout.map(|duration| Box::pin(tokio::time::sleep(duration)) as super::BoxTimeout);
    let termination = process
        .run_until_terminated(
            &mut writer,
            &shared.cancel_token,
            timeout_sleep,
            on_update.as_ref(),
        )
        .await;
    if !matches!(termination, PendingTermination::Completed(_)) {
        process.terminate_tree().await;
    } else {
        process.disarm();
    }
    process
        .drain_remaining(&mut writer, on_update.as_ref())
        .await;
    let state = match termination {
        PendingTermination::Completed(exit_code) => TaskState::Completed { exit_code },
        PendingTermination::TimedOut => TaskState::TimedOut,
        PendingTermination::Cancelled => TaskState::Cancelled,
        PendingTermination::Failed(message) => TaskState::Failed { message },
    };
    tracing::info!(
        target: "evo::lifecycle",
        domain = "task",
        phase = "finished",
        task_id = shared.id.get(),
        owner_kind = shared.owner.kind(),
        owner_id = shared.owner.id(),
        state = state.as_str(),
    );
    shared.finish(state);
}

/// Short-critical-section writer into the shared spool: the lock is held only
/// for one chunk copy, so concurrent `output(cursor)` reads observe partial
/// output while the process is still running.
struct SpoolWriter(Arc<TaskShared>);

impl OutputSink for SpoolWriter {
    fn push_stdout(&mut self, data: &[u8], on_update: Option<&ProcessUpdateCallback>) {
        lock_shared(&self.0.spool).push_stdout(data, on_update);
    }

    fn push_stderr(&mut self, data: &[u8], on_update: Option<&ProcessUpdateCallback>) {
        lock_shared(&self.0.spool).push_stderr(data, on_update);
    }
}

struct TaskRegistryInner {
    tasks: Mutex<HashMap<TaskId, TaskHandle>>,
    drivers: Mutex<HashMap<TaskId, JoinHandle<()>>>,
    next_id: AtomicU64,
    shutdown_token: CancellationToken,
}

/// Owner-scoped registry of background tasks for one runtime host.
///
/// Tasks survive the tool call that spawned them and are queried, waited on,
/// and cancelled through this registry. `shutdown` cancels every task and
/// joins its driver, which is the session-close policy; `terminate_all_for_owner`
/// applies the same termination to one owner group.
#[derive(Clone)]
pub struct TaskRegistry {
    inner: Arc<TaskRegistryInner>,
}

impl Default for TaskRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl TaskRegistry {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(TaskRegistryInner {
                tasks: Mutex::new(HashMap::new()),
                drivers: Mutex::new(HashMap::new()),
                next_id: AtomicU64::new(1),
                shutdown_token: CancellationToken::new(),
            }),
        }
    }

    /// Spawn a background task and register it under `owner`.
    ///
    /// `timeout` is the task budget: `None` means the task has no hard
    /// deadline and is bounded only by cancellation, owner termination, and
    /// registry shutdown (the background equivalent of the foreground tool
    /// timeout, which does not apply here). Spawn failures (missing program,
    /// containment attach failure) return `TaskSpawnError` without registering
    /// a task.
    pub async fn spawn(
        &self,
        spec: ProcessSpec,
        owner: TaskOwner,
        timeout: Option<Duration>,
    ) -> Result<TaskHandle, TaskSpawnError> {
        if self.inner.shutdown_token.is_cancelled() {
            return Err(TaskSpawnError {
                message: "task registry is shut down".into(),
            });
        }
        let process = SpawnedProcess::spawn(&spec)
            .await
            .map_err(|message| TaskSpawnError { message })?;
        let task_id = TaskId(self.inner.next_id.fetch_add(1, Ordering::Relaxed));
        let shared = Arc::new(TaskShared {
            id: task_id,
            owner: owner.clone(),
            spawned_at: SystemTime::now(),
            cancel_token: CancellationToken::new(),
            state: Mutex::new(TaskState::Running),
            spool: Mutex::new(OutputSpool::new(spec.output_budget.max_bytes)),
            notify: Notify::new(),
        });
        tracing::info!(
            target: "evo::lifecycle",
            domain = "task",
            phase = "started",
            task_id = task_id.get(),
            owner_kind = owner.kind(),
            owner_id = owner.id(),
            has_timeout = timeout.is_some(),
        );
        let driver_shared = shared.clone();
        let driver = tokio::spawn(background_driver(driver_shared, process, timeout, None));
        let handle = TaskHandle { shared };
        self.inner
            .tasks
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(task_id, handle.clone());
        self.inner
            .drivers
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(task_id, driver);
        Ok(handle)
    }

    pub fn task(&self, task_id: TaskId) -> Option<TaskHandle> {
        lock_shared(&self.inner.tasks).get(&task_id).cloned()
    }

    pub fn list(&self) -> Vec<TaskSnapshot> {
        let mut snapshots = lock_shared(&self.inner.tasks)
            .values()
            .map(TaskHandle::snapshot)
            .collect::<Vec<_>>();
        snapshots.sort_by_key(|snapshot| snapshot.task_id);
        snapshots
    }

    pub fn list_for_owner(&self, owner: &TaskOwner) -> Vec<TaskSnapshot> {
        let mut snapshots = lock_shared(&self.inner.tasks)
            .values()
            .filter(|handle| handle.owner() == owner)
            .map(TaskHandle::snapshot)
            .collect::<Vec<_>>();
        snapshots.sort_by_key(|snapshot| snapshot.task_id);
        snapshots
    }

    /// Cancel one task; returns false when it was already terminal.
    pub fn cancel(&self, task_id: TaskId) -> bool {
        self.task(task_id).is_some_and(|handle| handle.cancel())
    }

    /// Cancel every running task owned by `owner`; returns how many tasks were
    /// still running and therefore terminated.
    pub fn terminate_all_for_owner(&self, owner: &TaskOwner) -> usize {
        lock_shared(&self.inner.tasks)
            .values()
            .filter(|handle| handle.owner() == owner)
            .map(TaskHandle::cancel)
            .filter(|cancelled| *cancelled)
            .count()
    }

    /// Resolve when all listed tasks are terminal. Unknown ids report a
    /// `Failed` state with an "unknown task" message.
    pub async fn wait_all(&self, task_ids: &[TaskId]) -> Vec<(TaskId, TaskReport)> {
        let mut reports = Vec::with_capacity(task_ids.len());
        for task_id in task_ids {
            let report = match self.task(*task_id) {
                Some(handle) => handle.wait().await,
                None => TaskReport {
                    state: TaskState::Failed {
                        message: "unknown task".into(),
                    },
                    output: String::new(),
                    total_bytes: 0,
                    stdout_bytes: 0,
                    stderr_bytes: 0,
                    gap: None,
                },
            };
            reports.push((*task_id, report));
        }
        reports
    }

    /// Resolve when any of the listed tasks is terminal; returns its id and
    /// report, or `None` when the list is empty or every id is unknown.
    pub async fn wait_any(&self, task_ids: &[TaskId]) -> Option<(TaskId, TaskReport)> {
        let mut pending = Vec::new();
        for task_id in task_ids {
            let handle = self.task(*task_id)?;
            pending.push((*task_id, handle));
        }
        if pending.is_empty() {
            return None;
        }
        let mut set = tokio::task::JoinSet::new();
        for (task_id, handle) in pending {
            set.spawn(async move { (task_id, handle.wait().await) });
        }
        set.join_next().await?.ok()
    }

    /// Cancel every running task and join its driver. The registry stays
    /// readable (list/snapshot still return terminal history) but rejects new
    /// spawns. Returns how many tasks were still running.
    pub async fn shutdown(&self) -> usize {
        self.inner.shutdown_token.cancel();
        let running = lock_shared(&self.inner.tasks)
            .values()
            .map(TaskHandle::cancel)
            .filter(|cancelled| *cancelled)
            .count();
        let drivers = std::mem::take(&mut *lock_shared(&self.inner.drivers));
        for (_task_id, driver) in drivers {
            let _ = tokio::time::timeout(DRIVER_JOIN_TIMEOUT, driver).await;
        }
        running
    }
}

#[cfg(test)]
mod tests_background;
