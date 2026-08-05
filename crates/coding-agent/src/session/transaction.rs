use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
#[cfg(test)]
use std::sync::mpsc::SyncSender;
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, oneshot};

// A 100-checkpoint burst is the reliability-plan stress target. Keeping the
// entire burst plus 28% headroom bounded avoids producer failure during one
// slow fsync without allowing an unbounded command backlog.
const SESSION_TRANSACTION_WRITER_CAPACITY: usize = 128;
const SESSION_TRANSACTION_ENQUEUE_TIMEOUT: Duration = Duration::from_secs(5);
const SESSION_TRANSACTION_BLOCKING_RETRY_INTERVAL: Duration = Duration::from_millis(1);

use ai_protocol::api::conversation::Usage;
use futures::future::BoxFuture;
use serde_json::Value;

use super::manifest::SessionManifest;
use super::repository::{ManifestPatch, SessionHandle, SessionLogStore, SessionWriteLease};
use crate::events::outbox::{DurableOutboxIntent, DurableOutboxRecordCandidate};
use crate::kernel::error::CodingSessionError;
use crate::kernel::error::SessionWriteFailureReason;
use crate::mutex::{MutexExt, recover_poisoned};
use crate::operations::self_healing_edit::runner::{
    SelfHealingEditOutcome, SelfHealingEditRepairAttempt,
};
use crate::platform::time::{Clock, IdGenerator};
use crate::profiles::{ProfileId, ProfileKind};
use crate::session::event::{
    DiagnosticLevel, OperationKind, PersistedContentBlock, PersistedDelegationStatus,
    PersistedRole, PersistedRuntimeGenerationRef, PersistedSelfHealingEditCheckOutput,
    PersistedSelfHealingEditReplacement, PersistedToolResult, SessionEventData,
    SessionEventEnvelope,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TransactionState {
    Open,
    Committed,
    Aborted,
    Failed,
    InDoubt,
}

static SESSION_WRITER_REGISTRY: OnceLock<
    Mutex<HashMap<PathBuf, Arc<SessionTransactionWriterInner>>>,
> = OnceLock::new();

#[derive(Debug)]
pub(crate) struct SessionTransactionWriter {
    inner: Arc<SessionTransactionWriterInner>,
    owner: Arc<SessionWriterOwnerLease>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct SessionCommitReceipt {
    pub(crate) committed_session_sequence: Option<u64>,
}

#[derive(Debug)]
struct SessionTransactionWriterInner {
    sender: Mutex<Option<mpsc::Sender<SessionTransactionWriterEnvelope>>>,
    worker: Mutex<Option<std::thread::JoinHandle<()>>>,
    owners: AtomicUsize,
    #[cfg(test)]
    last_owner_release_pause: Mutex<Option<(SyncSender<()>, std::sync::mpsc::Receiver<()>)>>,
    #[cfg(test)]
    command_delay_millis: Arc<AtomicU64>,
    #[cfg(test)]
    enqueue_timeout_millis: AtomicU64,
    snapshot: Arc<Mutex<SessionManifest>>,
    committed_session_sequence: Arc<AtomicU64>,
    startup_storage_recoveries: Vec<String>,
    registry_key: PathBuf,
}

#[derive(Debug)]
struct SessionWriterOwnerLease {
    inner: Weak<SessionTransactionWriterInner>,
    released: AtomicBool,
}

#[derive(Debug)]
struct SessionTransactionWriterEnvelope {
    command: SessionTransactionWriterCommand,
    reply: oneshot::Sender<Result<SessionCommitReceipt, CodingSessionError>>,
}

#[derive(Debug)]
#[allow(
    clippy::large_enum_variant,
    reason = "bounded writer commands move complete durable batches through a single owner"
)]
enum SessionTransactionWriterCommand {
    InitializeSession {
        event: SessionEventEnvelope,
    },
    Checkpoint {
        events: Vec<SessionEventEnvelope>,
    },
    Finalize {
        events: Vec<SessionEventEnvelope>,
        outbox_records: Vec<DurableOutboxRecordCandidate>,
        updated_at: String,
        active_leaf_id: Option<String>,
    },
    CommitSessionMutation {
        events: Vec<SessionEventEnvelope>,
        outbox_records: Vec<DurableOutboxRecordCandidate>,
        manifest_patch: ManifestPatch,
        operation_id: Option<String>,
    },
    CommitSessionNameIfUnset {
        events: Vec<SessionEventEnvelope>,
        manifest_patch: ManifestPatch,
        operation_id: String,
    },
}

