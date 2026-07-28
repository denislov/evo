use std::path::PathBuf;

use sha2::{Digest, Sha256};

use crate::events::{
    CodingAgentProductEventTerminalOperation, CodingAgentProductEventTerminalOperationKind,
    CodingAgentProductEventTerminalStatus,
};
use crate::operations::agent_invocation::runner::{AgentInvocationOptions, AgentInvocationOutcome};
use crate::operations::export::CodingAgentSessionExport;
use crate::operations::export::runner::ExportOptions;
use crate::operations::prompt::context::{InternalPromptTurnOutcome, PromptTurnOptions};
use crate::operations::self_healing_edit::runner::{
    SelfHealingEditOutcome, SelfHealingEditRequest,
};
use crate::operations::team_invocation::runner::{AgentTeamOptions, AgentTeamOutcome};
use crate::profiles::ProfileId;
use crate::runtime::control::OperationKind;
use crate::runtime::operation::{
    Operation, OperationClass, OperationDispatchMode, OperationOutcome,
};
use crate::runtime::public_error::{
    CodingAgentPublicDiagnostic, CodingAgentPublicError, safe_public_summary,
};
use ai::api::conversation::AssistantMessage;

/// Controls whether branch summarization may reuse a previously persisted summary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BranchSummaryReusePolicy {
    /// Always create a new summary for the requested branch pair.
    AlwaysCreate,
    /// Reuse a matching persisted summary without emitting events or rewriting the session log.
    /// A new summary is created when no matching persisted summary exists.
    ReuseExisting,
}

#[derive(Debug)]
pub enum CodingAgentOperation {
    Prompt(PromptTurnOptions),
    Compact(PromptTurnOptions),
    BranchSummary {
        options: PromptTurnOptions,
        source_leaf_id: String,
        target_leaf_id: String,
        custom_instructions: Option<String>,
        reuse: BranchSummaryReusePolicy,
    },
    SelfHealingEdit(SelfHealingEditRequest),
    InvokeAgent(AgentInvocationOptions),
    InvokeTeam(AgentTeamOptions),
    SetDefaultAgentProfile {
        profile_id: ProfileId,
    },
    ApproveDelegation {
        operation_id: String,
        tool_call_id: String,
    },
    RejectDelegation {
        operation_id: String,
        tool_call_id: String,
        reason: String,
    },
    /// Move this owner to a forked persistent session while retaining live runtime state.
    ForkSession {
        /// The leaf to fork from, or the current active leaf when omitted.
        target_leaf_id: Option<String>,
    },
    /// Make an existing committed leaf active in a persistent session.
    SwitchActiveLeaf {
        target_leaf_id: String,
    },
    SetSessionTreeLabel {
        entry_id: String,
        label: Option<String>,
    },
    /// Set or clear the durable presentation name of the current session.
    SetSessionName {
        name: Option<String>,
    },
    ExportCurrent,
    ExportCurrentHtml(PathBuf),
}

#[derive(Debug)]
pub enum CodingAgentOperationOutcome {
    Prompt(PromptTurnOutcome),
    Compact(PromptTurnOutcome),
    BranchSummary(PromptTurnOutcome),
    SelfHealingEdit(SelfHealingEditOutcome),
    AgentInvocation(AgentInvocationOutcome),
    AgentTeam(AgentTeamOutcome),
    DefaultAgentProfileChanged,
    DelegationApproved,
    DelegationRejected,
    /// The session owner was replaced with a newly forked session.
    SessionForked,
    /// The requested existing leaf became active.
    ActiveLeafSwitched,
    SessionTreeLabelChanged {
        entry_id: String,
        label: Option<String>,
        updated_at: String,
    },
    SessionNameChanged {
        name: Option<String>,
        updated_at: String,
    },
    Export(CodingAgentSessionExport),
    ExportHtml(PathBuf),
}

#[derive(Debug, Clone, PartialEq)]
#[allow(
    clippy::large_enum_variant,
    reason = "the stable typed outcome preserves the final provider-neutral message without a second allocation"
)]
pub enum PromptTurnOutcome {
    Success {
        operation_id: String,
        turn_id: String,
        session_id: Option<String>,
        leaf_id: Option<String>,
        final_text: String,
        final_message: AssistantMessage,
        diagnostics: Vec<CodingAgentPublicDiagnostic>,
    },
    Aborted {
        operation_id: String,
        turn_id: Option<String>,
        reason: String,
        session_id: Option<String>,
    },
    Failed {
        operation_id: String,
        turn_id: Option<String>,
        error: CodingAgentPublicError,
        diagnostics: Vec<CodingAgentPublicDiagnostic>,
    },
}

