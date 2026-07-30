use super::emission::ProductEventDraft;
use super::{
    CodingAgentProductEventDurability, CodingAgentProductEventError, CodingAgentProductEventKind,
    CodingAgentProductEventTerminalStatus, CodingAgentTeamProductEvent,
};
use crate::profiles::ProfileId;
use crate::runtime::facade::CodingSessionError;
use crate::runtime::operation::contract::OperationRootTerminalEvidence;

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum TeamEvent {
    Started {
        operation_id: String,
        team_id: ProfileId,
        task: String,
    },
    MemberStarted {
        operation_id: String,
        child_operation_id: String,
        team_id: ProfileId,
        profile_id: ProfileId,
        task: String,
    },
    MemberCompleted {
        operation_id: String,
        child_operation_id: String,
        team_id: ProfileId,
        profile_id: ProfileId,
        final_text: String,
    },
    Completed {
        operation_id: String,
        team_id: ProfileId,
        final_text: String,
    },
    Failed {
        operation_id: String,
        team_id: ProfileId,
        error: CodingSessionError,
    },
    Aborted {
        operation_id: String,
        team_id: ProfileId,
        reason: String,
    },
}

impl TeamEvent {
    pub(crate) fn root_terminal_evidence(&self) -> Option<OperationRootTerminalEvidence> {
        match self {
            Self::Started { .. } | Self::MemberStarted { .. } | Self::MemberCompleted { .. } => {
                None
            }
            Self::Completed { .. } => Some(OperationRootTerminalEvidence::AgentTeamCompleted),
            Self::Failed { .. } => Some(OperationRootTerminalEvidence::AgentTeamFailed),
            Self::Aborted { .. } => Some(OperationRootTerminalEvidence::AgentTeamAborted),
        }
    }

    pub(crate) fn into_product_draft(self) -> ProductEventDraft {
        let (event, operation_id, terminal_status) = match self {
            Self::Started {
                operation_id,
                team_id,
                task,
            } => (
                CodingAgentTeamProductEvent::Started {
                    operation_id: operation_id.clone(),
                    team_id: team_id.as_str().to_owned(),
                    task,
                },
                operation_id,
                None,
            ),
            Self::MemberStarted {
                operation_id,
                child_operation_id,
                team_id,
                profile_id,
                task,
            } => (
                CodingAgentTeamProductEvent::MemberStarted {
                    operation_id: operation_id.clone(),
                    child_operation_id,
                    team_id: team_id.as_str().to_owned(),
                    profile_id: profile_id.as_str().to_owned(),
                    task,
                },
                operation_id,
                None,
            ),
            Self::MemberCompleted {
                operation_id,
                child_operation_id,
                team_id,
                profile_id,
                final_text,
            } => (
                CodingAgentTeamProductEvent::MemberCompleted {
                    operation_id: operation_id.clone(),
                    child_operation_id,
                    team_id: team_id.as_str().to_owned(),
                    profile_id: profile_id.as_str().to_owned(),
                    final_text,
                },
                operation_id,
                None,
            ),
            Self::Completed {
                operation_id,
                team_id,
                final_text,
            } => (
                CodingAgentTeamProductEvent::Completed {
                    operation_id: operation_id.clone(),
                    team_id: team_id.as_str().to_owned(),
                    final_text,
                },
                operation_id,
                Some(CodingAgentProductEventTerminalStatus::Completed),
            ),
            Self::Failed {
                operation_id,
                team_id,
                error,
            } => (
                CodingAgentTeamProductEvent::Failed {
                    operation_id: operation_id.clone(),
                    team_id: team_id.as_str().to_owned(),
                    error: CodingAgentProductEventError::from(error),
                },
                operation_id,
                Some(CodingAgentProductEventTerminalStatus::Failed),
            ),
            Self::Aborted {
                operation_id,
                team_id,
                reason,
            } => (
                CodingAgentTeamProductEvent::Aborted {
                    operation_id: operation_id.clone(),
                    team_id: team_id.as_str().to_owned(),
                    reason,
                },
                operation_id,
                Some(CodingAgentProductEventTerminalStatus::Aborted),
            ),
        };
        ProductEventDraft {
            event: CodingAgentProductEventKind::Team(event),
            operation_id: Some(operation_id),
            session_id: None,
            terminal_status,
            durability: CodingAgentProductEventDurability::LiveOnly,
        }
    }
}
