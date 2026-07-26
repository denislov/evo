use super::emission::ProductEventDraft;
use super::{
    CodingAgentImageContent, CodingAgentMessageProductEvent, CodingAgentProductEventDurability,
    CodingAgentProductEventKind, CodingAgentProductEventUsage,
};
use ai::api::conversation::Usage;

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum MessageEvent {
    Started {
        operation_id: String,
        turn_id: String,
        message_id: Option<String>,
    },
    Delta {
        operation_id: String,
        turn_id: String,
        message_id: Option<String>,
        text: String,
    },
    ThinkingDelta {
        operation_id: String,
        turn_id: String,
        message_id: Option<String>,
        text: String,
    },
    Completed {
        operation_id: String,
        turn_id: String,
        message_id: Option<String>,
        final_text: String,
        images: Vec<CodingAgentImageContent>,
        usage: Usage,
    },
}

impl MessageEvent {
    pub(crate) fn into_product_draft(self) -> ProductEventDraft {
        let (event, operation_id) = match self {
            Self::Started {
                operation_id,
                turn_id,
                message_id,
            } => (
                CodingAgentMessageProductEvent::Started {
                    operation_id: operation_id.clone(),
                    turn_id,
                    message_id,
                },
                operation_id,
            ),
            Self::Delta {
                operation_id,
                turn_id,
                message_id,
                text,
            } => (
                CodingAgentMessageProductEvent::Delta {
                    operation_id: operation_id.clone(),
                    turn_id,
                    message_id,
                    text,
                },
                operation_id,
            ),
            Self::ThinkingDelta {
                operation_id,
                turn_id,
                message_id,
                text,
            } => (
                CodingAgentMessageProductEvent::ThinkingDelta {
                    operation_id: operation_id.clone(),
                    turn_id,
                    message_id,
                    text,
                },
                operation_id,
            ),
            Self::Completed {
                operation_id,
                turn_id,
                message_id,
                final_text,
                images,
                usage,
            } => (
                CodingAgentMessageProductEvent::Completed {
                    operation_id: operation_id.clone(),
                    turn_id,
                    message_id,
                    final_text,
                    images,
                    usage: CodingAgentProductEventUsage {
                        input: usage.input,
                        output: usage.output,
                        cache_read: usage.cache_read,
                        cache_write: usage.cache_write,
                        total_tokens: usage.total_tokens,
                        cost_known: usage.cost.known,
                        input_cost: usage.cost.input,
                        output_cost: usage.cost.output,
                        cache_read_cost: usage.cost.cache_read,
                        cache_write_cost: usage.cost.cache_write,
                    },
                },
                operation_id,
            ),
        };
        ProductEventDraft {
            event: CodingAgentProductEventKind::Message(event),
            operation_id: Some(operation_id),
            session_id: None,
            terminal_status: None,
            durability: CodingAgentProductEventDurability::LiveOnly,
        }
    }
}
