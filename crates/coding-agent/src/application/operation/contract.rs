use std::path::PathBuf;

use sha2::{Digest, Sha256};

use super::OperationOutcome;
use crate::events::{
    CodingAgentProductEventTerminalOperation, CodingAgentProductEventTerminalOperationKind,
    CodingAgentProductEventTerminalStatus,
};
use crate::kernel::capability::SessionCapabilityAccess;
pub(crate) use crate::kernel::operation::{
    OPERATION_DESCRIPTOR_REVISION, OperationCancellation, OperationCapacity, OperationChildPolicy,
    OperationDescriptor, OperationDurability, OperationLineage, OperationOutcomeFamily,
    OperationPriority, OperationRootTerminalEvidence, OperationRuntimeAccess,
    OperationSessionAccess, OperationTerminalPolicy,
};
use crate::kernel::operation::{OperationClass, OperationDispatchMode, OperationKind};
use crate::operations::agent_invocation::runner::{AgentInvocationOptions, AgentInvocationOutcome};
use crate::operations::export::CodingAgentSessionExport;
use crate::operations::export::runner::ExportOptions;
use crate::operations::prompt::context::{
    InternalPromptTurnOutcome, PromptTurnOptions, RuntimeSnapshot,
};
use crate::operations::self_healing_edit::runner::{
    SelfHealingEditOutcome, SelfHealingEditRequest,
};
use crate::operations::team_invocation::runner::{AgentTeamOptions, AgentTeamOutcome};
use crate::public_error::{
    CodingAgentPublicDiagnostic, CodingAgentPublicError, safe_public_summary,
};
use ai_protocol::api::conversation::AssistantMessage;

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
    /// List reviewable child worktree proposals for the current workspace.
    ListMergeProposals,
    /// Apply a `MergePending` child worktree's changes into the parent workspace.
    MergeChildWorktree {
        worktree_id: String,
    },
    /// Discard a `MergePending` child worktree without merging it.
    DiscardChildWorktree {
        worktree_id: String,
    },
}

#[derive(Debug)]
pub enum CodingAgentOperationOutcome {
    Prompt(PromptTurnOutcome),
    Compact(PromptTurnOutcome),
    BranchSummary(PromptTurnOutcome),
    SelfHealingEdit(SelfHealingEditOutcome),
    AgentInvocation(AgentInvocationOutcome),
    AgentTeam(AgentTeamOutcome),
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
    MergeApplied {
        worktree_id: String,
        applied: usize,
    },
    WorktreeDiscarded {
        worktree_id: String,
    },
    MergeProposals(Vec<crate::events::CodingAgentMergeProposal>),
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
        | OperationKind::Export
        | OperationKind::ListMergeProposals
        | OperationKind::MergeChildWorktree
        | OperationKind::DiscardChildWorktree => return None,
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
        | OperationKind::Export
        | OperationKind::ListMergeProposals
        | OperationKind::MergeChildWorktree
        | OperationKind::DiscardChildWorktree => {
            unreachable!("non-terminal operation kind filtered above")
        }
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
        | OperationKind::Export
        | OperationKind::ListMergeProposals
        | OperationKind::MergeChildWorktree
        | OperationKind::DiscardChildWorktree => None,
    }
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
        | OperationKind::ListMergeProposals => return None,
        OperationKind::MergeChildWorktree => {
            CodingAgentProductEventTerminalOperationKind::MergeChildWorktree
        }
        OperationKind::DiscardChildWorktree => {
            CodingAgentProductEventTerminalOperationKind::DiscardChildWorktree
        }
    })
}

/// Static seed for a submitted operation's lifecycle descriptor.
///
/// This is data, not a second operation enum: the public operation remains the
/// only authoritative variant set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct OperationContract {
    submitted_kind: OperationKind,
    admission_class: OperationClass,
    dispatch_mode: OperationDispatchMode,
    outcome_family: OperationOutcomeFamily,
    terminal_policy: OperationTerminalPolicy,
    permitted_root_evidence: &'static [OperationRootTerminalEvidence],
}

