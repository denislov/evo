use super::emission::ProductEventDraft;
use super::{
    CodingAgentMergeChange, CodingAgentMergeChangeKind, CodingAgentMergeProductEvent,
    CodingAgentMergeProposal, CodingAgentProductEventDurability,
};
use crate::kernel::error::CodingSessionError;

/// Internal merge event for one managed child worktree proposal (ARC-340).
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum MergeEvent {
    /// A child finished successfully and its worktree awaits merge/discard.
    ProposalCreated {
        operation_id: String,
        proposal: workspace_runtime::api::MergeProposal,
    },
    /// The proposal was applied to the parent workspace.
    Applied {
        operation_id: String,
        worktree_id: String,
        applied: usize,
    },
    /// The proposal conflicts with parent-side changes; nothing was applied.
    Conflicted {
        operation_id: String,
        worktree_id: String,
        paths: Vec<String>,
    },
    /// The parent moved past the child's base revision; nothing was applied.
    StaleParent {
        operation_id: String,
        worktree_id: String,
        expected: Option<String>,
        actual: Option<String>,
    },
    /// The child worktree was discarded without merging.
    Discarded {
        operation_id: String,
        worktree_id: String,
    },
    /// The merge/discard operation failed.
    Failed {
        operation_id: String,
        worktree_id: String,
        error: CodingSessionError,
    },
}

impl MergeEvent {
    pub(crate) fn into_product_draft(self) -> ProductEventDraft {
        let (event, operation_id) = match self {
            Self::ProposalCreated {
                operation_id,
                proposal,
            } => (
                CodingAgentMergeProductEvent::ProposalCreated {
                    proposal: proposal.into(),
                },
                operation_id,
            ),
            Self::Applied {
                operation_id,
                worktree_id,
                applied,
            } => (
                CodingAgentMergeProductEvent::Applied {
                    worktree_id,
                    applied,
                },
                operation_id,
            ),
            Self::Conflicted {
                operation_id,
                worktree_id,
                paths,
            } => (
                CodingAgentMergeProductEvent::Conflicted { worktree_id, paths },
                operation_id,
            ),
            Self::StaleParent {
                operation_id,
                worktree_id,
                expected,
                actual,
            } => (
                CodingAgentMergeProductEvent::StaleParent {
                    worktree_id,
                    expected,
                    actual,
                },
                operation_id,
            ),
            Self::Discarded {
                operation_id,
                worktree_id,
            } => (
                CodingAgentMergeProductEvent::Discarded { worktree_id },
                operation_id,
            ),
            Self::Failed {
                operation_id,
                worktree_id,
                error,
            } => (
                CodingAgentMergeProductEvent::Failed {
                    worktree_id,
                    error: error.into(),
                },
                operation_id,
            ),
        };
        ProductEventDraft {
            event: super::CodingAgentProductEventKind::Merge(event),
            operation_id: Some(operation_id.clone()),
            session_id: None,
            terminal_status: None,
            durability: CodingAgentProductEventDurability::PendingSessionWrite {
                operation_id: operation_id.clone(),
            },
        }
    }
}

impl From<workspace_runtime::api::MergeProposal> for CodingAgentMergeProposal {
    fn from(proposal: workspace_runtime::api::MergeProposal) -> Self {
        Self {
            worktree_id: proposal.worktree_id,
            child_operation_id: proposal.child_operation_id,
            base_revision: proposal.changeset.base_revision,
            changes: proposal
                .changeset
                .entries
                .into_iter()
                .map(|entry| CodingAgentMergeChange {
                    path: entry.path.to_string_lossy().into_owned(),
                    kind: match entry.kind {
                        workspace_runtime::api::ChangeKind::Added => {
                            CodingAgentMergeChangeKind::Added
                        }
                        workspace_runtime::api::ChangeKind::Modified => {
                            CodingAgentMergeChangeKind::Modified
                        }
                        workspace_runtime::api::ChangeKind::Deleted => {
                            CodingAgentMergeChangeKind::Deleted
                        }
                    },
                    additions: entry.additions,
                    deletions: entry.deletions,
                })
                .collect(),
        }
    }
}
