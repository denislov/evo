use serde::{Deserialize, Serialize};

pub(crate) mod agent;
pub(crate) mod capability;
pub(crate) mod delegation;
pub(crate) mod diagnostic;
pub(crate) mod emission;
pub(crate) mod message;
pub(crate) mod outbox;
pub(crate) mod profile;
pub(crate) mod prompt;
pub(crate) mod prompt_stream;
pub(crate) mod recovery;
pub(crate) mod runtime;
pub(crate) mod session;
pub(crate) mod team;
pub(crate) mod tool;
pub(crate) mod workflow;

use crate::runtime::capability::CapabilityGeneration;

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
    Profile,
    Agent,
    Team,
    Message,
    Tool,
    Runtime,
    Delegation,
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
            Self::Profile => "profile",
            Self::Agent => "agent",
            Self::Team => "team",
            Self::Message => "message",
            Self::Tool => "tool",
            Self::Runtime => "runtime",
            Self::Delegation => "delegation",
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

pub type CodingAgentProductEventError = crate::runtime::public_error::CodingAgentPublicError;

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct CodingAgentProductEventUsage {
    pub input: u32,
    pub output: u32,
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

impl From<ai::api::conversation::Usage> for CodingAgentProductEventUsage {
    fn from(usage: ai::api::conversation::Usage) -> Self {
        Self {
            input: usage.input,
            output: usage.output,
            cache_read: usage.cache_read,
            cache_write: usage.cache_write,
            total_tokens: usage.total_tokens,
            cost_known: usage.cost.known,
            input_cost: usage.cost.input,
            output_cost: usage.cost.output,
            cache_read_cost: usage.cost.cache_read,
            cache_write_cost: usage.cost.cache_write,
        }
    }
}

impl From<crate::operations::self_healing_edit::runner::SelfHealingEditReplacement>
    for CodingAgentProductEventReplacement
{
    fn from(
        replacement: crate::operations::self_healing_edit::runner::SelfHealingEditReplacement,
    ) -> Self {
        Self {
            old_text: replacement.old_text,
            new_text: replacement.new_text,
        }
    }
}

impl From<crate::operations::self_healing_edit::runner::SelfHealingEditDiagnostic>
    for CodingAgentProductEventDiagnostic
{
    fn from(
        diagnostic: crate::operations::self_healing_edit::runner::SelfHealingEditDiagnostic,
    ) -> Self {
        Self {
            message: diagnostic.message,
        }
    }
}

impl From<crate::operations::self_healing_edit::runner::SelfHealingEditCheckOutput>
    for CodingAgentProductEventCheckOutput
{
    fn from(
        output: crate::operations::self_healing_edit::runner::SelfHealingEditCheckOutput,
    ) -> Self {
        Self {
            command: output.command,
            stdout: output.stdout,
            stderr: output.stderr,
            exit_code: output.exit_code,
        }
    }
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
    FutureOnly,
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

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum CodingAgentProfileProductEvent {
    DefaultChanged { profile_id: String },
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
    crate::runtime::operation::contract::OPERATION_DESCRIPTOR_REVISION
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum CodingAgentDiagnosticProductEvent {
    Diagnostic {
        diagnostic: crate::runtime::public_error::CodingAgentPublicDiagnostic,
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
    Profile(CodingAgentProfileProductEvent),
    Agent(CodingAgentAgentProductEvent),
    Team(CodingAgentTeamProductEvent),
    Message(CodingAgentMessageProductEvent),
    Tool(CodingAgentToolProductEvent),
    Runtime(CodingAgentRuntimeProductEvent),
    Delegation(CodingAgentDelegationProductEvent),
    Workflow(CodingAgentWorkflowProductEvent),
    Diagnostic(CodingAgentDiagnosticProductEvent),
    Capability(CodingAgentCapabilityProductEvent),
}

impl CodingAgentProductEventKind {
    pub const fn family(&self) -> CodingAgentProductEventFamily {
        match self {
            Self::Session(_) => CodingAgentProductEventFamily::Session,
            Self::Profile(_) => CodingAgentProductEventFamily::Profile,
            Self::Agent(_) => CodingAgentProductEventFamily::Agent,
            Self::Team(_) => CodingAgentProductEventFamily::Team,
            Self::Message(_) => CodingAgentProductEventFamily::Message,
            Self::Tool(_) => CodingAgentProductEventFamily::Tool,
            Self::Runtime(_) => CodingAgentProductEventFamily::Runtime,
            Self::Delegation(_) => CodingAgentProductEventFamily::Delegation,
            Self::Workflow(_) => CodingAgentProductEventFamily::Workflow,
            Self::Diagnostic(_) => CodingAgentProductEventFamily::Diagnostic,
            Self::Capability(_) => CodingAgentProductEventFamily::Capability,
        }
    }

    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Session(CodingAgentSessionProductEvent::Opened { .. }) => "opened",
            Self::Session(CodingAgentSessionProductEvent::WritePending { .. }) => "write_pending",
            Self::Session(CodingAgentSessionProductEvent::WriteCommitted { .. }) => {
                "write_committed"
            }
            Self::Session(CodingAgentSessionProductEvent::WriteSkipped { .. }) => "write_skipped",
            Self::Session(CodingAgentSessionProductEvent::WriteFailed { .. }) => "write_failed",
            Self::Session(CodingAgentSessionProductEvent::CompactionCompleted { .. }) => {
                "compaction_completed"
            }
            Self::Profile(CodingAgentProfileProductEvent::DefaultChanged { .. }) => {
                "default_changed"
            }
            Self::Agent(CodingAgentAgentProductEvent::InvocationStarted { .. }) => {
                "invocation_started"
            }
            Self::Agent(CodingAgentAgentProductEvent::InvocationCompleted { .. }) => {
                "invocation_completed"
            }
            Self::Agent(CodingAgentAgentProductEvent::InvocationFailed { .. }) => {
                "invocation_failed"
            }
            Self::Agent(CodingAgentAgentProductEvent::InvocationAborted { .. }) => {
                "invocation_aborted"
            }
            Self::Agent(CodingAgentAgentProductEvent::TurnStarted { .. }) => "turn_started",
            Self::Agent(CodingAgentAgentProductEvent::ProviderRequestStarted { .. }) => {
                "provider_request_started"
            }
            Self::Team(CodingAgentTeamProductEvent::Started { .. }) => "started",
            Self::Team(CodingAgentTeamProductEvent::MemberStarted { .. }) => "member_started",
            Self::Team(CodingAgentTeamProductEvent::MemberCompleted { .. }) => "member_completed",
            Self::Team(CodingAgentTeamProductEvent::Completed { .. }) => "completed",
            Self::Team(CodingAgentTeamProductEvent::Failed { .. }) => "failed",
            Self::Team(CodingAgentTeamProductEvent::Aborted { .. }) => "aborted",
            Self::Message(CodingAgentMessageProductEvent::Started { .. }) => "started",
            Self::Message(CodingAgentMessageProductEvent::Delta { .. }) => "delta",
            Self::Message(CodingAgentMessageProductEvent::ThinkingDelta { .. }) => "thinking_delta",
            Self::Message(CodingAgentMessageProductEvent::Completed { .. }) => "completed",
            Self::Tool(CodingAgentToolProductEvent::AuthorizationRequired { .. }) => {
                "authorization_required"
            }
            Self::Tool(CodingAgentToolProductEvent::AuthorizationApproved { .. }) => {
                "authorization_approved"
            }
            Self::Tool(CodingAgentToolProductEvent::AuthorizationDenied { .. }) => {
                "authorization_denied"
            }
            Self::Tool(CodingAgentToolProductEvent::AuthorizationCancelled { .. }) => {
                "authorization_cancelled"
            }
            Self::Tool(CodingAgentToolProductEvent::Started { .. }) => "started",
            Self::Tool(CodingAgentToolProductEvent::Updated { .. }) => "updated",
            Self::Tool(CodingAgentToolProductEvent::Completed { .. }) => "completed",
            Self::Tool(CodingAgentToolProductEvent::Failed { .. }) => "failed",
            Self::Runtime(CodingAgentRuntimeProductEvent::CompactionCompleted { .. }) => {
                "compaction_completed"
            }
            Self::Runtime(CodingAgentRuntimeProductEvent::ShutDown) => "shut_down",
            Self::Delegation(CodingAgentDelegationProductEvent::Requested { .. }) => "requested",
            Self::Delegation(CodingAgentDelegationProductEvent::Rejected { .. }) => "rejected",
            Self::Delegation(CodingAgentDelegationProductEvent::Approved { .. }) => "approved",
            Self::Delegation(CodingAgentDelegationProductEvent::ConfirmationRequired {
                ..
            }) => "confirmation_required",
            Self::Delegation(CodingAgentDelegationProductEvent::Started { .. }) => "started",
            Self::Delegation(CodingAgentDelegationProductEvent::Completed { .. }) => "completed",
            Self::Delegation(CodingAgentDelegationProductEvent::Failed { .. }) => "failed",
            Self::Workflow(CodingAgentWorkflowProductEvent::SelfHealingEditStarted { .. }) => {
                "self_healing_edit_started"
            }
            Self::Workflow(CodingAgentWorkflowProductEvent::SelfHealingEditRepairAttempted {
                ..
            }) => "self_healing_edit_repair_attempted",
            Self::Workflow(CodingAgentWorkflowProductEvent::SelfHealingEditCompleted {
                ..
            }) => "self_healing_edit_completed",
            Self::Workflow(CodingAgentWorkflowProductEvent::SelfHealingEditFailed { .. }) => {
                "self_healing_edit_failed"
            }
            Self::Workflow(CodingAgentWorkflowProductEvent::SelfHealingEditAborted { .. }) => {
                "self_healing_edit_aborted"
            }
            Self::Workflow(CodingAgentWorkflowProductEvent::PromptStarted { .. }) => {
                "prompt_started"
            }
            Self::Workflow(CodingAgentWorkflowProductEvent::PromptCompleted { .. }) => {
                "prompt_completed"
            }
            Self::Workflow(CodingAgentWorkflowProductEvent::PromptFailed { .. }) => "prompt_failed",
            Self::Workflow(CodingAgentWorkflowProductEvent::PromptAborted { .. }) => {
                "prompt_aborted"
            }
            Self::Workflow(CodingAgentWorkflowProductEvent::OperationRecoveryPending {
                ..
            }) => "operation_recovery_pending",
            Self::Workflow(CodingAgentWorkflowProductEvent::OperationRecoveryResolved {
                ..
            }) => "operation_recovery_resolved",
            Self::Workflow(CodingAgentWorkflowProductEvent::OperationRecovered { .. }) => {
                "operation_recovered"
            }
            Self::Diagnostic(CodingAgentDiagnosticProductEvent::Diagnostic { .. }) => "diagnostic",
            Self::Capability(CodingAgentCapabilityProductEvent::Changed { .. }) => "changed",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct CodingAgentProductEvent {
    stream_id: String,
    sequence: ProductEventSequence,
    event: CodingAgentProductEventKind,
    operation_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    parent_operation_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    root_operation_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    capability_generation: Option<u64>,
    terminal_status: Option<CodingAgentProductEventTerminalStatus>,
    terminal_operation: Option<CodingAgentProductEventTerminalOperation>,
    durability: CodingAgentProductEventDurability,
    delivery_class: CodingAgentProductEventDeliveryClass,
}

impl CodingAgentProductEvent {
    #[allow(
        clippy::too_many_arguments,
        reason = "event envelope construction keeps ordering, association, and durability explicit"
    )]
    pub(crate) fn new(
        stream_id: String,
        sequence: ProductEventSequence,
        event: CodingAgentProductEventKind,
        operation_id: Option<String>,
        parent_operation_id: Option<String>,
        root_operation_id: Option<String>,
        session_id: Option<String>,
        capability_generation: Option<CapabilityGeneration>,
        terminal_status: Option<CodingAgentProductEventTerminalStatus>,
        terminal_operation: Option<CodingAgentProductEventTerminalOperation>,
        durability: CodingAgentProductEventDurability,
    ) -> Self {
        let delivery_class = match &event {
            CodingAgentProductEventKind::Workflow(
                CodingAgentWorkflowProductEvent::OperationRecoveryPending { .. }
                | CodingAgentWorkflowProductEvent::OperationRecovered { .. },
            ) => CodingAgentProductEventDeliveryClass::Recovery,
            CodingAgentProductEventKind::Capability(_)
            | CodingAgentProductEventKind::Runtime(CodingAgentRuntimeProductEvent::ShutDown) => {
                CodingAgentProductEventDeliveryClass::Control
            }
            _ if terminal_operation.is_some() => CodingAgentProductEventDeliveryClass::Terminal,
            _ => CodingAgentProductEventDeliveryClass::Data,
        };
        Self {
            stream_id,
            sequence,
            event,
            operation_id,
            parent_operation_id,
            root_operation_id,
            session_id,
            capability_generation: capability_generation.map(CapabilityGeneration::get),
            terminal_status,
            terminal_operation,
            durability,
            delivery_class,
        }
    }

    pub fn sequence(&self) -> u64 {
        self.sequence.get()
    }
    pub fn stream_id(&self) -> &str {
        &self.stream_id
    }
    pub(crate) fn sequence_internal(&self) -> ProductEventSequence {
        self.sequence
    }
    pub fn event(&self) -> &CodingAgentProductEventKind {
        &self.event
    }
    pub fn family_typed(&self) -> CodingAgentProductEventFamily {
        self.event.family()
    }
    pub fn family(&self) -> CodingAgentProductEventFamily {
        self.event.family()
    }
    pub fn kind_name(&self) -> &'static str {
        self.event.as_str()
    }
    pub fn operation_id(&self) -> Option<&str> {
        self.operation_id.as_deref()
    }
    pub fn parent_operation_id(&self) -> Option<&str> {
        self.parent_operation_id.as_deref()
    }
    pub fn root_operation_id(&self) -> Option<&str> {
        self.root_operation_id.as_deref()
    }
    pub fn session_id(&self) -> Option<&str> {
        self.session_id.as_deref()
    }
    pub fn capability_generation(&self) -> Option<u64> {
        self.capability_generation
    }
    pub fn terminal_status(&self) -> Option<CodingAgentProductEventTerminalStatus> {
        self.terminal_status
    }
    pub fn terminal_operation(&self) -> Option<CodingAgentProductEventTerminalOperation> {
        self.terminal_operation
    }
    pub fn durability(&self) -> &CodingAgentProductEventDurability {
        &self.durability
    }
    pub fn delivery_class(&self) -> CodingAgentProductEventDeliveryClass {
        self.delivery_class
    }
}
