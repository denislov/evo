pub mod id;
pub mod types;

pub use id::{
    SessionIdGenerator, TranscriptIdError, create_session_id, create_timestamp, generate_entry_id,
};
pub use types::{
    SessionEntry, SessionHeader, SessionMetadata, SessionTreeNode, StoredAgentMessage, StoredUsage,
    StoredUsageCost,
};

pub fn agent_message_to_stored(
    msg: &crate::agent::types::AgentMessage,
    timestamp_ms: u64,
) -> Option<StoredAgentMessage> {
    use crate::agent::types::AgentMessage;

    match msg {
        AgentMessage::UserText {
            message_id: _,
            text,
        } => Some(StoredAgentMessage::User {
            content: vec![ai_protocol::api::conversation::ContentBlock::Text {
                text: text.clone(),
                text_signature: None,
            }],
            timestamp: timestamp_ms,
        }),
        AgentMessage::Assistant {
            message_id: _,
            message,
        } => Some(StoredAgentMessage::Assistant {
            content: message.content.clone(),
            api: message.api.clone(),
            provider: message.provider.clone().unwrap_or_default(),
            model: message.model.clone(),
            response_model: message.response_model.clone(),
            response_id: message.response_id.clone(),
            usage: StoredUsage {
                input: message.usage.input,
                output: message.usage.output,
                cache_read: message.usage.cache_read,
                cache_write: message.usage.cache_write,
                total: message.usage.total_tokens,
                cost: StoredUsageCost {
                    known: message.usage.cost.known,
                    input: message.usage.cost.input,
                    output: message.usage.cost.output,
                    cache_read: message.usage.cost.cache_read,
                    cache_write: message.usage.cost.cache_write,
                },
            },
            stop_reason: message.stop_reason.clone(),
            error_message: message.error_message.clone(),
            timestamp: message.timestamp,
        }),
        AgentMessage::ToolResult {
            message_id: _,
            tool_call_id,
            tool_name,
            is_error,
            content,
        } => Some(StoredAgentMessage::ToolResult {
            tool_call_id: tool_call_id.clone(),
            tool_name: tool_name.clone(),
            content: content.clone(),
            is_error: *is_error,
            timestamp: timestamp_ms,
        }),
        AgentMessage::BashExecution {
            command,
            output,
            exit_code,
            cancelled,
            truncated,
            full_output_path,
            exclude_from_context,
            timestamp,
            ..
        } => Some(StoredAgentMessage::BashExecution {
            command: command.clone(),
            output: output.clone(),
            exit_code: *exit_code,
            cancelled: *cancelled,
            truncated: *truncated,
            full_output_path: full_output_path.clone(),
            exclude_from_context: Some(*exclude_from_context).filter(|value| *value),
            timestamp: *timestamp,
        }),
        AgentMessage::Custom {
            custom_type,
            content,
            display,
            details,
            timestamp,
            ..
        } => Some(StoredAgentMessage::Custom {
            custom_type: custom_type.clone(),
            content: content.clone(),
            display: *display,
            details: details.clone(),
            timestamp: *timestamp,
        }),
        AgentMessage::BranchSummary {
            summary,
            from_id,
            timestamp,
            ..
        } => Some(StoredAgentMessage::BranchSummary {
            summary: summary.clone(),
            from_id: from_id.clone(),
            timestamp: *timestamp,
        }),
        AgentMessage::SystemPrompt { .. } => None,
        AgentMessage::CompactionSummary { .. } => None,
    }
}
