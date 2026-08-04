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
        /// Wall-clock time the turn was submitted (RFC 3339). `None` for
        /// in-memory transcripts that never persisted the event envelope.
        started_at: Option<String>,
    },
    AssistantMessage {
        message_id: String,
        content: Vec<PersistedContentBlock>,
        status: MessageStatus,
        reasoning_duration_millis: Option<u64>,
        /// Model that actually produced this message; `None` while the
        /// message is still streaming or for legacy session files.
        model_id: Option<String>,
        /// Wall-clock completion time (RFC 3339) when the message finished
        /// streaming; `None` while running or for legacy session files.
        completed_at: Option<String>,
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
    reasoning_started_at: HashMap<(String, u32), String>,
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

mod fold;

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
            PersistedContentBlock::ProviderItem { api, .. } => format!("[provider_item:{api}]"),
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
mod message_model_attribution_tests {
    use super::*;
    use crate::session::event::{PersistedContentBlock, SessionEventData, SessionEventEnvelope};

    fn message_started(message_id: &str, event_id: &str) -> SessionEventEnvelope {
        SessionEventEnvelope::new(
            "session-model",
            event_id,
            "2026-01-01T00:00:00Z",
            SessionEventData::MessageStarted {
                message_id: message_id.into(),
                role: crate::session::event::PersistedRole::Assistant,
            },
        )
    }

    fn message_completed(
        message_id: &str,
        event_id: &str,
        model_id: Option<&str>,
    ) -> SessionEventEnvelope {
        SessionEventEnvelope::new(
            "session-model",
            event_id,
            "2026-01-01T00:00:01Z",
            SessionEventData::MessageCompleted {
                message_id: message_id.into(),
                content: vec![PersistedContentBlock::Text {
                    text: "answer".into(),
                }],
                finish_reason: None,
                usage: Default::default(),
                model_id: model_id.map(str::to_owned),
            },
        )
    }

    #[test]
    fn message_completed_attributes_the_model_to_the_transcript_item() {
        let replay = fold_events(&[
            message_started("message-1", "event-1"),
            message_completed("message-1", "event-2", Some("deepseek-v4-pro")),
        ]);
        let [
            TranscriptItem::AssistantMessage {
                model_id,
                completed_at,
                ..
            },
        ] = replay.transcript.as_slice()
        else {
            panic!(
                "expected one assistant message, got {:?}",
                replay.transcript
            );
        };
        assert_eq!(model_id.as_deref(), Some("deepseek-v4-pro"));
        assert_eq!(
            completed_at.as_deref(),
            Some("2026-01-01T00:00:01Z"),
            "the completion event's created_at lands on the transcript item"
        );
    }

    #[test]
    fn turn_input_recorded_carries_the_submit_time_into_the_transcript() {
        let started = SessionEventEnvelope::new(
            "session-model",
            "event-turn",
            "2026-01-01T00:00:00Z",
            SessionEventData::TurnInputRecorded {
                content: vec![PersistedContentBlock::Text {
                    text: "do it".into(),
                }],
            },
        );
        let replay = fold_events(&[started]);
        let [TranscriptItem::UserInput { started_at, .. }] = replay.transcript.as_slice() else {
            panic!("expected one user input, got {:?}", replay.transcript);
        };
        assert_eq!(started_at.as_deref(), Some("2026-01-01T00:00:00Z"));
    }

    #[test]
    fn legacy_message_completed_without_model_id_stays_unattributed() {
        let replay = fold_events(&[
            message_started("message-2", "event-3"),
            message_completed("message-2", "event-4", None),
        ]);
        let [TranscriptItem::AssistantMessage { model_id, .. }] = replay.transcript.as_slice()
        else {
            panic!(
                "expected one assistant message, got {:?}",
                replay.transcript
            );
        };
        assert!(model_id.is_none(), "legacy events must stay unattributed");
    }

    #[test]
    fn provider_web_search_lifecycle_replays_as_a_completed_tool_call() {
        let started = SessionEventEnvelope::new(
            "session-model",
            "event-ws-start",
            "2026-01-01T00:00:00Z",
            SessionEventData::ToolCallStarted {
                tool_call_id: "call_ws_1".into(),
                name: "web_search".into(),
                arguments: serde_json::json!({
                    "type": "web_search_call",
                    "id": "call_ws_1",
                    "status": "in_progress"
                }),
            },
        );
        let updated = SessionEventEnvelope::new(
            "session-model",
            "event-ws-update",
            "2026-01-01T00:00:00.5Z",
            SessionEventData::ToolCallUpdated {
                tool_call_id: "call_ws_1".into(),
                message: "searching".into(),
            },
        );
        let completed = SessionEventEnvelope::new(
            "session-model",
            "event-ws-done",
            "2026-01-01T00:00:01Z",
            SessionEventData::ToolCallCompleted {
                tool_call_id: "call_ws_1".into(),
                result: PersistedToolResult::Json {
                    value: serde_json::json!({
                        "status": "completed",
                        "action": {"type": "search", "queries": ["2025年诺贝尔物理学奖"]}
                    }),
                },
            },
        );
        let replay = fold_events(&[started, updated, completed]);
        let [
            TranscriptItem::ToolCall {
                name,
                summary,
                status,
                ..
            },
        ] = replay.transcript.as_slice()
        else {
            panic!("expected one tool call, got {:?}", replay.transcript);
        };
        assert_eq!(name, "web_search");
        assert_eq!(status, &ToolCallStatus::Completed);
        assert!(
            serde_json::from_str::<serde_json::Value>(summary).is_ok_and(|value| value
                == serde_json::json!({
                    "status": "completed",
                    "action": {"type": "search", "queries": ["2025年诺贝尔物理学奖"]}
                })),
            "replayed summary must carry the action, got {summary:?}"
        );
    }
}
