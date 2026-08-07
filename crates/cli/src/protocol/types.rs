use crate::protocol::version::{ProtocolFamilyVersion, RequestedProtocolVersion};
use coding_agent::api::authorization::{
    ToolAuthorizationDecision, ToolAuthorizationIdentity, ToolAuthorizationRequest,
};
use coding_agent::api::client::CodingAgentSnapshotCursor;
use coding_agent::api::embedding::{CodingAgentPromptImage, CodingAgentThinkingLevel};
use coding_agent::api::event::CodingAgentRecoveryResolution;
use coding_agent::api::settings::CodingAgentQueueMode;
use coding_agent::api::view::{CapabilityStatus, CodingAgentCapabilities};
use serde::ser::SerializeStruct;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt::Display;
use std::str::FromStr;

mod rpc;
pub use rpc::*;

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(transparent)]
pub struct ProtocolEvent(ProtocolEventPayload);

impl ProtocolEvent {
    pub(crate) fn from_payload(payload: ProtocolEventPayload) -> Self {
        Self(payload)
    }

    pub fn agent_start() -> Self {
        Self(ProtocolEventPayload::AgentStart)
    }

    pub(crate) fn queue_update(steering: Vec<String>, follow_up: Vec<String>) -> Self {
        Self(ProtocolEventPayload::QueueUpdate {
            steering,
            follow_up,
        })
    }
}

