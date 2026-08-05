use std::collections::VecDeque;
use std::str::FromStr;

use super::types::AgentMessage;
use ai_protocol::api::conversation::ContentBlock;

pub const MAX_AGENT_QUEUE_ITEMS: usize = 32;
pub const MAX_AGENT_QUEUE_BYTES: usize = 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentInputQueue {
    Steering,
    FollowUp,
}

impl std::fmt::Display for AgentInputQueue {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Steering => "steering",
            Self::FollowUp => "follow-up",
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AgentQueueError {
    #[error("{queue} queue reached its item limit of {max_items}")]
    ItemLimit {
        queue: AgentInputQueue,
        max_items: usize,
    },
    #[error("{queue} queue would exceed its byte limit of {max_bytes}")]
    ByteLimit {
        queue: AgentInputQueue,
        max_bytes: usize,
    },
}

// ── QueueMode ──────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum QueueMode {
    #[default]
    All,
    OneAtATime,
}

impl std::fmt::Display for QueueMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            QueueMode::All => "all",
            QueueMode::OneAtATime => "one-at-a-time",
        };
        write!(f, "{}", s)
    }
}

impl FromStr for QueueMode {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "all" => Ok(QueueMode::All),
            "one-at-a-time" => Ok(QueueMode::OneAtATime),
            _ => Err(format!("unknown queue mode: {}", s)),
        }
    }
}

pub fn drain_queue(queue: &mut VecDeque<AgentMessage>, mode: QueueMode) -> Vec<AgentMessage> {
    match mode {
        QueueMode::All => queue.drain(..).collect(),
        QueueMode::OneAtATime => queue.pop_front().into_iter().collect(),
    }
}

pub(crate) fn enqueue_message(
    queue: &mut VecDeque<AgentMessage>,
    queue_kind: AgentInputQueue,
    message: AgentMessage,
) -> Result<(), AgentQueueError> {
    if queue.len() >= MAX_AGENT_QUEUE_ITEMS {
        return Err(AgentQueueError::ItemLimit {
            queue: queue_kind,
            max_items: MAX_AGENT_QUEUE_ITEMS,
        });
    }
    let retained_bytes = queue.iter().fold(0usize, |total, message| {
        total.saturating_add(message_bytes(message))
    });
    if retained_bytes.saturating_add(message_bytes(&message)) > MAX_AGENT_QUEUE_BYTES {
        return Err(AgentQueueError::ByteLimit {
            queue: queue_kind,
            max_bytes: MAX_AGENT_QUEUE_BYTES,
        });
    }
    queue.push_back(message);
    Ok(())
}

fn message_bytes(message: &AgentMessage) -> usize {
    match message {
        AgentMessage::UserText { message_id, text }
        | AgentMessage::SystemPrompt { message_id, text } => {
            message_id.len().saturating_add(text.len())
        }
        AgentMessage::Assistant {
            message_id,
            message,
        } => message_id
            .len()
            .saturating_add(content_bytes(&message.content)),
        AgentMessage::ToolResult {
            message_id,
            tool_call_id,
            tool_name,
            content,
            ..
        } => message_id
            .len()
            .saturating_add(tool_call_id.len())
            .saturating_add(tool_name.len())
            .saturating_add(content_bytes(content)),
        AgentMessage::CompactionSummary {
            message_id,
            summary,
            ..
        } => message_id.len().saturating_add(summary.len()),
        AgentMessage::BashExecution {
            message_id,
            command,
            output,
            full_output_path,
            ..
        } => message_id
            .len()
            .saturating_add(command.len())
            .saturating_add(output.len())
            .saturating_add(full_output_path.as_ref().map_or(0, String::len)),
        AgentMessage::Custom {
            message_id,
            custom_type,
            content,
            details,
            ..
        } => message_id
            .len()
            .saturating_add(custom_type.len())
            .saturating_add(content_bytes(content))
            .saturating_add(details.as_ref().map_or(0, json_bytes)),
        AgentMessage::BranchSummary {
            message_id,
            summary,
            from_id,
            ..
        } => message_id
            .len()
            .saturating_add(summary.len())
            .saturating_add(from_id.len()),
    }
}

fn content_bytes(content: &[ContentBlock]) -> usize {
    content.iter().fold(0usize, |total, block| {
        let block_bytes = match block {
            ContentBlock::Text {
                text,
                text_signature,
            } => text
                .len()
                .saturating_add(text_signature.as_ref().map_or(0, String::len)),
            ContentBlock::Thinking {
                thinking,
                thinking_signature,
                provider_metadata,
                ..
            } => thinking
                .len()
                .saturating_add(thinking_signature.as_ref().map_or(0, String::len))
                .saturating_add(provider_metadata.as_ref().map_or(0, |metadata| {
                    metadata
                        .api
                        .len()
                        .saturating_add(metadata.item_id.as_ref().map_or(0, String::len))
                        .saturating_add(metadata.encrypted_content.as_ref().map_or(0, String::len))
                })),
            ContentBlock::Image { data, mime_type } => data.len().saturating_add(mime_type.len()),
            ContentBlock::ToolCall {
                id,
                name,
                arguments,
                thought_signature,
                ..
            } => id
                .len()
                .saturating_add(name.len())
                .saturating_add(json_bytes(arguments))
                .saturating_add(thought_signature.as_ref().map_or(0, String::len)),
            ContentBlock::ProviderItem { api, item } => api.len().saturating_add(json_bytes(item)),
        };
        total.saturating_add(block_bytes)
    })
}

fn json_bytes(value: &serde_json::Value) -> usize {
    match value {
        serde_json::Value::Null => 4,
        serde_json::Value::Bool(_) => 5,
        serde_json::Value::Number(number) => number.to_string().len(),
        serde_json::Value::String(value) => value.len(),
        serde_json::Value::Array(values) => values.iter().fold(0usize, |total, value| {
            total.saturating_add(json_bytes(value))
        }),
        serde_json::Value::Object(values) => values.iter().fold(0usize, |total, (key, value)| {
            total
                .saturating_add(key.len())
                .saturating_add(json_bytes(value))
        }),
    }
}
