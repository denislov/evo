#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OperationKind {
    Prompt,
    Compact,
    DelegationConfirmation,
    BranchSummary,
    AgentInvocation,
    AgentTeam,
    Export,
    ForkSession,
    SwitchActiveLeaf,
    SetSessionTreeLabel,
    SetSessionName,
    SelfHealingEdit,
    ListMergeProposals,
    MergeChildWorktree,
    DiscardChildWorktree,
}

impl OperationKind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Prompt => "prompt",
            Self::Compact => "compact",
            Self::DelegationConfirmation => "delegation_confirmation",
            Self::BranchSummary => "branch_summary",
            Self::AgentInvocation => "agent_invocation",
            Self::AgentTeam => "agent_team",
            Self::Export => "export",
            Self::ForkSession => "fork_session",
            Self::SwitchActiveLeaf => "switch_active_leaf",
            Self::SetSessionTreeLabel => "set_session_tree_label",
            Self::SetSessionName => "set_session_name",
            Self::SelfHealingEdit => "self_healing_edit",
            Self::ListMergeProposals => "list_merge_proposals",
            Self::MergeChildWorktree => "merge_child_worktree",
            Self::DiscardChildWorktree => "discard_child_worktree",
        }
    }

    pub(crate) fn from_str(value: &str) -> Option<Self> {
        Some(match value {
            "prompt" => Self::Prompt,
            "compact" => Self::Compact,
            "delegation_confirmation" => Self::DelegationConfirmation,
            "branch_summary" => Self::BranchSummary,
            "agent_invocation" => Self::AgentInvocation,
            "agent_team" => Self::AgentTeam,
            "export" => Self::Export,
            "fork_session" => Self::ForkSession,
            "switch_active_leaf" => Self::SwitchActiveLeaf,
            "set_session_tree_label" => Self::SetSessionTreeLabel,
            "set_session_name" => Self::SetSessionName,
            "self_healing_edit" => Self::SelfHealingEdit,
            "list_merge_proposals" => Self::ListMergeProposals,
            "merge_child_worktree" => Self::MergeChildWorktree,
            "discard_child_worktree" => Self::DiscardChildWorktree,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OperationDispatchMode {
    Async,
    SyncReadOnly,
    SyncMutable,
}

impl OperationDispatchMode {
    pub(crate) fn dispatcher_label(self) -> &'static str {
        match self {
            Self::Async => "async",
            Self::SyncReadOnly => "read-only sync",
            Self::SyncMutable => "sync mutable",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OperationClass {
    Query,
    ReadOnly,
    SessionWriteRoot,
    NonSessionRoot,
    RuntimeWrite,
    Child,
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
    DelegationApproved,
    DelegationRejected,
    SessionForked,
    ActiveLeafSwitched,
    SessionTreeLabelChanged,
    SessionNameChanged,
    Export,
    ExportHtml,
    MergeApplied,
    WorktreeDiscarded,
    MergeProposals,
}

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
    pub(crate) const NONE: Self = Self {
        session_if_persistent: false,
        runtime_generation: false,
    };
    pub(crate) const SESSION: Self = Self {
        session_if_persistent: true,
        runtime_generation: false,
    };
    pub(crate) const SESSION_AND_RUNTIME: Self = Self {
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