fn queue_saturated_error(timeout: Duration) -> CodingSessionError {
    CodingSessionError::SessionWriteFailure {
        reason: SessionWriteFailureReason::QueueSaturated,
        message: format!(
            "session transaction writer queue remained saturated at bounded capacity {} for {} ms",
            SESSION_TRANSACTION_WRITER_CAPACITY,
            timeout.as_millis()
        ),
    }
}

mod writer;

impl Clone for SessionTransactionWriter {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            owner: self.owner.clone(),
        }
    }
}

impl SessionWriterOwnerLease {
    fn release(&self) -> Result<(), CodingSessionError> {
        if self.released.swap(true, Ordering::AcqRel) {
            return Ok(());
        }
        let Some(inner) = self.inner.upgrade() else {
            return Ok(());
        };
        inner.release_owner()
    }
}

impl Drop for SessionWriterOwnerLease {
    fn drop(&mut self) {
        let _ = self.release();
    }
}

impl SessionTransactionWriterInner {
    fn acquire_owner(&self) {
        self.owners.fetch_add(1, Ordering::AcqRel);
    }

    fn release_owner(self: &Arc<Self>) -> Result<(), CodingSessionError> {
        // Serialize the zero-owner transition with registry lookup/reuse.
        // Otherwise a concurrent `new` can observe this actor as open after
        // `owners` reaches zero but before the sender, worker, and OS lease
        // have been closed, then acquire a handle to an actor that is already
        // committed to shutdown.
        let registry = SESSION_WRITER_REGISTRY.get_or_init(|| Mutex::new(HashMap::new()));
        let mut registry = registry.lock_resource("session writer registry")?;
        if self.owners.fetch_sub(1, Ordering::AcqRel) != 1 {
            return Ok(());
        }
        #[cfg(test)]
        if let Some((entered, release)) = self
            .last_owner_release_pause
            .lock_resource("test session writer release pause")?
            .take()
        {
            let _ = entered.send(());
            let _ = release.recv();
        }
        let result = self.close_and_join();
        if registry
            .get(&self.registry_key)
            .is_some_and(|registered| Arc::ptr_eq(registered, self))
        {
            registry.remove(&self.registry_key);
        }
        result
    }

    fn is_open(&self) -> Result<bool, CodingSessionError> {
        Ok(self
            .sender
            .lock_resource("session transaction writer sender")?
            .is_some())
    }

    fn close_and_join(&self) -> Result<(), CodingSessionError> {
        self.sender
            .lock_resource("session transaction writer sender")?
            .take();
        let worker = self
            .worker
            .lock_resource("session transaction writer worker")?
            .take();
        if let Some(worker) = worker {
            worker.join().map_err(|_| CodingSessionError::Session {
                message: "session transaction writer panicked during shutdown".into(),
            })?;
        }
        Ok(())
    }
}

impl Drop for SessionTransactionWriterInner {
    fn drop(&mut self) {
        match self.sender.get_mut() {
            Ok(sender) => {
                sender.take();
            }
            Err(poisoned) => {
                // Drop cannot return the resource error; recover solely to
                // release the sender and report the poisoned lock once.
                recover_poisoned("session transaction writer sender", poisoned).take();
            }
        }
        let worker = match self.worker.get_mut() {
            Ok(worker) => worker.take(),
            Err(poisoned) => {
                // Drop cannot return the resource error; recover solely to
                // join the worker and report the poisoned lock once.
                recover_poisoned("session transaction writer worker", poisoned).take()
            }
        };
        if let Some(worker) = worker {
            let _ = worker.join();
        }
    }
}

#[cfg(test)]
mod tests;

mod worker;

use worker::*;

impl TransactionState {
    fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Committed => "committed",
            Self::Aborted => "aborted",
            Self::Failed => "failed",
            Self::InDoubt => "in_doubt",
        }
    }
}

#[derive(Debug)]
pub(crate) struct TurnTransaction<G, C>
where
    G: IdGenerator,
    C: Clock,
{
    writer: SessionTransactionWriter,
    session_id: String,
    ids: G,
    clock: C,
    operation_id: String,
    turn_id: String,
    pending_events: Vec<SessionEventEnvelope>,
    committed_session_sequence: Option<u64>,
    open_messages: HashSet<String>,
    open_reasoning: HashSet<(String, u32)>,
    open_tool_calls: HashSet<String>,
    state: TransactionState,
}