pub(crate) fn descriptor_for_child_kind(kind: OperationKind) -> Option<OperationDescriptor> {
    let contract = match kind {
        OperationKind::Prompt => OperationContract::PROMPT,
        OperationKind::AgentInvocation => OperationContract::INVOKE_AGENT,
        OperationKind::AgentTeam => OperationContract::INVOKE_TEAM,
        OperationKind::Compact
        | OperationKind::BranchSummary
        | OperationKind::SelfHealingEdit
        | OperationKind::DelegationConfirmation
        | OperationKind::ForkSession
        | OperationKind::SwitchActiveLeaf
        | OperationKind::SetSessionTreeLabel
        | OperationKind::SetSessionName
        | OperationKind::Export
        | OperationKind::ListMergeProposals
        | OperationKind::MergeChildWorktree
        | OperationKind::DiscardChildWorktree => return None,
    };
    contract.descriptor().for_child()
}

impl OperationContract {
    const fn new(
        submitted_kind: OperationKind,
        admission_class: OperationClass,
        dispatch_mode: OperationDispatchMode,
        outcome_family: OperationOutcomeFamily,
        terminal_policy: OperationTerminalPolicy,
        permitted_root_evidence: &'static [OperationRootTerminalEvidence],
    ) -> Self {
        Self {
            submitted_kind,
            admission_class,
            dispatch_mode,
            outcome_family,
            terminal_policy,
            permitted_root_evidence,
        }
    }

