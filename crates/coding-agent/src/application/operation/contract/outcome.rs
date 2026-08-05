use super::*;

pub(crate) fn prompt_text_submission_fingerprint(text: &str) -> String {
    submission_payload_fingerprint(text.as_bytes())
}

pub(super) fn submission_payload_fingerprint(payload: &[u8]) -> String {
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
            OperationOutcome::MergeApplied {
                worktree_id,
                applied,
            } => Self::MergeApplied {
                worktree_id,
                applied,
            },
            OperationOutcome::WorktreeDiscarded { worktree_id } => {
                Self::WorktreeDiscarded { worktree_id }
            }
            OperationOutcome::MergeProposals(proposals) => Self::MergeProposals(proposals),
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

    pub fn into_merge_proposals(
        self,
    ) -> Result<Vec<crate::events::CodingAgentMergeProposal>, Self> {
        match self {
            Self::MergeProposals(proposals) => Ok(proposals),
            other => Err(other),
        }
    }
}