impl From<ProtocolEventPayload> for ProtocolEvent {
    fn from(payload: ProtocolEventPayload) -> Self {
        Self::from_payload(payload)
    }
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(tag = "type")]
pub(crate) enum ProtocolEventPayload {
    #[serde(rename = "agent_start")]
    AgentStart,
    #[serde(rename = "turn_start")]
    TurnStart,
    #[serde(rename = "message_start")]
    MessageStart { message: serde_json::Value },
    #[serde(rename = "message_update")]
    MessageUpdate {
        message: serde_json::Value,
        #[serde(rename = "assistantMessageEvent")]
        assistant_message_event: serde_json::Value,
    },
    #[serde(rename = "message_end")]
    MessageEnd { message: serde_json::Value },
    #[serde(rename = "tool_execution_start")]
    ToolExecutionStart {
        #[serde(rename = "toolCallId")]
        tool_call_id: String,
        #[serde(rename = "toolName")]
        tool_name: String,
        args: serde_json::Value,
    },
    #[serde(rename = "tool_execution_end")]
    ToolExecutionEnd {
        #[serde(rename = "toolCallId")]
        tool_call_id: String,
        #[serde(rename = "toolName")]
        tool_name: String,
        result: ToolExecutionResult,
        #[serde(rename = "isError")]
        is_error: bool,
    },
    #[serde(rename = "tool_execution_update")]
    ToolExecutionUpdate {
        #[serde(rename = "toolCallId")]
        tool_call_id: String,
        #[serde(rename = "toolName")]
        tool_name: String,
        result: ToolExecutionResult,
    },
    #[serde(rename = "tool_authorization_required")]
    ToolAuthorizationRequired { request: ToolAuthorizationRequest },
    #[serde(rename = "tool_authorization_approved")]
    ToolAuthorizationApproved {
        #[serde(rename = "authorizationId")]
        authorization_id: String,
        #[serde(rename = "operationId")]
        operation_id: String,
        #[serde(rename = "toolCallId")]
        tool_call_id: String,
        decision: ToolAuthorizationDecision,
    },
    #[serde(rename = "tool_authorization_denied")]
    ToolAuthorizationDenied {
        #[serde(rename = "authorizationId")]
        authorization_id: String,
        #[serde(rename = "operationId")]
        operation_id: String,
        #[serde(rename = "toolCallId")]
        tool_call_id: String,
        reason: String,
    },
    #[serde(rename = "tool_authorization_cancelled")]
    ToolAuthorizationCancelled {
        #[serde(rename = "authorizationId")]
        authorization_id: String,
        #[serde(rename = "operationId")]
        operation_id: String,
        #[serde(rename = "toolCallId")]
        tool_call_id: String,
        reason: String,
    },
    #[serde(rename = "turn_end")]
    TurnEnd {
        message: serde_json::Value,
        #[serde(rename = "toolResults")]
        tool_results: Vec<serde_json::Value>,
    },
    #[serde(rename = "queue_update")]
    QueueUpdate {
        steering: Vec<String>,
        #[serde(rename = "followUp")]
        follow_up: Vec<String>,
    },
    #[serde(rename = "session_write_failed")]
    SessionWriteFailed {
        #[serde(rename = "operationId")]
        operation_id: String,
        status: String,
        reason: String,
    },
    #[serde(rename = "compaction_start")]
    CompactionStart { reason: CompactionReason },
    #[serde(rename = "compaction_end")]
    CompactionEnd {
        reason: CompactionReason,
        result: Option<CompactionProtocolResult>,
        aborted: bool,
        #[serde(rename = "willRetry")]
        will_retry: bool,
        #[serde(rename = "errorMessage", skip_serializing_if = "Option::is_none")]
        error_message: Option<String>,
    },
    #[serde(rename = "agent_end")]
    AgentEnd { messages: Vec<serde_json::Value> },
    #[serde(rename = "self_healing_edit_start")]
    SelfHealingEditStart {
        #[serde(rename = "operationId")]
        operation_id: String,
        path: String,
        replacements: usize,
    },
    #[serde(rename = "self_healing_edit_repair_attempt")]
    SelfHealingEditRepairAttempt {
        #[serde(rename = "operationId")]
        operation_id: String,
        path: String,
        attempt: usize,
        edits: Vec<ProtocolSelfHealingEditReplacement>,
        diagnostics: Vec<String>,
        #[serde(rename = "checkOutput", skip_serializing_if = "Option::is_none")]
        check_output: Option<ProtocolSelfHealingEditCheckOutput>,
    },
    #[serde(rename = "self_healing_edit_end")]
    SelfHealingEditEnd {
        #[serde(rename = "operationId")]
        operation_id: String,
        path: String,
        attempts: usize,
        #[serde(rename = "firstChangedLine", skip_serializing_if = "Option::is_none")]
        first_changed_line: Option<usize>,
        #[serde(rename = "checkOutput", skip_serializing_if = "Option::is_none")]
        check_output: Option<ProtocolSelfHealingEditCheckOutput>,
    },
    #[serde(rename = "self_healing_edit_error")]
    SelfHealingEditError {
        #[serde(rename = "operationId")]
        operation_id: String,
        path: String,
        error: String,
    },
    #[serde(rename = "self_healing_edit_abort")]
    SelfHealingEditAbort {
        #[serde(rename = "operationId")]
        operation_id: String,
        path: String,
        reason: String,
    },
    #[serde(rename = "delegation_requested")]
    DelegationRequested {
        #[serde(rename = "operationId")]
        operation_id: String,
        #[serde(rename = "turnId")]
        turn_id: String,
        #[serde(rename = "toolCallId")]
        tool_call_id: String,
        #[serde(rename = "requestingProfileId")]
        requesting_profile_id: String,
        #[serde(rename = "targetKind")]
        target_kind: String,
        #[serde(rename = "targetId")]
        target_id: String,
        task: String,
        #[serde(rename = "foldedBlock")]
        folded_block: ProtocolDelegationFoldedBlock,
    },
    #[serde(rename = "delegation_rejected")]
    DelegationRejected {
        #[serde(rename = "operationId")]
        operation_id: String,
        #[serde(rename = "turnId")]
        turn_id: String,
        #[serde(rename = "toolCallId")]
        tool_call_id: String,
        #[serde(rename = "requestingProfileId")]
        requesting_profile_id: String,
        #[serde(rename = "targetKind")]
        target_kind: String,
        #[serde(rename = "targetId")]
        target_id: String,
        task: String,
        reason: String,
        #[serde(rename = "foldedBlock")]
        folded_block: ProtocolDelegationFoldedBlock,
    },
    #[serde(rename = "delegation_approved")]
    DelegationApproved {
        #[serde(rename = "operationId")]
        operation_id: String,
        #[serde(rename = "turnId")]
        turn_id: String,
        #[serde(rename = "toolCallId")]
        tool_call_id: String,
        #[serde(rename = "requestingProfileId")]
        requesting_profile_id: String,
        #[serde(rename = "targetKind")]
        target_kind: String,
        #[serde(rename = "targetId")]
        target_id: String,
        task: String,
        #[serde(rename = "foldedBlock")]
        folded_block: ProtocolDelegationFoldedBlock,
    },
    #[serde(rename = "delegation_confirmation_required")]
    DelegationConfirmationRequired {
        #[serde(rename = "operationId")]
        operation_id: String,
        #[serde(rename = "turnId")]
        turn_id: String,
        #[serde(rename = "toolCallId")]
        tool_call_id: String,
        #[serde(rename = "requestingProfileId")]
        requesting_profile_id: String,
        #[serde(rename = "targetKind")]
        target_kind: String,
        #[serde(rename = "targetId")]
        target_id: String,
        task: String,
        reason: String,
        #[serde(rename = "foldedBlock")]
        folded_block: ProtocolDelegationFoldedBlock,
    },
    #[serde(rename = "delegation_started")]
    DelegationStarted {
        #[serde(rename = "operationId")]
        operation_id: String,
        #[serde(rename = "turnId")]
        turn_id: String,
        #[serde(rename = "toolCallId")]
        tool_call_id: String,
        #[serde(rename = "requestingProfileId")]
        requesting_profile_id: String,
        #[serde(rename = "targetKind")]
        target_kind: String,
        #[serde(rename = "targetId")]
        target_id: String,
        task: String,
        #[serde(rename = "childOperationId")]
        child_operation_id: String,
        #[serde(rename = "foldedBlock")]
        folded_block: ProtocolDelegationFoldedBlock,
    },
    #[serde(rename = "delegation_completed")]
    DelegationCompleted {
        #[serde(rename = "operationId")]
        operation_id: String,
        #[serde(rename = "turnId")]
        turn_id: String,
        #[serde(rename = "toolCallId")]
        tool_call_id: String,
        #[serde(rename = "requestingProfileId")]
        requesting_profile_id: String,
        #[serde(rename = "targetKind")]
        target_kind: String,
        #[serde(rename = "targetId")]
        target_id: String,
        task: String,
        #[serde(rename = "childOperationId")]
        child_operation_id: String,
        #[serde(rename = "finalText")]
        final_text: String,
        #[serde(rename = "foldedBlock")]
        folded_block: ProtocolDelegationFoldedBlock,
    },
    #[serde(rename = "delegation_failed")]
    DelegationFailed {
        #[serde(rename = "operationId")]
        operation_id: String,
        #[serde(rename = "turnId")]
        turn_id: String,
        #[serde(rename = "toolCallId")]
        tool_call_id: String,
        #[serde(rename = "requestingProfileId")]
        requesting_profile_id: String,
        #[serde(rename = "targetKind")]
        target_kind: String,
        #[serde(rename = "targetId")]
        target_id: String,
        task: String,
        #[serde(rename = "childOperationId")]
        child_operation_id: String,
        error: String,
        #[serde(rename = "foldedBlock")]
        folded_block: ProtocolDelegationFoldedBlock,
    },
    #[serde(rename = "agent_invocation_start")]
    AgentInvocationStart {
        #[serde(rename = "operationId")]
        operation_id: String,
        #[serde(rename = "childOperationId")]
        child_operation_id: String,
        #[serde(rename = "profileId")]
        profile_id: String,
        task: String,
    },
    #[serde(rename = "agent_invocation_end")]
    AgentInvocationEnd {
        #[serde(rename = "operationId")]
        operation_id: String,
        #[serde(rename = "childOperationId")]
        child_operation_id: String,
        #[serde(rename = "profileId")]
        profile_id: String,
        #[serde(rename = "finalText")]
        final_text: String,
    },
    #[serde(rename = "agent_invocation_error")]
    AgentInvocationError {
        #[serde(rename = "operationId")]
        operation_id: String,
        #[serde(rename = "childOperationId")]
        child_operation_id: String,
        #[serde(rename = "profileId")]
        profile_id: String,
        error: String,
    },
    #[serde(rename = "agent_invocation_abort")]
    AgentInvocationAbort {
        #[serde(rename = "operationId")]
        operation_id: String,
        #[serde(rename = "childOperationId")]
        child_operation_id: String,
        #[serde(rename = "profileId")]
        profile_id: String,
        reason: String,
    },
    #[serde(rename = "agent_team_start")]
    AgentTeamStart {
        #[serde(rename = "operationId")]
        operation_id: String,
        #[serde(rename = "teamId")]
        team_id: String,
        task: String,
    },
    #[serde(rename = "agent_team_member_start")]
    AgentTeamMemberStart {
        #[serde(rename = "operationId")]
        operation_id: String,
        #[serde(rename = "childOperationId")]
        child_operation_id: String,
        #[serde(rename = "teamId")]
        team_id: String,
        #[serde(rename = "profileId")]
        profile_id: String,
        task: String,
    },
    #[serde(rename = "agent_team_member_end")]
    AgentTeamMemberEnd {
        #[serde(rename = "operationId")]
        operation_id: String,
        #[serde(rename = "childOperationId")]
        child_operation_id: String,
        #[serde(rename = "teamId")]
        team_id: String,
        #[serde(rename = "profileId")]
        profile_id: String,
        #[serde(rename = "finalText")]
        final_text: String,
    },
    #[serde(rename = "agent_team_end")]
    AgentTeamEnd {
        #[serde(rename = "operationId")]
        operation_id: String,
        #[serde(rename = "teamId")]
        team_id: String,
        #[serde(rename = "finalText")]
        final_text: String,
    },
    #[serde(rename = "agent_team_error")]
    AgentTeamError {
        #[serde(rename = "operationId")]
        operation_id: String,
        #[serde(rename = "teamId")]
        team_id: String,
        error: String,
    },
    #[serde(rename = "agent_team_abort")]
    AgentTeamAbort {
        #[serde(rename = "operationId")]
        operation_id: String,
        #[serde(rename = "teamId")]
        team_id: String,
        reason: String,
    },
    #[serde(rename = "capability_changed")]
    CapabilityChanged { generation: u64, revocation: String },
    #[serde(rename = "operation_recovery_pending")]
    OperationRecoveryPending {
        #[serde(rename = "operationId")]
        operation_id: String,
        #[serde(rename = "recoveryId")]
        recovery_id: String,
        reason: String,
        #[serde(rename = "recordVersion")]
        record_version: u64,
        #[serde(rename = "descriptorRevision")]
        descriptor_revision: u16,
        #[serde(rename = "capabilityGeneration")]
        capability_generation: Option<u64>,
        #[serde(rename = "attemptCount")]
        attempt_count: u32,
        #[serde(rename = "lastAttemptAt")]
        last_attempt_at: Option<String>,
        #[serde(rename = "nextAttemptAt")]
        next_attempt_at: Option<String>,
    },
    #[serde(rename = "operation_recovery_resolved")]
    OperationRecoveryResolved {
        #[serde(rename = "operationId")]
        operation_id: String,
        #[serde(rename = "recoveryId")]
        recovery_id: String,
        resolution: String,
        reason: String,
        #[serde(rename = "recordVersion")]
        record_version: u64,
        #[serde(rename = "descriptorRevision")]
        descriptor_revision: u16,
        #[serde(rename = "capabilityGeneration")]
        capability_generation: Option<u64>,
    },
    #[serde(rename = "operation_recovered")]
    OperationRecovered {
        #[serde(rename = "operationId")]
        operation_id: String,
        #[serde(rename = "recoveryId")]
        recovery_id: String,
        reason: String,
    },
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ToolExecutionResult {
    pub(crate) content: Vec<serde_json::Value>,
    pub(crate) terminate: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) details: Option<serde_json::Value>,
}