impl<G, C> TurnTransaction<G, C>
where
    G: IdGenerator,
    C: Clock,
{
    pub(crate) fn begin_admitted_with_runtime_generation(
        writer: SessionTransactionWriter,
        session_id: String,
        mut ids: G,
        clock: C,
        operation: OperationKind,
        runtime_generation: PersistedRuntimeGenerationRef,
        operation_id: String,
    ) -> Self {
        let turn_id = ids.next_turn_id();
        let mut transaction = Self {
            writer,
            session_id,
            ids,
            clock,
            operation_id,
            turn_id,
            pending_events: Vec::new(),
            committed_session_sequence: None,
            open_messages: HashSet::new(),
            open_reasoning: HashSet::new(),
            open_tool_calls: HashSet::new(),
            state: TransactionState::Open,
        };
        transaction.push_event(SessionEventData::OperationStarted {
            operation,
            runtime_generation,
        });
        transaction.push_event(SessionEventData::TurnStarted {});
        transaction
    }

    pub(crate) fn operation_id(&self) -> &str {
        &self.operation_id
    }

    pub(crate) fn turn_id(&self) -> &str {
        &self.turn_id
    }

    pub(crate) fn committed_session_sequence(&self) -> Option<u64> {
        self.committed_session_sequence
    }

    pub(crate) fn record_user_input(
        &mut self,
        content: Vec<PersistedContentBlock>,
    ) -> Result<(), CodingSessionError> {
        self.ensure_open()?;
        self.push_event(SessionEventData::TurnInputRecorded { content });
        Ok(())
    }

    pub(crate) fn start_assistant_message(&mut self) -> Result<String, CodingSessionError> {
        self.ensure_open()?;
        let message_id = self.ids.next_message_id();
        self.open_messages.insert(message_id.clone());
        self.push_event(SessionEventData::MessageStarted {
            message_id: message_id.clone(),
            role: PersistedRole::Assistant,
        });
        Ok(message_id)
    }

    pub(crate) fn complete_assistant_message(
        &mut self,
        message_id: impl Into<String>,
        content: Vec<PersistedContentBlock>,
        finish_reason: Option<String>,
        usage: Usage,
        model_id: Option<String>,
    ) -> Result<(), CodingSessionError> {
        self.ensure_open()?;
        let message_id = message_id.into();
        self.ensure_message_open(&message_id)?;
        self.complete_open_reasoning(&message_id);
        self.open_messages.remove(&message_id);
        self.push_event(SessionEventData::MessageCompleted {
            message_id,
            content,
            finish_reason,
            usage,
            model_id,
        });
        Ok(())
    }

    pub(crate) fn start_assistant_reasoning(
        &mut self,
        message_id: impl Into<String>,
        content_index: u32,
    ) -> Result<(), CodingSessionError> {
        self.ensure_open()?;
        let message_id = message_id.into();
        self.ensure_message_open(&message_id)?;
        if !self
            .open_reasoning
            .insert((message_id.clone(), content_index))
        {
            return Err(CodingSessionError::Session {
                message: format!(
                    "assistant reasoning segment is already open: {message_id}/{content_index}"
                ),
            });
        }
        self.push_event(SessionEventData::MessageReasoningStarted {
            message_id,
            content_index,
        });
        Ok(())
    }

    pub(crate) fn complete_assistant_reasoning(
        &mut self,
        message_id: impl Into<String>,
        content_index: u32,
    ) -> Result<(), CodingSessionError> {
        self.ensure_open()?;
        let message_id = message_id.into();
        self.ensure_message_open(&message_id)?;
        if !self
            .open_reasoning
            .remove(&(message_id.clone(), content_index))
        {
            return Err(CodingSessionError::Session {
                message: format!(
                    "assistant reasoning segment is not open: {message_id}/{content_index}"
                ),
            });
        }
        self.push_event(SessionEventData::MessageReasoningCompleted {
            message_id,
            content_index,
        });
        Ok(())
    }

    pub(crate) fn record_tool_started(
        &mut self,
        name: impl Into<String>,
        arguments: Value,
    ) -> Result<String, CodingSessionError> {
        self.ensure_open()?;
        let tool_call_id = self.ids.next_tool_call_id();
        self.open_tool_calls.insert(tool_call_id.clone());
        self.push_event(SessionEventData::ToolCallStarted {
            tool_call_id: tool_call_id.clone(),
            name: name.into(),
            arguments,
        });
        Ok(tool_call_id)
    }

    pub(crate) fn record_tool_updated(
        &mut self,
        tool_call_id: impl Into<String>,
        message: impl Into<String>,
    ) -> Result<(), CodingSessionError> {
        self.ensure_open()?;
        let tool_call_id = tool_call_id.into();
        self.ensure_tool_call_open(&tool_call_id)?;
        self.push_event(SessionEventData::ToolCallUpdated {
            tool_call_id,
            message: message.into(),
        });
        Ok(())
    }

    pub(crate) fn record_tool_completed(
        &mut self,
        tool_call_id: impl Into<String>,
        result: PersistedToolResult,
    ) -> Result<(), CodingSessionError> {
        self.ensure_open()?;
        let tool_call_id = tool_call_id.into();
        self.ensure_tool_call_open(&tool_call_id)?;
        self.open_tool_calls.remove(&tool_call_id);
        self.push_event(SessionEventData::ToolCallCompleted {
            tool_call_id,
            result,
        });
        Ok(())
    }

    pub(crate) fn record_tool_failed(
        &mut self,
        tool_call_id: impl Into<String>,
        message: impl Into<String>,
    ) -> Result<(), CodingSessionError> {
        self.ensure_open()?;
        let tool_call_id = tool_call_id.into();
        self.ensure_tool_call_open(&tool_call_id)?;
        self.open_tool_calls.remove(&tool_call_id);
        self.push_event(SessionEventData::ToolCallFailed {
            tool_call_id,
            message: message.into(),
        });
        Ok(())
    }

    pub(crate) fn emit_diagnostic(
        &mut self,
        level: DiagnosticLevel,
        message: impl Into<String>,
    ) -> Result<(), CodingSessionError> {
        self.ensure_open()?;
        self.push_event(SessionEventData::DiagnosticEmitted {
            level,
            message: message.into(),
        });
        Ok(())
    }

    pub(crate) fn record_session_compaction_started(
        &mut self,
        first_kept_message_id: impl Into<String>,
        tokens_before: u32,
    ) -> Result<(), CodingSessionError> {
        self.ensure_open()?;
        self.push_event(SessionEventData::SessionCompactionStarted {
            first_kept_message_id: first_kept_message_id.into(),
            tokens_before,
        });
        Ok(())
    }

    pub(crate) fn record_session_compaction_completed(
        &mut self,
        summary: impl Into<String>,
        first_kept_message_id: impl Into<String>,
        tokens_before: u32,
    ) -> Result<(), CodingSessionError> {
        self.ensure_open()?;
        self.push_event(SessionEventData::SessionCompactionCompleted {
            summary: summary.into(),
            first_kept_message_id: first_kept_message_id.into(),
            tokens_before,
        });
        Ok(())
    }

    pub(crate) fn record_branch_summary_created(
        &mut self,
        summary: impl Into<String>,
        source_leaf_id: impl Into<String>,
        target_leaf_id: impl Into<String>,
    ) -> Result<(), CodingSessionError> {
        self.ensure_open()?;
        self.push_event(SessionEventData::BranchSummaryCreated {
            summary: summary.into(),
            source_leaf_id: source_leaf_id.into(),
            target_leaf_id: target_leaf_id.into(),
        });
        Ok(())
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "durable folded delegation updates retain every typed association fact"
    )]
    pub(crate) fn record_delegation_folded_update(
        &mut self,
        tool_call_id: impl Into<String>,
        requesting_profile_id: ProfileId,
        target_kind: ProfileKind,
        target_id: ProfileId,
        task: impl Into<String>,
        status: PersistedDelegationStatus,
        child_operation_id: Option<String>,
        summary: Option<String>,
    ) -> Result<(), CodingSessionError> {
        self.ensure_open()?;
        self.push_event(SessionEventData::DelegationFoldedUpdated {
            tool_call_id: tool_call_id.into(),
            requesting_profile_id,
            target_kind,
            target_id,
            task: task.into(),
            status,
            child_operation_id,
            summary,
        });
        Ok(())
    }

    pub(crate) fn record_self_healing_edit_started(
        &mut self,
        path: impl Into<String>,
        replacements: usize,
    ) -> Result<(), CodingSessionError> {
        self.ensure_open()?;
        self.push_event(SessionEventData::SelfHealingEditStarted {
            path: path.into(),
            replacements,
        });
        Ok(())
    }

    pub(crate) fn record_self_healing_edit_repair_attempted(
        &mut self,
        path: impl Into<String>,
        repair: &SelfHealingEditRepairAttempt,
    ) -> Result<(), CodingSessionError> {
        self.ensure_open()?;
        self.push_event(SessionEventData::SelfHealingEditRepairAttempted {
            path: path.into(),
            attempt: repair.attempt,
            replacements: repair
                .replacements
                .iter()
                .map(|replacement| PersistedSelfHealingEditReplacement {
                    old_text: replacement.old_text.clone(),
                    new_text: replacement.new_text.clone(),
                })
                .collect(),
            diagnostics: repair
                .diagnostics
                .iter()
                .map(|diagnostic| diagnostic.message.clone())
                .collect(),
            check_output: repair.check_output.as_ref().map(|output| {
                PersistedSelfHealingEditCheckOutput {
                    command: output.command.clone(),
                    stdout: output.stdout.clone(),
                    stderr: output.stderr.clone(),
                    exit_code: output.exit_code,
                }
            }),
        });
        Ok(())
    }

    pub(crate) fn record_self_healing_edit_completed(
        &mut self,
        outcome: &SelfHealingEditOutcome,
    ) -> Result<(), CodingSessionError> {
        self.ensure_open()?;
        self.push_event(SessionEventData::SelfHealingEditCompleted {
            path: outcome.path.clone(),
            message: outcome.message.clone(),
            diff: outcome.diff.clone(),
            patch: outcome.patch.clone(),
            first_changed_line: outcome.first_changed_line,
            attempts: outcome.attempts,
            diagnostics: outcome
                .diagnostics
                .iter()
                .map(|diagnostic| diagnostic.message.clone())
                .collect(),
            check_output: outcome.check_output.as_ref().map(|output| {
                PersistedSelfHealingEditCheckOutput {
                    command: output.command.clone(),
                    stdout: output.stdout.clone(),
                    stderr: output.stderr.clone(),
                    exit_code: output.exit_code,
                }
            }),
        });
        Ok(())
    }

    pub(crate) async fn checkpoint(&mut self) -> Result<(), CodingSessionError> {
        self.ensure_open()?;
        self.flush_pending().await
    }

    pub(crate) async fn commit_with_outbox(
        &mut self,
        new_leaf_id: Option<String>,
        intent: DurableOutboxIntent,
    ) -> Result<(), CodingSessionError> {
        self.ensure_open()?;
        self.push_event(SessionEventData::OperationCommitted {
            new_leaf_id: new_leaf_id.clone(),
        });
        let record = self.outbox_record(intent)?;
        self.finalize_pending(new_leaf_id, vec![record]).await?;
        self.state = TransactionState::Committed;
        Ok(())
    }

    pub(crate) async fn abort_with_outbox(
        &mut self,
        reason: impl Into<String>,
        intent: DurableOutboxIntent,
    ) -> Result<(), CodingSessionError> {
        self.abort_internal(reason.into(), Some(intent)).await
    }

    async fn abort_internal(
        &mut self,
        reason: String,
        intent: Option<DurableOutboxIntent>,
    ) -> Result<(), CodingSessionError> {
        self.ensure_open()?;
        self.cancel_open_lifecycle_events(&reason);
        self.push_event(SessionEventData::OperationAborted { reason });
        let outbox_records = intent
            .map(|intent| self.outbox_record(intent).map(|record| vec![record]))
            .transpose()?
            .unwrap_or_default();
        self.finalize_pending(None, outbox_records).await?;
        self.state = TransactionState::Aborted;
        Ok(())
    }

    pub(crate) async fn fail_with_outbox(
        &mut self,
        error_code: impl Into<String>,
        message: impl Into<String>,
        intent: DurableOutboxIntent,
    ) -> Result<(), CodingSessionError> {
        self.fail_internal(error_code.into(), message.into(), Some(intent))
            .await
    }

    async fn fail_internal(
        &mut self,
        error_code: String,
        message: String,
        intent: Option<DurableOutboxIntent>,
    ) -> Result<(), CodingSessionError> {
        self.ensure_open()?;
        self.cancel_open_lifecycle_events("failed");
        self.push_event(SessionEventData::DiagnosticEmitted {
            level: DiagnosticLevel::Error,
            message: message.clone(),
        });
        self.push_event(SessionEventData::OperationFailed {
            error_code,
            message,
        });
        let outbox_records = intent
            .map(|intent| self.outbox_record(intent).map(|record| vec![record]))
            .transpose()?
            .unwrap_or_default();
        self.finalize_pending(None, outbox_records).await?;
        self.state = TransactionState::Failed;
        Ok(())
    }

    fn push_event(&mut self, data: SessionEventData) {
        let event = SessionEventEnvelope::new(
            self.session_id.clone(),
            self.ids.next_event_id(),
            self.clock.now_rfc3339(),
            data,
        )
        .with_operation_id(self.operation_id.clone())
        .with_turn_id(self.turn_id.clone());
        self.pending_events.push(event);
    }

    fn outbox_record(
        &self,
        intent: DurableOutboxIntent,
    ) -> Result<DurableOutboxRecordCandidate, CodingSessionError> {
        let source_event_ids = self
            .pending_events
            .iter()
            .map(|event| event.event_id.clone())
            .collect();
        DurableOutboxRecordCandidate::new(
            intent.record_id,
            self.session_id.clone(),
            Some(self.operation_id.clone()),
            source_event_ids,
            intent.kind,
            intent.draft,
        )
        .map_err(|message| CodingSessionError::Session {
            message: message.into(),
        })
    }

    async fn flush_pending(&mut self) -> Result<(), CodingSessionError> {
        if let Err(error) = self
            .writer
            .append_checkpoint_events(self.pending_events.clone())
            .await
        {
            if matches!(error, CodingSessionError::SessionWriteRejected { .. }) {
                return Err(error);
            }
            self.state = TransactionState::InDoubt;
            return Err(CodingSessionError::PartialCommit {
                operation_id: self.operation_id.clone(),
                message: error.to_string(),
            });
        }
        self.pending_events.clear();
        Ok(())
    }

    async fn finalize_pending(
        &mut self,
        active_leaf_id: Option<String>,
        outbox_records: Vec<DurableOutboxRecordCandidate>,
    ) -> Result<(), CodingSessionError> {
        let receipt = match self
            .writer
            .execute_async(SessionTransactionWriterCommand::Finalize {
                events: self.pending_events.clone(),
                outbox_records,
                updated_at: self.clock.now_rfc3339(),
                active_leaf_id,
            })
            .await
        {
            Ok(receipt) => receipt,
            Err(error) => {
                self.state = TransactionState::InDoubt;
                return Err(CodingSessionError::PartialCommit {
                    operation_id: self.operation_id.clone(),
                    message: error.to_string(),
                });
            }
        };
        self.committed_session_sequence = receipt.committed_session_sequence;
        self.pending_events.clear();
        Ok(())
    }

    fn cancel_open_lifecycle_events(&mut self, reason: &str) {
        let open_messages = self.open_messages.drain().collect::<Vec<_>>();
        for message_id in open_messages {
            self.open_reasoning
                .retain(|(open_message_id, _)| open_message_id != &message_id);
            self.push_event(SessionEventData::MessageCancelled {
                message_id,
                reason: reason.to_owned(),
            });
        }

        let open_tool_calls = self.open_tool_calls.drain().collect::<Vec<_>>();
        for tool_call_id in open_tool_calls {
            self.push_event(SessionEventData::ToolCallCancelled {
                tool_call_id,
                reason: reason.to_owned(),
            });
        }
    }

    fn complete_open_reasoning(&mut self, message_id: &str) {
        let mut content_indices = self
            .open_reasoning
            .iter()
            .filter_map(|(open_message_id, content_index)| {
                (open_message_id == message_id).then_some(*content_index)
            })
            .collect::<Vec<_>>();
        content_indices.sort_unstable();
        for content_index in content_indices {
            self.open_reasoning
                .remove(&(message_id.to_owned(), content_index));
            self.push_event(SessionEventData::MessageReasoningCompleted {
                message_id: message_id.to_owned(),
                content_index,
            });
        }
    }

    fn ensure_open(&self) -> Result<(), CodingSessionError> {
        if self.state == TransactionState::Open {
            Ok(())
        } else {
            Err(CodingSessionError::Session {
                message: format!(
                    "turn transaction is already finalized: {}",
                    self.state.as_str()
                ),
            })
        }
    }

    fn ensure_message_open(&self, message_id: &str) -> Result<(), CodingSessionError> {
        if self.open_messages.contains(message_id) {
            Ok(())
        } else {
            Err(CodingSessionError::Session {
                message: format!("assistant message is not open: {message_id}"),
            })
        }
    }

    fn ensure_tool_call_open(&self, tool_call_id: &str) -> Result<(), CodingSessionError> {
        if self.open_tool_calls.contains(tool_call_id) {
            Ok(())
        } else {
            Err(CodingSessionError::Session {
                message: format!("tool call is not open: {tool_call_id}"),
            })
        }
    }
}
