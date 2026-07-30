use super::emission::ProductEventDraft;
use super::{
    CodingAgentAgentProductEvent, CodingAgentProductEventDurability, CodingAgentProductEventError,
    CodingAgentProductEventKind, CodingAgentProductEventTerminalStatus,
};
use crate::profiles::ProfileId;
use crate::runtime::facade::CodingSessionError;
use crate::runtime::operation::contract::OperationRootTerminalEvidence;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AgentStreamEvent {
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
        context_window: Option<u32>,
    },
}

impl AgentStreamEvent {
    pub(crate) fn into_product_draft(self) -> ProductEventDraft {
        let (event, operation_id) = match self {
            Self::TurnStarted {
                operation_id,
                turn_id,
                agent_turn,
            } => (
                CodingAgentAgentProductEvent::TurnStarted {
                    operation_id: operation_id.clone(),
                    turn_id,
                    agent_turn,
                },
                operation_id,
            ),
            Self::ProviderRequestStarted {
                operation_id,
                turn_id,
                provider,
                model,
                context_window,
            } => (
                CodingAgentAgentProductEvent::ProviderRequestStarted {
                    operation_id: operation_id.clone(),
                    turn_id,
                    provider,
                    model,
                    context_window,
                },
                operation_id,
            ),
        };
        ProductEventDraft {
            event: CodingAgentProductEventKind::Agent(event),
            operation_id: Some(operation_id),
            session_id: None,
            terminal_status: None,
            durability: CodingAgentProductEventDurability::LiveOnly,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum AgentInvocationEvent {
    Started {
        operation_id: String,
        child_operation_id: String,
        profile_id: ProfileId,
        task: String,
    },
    Completed {
        operation_id: String,
        child_operation_id: String,
        profile_id: ProfileId,
        final_text: String,
    },
    Failed {
        operation_id: String,
        child_operation_id: String,
        profile_id: ProfileId,
        error: CodingSessionError,
    },
    Aborted {
        operation_id: String,
        child_operation_id: String,
        profile_id: ProfileId,
        reason: String,
    },
}

impl AgentInvocationEvent {
    pub(crate) fn root_terminal_evidence(&self) -> Option<OperationRootTerminalEvidence> {
        match self {
            Self::Started { .. } => None,
            Self::Completed { .. } => Some(OperationRootTerminalEvidence::AgentInvocationCompleted),
            Self::Failed { .. } => Some(OperationRootTerminalEvidence::AgentInvocationFailed),
            Self::Aborted { .. } => Some(OperationRootTerminalEvidence::AgentInvocationAborted),
        }
    }

    pub(crate) fn into_product_draft(self) -> ProductEventDraft {
        let (event, operation_id, terminal_status) = match self {
            Self::Started {
                operation_id,
                child_operation_id,
                profile_id,
                task,
            } => (
                CodingAgentAgentProductEvent::InvocationStarted {
                    operation_id: operation_id.clone(),
                    child_operation_id,
                    profile_id: profile_id.as_str().to_owned(),
                    task,
                },
                operation_id,
                None,
            ),
            Self::Completed {
                operation_id,
                child_operation_id,
                profile_id,
                final_text,
            } => (
                CodingAgentAgentProductEvent::InvocationCompleted {
                    operation_id: operation_id.clone(),
                    child_operation_id,
                    profile_id: profile_id.as_str().to_owned(),
                    final_text,
                },
                operation_id,
                Some(CodingAgentProductEventTerminalStatus::Completed),
            ),
            Self::Failed {
                operation_id,
                child_operation_id,
                profile_id,
                error,
            } => (
                CodingAgentAgentProductEvent::InvocationFailed {
                    operation_id: operation_id.clone(),
                    child_operation_id,
                    profile_id: profile_id.as_str().to_owned(),
                    error: CodingAgentProductEventError::from(error),
                },
                operation_id,
                Some(CodingAgentProductEventTerminalStatus::Failed),
            ),
            Self::Aborted {
                operation_id,
                child_operation_id,
                profile_id,
                reason,
            } => (
                CodingAgentAgentProductEvent::InvocationAborted {
                    operation_id: operation_id.clone(),
                    child_operation_id,
                    profile_id: profile_id.as_str().to_owned(),
                    reason,
                },
                operation_id,
                Some(CodingAgentProductEventTerminalStatus::Aborted),
            ),
        };
        ProductEventDraft {
            event: CodingAgentProductEventKind::Agent(event),
            operation_id: Some(operation_id),
            session_id: None,
            terminal_status,
            durability: CodingAgentProductEventDurability::LiveOnly,
        }
    }
}
