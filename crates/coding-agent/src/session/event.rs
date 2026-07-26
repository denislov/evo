use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::authorization::{ToolAuthorizationDecision, ToolAuthorizationRequest};
use crate::operations::delegation::DelegationLineageEntry;
use crate::profiles::{ProfileId, ProfileKind};
use ai::api::conversation::Usage;
use ai::api::model::Model;

use super::manifest::{EVENT_SCHEMA, EVENT_VERSION};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct SessionEventEnvelope {
    pub schema: String,
    pub version: u32,
    pub session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_sequence: Option<u64>,
    pub event_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub operation_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub leaf_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_event_id: Option<String>,
    pub created_at: String,
    #[serde(flatten)]
    pub data: SessionEventData,
}

impl SessionEventEnvelope {
    pub(crate) fn new(
        session_id: impl Into<String>,
        event_id: impl Into<String>,
        created_at: impl Into<String>,
        data: SessionEventData,
    ) -> Self {
        Self {
            schema: EVENT_SCHEMA.into(),
            version: EVENT_VERSION,
            session_id: session_id.into(),
            session_sequence: None,
            event_id: event_id.into(),
            operation_id: None,
            turn_id: None,
            branch_id: None,
            leaf_id: None,
            parent_event_id: None,
            created_at: created_at.into(),
            data,
        }
    }

    pub(crate) fn with_session_sequence(mut self, sequence: u64) -> Self {
        self.session_sequence = Some(sequence);
        self
    }

    pub(crate) fn with_operation_id(mut self, operation_id: impl Into<String>) -> Self {
        self.operation_id = Some(operation_id.into());
        self
    }

    pub(crate) fn with_turn_id(mut self, turn_id: impl Into<String>) -> Self {
        self.turn_id = Some(turn_id.into());
        self
    }

    #[cfg(test)]
    pub(crate) fn with_branch_id(mut self, branch_id: impl Into<String>) -> Self {
        self.branch_id = Some(branch_id.into());
        self
    }

    #[cfg(test)]
    pub(crate) fn with_leaf_id(mut self, leaf_id: impl Into<String>) -> Self {
        self.leaf_id = Some(leaf_id.into());
        self
    }

