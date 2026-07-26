use std::collections::{BTreeMap, HashMap, HashSet};

use crate::authorization::ToolAuthorizationRequest;
use crate::profiles::{ProfileId, ProfileKind};
use crate::session::event::{
    DiagnosticLevel, PersistedContentBlock, PersistedDelegationRuntimeSeed,
    PersistedDelegationStatus, PersistedToolResult, SessionEventData, SessionEventEnvelope,
};
use agent_core::api::compaction::calculate_context_tokens;
use ai::api::conversation::Usage;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OperationReplayStatus {
    Committed,
    Failed,
    Aborted,
    Recovered,
    InDoubt,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SessionRecoverySummary {
    pub(crate) in_doubt_operations: Vec<String>,
}

impl SessionReplay {
    #[cfg(test)]
    pub(crate) fn operation_status(&self, operation_id: &str) -> Option<OperationReplayStatus> {
        self.operation_statuses.get(operation_id).copied()
    }

    pub(crate) fn recovery_summary(&self) -> SessionRecoverySummary {
        let mut in_doubt_operations: Vec<String> = self
            .operation_statuses
            .iter()
            .filter(|(_, status)| **status == OperationReplayStatus::InDoubt)
            .map(|(id, _)| id.clone())
            .collect();
        in_doubt_operations.sort();
        SessionRecoverySummary {
            in_doubt_operations,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct SessionReplay {
    pub(crate) session_id: String,
    pub(crate) committed_through_session_sequence: u64,
    pub(crate) cwd: Option<String>,
    pub(crate) active_leaf_id: Option<String>,
    pub(crate) leaves: Vec<ReplayLeaf>,
    pub(crate) tree_labels: HashMap<String, ReplayTreeLabel>,
    pub(crate) transcript: Vec<TranscriptItem>,
    pub(crate) diagnostics: Vec<ReplayDiagnostic>,
    pub(crate) pending_delegation_confirmations: Vec<ReplayPendingDelegationConfirmation>,
    pub(crate) pending_tool_authorizations: Vec<ToolAuthorizationRequest>,
    pub(crate) usage: ReplayUsageSummary,
    pub(crate) operation_statuses: HashMap<String, OperationReplayStatus>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ReplayTreeLabel {
    pub(crate) label: Option<String>,
    pub(crate) updated_at: String,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ReplayUsageSummary {
    pub(crate) input: u32,
    pub(crate) output: u32,
    pub(crate) cache_read: u32,
    pub(crate) cache_write: u32,
    pub(crate) cost: f64,
    pub(crate) cost_known: bool,
    pub(crate) last_context_tokens: Option<u32>,
    pub(crate) last_context_message_id: Option<String>,
}

impl Default for ReplayUsageSummary {
    fn default() -> Self {
        Self {
            input: 0,
            output: 0,
            cache_read: 0,
            cache_write: 0,
            cost: 0.0,
            cost_known: true,
            last_context_tokens: None,
            last_context_message_id: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ReplayLeaf {
    pub(crate) leaf_id: String,
    pub(crate) parent_leaf_id: Option<String>,
    pub(crate) transcript_start: usize,
    pub(crate) transcript_end: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TranscriptItem {
    UserInput {
        turn_id: String,
        text: String,
    },
    AssistantMessage {
        message_id: String,
        content: Vec<PersistedContentBlock>,
        status: MessageStatus,
    },
    ToolCall {
        tool_call_id: String,
        name: String,
        arguments: serde_json::Value,
        status: ToolCallStatus,
        summary: String,
        started_at: String,
        duration_millis: Option<u64>,
    },
    DelegationBlock {
        tool_call_id: String,
        requesting_profile_id: ProfileId,
        target_kind: ProfileKind,
        target_id: ProfileId,
        task: String,
        status: PersistedDelegationStatus,
        child_operation_id: Option<String>,
        summary: Option<String>,
    },
    CompactionSummary {
        summary: String,
        first_kept_message_id: String,
        tokens_before: u32,
    },
    BranchSummary {
        summary: String,
        source_leaf_id: String,
        target_leaf_id: String,
    },
    Diagnostic {
        level: DiagnosticLevel,
        message: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MessageStatus {
    Started,
    Completed,
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ToolCallStatus {
    Started,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ReplayDiagnostic {
    pub(crate) level: DiagnosticLevel,
    pub(crate) message: String,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ReplayPendingDelegationConfirmation {
    pub(crate) source_operation_id: String,
    pub(crate) turn_id: String,
    pub(crate) tool_call_id: String,
    pub(crate) requesting_profile_id: ProfileId,
    pub(crate) target_kind: ProfileKind,
    pub(crate) target_id: ProfileId,
    pub(crate) task: String,
    pub(crate) reason: String,
    pub(crate) requested_at: String,
    pub(crate) runtime_seed: PersistedDelegationRuntimeSeed,
}

#[derive(Debug, Default)]
struct ReplayBuilder {
    session_id: Option<String>,
    cwd: Option<String>,
    active_leaf_id: Option<String>,
    transcript: Vec<TranscriptItem>,
    leaves: Vec<ReplayLeaf>,
    tree_labels: HashMap<String, ReplayTreeLabel>,
    diagnostics: Vec<ReplayDiagnostic>,
    message_indices: HashMap<String, usize>,
    tool_indices: HashMap<String, usize>,
    delegation_indices: HashMap<String, usize>,
    operation_kinds: HashMap<String, crate::session::event::OperationKind>,
    operation_transcript_starts: HashMap<String, usize>,
    pending_delegation_confirmations: Vec<ReplayPendingDelegationConfirmation>,
    pending_tool_authorizations: BTreeMap<String, ToolAuthorizationRequest>,
    usage: ReplayUsageSummary,
    operation_statuses: HashMap<String, OperationReplayStatus>,
}

#[derive(Debug, Default)]
pub(crate) struct ReplayIndex {
    committed_through_session_sequence: u64,
    finalized_operations: HashSet<String>,
    operation_ids_in_order: Vec<String>,
    seen_operation_ids: HashSet<String>,
}

impl ReplayIndex {
    pub(crate) fn observe(&mut self, event: &SessionEventEnvelope) {
        self.committed_through_session_sequence = self
            .committed_through_session_sequence
            .max(event.session_sequence.unwrap_or_default());
        if let Some(operation_id) = event.operation_id.as_deref() {
            if !is_out_of_band_operation_event(&event.data)
                && self.seen_operation_ids.insert(operation_id.to_owned())
            {
                self.operation_ids_in_order.push(operation_id.to_owned());
            }
            if matches!(
                event.data,
                SessionEventData::OperationCommitted { .. }
                    | SessionEventData::OperationAborted { .. }
                    | SessionEventData::OperationFailed { .. }
                    | SessionEventData::OperationTerminalRecorded { .. }
                    | SessionEventData::OperationRecovered { .. }
            ) {
                self.finalized_operations.insert(operation_id.to_owned());
            }
        }
    }
}

#[derive(Debug)]
pub(crate) struct ReplayFold {
    index: ReplayIndex,
    builder: ReplayBuilder,
}

impl ReplayFold {
    pub(crate) fn new(index: ReplayIndex) -> Self {
        Self {
            index,
            builder: ReplayBuilder::default(),
        }
    }

    pub(crate) fn observe(&mut self, event: &SessionEventEnvelope) {
        self.builder.observe_session_id(event);
        self.builder.observe_operation_status(event);
        if let Some(operation_id) = event.operation_id.as_deref()
            && !self.index.finalized_operations.contains(operation_id)
            && !is_out_of_band_operation_event(&event.data)
        {
            return;
        }
        self.builder.apply_event(event);
    }

    pub(crate) fn finish(mut self) -> SessionReplay {
        for operation_id in self.index.operation_ids_in_order {
            if !self.index.finalized_operations.contains(&operation_id) {
                self.builder.warn(format!(
                    "operation {operation_id} has no final marker and was omitted from replay"
                ));
            }
        }
        let mut pending_tool_authorizations = self
            .builder
            .pending_tool_authorizations
            .into_values()
            .collect::<Vec<_>>();
        pending_tool_authorizations.sort_by(|left, right| {
            left.requested_at
                .cmp(&right.requested_at)
                .then_with(|| left.authorization_id.cmp(&right.authorization_id))
        });
        SessionReplay {
            session_id: self.builder.session_id.unwrap_or_default(),
            committed_through_session_sequence: self.index.committed_through_session_sequence,
            cwd: self.builder.cwd,
            active_leaf_id: self.builder.active_leaf_id,
            leaves: self.builder.leaves,
            tree_labels: self.builder.tree_labels,
            transcript: self.builder.transcript,
            diagnostics: self.builder.diagnostics,
            pending_delegation_confirmations: self.builder.pending_delegation_confirmations,
            pending_tool_authorizations,
            usage: self.builder.usage,
            operation_statuses: self.builder.operation_statuses,
        }
    }
}

pub(crate) fn fold_events(events: &[SessionEventEnvelope]) -> SessionReplay {
    let mut index = ReplayIndex::default();
    for event in events {
        index.observe(event);
    }
    let mut fold = ReplayFold::new(index);
    for event in events {
        fold.observe(event);
    }
    fold.finish()
}

fn is_out_of_band_operation_event(data: &SessionEventData) -> bool {
    matches!(
        data,
        SessionEventData::DelegationConfirmationRequested { .. }
            | SessionEventData::DelegationConfirmationApproved { .. }
            | SessionEventData::DelegationConfirmationRejected { .. }
            | SessionEventData::ToolAuthorizationRequested { .. }
            | SessionEventData::ToolAuthorizationResolved { .. }
    )
}

impl ReplayBuilder {
    fn observe_session_id(&mut self, event: &SessionEventEnvelope) {
        match self.session_id.as_deref() {
            None => self.session_id = Some(event.session_id.clone()),
            Some(session_id) if session_id != event.session_id => self.warn(format!(
                "event {} belongs to {}, expected {}",
                event.event_id, event.session_id, session_id
            )),
            Some(_) => {}
        }
    }

    fn observe_operation_status(&mut self, event: &SessionEventEnvelope) {
        let Some(operation_id) = event.operation_id.as_deref() else {
            return;
        };
        match &event.data {
            SessionEventData::OperationStarted { .. } => {
                self.operation_statuses
                    .entry(operation_id.to_owned())
                    .or_insert(OperationReplayStatus::InDoubt);
            }
            SessionEventData::OperationCommitted { .. } => {
                self.operation_statuses
                    .insert(operation_id.to_owned(), OperationReplayStatus::Committed);
            }
            SessionEventData::OperationFailed { .. } => {
                self.operation_statuses
                    .insert(operation_id.to_owned(), OperationReplayStatus::Failed);
            }
            SessionEventData::OperationAborted { .. } => {
                self.operation_statuses
                    .insert(operation_id.to_owned(), OperationReplayStatus::Aborted);
            }
            SessionEventData::OperationRecovered { .. } => {
                self.operation_statuses
                    .insert(operation_id.to_owned(), OperationReplayStatus::Recovered);
            }
            SessionEventData::OperationRecoveryPending { .. } => {}
            SessionEventData::OperationTerminalRecorded { status, .. } => {
                let status = match status.as_str() {
                    "completed" => OperationReplayStatus::Committed,
                    "failed" => OperationReplayStatus::Failed,
                    "aborted" => OperationReplayStatus::Aborted,
                    _ => return,
                };
                self.operation_statuses
                    .insert(operation_id.to_owned(), status);
            }
            _ => {}
        }
    }

    fn apply_event(&mut self, event: &SessionEventEnvelope) {
        match &event.data {
            SessionEventData::SessionCreated { cwd } => {
                self.cwd = cwd.clone();
            }
            SessionEventData::OperationStarted { operation, .. } => {
                if let Some(operation_id) = event.operation_id.as_deref() {
                    self.operation_kinds
                        .insert(operation_id.to_owned(), operation.clone());
                    self.operation_transcript_starts
                        .insert(operation_id.to_owned(), self.transcript.len());
                }
            }
            SessionEventData::SessionCloned { .. }
            | SessionEventData::SessionForked { .. }
            | SessionEventData::SessionCompactionStarted { .. }
            | SessionEventData::TurnStarted {}
            | SessionEventData::SelfHealingEditStarted { .. }
            | SessionEventData::SelfHealingEditRepairAttempted { .. }
            | SessionEventData::SelfHealingEditCompleted { .. }
            | SessionEventData::OperationRecoveryResolved { .. }
            | SessionEventData::MetadataUpdated { .. } => {}
            SessionEventData::SessionTreeLabelUpdated { entry_id, label } => {
                self.tree_labels.insert(
                    entry_id.clone(),
                    ReplayTreeLabel {
                        label: label.clone(),
                        updated_at: event.created_at.clone(),
                    },
                );
            }
            SessionEventData::SessionCompactionCompleted {
                summary,
                first_kept_message_id,
                tokens_before,
            } => {
                self.usage.last_context_tokens = None;
                self.usage.last_context_message_id = None;
                self.apply_compaction_completed(summary, first_kept_message_id, *tokens_before);
            }
            SessionEventData::BranchSummaryCreated {
                summary,
                source_leaf_id,
                target_leaf_id,
            } => {
                self.transcript.push(TranscriptItem::BranchSummary {
                    summary: summary.clone(),
                    source_leaf_id: source_leaf_id.clone(),
                    target_leaf_id: target_leaf_id.clone(),
                });
            }
            SessionEventData::DelegationConfirmationRequested {
                source_operation_id,
                turn_id,
                tool_call_id,
                requesting_profile_id,
                target_kind,
                target_id,
                task,
                reason,
                runtime_seed,
            } => {
                self.add_pending_delegation_confirmation(ReplayPendingDelegationConfirmation {
                    source_operation_id: source_operation_id.clone(),
                    turn_id: turn_id.clone(),
                    tool_call_id: tool_call_id.clone(),
                    requesting_profile_id: requesting_profile_id.clone(),
                    target_kind: *target_kind,
                    target_id: target_id.clone(),
                    task: task.clone(),
                    reason: reason.clone(),
                    requested_at: event.created_at.clone(),
                    runtime_seed: runtime_seed.clone(),
                });
            }
            SessionEventData::DelegationConfirmationApproved {
                source_operation_id,
                tool_call_id,
                ..
            }
            | SessionEventData::DelegationConfirmationRejected {
                source_operation_id,
                tool_call_id,
                ..
            } => {
                self.resolve_pending_delegation_confirmation(source_operation_id, tool_call_id);
            }
            SessionEventData::DelegationFoldedUpdated {
                tool_call_id,
                requesting_profile_id,
                target_kind,
                target_id,
                task,
                status,
                child_operation_id,
                summary,
            } => {
                self.apply_delegation_folded_update(DelegationBlockUpdate {
                    tool_call_id: tool_call_id.clone(),
                    requesting_profile_id: requesting_profile_id.clone(),
                    target_kind: *target_kind,
                    target_id: target_id.clone(),
                    task: task.clone(),
                    status: *status,
                    child_operation_id: child_operation_id.clone(),
                    summary: summary.clone(),
                });
            }
            SessionEventData::ToolAuthorizationRequested { request } => {
                if self
                    .pending_tool_authorizations
                    .insert(request.authorization_id.clone(), request.clone())
                    .is_some()
                {
                    self.warn(format!(
                        "duplicate pending tool authorization: {}",
                        request.authorization_id
                    ));
                }
            }
            SessionEventData::ToolAuthorizationResolved {
                authorization_id, ..
            } => {
                if self
                    .pending_tool_authorizations
                    .remove(authorization_id)
                    .is_none()
                {
                    self.warn(format!(
                        "tool authorization resolution references unknown pending request: {authorization_id}"
                    ));
                }
            }
            SessionEventData::OperationCommitted { new_leaf_id } => {
                if let Some(new_leaf_id) = new_leaf_id {
                    self.record_prompt_leaf(event, new_leaf_id);
                    self.active_leaf_id = Some(new_leaf_id.clone());
                }
            }
            SessionEventData::OperationAborted { reason } => {
                self.warn(format!(
                    "operation {} aborted: {reason}",
                    event.operation_id.as_deref().unwrap_or("<unknown>")
                ));
            }
            SessionEventData::OperationFailed {
                error_code,
                message,
            } => {
                self.diagnostics.push(ReplayDiagnostic {
                    level: DiagnosticLevel::Error,
                    message: format!(
                        "operation {} failed ({error_code}): {message}",
                        event.operation_id.as_deref().unwrap_or("<unknown>")
                    ),
                });
            }
            SessionEventData::OperationRecoveryPending { .. } => {}
            SessionEventData::OperationRecovered { reason, .. } => {
                self.warn(format!(
                    "operation {} recovered: {reason}",
                    event.operation_id.as_deref().unwrap_or("<unknown>")
                ));
            }
            SessionEventData::OperationTerminalRecorded { .. } => {}
            SessionEventData::TurnInputRecorded { content } => {
                self.transcript.push(TranscriptItem::UserInput {
                    turn_id: event.turn_id.clone().unwrap_or_default(),
                    text: content_blocks_text(content),
                });
            }
            SessionEventData::MessageStarted { message_id, .. } => {
                self.message_indices
                    .insert(message_id.clone(), self.transcript.len());
                self.transcript.push(TranscriptItem::AssistantMessage {
                    message_id: message_id.clone(),
                    content: Vec::new(),
                    status: MessageStatus::Started,
                });
            }
            SessionEventData::MessageCompleted {
                message_id,
                content,
                finish_reason: _,
                usage,
            } => {
                self.record_assistant_usage(message_id, usage);
                if self.complete_message(message_id, content.clone()).is_err() {
                    self.warn(format!(
                        "message completion references unknown message: {message_id}"
                    ));
                }
            }
            SessionEventData::MessageCancelled { message_id, .. } => {
                if self
                    .set_message_status(message_id, MessageStatus::Cancelled)
                    .is_err()
                {
                    self.warn(format!(
                        "message cancellation references unknown message: {message_id}"
                    ));
                }
            }
            SessionEventData::ToolCallStarted {
                tool_call_id,
                name,
                arguments,
            } => {
                self.tool_indices
                    .insert(tool_call_id.clone(), self.transcript.len());
                self.transcript.push(TranscriptItem::ToolCall {
                    tool_call_id: tool_call_id.clone(),
                    name: name.clone(),
                    arguments: arguments.clone(),
                    status: ToolCallStatus::Started,
                    summary: String::new(),
                    started_at: event.created_at.clone(),
                    duration_millis: None,
                });
            }
            SessionEventData::ToolCallUpdated {
                tool_call_id,
                message,
            } => {
                if let Some(tool) = self.tool_mut(tool_call_id) {
                    if !tool.is_empty() {
                        tool.push('\n');
                    }
                    tool.push_str(message);
                } else {
                    self.warn(format!(
                        "tool update references unknown tool call: {tool_call_id}"
                    ));
                }
            }
            SessionEventData::ToolCallCompleted {
                tool_call_id,
                result,
            } => {
                if self
                    .set_tool_status(tool_call_id, ToolCallStatus::Completed, &event.created_at)
                    .is_err()
                {
                    self.warn(format!(
                        "tool completion references unknown tool call: {tool_call_id}"
                    ));
                } else if let Some(summary) = self.tool_mut(tool_call_id) {
                    *summary = tool_result_summary(result);
                }
            }
            SessionEventData::ToolCallFailed {
                tool_call_id,
                message,
            } => {
                if self
                    .set_tool_status(tool_call_id, ToolCallStatus::Failed, &event.created_at)
                    .is_err()
                {
                    self.warn(format!(
                        "tool failure references unknown tool call: {tool_call_id}"
                    ));
                } else if let Some(summary) = self.tool_mut(tool_call_id) {
                    *summary = message.clone();
                }
            }
            SessionEventData::ToolCallCancelled {
                tool_call_id,
                reason,
            } => {
                if self
                    .set_tool_status(tool_call_id, ToolCallStatus::Cancelled, &event.created_at)
                    .is_err()
                {
                    self.warn(format!(
                        "tool cancellation references unknown tool call: {tool_call_id}"
                    ));
                } else if let Some(summary) = self.tool_mut(tool_call_id) {
                    *summary = reason.clone();
                }
            }
            SessionEventData::DiagnosticEmitted { level, message } => {
                self.diagnostics.push(ReplayDiagnostic {
                    level: level.clone(),
                    message: message.clone(),
                });
                self.transcript.push(TranscriptItem::Diagnostic {
                    level: level.clone(),
                    message: message.clone(),
                });
            }
            SessionEventData::ActiveLeafChanged { leaf_id } => {
                self.active_leaf_id = Some(leaf_id.clone());
            }
        }
    }

    fn record_prompt_leaf(&mut self, event: &SessionEventEnvelope, leaf_id: &str) {
        let Some(operation_id) = event.operation_id.as_deref() else {
            return;
        };
        if self.operation_kinds.get(operation_id)
            != Some(&crate::session::event::OperationKind::Prompt)
        {
            return;
        }
        let transcript_start = self
            .operation_transcript_starts
            .get(operation_id)
            .copied()
            .unwrap_or(self.transcript.len());
        self.leaves.push(ReplayLeaf {
            leaf_id: leaf_id.to_owned(),
            parent_leaf_id: self.active_leaf_id.clone(),
            transcript_start,
            transcript_end: self.transcript.len(),
        });
    }

    fn record_assistant_usage(&mut self, message_id: &str, usage: &Usage) {
        self.usage.input = self.usage.input.saturating_add(usage.input);
        self.usage.output = self.usage.output.saturating_add(usage.output);
        self.usage.cache_read = self.usage.cache_read.saturating_add(usage.cache_read);
        self.usage.cache_write = self.usage.cache_write.saturating_add(usage.cache_write);
        if usage.cost.known {
            self.usage.cost += usage.cost.input
                + usage.cost.output
                + usage.cost.cache_read
                + usage.cost.cache_write;
        } else {
            self.usage.cost_known = false;
        }

        let context_tokens = calculate_context_tokens(usage);
        if context_tokens > 0 {
            self.usage.last_context_tokens = Some(context_tokens);
            self.usage.last_context_message_id = Some(message_id.to_owned());
        }
    }

    fn add_pending_delegation_confirmation(
        &mut self,
        pending: ReplayPendingDelegationConfirmation,
    ) {
        if self
            .pending_delegation_confirmations
            .iter()
            .any(|existing| {
                existing.source_operation_id == pending.source_operation_id
                    && existing.tool_call_id == pending.tool_call_id
            })
        {
            self.warn(format!(
                "duplicate pending delegation confirmation: operation_id={}, tool_call_id={}",
                pending.source_operation_id, pending.tool_call_id
            ));
            return;
        }
        self.pending_delegation_confirmations.push(pending);
    }

    fn resolve_pending_delegation_confirmation(
        &mut self,
        source_operation_id: &str,
        tool_call_id: &str,
    ) {
        let Some(index) = self
            .pending_delegation_confirmations
            .iter()
            .position(|pending| {
                pending.source_operation_id == source_operation_id
                    && pending.tool_call_id == tool_call_id
            })
        else {
            self.warn(format!(
                "delegation confirmation resolution references unknown pending request: operation_id={source_operation_id}, tool_call_id={tool_call_id}"
            ));
            return;
        };
        self.pending_delegation_confirmations.remove(index);
    }

    fn apply_delegation_folded_update(&mut self, update: DelegationBlockUpdate) {
        let item = TranscriptItem::DelegationBlock {
            tool_call_id: update.tool_call_id.clone(),
            requesting_profile_id: update.requesting_profile_id,
            target_kind: update.target_kind,
            target_id: update.target_id,
            task: update.task,
            status: update.status,
            child_operation_id: update.child_operation_id,
            summary: update.summary,
        };
        if let Some(index) = self.delegation_indices.get(&update.tool_call_id).copied() {
            self.transcript[index] = item;
            return;
        }
        if let Some(index) = self.tool_indices.remove(&update.tool_call_id) {
            self.transcript[index] = item;
            self.delegation_indices.insert(update.tool_call_id, index);
            return;
        }
        let index = self.transcript.len();
        self.transcript.push(item);
        self.delegation_indices.insert(update.tool_call_id, index);
    }

    fn tool_mut(&mut self, tool_call_id: &str) -> Option<&mut String> {
        let index = *self.tool_indices.get(tool_call_id)?;
        match self.transcript.get_mut(index)? {
            TranscriptItem::ToolCall { summary, .. } => Some(summary),
            _ => None,
        }
    }

    fn complete_message(
        &mut self,
        message_id: &str,
        content: Vec<PersistedContentBlock>,
    ) -> Result<(), ()> {
        let index = *self.message_indices.get(message_id).ok_or(())?;
        match self.transcript.get_mut(index).ok_or(())? {
            TranscriptItem::AssistantMessage {
                content: current,
                status,
                ..
            } => {
                *current = content;
                *status = MessageStatus::Completed;
                Ok(())
            }
            _ => Err(()),
        }
    }

    fn set_message_status(&mut self, message_id: &str, status: MessageStatus) -> Result<(), ()> {
        let index = *self.message_indices.get(message_id).ok_or(())?;
        match self.transcript.get_mut(index).ok_or(())? {
            TranscriptItem::AssistantMessage {
                status: current, ..
            } => {
                *current = status;
                Ok(())
            }
            _ => Err(()),
        }
    }

    fn set_tool_status(
        &mut self,
        tool_call_id: &str,
        status: ToolCallStatus,
        terminal_at: &str,
    ) -> Result<(), ()> {
        let index = *self.tool_indices.get(tool_call_id).ok_or(())?;
        match self.transcript.get_mut(index).ok_or(())? {
            TranscriptItem::ToolCall {
                status: current,
                started_at,
                duration_millis,
                ..
            } => {
                *current = status;
                *duration_millis = elapsed_millis(started_at, terminal_at);
                Ok(())
            }
            _ => Err(()),
        }
    }

    fn warn(&mut self, message: impl Into<String>) {
        self.diagnostics.push(ReplayDiagnostic {
            level: DiagnosticLevel::Warn,
            message: message.into(),
        });
    }

    fn apply_compaction_completed(
        &mut self,
        summary: &str,
        first_kept_message_id: &str,
        tokens_before: u32,
    ) {
        let Some(first_kept_index) = self
            .transcript
            .iter()
            .position(|item| transcript_item_id(item).as_deref() == Some(first_kept_message_id))
        else {
            self.warn(format!(
                "session compaction references unknown first kept message: {first_kept_message_id}"
            ));
            return;
        };

        let kept = self.transcript.split_off(first_kept_index);
        self.transcript.clear();
        self.transcript.push(TranscriptItem::CompactionSummary {
            summary: summary.to_owned(),
            first_kept_message_id: first_kept_message_id.to_owned(),
            tokens_before,
        });
        self.transcript.extend(kept);
        self.rebuild_indices();
    }

    fn rebuild_indices(&mut self) {
        self.message_indices.clear();
        self.tool_indices.clear();
        self.delegation_indices.clear();
        for (index, item) in self.transcript.iter().enumerate() {
            match item {
                TranscriptItem::AssistantMessage { message_id, .. } => {
                    self.message_indices.insert(message_id.clone(), index);
                }
                TranscriptItem::ToolCall { tool_call_id, .. } => {
                    self.tool_indices.insert(tool_call_id.clone(), index);
                }
                TranscriptItem::DelegationBlock { tool_call_id, .. } => {
                    self.delegation_indices.insert(tool_call_id.clone(), index);
                }
                TranscriptItem::UserInput { .. }
                | TranscriptItem::CompactionSummary { .. }
                | TranscriptItem::BranchSummary { .. }
                | TranscriptItem::Diagnostic { .. } => {}
            }
        }
    }
}

fn elapsed_millis(started_at: &str, terminal_at: &str) -> Option<u64> {
    let started_at = OffsetDateTime::parse(started_at, &Rfc3339).ok()?;
    let terminal_at = OffsetDateTime::parse(terminal_at, &Rfc3339).ok()?;
    u64::try_from((terminal_at - started_at).whole_milliseconds()).ok()
}

struct DelegationBlockUpdate {
    tool_call_id: String,
    requesting_profile_id: ProfileId,
    target_kind: ProfileKind,
    target_id: ProfileId,
    task: String,
    status: PersistedDelegationStatus,
    child_operation_id: Option<String>,
    summary: Option<String>,
}

pub(crate) fn transcript_item_id(item: &TranscriptItem) -> Option<String> {
    match item {
        TranscriptItem::UserInput { turn_id, .. } => Some(turn_id.clone()),
        TranscriptItem::AssistantMessage { message_id, .. } => Some(message_id.clone()),
        TranscriptItem::ToolCall { tool_call_id, .. } => Some(tool_call_id.clone()),
        TranscriptItem::DelegationBlock { tool_call_id, .. } => Some(tool_call_id.clone()),
        TranscriptItem::CompactionSummary { .. }
        | TranscriptItem::BranchSummary { .. }
        | TranscriptItem::Diagnostic { .. } => None,
    }
}

fn content_blocks_text(content: &[PersistedContentBlock]) -> String {
    content
        .iter()
        .map(|block| match block {
            PersistedContentBlock::Text { text } => text.clone(),
            PersistedContentBlock::Thinking { thinking, .. } => thinking.clone(),
            PersistedContentBlock::Image { mime_type, .. } => format!("[image:{mime_type}]"),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn tool_result_summary(result: &PersistedToolResult) -> String {
    match result {
        PersistedToolResult::Text { text } => text.clone(),
        PersistedToolResult::Json { value } => value.to_string(),
        PersistedToolResult::Error { message } => message.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::authorization::{
        ToolAuthorizationDecision, ToolAuthorizationPreview, ToolAuthorizationRisk,
        ToolAuthorizationScope,
    };
    use crate::session::event::{
        OperationKind, PersistedRole, PersistedToolAuthorizationResolution,
    };
    use ai::api::model::{Model, ModelCost, ModelInput};

    fn event(
        event_id: &str,
        operation_id: Option<&str>,
        turn_id: Option<&str>,
        data: SessionEventData,
    ) -> SessionEventEnvelope {
        let mut event =
            SessionEventEnvelope::new("sess_replay", event_id, "2026-06-29T00:00:00Z", data);
        if let Some(operation_id) = operation_id {
            event = event.with_operation_id(operation_id);
        }
        if let Some(turn_id) = turn_id {
            event = event.with_turn_id(turn_id);
        }
        event
    }

    fn op_event(event_id: &str, data: SessionEventData) -> SessionEventEnvelope {
        event(event_id, Some("op_1"), Some("turn_1"), data)
    }

    fn op_event_at(
        event_id: &str,
        created_at: &str,
        data: SessionEventData,
    ) -> SessionEventEnvelope {
        let mut event = op_event(event_id, data);
        event.created_at = created_at.into();
        event
    }

    fn authorization_request() -> ToolAuthorizationRequest {
        ToolAuthorizationRequest {
            authorization_id: "auth_1".into(),
            operation_id: "op_1".into(),
            turn_id: "turn_1".into(),
            tool_call_id: "call_1".into(),
            tool_name: "write".into(),
            risk: ToolAuthorizationRisk::FilesystemMutation,
            scope: ToolAuthorizationScope::Path {
                path: "/workspace/file.txt".into(),
            },
            preview: ToolAuthorizationPreview {
                summary: "Modify a file".into(),
                path: Some("/workspace/file.txt".into()),
                command: None,
                cwd: None,
                content_preview: Some("<redacted>".into()),
            },
            capability_generation: 1,
            requested_at: "2026-07-17T00:00:00Z".into(),
        }
    }

    fn delegation_runtime_seed() -> PersistedDelegationRuntimeSeed {
        PersistedDelegationRuntimeSeed {
            mode: "print".into(),
            model: Model {
                id: "test-model".into(),
                name: "Test Model".into(),
                api: "test-api".into(),
                provider: "test".into(),
                base_url: String::new(),
                reasoning: false,
                thinking_level_map: None,
                input: vec![ModelInput::Text],
                cost: ModelCost::default(),
                context_window: 0,
                max_tokens: 0,
                headers: None,
                compat: None,
            },
            system_prompt: Some("runtime instructions".into()),
            max_turns: Some(4),
            tool_names: vec!["read".into()],
            register_builtins: false,
            thinking_level: None,
            tool_execution: None,
            session_name: None,
            parent_delegation_depth: 0,
            delegation_lineage: Vec::new(),
        }
    }

    fn delegation_confirmation_requested(event_id: &str) -> SessionEventEnvelope {
        event(
            event_id,
            Some("op_parent"),
            Some("turn_parent"),
            SessionEventData::DelegationConfirmationRequested {
                source_operation_id: "op_parent".into(),
                turn_id: "turn_parent".into(),
                tool_call_id: "tool_delegate".into(),
                requesting_profile_id: ProfileId::from("planner"),
                target_kind: ProfileKind::Agent,
                target_id: ProfileId::from("coder"),
                task: "implement parser".into(),
                reason: "delegation policy requires confirmation".into(),
                runtime_seed: delegation_runtime_seed(),
            },
        )
    }

    fn parent_operation_committed(event_id: &str) -> SessionEventEnvelope {
        event(
            event_id,
            Some("op_parent"),
            Some("turn_parent"),
            SessionEventData::OperationCommitted { new_leaf_id: None },
        )
    }

    #[test]
    fn unresolved_tool_authorization_replays_even_for_incomplete_operation() {
        let request = authorization_request();
        let replay = fold_events(&[op_event(
            "evt_auth",
            SessionEventData::ToolAuthorizationRequested {
                request: request.clone(),
            },
        )]);

        assert_eq!(replay.pending_tool_authorizations, vec![request]);
        assert!(replay.operation_statuses.is_empty());
    }

    #[test]
    fn durable_tool_authorization_resolution_removes_pending_request() {
        let replay = fold_events(&[
            op_event(
                "evt_request",
                SessionEventData::ToolAuthorizationRequested {
                    request: authorization_request(),
                },
            ),
            op_event(
                "evt_resolution",
                SessionEventData::ToolAuthorizationResolved {
                    authorization_id: "auth_1".into(),
                    resolution: PersistedToolAuthorizationResolution::Approved {
                        decision: ToolAuthorizationDecision::AllowOnce,
                    },
                },
            ),
        ]);

        assert!(replay.pending_tool_authorizations.is_empty());
        assert!(replay.diagnostics.is_empty());
    }

    #[test]
    fn delegation_confirmation_requested_replays_as_pending_confirmation() {
        let events = vec![
            delegation_confirmation_requested("evt_1"),
            parent_operation_committed("evt_2"),
        ];

        let replay = fold_events(&events);

        assert_eq!(replay.pending_delegation_confirmations.len(), 1);
        let pending = &replay.pending_delegation_confirmations[0];
        assert_eq!(pending.source_operation_id, "op_parent");
        assert_eq!(pending.turn_id, "turn_parent");
        assert_eq!(pending.tool_call_id, "tool_delegate");
        assert_eq!(pending.requesting_profile_id.as_str(), "planner");
        assert_eq!(pending.target_kind, ProfileKind::Agent);
        assert_eq!(pending.target_id.as_str(), "coder");
        assert_eq!(pending.task, "implement parser");
        assert_eq!(pending.reason, "delegation policy requires confirmation");
        assert_eq!(pending.requested_at, "2026-06-29T00:00:00Z");
        assert_eq!(pending.runtime_seed.model.id, "test-model");
    }

    #[test]
    fn delegation_confirmation_approved_removes_pending_confirmation() {
        let events = vec![
            delegation_confirmation_requested("evt_1"),
            event(
                "evt_2",
                Some("op_parent"),
                None,
                SessionEventData::DelegationConfirmationApproved {
                    source_operation_id: "op_parent".into(),
                    tool_call_id: "tool_delegate".into(),
                    approval_operation_id: "op_approval".into(),
                },
            ),
            parent_operation_committed("evt_3"),
        ];

        let replay = fold_events(&events);

        assert!(replay.pending_delegation_confirmations.is_empty());
        assert!(replay.diagnostics.is_empty());
    }

    #[test]
    fn delegation_confirmation_rejected_removes_pending_confirmation() {
        let events = vec![
            delegation_confirmation_requested("evt_1"),
            event(
                "evt_2",
                Some("op_parent"),
                None,
                SessionEventData::DelegationConfirmationRejected {
                    source_operation_id: "op_parent".into(),
                    tool_call_id: "tool_delegate".into(),
                    reason: "not now".into(),
                },
            ),
            parent_operation_committed("evt_3"),
        ];

        let replay = fold_events(&events);

        assert!(replay.pending_delegation_confirmations.is_empty());
        assert!(replay.diagnostics.is_empty());
    }

    #[test]
    fn duplicate_delegation_confirmation_request_keeps_first_and_warns() {
        let events = vec![
            delegation_confirmation_requested("evt_1"),
            delegation_confirmation_requested("evt_2"),
            parent_operation_committed("evt_3"),
        ];

        let replay = fold_events(&events);

        assert_eq!(replay.pending_delegation_confirmations.len(), 1);
        assert!(
            replay.diagnostics.iter().any(|diagnostic| {
                diagnostic
                    .message
                    .contains("duplicate pending delegation confirmation")
            }),
            "expected duplicate warning, got {:#?}",
            replay.diagnostics
        );
    }

    #[test]
    fn committed_operation_folds_transcript_in_event_order() {
        let events = vec![
            op_event(
                "evt_1",
                SessionEventData::OperationStarted {
                    operation: OperationKind::Prompt,
                    runtime_generation: Default::default(),
                },
            ),
            op_event("evt_2", SessionEventData::TurnStarted {}),
            op_event(
                "evt_3",
                SessionEventData::TurnInputRecorded {
                    content: vec![PersistedContentBlock::Text {
                        text: "hello".into(),
                    }],
                },
            ),
            op_event(
                "evt_4",
                SessionEventData::MessageStarted {
                    message_id: "msg_1".into(),
                    role: PersistedRole::Assistant,
                },
            ),
            op_event(
                "evt_5",
                SessionEventData::MessageCompleted {
                    message_id: "msg_1".into(),
                    content: vec![PersistedContentBlock::Text { text: "hi".into() }],
                    finish_reason: Some("stop".into()),
                    usage: Default::default(),
                },
            ),
            op_event(
                "evt_7",
                SessionEventData::ToolCallStarted {
                    tool_call_id: "tool_1".into(),
                    name: "read".into(),
                    arguments: serde_json::json!({"path": "src/lib.rs"}),
                },
            ),
            op_event(
                "evt_8",
                SessionEventData::ToolCallCompleted {
                    tool_call_id: "tool_1".into(),
                    result: PersistedToolResult::Text { text: "ok".into() },
                },
            ),
            op_event(
                "evt_9",
                SessionEventData::DiagnosticEmitted {
                    level: DiagnosticLevel::Info,
                    message: "note".into(),
                },
            ),
            op_event(
                "evt_10",
                SessionEventData::OperationCommitted {
                    new_leaf_id: Some("leaf_1".into()),
                },
            ),
        ];

        let replay = fold_events(&events);

        assert_eq!(replay.session_id, "sess_replay");
        assert_eq!(replay.active_leaf_id.as_deref(), Some("leaf_1"));
        assert_eq!(
            replay.transcript,
            vec![
                TranscriptItem::UserInput {
                    turn_id: "turn_1".into(),
                    text: "hello".into(),
                },
                TranscriptItem::AssistantMessage {
                    message_id: "msg_1".into(),
                    content: vec![PersistedContentBlock::Text { text: "hi".into() }],
                    status: MessageStatus::Completed,
                },
                TranscriptItem::ToolCall {
                    tool_call_id: "tool_1".into(),
                    name: "read".into(),
                    arguments: serde_json::json!({"path": "src/lib.rs"}),
                    status: ToolCallStatus::Completed,
                    summary: "ok".into(),
                    started_at: "2026-06-29T00:00:00Z".into(),
                    duration_millis: Some(0),
                },
                TranscriptItem::Diagnostic {
                    level: DiagnosticLevel::Info,
                    message: "note".into(),
                },
            ]
        );
        assert_eq!(
            replay.diagnostics,
            vec![ReplayDiagnostic {
                level: DiagnosticLevel::Info,
                message: "note".into(),
            }]
        );
    }

    #[test]
    fn tool_duration_uses_durable_terminal_event_time() {
        let events = vec![
            op_event(
                "evt_1",
                SessionEventData::OperationStarted {
                    operation: OperationKind::Prompt,
                    runtime_generation: Default::default(),
                },
            ),
            op_event_at(
                "evt_2",
                "2026-06-29T00:00:00.250Z",
                SessionEventData::ToolCallStarted {
                    tool_call_id: "tool_timed".into(),
                    name: "read".into(),
                    arguments: serde_json::json!({"path": "src/lib.rs"}),
                },
            ),
            op_event_at(
                "evt_3",
                "2026-06-29T00:00:01.500Z",
                SessionEventData::ToolCallCompleted {
                    tool_call_id: "tool_timed".into(),
                    result: PersistedToolResult::Text { text: "ok".into() },
                },
            ),
            op_event(
                "evt_4",
                SessionEventData::OperationCommitted { new_leaf_id: None },
            ),
        ];

        let replay = fold_events(&events);
        assert!(matches!(
            replay.transcript.as_slice(),
            [TranscriptItem::ToolCall {
                tool_call_id,
                duration_millis: Some(1_250),
                ..
            }] if tool_call_id == "tool_timed"
        ));
        assert_eq!(
            elapsed_millis("not-a-timestamp", "2026-06-29T00:00:01Z"),
            None
        );
        assert_eq!(
            elapsed_millis("2026-06-29T00:00:02Z", "2026-06-29T00:00:01Z"),
            None
        );
    }

    #[test]
    fn session_created_records_cwd() {
        let events = vec![event(
            "evt_1",
            None,
            None,
            SessionEventData::SessionCreated {
                cwd: Some("/work".into()),
            },
        )];

        let replay = fold_events(&events);

        assert_eq!(replay.cwd.as_deref(), Some("/work"));
    }

    #[test]
    fn incomplete_operation_is_omitted_and_diagnosed() {
        let events = vec![
            op_event(
                "evt_1",
                SessionEventData::OperationStarted {
                    operation: OperationKind::Prompt,
                    runtime_generation: Default::default(),
                },
            ),
            op_event(
                "evt_2",
                SessionEventData::TurnInputRecorded {
                    content: vec![PersistedContentBlock::Text {
                        text: "ignored".into(),
                    }],
                },
            ),
        ];

        let replay = fold_events(&events);

        assert!(replay.transcript.is_empty());
        assert_eq!(replay.diagnostics.len(), 1);
        assert_eq!(replay.diagnostics[0].level, DiagnosticLevel::Warn);
        assert!(
            replay.diagnostics[0]
                .message
                .contains("operation op_1 has no final marker")
        );
    }

    #[test]
    fn aborted_operation_marks_open_items_cancelled() {
        let events = vec![
            op_event(
                "evt_1",
                SessionEventData::OperationStarted {
                    operation: OperationKind::Prompt,
                    runtime_generation: Default::default(),
                },
            ),
            op_event(
                "evt_2",
                SessionEventData::MessageStarted {
                    message_id: "msg_1".into(),
                    role: PersistedRole::Assistant,
                },
            ),
            op_event(
                "evt_3",
                SessionEventData::ToolCallStarted {
                    tool_call_id: "tool_1".into(),
                    name: "bash".into(),
                    arguments: serde_json::json!({"cmd": "cargo test"}),
                },
            ),
            op_event(
                "evt_5",
                SessionEventData::MessageCancelled {
                    message_id: "msg_1".into(),
                    reason: "abort".into(),
                },
            ),
            op_event(
                "evt_6",
                SessionEventData::ToolCallCancelled {
                    tool_call_id: "tool_1".into(),
                    reason: "abort".into(),
                },
            ),
            op_event(
                "evt_7",
                SessionEventData::OperationAborted {
                    reason: "abort".into(),
                },
            ),
        ];

        let replay = fold_events(&events);

        assert_eq!(
            replay.transcript,
            vec![
                TranscriptItem::AssistantMessage {
                    message_id: "msg_1".into(),
                    content: Vec::new(),
                    status: MessageStatus::Cancelled,
                },
                TranscriptItem::ToolCall {
                    tool_call_id: "tool_1".into(),
                    name: "bash".into(),
                    arguments: serde_json::json!({"cmd": "cargo test"}),
                    status: ToolCallStatus::Cancelled,
                    summary: "abort".into(),
                    started_at: "2026-06-29T00:00:00Z".into(),
                    duration_millis: Some(0),
                },
            ]
        );
        assert!(
            replay
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.message.contains("operation op_1 aborted"))
        );
    }

    #[test]
    fn failed_operation_keeps_error_diagnostic_and_failure_marker() {
        let events = vec![
            op_event(
                "evt_1",
                SessionEventData::OperationStarted {
                    operation: OperationKind::Prompt,
                    runtime_generation: Default::default(),
                },
            ),
            op_event(
                "evt_2",
                SessionEventData::DiagnosticEmitted {
                    level: DiagnosticLevel::Error,
                    message: "stream failed".into(),
                },
            ),
            op_event(
                "evt_3",
                SessionEventData::OperationFailed {
                    error_code: "provider".into(),
                    message: "stream failed".into(),
                },
            ),
        ];

        let replay = fold_events(&events);

        assert_eq!(
            replay.transcript,
            vec![TranscriptItem::Diagnostic {
                level: DiagnosticLevel::Error,
                message: "stream failed".into(),
            }]
        );
        assert_eq!(
            replay.diagnostics,
            vec![
                ReplayDiagnostic {
                    level: DiagnosticLevel::Error,
                    message: "stream failed".into(),
                },
                ReplayDiagnostic {
                    level: DiagnosticLevel::Error,
                    message: "operation op_1 failed (provider): stream failed".into(),
                },
            ]
        );
    }

    #[test]
    fn global_active_leaf_event_updates_replay_leaf() {
        let events = vec![event(
            "evt_1",
            None,
            None,
            SessionEventData::ActiveLeafChanged {
                leaf_id: "leaf_global".into(),
            },
        )];

        let replay = fold_events(&events);

        assert_eq!(replay.active_leaf_id.as_deref(), Some("leaf_global"));
    }

    #[test]
    fn committed_prompt_operations_record_leaf_transcript_ranges() {
        let events = vec![
            event(
                "evt_1",
                Some("op_root"),
                Some("turn_root"),
                SessionEventData::OperationStarted {
                    operation: OperationKind::Prompt,
                    runtime_generation: Default::default(),
                },
            ),
            event(
                "evt_2",
                Some("op_root"),
                Some("turn_root"),
                SessionEventData::TurnInputRecorded {
                    content: vec![PersistedContentBlock::Text {
                        text: "root prompt".into(),
                    }],
                },
            ),
            event(
                "evt_3",
                Some("op_root"),
                Some("turn_root"),
                SessionEventData::OperationCommitted {
                    new_leaf_id: Some("leaf_root".into()),
                },
            ),
            event(
                "evt_4",
                Some("op_branch"),
                Some("turn_branch"),
                SessionEventData::OperationStarted {
                    operation: OperationKind::Prompt,
                    runtime_generation: Default::default(),
                },
            ),
            event(
                "evt_5",
                Some("op_branch"),
                Some("turn_branch"),
                SessionEventData::TurnInputRecorded {
                    content: vec![PersistedContentBlock::Text {
                        text: "branch prompt".into(),
                    }],
                },
            ),
            event(
                "evt_6",
                Some("op_branch"),
                Some("turn_branch"),
                SessionEventData::OperationCommitted {
                    new_leaf_id: Some("leaf_branch".into()),
                },
            ),
            event(
                "evt_7",
                None,
                None,
                SessionEventData::ActiveLeafChanged {
                    leaf_id: "leaf_root".into(),
                },
            ),
            event(
                "evt_8",
                Some("op_alt"),
                Some("turn_alt"),
                SessionEventData::OperationStarted {
                    operation: OperationKind::Prompt,
                    runtime_generation: Default::default(),
                },
            ),
            event(
                "evt_9",
                Some("op_alt"),
                Some("turn_alt"),
                SessionEventData::TurnInputRecorded {
                    content: vec![PersistedContentBlock::Text {
                        text: "alternate prompt".into(),
                    }],
                },
            ),
            event(
                "evt_10",
                Some("op_alt"),
                Some("turn_alt"),
                SessionEventData::OperationCommitted {
                    new_leaf_id: Some("leaf_alt".into()),
                },
            ),
        ];

        let replay = fold_events(&events);

        assert_eq!(
            replay.leaves,
            vec![
                ReplayLeaf {
                    leaf_id: "leaf_root".into(),
                    parent_leaf_id: None,
                    transcript_start: 0,
                    transcript_end: 1,
                },
                ReplayLeaf {
                    leaf_id: "leaf_branch".into(),
                    parent_leaf_id: Some("leaf_root".into()),
                    transcript_start: 1,
                    transcript_end: 2,
                },
                ReplayLeaf {
                    leaf_id: "leaf_alt".into(),
                    parent_leaf_id: Some("leaf_root".into()),
                    transcript_start: 2,
                    transcript_end: 3,
                },
            ]
        );
        assert_eq!(replay.active_leaf_id.as_deref(), Some("leaf_alt"));
    }

    #[test]
    fn branch_summary_created_replays_as_transcript_item() {
        let events = vec![
            op_event(
                "evt_1",
                SessionEventData::OperationStarted {
                    operation: OperationKind::BranchSummary,
                    runtime_generation: Default::default(),
                },
            ),
            op_event(
                "evt_2",
                SessionEventData::BranchSummaryCreated {
                    summary: "summary of abandoned work".into(),
                    source_leaf_id: "leaf_old".into(),
                    target_leaf_id: "leaf_target".into(),
                },
            ),
            op_event(
                "evt_3",
                SessionEventData::OperationCommitted { new_leaf_id: None },
            ),
        ];

        let replay = fold_events(&events);

        assert_eq!(
            replay.transcript,
            vec![TranscriptItem::BranchSummary {
                summary: "summary of abandoned work".into(),
                source_leaf_id: "leaf_old".into(),
                target_leaf_id: "leaf_target".into(),
            }]
        );
        assert_eq!(replay.active_leaf_id, None);
        assert!(replay.diagnostics.is_empty());
    }

    #[test]
    fn session_compaction_completed_folds_transcript_to_summary_and_kept_tail() {
        let events = vec![
            op_event(
                "evt_1",
                SessionEventData::OperationStarted {
                    operation: OperationKind::Prompt,
                    runtime_generation: Default::default(),
                },
            ),
            op_event(
                "evt_2",
                SessionEventData::TurnInputRecorded {
                    content: vec![PersistedContentBlock::Text {
                        text: "old prompt".into(),
                    }],
                },
            ),
            op_event(
                "evt_3",
                SessionEventData::MessageStarted {
                    message_id: "msg_old".into(),
                    role: PersistedRole::Assistant,
                },
            ),
            op_event(
                "evt_4",
                SessionEventData::MessageCompleted {
                    message_id: "msg_old".into(),
                    content: vec![PersistedContentBlock::Text {
                        text: "old answer".into(),
                    }],
                    finish_reason: Some("stop".into()),
                    usage: Default::default(),
                },
            ),
            op_event(
                "evt_6",
                SessionEventData::MessageStarted {
                    message_id: "msg_kept".into(),
                    role: PersistedRole::Assistant,
                },
            ),
            op_event(
                "evt_7",
                SessionEventData::MessageCompleted {
                    message_id: "msg_kept".into(),
                    content: vec![PersistedContentBlock::Text {
                        text: "kept answer".into(),
                    }],
                    finish_reason: Some("stop".into()),
                    usage: Default::default(),
                },
            ),
            op_event(
                "evt_9",
                SessionEventData::SessionCompactionCompleted {
                    summary: "summary of old context".into(),
                    first_kept_message_id: "msg_kept".into(),
                    tokens_before: 1200,
                },
            ),
            op_event(
                "evt_10",
                SessionEventData::OperationCommitted {
                    new_leaf_id: Some("leaf_1".into()),
                },
            ),
        ];

        let replay = fold_events(&events);

        assert_eq!(
            replay.transcript,
            vec![
                TranscriptItem::CompactionSummary {
                    summary: "summary of old context".into(),
                    first_kept_message_id: "msg_kept".into(),
                    tokens_before: 1200,
                },
                TranscriptItem::AssistantMessage {
                    message_id: "msg_kept".into(),
                    content: vec![PersistedContentBlock::Text {
                        text: "kept answer".into(),
                    }],
                    status: MessageStatus::Completed,
                },
            ]
        );
        assert!(replay.diagnostics.is_empty());
    }

    #[test]
    fn session_compaction_completed_warns_when_kept_id_is_unknown() {
        let events = vec![event(
            "evt_1",
            None,
            None,
            SessionEventData::SessionCompactionCompleted {
                summary: "summary".into(),
                first_kept_message_id: "missing".into(),
                tokens_before: 1200,
            },
        )];

        let replay = fold_events(&events);

        assert!(replay.transcript.is_empty());
        assert_eq!(replay.diagnostics.len(), 1);
        assert!(
            replay.diagnostics[0]
                .message
                .contains("unknown first kept message")
        );
    }

    #[test]
    fn replay_classifies_terminal_operation_statuses() {
        let events = vec![
            op_event(
                "evt_1",
                SessionEventData::OperationStarted {
                    operation: OperationKind::Prompt,
                    runtime_generation: Default::default(),
                },
            ),
            op_event(
                "evt_2",
                SessionEventData::OperationCommitted { new_leaf_id: None },
            ),
            event(
                "evt_3",
                Some("op_failed"),
                Some("turn_failed"),
                SessionEventData::OperationStarted {
                    operation: OperationKind::Prompt,
                    runtime_generation: Default::default(),
                },
            ),
            event(
                "evt_4",
                Some("op_failed"),
                Some("turn_failed"),
                SessionEventData::OperationFailed {
                    error_code: "provider".into(),
                    message: "provider failed".into(),
                },
            ),
            event(
                "evt_5",
                Some("op_aborted"),
                Some("turn_aborted"),
                SessionEventData::OperationStarted {
                    operation: OperationKind::Prompt,
                    runtime_generation: Default::default(),
                },
            ),
            event(
                "evt_6",
                Some("op_aborted"),
                Some("turn_aborted"),
                SessionEventData::OperationAborted {
                    reason: "user abort".into(),
                },
            ),
        ];

        let replay = fold_events(&events);

        assert_eq!(
            replay.operation_status("op_1"),
            Some(OperationReplayStatus::Committed)
        );
        assert_eq!(
            replay.operation_status("op_failed"),
            Some(OperationReplayStatus::Failed)
        );
        assert_eq!(
            replay.operation_status("op_aborted"),
            Some(OperationReplayStatus::Aborted)
        );
    }

    #[test]
    fn replay_marks_started_operation_without_terminal_marker_in_doubt() {
        let events = vec![op_event(
            "evt_1",
            SessionEventData::OperationStarted {
                operation: OperationKind::Prompt,
                runtime_generation: Default::default(),
            },
        )];

        let replay = fold_events(&events);

        assert_eq!(
            replay.operation_status("op_1"),
            Some(OperationReplayStatus::InDoubt)
        );
    }

    #[test]
    fn replay_classifies_recovered_operation() {
        let events = vec![
            op_event(
                "evt_1",
                SessionEventData::OperationStarted {
                    operation: OperationKind::Prompt,
                    runtime_generation: Default::default(),
                },
            ),
            op_event(
                "evt_2",
                SessionEventData::TurnInputRecorded {
                    content: vec![PersistedContentBlock::Text {
                        text: "recovered input".into(),
                    }],
                },
            ),
            event(
                "evt_3",
                Some("op_1"),
                Some("turn_1"),
                SessionEventData::OperationRecovered {
                    reason: "manual recovery".into(),
                    recovery_id: "recovery_1".into(),
                },
            ),
        ];

        let replay = fold_events(&events);

        assert_eq!(
            replay.operation_status("op_1"),
            Some(OperationReplayStatus::Recovered)
        );
        assert!(
            !replay.transcript.is_empty(),
            "recovered operation payload should not be omitted as incomplete"
        );
        assert!(
            !replay
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.message.contains("no final marker")),
            "recovered operation should not be warned as incomplete"
        );
    }

    #[test]
    fn operation_recovered_marker_finalizes_in_doubt_operation() {
        let replay = fold_events(&[
            event(
                "evt_started",
                Some("op_recovered"),
                Some("turn_1"),
                SessionEventData::OperationStarted {
                    operation: OperationKind::Prompt,
                    runtime_generation: Default::default(),
                },
            ),
            event(
                "evt_recovered",
                Some("op_recovered"),
                Some("turn_1"),
                SessionEventData::OperationRecovered {
                    reason: "startup recovery marked incomplete operation in-doubt".into(),
                    recovery_id: "op_recovery_1".into(),
                },
            ),
        ]);

        assert_eq!(
            replay.operation_status("op_recovered"),
            Some(OperationReplayStatus::Recovered)
        );
        assert!(replay.recovery_summary().in_doubt_operations.is_empty());
    }
}
