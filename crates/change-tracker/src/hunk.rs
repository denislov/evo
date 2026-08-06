//! Single-owner hunk attribution and snapshot actor.

use std::collections::{BTreeMap, VecDeque};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, oneshot, watch};
use workspace_runtime::api::WorkspaceHandle;

use crate::{
    ChangeReceipt, ChangeTrackerError, FsChangeKind, FsEvent, FsEventService, SemanticEvent,
    WatchOptions,
};

mod actor;
mod checkpoint;
mod diff;
mod observation;
mod reconstruct;
mod state;
mod validation;

use diff::{
    HunkIdentity, best_identity_match, bounded_unified_diff, parse_hunks, replace_line_range,
};
use observation::revision;
use observation::{ObservedFile, normalize_relative, read_observed};
use reconstruct::baseline_from_receipt;
use state::{FileState, FileVersion};
use validation::{validate_context, validate_options, validate_revision};

pub use checkpoint::{
    HunkCheckpointFile, HunkCheckpointIdentity, HunkCheckpointVersion, HunkTrackerCheckpoint,
};

#[derive(Debug, Clone)]
pub struct HunkTrackerOptions {
    pub causal_window: Duration,
    pub command_queue: usize,
    pub max_pending_facts: usize,
    pub max_change_facts: usize,
    pub max_files: usize,
    pub max_hunks_per_file: usize,
    pub max_diff_bytes: usize,
    pub max_diff_lines: usize,
    pub max_history_bytes: usize,
    pub max_content_bytes: usize,
}

