use serde::{Deserialize, Serialize};

pub(crate) mod agent;
pub(crate) mod capability;
pub(crate) mod delegation;
pub(crate) mod diagnostic;
pub(crate) mod emission;
pub(crate) mod merge;
pub(crate) mod message;
pub(crate) mod outbox;
pub(crate) mod prompt;
pub(crate) mod prompt_stream;
pub(crate) mod recovery;
pub(crate) mod runtime;
pub(crate) mod session;
pub(crate) mod team;
pub(crate) mod tool;
pub(crate) mod workflow;

use crate::kernel::capability::CapabilityGeneration;

pub(crate) type ProductEvent = CodingAgentProductEvent;
pub(crate) type ProductEventTerminalStatus = CodingAgentProductEventTerminalStatus;

#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Deserialize, Serialize,
)]
#[serde(transparent)]
pub(crate) struct ProductEventSequence(pub(crate) u64);

impl ProductEventSequence {
    pub(crate) fn new(value: u64) -> Self {
        Self(value)
    }

    pub(crate) fn get(self) -> u64 {
        self.0
    }

    pub(crate) fn next(self) -> Self {
        Self(self.0 + 1)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CodingAgentProductEventFamily {
    Session,
    Agent,
    Team,
    Message,
    Tool,
    Runtime,
    Delegation,
    Merge,
    Workflow,
    Diagnostic,
    Capability,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CodingAgentProductEventDeliveryClass {
    Data,
    Terminal,
    Control,
    Recovery,
}

impl CodingAgentProductEventFamily {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Session => "session",
            Self::Agent => "agent",
            Self::Team => "team",
            Self::Message => "message",
            Self::Tool => "tool",
            Self::Runtime => "runtime",
            Self::Delegation => "delegation",
            Self::Merge => "merge",
            Self::Workflow => "workflow",
            Self::Diagnostic => "diagnostic",
            Self::Capability => "capability",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CodingAgentProductEventTerminalStatus {
    Completed,
    Failed,
    Aborted,
    Recovered,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CodingAgentRecoveryResolution {
    Failed,
    Aborted,
}

impl CodingAgentProductEventTerminalStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Aborted => "aborted",
            Self::Recovered => "recovered",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CodingAgentProductEventTerminalOperationKind {
    Prompt,
    BranchSummary,
    AgentInvocation,
    AgentTeam,
    SelfHealingEdit,
    Compact,
    Export,
}

impl CodingAgentProductEventTerminalOperationKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Prompt => "prompt",
            Self::BranchSummary => "branch_summary",
            Self::AgentInvocation => "agent_invocation",
            Self::AgentTeam => "agent_team",
            Self::SelfHealingEdit => "self_healing_edit",
            Self::Compact => "compact",
            Self::Export => "export",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
pub struct CodingAgentProductEventTerminalOperation {
    pub kind: CodingAgentProductEventTerminalOperationKind,
    pub status: CodingAgentProductEventTerminalStatus,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case", tag = "state")]
pub enum CodingAgentProductEventDurability {
    LiveOnly,
    PendingSessionWrite {
        operation_id: String,
    },
    Durable {
        session_id: String,
    },
    DerivedFromSession {
        session_id: String,
        source_operation_id: String,
        recovery_id: String,
    },
    PersistenceUncertain {
        operation_id: String,
    },
    PersistenceFailed {
        operation_id: String,
        reason: String,
    },
}

pub type CodingAgentProductEventError = crate::public_error::CodingAgentPublicError;

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct CodingAgentProductEventUsage {
    pub input: u32,
    pub output: u32,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub reasoning_tokens: u32,
    pub cache_read: u32,
    pub cache_write: u32,
    pub total_tokens: u32,
    #[serde(default = "default_true", skip_serializing_if = "is_true")]
    pub cost_known: bool,
    pub input_cost: f64,
    pub output_cost: f64,
    pub cache_read_cost: f64,
    pub cache_write_cost: f64,
}

const fn default_true() -> bool {
    true
}

const fn is_true(value: &bool) -> bool {
    *value
}

const fn is_zero(value: &u32) -> bool {
    *value == 0
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct CodingAgentProductEventReplacement {
    pub old_text: String,
    pub new_text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct CodingAgentProductEventDiagnostic {
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct CodingAgentProductEventCheckOutput {
    pub command: String,
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CodingAgentProductEventProfileKind {
    Agent,
    Team,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CodingAgentProductEventCapabilityRevocation {
    RequestCancelOlderOperations,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum CodingAgentSessionProductEvent {
    Opened {
        session_id: String,
    },
    WritePending {
        operation_id: String,
    },
    WriteCommitted {
        operation_id: String,
        session_id: String,
    },
    WriteSkipped {
        operation_id: String,
        reason: String,
    },
    WriteFailed {
        operation_id: String,
        reason: String,
        status: CodingAgentSessionWriteFailureStatus,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        failure_reason: Option<CodingAgentSessionWriteFailureReason>,
    },
    CompactionCompleted {
        operation_id: String,
        turn_id: String,
        summary: String,
        first_kept_message_id: String,
        tokens_before: u32,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CodingAgentSessionWriteFailureStatus {
    Definite,
    Uncertain,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CodingAgentSessionWriteFailureReason {
    QueueSaturated,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum CodingAgentAgentProductEvent {
    InvocationStarted {
        operation_id: String,
        child_operation_id: String,
        profile_id: String,
        task: String,
    },
    InvocationCompleted {
        operation_id: String,
        child_operation_id: String,
        profile_id: String,
        final_text: String,
    },
    InvocationFailed {
        operation_id: String,
        child_operation_id: String,
        profile_id: String,
        error: CodingAgentProductEventError,
    },
    InvocationAborted {
        operation_id: String,
        child_operation_id: String,
        profile_id: String,
        reason: String,
    },
    TurnStarted {
        operation_id: String,
        turn_id: String,
        agent_turn: u32,
    },
    ProviderRequestStarted {
        operation_id: String,
        turn_id: String,
        provider: String,
        model: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        context_window: Option<u32>,
    },
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum CodingAgentTeamProductEvent {
    Started {
        operation_id: String,
        team_id: String,
        task: String,
    },
    MemberStarted {
        operation_id: String,
        child_operation_id: String,
        team_id: String,
        profile_id: String,
        task: String,
    },
    MemberCompleted {
        operation_id: String,
        child_operation_id: String,
        team_id: String,
        profile_id: String,
        final_text: String,
    },
    Completed {
        operation_id: String,
        team_id: String,
        final_text: String,
    },
    Failed {
        operation_id: String,
        team_id: String,
        error: CodingAgentProductEventError,
    },
    Aborted {
        operation_id: String,
        team_id: String,
        reason: String,
    },
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum CodingAgentMessageProductEvent {
    Started {
        operation_id: String,
        turn_id: String,
        message_id: Option<String>,
    },
    Delta {
        operation_id: String,
        turn_id: String,
        message_id: Option<String>,
        text: String,
    },
    ThinkingDelta {
        operation_id: String,
        turn_id: String,
        message_id: Option<String>,
        text: String,
    },
    Completed {
        operation_id: String,
        turn_id: String,
        message_id: Option<String>,
        final_text: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        images: Vec<CodingAgentImageContent>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reasoning_duration_millis: Option<u64>,
        usage: CodingAgentProductEventUsage,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct CodingAgentImageContent {
    pub mime_type: String,
    pub data: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
#[allow(
    clippy::large_enum_variant,
    reason = "public serialized event variants retain their stable typed payload shape"
)]
pub enum CodingAgentToolProductEvent {
    AuthorizationRequired {
        request: crate::authorization::ToolAuthorizationRequest,
    },
    AuthorizationApproved {
        authorization_id: String,
        operation_id: String,
        tool_call_id: String,
        decision: crate::authorization::ToolAuthorizationDecision,
    },
    AuthorizationDenied {
        authorization_id: String,
        operation_id: String,
        tool_call_id: String,
        reason: String,
    },
    AuthorizationCancelled {
        authorization_id: String,
        operation_id: String,
        tool_call_id: String,
        reason: String,
    },
    Started {
        operation_id: String,
        turn_id: String,
        tool_call_id: String,
        name: String,
        arguments_json: String,
    },
    Updated {
        operation_id: String,
        turn_id: String,
        tool_call_id: String,
        name: String,
        message: String,
    },
    Completed {
        operation_id: String,
        turn_id: String,
        tool_call_id: String,
        name: String,
        summary: String,
    },
    Failed {
        operation_id: String,
        turn_id: String,
        tool_call_id: String,
        name: String,
        message: String,
    },
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum CodingAgentRuntimeProductEvent {
    CompactionCompleted {
        operation_id: String,
        turn_id: String,
        summary: String,
        first_kept_message_id: String,
        tokens_before: u32,
    },
    ShutDown,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct CodingAgentDelegationEventContext {
    pub operation_id: String,
    pub turn_id: String,
    pub tool_call_id: String,
    pub requesting_profile_id: String,
    pub target_kind: CodingAgentProductEventProfileKind,
    pub target_id: String,
    pub task: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum CodingAgentMergeProductEvent {
    ProposalCreated {
        worktree_id: String,
        child_operation_id: String,
    },
    Applied {
        worktree_id: String,
        applied: usize,
    },
    Conflicted {
        worktree_id: String,
        paths: Vec<String>,
    },
    StaleParent {
        worktree_id: String,
        expected: Option<String>,
        actual: Option<String>,
    },
    Discarded {
        worktree_id: String,
    },
    Failed {
        worktree_id: String,
        error: CodingAgentProductEventError,
    },
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum CodingAgentDelegationProductEvent {
    Requested {
        context: CodingAgentDelegationEventContext,
    },
    Rejected {
        context: CodingAgentDelegationEventContext,
        reason: String,
    },
    Approved {
        context: CodingAgentDelegationEventContext,
    },
    ConfirmationRequired {
        context: CodingAgentDelegationEventContext,
        reason: String,
    },
    Started {
        context: CodingAgentDelegationEventContext,
        child_operation_id: String,
    },
    Completed {
        context: CodingAgentDelegationEventContext,
        child_operation_id: String,
        final_text: String,
    },
    Failed {
        context: CodingAgentDelegationEventContext,
        child_operation_id: String,
        error: CodingAgentProductEventError,
    },
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum CodingAgentWorkflowProductEvent {
    SelfHealingEditStarted {
        operation_id: String,
        path: String,
        replacements: usize,
    },
    SelfHealingEditRepairAttempted {
        operation_id: String,
        path: String,
        attempt: usize,
        replacements: Vec<CodingAgentProductEventReplacement>,
        diagnostics: Vec<CodingAgentProductEventDiagnostic>,
        check_output: Option<CodingAgentProductEventCheckOutput>,
    },
    SelfHealingEditCompleted {
        operation_id: String,
        path: String,
        attempts: usize,
        first_changed_line: Option<usize>,
        check_output: Option<CodingAgentProductEventCheckOutput>,
    },
    SelfHealingEditFailed {
        operation_id: String,
        path: String,
        error: CodingAgentProductEventError,
    },
    SelfHealingEditAborted {
        operation_id: String,
        path: String,
        reason: String,
    },
    PromptStarted {
        operation_id: String,
        turn_id: String,
    },
    PromptCompleted {
        operation_id: String,
        turn_id: String,
    },
    PromptFailed {
        operation_id: String,
        error: CodingAgentProductEventError,
    },
    PromptAborted {
        operation_id: String,
        reason: String,
    },
    OperationRecoveryPending {
        operation_id: String,
        recovery_id: String,
        reason: String,
        #[serde(default = "default_recovery_record_version")]
        record_version: u64,
        #[serde(default = "default_operation_descriptor_revision")]
        descriptor_revision: u16,
        #[serde(default)]
        capability_generation: Option<u64>,
        #[serde(default)]
        attempt_count: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        last_attempt_at: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        next_attempt_at: Option<String>,
    },
    OperationRecoveryResolved {
        operation_id: String,
        recovery_id: String,
        resolution: CodingAgentRecoveryResolution,
        reason: String,
        record_version: u64,
        descriptor_revision: u16,
        capability_generation: Option<u64>,
    },
    OperationRecovered {
        operation_id: String,
        recovery_id: String,
        reason: String,
    },
}

fn default_recovery_record_version() -> u64 {
    recovery::RECOVERY_RECORD_VERSION
}

fn default_operation_descriptor_revision() -> u16 {
    crate::kernel::operation::OPERATION_DESCRIPTOR_REVISION
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum CodingAgentDiagnosticProductEvent {
    Diagnostic {
        diagnostic: crate::public_error::CodingAgentPublicDiagnostic,
    },
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum CodingAgentCapabilityProductEvent {
    Changed {
        generation: u64,
        revocation: CodingAgentProductEventCapabilityRevocation,
        cancellation_requested_operation_ids: Vec<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case", tag = "family", content = "payload")]
pub enum CodingAgentProductEventKind {
    Session(CodingAgentSessionProductEvent),
    Agent(CodingAgentAgentProductEvent),
    Team(CodingAgentTeamProductEvent),
    Message(CodingAgentMessageProductEvent),
    Tool(CodingAgentToolProductEvent),
    Runtime(CodingAgentRuntimeProductEvent),
    Delegation(CodingAgentDelegationProductEvent),
    Merge(CodingAgentMergeProductEvent),
    Workflow(CodingAgentWorkflowProductEvent),
    Diagnostic(CodingAgentDiagnosticProductEvent),
    Capability(CodingAgentCapabilityProductEvent),
}

mod model;

pub use model::CodingAgentProductEvent;
