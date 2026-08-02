use super::emission::ProductEventDraft;
use super::{
    CodingAgentProductEventDurability, CodingAgentProductEventError, CodingAgentProductEventKind,
    CodingAgentProductEventTerminalStatus, CodingAgentWorkflowProductEvent,
};
use crate::kernel::error::CodingSessionError;
use crate::kernel::operation::{OperationKind, OperationRootTerminalEvidence};

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum PromptEvent {
    Started {
        operation_id: String,
        turn_id: String,
    },
    Completed {
        operation_id: String,
        turn_id: String,
    },
    Failed {
        operation_id: String,
        error: CodingSessionError,
    },
    Aborted {
        operation_id: String,
        reason: String,
    },
}

impl PromptEvent {
    pub(crate) fn root_terminal_evidence(
        &self,
        admitted_kind: OperationKind,
    ) -> Option<OperationRootTerminalEvidence> {
        match self {
            Self::Started { .. } => None,
            Self::Completed { .. } => Some(OperationRootTerminalEvidence::PromptCompleted),
            Self::Failed { .. } if admitted_kind == OperationKind::Compact => {
                Some(OperationRootTerminalEvidence::CompactPromptFailed)
            }
            Self::Failed { .. } => Some(OperationRootTerminalEvidence::PromptFailed),
            Self::Aborted { .. } => Some(OperationRootTerminalEvidence::PromptAborted),
        }
    }

    pub(crate) fn into_product_draft(self) -> ProductEventDraft {
        match self {
            Self::Started {
                operation_id,
                turn_id,
            } => ProductEventDraft {
                event: CodingAgentProductEventKind::Workflow(
                    CodingAgentWorkflowProductEvent::PromptStarted {
                        operation_id: operation_id.clone(),
                        turn_id,
                    },
                ),
                operation_id: Some(operation_id),
                session_id: None,
                terminal_status: None,
                durability: CodingAgentProductEventDurability::LiveOnly,
            },
            Self::Completed {
                operation_id,
                turn_id,
            } => ProductEventDraft {
                event: CodingAgentProductEventKind::Workflow(
                    CodingAgentWorkflowProductEvent::PromptCompleted {
                        operation_id: operation_id.clone(),
                        turn_id,
                    },
                ),
                operation_id: Some(operation_id),
                session_id: None,
                terminal_status: Some(CodingAgentProductEventTerminalStatus::Completed),
                durability: CodingAgentProductEventDurability::LiveOnly,
            },
            Self::Failed {
                operation_id,
                error,
            } => {
                let durability = if matches!(error, CodingSessionError::PartialCommit { .. }) {
                    CodingAgentProductEventDurability::PersistenceUncertain {
                        operation_id: operation_id.clone(),
                    }
                } else {
                    CodingAgentProductEventDurability::LiveOnly
                };
                ProductEventDraft {
                    event: CodingAgentProductEventKind::Workflow(
                        CodingAgentWorkflowProductEvent::PromptFailed {
                            operation_id: operation_id.clone(),
                            error: CodingAgentProductEventError::from(error),
                        },
                    ),
                    operation_id: Some(operation_id),
                    session_id: None,
                    terminal_status: Some(CodingAgentProductEventTerminalStatus::Failed),
                    durability,
                }
            }
            Self::Aborted {
                operation_id,
                reason,
            } => ProductEventDraft {
                event: CodingAgentProductEventKind::Workflow(
                    CodingAgentWorkflowProductEvent::PromptAborted {
                        operation_id: operation_id.clone(),
                        reason,
                    },
                ),
                operation_id: Some(operation_id),
                session_id: None,
                terminal_status: Some(CodingAgentProductEventTerminalStatus::Aborted),
                durability: CodingAgentProductEventDurability::LiveOnly,
            },
        }
    }
}