impl Default for HunkTrackerOptions {
    fn default() -> Self {
        Self {
            causal_window: Duration::from_millis(750),
            command_queue: 256,
            max_pending_facts: 512,
            max_change_facts: 8192,
            max_files: 4096,
            max_hunks_per_file: 256,
            max_diff_bytes: 256 * 1024,
            max_diff_lines: 20_000,
            max_history_bytes: 32 * 1024 * 1024,
            max_content_bytes: 1024 * 1024,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrackingContext {
    pub session_id: String,
    pub turn_id: String,
    pub operation_id: String,
    pub tool_call_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeSource {
    AgentEdit,
    ExternalEditOnAgentFile,
    ExternalEdit,
    MergeApply,
    HookEdit,
}

impl ChangeSource {
    fn accepts_receipt(self) -> bool {
        matches!(self, Self::AgentEdit | Self::MergeApply | Self::HookEdit)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct HunkId(String);

impl HunkId {
    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn parse(value: impl Into<String>) -> Result<Self, ChangeTrackerError> {
        let value = value.into();
        if value.is_empty() || value.len() > 128 {
            return Err(ChangeTrackerError::InvalidFact {
                message: "hunk identity must contain between 1 and 128 bytes".into(),
            });
        }
        Ok(Self(value))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct HunkRange {
    pub old_start: usize,
    pub old_lines: usize,
    pub new_start: usize,
    pub new_lines: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HunkSnapshot {
    pub id: HunkId,
    pub range: HunkRange,
    pub source: ChangeSource,
    pub context: Option<TrackingContext>,
    pub before_revision: Option<String>,
    pub after_revision: String,
    pub after_exists: bool,
    pub unified_diff: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrackedFileSnapshot {
    pub recorded_sequence: u64,
    pub path: PathBuf,
    pub target_fingerprint: Option<String>,
    pub before_revision: Option<String>,
    pub after_revision: String,
    pub after_exists: bool,
    pub source: ChangeSource,
    pub mutation_kind: String,
    pub context: Option<TrackingContext>,
    pub hunks: Vec<HunkSnapshot>,
    pub updated_at: SystemTime,
}

/// One immutable attribution decision in actor arrival order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangeFactSnapshot {
    pub recorded_sequence: u64,
    pub path: PathBuf,
    pub target_fingerprint: Option<String>,
    pub before_revision: Option<String>,
    pub after_revision: String,
    pub after_exists: bool,
    pub source: ChangeSource,
    pub mutation_kind: String,
    pub context: Option<TrackingContext>,
    pub hunks: Vec<HunkSnapshot>,
    pub recorded_at: SystemTime,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReconcileState {
    Ready,
    Required { lost: u64 },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HunkTrackerSnapshot {
    /// Latest attributed state for each path, sorted by path.
    pub files: Vec<TrackedFileSnapshot>,
    /// Immutable fact history in actor arrival order.
    pub facts: Vec<ChangeFactSnapshot>,
    pub reconcile: ReconcileState,
    pub pending_receipts: usize,
    pub pending_events: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RejectReplacement {
    Write(Vec<u8>),
    Delete,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RejectPlan {
    pub path: PathBuf,
    pub expected_sequence: u64,
    pub expected_revision: String,
    pub expected_exists: bool,
    pub target_fingerprint: String,
    pub replacement: RejectReplacement,
}

impl HunkTrackerSnapshot {
    fn empty() -> Self {
        Self {
            files: Vec::new(),
            facts: Vec::new(),
            reconcile: ReconcileState::Ready,
            pending_receipts: 0,
            pending_events: 0,
        }
    }
}

pub struct HunkTracker {
    handle: HunkTrackerHandle,
    task: tokio::task::JoinHandle<()>,
}

/// End-to-end workspace fact service: normalized watcher events are forwarded
/// into one `HunkTracker` actor with bounded backpressure.
pub struct HunkTrackingService {
    events: FsEventService,
    tracker: HunkTracker,
    forwarder: tokio::task::JoinHandle<Result<(), ChangeTrackerError>>,
}

#[derive(Debug, Clone)]
pub struct HunkTrackerHandle {
    commands: mpsc::Sender<Command>,
    snapshots: watch::Sender<HunkTrackerSnapshot>,
}

impl HunkTracker {
    pub fn start(
        root: impl AsRef<Path>,
        options: HunkTrackerOptions,
    ) -> Result<Self, ChangeTrackerError> {
        validate_options(&options)?;
        let root = std::fs::canonicalize(root.as_ref()).map_err(|error| {
            ChangeTrackerError::InvalidRoot {
                message: format!("cannot resolve {}: {error}", root.as_ref().display()),
            }
        })?;
        let (commands, receiver) = mpsc::channel(options.command_queue);
        let (snapshots, _) = watch::channel(HunkTrackerSnapshot::empty());
        let runtime = tokio::runtime::Handle::try_current().map_err(|error| {
            ChangeTrackerError::WatchFailed {
                message: format!("hunk tracker requires an active Tokio runtime: {error}"),
            }
        })?;
        let tick = options.causal_window;
        let task = runtime.spawn(run_actor(
            ActorState::new(root, options),
            receiver,
            snapshots.clone(),
            tick,
        ));
        Ok(Self {
            handle: HunkTrackerHandle {
                commands,
                snapshots,
            },
            task,
        })
    }

    pub fn handle(&self) -> HunkTrackerHandle {
        self.handle.clone()
    }

    pub async fn shutdown(self) -> Result<(), ChangeTrackerError> {
        self.handle.request(CommandKind::Shutdown).await?;
        self.task.await.map_err(|_| ChangeTrackerError::Shutdown)
    }
}

impl HunkTrackingService {
    pub fn start(
        workspace: &WorkspaceHandle,
        watch_options: WatchOptions,
        hunk_options: HunkTrackerOptions,
    ) -> Result<Self, ChangeTrackerError> {
        let events = FsEventService::start(workspace, watch_options)?;
        let mut receiver = events.events();
        let tracker = HunkTracker::start(workspace.root(), hunk_options)?;
        let handle = tracker.handle();
        let runtime = tokio::runtime::Handle::try_current().map_err(|error| {
            ChangeTrackerError::WatchFailed {
                message: format!("hunk event forwarder requires an active Tokio runtime: {error}"),
            }
        })?;
        let forwarder = runtime.spawn(async move {
            loop {
                match receiver.recv().await {
                    Ok(event) => handle.observe_wait(event).await?,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(lost)) => {
                        handle.observe_wait(FsEvent::WatchGap { lost }).await?;
                        let _ = handle.reconcile().await;
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => return Ok(()),
                }
            }
        });
        Ok(Self {
            events,
            tracker,
            forwarder,
        })
    }

    pub fn handle(&self) -> HunkTrackerHandle {
        self.tracker.handle()
    }

    pub fn snapshots(&self) -> watch::Receiver<HunkTrackerSnapshot> {
        self.tracker.handle().snapshots()
    }

    pub async fn shutdown(self) -> Result<(), ChangeTrackerError> {
        let Self {
            events,
            tracker,
            forwarder,
        } = self;
        events.shutdown();
        drop(events);
        forwarder
            .await
            .map_err(|_| ChangeTrackerError::Shutdown)??;
        tracker.shutdown().await
    }
}

impl HunkTrackerHandle {
    pub fn snapshots(&self) -> watch::Receiver<HunkTrackerSnapshot> {
        self.snapshots.subscribe()
    }

    pub async fn record_receipt(
        &self,
        receipt: ChangeReceipt,
        source: ChangeSource,
        context: TrackingContext,
    ) -> Result<(), ChangeTrackerError> {
        self.request(CommandKind::Receipt {
            receipt,
            source,
            context,
        })
        .await
    }

    pub async fn observe(&self, event: FsEvent) -> Result<(), ChangeTrackerError> {
        self.request(CommandKind::FsEvent(event)).await
    }

    async fn observe_wait(&self, event: FsEvent) -> Result<(), ChangeTrackerError> {
        let (reply, receiver) = oneshot::channel();
        self.commands
            .send(Command::Mutate {
                kind: Box::new(CommandKind::FsEvent(event)),
                reply,
            })
            .await
            .map_err(|_| ChangeTrackerError::Shutdown)?;
        receiver.await.map_err(|_| ChangeTrackerError::Shutdown)?
    }

    pub async fn snapshot(&self) -> Result<HunkTrackerSnapshot, ChangeTrackerError> {
        let (reply, receiver) = oneshot::channel();
        self.commands
            .try_send(Command::Snapshot { reply })
            .map_err(map_send_error)?;
        receiver.await.map_err(|_| ChangeTrackerError::Shutdown)?
    }

    pub async fn checkpoint(&self) -> Result<HunkTrackerCheckpoint, ChangeTrackerError> {
        let (reply, receiver) = oneshot::channel();
        self.commands
            .try_send(Command::Checkpoint { reply })
            .map_err(map_send_error)?;
        receiver.await.map_err(|_| ChangeTrackerError::Shutdown)?
    }

    pub async fn restore_checkpoint(
        &self,
        checkpoint: HunkTrackerCheckpoint,
    ) -> Result<(), ChangeTrackerError> {
        self.request(CommandKind::RestoreCheckpoint(checkpoint))
            .await
    }

    pub async fn reconcile(&self) -> Result<(), ChangeTrackerError> {
        self.request(CommandKind::Reconcile).await
    }

    pub async fn accept_hunk(
        &self,
        path: impl Into<PathBuf>,
        expected_sequence: u64,
        hunk_id: HunkId,
        expected_revision: impl Into<String>,
        expected_target_fingerprint: impl Into<String>,
    ) -> Result<(), ChangeTrackerError> {
        self.request(CommandKind::AcceptHunk {
            path: path.into(),
            expected_sequence,
            hunk_id,
            expected_revision: expected_revision.into(),
            expected_target_fingerprint: expected_target_fingerprint.into(),
        })
        .await
    }

    pub async fn accept_file(
        &self,
        path: impl Into<PathBuf>,
        expected_sequence: u64,
        expected_revision: impl Into<String>,
        expected_target_fingerprint: impl Into<String>,
    ) -> Result<(), ChangeTrackerError> {
        self.request(CommandKind::AcceptFile {
            path: path.into(),
            expected_sequence,
            expected_revision: expected_revision.into(),
            expected_target_fingerprint: expected_target_fingerprint.into(),
        })
        .await
    }

    pub async fn prepare_reject_hunk(
        &self,
        path: impl Into<PathBuf>,
        expected_sequence: u64,
        hunk_id: HunkId,
        expected_revision: impl Into<String>,
        expected_target_fingerprint: impl Into<String>,
    ) -> Result<RejectPlan, ChangeTrackerError> {
        let (reply, receiver) = oneshot::channel();
        self.commands
            .try_send(Command::PrepareRejectHunk {
                path: path.into(),
                expected_sequence,
                hunk_id,
                expected_revision: expected_revision.into(),
                expected_target_fingerprint: expected_target_fingerprint.into(),
                reply,
            })
            .map_err(map_send_error)?;
        receiver.await.map_err(|_| ChangeTrackerError::Shutdown)?
    }

    pub async fn prepare_reject_file(
        &self,
        path: impl Into<PathBuf>,
        expected_sequence: u64,
        expected_revision: impl Into<String>,
        expected_target_fingerprint: impl Into<String>,
    ) -> Result<RejectPlan, ChangeTrackerError> {
        let (reply, receiver) = oneshot::channel();
        self.commands
            .try_send(Command::PrepareRejectFile {
                path: path.into(),
                expected_sequence,
                expected_revision: expected_revision.into(),
                expected_target_fingerprint: expected_target_fingerprint.into(),
                reply,
            })
            .map_err(map_send_error)?;
        receiver.await.map_err(|_| ChangeTrackerError::Shutdown)?
    }

    async fn request(&self, kind: CommandKind) -> Result<(), ChangeTrackerError> {
        let (reply, receiver) = oneshot::channel();
        self.commands
            .try_send(Command::Mutate {
                kind: Box::new(kind),
                reply,
            })
            .map_err(map_send_error)?;
        receiver.await.map_err(|_| ChangeTrackerError::Shutdown)?
    }
}

fn map_send_error<T>(error: mpsc::error::TrySendError<T>) -> ChangeTrackerError {
    match error {
        mpsc::error::TrySendError::Full(_) => ChangeTrackerError::BudgetExceeded {
            message: "hunk tracker command queue is saturated".into(),
        },
        mpsc::error::TrySendError::Closed(_) => ChangeTrackerError::Shutdown,
    }
}

enum Command {
    Mutate {
        kind: Box<CommandKind>,
        reply: oneshot::Sender<Result<(), ChangeTrackerError>>,
    },
    Snapshot {
        reply: oneshot::Sender<Result<HunkTrackerSnapshot, ChangeTrackerError>>,
    },
    Checkpoint {
        reply: oneshot::Sender<Result<HunkTrackerCheckpoint, ChangeTrackerError>>,
    },
    PrepareRejectHunk {
        path: PathBuf,
        expected_sequence: u64,
        hunk_id: HunkId,
        expected_revision: String,
        expected_target_fingerprint: String,
        reply: oneshot::Sender<Result<RejectPlan, ChangeTrackerError>>,
    },
    PrepareRejectFile {
        path: PathBuf,
        expected_sequence: u64,
        expected_revision: String,
        expected_target_fingerprint: String,
        reply: oneshot::Sender<Result<RejectPlan, ChangeTrackerError>>,
    },
}

enum CommandKind {
    Receipt {
        receipt: ChangeReceipt,
        source: ChangeSource,
        context: TrackingContext,
    },
    FsEvent(FsEvent),
    AcceptHunk {
        path: PathBuf,
        expected_sequence: u64,
        hunk_id: HunkId,
        expected_revision: String,
        expected_target_fingerprint: String,
    },
    AcceptFile {
        path: PathBuf,
        expected_sequence: u64,
        expected_revision: String,
        expected_target_fingerprint: String,
    },
    RestoreCheckpoint(HunkTrackerCheckpoint),
    Reconcile,
    Shutdown,
}

struct PendingReceipt {
    path: PathBuf,
    after_revision: String,
    after_exists: bool,
    expires: Instant,
}

struct PendingEvent {
    event: SemanticEvent,
    observed: ObservedFile,
    expires: Instant,
}

struct ActorState {
    root: PathBuf,
    options: HunkTrackerOptions,
    files: BTreeMap<PathBuf, FileState>,
    pending_receipts: VecDeque<PendingReceipt>,
    pending_events: VecDeque<PendingEvent>,
    facts: VecDeque<ChangeFactSnapshot>,
    next_hunk: u64,
    next_fact: u64,
    history_bytes: usize,
    reconcile: ReconcileState,
}

async fn run_actor(
    mut state: ActorState,
    mut commands: mpsc::Receiver<Command>,
    snapshots: watch::Sender<HunkTrackerSnapshot>,
    tick: Duration,
) {
    let mut interval = tokio::time::interval(tick);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        let command = tokio::select! {
            command = commands.recv() => command,
            _ = interval.tick() => {
                if let Ok(snapshot) = state.snapshot() {
                    publish_snapshot(&snapshots, snapshot);
                }
                continue;
            }
        };
        let Some(command) = command else {
            return;
        };
        match command {
            Command::Checkpoint { reply } => {
                let result = state.checkpoint();
                if let Ok(checkpoint) = &result {
                    publish_snapshot(&snapshots, checkpoint.snapshot());
                }
                let _ = reply.send(result);
            }
            Command::Snapshot { reply } => {
                let result = state.snapshot();
                if let Ok(snapshot) = &result {
                    publish_snapshot(&snapshots, snapshot.clone());
                }
                let _ = reply.send(result);
            }
            Command::PrepareRejectHunk {
                path,
                expected_sequence,
                hunk_id,
                expected_revision,
                expected_target_fingerprint,
                reply,
            } => {
                let result = state.prepare_reject_hunk(
                    path,
                    expected_sequence,
                    hunk_id,
                    expected_revision,
                    expected_target_fingerprint,
                );
                let _ = reply.send(result);
            }
            Command::PrepareRejectFile {
                path,
                expected_sequence,
                expected_revision,
                expected_target_fingerprint,
                reply,
            } => {
                let result = state.prepare_reject_file(
                    path,
                    expected_sequence,
                    expected_revision,
                    expected_target_fingerprint,
                );
                let _ = reply.send(result);
            }
            Command::Mutate { kind, reply } => {
                let shutdown = matches!(*kind, CommandKind::Shutdown);
                let result = match *kind {
                    CommandKind::Receipt {
                        receipt,
                        source,
                        context,
                    } => state.record_receipt(receipt, source, context),
                    CommandKind::FsEvent(event) => state.observe(event),
                    CommandKind::AcceptHunk {
                        path,
                        expected_sequence,
                        hunk_id,
                        expected_revision,
                        expected_target_fingerprint,
                    } => state.accept_hunk(
                        path,
                        expected_sequence,
                        hunk_id,
                        expected_revision,
                        expected_target_fingerprint,
                    ),
                    CommandKind::AcceptFile {
                        path,
                        expected_sequence,
                        expected_revision,
                        expected_target_fingerprint,
                    } => state.accept_file(
                        path,
                        expected_sequence,
                        expected_revision,
                        expected_target_fingerprint,
                    ),
                    CommandKind::RestoreCheckpoint(checkpoint) => {
                        state.restore_checkpoint(checkpoint)
                    }
                    CommandKind::Reconcile => state.reconcile(),
                    CommandKind::Shutdown => state.flush_all_events(),
                };
                if result.is_ok()
                    && let Ok(snapshot) = state.snapshot()
                {
                    publish_snapshot(&snapshots, snapshot);
                }
                let _ = reply.send(result);
                if shutdown {
                    return;
                }
            }
        }
    }
}

fn publish_snapshot(snapshots: &watch::Sender<HunkTrackerSnapshot>, snapshot: HunkTrackerSnapshot) {
    snapshots.send_if_modified(|current| {
        if *current == snapshot {
            return false;
        }
        *current = snapshot;
        true
    });
}

fn unpatchable_review() -> ChangeTrackerError {
    ChangeTrackerError::InvalidFact {
        message: "review content is unavailable for hunk action".into(),
    }
}

fn replacement_for_version(version: &FileVersion) -> Result<RejectReplacement, ChangeTrackerError> {
    if !version.exists {
        return Ok(RejectReplacement::Delete);
    }
    version
        .content
        .clone()
        .map(RejectReplacement::Write)
        .ok_or_else(unpatchable_review)
}

#[cfg(test)]
#[path = "hunk_tests.rs"]
mod tests;