    const PROMPT: Self = Self::new(
        OperationKind::Prompt,
        OperationClass::SessionWriteRoot,
        OperationDispatchMode::Async,
        OperationOutcomeFamily::Prompt,
        OperationTerminalPolicy::ProductEvent,
        PROMPT_ROOT_EVIDENCE,
    );
    const COMPACT: Self = Self::new(
        OperationKind::Compact,
        OperationClass::SessionWriteRoot,
        OperationDispatchMode::Async,
        OperationOutcomeFamily::Compact,
        OperationTerminalPolicy::ProductEvent,
        COMPACT_ROOT_EVIDENCE,
    );
    const BRANCH_SUMMARY: Self = Self::new(
        OperationKind::BranchSummary,
        OperationClass::SessionWriteRoot,
        OperationDispatchMode::Async,
        OperationOutcomeFamily::BranchSummary,
        OperationTerminalPolicy::OutcomeAcknowledgement,
        &[],
    );
    const SELF_HEALING_EDIT: Self = Self::new(
        OperationKind::SelfHealingEdit,
        OperationClass::SessionWriteRoot,
        OperationDispatchMode::Async,
        OperationOutcomeFamily::SelfHealingEdit,
        OperationTerminalPolicy::ProductEvent,
        SELF_HEALING_EDIT_ROOT_EVIDENCE,
    );
    const INVOKE_AGENT: Self = Self::new(
        OperationKind::AgentInvocation,
        OperationClass::NonSessionRoot,
        OperationDispatchMode::Async,
        OperationOutcomeFamily::AgentInvocation,
        OperationTerminalPolicy::ProductEvent,
        AGENT_INVOCATION_ROOT_EVIDENCE,
    );
    const INVOKE_TEAM: Self = Self::new(
        OperationKind::AgentTeam,
        OperationClass::NonSessionRoot,
        OperationDispatchMode::Async,
        OperationOutcomeFamily::AgentTeam,
        OperationTerminalPolicy::ProductEvent,
        AGENT_TEAM_ROOT_EVIDENCE,
    );
    const APPROVE_DELEGATION: Self = Self::new(
        OperationKind::DelegationConfirmation,
        OperationClass::SessionWriteRoot,
        OperationDispatchMode::Async,
        OperationOutcomeFamily::DelegationApproved,
        OperationTerminalPolicy::OutcomeAcknowledgement,
        &[],
    );
    const REJECT_DELEGATION: Self = Self::new(
        OperationKind::DelegationConfirmation,
        OperationClass::SessionWriteRoot,
        OperationDispatchMode::SyncMutable,
        OperationOutcomeFamily::DelegationRejected,
        OperationTerminalPolicy::OutcomeAcknowledgement,
        &[],
    );
    const FORK_SESSION: Self = Self::new(
        OperationKind::ForkSession,
        OperationClass::SessionWriteRoot,
        OperationDispatchMode::SyncMutable,
        OperationOutcomeFamily::SessionForked,
        OperationTerminalPolicy::OutcomeAcknowledgement,
        &[],
    );
    const SWITCH_ACTIVE_LEAF: Self = Self::new(
        OperationKind::SwitchActiveLeaf,
        OperationClass::SessionWriteRoot,
        OperationDispatchMode::SyncMutable,
        OperationOutcomeFamily::ActiveLeafSwitched,
        OperationTerminalPolicy::OutcomeAcknowledgement,
        &[],
    );
    const SET_SESSION_TREE_LABEL: Self = Self::new(
        OperationKind::SetSessionTreeLabel,
        OperationClass::SessionWriteRoot,
        OperationDispatchMode::SyncMutable,
        OperationOutcomeFamily::SessionTreeLabelChanged,
        OperationTerminalPolicy::OutcomeAcknowledgement,
        &[],
    );
    const SET_SESSION_NAME: Self = Self::new(
        OperationKind::SetSessionName,
        OperationClass::SessionWriteRoot,
        OperationDispatchMode::SyncMutable,
        OperationOutcomeFamily::SessionNameChanged,
        OperationTerminalPolicy::OutcomeAcknowledgement,
        &[],
    );
    const EXPORT_CURRENT: Self = Self::new(
        OperationKind::Export,
        OperationClass::ReadOnly,
        OperationDispatchMode::SyncReadOnly,
        OperationOutcomeFamily::Export,
        OperationTerminalPolicy::OutcomeAcknowledgement,
        &[],
    );
    const EXPORT_CURRENT_HTML: Self = Self::new(
        OperationKind::Export,
        OperationClass::ReadOnly,
        OperationDispatchMode::SyncReadOnly,
        OperationOutcomeFamily::ExportHtml,
        OperationTerminalPolicy::OutcomeAcknowledgement,
        &[],
    );
    const MERGE_CHILD_WORKTREE: Self = Self::new(
        OperationKind::MergeChildWorktree,
        OperationClass::SessionWriteRoot,
        OperationDispatchMode::Async,
        OperationOutcomeFamily::MergeApplied,
        OperationTerminalPolicy::OutcomeAcknowledgement,
        &[],
    );
    const LIST_MERGE_PROPOSALS: Self = Self::new(
        OperationKind::ListMergeProposals,
        OperationClass::ReadOnly,
        OperationDispatchMode::Async,
        OperationOutcomeFamily::MergeProposals,
        OperationTerminalPolicy::OutcomeAcknowledgement,
        &[],
    );
    const DISCARD_CHILD_WORKTREE: Self = Self::new(
        OperationKind::DiscardChildWorktree,
        OperationClass::SessionWriteRoot,
        OperationDispatchMode::Async,
        OperationOutcomeFamily::WorktreeDiscarded,
        OperationTerminalPolicy::OutcomeAcknowledgement,
        &[],
    );