impl PromptTurnOutcome {
    fn from_internal(outcome: InternalPromptTurnOutcome) -> Self {
        match outcome {
            InternalPromptTurnOutcome::Success {
                operation_id,
                turn_id,
                session_id,
                leaf_id,
                final_text,
                final_message,
                diagnostics,
            } => Self::Success {
                diagnostics: CodingAgentPublicDiagnostic::from_runtime_diagnostics(
                    &diagnostics,
                    Some(&operation_id),
                ),
                operation_id,
                turn_id,
                session_id,
                leaf_id,
                final_text,
                final_message,
            },
            InternalPromptTurnOutcome::Aborted {
                operation_id,
                turn_id,
                reason,
                session_id,
            } => Self::Aborted {
                operation_id,
                turn_id,
                reason: safe_public_summary(&reason),
                session_id,
            },
            InternalPromptTurnOutcome::Failed {
                operation_id,
                turn_id,
                error,
                diagnostics,
            } => Self::Failed {
                diagnostics: CodingAgentPublicDiagnostic::from_runtime_diagnostics(
                    &diagnostics,
                    Some(&operation_id),
                ),
                operation_id,
                turn_id,
                error: CodingAgentPublicError::from(error),
            },
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OperationTerminalPolicy {
    ProductEvent,
    OutcomeAcknowledgement,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum OperationOutcomeFamily {
    Prompt,
    Compact,
    BranchSummary,
    SelfHealingEdit,
    AgentInvocation,
    AgentTeam,
    DefaultAgentProfileChanged,
    DelegationApproved,
    DelegationRejected,
    SessionForked,
    ActiveLeafSwitched,
    SessionTreeLabelChanged,
    SessionNameChanged,
    Export,
    ExportHtml,
}

/// Exact root-event variants that may finalize a terminal-associated operation.
///
/// `CompactPromptFailed` is intentionally distinct from ordinary Prompt failure: it is
/// admitted only for a Compact operation whose typed outcome is the failed Compact branch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum OperationRootTerminalEvidence {
    PromptCompleted,
    PromptFailed,
    PromptAborted,
    CompactionCompleted,
    CompactPromptFailed,
    SelfHealingEditCompleted,
    SelfHealingEditFailed,
    SelfHealingEditAborted,
    AgentInvocationCompleted,
    AgentInvocationFailed,
    AgentInvocationAborted,
    AgentTeamCompleted,
    AgentTeamFailed,
    AgentTeamAborted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct OperationDescriptor {
    pub(crate) revision: u16,
    pub(crate) submitted_kind: OperationKind,
    pub(crate) dispatch_mode: OperationDispatchMode,
    pub(crate) outcome_family: OperationOutcomeFamily,
    pub(crate) terminal_policy: OperationTerminalPolicy,
    pub(crate) permitted_root_evidence: &'static [OperationRootTerminalEvidence],
    pub(crate) lineage: OperationLineage,
    pub(crate) session_access: OperationSessionAccess,
    pub(crate) runtime_access: OperationRuntimeAccess,
    pub(crate) priority: OperationPriority,
    pub(crate) capacity: OperationCapacity,
    pub(crate) durability: OperationDurability,
    pub(crate) cancellation: OperationCancellation,
    pub(crate) child_policy: OperationChildPolicy,
}

pub(crate) const OPERATION_DESCRIPTOR_REVISION: u16 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OperationLineage {
    Root,
    Child,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OperationSessionAccess {
    None,
    Read,
    Write,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OperationRuntimeAccess {
    None,
    Read,
    Write,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OperationPriority {
    Interactive,
    Normal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OperationCapacity {
    Shared,
    SessionWriter,
    BoundedRuntime,
    RuntimeExclusive,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct OperationDurability {
    pub(crate) session_if_persistent: bool,
    pub(crate) runtime_generation: bool,
}

impl OperationDurability {
    const NONE: Self = Self {
        session_if_persistent: false,
        runtime_generation: false,
    };
    const SESSION: Self = Self {
        session_if_persistent: true,
        runtime_generation: false,
    };
    const SESSION_AND_RUNTIME: Self = Self {
        session_if_persistent: true,
        runtime_generation: true,
    };
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OperationCancellation {
    Cancellable,
    Atomic,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OperationChildPolicy {
    Forbidden,
    Structured,
}

impl OperationDescriptor {
    pub(crate) fn admission_class(self) -> OperationClass {
        match (
            self.lineage,
            self.session_access,
            self.runtime_access,
            self.capacity,
        ) {
            (
                OperationLineage::Root,
                _,
                OperationRuntimeAccess::Write,
                OperationCapacity::RuntimeExclusive,
            ) => OperationClass::RuntimeWrite,
            (
                OperationLineage::Root,
                OperationSessionAccess::Write,
                _,
                OperationCapacity::SessionWriter,
            ) => OperationClass::SessionWriteRoot,
            (
                OperationLineage::Root,
                OperationSessionAccess::None,
                _,
                OperationCapacity::BoundedRuntime,
            ) => OperationClass::NonSessionRoot,
            (
                OperationLineage::Root,
                OperationSessionAccess::Read,
                _,
                OperationCapacity::Shared,
            ) => OperationClass::ReadOnly,
            (OperationLineage::Child, _, _, _) => OperationClass::Child,
            _ => unreachable!("validated descriptor must derive one admission class"),
        }
    }

    pub(crate) fn validate(self) -> Result<(), &'static str> {
        self.validate_terminal_policy()?;
        match (
            self.lineage,
            self.session_access,
            self.runtime_access,
            self.capacity,
        ) {
            (
                OperationLineage::Root,
                _,
                OperationRuntimeAccess::Write,
                OperationCapacity::RuntimeExclusive,
            )
            | (
                OperationLineage::Root,
                OperationSessionAccess::Write,
                _,
                OperationCapacity::SessionWriter,
            )
            | (
                OperationLineage::Root,
                OperationSessionAccess::None,
                _,
                OperationCapacity::BoundedRuntime,
            )
            | (
                OperationLineage::Root,
                OperationSessionAccess::Read,
                _,
                OperationCapacity::Shared,
            ) => {}
            (OperationLineage::Child, OperationSessionAccess::None, _, _) => {}
            _ => return Err("operation access and capacity claims do not derive a valid class"),
        }
        if self.durability.session_if_persistent
            && self.session_access != OperationSessionAccess::Write
        {
            return Err("session durability requires session write access");
        }
        if self.durability.runtime_generation
            && self.runtime_access != OperationRuntimeAccess::Write
        {
            return Err("runtime generation durability requires runtime write access");
        }
        match (self.dispatch_mode, self.cancellation) {
            (OperationDispatchMode::Async, OperationCancellation::Cancellable)
            | (
                OperationDispatchMode::SyncReadOnly | OperationDispatchMode::SyncMutable,
                OperationCancellation::Atomic,
            ) => {}
            _ => return Err("dispatch mode and cancellation claim conflict"),
        }
        if self.child_policy == OperationChildPolicy::Structured
            && self.cancellation != OperationCancellation::Cancellable
        {
            return Err("structured children require cancellable ownership");
        }
        Ok(())
    }

    pub(crate) fn validate_terminal_policy(self) -> Result<(), &'static str> {
        match (
            self.terminal_policy,
            self.permitted_root_evidence.is_empty(),
        ) {
            (OperationTerminalPolicy::ProductEvent, false)
            | (OperationTerminalPolicy::OutcomeAcknowledgement, true) => Ok(()),
            (OperationTerminalPolicy::ProductEvent, true) => {
                Err("ProductEvent terminal policy requires root terminal evidence")
            }
            (OperationTerminalPolicy::OutcomeAcknowledgement, false) => {
                Err("outcome acknowledgement policy forbids root terminal evidence")
            }
        }
    }

    fn for_child(mut self) -> Option<Self> {
        if self.child_policy != OperationChildPolicy::Structured
            || self.dispatch_mode != OperationDispatchMode::Async
            || self.cancellation != OperationCancellation::Cancellable
        {
            return None;
        }
        self.lineage = OperationLineage::Child;
        self.session_access = OperationSessionAccess::None;
        self.runtime_access = OperationRuntimeAccess::Read;
        self.capacity = OperationCapacity::BoundedRuntime;
        self.durability = OperationDurability::NONE;
        debug_assert_eq!(self.validate(), Ok(()));
        Some(self)
    }
}

const PROMPT_ROOT_EVIDENCE: &[OperationRootTerminalEvidence] = &[
    OperationRootTerminalEvidence::PromptCompleted,
    OperationRootTerminalEvidence::PromptFailed,
    OperationRootTerminalEvidence::PromptAborted,
];
const COMPACT_ROOT_EVIDENCE: &[OperationRootTerminalEvidence] = &[
    OperationRootTerminalEvidence::CompactionCompleted,
    OperationRootTerminalEvidence::CompactPromptFailed,
];
const SELF_HEALING_EDIT_ROOT_EVIDENCE: &[OperationRootTerminalEvidence] = &[
    OperationRootTerminalEvidence::SelfHealingEditCompleted,
    OperationRootTerminalEvidence::SelfHealingEditFailed,
    OperationRootTerminalEvidence::SelfHealingEditAborted,
];
const AGENT_INVOCATION_ROOT_EVIDENCE: &[OperationRootTerminalEvidence] = &[
    OperationRootTerminalEvidence::AgentInvocationCompleted,
    OperationRootTerminalEvidence::AgentInvocationFailed,
    OperationRootTerminalEvidence::AgentInvocationAborted,
];
const AGENT_TEAM_ROOT_EVIDENCE: &[OperationRootTerminalEvidence] = &[
    OperationRootTerminalEvidence::AgentTeamCompleted,
    OperationRootTerminalEvidence::AgentTeamFailed,
    OperationRootTerminalEvidence::AgentTeamAborted,
];
pub(crate) fn product_terminal_operation(
    kind: OperationKind,
    evidence: OperationRootTerminalEvidence,
    status: CodingAgentProductEventTerminalStatus,
) -> Option<CodingAgentProductEventTerminalOperation> {
    let permitted = match kind {
        OperationKind::Prompt => PROMPT_ROOT_EVIDENCE,
        OperationKind::Compact => COMPACT_ROOT_EVIDENCE,
        OperationKind::SelfHealingEdit => SELF_HEALING_EDIT_ROOT_EVIDENCE,
        OperationKind::AgentInvocation => AGENT_INVOCATION_ROOT_EVIDENCE,
        OperationKind::AgentTeam => AGENT_TEAM_ROOT_EVIDENCE,
        OperationKind::BranchSummary
        | OperationKind::DelegationConfirmation
        | OperationKind::ForkSession
        | OperationKind::SwitchActiveLeaf
        | OperationKind::SetSessionTreeLabel
        | OperationKind::SetSessionName
        | OperationKind::SetDefaultAgentProfile
        | OperationKind::Export => return None,
    };
    if !permitted.contains(&evidence) {
        return None;
    }
    let kind = match kind {
        OperationKind::Prompt => CodingAgentProductEventTerminalOperationKind::Prompt,
        OperationKind::Compact => CodingAgentProductEventTerminalOperationKind::Compact,
        OperationKind::SelfHealingEdit => {
            CodingAgentProductEventTerminalOperationKind::SelfHealingEdit
        }
        OperationKind::AgentInvocation => {
            CodingAgentProductEventTerminalOperationKind::AgentInvocation
        }
        OperationKind::AgentTeam => CodingAgentProductEventTerminalOperationKind::AgentTeam,
        OperationKind::BranchSummary
        | OperationKind::DelegationConfirmation
        | OperationKind::ForkSession
        | OperationKind::SwitchActiveLeaf
        | OperationKind::SetSessionTreeLabel
        | OperationKind::SetSessionName
        | OperationKind::SetDefaultAgentProfile
        | OperationKind::Export => unreachable!("non-terminal operation kind filtered above"),
    };
    Some(CodingAgentProductEventTerminalOperation { kind, status })
}

pub(crate) fn terminal_operation_kind(
    kind: OperationKind,
) -> Option<CodingAgentProductEventTerminalOperationKind> {
    match kind {
        OperationKind::Prompt => Some(CodingAgentProductEventTerminalOperationKind::Prompt),
        OperationKind::Compact => Some(CodingAgentProductEventTerminalOperationKind::Compact),
        OperationKind::SelfHealingEdit => {
            Some(CodingAgentProductEventTerminalOperationKind::SelfHealingEdit)
        }
        OperationKind::AgentInvocation => {
            Some(CodingAgentProductEventTerminalOperationKind::AgentInvocation)
        }
        OperationKind::AgentTeam => Some(CodingAgentProductEventTerminalOperationKind::AgentTeam),
        OperationKind::BranchSummary
        | OperationKind::DelegationConfirmation
        | OperationKind::ForkSession
        | OperationKind::SwitchActiveLeaf
        | OperationKind::SetSessionTreeLabel
        | OperationKind::SetSessionName
        | OperationKind::SetDefaultAgentProfile
        | OperationKind::Export => None,
    }
}

#[cfg(test)]
pub(crate) fn recovered_product_terminal_operation(
    kind: OperationKind,
) -> Option<CodingAgentProductEventTerminalOperation> {
    Some(CodingAgentProductEventTerminalOperation {
        kind: recovery_terminal_operation_kind(kind)?,
        status: CodingAgentProductEventTerminalStatus::Recovered,
    })
}

pub(crate) fn recovery_resolution_terminal_operation(
    kind: OperationKind,
    status: CodingAgentProductEventTerminalStatus,
) -> Option<CodingAgentProductEventTerminalOperation> {
    if !matches!(
        status,
        CodingAgentProductEventTerminalStatus::Failed
            | CodingAgentProductEventTerminalStatus::Aborted
    ) {
        return None;
    }
    Some(CodingAgentProductEventTerminalOperation {
        kind: recovery_terminal_operation_kind(kind)?,
        status,
    })
}

fn recovery_terminal_operation_kind(
    kind: OperationKind,
) -> Option<CodingAgentProductEventTerminalOperationKind> {
    Some(match kind {
        OperationKind::Prompt => CodingAgentProductEventTerminalOperationKind::Prompt,
        OperationKind::Compact => CodingAgentProductEventTerminalOperationKind::Compact,
        OperationKind::BranchSummary => CodingAgentProductEventTerminalOperationKind::BranchSummary,
        OperationKind::SelfHealingEdit => {
            CodingAgentProductEventTerminalOperationKind::SelfHealingEdit
        }
        OperationKind::Export => CodingAgentProductEventTerminalOperationKind::Export,
        OperationKind::AgentInvocation
        | OperationKind::AgentTeam
        | OperationKind::DelegationConfirmation
        | OperationKind::ForkSession
        | OperationKind::SwitchActiveLeaf
        | OperationKind::SetSessionTreeLabel
        | OperationKind::SetSessionName
        | OperationKind::SetDefaultAgentProfile => return None,
    })
}

/// Resolve the internal payload enum through the public operation contract.
///
/// The internal enum intentionally owns no scheduling or lifecycle table: it
/// only maps its payload shape back to the authoritative public descriptor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OperationContract {
    Prompt,
    Compact,
    BranchSummary,
    SelfHealingEdit,
    InvokeAgent,
    InvokeTeam,
    SetDefaultAgentProfile,
    ApproveDelegation,
    RejectDelegation,
    ForkSession,
    SwitchActiveLeaf,
    SetSessionTreeLabel,
    SetSessionName,
    ExportCurrent,
    ExportCurrentHtml,
}

pub(crate) fn descriptor_for_internal_operation(operation: &Operation) -> OperationDescriptor {
    let contract = match operation {
        Operation::Prompt(_) => OperationContract::Prompt,
        Operation::ManualCompaction(_) => OperationContract::Compact,
        Operation::ApproveDelegationConfirmation { .. } => OperationContract::ApproveDelegation,
        Operation::RejectDelegationConfirmation { .. } => OperationContract::RejectDelegation,
        Operation::BranchSummary { .. } => OperationContract::BranchSummary,
        Operation::SelfHealingEdit(_) => OperationContract::SelfHealingEdit,
        Operation::AgentInvocation(_) => OperationContract::InvokeAgent,
        Operation::AgentTeam(_) => OperationContract::InvokeTeam,
        Operation::ForkSession { .. } => OperationContract::ForkSession,
        Operation::SwitchActiveLeaf { .. } => OperationContract::SwitchActiveLeaf,
        Operation::SetSessionTreeLabel { .. } => OperationContract::SetSessionTreeLabel,
        Operation::SetSessionName { .. } => OperationContract::SetSessionName,
        Operation::SetDefaultAgentProfile { .. } => OperationContract::SetDefaultAgentProfile,
        Operation::Export(options) if options.writes_html() => OperationContract::ExportCurrentHtml,
        Operation::Export(_) => OperationContract::ExportCurrent,
    };
    contract.descriptor()
}

pub(crate) fn descriptor_for_child_kind(kind: OperationKind) -> Option<OperationDescriptor> {
    let contract = match kind {
        OperationKind::Prompt => OperationContract::Prompt,
        OperationKind::AgentInvocation => OperationContract::InvokeAgent,
        OperationKind::AgentTeam => OperationContract::InvokeTeam,
        OperationKind::Compact
        | OperationKind::BranchSummary
        | OperationKind::SelfHealingEdit
        | OperationKind::DelegationConfirmation
        | OperationKind::ForkSession
        | OperationKind::SwitchActiveLeaf
        | OperationKind::SetSessionTreeLabel
        | OperationKind::SetSessionName
        | OperationKind::SetDefaultAgentProfile
        | OperationKind::Export => return None,
    };
    contract.descriptor().for_child()
}

#[cfg(test)]
pub(crate) fn descriptor_for_test_admission(
    kind: OperationKind,
    class: OperationClass,
    dispatch_mode: OperationDispatchMode,
) -> OperationDescriptor {
    let mut descriptor = match class {
        OperationClass::ReadOnly => OperationContract::ExportCurrent.descriptor(),
        OperationClass::SessionWriteRoot => OperationContract::Prompt.descriptor(),
        OperationClass::NonSessionRoot => OperationContract::InvokeAgent.descriptor(),
        OperationClass::RuntimeWrite => OperationContract::SetDefaultAgentProfile.descriptor(),
        OperationClass::Child => descriptor_for_child_kind(OperationKind::Prompt)
            .expect("prompt contract permits structured children"),
        OperationClass::Query => panic!("query intents do not create operation executions"),
    };
    descriptor.submitted_kind = kind;
    descriptor.dispatch_mode = dispatch_mode;
    descriptor.cancellation = match dispatch_mode {
        OperationDispatchMode::Async => OperationCancellation::Cancellable,
        OperationDispatchMode::SyncReadOnly | OperationDispatchMode::SyncMutable => {
            OperationCancellation::Atomic
        }
    };
    descriptor
}

impl OperationContract {
    fn descriptor(self) -> OperationDescriptor {
        let (
            submitted_kind,
            admission_class,
            dispatch_mode,
            outcome_family,
            terminal_policy,
            permitted_root_evidence,
        ) = match self {
            Self::Prompt => (
                OperationKind::Prompt,
                OperationClass::SessionWriteRoot,
                OperationDispatchMode::Async,
                OperationOutcomeFamily::Prompt,
                OperationTerminalPolicy::ProductEvent,
                PROMPT_ROOT_EVIDENCE,
            ),
            Self::Compact => (
                OperationKind::Compact,
                OperationClass::SessionWriteRoot,
                OperationDispatchMode::Async,
                OperationOutcomeFamily::Compact,
                OperationTerminalPolicy::ProductEvent,
                COMPACT_ROOT_EVIDENCE,
            ),
            Self::BranchSummary => (
                OperationKind::BranchSummary,
                OperationClass::SessionWriteRoot,
                OperationDispatchMode::Async,
                OperationOutcomeFamily::BranchSummary,
                OperationTerminalPolicy::OutcomeAcknowledgement,
                &[][..],
            ),
            Self::SelfHealingEdit => (
                OperationKind::SelfHealingEdit,
                OperationClass::SessionWriteRoot,
                OperationDispatchMode::Async,
                OperationOutcomeFamily::SelfHealingEdit,
                OperationTerminalPolicy::ProductEvent,
                SELF_HEALING_EDIT_ROOT_EVIDENCE,
            ),
            Self::InvokeAgent => (
                OperationKind::AgentInvocation,
                OperationClass::NonSessionRoot,
                OperationDispatchMode::Async,
                OperationOutcomeFamily::AgentInvocation,
                OperationTerminalPolicy::ProductEvent,
                AGENT_INVOCATION_ROOT_EVIDENCE,
            ),
            Self::InvokeTeam => (
                OperationKind::AgentTeam,
                OperationClass::NonSessionRoot,
                OperationDispatchMode::Async,
                OperationOutcomeFamily::AgentTeam,
                OperationTerminalPolicy::ProductEvent,
                AGENT_TEAM_ROOT_EVIDENCE,
            ),
            Self::SetDefaultAgentProfile => (
                OperationKind::SetDefaultAgentProfile,
                OperationClass::RuntimeWrite,
                OperationDispatchMode::SyncMutable,
                OperationOutcomeFamily::DefaultAgentProfileChanged,
                OperationTerminalPolicy::OutcomeAcknowledgement,
                &[][..],
            ),
            Self::ApproveDelegation => (
                OperationKind::DelegationConfirmation,
                OperationClass::SessionWriteRoot,
                OperationDispatchMode::Async,
                OperationOutcomeFamily::DelegationApproved,
                OperationTerminalPolicy::OutcomeAcknowledgement,
                &[][..],
            ),
            Self::RejectDelegation => (
                OperationKind::DelegationConfirmation,
                OperationClass::SessionWriteRoot,
                OperationDispatchMode::SyncMutable,
                OperationOutcomeFamily::DelegationRejected,
                OperationTerminalPolicy::OutcomeAcknowledgement,
                &[][..],
            ),
            Self::ForkSession => (
                OperationKind::ForkSession,
                OperationClass::SessionWriteRoot,
                OperationDispatchMode::SyncMutable,
                OperationOutcomeFamily::SessionForked,
                OperationTerminalPolicy::OutcomeAcknowledgement,
                &[][..],
            ),
            Self::SwitchActiveLeaf => (
                OperationKind::SwitchActiveLeaf,
                OperationClass::SessionWriteRoot,
                OperationDispatchMode::SyncMutable,
                OperationOutcomeFamily::ActiveLeafSwitched,
                OperationTerminalPolicy::OutcomeAcknowledgement,
                &[][..],
            ),
            Self::SetSessionTreeLabel => (
                OperationKind::SetSessionTreeLabel,
                OperationClass::SessionWriteRoot,
                OperationDispatchMode::SyncMutable,
                OperationOutcomeFamily::SessionTreeLabelChanged,
                OperationTerminalPolicy::OutcomeAcknowledgement,
                &[][..],
            ),
            Self::SetSessionName => (
                OperationKind::SetSessionName,
                OperationClass::SessionWriteRoot,
                OperationDispatchMode::SyncMutable,
                OperationOutcomeFamily::SessionNameChanged,
                OperationTerminalPolicy::OutcomeAcknowledgement,
                &[][..],
            ),
            Self::ExportCurrent => (
                OperationKind::Export,
                OperationClass::ReadOnly,
                OperationDispatchMode::SyncReadOnly,
                OperationOutcomeFamily::Export,
                OperationTerminalPolicy::OutcomeAcknowledgement,
                &[][..],
            ),
            Self::ExportCurrentHtml => (
                OperationKind::Export,
                OperationClass::ReadOnly,
                OperationDispatchMode::SyncReadOnly,
                OperationOutcomeFamily::ExportHtml,
                OperationTerminalPolicy::OutcomeAcknowledgement,
                &[][..],
            ),
        };
        let (session_access, runtime_access, capacity, durability) = match admission_class {
            OperationClass::SessionWriteRoot => (
                OperationSessionAccess::Write,
                OperationRuntimeAccess::None,
                OperationCapacity::SessionWriter,
                OperationDurability::SESSION,
            ),
            OperationClass::NonSessionRoot => (
                OperationSessionAccess::None,
                OperationRuntimeAccess::Read,
                OperationCapacity::BoundedRuntime,
                OperationDurability::NONE,
            ),
            OperationClass::RuntimeWrite => (
                OperationSessionAccess::Write,
                OperationRuntimeAccess::Write,
                OperationCapacity::RuntimeExclusive,
                OperationDurability::SESSION_AND_RUNTIME,
            ),
            OperationClass::ReadOnly => (
                OperationSessionAccess::Read,
                OperationRuntimeAccess::None,
                OperationCapacity::Shared,
                OperationDurability::NONE,
            ),
            OperationClass::Query | OperationClass::Child => {
                unreachable!("public root descriptor cannot use a dedicated intent class")
            }
        };
        let priority = match submitted_kind {
            OperationKind::Prompt | OperationKind::DelegationConfirmation => {
                OperationPriority::Interactive
            }
            _ => OperationPriority::Normal,
        };
        let cancellation = match dispatch_mode {
            OperationDispatchMode::Async => OperationCancellation::Cancellable,
            OperationDispatchMode::SyncReadOnly | OperationDispatchMode::SyncMutable => {
                OperationCancellation::Atomic
            }
        };
        let child_policy = match submitted_kind {
            OperationKind::Prompt | OperationKind::AgentInvocation | OperationKind::AgentTeam => {
                OperationChildPolicy::Structured
            }
            _ => OperationChildPolicy::Forbidden,
        };
        OperationDescriptor {
            revision: OPERATION_DESCRIPTOR_REVISION,
            submitted_kind,
            dispatch_mode,
            outcome_family,
            terminal_policy,
            permitted_root_evidence,
            lineage: OperationLineage::Root,
            session_access,
            runtime_access,
            priority,
            capacity,
            durability,
            cancellation,
            child_policy,
        }
    }
}

impl CodingAgentOperation {
    fn contract(&self) -> OperationContract {
        match self {
            Self::Prompt(_) => OperationContract::Prompt,
            Self::Compact(_) => OperationContract::Compact,
            Self::BranchSummary { .. } => OperationContract::BranchSummary,
            Self::SelfHealingEdit(_) => OperationContract::SelfHealingEdit,
            Self::InvokeAgent(_) => OperationContract::InvokeAgent,
            Self::InvokeTeam(_) => OperationContract::InvokeTeam,
            Self::SetDefaultAgentProfile { .. } => OperationContract::SetDefaultAgentProfile,
            Self::ApproveDelegation { .. } => OperationContract::ApproveDelegation,
            Self::RejectDelegation { .. } => OperationContract::RejectDelegation,
            Self::ForkSession { .. } => OperationContract::ForkSession,
            Self::SwitchActiveLeaf { .. } => OperationContract::SwitchActiveLeaf,
            Self::SetSessionTreeLabel { .. } => OperationContract::SetSessionTreeLabel,
            Self::SetSessionName { .. } => OperationContract::SetSessionName,
            Self::ExportCurrent => OperationContract::ExportCurrent,
            Self::ExportCurrentHtml(_) => OperationContract::ExportCurrentHtml,
        }
    }

    pub(crate) fn descriptor(&self) -> OperationDescriptor {
        self.contract().descriptor()
    }

    pub(crate) fn submission_fingerprint(&self) -> Option<(String, String)> {
        match self {
            Self::Prompt(options) => match options.invocation() {
                crate::app::bootstrap::PromptInvocation::Text(text) => Some((
                    "prompt".into(),
                    submission_payload_fingerprint(text.as_bytes()),
                )),
                crate::app::bootstrap::PromptInvocation::Content(content) => Some((
                    "prompt_content".into(),
                    submission_payload_fingerprint(
                        &serde_json::to_vec(content)
                            .expect("structured prompt content must serialize"),
                    ),
                )),
                _ => None,
            },
            _ => None,
        }
    }

    pub(crate) fn into_internal(self) -> Operation {
        match self {
            Self::Prompt(options) => Operation::Prompt(options),
            Self::Compact(options) => Operation::ManualCompaction(options),
            Self::BranchSummary {
                options,
                source_leaf_id,
                target_leaf_id,
                custom_instructions,
                reuse,
            } => Operation::BranchSummary {
                options,
                source_leaf_id,
                target_leaf_id,
                custom_instructions,
                reuse_existing: matches!(reuse, BranchSummaryReusePolicy::ReuseExisting),
            },
            Self::SelfHealingEdit(request) => Operation::SelfHealingEdit(request),
            Self::InvokeAgent(options) => Operation::AgentInvocation(options),
            Self::InvokeTeam(options) => Operation::AgentTeam(options),
            Self::SetDefaultAgentProfile { profile_id } => {
                Operation::SetDefaultAgentProfile { profile_id }
            }
            Self::ApproveDelegation {
                operation_id,
                tool_call_id,
            } => Operation::ApproveDelegationConfirmation {
                operation_id,
                tool_call_id,
            },
            Self::RejectDelegation {
                operation_id,
                tool_call_id,
                reason,
            } => Operation::RejectDelegationConfirmation {
                operation_id,
                tool_call_id,
                reason,
            },
            Self::ForkSession { target_leaf_id } => Operation::ForkSession { target_leaf_id },
            Self::SwitchActiveLeaf { target_leaf_id } => {
                Operation::SwitchActiveLeaf { target_leaf_id }
            }
            Self::SetSessionTreeLabel { entry_id, label } => {
                Operation::SetSessionTreeLabel { entry_id, label }
            }
            Self::SetSessionName { name } => Operation::SetSessionName { name },
            Self::ExportCurrent => Operation::Export(ExportOptions::view()),
            Self::ExportCurrentHtml(path) => Operation::Export(ExportOptions::html(path)),
        }
    }
}

pub(crate) fn prompt_text_submission_fingerprint(text: &str) -> String {
    submission_payload_fingerprint(text.as_bytes())
}

fn submission_payload_fingerprint(payload: &[u8]) -> String {
    format!("{:x}", Sha256::digest(payload))
}

#[allow(
    clippy::result_large_err,
    reason = "typed extractors intentionally return the intact mismatched outcome for diagnostics"
)]
impl CodingAgentOperationOutcome {
    pub(crate) fn from_internal(outcome: OperationOutcome) -> Self {
        match outcome {
            OperationOutcome::Prompt(outcome) => {
                Self::Prompt(PromptTurnOutcome::from_internal(outcome))
            }
            OperationOutcome::ManualCompaction(outcome) => {
                Self::Compact(PromptTurnOutcome::from_internal(outcome))
            }
            OperationOutcome::DelegationApproval => Self::DelegationApproved,
            OperationOutcome::DelegationRejection => Self::DelegationRejected,
            OperationOutcome::BranchSummary(outcome) => {
                Self::BranchSummary(PromptTurnOutcome::from_internal(outcome))
            }
            OperationOutcome::SelfHealingEdit(outcome) => Self::SelfHealingEdit(outcome),
            OperationOutcome::AgentInvocation(outcome) => Self::AgentInvocation(outcome),
            OperationOutcome::AgentTeam(outcome) => Self::AgentTeam(outcome),
            OperationOutcome::SetDefaultAgentProfile => Self::DefaultAgentProfileChanged,
            OperationOutcome::ForkSession => Self::SessionForked,
            OperationOutcome::SwitchActiveLeaf => Self::ActiveLeafSwitched,
            OperationOutcome::SessionTreeLabelChanged {
                entry_id,
                label,
                updated_at,
            } => Self::SessionTreeLabelChanged {
                entry_id,
                label,
                updated_at,
            },
            OperationOutcome::SessionNameChanged { name, updated_at } => {
                Self::SessionNameChanged { name, updated_at }
            }
            OperationOutcome::Export(outcome) => match outcome.path {
                Some(path) => Self::ExportHtml(path),
                None => Self::Export(outcome.export),
            },
        }
    }

    pub fn into_prompt(self) -> Result<PromptTurnOutcome, Self> {
        match self {
            Self::Prompt(outcome) => Ok(outcome),
            other => Err(other),
        }
    }

    pub fn into_compact(self) -> Result<PromptTurnOutcome, Self> {
        match self {
            Self::Compact(outcome) => Ok(outcome),
            other => Err(other),
        }
    }

    pub fn into_branch_summary(self) -> Result<PromptTurnOutcome, Self> {
        match self {
            Self::BranchSummary(outcome) => Ok(outcome),
            other => Err(other),
        }
    }

    pub fn into_self_healing_edit(self) -> Result<SelfHealingEditOutcome, Self> {
        match self {
            Self::SelfHealingEdit(outcome) => Ok(outcome),
            other => Err(other),
        }
    }

    pub fn into_agent_invocation(self) -> Result<AgentInvocationOutcome, Self> {
        match self {
            Self::AgentInvocation(outcome) => Ok(outcome),
            other => Err(other),
        }
    }

    pub fn into_agent_team(self) -> Result<AgentTeamOutcome, Self> {
        match self {
            Self::AgentTeam(outcome) => Ok(outcome),
            other => Err(other),
        }
    }

    #[cfg(test)]
    pub(crate) fn into_export(self) -> Result<CodingAgentSessionExport, Self> {
        match self {
            Self::Export(outcome) => Ok(outcome),
            other => Err(other),
        }
    }

    #[cfg(test)]
    pub(crate) fn into_export_html(self) -> Result<PathBuf, Self> {
        match self {
            Self::ExportHtml(path) => Ok(path),
            other => Err(other),
        }
    }

    pub fn into_delegation_approved(self) -> Result<(), Self> {
        match self {
            Self::DelegationApproved => Ok(()),
            other => Err(other),
        }
    }

    pub fn into_delegation_rejected(self) -> Result<(), Self> {
        match self {
            Self::DelegationRejected => Ok(()),
            other => Err(other),
        }
    }

    pub fn into_default_agent_profile_changed(self) -> Result<(), Self> {
        match self {
            Self::DefaultAgentProfileChanged => Ok(()),
            other => Err(other),
        }
    }

    pub fn into_session_forked(self) -> Result<(), Self> {
        match self {
            Self::SessionForked => Ok(()),
            other => Err(other),
        }
    }

    pub fn into_session_tree_label_changed(self) -> Result<(String, Option<String>, String), Self> {
        match self {
            Self::SessionTreeLabelChanged {
                entry_id,
                label,
                updated_at,
            } => Ok((entry_id, label, updated_at)),
            other => Err(other),
        }
    }

    pub fn into_session_name_changed(self) -> Result<(Option<String>, String), Self> {
        match self {
            Self::SessionNameChanged { name, updated_at } => Ok((name, updated_at)),
            other => Err(other),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;
    use crate::app::bootstrap::PromptInvocation;
    use crate::operations::export::runner::ExportOutcome;
    use crate::runtime::control::OperationKind;
    use crate::runtime::facade::context::CodingAgentSessionSummary;
    use crate::runtime::operation::OperationDispatchMode;
    use ai::api::conversation::AssistantMessage;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum ExpectedInternalOperationVariant {
        Prompt,
        ManualCompaction,
        BranchSummary,
        SelfHealingEdit,
        AgentInvocation,
        AgentTeam,
        SetDefaultAgentProfile,
        ApproveDelegationConfirmation,
        RejectDelegationConfirmation,
        ForkSession,
        SwitchActiveLeaf,
        SetSessionTreeLabel,
        SetSessionName,
        ExportView,
        ExportHtml,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    enum ExpectedPublicOutcomeFamily {
        Prompt,
        Compact,
        BranchSummary,
        SelfHealingEdit,
        AgentInvocation,
        AgentTeam,
        DefaultAgentProfileChanged,
        DelegationApproved,
        DelegationRejected,
        SessionForked,
        ActiveLeafSwitched,
        SessionTreeLabelChanged,
        SessionNameChanged,
        Export,
        ExportHtml,
    }

    struct OperationContractCase {
        public_variant: &'static str,
        build_operation: fn() -> CodingAgentOperation,
        expected_internal: ExpectedInternalOperationVariant,
        expected_dispatch: OperationDispatchMode,
        expected_outcome: ExpectedPublicOutcomeFamily,
        expected_submitted_kind: OperationKind,
        expected_terminal_policy: OperationTerminalPolicy,
        expected_root_evidence: &'static [OperationRootTerminalEvidence],
    }

    struct OutcomeProjectionCase {
        internal_outcome: &'static str,
        build_outcome: fn() -> OperationOutcome,
        expected_outcome: ExpectedPublicOutcomeFamily,
    }

    fn prompt_operation_options() -> PromptTurnOptions {
        PromptTurnOptions::new(PromptInvocation::Text("contract".into()))
    }

    fn operation_contract_cases() -> [OperationContractCase; 15] {
        [
            OperationContractCase {
                public_variant: "Prompt",
                build_operation: || CodingAgentOperation::Prompt(prompt_operation_options()),
                expected_internal: ExpectedInternalOperationVariant::Prompt,
                expected_dispatch: OperationDispatchMode::Async,
                expected_outcome: ExpectedPublicOutcomeFamily::Prompt,
                expected_submitted_kind: OperationKind::Prompt,
                expected_terminal_policy: OperationTerminalPolicy::ProductEvent,
                expected_root_evidence: &[
                    OperationRootTerminalEvidence::PromptCompleted,
                    OperationRootTerminalEvidence::PromptFailed,
                    OperationRootTerminalEvidence::PromptAborted,
                ],
            },
            OperationContractCase {
                public_variant: "Compact",
                build_operation: || CodingAgentOperation::Compact(prompt_operation_options()),
                expected_internal: ExpectedInternalOperationVariant::ManualCompaction,
                expected_dispatch: OperationDispatchMode::Async,
                expected_outcome: ExpectedPublicOutcomeFamily::Compact,
                expected_submitted_kind: OperationKind::Compact,
                expected_terminal_policy: OperationTerminalPolicy::ProductEvent,
                expected_root_evidence: &[
                    OperationRootTerminalEvidence::CompactionCompleted,
                    OperationRootTerminalEvidence::CompactPromptFailed,
                ],
            },
            OperationContractCase {
                public_variant: "BranchSummary",
                build_operation: || CodingAgentOperation::BranchSummary {
                    options: prompt_operation_options(),
                    source_leaf_id: "leaf_source".into(),
                    target_leaf_id: "leaf_target".into(),
                    custom_instructions: Some("contract instructions".into()),
                    reuse: BranchSummaryReusePolicy::ReuseExisting,
                },
                expected_internal: ExpectedInternalOperationVariant::BranchSummary,
                expected_dispatch: OperationDispatchMode::Async,
                expected_outcome: ExpectedPublicOutcomeFamily::BranchSummary,
                expected_submitted_kind: OperationKind::BranchSummary,
                expected_terminal_policy: OperationTerminalPolicy::OutcomeAcknowledgement,
                expected_root_evidence: &[],
            },
            OperationContractCase {
                public_variant: "SelfHealingEdit",
                build_operation: || {
                    CodingAgentOperation::SelfHealingEdit(SelfHealingEditRequest::new(
                        "src/lib.rs",
                        Vec::new(),
                    ))
                },
                expected_internal: ExpectedInternalOperationVariant::SelfHealingEdit,
                expected_dispatch: OperationDispatchMode::Async,
                expected_outcome: ExpectedPublicOutcomeFamily::SelfHealingEdit,
                expected_submitted_kind: OperationKind::SelfHealingEdit,
                expected_terminal_policy: OperationTerminalPolicy::ProductEvent,
                expected_root_evidence: &[
                    OperationRootTerminalEvidence::SelfHealingEditCompleted,
                    OperationRootTerminalEvidence::SelfHealingEditFailed,
                    OperationRootTerminalEvidence::SelfHealingEditAborted,
                ],
            },
            OperationContractCase {
                public_variant: "InvokeAgent",
                build_operation: || {
                    CodingAgentOperation::InvokeAgent(AgentInvocationOptions::new(
                        "agent",
                        "task",
                        prompt_operation_options(),
                    ))
                },
                expected_internal: ExpectedInternalOperationVariant::AgentInvocation,
                expected_dispatch: OperationDispatchMode::Async,
                expected_outcome: ExpectedPublicOutcomeFamily::AgentInvocation,
                expected_submitted_kind: OperationKind::AgentInvocation,
                expected_terminal_policy: OperationTerminalPolicy::ProductEvent,
                expected_root_evidence: &[
                    OperationRootTerminalEvidence::AgentInvocationCompleted,
                    OperationRootTerminalEvidence::AgentInvocationFailed,
                    OperationRootTerminalEvidence::AgentInvocationAborted,
                ],
            },
            OperationContractCase {
                public_variant: "InvokeTeam",
                build_operation: || {
                    CodingAgentOperation::InvokeTeam(AgentTeamOptions::new(
                        "team",
                        "task",
                        prompt_operation_options(),
                    ))
                },
                expected_internal: ExpectedInternalOperationVariant::AgentTeam,
                expected_dispatch: OperationDispatchMode::Async,
                expected_outcome: ExpectedPublicOutcomeFamily::AgentTeam,
                expected_submitted_kind: OperationKind::AgentTeam,
                expected_terminal_policy: OperationTerminalPolicy::ProductEvent,
                expected_root_evidence: &[
                    OperationRootTerminalEvidence::AgentTeamCompleted,
                    OperationRootTerminalEvidence::AgentTeamFailed,
                    OperationRootTerminalEvidence::AgentTeamAborted,
                ],
            },
            OperationContractCase {
                public_variant: "SetDefaultAgentProfile",
                build_operation: || CodingAgentOperation::SetDefaultAgentProfile {
                    profile_id: ProfileId::from("reviewer"),
                },
                expected_internal: ExpectedInternalOperationVariant::SetDefaultAgentProfile,
                expected_dispatch: OperationDispatchMode::SyncMutable,
                expected_outcome: ExpectedPublicOutcomeFamily::DefaultAgentProfileChanged,
                expected_submitted_kind: OperationKind::SetDefaultAgentProfile,
                expected_terminal_policy: OperationTerminalPolicy::OutcomeAcknowledgement,
                expected_root_evidence: &[],
            },
            OperationContractCase {
                public_variant: "ApproveDelegation",
                build_operation: || CodingAgentOperation::ApproveDelegation {
                    operation_id: "op_parent".into(),
                    tool_call_id: "tool_delegate".into(),
                },
                expected_internal: ExpectedInternalOperationVariant::ApproveDelegationConfirmation,
                expected_dispatch: OperationDispatchMode::Async,
                expected_outcome: ExpectedPublicOutcomeFamily::DelegationApproved,
                expected_submitted_kind: OperationKind::DelegationConfirmation,
                expected_terminal_policy: OperationTerminalPolicy::OutcomeAcknowledgement,
                expected_root_evidence: &[],
            },
            OperationContractCase {
                public_variant: "RejectDelegation",
                build_operation: || CodingAgentOperation::RejectDelegation {
                    operation_id: "op_parent".into(),
                    tool_call_id: "tool_delegate".into(),
                    reason: "not now".into(),
                },
                expected_internal: ExpectedInternalOperationVariant::RejectDelegationConfirmation,
                expected_dispatch: OperationDispatchMode::SyncMutable,
                expected_outcome: ExpectedPublicOutcomeFamily::DelegationRejected,
                expected_submitted_kind: OperationKind::DelegationConfirmation,
                expected_terminal_policy: OperationTerminalPolicy::OutcomeAcknowledgement,
                expected_root_evidence: &[],
            },
            OperationContractCase {
                public_variant: "ForkSession",
                build_operation: || CodingAgentOperation::ForkSession {
                    target_leaf_id: Some("leaf_target".into()),
                },
                expected_internal: ExpectedInternalOperationVariant::ForkSession,
                expected_dispatch: OperationDispatchMode::SyncMutable,
                expected_outcome: ExpectedPublicOutcomeFamily::SessionForked,
                expected_submitted_kind: OperationKind::ForkSession,
                expected_terminal_policy: OperationTerminalPolicy::OutcomeAcknowledgement,
                expected_root_evidence: &[],
            },
            OperationContractCase {
                public_variant: "SwitchActiveLeaf",
                build_operation: || CodingAgentOperation::SwitchActiveLeaf {
                    target_leaf_id: "leaf_target".into(),
                },
                expected_internal: ExpectedInternalOperationVariant::SwitchActiveLeaf,
                expected_dispatch: OperationDispatchMode::SyncMutable,
                expected_outcome: ExpectedPublicOutcomeFamily::ActiveLeafSwitched,
                expected_submitted_kind: OperationKind::SwitchActiveLeaf,
                expected_terminal_policy: OperationTerminalPolicy::OutcomeAcknowledgement,
                expected_root_evidence: &[],
            },
            OperationContractCase {
                public_variant: "SetSessionTreeLabel",
                build_operation: || CodingAgentOperation::SetSessionTreeLabel {
                    entry_id: "leaf_target".into(),
                    label: Some("checkpoint".into()),
                },
                expected_internal: ExpectedInternalOperationVariant::SetSessionTreeLabel,
                expected_dispatch: OperationDispatchMode::SyncMutable,
                expected_outcome: ExpectedPublicOutcomeFamily::SessionTreeLabelChanged,
                expected_submitted_kind: OperationKind::SetSessionTreeLabel,
                expected_terminal_policy: OperationTerminalPolicy::OutcomeAcknowledgement,
                expected_root_evidence: &[],
            },
            OperationContractCase {
                public_variant: "SetSessionName",
                build_operation: || CodingAgentOperation::SetSessionName {
                    name: Some("planning".into()),
                },
                expected_internal: ExpectedInternalOperationVariant::SetSessionName,
                expected_dispatch: OperationDispatchMode::SyncMutable,
                expected_outcome: ExpectedPublicOutcomeFamily::SessionNameChanged,
                expected_submitted_kind: OperationKind::SetSessionName,
                expected_terminal_policy: OperationTerminalPolicy::OutcomeAcknowledgement,
                expected_root_evidence: &[],
            },
            OperationContractCase {
                public_variant: "ExportCurrent",
                build_operation: || CodingAgentOperation::ExportCurrent,
                expected_internal: ExpectedInternalOperationVariant::ExportView,
                expected_dispatch: OperationDispatchMode::SyncReadOnly,
                expected_outcome: ExpectedPublicOutcomeFamily::Export,
                expected_submitted_kind: OperationKind::Export,
                expected_terminal_policy: OperationTerminalPolicy::OutcomeAcknowledgement,
                expected_root_evidence: &[],
            },
            OperationContractCase {
                public_variant: "ExportCurrentHtml",
                build_operation: || {
                    CodingAgentOperation::ExportCurrentHtml(PathBuf::from("session.html"))
                },
                expected_internal: ExpectedInternalOperationVariant::ExportHtml,
                expected_dispatch: OperationDispatchMode::SyncReadOnly,
                expected_outcome: ExpectedPublicOutcomeFamily::ExportHtml,
                expected_submitted_kind: OperationKind::Export,
                expected_terminal_policy: OperationTerminalPolicy::OutcomeAcknowledgement,
                expected_root_evidence: &[],
            },
        ]
    }

    fn internal_prompt_outcome_fixture() -> InternalPromptTurnOutcome {
        InternalPromptTurnOutcome::Aborted {
            operation_id: "op_contract".into(),
            turn_id: Some("turn_contract".into()),
            reason: "fixture".into(),
            session_id: Some("sess_contract".into()),
        }
    }

    fn self_healing_outcome_fixture() -> SelfHealingEditOutcome {
        SelfHealingEditOutcome {
            path: "src/lib.rs".into(),
            message: "updated".into(),
            diff: "diff".into(),
            patch: "patch".into(),
            first_changed_line: Some(1),
            attempts: 1,
            diagnostics: Vec::new(),
            check_output: None,
            repair_attempts: Vec::new(),
        }
    }

    fn agent_invocation_outcome_fixture() -> AgentInvocationOutcome {
        AgentInvocationOutcome {
            operation_id: "op_agent".into(),
            child_operation_id: "op_child".into(),
            turn_id: "turn_agent".into(),
            profile_id: ProfileId::from("agent"),
            final_text: "agent result".into(),
            final_message: AssistantMessage::empty("test", "test-model"),
            diagnostics: Vec::new(),
        }
    }

    fn agent_team_outcome_fixture() -> AgentTeamOutcome {
        AgentTeamOutcome {
            operation_id: "op_team".into(),
            team_id: ProfileId::from("team"),
            final_text: "team result".into(),
            member_results: Vec::new(),
            supervisor_result: None,
            diagnostics: Vec::new(),
        }
    }

    fn export_fixture() -> CodingAgentSessionExport {
        CodingAgentSessionExport {
            summary: CodingAgentSessionSummary {
                session_id: "sess_export".into(),
                name: None,
                session_dir: PathBuf::from("sessions/sess_export"),
                created_at: "2026-07-10T00:00:00Z".into(),
                updated_at: "2026-07-10T00:00:00Z".into(),
                active_leaf_id: None,
            },
            cwd: None,
            transcript: Vec::new(),
            diagnostics: Vec::new(),
        }
    }

    fn operation_outcome_projection_cases() -> [OutcomeProjectionCase; 15] {
        [
            OutcomeProjectionCase {
                internal_outcome: "Prompt",
                build_outcome: || OperationOutcome::Prompt(internal_prompt_outcome_fixture()),
                expected_outcome: ExpectedPublicOutcomeFamily::Prompt,
            },
            OutcomeProjectionCase {
                internal_outcome: "ManualCompaction",
                build_outcome: || {
                    OperationOutcome::ManualCompaction(internal_prompt_outcome_fixture())
                },
                expected_outcome: ExpectedPublicOutcomeFamily::Compact,
            },
            OutcomeProjectionCase {
                internal_outcome: "BranchSummary",
                build_outcome: || OperationOutcome::BranchSummary(internal_prompt_outcome_fixture()),
                expected_outcome: ExpectedPublicOutcomeFamily::BranchSummary,
            },
            OutcomeProjectionCase {
                internal_outcome: "SelfHealingEdit",
                build_outcome: || OperationOutcome::SelfHealingEdit(self_healing_outcome_fixture()),
                expected_outcome: ExpectedPublicOutcomeFamily::SelfHealingEdit,
            },
            OutcomeProjectionCase {
                internal_outcome: "AgentInvocation",
                build_outcome: || {
                    OperationOutcome::AgentInvocation(agent_invocation_outcome_fixture())
                },
                expected_outcome: ExpectedPublicOutcomeFamily::AgentInvocation,
            },
            OutcomeProjectionCase {
                internal_outcome: "AgentTeam",
                build_outcome: || OperationOutcome::AgentTeam(agent_team_outcome_fixture()),
                expected_outcome: ExpectedPublicOutcomeFamily::AgentTeam,
            },
            OutcomeProjectionCase {
                internal_outcome: "SetDefaultAgentProfile",
                build_outcome: || OperationOutcome::SetDefaultAgentProfile,
                expected_outcome: ExpectedPublicOutcomeFamily::DefaultAgentProfileChanged,
            },
            OutcomeProjectionCase {
                internal_outcome: "DelegationApproval",
                build_outcome: || OperationOutcome::DelegationApproval,
                expected_outcome: ExpectedPublicOutcomeFamily::DelegationApproved,
            },
            OutcomeProjectionCase {
                internal_outcome: "DelegationRejection",
                build_outcome: || OperationOutcome::DelegationRejection,
                expected_outcome: ExpectedPublicOutcomeFamily::DelegationRejected,
            },
            OutcomeProjectionCase {
                internal_outcome: "ForkSession",
                build_outcome: || OperationOutcome::ForkSession,
                expected_outcome: ExpectedPublicOutcomeFamily::SessionForked,
            },
            OutcomeProjectionCase {
                internal_outcome: "SwitchActiveLeaf",
                build_outcome: || OperationOutcome::SwitchActiveLeaf,
                expected_outcome: ExpectedPublicOutcomeFamily::ActiveLeafSwitched,
            },
            OutcomeProjectionCase {
                internal_outcome: "SessionTreeLabelChanged",
                build_outcome: || OperationOutcome::SessionTreeLabelChanged {
                    entry_id: "leaf_target".into(),
                    label: Some("checkpoint".into()),
                    updated_at: "2026-07-16T00:00:00Z".into(),
                },
                expected_outcome: ExpectedPublicOutcomeFamily::SessionTreeLabelChanged,
            },
            OutcomeProjectionCase {
                internal_outcome: "SessionNameChanged",
                build_outcome: || OperationOutcome::SessionNameChanged {
                    name: Some("planning".into()),
                    updated_at: "2026-07-16T00:00:00Z".into(),
                },
                expected_outcome: ExpectedPublicOutcomeFamily::SessionNameChanged,
            },
            OutcomeProjectionCase {
                internal_outcome: "Export(view)",
                build_outcome: || {
                    OperationOutcome::Export(ExportOutcome {
                        export: export_fixture(),
                        path: None,
                    })
                },
                expected_outcome: ExpectedPublicOutcomeFamily::Export,
            },
            OutcomeProjectionCase {
                internal_outcome: "Export(html)",
                build_outcome: || {
                    OperationOutcome::Export(ExportOutcome {
                        export: export_fixture(),
                        path: Some(PathBuf::from("session.html")),
                    })
                },
                expected_outcome: ExpectedPublicOutcomeFamily::ExportHtml,
            },
        ]
    }

    fn internal_operation_variant(operation: &Operation) -> ExpectedInternalOperationVariant {
        match operation {
            Operation::Prompt(_) => ExpectedInternalOperationVariant::Prompt,
            Operation::ManualCompaction(_) => ExpectedInternalOperationVariant::ManualCompaction,
            Operation::ApproveDelegationConfirmation { .. } => {
                ExpectedInternalOperationVariant::ApproveDelegationConfirmation
            }
            Operation::RejectDelegationConfirmation { .. } => {
                ExpectedInternalOperationVariant::RejectDelegationConfirmation
            }
            Operation::BranchSummary { .. } => ExpectedInternalOperationVariant::BranchSummary,
            Operation::SelfHealingEdit(_) => ExpectedInternalOperationVariant::SelfHealingEdit,
            Operation::AgentInvocation(_) => ExpectedInternalOperationVariant::AgentInvocation,
            Operation::AgentTeam(_) => ExpectedInternalOperationVariant::AgentTeam,
            Operation::ForkSession { .. } => ExpectedInternalOperationVariant::ForkSession,
            Operation::SwitchActiveLeaf { .. } => {
                ExpectedInternalOperationVariant::SwitchActiveLeaf
            }
            Operation::SetSessionTreeLabel { .. } => {
                ExpectedInternalOperationVariant::SetSessionTreeLabel
            }
            Operation::SetSessionName { .. } => ExpectedInternalOperationVariant::SetSessionName,
            Operation::SetDefaultAgentProfile { .. } => {
                ExpectedInternalOperationVariant::SetDefaultAgentProfile
            }
            Operation::Export(options) => {
                if options == &ExportOptions::view() {
                    ExpectedInternalOperationVariant::ExportView
                } else if options == &ExportOptions::html("session.html") {
                    ExpectedInternalOperationVariant::ExportHtml
                } else {
                    panic!("unexpected export options in operation contract: {options:?}")
                }
            }
        }
    }

    fn public_outcome_family(outcome: &CodingAgentOperationOutcome) -> ExpectedPublicOutcomeFamily {
        match outcome {
            CodingAgentOperationOutcome::Prompt(_) => ExpectedPublicOutcomeFamily::Prompt,
            CodingAgentOperationOutcome::Compact(_) => ExpectedPublicOutcomeFamily::Compact,
            CodingAgentOperationOutcome::BranchSummary(_) => {
                ExpectedPublicOutcomeFamily::BranchSummary
            }
            CodingAgentOperationOutcome::SelfHealingEdit(_) => {
                ExpectedPublicOutcomeFamily::SelfHealingEdit
            }
            CodingAgentOperationOutcome::AgentInvocation(_) => {
                ExpectedPublicOutcomeFamily::AgentInvocation
            }
            CodingAgentOperationOutcome::AgentTeam(_) => ExpectedPublicOutcomeFamily::AgentTeam,
            CodingAgentOperationOutcome::DefaultAgentProfileChanged => {
                ExpectedPublicOutcomeFamily::DefaultAgentProfileChanged
            }
            CodingAgentOperationOutcome::DelegationApproved => {
                ExpectedPublicOutcomeFamily::DelegationApproved
            }
            CodingAgentOperationOutcome::DelegationRejected => {
                ExpectedPublicOutcomeFamily::DelegationRejected
            }
            CodingAgentOperationOutcome::SessionForked => {
                ExpectedPublicOutcomeFamily::SessionForked
            }
            CodingAgentOperationOutcome::ActiveLeafSwitched => {
                ExpectedPublicOutcomeFamily::ActiveLeafSwitched
            }
            CodingAgentOperationOutcome::SessionTreeLabelChanged { .. } => {
                ExpectedPublicOutcomeFamily::SessionTreeLabelChanged
            }
            CodingAgentOperationOutcome::SessionNameChanged { .. } => {
                ExpectedPublicOutcomeFamily::SessionNameChanged
            }
            CodingAgentOperationOutcome::Export(_) => ExpectedPublicOutcomeFamily::Export,
            CodingAgentOperationOutcome::ExportHtml(_) => ExpectedPublicOutcomeFamily::ExportHtml,
        }
    }

    fn descriptor_outcome_family(family: ExpectedPublicOutcomeFamily) -> OperationOutcomeFamily {
        match family {
            ExpectedPublicOutcomeFamily::Prompt => OperationOutcomeFamily::Prompt,
            ExpectedPublicOutcomeFamily::Compact => OperationOutcomeFamily::Compact,
            ExpectedPublicOutcomeFamily::BranchSummary => OperationOutcomeFamily::BranchSummary,
            ExpectedPublicOutcomeFamily::SelfHealingEdit => OperationOutcomeFamily::SelfHealingEdit,
            ExpectedPublicOutcomeFamily::AgentInvocation => OperationOutcomeFamily::AgentInvocation,
            ExpectedPublicOutcomeFamily::AgentTeam => OperationOutcomeFamily::AgentTeam,
            ExpectedPublicOutcomeFamily::DefaultAgentProfileChanged => {
                OperationOutcomeFamily::DefaultAgentProfileChanged
            }
            ExpectedPublicOutcomeFamily::DelegationApproved => {
                OperationOutcomeFamily::DelegationApproved
            }
            ExpectedPublicOutcomeFamily::DelegationRejected => {
                OperationOutcomeFamily::DelegationRejected
            }
            ExpectedPublicOutcomeFamily::SessionForked => OperationOutcomeFamily::SessionForked,
            ExpectedPublicOutcomeFamily::ActiveLeafSwitched => {
                OperationOutcomeFamily::ActiveLeafSwitched
            }
            ExpectedPublicOutcomeFamily::SessionTreeLabelChanged => {
                OperationOutcomeFamily::SessionTreeLabelChanged
            }
            ExpectedPublicOutcomeFamily::SessionNameChanged => {
                OperationOutcomeFamily::SessionNameChanged
            }
            ExpectedPublicOutcomeFamily::Export => OperationOutcomeFamily::Export,
            ExpectedPublicOutcomeFamily::ExportHtml => OperationOutcomeFamily::ExportHtml,
        }
    }

    fn branch_summary_reuse_flag(reuse: BranchSummaryReusePolicy) -> bool {
        let operation = CodingAgentOperation::BranchSummary {
            options: PromptTurnOptions::new(PromptInvocation::Text("summarize".into())),
            source_leaf_id: "leaf_source".into(),
            target_leaf_id: "leaf_target".into(),
            custom_instructions: None,
            reuse,
        }
        .into_internal();

        let Operation::BranchSummary { reuse_existing, .. } = operation else {
            panic!("branch summary should map to the internal branch-summary operation")
        };
        reuse_existing
    }

    #[test]
    fn branch_summary_reuse_policy_maps_to_internal_flag() {
        assert!(!branch_summary_reuse_flag(
            BranchSummaryReusePolicy::AlwaysCreate
        ));
        assert!(branch_summary_reuse_flag(
            BranchSummaryReusePolicy::ReuseExisting
        ));
    }

    #[test]
    fn operation_contract_covers_all_public_variants() {
        let cases = operation_contract_cases();

        assert_eq!(cases.len(), 15);
        assert_eq!(
            cases
                .iter()
                .map(|case| case.expected_outcome)
                .collect::<HashSet<_>>()
                .len(),
            15
        );
        for case in &cases {
            let operation = (case.build_operation)().into_internal();
            let internal_descriptor = operation.descriptor();
            assert_eq!(
                internal_operation_variant(&operation),
                case.expected_internal,
                "{} internal variant",
                case.public_variant
            );
            assert_eq!(
                internal_descriptor.dispatch_mode, case.expected_dispatch,
                "{} dispatch mode",
                case.public_variant
            );
            let descriptor = (case.build_operation)().descriptor();
            assert_eq!(descriptor, internal_descriptor);
            if let Some(static_kind) = operation.static_kind() {
                assert_eq!(descriptor.submitted_kind, static_kind);
            } else {
                assert_eq!(
                    descriptor.submitted_kind,
                    OperationKind::DelegationConfirmation,
                    "{} dynamic kind",
                    case.public_variant
                );
            }
        }
    }

    #[test]
    fn terminal_policy_matrix_classifies_all_public_operations_exactly_once() {
        let cases = operation_contract_cases();
        let mut public_variants = HashSet::new();
        let mut terminal_associated = 0;
        let mut outcome_only = 0;

        for case in &cases {
            assert!(
                public_variants.insert(case.public_variant),
                "duplicate public operation row: {}",
                case.public_variant
            );
            let operation = (case.build_operation)();
            let descriptor = operation.descriptor();
            assert_eq!(descriptor.submitted_kind, case.expected_submitted_kind);
            assert_eq!(
                descriptor.outcome_family,
                descriptor_outcome_family(case.expected_outcome)
            );
            assert_eq!(descriptor.terminal_policy, case.expected_terminal_policy);
            assert_eq!(descriptor.validate(), Ok(()));
            assert_eq!(
                descriptor.permitted_root_evidence, case.expected_root_evidence,
                "{} root evidence",
                case.public_variant
            );
            match descriptor.terminal_policy {
                OperationTerminalPolicy::ProductEvent => terminal_associated += 1,
                OperationTerminalPolicy::OutcomeAcknowledgement => outcome_only += 1,
            }
        }

        assert_eq!(cases.len(), 15);
        assert_eq!(public_variants.len(), 15);
        assert_eq!(terminal_associated, 5);
        assert_eq!(outcome_only, 10);
    }

    #[test]
    fn descriptor_claim_matrix_is_exhaustive_and_orthogonal() {
        let descriptors = operation_contract_cases()
            .into_iter()
            .map(|case| (case.build_operation)().descriptor())
            .collect::<Vec<_>>();

        assert_eq!(descriptors.len(), 15);
        assert!(
            descriptors
                .iter()
                .all(|descriptor| descriptor.validate().is_ok())
        );
        assert_eq!(
            descriptors
                .iter()
                .filter(|descriptor| {
                    descriptor.admission_class() == OperationClass::SessionWriteRoot
                })
                .count(),
            10
        );
        assert_eq!(
            descriptors
                .iter()
                .filter(|descriptor| descriptor.admission_class() == OperationClass::RuntimeWrite)
                .count(),
            1
        );
        assert_eq!(
            descriptors
                .iter()
                .filter(|descriptor| {
                    descriptor.admission_class() == OperationClass::NonSessionRoot
                })
                .count(),
            2
        );
        assert_eq!(
            descriptors
                .iter()
                .filter(|descriptor| descriptor.admission_class() == OperationClass::ReadOnly)
                .count(),
            2
        );
        assert_eq!(
            descriptors
                .iter()
                .filter(|descriptor| descriptor.priority == OperationPriority::Interactive)
                .count(),
            3
        );
        assert_eq!(
            descriptors
                .iter()
                .filter(|descriptor| descriptor.child_policy == OperationChildPolicy::Structured)
                .count(),
            3
        );
        assert_eq!(
            descriptors
                .iter()
                .filter(|descriptor| descriptor.cancellation == OperationCancellation::Cancellable)
                .count(),
            7
        );
        assert_eq!(
            descriptors
                .iter()
                .filter(|descriptor| descriptor.durability.session_if_persistent)
                .count(),
            11
        );
        assert_eq!(
            descriptors
                .iter()
                .filter(|descriptor| descriptor.durability.runtime_generation)
                .count(),
            1
        );
    }

    #[test]
    fn terminal_policy_validation_rejects_implicit_or_conflicting_contracts() {
        let mut descriptor = CodingAgentOperation::Prompt(prompt_operation_options()).descriptor();
        descriptor.permitted_root_evidence = &[];
        assert_eq!(
            descriptor.validate_terminal_policy(),
            Err("ProductEvent terminal policy requires root terminal evidence")
        );

        let mut descriptor = CodingAgentOperation::ExportCurrent.descriptor();
        descriptor.permitted_root_evidence = PROMPT_ROOT_EVIDENCE;
        assert_eq!(
            descriptor.validate_terminal_policy(),
            Err("outcome acknowledgement policy forbids root terminal evidence")
        );
    }

    #[test]
    fn descriptor_claim_validation_rejects_non_derivable_or_conflicting_claims() {
        let mut descriptor = CodingAgentOperation::Prompt(prompt_operation_options()).descriptor();
        descriptor.capacity = OperationCapacity::BoundedRuntime;
        assert_eq!(
            descriptor.validate(),
            Err("operation access and capacity claims do not derive a valid class")
        );

        let mut descriptor = CodingAgentOperation::ExportCurrent.descriptor();
        descriptor.durability.session_if_persistent = true;
        assert_eq!(
            descriptor.validate(),
            Err("session durability requires session write access")
        );

        let mut descriptor = CodingAgentOperation::Prompt(prompt_operation_options()).descriptor();
        descriptor.durability.runtime_generation = true;
        assert_eq!(
            descriptor.validate(),
            Err("runtime generation durability requires runtime write access")
        );
    }

    #[test]
    fn terminal_event_contract_matches_every_operation_descriptor() {
        const ALL_EVIDENCE: &[OperationRootTerminalEvidence] = &[
            OperationRootTerminalEvidence::PromptCompleted,
            OperationRootTerminalEvidence::PromptFailed,
            OperationRootTerminalEvidence::PromptAborted,
            OperationRootTerminalEvidence::CompactionCompleted,
            OperationRootTerminalEvidence::CompactPromptFailed,
            OperationRootTerminalEvidence::SelfHealingEditCompleted,
            OperationRootTerminalEvidence::SelfHealingEditFailed,
            OperationRootTerminalEvidence::SelfHealingEditAborted,
            OperationRootTerminalEvidence::AgentInvocationCompleted,
            OperationRootTerminalEvidence::AgentInvocationFailed,
            OperationRootTerminalEvidence::AgentInvocationAborted,
            OperationRootTerminalEvidence::AgentTeamCompleted,
            OperationRootTerminalEvidence::AgentTeamFailed,
            OperationRootTerminalEvidence::AgentTeamAborted,
        ];

        for case in operation_contract_cases() {
            let descriptor = (case.build_operation)().descriptor();
            for evidence in ALL_EVIDENCE {
                let terminal = product_terminal_operation(
                    descriptor.submitted_kind,
                    *evidence,
                    CodingAgentProductEventTerminalStatus::Completed,
                );
                assert_eq!(
                    terminal.is_some(),
                    descriptor.permitted_root_evidence.contains(evidence),
                    "{} evidence {evidence:?}",
                    case.public_variant
                );
                if let Some(terminal) = terminal {
                    assert_eq!(
                        Some(terminal.kind),
                        terminal_operation_kind(descriptor.submitted_kind),
                        "{} terminal kind",
                        case.public_variant
                    );
                }
            }
        }
    }

    #[test]
    fn branch_summary_and_self_healing_edit_have_explicit_lifecycle_policies() {
        let branch = operation_contract_cases()
            .into_iter()
            .find(|case| case.public_variant == "BranchSummary")
            .expect("branch summary contract");
        let edit = operation_contract_cases()
            .into_iter()
            .find(|case| case.public_variant == "SelfHealingEdit")
            .expect("self-healing edit contract");
        assert_eq!(
            branch.expected_terminal_policy,
            OperationTerminalPolicy::OutcomeAcknowledgement
        );
        assert!(branch.expected_root_evidence.is_empty());
        assert_eq!(
            edit.expected_terminal_policy,
            OperationTerminalPolicy::ProductEvent
        );
        assert_eq!(
            edit.expected_root_evidence,
            &[
                OperationRootTerminalEvidence::SelfHealingEditCompleted,
                OperationRootTerminalEvidence::SelfHealingEditFailed,
                OperationRootTerminalEvidence::SelfHealingEditAborted,
            ]
        );
    }

    #[test]
    fn recovery_terminal_contract_covers_durable_operation_families() {
        let cases = [
            (
                OperationKind::Prompt,
                CodingAgentProductEventTerminalOperationKind::Prompt,
            ),
            (
                OperationKind::Compact,
                CodingAgentProductEventTerminalOperationKind::Compact,
            ),
            (
                OperationKind::BranchSummary,
                CodingAgentProductEventTerminalOperationKind::BranchSummary,
            ),
            (
                OperationKind::SelfHealingEdit,
                CodingAgentProductEventTerminalOperationKind::SelfHealingEdit,
            ),
            (
                OperationKind::Export,
                CodingAgentProductEventTerminalOperationKind::Export,
            ),
        ];
        for (operation, expected) in cases {
            let terminal = recovered_product_terminal_operation(operation).unwrap();
            assert_eq!(terminal.kind, expected);
            assert_eq!(
                terminal.status,
                CodingAgentProductEventTerminalStatus::Recovered
            );
        }
    }

    #[test]
    fn html_export_outcome_projects_to_public_path() {
        let path = PathBuf::from("session.html");
        let export = CodingAgentSessionExport {
            summary: CodingAgentSessionSummary {
                session_id: "sess_export".into(),
                name: None,
                session_dir: PathBuf::from("sessions/sess_export"),
                created_at: "2026-07-10T00:00:00Z".into(),
                updated_at: "2026-07-10T00:00:00Z".into(),
                active_leaf_id: None,
            },
            cwd: None,
            transcript: Vec::new(),
            diagnostics: Vec::new(),
        };

        let outcome =
            CodingAgentOperationOutcome::from_internal(OperationOutcome::Export(ExportOutcome {
                export,
                path: Some(path.clone()),
            }));

        assert!(matches!(
            outcome,
            CodingAgentOperationOutcome::ExportHtml(projected) if projected == path
        ));
    }

    #[test]
    fn operation_outcome_projection_covers_all_families() {
        let cases = operation_outcome_projection_cases();
        let contract_outcomes = operation_contract_cases()
            .iter()
            .map(|case| case.expected_outcome)
            .collect::<HashSet<_>>();
        let projection_outcomes = cases
            .iter()
            .map(|case| case.expected_outcome)
            .collect::<HashSet<_>>();

        assert_eq!(cases.len(), 15);
        assert_eq!(projection_outcomes, contract_outcomes);
        for case in cases {
            let projected = CodingAgentOperationOutcome::from_internal((case.build_outcome)());
            assert_eq!(
                public_outcome_family(&projected),
                case.expected_outcome,
                "{} projection",
                case.internal_outcome
            );
        }
    }
}
