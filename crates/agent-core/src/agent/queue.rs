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
    Interjection,
}

impl std::fmt::Display for AgentInputQueue {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Steering => "steering",
            Self::FollowUp => "follow-up",
            Self::Interjection => "interjection",
        })
    }
}

#[derive(Debug, Clone)]
pub(crate) struct PromptQueueEntry {
    pub id: String,
    pub version: u32,
    pub message: AgentMessage,
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
    #[error("agent mailbox is full")]
    MailboxFull,
    #[error("agent actor is closed")]
    ActorClosed,
    #[error(
        "queue entry {entry_id} is stale: expected version {expected_version}, actual {actual}"
    )]
    StaleVersion {
        entry_id: String,
        expected_version: u32,
        actual: u32,
    },
    #[error("queue entry {entry_id} not found")]
    NotFound { entry_id: String },
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

pub fn drain_queue(queue: &mut VecDeque<PromptQueueEntry>, mode: QueueMode) -> Vec<AgentMessage> {
    match mode {
        QueueMode::All => queue.drain(..).map(|entry| entry.message).collect(),
        QueueMode::OneAtATime => queue
            .pop_front()
            .map(|entry| entry.message)
            .into_iter()
            .collect(),
    }
}

pub(crate) fn enqueue_message(
    queue: &mut VecDeque<PromptQueueEntry>,
    queue_kind: AgentInputQueue,
    entry: PromptQueueEntry,
) -> Result<(), AgentQueueError> {
    if queue.len() >= MAX_AGENT_QUEUE_ITEMS {
        return Err(AgentQueueError::ItemLimit {
            queue: queue_kind,
            max_items: MAX_AGENT_QUEUE_ITEMS,
        });
    }
    let retained_bytes = queue.iter().fold(0usize, |total, entry| {
        total.saturating_add(message_bytes(&entry.message))
    });
    if retained_bytes.saturating_add(message_bytes(&entry.message)) > MAX_AGENT_QUEUE_BYTES {
        return Err(AgentQueueError::ByteLimit {
            queue: queue_kind,
            max_bytes: MAX_AGENT_QUEUE_BYTES,
        });
    }
    queue.push_back(entry);
    Ok(())
}

pub(crate) fn edit_entry(
    queues: &mut [&mut VecDeque<PromptQueueEntry>],
    entry_id: &str,
    expected_version: u32,
    new_message: AgentMessage,
) -> Result<(), AgentQueueError> {
    for queue in queues.iter_mut() {
        if let Some(entry) = queue.iter_mut().find(|entry| entry.id == entry_id) {
            if entry.version != expected_version {
                return Err(AgentQueueError::StaleVersion {
                    entry_id: entry_id.into(),
                    expected_version,
                    actual: entry.version,
                });
            }
            entry.version += 1;
            entry.message = new_message;
            return Ok(());
        }
    }
    Err(AgentQueueError::NotFound {
        entry_id: entry_id.into(),
    })
}

pub(crate) fn remove_entry(
    queues: &mut [&mut VecDeque<PromptQueueEntry>],
    entry_id: &str,
    expected_version: u32,
) -> Result<(), AgentQueueError> {
    for queue in queues.iter_mut() {
        if let Some(index) = queue.iter().position(|entry| entry.id == entry_id) {
            let entry = &queue[index];
            if entry.version != expected_version {
                return Err(AgentQueueError::StaleVersion {
                    entry_id: entry_id.into(),
                    expected_version,
                    actual: entry.version,
                });
            }
            queue.remove(index);
            return Ok(());
        }
    }
    Err(AgentQueueError::NotFound {
        entry_id: entry_id.into(),
    })
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::types::AgentMessage;

    fn text_entry(id: &str, text: &str) -> PromptQueueEntry {
        PromptQueueEntry {
            id: id.into(),
            version: 0,
            message: AgentMessage::UserText {
                message_id: id.into(),
                text: text.into(),
            },
        }
    }

    #[test]
    fn drain_queue_all_strips_metadata_and_returns_messages_in_order() {
        let mut queue = VecDeque::new();
        queue.push_back(text_entry("steer_0", "first"));
        queue.push_back(text_entry("steer_1", "second"));
        let drained = drain_queue(&mut queue, QueueMode::All);
        assert_eq!(drained.len(), 2);
        assert!(matches!(
            &drained[0],
            AgentMessage::UserText { text, .. } if text == "first"
        ));
        assert!(matches!(
            &drained[1],
            AgentMessage::UserText { text, .. } if text == "second"
        ));
        assert!(queue.is_empty());
    }

    #[test]
    fn drain_queue_one_at_a_time_returns_only_front_entry() {
        let mut queue = VecDeque::new();
        queue.push_back(text_entry("steer_0", "first"));
        queue.push_back(text_entry("steer_1", "second"));
        let drained = drain_queue(&mut queue, QueueMode::OneAtATime);
        assert_eq!(drained.len(), 1);
        assert!(matches!(
            &drained[0],
            AgentMessage::UserText { text, .. } if text == "first"
        ));
        assert_eq!(queue.len(), 1);
    }

    #[test]
    fn enqueue_message_respects_item_limit() {
        let mut queue = VecDeque::new();
        for i in 0..MAX_AGENT_QUEUE_ITEMS {
            enqueue_message(
                &mut queue,
                AgentInputQueue::Steering,
                text_entry(&format!("steer_{i}"), "x"),
            )
            .unwrap();
        }
        let result = enqueue_message(
            &mut queue,
            AgentInputQueue::Steering,
            text_entry("overflow", "x"),
        );
        assert!(matches!(result, Err(AgentQueueError::ItemLimit { .. })));
    }

    #[test]
    fn interjection_queue_kind_display() {
        assert_eq!(AgentInputQueue::Interjection.to_string(), "interjection");
        assert_eq!(AgentInputQueue::Steering.to_string(), "steering");
        assert_eq!(AgentInputQueue::FollowUp.to_string(), "follow-up");
    }
}