    #[cfg(test)]
    pub(crate) fn with_parent_event_id(mut self, parent_event_id: impl Into<String>) -> Self {
        self.parent_event_id = Some(parent_event_id.into());
        self
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "data")]
#[allow(
    clippy::large_enum_variant,
    reason = "durable session event variants retain their versioned serialized payload shape"
)]
pub(crate) enum SessionEventData {
    #[serde(rename = "session.created")]
    SessionCreated { cwd: Option<String> },
    #[serde(rename = "session.cloned")]
    SessionCloned {
        source_session_id: String,
        source_leaf_id: String,
    },
    #[serde(rename = "session.forked")]
    SessionForked {
        source_session_id: String,
        source_leaf_id: String,
    },
    #[serde(rename = "session.compaction.started")]
    SessionCompactionStarted {
        first_kept_message_id: String,
        tokens_before: u32,
    },
    #[serde(rename = "session.compaction.completed")]
    SessionCompactionCompleted {
        summary: String,
        first_kept_message_id: String,
        tokens_before: u32,
    },
    #[serde(rename = "branch.summary.created")]
    BranchSummaryCreated {
        summary: String,
        source_leaf_id: String,
        target_leaf_id: String,
    },
    #[serde(rename = "session.tree_label.updated")]
    SessionTreeLabelUpdated {
        entry_id: String,
        label: Option<String>,
    },
    #[serde(rename = "delegation.confirmation.requested")]
    DelegationConfirmationRequested {
        source_operation_id: String,
        turn_id: String,
        tool_call_id: String,
        requesting_profile_id: ProfileId,
        target_kind: ProfileKind,
        target_id: ProfileId,
        task: String,
        reason: String,
        runtime_seed: PersistedDelegationRuntimeSeed,
    },
    #[serde(rename = "delegation.confirmation.approved")]
    DelegationConfirmationApproved {
        source_operation_id: String,
        tool_call_id: String,
        approval_operation_id: String,
    },
    #[serde(rename = "delegation.confirmation.rejected")]
    DelegationConfirmationRejected {
        source_operation_id: String,
        tool_call_id: String,
        reason: String,
    },
    #[serde(rename = "delegation.folded.updated")]
    DelegationFoldedUpdated {
        tool_call_id: String,
        requesting_profile_id: ProfileId,
        target_kind: ProfileKind,
        target_id: ProfileId,
        task: String,
        status: PersistedDelegationStatus,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        child_operation_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        summary: Option<String>,
    },
    #[serde(rename = "tool.authorization.requested")]
    ToolAuthorizationRequested { request: ToolAuthorizationRequest },
    #[serde(rename = "tool.authorization.resolved")]
    ToolAuthorizationResolved {
        authorization_id: String,
        resolution: PersistedToolAuthorizationResolution,
    },
    #[serde(rename = "operation.started")]
    OperationStarted {
        operation: OperationKind,
        #[serde(
            default,
            skip_serializing_if = "PersistedRuntimeGenerationRef::is_empty"
        )]
        runtime_generation: PersistedRuntimeGenerationRef,
    },
    #[serde(rename = "operation.committed")]
    OperationCommitted { new_leaf_id: Option<String> },
    #[serde(rename = "operation.aborted")]
    OperationAborted { reason: String },
    #[serde(rename = "operation.failed")]
    OperationFailed { error_code: String, message: String },
    #[serde(rename = "operation.terminal.recorded")]
    OperationTerminalRecorded {
        status: String,
        semantic_event_id: String,
    },
    #[serde(rename = "operation.recovery_pending")]
    OperationRecoveryPending {
        reason: String,
        recovery_id: String,
        #[serde(default = "default_recovery_record_version")]
        record_version: u64,
        #[serde(default = "default_operation_descriptor_revision")]
        descriptor_revision: u16,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        capability_generation: Option<u64>,
        #[serde(default)]
        attempt_count: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        last_attempt_at: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        next_attempt_at: Option<String>,
    },
    #[serde(rename = "operation.recovery_resolved")]
    OperationRecoveryResolved {
        recovery_id: String,
        record_version: u64,
        descriptor_revision: u16,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        capability_generation: Option<u64>,
        resolution: PersistedRecoveryResolution,
        reason: String,
        #[serde(default = "default_recovery_authority")]
        authorization_subject: String,
    },
    #[serde(rename = "operation.recovered")]
    OperationRecovered { reason: String, recovery_id: String },
    #[serde(rename = "turn.started")]
    TurnStarted {},
    #[serde(rename = "turn.input.recorded")]
    TurnInputRecorded { content: Vec<PersistedContentBlock> },
    #[serde(rename = "message.started")]
    MessageStarted {
        message_id: String,
        role: PersistedRole,
    },
    #[serde(rename = "message.reasoning.started")]
    MessageReasoningStarted {
        message_id: String,
        content_index: u32,
    },
    #[serde(rename = "message.reasoning.completed")]
    MessageReasoningCompleted {
        message_id: String,
        content_index: u32,
    },
    #[serde(rename = "message.completed")]
    MessageCompleted {
        message_id: String,
        content: Vec<PersistedContentBlock>,
        finish_reason: Option<String>,
        #[serde(default)]
        usage: Usage,
    },
    #[serde(rename = "message.cancelled")]
    MessageCancelled { message_id: String, reason: String },
    #[serde(rename = "tool.call.started")]
    ToolCallStarted {
        tool_call_id: String,
        name: String,
        arguments: Value,
    },
    #[serde(rename = "tool.call.updated")]
    ToolCallUpdated {
        tool_call_id: String,
        message: String,
    },
    #[serde(rename = "tool.call.completed")]
    ToolCallCompleted {
        tool_call_id: String,
        result: PersistedToolResult,
    },
    #[serde(rename = "tool.call.failed")]
    ToolCallFailed {
        tool_call_id: String,
        message: String,
    },
    #[serde(rename = "tool.call.cancelled")]
    ToolCallCancelled {
        tool_call_id: String,
        reason: String,
    },
    #[serde(rename = "self_healing_edit.started")]
    SelfHealingEditStarted { path: String, replacements: usize },
    #[serde(rename = "self_healing_edit.repair_attempted")]
    SelfHealingEditRepairAttempted {
        path: String,
        attempt: usize,
        replacements: Vec<PersistedSelfHealingEditReplacement>,
        diagnostics: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        check_output: Option<PersistedSelfHealingEditCheckOutput>,
    },
    #[serde(rename = "self_healing_edit.completed")]
    SelfHealingEditCompleted {
        path: String,
        message: String,
        diff: String,
        patch: String,
        first_changed_line: Option<usize>,
        attempts: usize,
        diagnostics: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        check_output: Option<PersistedSelfHealingEditCheckOutput>,
    },
    #[serde(rename = "diagnostic.emitted")]
    DiagnosticEmitted {
        level: DiagnosticLevel,
        message: String,
    },
    #[serde(rename = "metadata.updated")]
    MetadataUpdated { key: String, value: Value },
    #[serde(rename = "active_leaf.changed")]
    ActiveLeafChanged { leaf_id: String },
}

