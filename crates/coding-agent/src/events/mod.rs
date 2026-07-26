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
#[cfg(test)]
pub(crate) type ProductEventDurability = CodingAgentProductEventDurability;
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
    crate::runtime::outcome::OPERATION_DESCRIPTOR_REVISION
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

    #[cfg(test)]
    pub(crate) fn from_draft_for_tests(
        sequence: ProductEventSequence,
        draft: emission::ProductEventDraft,
        terminal_operation: Option<CodingAgentProductEventTerminalOperation>,
    ) -> Self {
        Self::new(
            "test-stream".into(),
            sequence,
            draft.event,
            draft.operation_id,
            None,
            None,
            draft.session_id,
            None,
            draft.terminal_status,
            terminal_operation,
            draft.durability,
        )
    }

    #[cfg(test)]
    pub(crate) fn with_lineage_for_tests(
        mut self,
        parent_operation_id: impl Into<String>,
        root_operation_id: impl Into<String>,
    ) -> Self {
        self.parent_operation_id = Some(parent_operation_id.into());
        self.root_operation_id = Some(root_operation_id.into());
        self
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::facade::{
        ProfileId, ProfileKind, SelfHealingEditCheckOutput, SelfHealingEditDiagnostic,
        SelfHealingEditReplacement,
    };
    use ai::api::conversation::{Cost, Usage};

    // product-event-inventory:start
    const EXPECTED_PUBLIC_EVENT_INVENTORY: [(&str, CodingAgentProductEventFamily, &str); 52] = [
        (
            "SessionOpened",
            CodingAgentProductEventFamily::Session,
            "opened",
        ),
        (
            "SessionWritePending",
            CodingAgentProductEventFamily::Session,
            "write_pending",
        ),
        (
            "SessionWriteCommitted",
            CodingAgentProductEventFamily::Session,
            "write_committed",
        ),
        (
            "SessionWriteSkipped",
            CodingAgentProductEventFamily::Session,
            "write_skipped",
        ),
        (
            "SessionCompactionCompleted",
            CodingAgentProductEventFamily::Session,
            "compaction_completed",
        ),
        (
            "DefaultAgentProfileChanged",
            CodingAgentProductEventFamily::Profile,
            "default_changed",
        ),
        (
            "AgentInvocationStarted",
            CodingAgentProductEventFamily::Agent,
            "invocation_started",
        ),
        (
            "AgentInvocationCompleted",
            CodingAgentProductEventFamily::Agent,
            "invocation_completed",
        ),
        (
            "AgentInvocationFailed",
            CodingAgentProductEventFamily::Agent,
            "invocation_failed",
        ),
        (
            "AgentInvocationAborted",
            CodingAgentProductEventFamily::Agent,
            "invocation_aborted",
        ),
        (
            "AgentTurnStarted",
            CodingAgentProductEventFamily::Agent,
            "turn_started",
        ),
        (
            "ProviderRequestStarted",
            CodingAgentProductEventFamily::Agent,
            "provider_request_started",
        ),
        (
            "AgentTeamStarted",
            CodingAgentProductEventFamily::Team,
            "started",
        ),
        (
            "AgentTeamMemberStarted",
            CodingAgentProductEventFamily::Team,
            "member_started",
        ),
        (
            "AgentTeamMemberCompleted",
            CodingAgentProductEventFamily::Team,
            "member_completed",
        ),
        (
            "AgentTeamCompleted",
            CodingAgentProductEventFamily::Team,
            "completed",
        ),
        (
            "AgentTeamFailed",
            CodingAgentProductEventFamily::Team,
            "failed",
        ),
        (
            "AgentTeamAborted",
            CodingAgentProductEventFamily::Team,
            "aborted",
        ),
        (
            "AssistantMessageStarted",
            CodingAgentProductEventFamily::Message,
            "started",
        ),
        (
            "AssistantMessageDelta",
            CodingAgentProductEventFamily::Message,
            "delta",
        ),
        (
            "AssistantThinkingDelta",
            CodingAgentProductEventFamily::Message,
            "thinking_delta",
        ),
        (
            "AssistantMessageCompleted",
            CodingAgentProductEventFamily::Message,
            "completed",
        ),
        (
            "ToolCallStarted",
            CodingAgentProductEventFamily::Tool,
            "started",
        ),
        (
            "ToolCallUpdated",
            CodingAgentProductEventFamily::Tool,
            "updated",
        ),
        (
            "ToolCallCompleted",
            CodingAgentProductEventFamily::Tool,
            "completed",
        ),
        (
            "ToolCallFailed",
            CodingAgentProductEventFamily::Tool,
            "failed",
        ),
        (
            "RuntimeCompactionCompleted",
            CodingAgentProductEventFamily::Runtime,
            "compaction_completed",
        ),
        (
            "RuntimeShutDown",
            CodingAgentProductEventFamily::Runtime,
            "shut_down",
        ),
        (
            "DelegationRequested",
            CodingAgentProductEventFamily::Delegation,
            "requested",
        ),
        (
            "DelegationRejected",
            CodingAgentProductEventFamily::Delegation,
            "rejected",
        ),
        (
            "DelegationApproved",
            CodingAgentProductEventFamily::Delegation,
            "approved",
        ),
        (
            "DelegationConfirmationRequired",
            CodingAgentProductEventFamily::Delegation,
            "confirmation_required",
        ),
        (
            "DelegationStarted",
            CodingAgentProductEventFamily::Delegation,
            "started",
        ),
        (
            "DelegationCompleted",
            CodingAgentProductEventFamily::Delegation,
            "completed",
        ),
        (
            "DelegationFailed",
            CodingAgentProductEventFamily::Delegation,
            "failed",
        ),
        (
            "SelfHealingEditStarted",
            CodingAgentProductEventFamily::Workflow,
            "self_healing_edit_started",
        ),
        (
            "SelfHealingEditRepairAttempted",
            CodingAgentProductEventFamily::Workflow,
            "self_healing_edit_repair_attempted",
        ),
        (
            "SelfHealingEditCompleted",
            CodingAgentProductEventFamily::Workflow,
            "self_healing_edit_completed",
        ),
        (
            "SelfHealingEditFailed",
            CodingAgentProductEventFamily::Workflow,
            "self_healing_edit_failed",
        ),
        (
            "PromptStarted",
            CodingAgentProductEventFamily::Workflow,
            "prompt_started",
        ),
        (
            "PromptCompleted",
            CodingAgentProductEventFamily::Workflow,
            "prompt_completed",
        ),
        (
            "PromptFailed",
            CodingAgentProductEventFamily::Workflow,
            "prompt_failed",
        ),
        (
            "PromptAborted",
            CodingAgentProductEventFamily::Workflow,
            "prompt_aborted",
        ),
        (
            "OperationRecovered",
            CodingAgentProductEventFamily::Workflow,
            "operation_recovered",
        ),
        (
            "Diagnostic",
            CodingAgentProductEventFamily::Diagnostic,
            "diagnostic",
        ),
        (
            "CapabilityChanged",
            CodingAgentProductEventFamily::Capability,
            "changed",
        ),
        (
            "SelfHealingEditAborted",
            CodingAgentProductEventFamily::Workflow,
            "self_healing_edit_aborted",
        ),
        (
            "SessionWriteFailed",
            CodingAgentProductEventFamily::Session,
            "write_failed",
        ),
        (
            "ToolCallAuthorizationRequired",
            CodingAgentProductEventFamily::Tool,
            "authorization_required",
        ),
        (
            "ToolCallAuthorizationApproved",
            CodingAgentProductEventFamily::Tool,
            "authorization_approved",
        ),
        (
            "ToolCallAuthorizationDenied",
            CodingAgentProductEventFamily::Tool,
            "authorization_denied",
        ),
        (
            "ToolCallAuthorizationCancelled",
            CodingAgentProductEventFamily::Tool,
            "authorization_cancelled",
        ),
    ];
    // product-event-inventory:end

    fn project_draft(
        sequence: u64,
        draft: crate::events::emission::ProductEventDraft,
    ) -> CodingAgentProductEvent {
        CodingAgentProductEvent::from_draft_for_tests(
            ProductEventSequence::new(sequence),
            draft,
            None,
        )
    }

    fn project_session(
        sequence: u64,
        event: crate::events::session::SessionWriteEvent,
    ) -> CodingAgentProductEvent {
        project_draft(sequence, event.into_product_draft())
    }

    fn project_prompt(
        sequence: u64,
        event: crate::events::prompt::PromptEvent,
    ) -> CodingAgentProductEvent {
        CodingAgentProductEvent::from_draft_for_tests(
            ProductEventSequence::new(sequence),
            event.into_product_draft(),
            None,
        )
    }

    fn project_prompt_terminal(
        sequence: u64,
        event: crate::events::prompt::PromptEvent,
        operation_kind: crate::runtime::control::OperationKind,
    ) -> CodingAgentProductEvent {
        let evidence = event.root_terminal_evidence(operation_kind);
        let draft = event.into_product_draft();
        let terminal_operation = draft.terminal_status.and_then(|status| {
            evidence.and_then(|evidence| {
                crate::runtime::outcome::product_terminal_operation(
                    operation_kind,
                    evidence,
                    status,
                )
            })
        });
        CodingAgentProductEvent::from_draft_for_tests(
            ProductEventSequence::new(sequence),
            draft,
            terminal_operation,
        )
    }

    fn exhaustive_inventory_fixture() -> Vec<CodingAgentProductEvent> {
        let pid = || ProfileId::from("profile");
        let error = || crate::runtime::facade::CodingSessionError::UnsupportedCapability {
            capability: "fixture".into(),
        };
        let delegation = || {
            (
                "op".to_owned(),
                "turn".to_owned(),
                "call".to_owned(),
                pid(),
                ProfileKind::Agent,
                pid(),
                "task".to_owned(),
            )
        };
        let delegation_context = || {
            let (
                operation_id,
                turn_id,
                tool_call_id,
                requesting_profile_id,
                target_kind,
                target_id,
                task,
            ) = delegation();
            crate::events::delegation::DelegationEventContext {
                operation_id,
                turn_id,
                tool_call_id,
                requesting_profile_id,
                target_kind,
                target_id,
                task,
            }
        };
        let authorization_request = || crate::authorization::ToolAuthorizationRequest {
            authorization_id: "authorization".into(),
            operation_id: "op".into(),
            turn_id: "turn".into(),
            tool_call_id: "call".into(),
            tool_name: "write".into(),
            risk: crate::authorization::ToolAuthorizationRisk::FilesystemMutation,
            scope: crate::authorization::ToolAuthorizationScope::Path {
                path: "file".into(),
            },
            preview: crate::authorization::ToolAuthorizationPreview {
                summary: "write file".into(),
                path: Some("file".into()),
                command: None,
                cwd: None,
                content_preview: Some("content".into()),
            },
            capability_generation: 2,
            requested_at: "2026-07-17T00:00:00Z".into(),
        };
        let usage = Usage {
            input: 1,
            output: 2,
            cache_read: 3,
            cache_write: 4,
            total_tokens: 10,
            cost: Cost::default(),
        };
        // product-event-fixture:start
        let events = vec![
            crate::events::session::SessionCompactionEvent {
                operation_id: "op".into(),
                turn_id: "turn".into(),
                summary: "summary".into(),
                first_kept_message_id: "message".into(),
                tokens_before: 5,
            }
            .into_product_draft(),
            crate::events::workflow::SelfHealingEditEvent::Started {
                operation_id: "op".into(),
                path: "file".into(),
                replacements: 1,
            }
            .into_product_draft(),
            crate::events::workflow::SelfHealingEditEvent::RepairAttempted {
                operation_id: "op".into(),
                path: "file".into(),
                attempt: 1,
                replacements: vec![SelfHealingEditReplacement::new("old", "new")],
                diagnostics: vec![SelfHealingEditDiagnostic {
                    message: "diagnostic".into(),
                }],
                check_output: Some(SelfHealingEditCheckOutput {
                    command: "check".into(),
                    stdout: "out".into(),
                    stderr: String::new(),
                    exit_code: 0,
                }),
            }
            .into_product_draft(),
            crate::events::workflow::SelfHealingEditEvent::Completed {
                operation_id: "op".into(),
                path: "file".into(),
                attempts: 1,
                first_changed_line: Some(1),
                check_output: None,
            }
            .into_product_draft(),
            crate::events::workflow::SelfHealingEditEvent::Failed {
                operation_id: "op".into(),
                path: "file".into(),
                error: error(),
            }
            .into_product_draft(),
            crate::events::recovery::RecoveryEvent {
                operation_id: "op".into(),
                recovery_id: "recovery".into(),
                reason: "restart".into(),
                session_id: "session".into(),
            }
            .into_product_draft(),
            crate::events::workflow::SelfHealingEditEvent::Aborted {
                operation_id: "op-aborted".into(),
                path: "cancelled.rs".into(),
                reason: "cancelled".into(),
            }
            .into_product_draft(),
        ];
        let mut projected = events
            .into_iter()
            .enumerate()
            .map(|(index, draft)| project_draft(index as u64 + 1, draft))
            .collect::<Vec<_>>();
        projected.insert(
            0,
            CodingAgentProductEvent::from_draft_for_tests(
                ProductEventSequence::new(0),
                crate::events::session::SessionLifecycleEvent::Opened {
                    session_id: "session".into(),
                }
                .into_product_draft(),
                None,
            ),
        );
        projected.insert(
            1,
            project_session(
                0,
                crate::events::session::SessionWriteEvent::Pending {
                    operation_id: "op".into(),
                },
            ),
        );
        projected.insert(
            2,
            project_session(
                0,
                crate::events::session::SessionWriteEvent::Committed {
                    operation_id: "op".into(),
                    session_id: "session".into(),
                },
            ),
        );
        projected.insert(
            3,
            project_session(
                0,
                crate::events::session::SessionWriteEvent::Skipped {
                    operation_id: "op".into(),
                    reason: "skip".into(),
                },
            ),
        );
        projected.insert(
            5,
            CodingAgentProductEvent::from_draft_for_tests(
                ProductEventSequence::new(0),
                crate::events::profile::ProfileEvent::DefaultChanged { profile_id: pid() }
                    .into_product_draft(),
                None,
            ),
        );
        for (index, event) in [
            crate::events::agent::AgentInvocationEvent::Started {
                operation_id: "op".into(),
                child_operation_id: "child".into(),
                profile_id: pid(),
                task: "task".into(),
            },
            crate::events::agent::AgentInvocationEvent::Completed {
                operation_id: "op".into(),
                child_operation_id: "child".into(),
                profile_id: pid(),
                final_text: "done".into(),
            },
            crate::events::agent::AgentInvocationEvent::Failed {
                operation_id: "op".into(),
                child_operation_id: "child".into(),
                profile_id: pid(),
                error: error(),
            },
            crate::events::agent::AgentInvocationEvent::Aborted {
                operation_id: "op".into(),
                child_operation_id: "child".into(),
                profile_id: pid(),
                reason: "abort".into(),
            },
        ]
        .into_iter()
        .enumerate()
        {
            projected.insert(
                6 + index,
                CodingAgentProductEvent::from_draft_for_tests(
                    ProductEventSequence::new(0),
                    event.into_product_draft(),
                    None,
                ),
            );
        }
        for (index, event) in [
            crate::events::agent::AgentStreamEvent::TurnStarted {
                operation_id: "op".into(),
                turn_id: "turn".into(),
                agent_turn: 1,
            },
            crate::events::agent::AgentStreamEvent::ProviderRequestStarted {
                operation_id: "op".into(),
                turn_id: "turn".into(),
                provider: "faux".into(),
                model: "model".into(),
                context_window: Some(128_000),
            },
        ]
        .into_iter()
        .enumerate()
        {
            projected.insert(
                10 + index,
                CodingAgentProductEvent::from_draft_for_tests(
                    ProductEventSequence::new(0),
                    event.into_product_draft(),
                    None,
                ),
            );
        }
        for (index, event) in [
            crate::events::team::TeamEvent::Started {
                operation_id: "op".into(),
                team_id: pid(),
                task: "task".into(),
            },
            crate::events::team::TeamEvent::MemberStarted {
                operation_id: "op".into(),
                child_operation_id: "child".into(),
                team_id: pid(),
                profile_id: pid(),
                task: "task".into(),
            },
            crate::events::team::TeamEvent::MemberCompleted {
                operation_id: "op".into(),
                child_operation_id: "child".into(),
                team_id: pid(),
                profile_id: pid(),
                final_text: "done".into(),
            },
            crate::events::team::TeamEvent::Completed {
                operation_id: "op".into(),
                team_id: pid(),
                final_text: "done".into(),
            },
            crate::events::team::TeamEvent::Failed {
                operation_id: "op".into(),
                team_id: pid(),
                error: error(),
            },
            crate::events::team::TeamEvent::Aborted {
                operation_id: "op".into(),
                team_id: pid(),
                reason: "abort".into(),
            },
        ]
        .into_iter()
        .enumerate()
        {
            projected.insert(
                12 + index,
                CodingAgentProductEvent::from_draft_for_tests(
                    ProductEventSequence::new(0),
                    event.into_product_draft(),
                    None,
                ),
            );
        }
        for (index, event) in [
            crate::events::message::MessageEvent::Started {
                operation_id: "op".into(),
                turn_id: "turn".into(),
                message_id: None,
            },
            crate::events::message::MessageEvent::Delta {
                operation_id: "op".into(),
                turn_id: "turn".into(),
                message_id: Some("message".into()),
                text: "delta".into(),
            },
            crate::events::message::MessageEvent::ThinkingDelta {
                operation_id: "op".into(),
                turn_id: "turn".into(),
                message_id: None,
                text: "thinking".into(),
            },
            crate::events::message::MessageEvent::Completed {
                operation_id: "op".into(),
                turn_id: "turn".into(),
                message_id: Some("message".into()),
                final_text: "done".into(),
                images: Vec::new(),
                usage,
            },
        ]
        .into_iter()
        .enumerate()
        {
            projected.insert(
                18 + index,
                CodingAgentProductEvent::from_draft_for_tests(
                    ProductEventSequence::new(0),
                    event.into_product_draft(),
                    None,
                ),
            );
        }
        for (index, event) in [
            crate::events::tool::ToolEvent::Started {
                operation_id: "op".into(),
                turn_id: "turn".into(),
                tool_call_id: "call".into(),
                name: "read".into(),
                arguments_json: "{}".into(),
            },
            crate::events::tool::ToolEvent::Updated {
                operation_id: "op".into(),
                turn_id: "turn".into(),
                tool_call_id: "call".into(),
                name: "read".into(),
                message: "running".into(),
            },
            crate::events::tool::ToolEvent::Completed {
                operation_id: "op".into(),
                turn_id: "turn".into(),
                tool_call_id: "call".into(),
                name: "read".into(),
                summary: "done".into(),
            },
            crate::events::tool::ToolEvent::Failed {
                operation_id: "op".into(),
                turn_id: "turn".into(),
                tool_call_id: "call".into(),
                name: "read".into(),
                message: "failed".into(),
            },
        ]
        .into_iter()
        .enumerate()
        {
            projected.insert(
                22 + index,
                CodingAgentProductEvent::from_draft_for_tests(
                    ProductEventSequence::new(0),
                    event.into_product_draft(),
                    None,
                ),
            );
        }
        for (index, event) in [
            crate::events::runtime::RuntimeEvent::CompactionCompleted {
                operation_id: "op".into(),
                turn_id: "turn".into(),
                summary: "summary".into(),
                first_kept_message_id: "message".into(),
                tokens_before: 5,
            },
            crate::events::runtime::RuntimeEvent::ShutDown,
        ]
        .into_iter()
        .enumerate()
        {
            projected.insert(
                26 + index,
                CodingAgentProductEvent::from_draft_for_tests(
                    ProductEventSequence::new(0),
                    event.into_product_draft(),
                    None,
                ),
            );
        }
        for (index, event) in [
            crate::events::delegation::DelegationEvent::Requested {
                context: delegation_context(),
            },
            crate::events::delegation::DelegationEvent::Rejected {
                context: delegation_context(),
                reason: "rejected".into(),
            },
            crate::events::delegation::DelegationEvent::Approved {
                context: delegation_context(),
            },
            crate::events::delegation::DelegationEvent::ConfirmationRequired {
                context: delegation_context(),
                reason: "confirm".into(),
            },
            crate::events::delegation::DelegationEvent::Started {
                context: delegation_context(),
                child_operation_id: "child".into(),
            },
            crate::events::delegation::DelegationEvent::Completed {
                context: delegation_context(),
                child_operation_id: "child".into(),
                final_text: "done".into(),
            },
            crate::events::delegation::DelegationEvent::Failed {
                context: delegation_context(),
                child_operation_id: "child".into(),
                error: error(),
            },
        ]
        .into_iter()
        .enumerate()
        {
            projected.insert(
                28 + index,
                CodingAgentProductEvent::from_draft_for_tests(
                    ProductEventSequence::new(0),
                    event.into_product_draft(),
                    None,
                ),
            );
        }
        for (index, event) in [
            crate::events::prompt::PromptEvent::Started {
                operation_id: "op".into(),
                turn_id: "turn".into(),
            },
            crate::events::prompt::PromptEvent::Completed {
                operation_id: "op".into(),
                turn_id: "turn".into(),
            },
            crate::events::prompt::PromptEvent::Failed {
                operation_id: "op".into(),
                error: error(),
            },
            crate::events::prompt::PromptEvent::Aborted {
                operation_id: "op".into(),
                reason: "abort".into(),
            },
        ]
        .into_iter()
        .enumerate()
        {
            projected.insert(39 + index, project_prompt(0, event));
        }
        projected.insert(
            44,
            CodingAgentProductEvent::from_draft_for_tests(
                ProductEventSequence::new(0),
                crate::events::diagnostic::DiagnosticEvent::Diagnostic {
                    operation_id: None,
                    message: "diagnostic".into(),
                }
                .into_product_draft(),
                None,
            ),
        );
        projected.insert(
            45,
            CodingAgentProductEvent::from_draft_for_tests(
                ProductEventSequence::new(0),
                crate::events::capability::CapabilityEvent::Changed {
                    generation: 2,
                    revocation: crate::runtime::capability::CapabilityRevocationPolicy::FutureOnly,
                    cancellation_requested_operation_ids: Vec::new(),
                }
                .into_product_draft(),
                None,
            ),
        );
        projected.push(project_session(
            0,
            crate::events::session::SessionWriteEvent::Failed {
                operation_id: "op".into(),
                reason: "write failed".into(),
                status: CodingAgentSessionWriteFailureStatus::Definite,
            },
        ));
        for event in [
            crate::events::tool::ToolEvent::AuthorizationRequired {
                request: authorization_request(),
            },
            crate::events::tool::ToolEvent::AuthorizationApproved {
                request: authorization_request(),
                decision: crate::authorization::ToolAuthorizationDecision::AllowOnce,
            },
            crate::events::tool::ToolEvent::AuthorizationDenied {
                request: authorization_request(),
                reason: "denied".into(),
            },
            crate::events::tool::ToolEvent::AuthorizationCancelled {
                request: authorization_request(),
                reason: "cancelled".into(),
            },
        ] {
            projected.push(project_draft(0, event.into_product_draft()));
        }
        // product-event-fixture:end
        for (index, event) in projected.iter_mut().enumerate() {
            event.sequence = ProductEventSequence::new(index as u64 + 1);
        }
        projected
    }

    #[test]
    fn typed_contract_has_stable_names_and_independent_metadata() {
        let pending = project_session(
            7,
            crate::events::session::SessionWriteEvent::Pending {
                operation_id: "op-7".into(),
            },
        );
        assert_eq!(
            pending.family_typed(),
            CodingAgentProductEventFamily::Session
        );
        assert_eq!(pending.kind_name(), "write_pending");
        assert_eq!(pending.operation_id(), Some("op-7"));
        assert_eq!(pending.terminal_status(), None);
        assert_eq!(pending.terminal_operation(), None);
        assert_eq!(
            pending.durability(),
            &CodingAgentProductEventDurability::PendingSessionWrite {
                operation_id: "op-7".into()
            }
        );
        assert_eq!(
            serde_json::to_string(&CodingAgentProductEventDurability::LiveOnly).unwrap(),
            "{\"state\":\"live_only\"}"
        );
    }

    #[test]
    fn completed_message_serializes_images_and_omits_an_empty_inventory() {
        let usage = CodingAgentProductEventUsage {
            input: 0,
            output: 0,
            cache_read: 0,
            cache_write: 0,
            total_tokens: 0,
            cost_known: true,
            input_cost: 0.0,
            output_cost: 0.0,
            cache_read_cost: 0.0,
            cache_write_cost: 0.0,
        };
        let event = CodingAgentMessageProductEvent::Completed {
            operation_id: "op-1".into(),
            turn_id: "turn-1".into(),
            message_id: Some("message-1".into()),
            final_text: "caption".into(),
            images: vec![CodingAgentImageContent {
                mime_type: "image/png".into(),
                data: "cG5n".into(),
            }],
            usage: usage.clone(),
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["images"][0]["mime_type"], "image/png");
        assert_eq!(json["images"][0]["data"], "cG5n");
        assert!(json["usage"].get("cost_known").is_none());

        let unknown_cost = CodingAgentProductEventUsage {
            cost_known: false,
            ..usage.clone()
        };
        assert_eq!(
            serde_json::to_value(unknown_cost).unwrap()["cost_known"],
            false
        );

        let empty = CodingAgentMessageProductEvent::Completed {
            operation_id: "op-1".into(),
            turn_id: "turn-1".into(),
            message_id: Some("message-1".into()),
            final_text: "text only".into(),
            images: Vec::new(),
            usage,
        };
        assert!(serde_json::to_value(empty).unwrap().get("images").is_none());
    }

    #[test]
    fn session_write_failure_distinguishes_definite_from_uncertain_persistence() {
        let definite = project_session(
            8,
            crate::events::session::SessionWriteEvent::Failed {
                operation_id: "op-definite".into(),
                reason: "capability rejected before commit".into(),
                status: CodingAgentSessionWriteFailureStatus::Definite,
            },
        );
        assert!(matches!(
            definite.durability(),
            CodingAgentProductEventDurability::PersistenceFailed {
                operation_id,
                reason,
            } if operation_id == "op-definite" && reason == "capability rejected before commit"
        ));

        let uncertain = project_session(
            9,
            crate::events::session::SessionWriteEvent::Failed {
                operation_id: "op-uncertain".into(),
                reason: "append result is uncertain".into(),
                status: CodingAgentSessionWriteFailureStatus::Uncertain,
            },
        );
        assert!(matches!(
            uncertain.durability(),
            CodingAgentProductEventDurability::PersistenceUncertain { operation_id }
                if operation_id == "op-uncertain"
        ));
    }

    #[test]
    fn event_terminal_does_not_imply_root_operation_terminal() {
        let tool = CodingAgentProductEvent::from_draft_for_tests(
            ProductEventSequence::new(8),
            crate::events::tool::ToolEvent::Completed {
                operation_id: "op".into(),
                turn_id: "turn".into(),
                tool_call_id: "call".into(),
                name: "read".into(),
                summary: "ok".into(),
            }
            .into_product_draft(),
            None,
        );
        assert_eq!(
            tool.terminal_status(),
            Some(CodingAgentProductEventTerminalStatus::Completed)
        );
        assert_eq!(tool.terminal_operation(), None);
        let prompt = project_prompt_terminal(
            9,
            crate::events::prompt::PromptEvent::Completed {
                operation_id: "op".into(),
                turn_id: "turn".into(),
            },
            crate::runtime::control::OperationKind::Prompt,
        );
        assert_eq!(
            prompt.terminal_operation().unwrap().kind,
            CodingAgentProductEventTerminalOperationKind::Prompt
        );
    }

    #[test]
    fn exhaustive_inventory_covers_all_current_variants() {
        let projected = exhaustive_inventory_fixture();
        assert_eq!(projected.len(), EXPECTED_PUBLIC_EVENT_INVENTORY.len());
        assert_eq!(projected.len(), 52);
        for (index, (event, (_, family, kind))) in projected
            .iter()
            .zip(EXPECTED_PUBLIC_EVENT_INVENTORY.iter())
            .enumerate()
        {
            assert_eq!(event.sequence(), index as u64 + 1, "inventory row {index}");
            assert_eq!(event.family(), *family, "inventory row {index}");
            assert_eq!(event.kind_name(), *kind, "inventory row {index}");
            assert_public_inventory_payload(index, event);
        }
        let expected_counts = [6, 1, 6, 6, 4, 8, 2, 7, 10, 1, 1];
        let families = [
            CodingAgentProductEventFamily::Session,
            CodingAgentProductEventFamily::Profile,
            CodingAgentProductEventFamily::Agent,
            CodingAgentProductEventFamily::Team,
            CodingAgentProductEventFamily::Message,
            CodingAgentProductEventFamily::Tool,
            CodingAgentProductEventFamily::Runtime,
            CodingAgentProductEventFamily::Delegation,
            CodingAgentProductEventFamily::Workflow,
            CodingAgentProductEventFamily::Diagnostic,
            CodingAgentProductEventFamily::Capability,
        ];
        let counts: Vec<_> = families
            .iter()
            .map(|family| {
                projected
                    .iter()
                    .filter(|event| event.family() == *family)
                    .count()
            })
            .collect();
        assert_eq!(counts, expected_counts);
        assert_eq!(projected[0].operation_id(), None);
        assert_eq!(
            projected[1].durability(),
            &CodingAgentProductEventDurability::PendingSessionWrite {
                operation_id: "op".into()
            }
        );
        assert_eq!(
            projected[2].durability(),
            &CodingAgentProductEventDurability::Durable {
                session_id: "session".into()
            }
        );
        assert_eq!(
            projected[3].durability(),
            &CodingAgentProductEventDurability::LiveOnly
        );
        assert_eq!(projected[27].operation_id(), None);
        assert_eq!(projected[44].operation_id(), None);
        assert_eq!(projected[45].operation_id(), None);
        assert_eq!(
            projected[43].terminal_status(),
            Some(CodingAgentProductEventTerminalStatus::Recovered)
        );
        assert_eq!(projected[43].terminal_operation(), None);
        assert_eq!(
            projected
                .iter()
                .map(CodingAgentProductEvent::sequence)
                .collect::<Vec<_>>(),
            (1..=52).collect::<Vec<_>>()
        );
    }

    fn assert_public_inventory_payload(index: usize, event: &CodingAgentProductEvent) {
        use CodingAgentProductEventKind as K;
        let valid = match (index, event.event()) {
            (0, K::Session(CodingAgentSessionProductEvent::Opened { session_id })) => {
                session_id == "session"
            }
            (1, K::Session(CodingAgentSessionProductEvent::WritePending { operation_id })) => {
                operation_id == "op"
            }
            (2, K::Session(CodingAgentSessionProductEvent::WriteCommitted { session_id, .. })) => {
                session_id == "session"
            }
            (3, K::Session(CodingAgentSessionProductEvent::WriteSkipped { reason, .. })) => {
                reason == "skip"
            }
            (
                4,
                K::Session(CodingAgentSessionProductEvent::CompactionCompleted {
                    turn_id,
                    tokens_before,
                    ..
                }),
            ) => turn_id == "turn" && *tokens_before == 5,
            (5, K::Profile(CodingAgentProfileProductEvent::DefaultChanged { profile_id })) => {
                profile_id == "profile"
            }
            (
                6,
                K::Agent(CodingAgentAgentProductEvent::InvocationStarted {
                    child_operation_id,
                    task,
                    ..
                }),
            ) => child_operation_id == "child" && task == "task",
            (7, K::Agent(CodingAgentAgentProductEvent::InvocationCompleted { final_text, .. })) => {
                final_text == "done"
            }
            (8, K::Agent(CodingAgentAgentProductEvent::InvocationFailed { error, .. })) => {
                error.code == "unsupported_capability"
            }
            (9, K::Agent(CodingAgentAgentProductEvent::InvocationAborted { reason, .. })) => {
                reason == "abort"
            }
            (10, K::Agent(CodingAgentAgentProductEvent::TurnStarted { agent_turn, .. })) => {
                *agent_turn == 1
            }
            (
                11,
                K::Agent(CodingAgentAgentProductEvent::ProviderRequestStarted {
                    provider,
                    model,
                    ..
                }),
            ) => provider == "faux" && model == "model",
            (12, K::Team(CodingAgentTeamProductEvent::Started { team_id, task, .. })) => {
                team_id == "profile" && task == "task"
            }
            (
                13,
                K::Team(CodingAgentTeamProductEvent::MemberStarted {
                    child_operation_id,
                    profile_id,
                    ..
                }),
            ) => child_operation_id == "child" && profile_id == "profile",
            (14, K::Team(CodingAgentTeamProductEvent::MemberCompleted { final_text, .. })) => {
                final_text == "done"
            }
            (15, K::Team(CodingAgentTeamProductEvent::Completed { final_text, .. })) => {
                final_text == "done"
            }
            (16, K::Team(CodingAgentTeamProductEvent::Failed { error, .. })) => {
                error.code == "unsupported_capability"
            }
            (17, K::Team(CodingAgentTeamProductEvent::Aborted { reason, .. })) => reason == "abort",
            (18, K::Message(CodingAgentMessageProductEvent::Started { message_id, .. })) => {
                message_id.is_none()
            }
            (
                19,
                K::Message(CodingAgentMessageProductEvent::Delta {
                    message_id, text, ..
                }),
            ) => message_id.as_deref() == Some("message") && text == "delta",
            (20, K::Message(CodingAgentMessageProductEvent::ThinkingDelta { text, .. })) => {
                text == "thinking"
            }
            (
                21,
                K::Message(CodingAgentMessageProductEvent::Completed {
                    final_text, usage, ..
                }),
            ) => final_text == "done" && usage.total_tokens == 10,
            (
                22,
                K::Tool(CodingAgentToolProductEvent::Started {
                    tool_call_id,
                    arguments_json,
                    ..
                }),
            ) => tool_call_id == "call" && arguments_json == "{}",
            (23, K::Tool(CodingAgentToolProductEvent::Updated { message, .. })) => {
                message == "running"
            }
            (24, K::Tool(CodingAgentToolProductEvent::Completed { summary, .. })) => {
                summary == "done"
            }
            (25, K::Tool(CodingAgentToolProductEvent::Failed { message, .. })) => {
                message == "failed"
            }
            (
                26,
                K::Runtime(CodingAgentRuntimeProductEvent::CompactionCompleted {
                    first_kept_message_id,
                    tokens_before,
                    ..
                }),
            ) => first_kept_message_id == "message" && *tokens_before == 5,
            (27, K::Runtime(CodingAgentRuntimeProductEvent::ShutDown)) => true,
            (28, K::Delegation(CodingAgentDelegationProductEvent::Requested { context })) => {
                context.target_kind == CodingAgentProductEventProfileKind::Agent
                    && context.task == "task"
            }
            (29, K::Delegation(CodingAgentDelegationProductEvent::Rejected { reason, .. })) => {
                reason == "rejected"
            }
            (30, K::Delegation(CodingAgentDelegationProductEvent::Approved { context })) => {
                context.tool_call_id == "call"
            }
            (
                31,
                K::Delegation(CodingAgentDelegationProductEvent::ConfirmationRequired {
                    reason,
                    ..
                }),
            ) => reason == "confirm",
            (
                32,
                K::Delegation(CodingAgentDelegationProductEvent::Started {
                    child_operation_id,
                    ..
                }),
            ) => child_operation_id == "child",
            (
                33,
                K::Delegation(CodingAgentDelegationProductEvent::Completed { final_text, .. }),
            ) => final_text == "done",
            (34, K::Delegation(CodingAgentDelegationProductEvent::Failed { error, .. })) => {
                error.code == "unsupported_capability"
            }
            (
                35,
                K::Workflow(CodingAgentWorkflowProductEvent::SelfHealingEditStarted {
                    path,
                    replacements,
                    ..
                }),
            ) => path == "file" && *replacements == 1,
            (
                36,
                K::Workflow(CodingAgentWorkflowProductEvent::SelfHealingEditRepairAttempted {
                    attempt,
                    replacements,
                    diagnostics,
                    check_output,
                    ..
                }),
            ) => {
                *attempt == 1
                    && replacements[0].old_text == "old"
                    && diagnostics[0].message == "diagnostic"
                    && check_output
                        .as_ref()
                        .is_some_and(|output| output.command == "check")
            }
            (
                37,
                K::Workflow(CodingAgentWorkflowProductEvent::SelfHealingEditCompleted {
                    attempts,
                    first_changed_line,
                    ..
                }),
            ) => *attempts == 1 && *first_changed_line == Some(1),
            (
                38,
                K::Workflow(CodingAgentWorkflowProductEvent::SelfHealingEditFailed {
                    error, ..
                }),
            ) => error.code == "unsupported_capability",
            (39, K::Workflow(CodingAgentWorkflowProductEvent::PromptStarted { turn_id, .. })) => {
                turn_id == "turn"
            }
            (40, K::Workflow(CodingAgentWorkflowProductEvent::PromptCompleted { turn_id, .. })) => {
                turn_id == "turn"
            }
            (41, K::Workflow(CodingAgentWorkflowProductEvent::PromptFailed { error, .. })) => {
                error.code == "unsupported_capability"
            }
            (42, K::Workflow(CodingAgentWorkflowProductEvent::PromptAborted { reason, .. })) => {
                reason == "abort"
            }
            (
                43,
                K::Workflow(CodingAgentWorkflowProductEvent::OperationRecovered {
                    recovery_id,
                    reason,
                    ..
                }),
            ) => recovery_id == "recovery" && reason == "restart",
            (44, K::Diagnostic(CodingAgentDiagnosticProductEvent::Diagnostic { diagnostic })) => {
                diagnostic.operation_id.is_none() && diagnostic.summary == "diagnostic"
            }
            (
                45,
                K::Capability(CodingAgentCapabilityProductEvent::Changed {
                    generation,
                    revocation,
                    cancellation_requested_operation_ids,
                }),
            ) => {
                *generation == 2
                    && *revocation == CodingAgentProductEventCapabilityRevocation::FutureOnly
                    && cancellation_requested_operation_ids.is_empty()
            }
            (
                46,
                K::Workflow(CodingAgentWorkflowProductEvent::SelfHealingEditAborted {
                    path,
                    reason,
                    ..
                }),
            ) => path == "cancelled.rs" && reason == "cancelled",
            (
                47,
                K::Session(CodingAgentSessionProductEvent::WriteFailed { reason, status, .. }),
            ) => {
                reason == "write failed"
                    && *status == CodingAgentSessionWriteFailureStatus::Definite
            }
            (48, K::Tool(CodingAgentToolProductEvent::AuthorizationRequired { request })) => {
                request.authorization_id == "authorization" && request.tool_name == "write"
            }
            (
                49,
                K::Tool(CodingAgentToolProductEvent::AuthorizationApproved {
                    authorization_id,
                    decision,
                    ..
                }),
            ) => {
                authorization_id == "authorization"
                    && *decision == crate::authorization::ToolAuthorizationDecision::AllowOnce
            }
            (
                50,
                K::Tool(CodingAgentToolProductEvent::AuthorizationDenied {
                    authorization_id,
                    reason,
                    ..
                }),
            ) => authorization_id == "authorization" && reason == "denied",
            (
                51,
                K::Tool(CodingAgentToolProductEvent::AuthorizationCancelled {
                    authorization_id,
                    reason,
                    ..
                }),
            ) => authorization_id == "authorization" && reason == "cancelled",
            _ => false,
        };
        assert!(
            valid,
            "typed payload mismatch at inventory row {index}: {:?}",
            event.event()
        );
    }
}
