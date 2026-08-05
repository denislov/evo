//! Single-owner hunk attribution and snapshot actor.

use std::collections::{BTreeMap, VecDeque};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, oneshot};
use workspace_runtime::api::WorkspaceHandle;

use crate::{
    ChangeReceipt, ChangeTrackerError, FsChangeKind, FsEvent, FsEventService, SemanticEvent,
    WatchOptions,
};

mod diff;
mod observation;

use diff::{HunkIdentity, best_identity_match, bounded_unified_diff, parse_hunks};
#[cfg(test)]
use observation::revision;
use observation::{ObservedFile, normalize_relative, read_observed};

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
    pub unified_diff: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrackedFileSnapshot {
    pub path: PathBuf,
    pub target_fingerprint: Option<String>,
    pub before_revision: Option<String>,
    pub after_revision: String,
    pub source: ChangeSource,
    pub context: Option<TrackingContext>,
    pub hunks: Vec<HunkSnapshot>,
    pub updated_at: SystemTime,
}

/// One immutable attribution decision in actor arrival order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangeFactSnapshot {
    pub path: PathBuf,
    pub target_fingerprint: Option<String>,
    pub before_revision: Option<String>,
    pub after_revision: String,
    pub source: ChangeSource,
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
        let runtime = tokio::runtime::Handle::try_current().map_err(|error| {
            ChangeTrackerError::WatchFailed {
                message: format!("hunk tracker requires an active Tokio runtime: {error}"),
            }
        })?;
        let task = runtime.spawn(run_actor(ActorState::new(root, options), receiver));
        Ok(Self {
            handle: HunkTrackerHandle { commands },
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
}

enum CommandKind {
    Receipt {
        receipt: ChangeReceipt,
        source: ChangeSource,
        context: TrackingContext,
    },
    FsEvent(FsEvent),
    Shutdown,
}

struct PendingReceipt {
    path: PathBuf,
    after_revision: String,
    expires: Instant,
}

struct PendingEvent {
    event: SemanticEvent,
    observed: ObservedFile,
    expires: Instant,
}

#[derive(Default)]
struct FileState {
    snapshot: Option<TrackedFileSnapshot>,
    content: Option<Vec<u8>>,
    identities: Vec<HunkIdentity>,
    agent_touched: bool,
}

struct ActorState {
    root: PathBuf,
    options: HunkTrackerOptions,
    files: BTreeMap<PathBuf, FileState>,
    pending_receipts: VecDeque<PendingReceipt>,
    pending_events: VecDeque<PendingEvent>,
    facts: VecDeque<ChangeFactSnapshot>,
    next_hunk: u64,
    history_bytes: usize,
    reconcile: ReconcileState,
}

impl ActorState {
    fn new(root: PathBuf, options: HunkTrackerOptions) -> Self {
        Self {
            root,
            options,
            files: BTreeMap::new(),
            pending_receipts: VecDeque::new(),
            pending_events: VecDeque::new(),
            facts: VecDeque::new(),
            next_hunk: 1,
            history_bytes: 0,
            reconcile: ReconcileState::Ready,
        }
    }

    fn record_receipt(
        &mut self,
        receipt: ChangeReceipt,
        source: ChangeSource,
        context: TrackingContext,
    ) -> Result<(), ChangeTrackerError> {
        self.flush_expired()?;
        if !source.accepts_receipt() {
            return Err(ChangeTrackerError::InvalidFact {
                message: format!("{source:?} cannot be submitted as a mutation receipt"),
            });
        }
        validate_context(&context)?;
        validate_revision(&receipt.after_revision, "after_revision")?;
        if let Some(before) = receipt.before_revision.as_deref() {
            validate_revision(before, "before_revision")?;
        }
        if receipt.target_fingerprint.is_empty() || receipt.origin.is_empty() {
            return Err(ChangeTrackerError::InvalidFact {
                message: "receipt requires target_fingerprint and origin".into(),
            });
        }
        if receipt
            .unified_diff
            .as_ref()
            .is_some_and(|diff| diff.len() > self.options.max_diff_bytes)
        {
            return Err(ChangeTrackerError::BudgetExceeded {
                message: format!("receipt diff exceeds {} bytes", self.options.max_diff_bytes),
            });
        }
        let path = normalize_relative(Path::new(&receipt.path))?;
        self.ensure_file_budget(&path)?;
        self.ensure_fact_budget()?;

        let matching_event = self.pending_events.iter().position(|pending| {
            pending.event.path == path && pending.observed.revision == receipt.after_revision
        });
        if let Some(index) = matching_event {
            self.pending_events.remove(index);
        } else if self.pending_receipts.len() >= self.options.max_pending_facts {
            return Err(ChangeTrackerError::BudgetExceeded {
                message: "pending receipt budget exhausted".into(),
            });
        }

        let observed = read_observed(&self.root, &path, self.options.max_content_bytes)?;
        let content = (observed.revision == receipt.after_revision)
            .then_some(observed.content)
            .flatten();
        self.apply_change(
            path.clone(),
            receipt.target_fingerprint.clone().into(),
            receipt.before_revision.clone(),
            receipt.after_revision.clone(),
            source,
            Some(context),
            receipt.unified_diff.clone(),
            content,
        )?;

        if matching_event.is_none() {
            self.pending_receipts.push_back(PendingReceipt {
                path,
                after_revision: receipt.after_revision,
                expires: Instant::now() + self.options.causal_window,
            });
        }
        Ok(())
    }

    fn observe(&mut self, event: FsEvent) -> Result<(), ChangeTrackerError> {
        self.flush_expired()?;
        match event {
            FsEvent::Git(_) => Ok(()),
            FsEvent::WatchGap { lost } => {
                self.reconcile = ReconcileState::Required {
                    lost: match self.reconcile {
                        ReconcileState::Ready => lost,
                        ReconcileState::Required { lost: previous } => {
                            previous.saturating_add(lost)
                        }
                    },
                };
                self.pending_events.clear();
                self.pending_receipts.clear();
                Ok(())
            }
            FsEvent::Workspace(event) => self.observe_workspace(event),
        }
    }

    fn observe_workspace(&mut self, event: SemanticEvent) -> Result<(), ChangeTrackerError> {
        if event.root != self.root {
            return Err(ChangeTrackerError::InvalidFact {
                message: format!(
                    "event root {} does not match tracker root {}",
                    event.root.display(),
                    self.root.display()
                ),
            });
        }
        let path = normalize_relative(&event.path)?;
        if event.kind == FsChangeKind::Renamed {
            let from = event
                .from
                .as_deref()
                .ok_or_else(|| ChangeTrackerError::InvalidFact {
                    message: "rename event is missing its source path".into(),
                })?;
            let from = normalize_relative(from)?;
            if let Some(mut state) = self.files.remove(&from) {
                if self.files.contains_key(&path) {
                    return Err(ChangeTrackerError::InvalidFact {
                        message: format!(
                            "rename destination already has tracked state: {}",
                            path.display()
                        ),
                    });
                }
                self.ensure_file_budget(&path)?;
                if let Some(snapshot) = &mut state.snapshot {
                    snapshot.path = path.clone();
                }
                self.files.insert(path.clone(), state);
            }
            for receipt in &mut self.pending_receipts {
                if receipt.path == from {
                    receipt.path = path.clone();
                }
            }
            for pending in &mut self.pending_events {
                if pending.event.path == from {
                    pending.event.path = path.clone();
                }
            }
        }
        self.ensure_file_budget(&path)?;
        let observed = read_observed(&self.root, &path, self.options.max_content_bytes)?;
        if let Some(index) = self
            .pending_receipts
            .iter()
            .position(|pending| pending.path == path && pending.after_revision == observed.revision)
        {
            self.pending_receipts.remove(index);
            if let Some(state) = self.files.get_mut(&path) {
                state.content = observed.content;
            }
            return Ok(());
        }
        if self.pending_events.len() >= self.options.max_pending_facts {
            return Err(ChangeTrackerError::BudgetExceeded {
                message: "pending filesystem event budget exhausted".into(),
            });
        }
        self.pending_events.push_back(PendingEvent {
            event: SemanticEvent { path, ..event },
            observed,
            expires: Instant::now() + self.options.causal_window,
        });
        Ok(())
    }

    fn flush_expired(&mut self) -> Result<(), ChangeTrackerError> {
        let now = Instant::now();
        self.pending_receipts
            .retain(|pending| pending.expires > now);
        let mut expired = Vec::new();
        while self
            .pending_events
            .front()
            .is_some_and(|pending| pending.expires <= now)
        {
            if let Some(pending) = self.pending_events.pop_front() {
                expired.push(pending);
            }
        }
        for pending in expired {
            self.apply_external(pending)?;
        }
        Ok(())
    }

    fn flush_all_events(&mut self) -> Result<(), ChangeTrackerError> {
        while let Some(pending) = self.pending_events.pop_front() {
            self.apply_external(pending)?;
        }
        Ok(())
    }

    fn apply_external(&mut self, pending: PendingEvent) -> Result<(), ChangeTrackerError> {
        let path = pending.event.path;
        let state = self.files.entry(path.clone()).or_default();
        let source = if state.agent_touched {
            ChangeSource::ExternalEditOnAgentFile
        } else {
            ChangeSource::ExternalEdit
        };
        let before_revision = state
            .snapshot
            .as_ref()
            .map(|snapshot| snapshot.after_revision.clone());
        let diff = state
            .content
            .as_deref()
            .zip(pending.observed.content.as_deref())
            .and_then(|(before, after)| {
                bounded_unified_diff(
                    &path,
                    before,
                    after,
                    self.options.max_diff_bytes,
                    self.options.max_diff_lines,
                )
            });
        self.apply_change(
            path,
            None,
            before_revision,
            pending.observed.revision,
            source,
            None,
            diff,
            pending.observed.content,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn apply_change(
        &mut self,
        path: PathBuf,
        target_fingerprint: Option<String>,
        before_revision: Option<String>,
        after_revision: String,
        source: ChangeSource,
        context: Option<TrackingContext>,
        unified_diff: Option<String>,
        content: Option<Vec<u8>>,
    ) -> Result<(), ChangeTrackerError> {
        let parsed = parse_hunks(unified_diff.as_deref(), &after_revision);
        if parsed.len() > self.options.max_hunks_per_file {
            return Err(ChangeTrackerError::BudgetExceeded {
                message: format!(
                    "{} hunks exceed the per-file budget of {}",
                    parsed.len(),
                    self.options.max_hunks_per_file
                ),
            });
        }
        self.ensure_fact_budget()?;
        let history_bytes = parsed
            .iter()
            .filter_map(|hunk| hunk.diff.as_ref())
            .fold(0_usize, |total, diff| total.saturating_add(diff.len()));
        if self.history_bytes.saturating_add(history_bytes) > self.options.max_history_bytes {
            return Err(ChangeTrackerError::BudgetExceeded {
                message: format!(
                    "hunk history exceeds the {} byte budget",
                    self.options.max_history_bytes
                ),
            });
        }
        let state = self.files.entry(path.clone()).or_default();
        let old = std::mem::take(&mut state.identities);
        let mut used = vec![false; old.len()];
        let mut identities = Vec::with_capacity(parsed.len());
        let mut hunks = Vec::with_capacity(parsed.len());
        for parsed in parsed {
            let matched = best_identity_match(&parsed, &old, &used);
            let id = if let Some(index) = matched {
                used[index] = true;
                old[index].id.clone()
            } else {
                let id = HunkId(format!("hunk-{:016x}", self.next_hunk));
                self.next_hunk = self.next_hunk.saturating_add(1);
                id
            };
            identities.push(HunkIdentity {
                id: id.clone(),
                fingerprint: parsed.fingerprint,
                range: parsed.range,
            });
            hunks.push(HunkSnapshot {
                id,
                range: parsed.range,
                source,
                context: context.clone(),
                before_revision: before_revision.clone(),
                after_revision: after_revision.clone(),
                unified_diff: parsed.diff,
            });
        }
        state.identities = identities;
        state.content = content;
        state.agent_touched |= source == ChangeSource::AgentEdit;
        let recorded_at = SystemTime::now();
        let snapshot = TrackedFileSnapshot {
            path: path.clone(),
            target_fingerprint: target_fingerprint.clone(),
            before_revision: before_revision.clone(),
            after_revision: after_revision.clone(),
            source,
            context: context.clone(),
            hunks: hunks.clone(),
            updated_at: recorded_at,
        };
        state.snapshot = Some(snapshot);
        self.facts.push_back(ChangeFactSnapshot {
            path,
            target_fingerprint,
            before_revision,
            after_revision,
            source,
            context,
            hunks,
            recorded_at,
        });
        self.history_bytes = self.history_bytes.saturating_add(history_bytes);
        Ok(())
    }

    fn ensure_fact_budget(&self) -> Result<(), ChangeTrackerError> {
        if self.facts.len() >= self.options.max_change_facts {
            return Err(ChangeTrackerError::BudgetExceeded {
                message: format!(
                    "change fact budget of {} exhausted",
                    self.options.max_change_facts
                ),
            });
        }
        Ok(())
    }

    fn ensure_file_budget(&self, path: &Path) -> Result<(), ChangeTrackerError> {
        if !self.files.contains_key(path) && self.files.len() >= self.options.max_files {
            return Err(ChangeTrackerError::BudgetExceeded {
                message: format!(
                    "tracked file budget of {} exhausted",
                    self.options.max_files
                ),
            });
        }
        Ok(())
    }

    fn snapshot(&mut self) -> Result<HunkTrackerSnapshot, ChangeTrackerError> {
        self.flush_expired()?;
        Ok(HunkTrackerSnapshot {
            files: self
                .files
                .values()
                .filter_map(|state| state.snapshot.clone())
                .collect(),
            facts: self.facts.iter().cloned().collect(),
            reconcile: self.reconcile,
            pending_receipts: self.pending_receipts.len(),
            pending_events: self.pending_events.len(),
        })
    }
}

async fn run_actor(mut state: ActorState, mut commands: mpsc::Receiver<Command>) {
    while let Some(command) = commands.recv().await {
        match command {
            Command::Snapshot { reply } => {
                let _ = reply.send(state.snapshot());
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
                    CommandKind::Shutdown => state.flush_all_events(),
                };
                let _ = reply.send(result);
                if shutdown {
                    return;
                }
            }
        }
    }
}

fn validate_options(options: &HunkTrackerOptions) -> Result<(), ChangeTrackerError> {
    let invalid = [
        (
            options.causal_window.is_zero(),
            "causal_window must be non-zero",
        ),
        (options.command_queue == 0, "command_queue must be non-zero"),
        (
            options.max_pending_facts == 0,
            "max_pending_facts must be non-zero",
        ),
        (
            options.max_change_facts == 0,
            "max_change_facts must be non-zero",
        ),
        (options.max_files == 0, "max_files must be non-zero"),
        (
            options.max_hunks_per_file == 0,
            "max_hunks_per_file must be non-zero",
        ),
        (
            options.max_diff_bytes == 0,
            "max_diff_bytes must be non-zero",
        ),
        (
            options.max_diff_lines == 0,
            "max_diff_lines must be non-zero",
        ),
        (
            options.max_history_bytes == 0,
            "max_history_bytes must be non-zero",
        ),
        (
            options.max_content_bytes == 0,
            "max_content_bytes must be non-zero",
        ),
    ];
    if let Some((_, message)) = invalid.into_iter().find(|(invalid, _)| *invalid) {
        return Err(ChangeTrackerError::InvalidOptions {
            message: message.into(),
        });
    }
    Ok(())
}

fn validate_context(context: &TrackingContext) -> Result<(), ChangeTrackerError> {
    if context.session_id.is_empty()
        || context.turn_id.is_empty()
        || context.operation_id.is_empty()
    {
        return Err(ChangeTrackerError::InvalidFact {
            message: "tracking context requires session_id, turn_id, and operation_id".into(),
        });
    }
    Ok(())
}

fn validate_revision(revision: &str, field: &str) -> Result<(), ChangeTrackerError> {
    if revision.len() != 64
        || !revision
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(ChangeTrackerError::InvalidFact {
            message: format!("{field} must be a SHA-256 content revision"),
        });
    }
    Ok(())
}

#[cfg(test)]
#[path = "hunk_tests.rs"]
mod tests;