fn default_recovery_authority() -> String {
    "trusted_host".into()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum PersistedRecoveryResolution {
    Failed,
    Aborted,
}

fn default_recovery_record_version() -> u64 {
    crate::events::recovery::RECOVERY_RECORD_VERSION
}

fn default_operation_descriptor_revision() -> u16 {
    crate::runtime::outcome::OPERATION_DESCRIPTOR_REVISION
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub(crate) enum PersistedToolAuthorizationResolution {
    Approved { decision: ToolAuthorizationDecision },
    Denied { reason: String },
    Cancelled { reason: String },
    Interrupted { reason: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "data", rename_all = "snake_case")]
pub(crate) enum OperationKind {
    Prompt,
    ManualCompaction,
    BranchSummary,
    Export,
    SelfHealingEdit,
    SessionTreeLabel,
    Other { name: String },
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct PersistedRuntimeGenerationRef {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) profile_id: Option<ProfileId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) capability_generation: Option<u64>,
}

impl PersistedRuntimeGenerationRef {
    pub(crate) fn is_empty(&self) -> bool {
        self.profile_id.is_none() && self.capability_generation.is_none()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum PersistedDelegationStatus {
    Requested,
    Running,
    Completed,
    Failed,
    Rejected,
    Cancelled,
    ConfirmationRequired,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum PersistedRole {
    User,
    Assistant,
    Tool,
    System,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub(crate) enum PersistedContentBlock {
    Text {
        text: String,
    },
    Thinking {
        thinking: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        thinking_signature: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        redacted: Option<bool>,
    },
    Image {
        mime_type: String,
        data: String,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "data", rename_all = "snake_case")]
pub(crate) enum PersistedToolResult {
    Text { text: String },
    Json { value: Value },
    Error { message: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct PersistedSelfHealingEditCheckOutput {
    pub(crate) command: String,
    pub(crate) stdout: String,
    pub(crate) stderr: String,
    pub(crate) exit_code: i32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct PersistedSelfHealingEditReplacement {
    pub(crate) old_text: String,
    pub(crate) new_text: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct PersistedDelegationRuntimeSeed {
    pub(crate) mode: String,
    pub(crate) model: Model,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) system_prompt: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) max_turns: Option<u32>,
    pub(crate) tool_names: Vec<String>,
    pub(crate) register_builtins: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) thinking_level: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) tool_execution: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) session_name: Option<String>,
    pub(crate) parent_delegation_depth: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) delegation_lineage: Vec<DelegationLineageEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum DiagnosticLevel {
    Debug,
    Info,
    Warn,
    Error,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::authorization::{
        ToolAuthorizationPreview, ToolAuthorizationRisk, ToolAuthorizationScope,
    };
    use ai::api::model::{ModelCost, ModelInput};
    use serde_json::json;

    #[test]
    fn event_envelope_serializes_kind_and_data_at_top_level() {
        let envelope = SessionEventEnvelope::new(
            "sess_1",
            "evt_1",
            "2026-06-29T00:00:00Z",
            SessionEventData::TurnStarted {},
        )
        .with_operation_id("op_1")
        .with_turn_id("turn_1")
        .with_branch_id("branch_1")
        .with_leaf_id("leaf_1")
        .with_parent_event_id("evt_0");

        let value = serde_json::to_value(&envelope).unwrap();
        assert_eq!(value["schema"], EVENT_SCHEMA);
        assert_eq!(value["version"], EVENT_VERSION);
        assert_eq!(value["session_id"], "sess_1");
        assert_eq!(value["event_id"], "evt_1");
        assert_eq!(value["operation_id"], "op_1");
        assert_eq!(value["turn_id"], "turn_1");
        assert_eq!(value["branch_id"], "branch_1");
        assert_eq!(value["leaf_id"], "leaf_1");
        assert_eq!(value["parent_event_id"], "evt_0");
        assert_eq!(value["created_at"], "2026-06-29T00:00:00Z");
        assert_eq!(value["kind"], "turn.started");
        assert_eq!(value["data"], json!({}));

        let decoded: SessionEventEnvelope = serde_json::from_value(value).unwrap();
        assert_eq!(decoded, envelope);
    }

    #[test]
    fn session_event_data_variants_keep_stable_kind_names() {
        let cases = [
            (
                SessionEventData::SessionCreated { cwd: None },
                "session.created",
            ),
            (
                SessionEventData::SessionCloned {
                    source_session_id: "sess_source".into(),
                    source_leaf_id: "leaf_source".into(),
                },
                "session.cloned",
            ),
            (
                SessionEventData::SessionForked {
                    source_session_id: "sess_source".into(),
                    source_leaf_id: "leaf_source".into(),
                },
                "session.forked",
            ),
            (
                SessionEventData::SessionCompactionStarted {
                    first_kept_message_id: "msg_1".into(),
                    tokens_before: 1200,
                },
                "session.compaction.started",
            ),
            (
                SessionEventData::SessionCompactionCompleted {
                    summary: "summary".into(),
                    first_kept_message_id: "msg_1".into(),
                    tokens_before: 1200,
                },
                "session.compaction.completed",
            ),
            (
                SessionEventData::BranchSummaryCreated {
                    summary: "summary".into(),
                    source_leaf_id: "leaf_old".into(),
                    target_leaf_id: "leaf_target".into(),
                },
                "branch.summary.created",
            ),
            (
                SessionEventData::SessionTreeLabelUpdated {
                    entry_id: "leaf_target".into(),
                    label: Some("checkpoint".into()),
                },
                "session.tree_label.updated",
            ),
            (
                SessionEventData::SelfHealingEditStarted {
                    path: "src/app.txt".into(),
                    replacements: 1,
                },
                "self_healing_edit.started",
            ),
            (
                SessionEventData::SelfHealingEditRepairAttempted {
                    path: "src/app.txt".into(),
                    attempt: 1,
                    replacements: vec![PersistedSelfHealingEditReplacement {
                        old_text: "deux".into(),
                        new_text: "dos".into(),
                    }],
                    diagnostics: vec!["compile error".into()],
                    check_output: None,
                },
                "self_healing_edit.repair_attempted",
            ),
            (
                SessionEventData::SelfHealingEditCompleted {
                    path: "src/app.txt".into(),
                    message: "Successfully replaced 1 block".into(),
                    diff: "-two\n+deux".into(),
                    patch: "--- src/app.txt\n+++ src/app.txt".into(),
                    first_changed_line: Some(2),
                    attempts: 1,
                    diagnostics: Vec::new(),
                    check_output: None,
                },
                "self_healing_edit.completed",
            ),
            (
                SessionEventData::DelegationConfirmationRequested {
                    source_operation_id: "op_parent".into(),
                    turn_id: "turn_parent".into(),
                    tool_call_id: "tool_delegate".into(),
                    requesting_profile_id: ProfileId::from("planner"),
                    target_kind: ProfileKind::Agent,
                    target_id: ProfileId::from("coder"),
                    task: "implement parser".into(),
                    reason: "delegation policy requires confirmation".into(),
                    runtime_seed: test_delegation_runtime_seed(),
                },
                "delegation.confirmation.requested",
            ),
            (
                SessionEventData::DelegationConfirmationApproved {
                    source_operation_id: "op_parent".into(),
                    tool_call_id: "tool_delegate".into(),
                    approval_operation_id: "op_approval".into(),
                },
                "delegation.confirmation.approved",
            ),
            (
                SessionEventData::DelegationConfirmationRejected {
                    source_operation_id: "op_parent".into(),
                    tool_call_id: "tool_delegate".into(),
                    reason: "not now".into(),
                },
                "delegation.confirmation.rejected",
            ),
            (
                SessionEventData::DelegationFoldedUpdated {
                    tool_call_id: "tool_delegate".into(),
                    requesting_profile_id: ProfileId::from("planner"),
                    target_kind: ProfileKind::Agent,
                    target_id: ProfileId::from("coder"),
                    task: "implement parser".into(),
                    status: PersistedDelegationStatus::Completed,
                    child_operation_id: Some("op_child".into()),
                    summary: Some("child result".into()),
                },
                "delegation.folded.updated",
            ),
            (
                SessionEventData::ToolAuthorizationRequested {
                    request: test_authorization_request(),
                },
                "tool.authorization.requested",
            ),
            (
                SessionEventData::ToolAuthorizationResolved {
                    authorization_id: "auth_1".into(),
                    resolution: PersistedToolAuthorizationResolution::Approved {
                        decision: ToolAuthorizationDecision::AllowOnce,
                    },
                },
                "tool.authorization.resolved",
            ),
            (
                SessionEventData::OperationStarted {
                    operation: OperationKind::Prompt,
                    runtime_generation: PersistedRuntimeGenerationRef::default(),
                },
                "operation.started",
            ),
            (
                SessionEventData::OperationCommitted { new_leaf_id: None },
                "operation.committed",
            ),
            (
                SessionEventData::OperationAborted {
                    reason: "user".into(),
                },
                "operation.aborted",
            ),
            (
                SessionEventData::OperationFailed {
                    error_code: "session".into(),
                    message: "failed".into(),
                },
                "operation.failed",
            ),
            (
                SessionEventData::OperationTerminalRecorded {
                    status: "completed".into(),
                    semantic_event_id: "session/op/operation_terminal".into(),
                },
                "operation.terminal.recorded",
            ),
            (
                SessionEventData::OperationRecoveryPending {
                    reason: "awaiting operator resolution".into(),
                    recovery_id: "recovery_pending:sess/op".into(),
                    record_version: 1,
                    descriptor_revision: 1,
                    capability_generation: Some(7),
                    attempt_count: 0,
                    last_attempt_at: None,
                    next_attempt_at: None,
                },
                "operation.recovery_pending",
            ),
            (
                SessionEventData::OperationRecovered {
                    reason: "manual recovery".into(),
                    recovery_id: "recovery_1".into(),
                },
                "operation.recovered",
            ),
            (SessionEventData::TurnStarted {}, "turn.started"),
            (
                SessionEventData::TurnInputRecorded {
                    content: vec![PersistedContentBlock::Text {
                        text: "hello".into(),
                    }],
                },
                "turn.input.recorded",
            ),
            (
                SessionEventData::MessageStarted {
                    message_id: "msg_1".into(),
                    role: PersistedRole::Assistant,
                },
                "message.started",
            ),
            (
                SessionEventData::MessageReasoningStarted {
                    message_id: "msg_1".into(),
                    content_index: 0,
                },
                "message.reasoning.started",
            ),
            (
                SessionEventData::MessageReasoningCompleted {
                    message_id: "msg_1".into(),
                    content_index: 0,
                },
                "message.reasoning.completed",
            ),
            (
                SessionEventData::MessageCompleted {
                    message_id: "msg_1".into(),
                    content: vec![PersistedContentBlock::Text { text: "hi".into() }],
                    finish_reason: None,
                    usage: Default::default(),
                },
                "message.completed",
            ),
            (
                SessionEventData::MessageCancelled {
                    message_id: "msg_1".into(),
                    reason: "abort".into(),
                },
                "message.cancelled",
            ),
            (
                SessionEventData::ToolCallStarted {
                    tool_call_id: "tool_1".into(),
                    name: "read".into(),
                    arguments: json!({"path": "src/lib.rs"}),
                },
                "tool.call.started",
            ),
            (
                SessionEventData::ToolCallUpdated {
                    tool_call_id: "tool_1".into(),
                    message: "running".into(),
                },
                "tool.call.updated",
            ),
            (
                SessionEventData::ToolCallCompleted {
                    tool_call_id: "tool_1".into(),
                    result: PersistedToolResult::Text { text: "ok".into() },
                },
                "tool.call.completed",
            ),
            (
                SessionEventData::ToolCallFailed {
                    tool_call_id: "tool_1".into(),
                    message: "failed".into(),
                },
                "tool.call.failed",
            ),
            (
                SessionEventData::ToolCallCancelled {
                    tool_call_id: "tool_1".into(),
                    reason: "abort".into(),
                },
                "tool.call.cancelled",
            ),
            (
                SessionEventData::DiagnosticEmitted {
                    level: DiagnosticLevel::Info,
                    message: "note".into(),
                },
                "diagnostic.emitted",
            ),
            (
                SessionEventData::MetadataUpdated {
                    key: "model".into(),
                    value: json!("test"),
                },
                "metadata.updated",
            ),
            (
                SessionEventData::ActiveLeafChanged {
                    leaf_id: "leaf_1".into(),
                },
                "active_leaf.changed",
            ),
        ];

        for (event, expected_kind) in cases {
            let value = serde_json::to_value(event).unwrap();
            assert_eq!(value["kind"], expected_kind);
            assert!(value.get("data").is_some());
        }
    }

    #[test]
    fn operation_recovery_pending_event_round_trips() {
        let envelope = SessionEventEnvelope::new(
            "sess_recover",
            "evt_1",
            "2026-07-08T00:00:00Z",
            SessionEventData::OperationRecoveryPending {
                reason: "awaiting operator resolution".into(),
                recovery_id: "recovery_pending:sess_recover/op_in_doubt".into(),
                record_version: 1,
                descriptor_revision: 1,
                capability_generation: Some(7),
                attempt_count: 0,
                last_attempt_at: None,
                next_attempt_at: None,
            },
        )
        .with_operation_id("op_in_doubt");

        let json = serde_json::to_string(&envelope).unwrap();
        let decoded: SessionEventEnvelope = serde_json::from_str(&json).unwrap();

        assert_eq!(decoded, envelope);
        assert_eq!(
            serde_json::to_value(&envelope).unwrap()["kind"],
            "operation.recovery_pending"
        );

        let mut legacy = serde_json::to_value(&envelope).unwrap();
        let data = legacy["data"].as_object_mut().unwrap();
        data.remove("record_version");
        data.remove("descriptor_revision");
        data.remove("capability_generation");
        let decoded_legacy: SessionEventEnvelope = serde_json::from_value(legacy).unwrap();
        assert!(matches!(
            decoded_legacy.data,
            SessionEventData::OperationRecoveryPending {
                record_version: 1,
                descriptor_revision: 1,
                capability_generation: None,
                ..
            }
        ));
    }

    #[test]
    fn operation_recovered_event_round_trips() {
        let envelope = SessionEventEnvelope::new(
            "sess_recover",
            "evt_1",
            "2026-07-08T00:00:00Z",
            SessionEventData::OperationRecovered {
                reason: "manual recovery after crash".into(),
                recovery_id: "recovery_1".into(),
            },
        )
        .with_operation_id("op_recovered");

        let json = serde_json::to_string(&envelope).unwrap();
        let decoded: SessionEventEnvelope = serde_json::from_str(&json).unwrap();

        assert_eq!(decoded, envelope);
        assert_eq!(
            serde_json::to_value(&envelope).unwrap()["kind"],
            "operation.recovered"
        );
    }

    fn test_authorization_request() -> ToolAuthorizationRequest {
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

    fn test_delegation_runtime_seed() -> PersistedDelegationRuntimeSeed {
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
            session_name: Some("session".into()),
            parent_delegation_depth: 0,
            delegation_lineage: Vec::new(),
        }
    }
}