impl ToolExecutionResult {
    pub(crate) fn new(
        content: Vec<serde_json::Value>,
        terminate: bool,
        details: Option<serde_json::Value>,
    ) -> Self {
        Self {
            content,
            terminate,
            details,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum CompactionReason {
    Manual,
    Threshold,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct CompactionProtocolResult {
    pub summary: String,
    #[serde(rename = "firstKeptMessageId")]
    pub first_kept_message_id: String,
    #[serde(rename = "tokensBefore")]
    pub tokens_before: u32,
    pub details: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ProtocolSelfHealingEditReplacement {
    #[serde(rename = "oldText")]
    pub old_text: String,
    #[serde(rename = "newText")]
    pub new_text: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ProtocolSelfHealingEditCheckOutput {
    pub command: String,
    pub stdout: String,
    pub stderr: String,
    #[serde(rename = "exitCode")]
    pub exit_code: i32,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ProtocolDelegationFoldedBlock {
    #[serde(rename = "toolCallId")]
    pub tool_call_id: String,
    #[serde(rename = "targetKind")]
    pub target_kind: String,
    #[serde(rename = "targetId")]
    pub target_id: String,
    pub task: String,
    pub status: String,
    #[serde(rename = "childOperationId", skip_serializing_if = "Option::is_none")]
    pub child_operation_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(rename = "isError")]
    pub is_error: bool,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct RpcSelfHealingEditReplacement {
    #[serde(rename = "oldText")]
    pub old_text: String,
    #[serde(rename = "newText")]
    pub new_text: String,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct RpcSelfHealingEditModelRepair {
    #[serde(rename = "maxAttempts")]
    pub max_attempts: Option<usize>,
}
