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

#[cfg(test)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RpcDetachRequest {
    pub id: Option<String>,
}

#[cfg(test)]
impl Serialize for RpcDetachRequest {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut state = serializer.serialize_struct("RpcDetachRequest", 2)?;
        if let Some(id) = &self.id {
            state.serialize_field("id", id)?;
        }
        state.serialize_field("type", "detach")?;
        state.end()
    }
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RpcDetachStatus {
    Detached,
    AlreadyDetached,
    StaleGeneration,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
pub struct RpcDetachResponse {
    pub status: RpcDetachStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RpcDetachLifecycleEvent {
    pub status: RpcDetachStatus,
}

impl Serialize for RpcDetachLifecycleEvent {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut state = serializer.serialize_struct("RpcDetachLifecycleEvent", 2)?;
        state.serialize_field("type", "client_detached")?;
        state.serialize_field("status", &self.status)?;
        state.end()
    }
}

#[cfg(test)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RpcShutdownRequest {
    pub id: Option<String>,
}

#[cfg(test)]
impl Serialize for RpcShutdownRequest {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut state = serializer.serialize_struct("RpcShutdownRequest", 2)?;
        if let Some(id) = &self.id {
            state.serialize_field("id", id)?;
        }
        state.serialize_field("type", "shutdown")?;
        state.end()
    }
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RpcShutdownStatus {
    #[cfg(test)]
    ShutdownRequested,
    ShutDown,
    AlreadyShutDown,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
pub struct RpcShutdownResponse {
    pub status: RpcShutdownStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RpcShutdownLifecycleEvent {
    pub status: RpcShutdownStatus,
}

impl Serialize for RpcShutdownLifecycleEvent {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut state = serializer.serialize_struct("RpcShutdownLifecycleEvent", 2)?;
        state.serialize_field("type", "runtime_shut_down")?;
        state.serialize_field("status", &self.status)?;
        state.end()
    }
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(tag = "type")]
pub enum RpcCommand {
    #[serde(rename = "hello")]
    Hello {
        id: Option<String>,
        protocol: RequestedProtocolVersion,
    },
    #[serde(rename = "detach")]
    Detach { id: Option<String> },
    #[serde(rename = "shutdown")]
    Shutdown { id: Option<String> },
    #[serde(rename = "prompt")]
    Prompt {
        id: Option<String>,
        message: String,
        images: Option<Vec<CodingAgentPromptImage>>,
        #[serde(rename = "streamingBehavior")]
        streaming_behavior: Option<StreamingBehavior>,
        #[serde(
            rename = "afterSnapshotCursor",
            skip_serializing_if = "Option::is_none"
        )]
        after_snapshot_cursor: Option<CodingAgentSnapshotCursor>,
        #[serde(rename = "idempotencyKey", skip_serializing_if = "Option::is_none")]
        idempotency_key: Option<String>,
    },
    #[serde(rename = "steer")]
    Steer {
        id: Option<String>,
        message: String,
        images: Option<Vec<CodingAgentPromptImage>>,
    },
    #[serde(rename = "follow_up")]
    FollowUp {
        id: Option<String>,
        message: String,
        images: Option<Vec<CodingAgentPromptImage>>,
    },
    #[serde(rename = "abort")]
    Abort {
        id: Option<String>,
        #[serde(rename = "operationId", default)]
        operation_id: Option<String>,
    },
    #[serde(rename = "new_session")]
    NewSession {
        id: Option<String>,
        #[serde(rename = "parentSession")]
        parent_session: Option<String>,
    },
    #[serde(rename = "get_state")]
    GetState { id: Option<String> },
    #[serde(rename = "self_healing_edit")]
    SelfHealingEdit {
        id: Option<String>,
        path: String,
        edits: Vec<RpcSelfHealingEditReplacement>,
        #[serde(rename = "checkCommand")]
        check_command: Option<String>,
        #[serde(rename = "repairAttempts")]
        repair_attempts: Option<Vec<Vec<RpcSelfHealingEditReplacement>>>,
        #[serde(rename = "modelRepair")]
        model_repair: Option<RpcSelfHealingEditModelRepair>,
        #[serde(rename = "idempotencyKey", skip_serializing_if = "Option::is_none")]
        idempotency_key: Option<String>,
    },
    #[serde(rename = "list_agent_profiles")]
    ListAgentProfiles { id: Option<String> },
    #[serde(rename = "list_team_profiles")]
    ListTeamProfiles { id: Option<String> },
    #[serde(rename = "invoke_agent")]
    InvokeAgent {
        id: Option<String>,
        #[serde(rename = "profileId")]
        profile_id: String,
        task: String,
        #[serde(rename = "idempotencyKey", skip_serializing_if = "Option::is_none")]
        idempotency_key: Option<String>,
    },
    #[serde(rename = "invoke_team")]
    InvokeTeam {
        id: Option<String>,
        #[serde(rename = "teamId")]
        team_id: String,
        task: String,
        #[serde(rename = "idempotencyKey", skip_serializing_if = "Option::is_none")]
        idempotency_key: Option<String>,
    },
    #[serde(rename = "list_delegation_confirmations")]
    ListDelegationConfirmations { id: Option<String> },
    #[serde(rename = "list_tool_authorizations")]
    ListToolAuthorizations { id: Option<String> },
    #[serde(rename = "approve_tool_authorization")]
    ApproveToolAuthorization {
        id: Option<String>,
        #[serde(flatten)]
        identity: ToolAuthorizationIdentity,
        scope: RpcToolAuthorizationApprovalScope,
    },
    #[serde(rename = "deny_tool_authorization")]
    DenyToolAuthorization {
        id: Option<String>,
        #[serde(flatten)]
        identity: ToolAuthorizationIdentity,
        reason: Option<String>,
    },
    #[serde(rename = "approve_delegation")]
    ApproveDelegation {
        id: Option<String>,
        #[serde(rename = "operationId")]
        operation_id: String,
        #[serde(rename = "toolCallId")]
        tool_call_id: String,
        #[serde(rename = "idempotencyKey", skip_serializing_if = "Option::is_none")]
        idempotency_key: Option<String>,
    },
    #[serde(rename = "reject_delegation")]
    RejectDelegation {
        id: Option<String>,
        #[serde(rename = "operationId")]
        operation_id: String,
        #[serde(rename = "toolCallId")]
        tool_call_id: String,
        reason: Option<String>,
        #[serde(rename = "idempotencyKey", skip_serializing_if = "Option::is_none")]
        idempotency_key: Option<String>,
    },
    #[serde(rename = "set_thinking_level")]
    SetThinkingLevel {
        id: Option<String>,
        #[serde(deserialize_with = "deserialize_from_display")]
        level: CodingAgentThinkingLevel,
    },
    #[serde(rename = "set_steering_mode")]
    SetSteeringMode {
        id: Option<String>,
        #[serde(deserialize_with = "deserialize_from_display")]
        mode: CodingAgentQueueMode,
    },
    #[serde(rename = "set_follow_up_mode")]
    SetFollowUpMode {
        id: Option<String>,
        #[serde(deserialize_with = "deserialize_from_display")]
        mode: CodingAgentQueueMode,
    },
    #[serde(rename = "compact")]
    Compact {
        id: Option<String>,
        #[serde(rename = "customInstructions")]
        custom_instructions: Option<String>,
    },
    #[serde(rename = "set_auto_compaction")]
    SetAutoCompaction { id: Option<String>, enabled: bool },
    #[serde(rename = "get_session_stats")]
    GetSessionStats { id: Option<String> },
    #[serde(rename = "get_last_assistant_text")]
    GetLastAssistantText { id: Option<String> },
    #[serde(rename = "set_session_name")]
    SetSessionName { id: Option<String>, name: String },
    #[serde(rename = "get_messages")]
    GetMessages { id: Option<String> },
    #[serde(rename = "recovery_inspect")]
    RecoveryInspect {
        id: Option<String>,
        #[serde(rename = "authorizationToken")]
        authorization_token: String,
    },
    #[serde(rename = "recovery_retry")]
    RecoveryRetry {
        id: Option<String>,
        #[serde(rename = "authorizationToken")]
        authorization_token: String,
        #[serde(rename = "operationId")]
        operation_id: String,
        #[serde(rename = "recoveryId")]
        recovery_id: String,
        #[serde(rename = "recordVersion")]
        record_version: u64,
        #[serde(rename = "descriptorRevision")]
        descriptor_revision: u16,
        #[serde(rename = "capabilityGeneration")]
        capability_generation: Option<u64>,
        #[serde(rename = "attemptCount")]
        attempt_count: u32,
        #[serde(rename = "scheduleWithBackoff", default)]
        schedule_with_backoff: bool,
        #[serde(rename = "idempotencyKey")]
        idempotency_key: Option<String>,
    },
    #[serde(rename = "recovery_resolve")]
    RecoveryResolve {
        id: Option<String>,
        #[serde(rename = "authorizationToken")]
        authorization_token: String,
        #[serde(rename = "operationId")]
        operation_id: String,
        #[serde(rename = "recoveryId")]
        recovery_id: String,
        #[serde(rename = "recordVersion")]
        record_version: u64,
        #[serde(rename = "descriptorRevision")]
        descriptor_revision: u16,
        #[serde(rename = "capabilityGeneration")]
        capability_generation: Option<u64>,
        #[serde(rename = "attemptCount")]
        attempt_count: u32,
        resolution: CodingAgentRecoveryResolution,
        reason: String,
        #[serde(rename = "idempotencyKey")]
        idempotency_key: Option<String>,
    },
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RpcToolAuthorizationApprovalScope {
    Once,
    Operation,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
pub enum StreamingBehavior {
    #[serde(rename = "steer")]
    Steer,
    #[serde(rename = "followUp")]
    FollowUp,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct RpcHelloResponse {
    pub protocol: ProtocolFamilyVersion,
    #[serde(rename = "productEvents")]
    pub product_events: ProtocolFamilyVersion,
    #[serde(rename = "uiSnapshot")]
    pub ui_snapshot: ProtocolFamilyVersion,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct RpcNegotiatedProtocolState {
    pub rpc: Option<ProtocolFamilyVersion>,
    #[serde(rename = "productEvents")]
    pub product_events: ProtocolFamilyVersion,
    #[serde(rename = "uiSnapshot")]
    pub ui_snapshot: ProtocolFamilyVersion,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct RpcSessionState {
    pub(crate) model: Option<serde_json::Value>,
    #[serde(rename = "thinkingLevel", serialize_with = "serialize_display")]
    pub thinking_level: CodingAgentThinkingLevel,
    #[serde(rename = "isStreaming")]
    pub is_streaming: bool,
    #[serde(rename = "isCompacting")]
    pub is_compacting: bool,
    #[serde(rename = "steeringMode", serialize_with = "serialize_display")]
    pub steering_mode: CodingAgentQueueMode,
    #[serde(rename = "followUpMode", serialize_with = "serialize_display")]
    pub follow_up_mode: CodingAgentQueueMode,
    #[serde(rename = "sessionFile", skip_serializing_if = "Option::is_none")]
    pub session_file: Option<String>,
    #[serde(rename = "sessionId")]
    pub session_id: String,
    #[serde(rename = "eventStreamId", skip_serializing_if = "Option::is_none")]
    pub event_stream_id: Option<String>,
    #[serde(rename = "clientId", skip_serializing_if = "Option::is_none")]
    pub client_id: Option<String>,
    #[serde(rename = "snapshotSequence")]
    pub snapshot_sequence: u64,
    #[serde(rename = "capabilityGeneration")]
    pub capability_generation: u64,
    #[serde(rename = "snapshotVersion")]
    pub snapshot_version: ProtocolFamilyVersion,
    #[serde(rename = "negotiatedProtocol")]
    pub negotiated_protocol: RpcNegotiatedProtocolState,
    #[serde(rename = "sessionName", skip_serializing_if = "Option::is_none")]
    pub session_name: Option<String>,
    #[serde(rename = "sessionNamePersistence")]
    pub session_name_persistence: RpcSessionNamePersistence,
    #[serde(rename = "autoCompactionEnabled")]
    pub auto_compaction_enabled: bool,
    #[serde(rename = "messageCount")]
    pub message_count: usize,
    #[serde(rename = "pendingMessageCount")]
    pub pending_message_count: usize,
    #[serde(rename = "pendingToolAuthorizations")]
    pub pending_tool_authorizations: Vec<ToolAuthorizationRequest>,
    pub capabilities: RpcCapabilities,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RpcSessionNamePersistence {
    AdapterLocal,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct RpcSetSessionNameResponse {
    pub name: String,
    pub persistence: RpcSessionNamePersistence,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct RpcSessionStats {
    #[serde(rename = "sessionFile", skip_serializing_if = "Option::is_none")]
    pub session_file: Option<String>,
    #[serde(rename = "sessionId")]
    pub session_id: String,
    #[serde(rename = "activeLeafId", skip_serializing_if = "Option::is_none")]
    pub active_leaf_id: Option<String>,
    #[serde(rename = "userMessages")]
    pub user_messages: usize,
    #[serde(rename = "assistantMessages")]
    pub assistant_messages: usize,
    #[serde(rename = "toolCalls")]
    pub tool_calls: usize,
    #[serde(rename = "toolResults")]
    pub tool_results: usize,
    #[serde(rename = "totalMessages")]
    pub total_messages: usize,
    pub tokens: RpcSessionTokenStats,
    pub cost: f64,
    #[serde(rename = "costKnown")]
    pub cost_known: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct RpcSessionTokenStats {
    pub input: u64,
    pub output: u64,
    #[serde(rename = "cacheRead")]
    pub cache_read: u64,
    #[serde(rename = "cacheWrite")]
    pub cache_write: u64,
    pub total: u64,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct RpcCapabilities {
    pub prompt: RpcCapabilityStatus,
    pub abort: RpcCapabilityStatus,
    pub steer: RpcCapabilityStatus,
    #[serde(rename = "followUp")]
    pub follow_up: RpcCapabilityStatus,
    pub compact: RpcCapabilityStatus,
    pub fork: RpcCapabilityStatus,
    #[serde(rename = "cloneSession")]
    pub clone_session: RpcCapabilityStatus,
    #[serde(rename = "branchSummary")]
    pub branch_summary: RpcCapabilityStatus,
    #[serde(rename = "switchSession")]
    pub switch_session: RpcCapabilityStatus,
    pub export: RpcCapabilityStatus,
    #[serde(rename = "selfHealingEdit")]
    pub self_healing_edit: RpcCapabilityStatus,
    #[serde(rename = "agentProfiles")]
    pub agent_profiles: RpcCapabilityStatus,
    #[serde(rename = "teamProfiles")]
    pub team_profiles: RpcCapabilityStatus,
    pub delegation: RpcDelegationCapabilityStatus,
    pub tools: RpcCapabilityStatus,
    pub shell: RpcCapabilityStatus,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum RpcCapabilityStatus {
    Available,
    Disabled { reason: String },
    Unsupported { reason: String },
    Busy { operation: String },
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct RpcDelegationCapabilityStatus {
    #[serde(flatten)]
    pub status: RpcCapabilityStatus,
    pub rendering: RpcDelegationRenderingMetadata,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct RpcDelegationRenderingMetadata {
    pub mode: &'static str,
    #[serde(rename = "eventFamily")]
    pub event_family: &'static str,
    #[serde(rename = "payloadField")]
    pub payload_field: &'static str,
    #[serde(rename = "upsertKey")]
    pub upsert_key: &'static str,
    #[serde(rename = "lifecycleEvents")]
    pub lifecycle_events: Vec<&'static str>,
}

impl RpcDelegationRenderingMetadata {
    fn folded_block() -> Self {
        Self {
            mode: "folded_block",
            event_family: "delegation",
            payload_field: "foldedBlock",
            upsert_key: "toolCallId",
            lifecycle_events: vec![
                "delegation_requested",
                "delegation_rejected",
                "delegation_approved",
                "delegation_confirmation_required",
                "delegation_started",
                "delegation_completed",
                "delegation_failed",
            ],
        }
    }
}

impl From<CapabilityStatus> for RpcDelegationCapabilityStatus {
    fn from(status: CapabilityStatus) -> Self {
        Self {
            status: status.into(),
            rendering: RpcDelegationRenderingMetadata::folded_block(),
        }
    }
}

impl From<CodingAgentCapabilities> for RpcCapabilities {
    fn from(capabilities: CodingAgentCapabilities) -> Self {
        Self {
            prompt: capabilities.prompt.into(),
            abort: capabilities.abort.into(),
            steer: capabilities.steer.into(),
            follow_up: capabilities.follow_up.into(),
            compact: capabilities.compact.into(),
            fork: rpc_unsupported("RPC protocol 3.0 does not expose a fork command"),
            clone_session: rpc_unsupported(
                "RPC protocol 3.0 does not expose a cloneSession command",
            ),
            branch_summary: rpc_unsupported(
                "RPC protocol 3.0 does not expose a branchSummary command",
            ),
            switch_session: rpc_unsupported(
                "RPC protocol 3.0 does not expose a switchSession command",
            ),
            export: rpc_unsupported("RPC protocol 3.0 does not expose an export command"),
            self_healing_edit: capabilities.self_healing_edit.into(),
            agent_profiles: capabilities.agent_profiles.into(),
            team_profiles: capabilities.team_profiles.into(),
            delegation: capabilities.delegation.into(),
            tools: capabilities.tools.into(),
            shell: capabilities.shell.into(),
        }
    }
}

fn rpc_unsupported(reason: &'static str) -> RpcCapabilityStatus {
    RpcCapabilityStatus::Unsupported {
        reason: reason.to_owned(),
    }
}

impl From<CapabilityStatus> for RpcCapabilityStatus {
    fn from(status: CapabilityStatus) -> Self {
        match status {
            CapabilityStatus::Available => Self::Available,
            CapabilityStatus::Disabled { reason } => Self::Disabled { reason },
            CapabilityStatus::Unsupported { reason } => Self::Unsupported { reason },
            CapabilityStatus::Busy { operation } => Self::Busy { operation },
        }
    }
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct RpcResponse {
    #[serde(rename = "type")]
    pub response_type: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub command: String,
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl RpcResponse {
    pub fn success(
        id: Option<String>,
        command: impl Into<String>,
        data: Option<serde_json::Value>,
    ) -> Self {
        Self {
            response_type: "response",
            id,
            command: command.into(),
            success: true,
            data,
            error: None,
        }
    }

    pub fn error(id: Option<String>, command: impl Into<String>, error: impl Into<String>) -> Self {
        Self {
            response_type: "response",
            id,
            command: command.into(),
            success: false,
            data: None,
            error: Some(error.into()),
        }
    }

    pub fn error_with_data(
        id: Option<String>,
        command: impl Into<String>,
        error: impl Into<String>,
        data: serde_json::Value,
    ) -> Self {
        Self {
            response_type: "response",
            id,
            command: command.into(),
            success: false,
            data: Some(data),
            error: Some(error.into()),
        }
    }
}

fn serialize_display<T, S>(value: &T, serializer: S) -> Result<S::Ok, S::Error>
where
    T: Display,
    S: Serializer,
{
    serializer.serialize_str(&value.to_string())
}

fn deserialize_from_display<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: FromStr,
    T::Err: Display,
{
    let value = String::deserialize(deserializer)?;
    value.parse().map_err(serde::de::Error::custom)
}