    fn descriptor(self) -> OperationDescriptor {
        let Self {
            submitted_kind,
            admission_class,
            dispatch_mode,
            outcome_family,
            terminal_policy,
            permitted_root_evidence,
        } = self;
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
            Self::Prompt(_) => OperationContract::PROMPT,
            Self::Compact(_) => OperationContract::COMPACT,
            Self::BranchSummary { .. } => OperationContract::BRANCH_SUMMARY,
            Self::SelfHealingEdit(_) => OperationContract::SELF_HEALING_EDIT,
            Self::InvokeAgent(_) => OperationContract::INVOKE_AGENT,
            Self::InvokeTeam(_) => OperationContract::INVOKE_TEAM,
            Self::ApproveDelegation { .. } => OperationContract::APPROVE_DELEGATION,
            Self::RejectDelegation { .. } => OperationContract::REJECT_DELEGATION,
            Self::ForkSession { .. } => OperationContract::FORK_SESSION,
            Self::SwitchActiveLeaf { .. } => OperationContract::SWITCH_ACTIVE_LEAF,
            Self::SetSessionTreeLabel { .. } => OperationContract::SET_SESSION_TREE_LABEL,
            Self::SetSessionName { .. } => OperationContract::SET_SESSION_NAME,
            Self::ExportCurrent => OperationContract::EXPORT_CURRENT,
            Self::ExportCurrentHtml(_) => OperationContract::EXPORT_CURRENT_HTML,
            Self::ListMergeProposals => OperationContract::LIST_MERGE_PROPOSALS,
            Self::MergeChildWorktree { .. } => OperationContract::MERGE_CHILD_WORKTREE,
            Self::DiscardChildWorktree { .. } => OperationContract::DISCARD_CHILD_WORKTREE,
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

    /// The provider runtime this operation carries, when it drives a model.
    pub(crate) fn runtime(&self) -> Option<&RuntimeSnapshot> {
        match self {
            Self::Prompt(options)
            | Self::Compact(options)
            | Self::BranchSummary { options, .. } => options.runtime(),
            Self::InvokeAgent(options) => options.prompt_options().runtime(),
            Self::InvokeTeam(options) => options.prompt_options().runtime(),
            Self::SelfHealingEdit(request) => request
                .model_repair()
                .and_then(|repair| repair.prompt_options().runtime()),
            Self::ApproveDelegation { .. }
            | Self::RejectDelegation { .. }
            | Self::ForkSession { .. }
            | Self::SwitchActiveLeaf { .. }
            | Self::SetSessionTreeLabel { .. }
            | Self::SetSessionName { .. }
            | Self::ExportCurrent
            | Self::ExportCurrentHtml(_)
            | Self::ListMergeProposals
            | Self::MergeChildWorktree { .. }
            | Self::DiscardChildWorktree { .. } => None,
        }
    }

    pub(crate) fn session_access(&self) -> SessionCapabilityAccess {
        match self.descriptor().session_access {
            OperationSessionAccess::None => SessionCapabilityAccess::None,
            OperationSessionAccess::Read => SessionCapabilityAccess::Read,
            OperationSessionAccess::Write => SessionCapabilityAccess::Write,
        }
    }

    pub(crate) fn prompt_options_mut(&mut self) -> Option<&mut PromptTurnOptions> {
        match self {
            Self::Prompt(options) | Self::Compact(options) => Some(options),
            Self::BranchSummary { options, .. } => Some(options),
            Self::SelfHealingEdit(request) => request
                .model_repair_mut()
                .map(|repair| repair.prompt_options_mut()),
            Self::InvokeAgent(options) => Some(options.prompt_options_mut()),
            Self::InvokeTeam(options) => Some(options.prompt_options_mut()),
            Self::ApproveDelegation { .. }
            | Self::RejectDelegation { .. }
            | Self::ForkSession { .. }
            | Self::SwitchActiveLeaf { .. }
            | Self::SetSessionTreeLabel { .. }
            | Self::SetSessionName { .. }
            | Self::ExportCurrent
            | Self::ExportCurrentHtml(_)
            | Self::ListMergeProposals
            | Self::MergeChildWorktree { .. }
            | Self::DiscardChildWorktree { .. } => None,
        }
    }

    /// The kind known before admission. Delegation approval resolves its kind
    /// from the pending request, so it has none until then.
    pub(crate) fn static_kind(&self) -> Option<OperationKind> {
        (!matches!(self, Self::ApproveDelegation { .. }))
            .then_some(self.descriptor().submitted_kind)
    }

    /// Normalizes the two export variants into the runner's options. This is the
    /// only shape difference between the submitted operation and what a runner
    /// consumes.
    pub(crate) fn export_options(&self) -> Option<ExportOptions> {
        match self {
            Self::ExportCurrent => Some(ExportOptions::view()),
            Self::ExportCurrentHtml(path) => Some(ExportOptions::html(path.clone())),
            _ => None,
        }
    }
}

mod outcome;

pub(crate) use outcome::prompt_text_submission_fingerprint;
use outcome::submission_payload_fingerprint;
