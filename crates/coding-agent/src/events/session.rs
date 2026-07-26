use super::emission::ProductEventDraft;
use super::{
    CodingAgentProductEventDurability, CodingAgentProductEventKind,
    CodingAgentProductEventTerminalStatus, CodingAgentSessionProductEvent,
    CodingAgentSessionWriteFailureStatus,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SessionLifecycleEvent {
    Opened { session_id: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SessionCompactionEvent {
    pub(crate) operation_id: String,
    pub(crate) turn_id: String,
    pub(crate) summary: String,
    pub(crate) first_kept_message_id: String,
    pub(crate) tokens_before: u32,
}

impl SessionCompactionEvent {
    #[cfg(test)]
    pub(crate) fn root_terminal_evidence(
        &self,
    ) -> crate::runtime::outcome::OperationRootTerminalEvidence {
        crate::runtime::outcome::OperationRootTerminalEvidence::CompactionCompleted
    }

    pub(crate) fn into_product_draft(self) -> ProductEventDraft {
        ProductEventDraft {
            event: CodingAgentProductEventKind::Session(
                CodingAgentSessionProductEvent::CompactionCompleted {
                    operation_id: self.operation_id.clone(),
                    turn_id: self.turn_id,
                    summary: self.summary,
                    first_kept_message_id: self.first_kept_message_id,
                    tokens_before: self.tokens_before,
                },
            ),
            operation_id: Some(self.operation_id),
            session_id: None,
            terminal_status: Some(CodingAgentProductEventTerminalStatus::Completed),
            durability: CodingAgentProductEventDurability::LiveOnly,
        }
    }
}

impl SessionLifecycleEvent {
    pub(crate) fn into_product_draft(self) -> ProductEventDraft {
        match self {
            Self::Opened { session_id } => ProductEventDraft {
                event: CodingAgentProductEventKind::Session(
                    CodingAgentSessionProductEvent::Opened {
                        session_id: session_id.clone(),
                    },
                ),
                operation_id: None,
                session_id: Some(session_id),
                terminal_status: None,
                durability: CodingAgentProductEventDurability::LiveOnly,
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SessionWriteEvent {
    Pending {
        operation_id: String,
    },
    Committed {
        operation_id: String,
        session_id: String,
    },
    Skipped {
        operation_id: String,
        reason: String,
    },
    Failed {
        operation_id: String,
        reason: String,
        status: CodingAgentSessionWriteFailureStatus,
    },
}

impl SessionWriteEvent {
    pub(crate) fn is_pending(&self) -> bool {
        matches!(self, Self::Pending { .. })
    }

    pub(crate) fn is_final(&self) -> bool {
        !self.is_pending()
    }

    pub(crate) fn into_product_draft(self) -> ProductEventDraft {
        match self {
            Self::Pending { operation_id } => ProductEventDraft {
                event: CodingAgentProductEventKind::Session(
                    CodingAgentSessionProductEvent::WritePending {
                        operation_id: operation_id.clone(),
                    },
                ),
                operation_id: Some(operation_id.clone()),
                session_id: None,
                terminal_status: None,
                durability: CodingAgentProductEventDurability::PendingSessionWrite { operation_id },
            },
            Self::Committed {
                operation_id,
                session_id,
            } => ProductEventDraft {
                event: CodingAgentProductEventKind::Session(
                    CodingAgentSessionProductEvent::WriteCommitted {
                        operation_id: operation_id.clone(),
                        session_id: session_id.clone(),
                    },
                ),
                operation_id: Some(operation_id),
                session_id: Some(session_id.clone()),
                terminal_status: Some(CodingAgentProductEventTerminalStatus::Completed),
                durability: CodingAgentProductEventDurability::Durable { session_id },
            },
            Self::Skipped {
                operation_id,
                reason,
            } => ProductEventDraft {
                event: CodingAgentProductEventKind::Session(
                    CodingAgentSessionProductEvent::WriteSkipped {
                        operation_id: operation_id.clone(),
                        reason,
                    },
                ),
                operation_id: Some(operation_id),
                session_id: None,
                terminal_status: None,
                durability: CodingAgentProductEventDurability::LiveOnly,
            },
            Self::Failed {
                operation_id,
                reason,
                status,
            } => {
                let durability = match status {
                    CodingAgentSessionWriteFailureStatus::Definite => {
                        CodingAgentProductEventDurability::PersistenceFailed {
                            operation_id: operation_id.clone(),
                            reason: reason.clone(),
                        }
                    }
                    CodingAgentSessionWriteFailureStatus::Uncertain => {
                        CodingAgentProductEventDurability::PersistenceUncertain {
                            operation_id: operation_id.clone(),
                        }
                    }
                };
                ProductEventDraft {
                    event: CodingAgentProductEventKind::Session(
                        CodingAgentSessionProductEvent::WriteFailed {
                            operation_id: operation_id.clone(),
                            reason,
                            status,
                        },
                    ),
                    operation_id: Some(operation_id),
                    session_id: None,
                    terminal_status: Some(CodingAgentProductEventTerminalStatus::Failed),
                    durability,
                }
            }
        }
    }
}
