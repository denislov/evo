use super::emission::ProductEventDraft;
use super::{
    CodingAgentProductEventDurability, CodingAgentProductEventKind, CodingAgentRuntimeProductEvent,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RuntimeEvent {
    CompactionCompleted {
        operation_id: String,
        turn_id: String,
        summary: String,
        first_kept_message_id: String,
        tokens_before: u32,
    },
    ShutDown,
}

impl RuntimeEvent {
    pub(crate) fn into_product_draft(self) -> ProductEventDraft {
        match self {
            Self::CompactionCompleted {
                operation_id,
                turn_id,
                summary,
                first_kept_message_id,
                tokens_before,
            } => ProductEventDraft {
                event: CodingAgentProductEventKind::Runtime(
                    CodingAgentRuntimeProductEvent::CompactionCompleted {
                        operation_id: operation_id.clone(),
                        turn_id,
                        summary,
                        first_kept_message_id,
                        tokens_before,
                    },
                ),
                operation_id: Some(operation_id),
                session_id: None,
                terminal_status: None,
                durability: CodingAgentProductEventDurability::LiveOnly,
            },
            Self::ShutDown => ProductEventDraft {
                event: CodingAgentProductEventKind::Runtime(
                    CodingAgentRuntimeProductEvent::ShutDown,
                ),
                operation_id: None,
                session_id: None,
                terminal_status: None,
                durability: CodingAgentProductEventDurability::LiveOnly,
            },
        }
    }
}
