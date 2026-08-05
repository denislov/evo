use super::*;

impl CodingAgentProductEventKind {
    pub const fn family(&self) -> CodingAgentProductEventFamily {
        match self {
            Self::Session(_) => CodingAgentProductEventFamily::Session,
            Self::Agent(_) => CodingAgentProductEventFamily::Agent,
            Self::Team(_) => CodingAgentProductEventFamily::Team,
            Self::Message(_) => CodingAgentProductEventFamily::Message,
            Self::Tool(_) => CodingAgentProductEventFamily::Tool,
            Self::Runtime(_) => CodingAgentProductEventFamily::Runtime,
            Self::Delegation(_) => CodingAgentProductEventFamily::Delegation,
            Self::Merge(_) => CodingAgentProductEventFamily::Merge,
            Self::Workflow(_) => CodingAgentProductEventFamily::Workflow,
            Self::Diagnostic(_) => CodingAgentProductEventFamily::Diagnostic,
            Self::Capability(_) => CodingAgentProductEventFamily::Capability,
            Self::Review(_) => CodingAgentProductEventFamily::Review,
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
            Self::Merge(CodingAgentMergeProductEvent::ProposalCreated { .. }) => "proposal_created",
            Self::Merge(CodingAgentMergeProductEvent::Applied { .. }) => "applied",
            Self::Merge(CodingAgentMergeProductEvent::Conflicted { .. }) => "conflicted",
            Self::Merge(CodingAgentMergeProductEvent::StaleParent { .. }) => "stale_parent",
            Self::Merge(CodingAgentMergeProductEvent::Discarded { .. }) => "discarded",
            Self::Merge(CodingAgentMergeProductEvent::Failed { .. }) => "failed",
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
            Self::Review(CodingAgentReviewProductEvent::Changed { .. }) => "changed",
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
