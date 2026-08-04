use std::collections::{HashMap, HashSet};

use crate::session::event::PersistedDelegationStatus;
use crate::session::replay::{MessageStatus, ToolCallStatus, TranscriptItem};
use crate::session::view::CodingAgentSessionTranscriptItem;

/// Projects a replay transcript while restoring provider-executed tools to
/// their position inside the assistant message's ordered content blocks.
///
/// Replay stores a message when it starts and updates that item in place when
/// it completes. Provider tool calls are appended as separate transcript
/// items in between, which otherwise makes a hydrated transcript render the
/// completed assistant text before the tool. Only uniquely identified tools
/// are relocated; incomplete pages and malformed/legacy logs retain their
/// original one-to-one projection.
pub(crate) fn coding_transcript_from_replay(
    items: Vec<TranscriptItem>,
) -> Vec<CodingAgentSessionTranscriptItem> {
    let provider_item_ids = items
        .iter()
        .filter_map(|item| match item {
            TranscriptItem::AssistantMessage { content, .. } => Some(content.as_slice()),
            _ => None,
        })
        .flatten()
        .filter_map(provider_item_id)
        .map(str::to_owned)
        .collect::<HashSet<_>>();

    let mut provider_tools = HashMap::<String, TranscriptItem>::new();
    let mut duplicate_provider_call_ids = HashSet::new();
    for item in &items {
        let Some(provider_call_id) = replay_provider_call_id(item) else {
            continue;
        };
        if provider_tools
            .insert(provider_call_id.to_owned(), item.clone())
            .is_some()
        {
            duplicate_provider_call_ids.insert(provider_call_id.to_owned());
        }
    }
    provider_tools.retain(|provider_call_id, _| {
        provider_item_ids.contains(provider_call_id)
            && !duplicate_provider_call_ids.contains(provider_call_id)
    });
    let relocated_tool_call_ids = provider_tools
        .values()
        .filter_map(replay_tool_call_id)
        .map(str::to_owned)
        .collect::<HashSet<_>>();

    let mut projected = Vec::new();
    let mut emitted_provider_tools = HashSet::new();
    for item in items {
        match item {
            item @ TranscriptItem::AssistantMessage { .. } => {
                projected.extend(project_assistant_with_provider_tools(
                    item,
                    &provider_tools,
                    &mut emitted_provider_tools,
                ))
            }
            TranscriptItem::ToolCall {
                ref tool_call_id, ..
            } if relocated_tool_call_ids.contains(tool_call_id) => {}
            item => projected.push(coding_transcript_item_from_replay(item)),
        }
    }
    projected
}

pub(crate) fn coding_transcript_item_from_replay(
    item: TranscriptItem,
) -> CodingAgentSessionTranscriptItem {
    match item {
        TranscriptItem::UserInput {
            text, started_at, ..
        } => CodingAgentSessionTranscriptItem::User { text, started_at },
        TranscriptItem::AssistantMessage {
            message_id,
            content,
            status,
            reasoning_duration_millis,
            model_id,
            completed_at,
        } => CodingAgentSessionTranscriptItem::Assistant {
            id: message_id,
            text: persisted_content_blocks_text(&content),
            thinking: persisted_content_blocks_thinking(&content),
            images: persisted_content_blocks_images(&content),
            done: !matches!(status, MessageStatus::Started),
            reasoning_duration_millis,
            model_id,
            completed_at,
        },
        TranscriptItem::ToolCall {
            tool_call_id,
            name,
            arguments,
            status,
            summary,
            duration_millis,
            ..
        } => CodingAgentSessionTranscriptItem::Tool {
            call_id: tool_call_id,
            name,
            args: arguments,
            result: if summary.is_empty() {
                None
            } else {
                Some(summary)
            },
            is_error: matches!(status, ToolCallStatus::Failed),
            duration_millis,
        },
        TranscriptItem::DelegationBlock {
            tool_call_id,
            requesting_profile_id,
            target_kind,
            target_id,
            task,
            status,
            child_operation_id,
            summary,
        } => CodingAgentSessionTranscriptItem::Delegation {
            tool_call_id,
            requesting_profile_id,
            target_kind,
            target_id,
            task,
            status: delegation_status_label(status).into(),
            child_operation_id,
            summary,
        },
        TranscriptItem::CompactionSummary { summary, .. } => {
            CodingAgentSessionTranscriptItem::CompactionSummary { summary }
        }
        TranscriptItem::BranchSummary { summary, .. } => {
            CodingAgentSessionTranscriptItem::BranchSummary { summary }
        }
        TranscriptItem::Diagnostic { message, .. } => {
            CodingAgentSessionTranscriptItem::Diagnostic { message }
        }
    }
}

fn project_assistant_with_provider_tools(
    item: TranscriptItem,
    provider_tools: &HashMap<String, TranscriptItem>,
    emitted_provider_tools: &mut HashSet<String>,
) -> Vec<CodingAgentSessionTranscriptItem> {
    let TranscriptItem::AssistantMessage {
        message_id,
        content,
        status,
        reasoning_duration_millis,
        model_id,
        completed_at,
    } = item
    else {
        unreachable!("provider-tool projection only accepts assistant messages");
    };

    let has_provider_tool = content
        .iter()
        .any(|block| provider_item_id(block).is_some_and(|id| provider_tools.contains_key(id)));
    if !has_provider_tool {
        return vec![coding_transcript_item_from_replay(
            TranscriptItem::AssistantMessage {
                message_id,
                content,
                status,
                reasoning_duration_millis,
                model_id,
                completed_at,
            },
        )];
    }

    let mut projected = Vec::new();
    let mut segment = Vec::new();
    let mut segment_index = 0;
    for block in content {
        let provider_tool = provider_item_id(&block)
            .and_then(|id| provider_tools.get(id).map(|tool| (id.to_owned(), tool)));
        let Some((tool_call_id, tool)) = provider_tool else {
            segment.push(block);
            continue;
        };
        if !emitted_provider_tools.insert(tool_call_id) {
            segment.push(block);
            continue;
        }

        push_assistant_segment(
            &mut projected,
            &message_id,
            &mut segment,
            segment_index,
            true,
        );
        projected.push(coding_transcript_item_from_replay(tool.clone()));
        segment_index += 1;
    }
    push_assistant_segment(
        &mut projected,
        &message_id,
        &mut segment,
        segment_index,
        !matches!(status, MessageStatus::Started),
    );

    if let Some(CodingAgentSessionTranscriptItem::Assistant {
        reasoning_duration_millis: segment_reasoning_duration_millis,
        model_id: segment_model_id,
        completed_at: segment_completed_at,
        ..
    }) = projected
        .iter_mut()
        .rev()
        .find(|item| matches!(item, CodingAgentSessionTranscriptItem::Assistant { .. }))
    {
        *segment_reasoning_duration_millis = reasoning_duration_millis;
        *segment_model_id = model_id;
        *segment_completed_at = completed_at;
    }

    projected
}

fn push_assistant_segment(
    projected: &mut Vec<CodingAgentSessionTranscriptItem>,
    message_id: &str,
    content: &mut Vec<crate::session::event::PersistedContentBlock>,
    segment_index: usize,
    done: bool,
) {
    if !content.iter().any(|block| {
        !matches!(
            block,
            crate::session::event::PersistedContentBlock::ProviderItem { .. }
        )
    }) {
        content.clear();
        return;
    }

    projected.push(coding_transcript_item_from_replay(
        TranscriptItem::AssistantMessage {
            message_id: assistant_segment_id(message_id, segment_index),
            content: std::mem::take(content),
            status: if done {
                MessageStatus::Completed
            } else {
                MessageStatus::Started
            },
            reasoning_duration_millis: None,
            model_id: None,
            completed_at: None,
        },
    ));
}

pub(super) fn assistant_segment_id(message_id: &str, segment_index: usize) -> String {
    if segment_index == 0 {
        message_id.to_owned()
    } else {
        format!("{message_id}:segment:{segment_index}")
    }
}

fn provider_item_id(block: &crate::session::event::PersistedContentBlock) -> Option<&str> {
    let crate::session::event::PersistedContentBlock::ProviderItem { item, .. } = block else {
        return None;
    };
    item.get("id")?.as_str()
}

fn replay_provider_call_id(item: &TranscriptItem) -> Option<&str> {
    let TranscriptItem::ToolCall {
        tool_call_id,
        arguments,
        ..
    } = item
    else {
        return None;
    };
    arguments
        .get("id")
        .and_then(serde_json::Value::as_str)
        .or(Some(tool_call_id))
}

fn replay_tool_call_id(item: &TranscriptItem) -> Option<&str> {
    let TranscriptItem::ToolCall { tool_call_id, .. } = item else {
        return None;
    };
    Some(tool_call_id)
}

fn delegation_status_label(status: PersistedDelegationStatus) -> &'static str {
    match status {
        PersistedDelegationStatus::Requested => "requested",
        PersistedDelegationStatus::Running => "running",
        PersistedDelegationStatus::Completed => "completed",
        PersistedDelegationStatus::Failed => "failed",
        PersistedDelegationStatus::Rejected => "rejected",
        PersistedDelegationStatus::Cancelled => "cancelled",
        PersistedDelegationStatus::ConfirmationRequired => "confirmation_required",
    }
}

fn persisted_content_blocks_text(
    content: &[crate::session::event::PersistedContentBlock],
) -> String {
    content
        .iter()
        .filter_map(|block| match block {
            crate::session::event::PersistedContentBlock::Text { text } => Some(text.clone()),
            crate::session::event::PersistedContentBlock::Thinking { .. }
            | crate::session::event::PersistedContentBlock::Image { .. }
            | crate::session::event::PersistedContentBlock::ProviderItem { .. } => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn persisted_content_blocks_thinking(
    content: &[crate::session::event::PersistedContentBlock],
) -> String {
    content
        .iter()
        .filter_map(|block| match block {
            crate::session::event::PersistedContentBlock::Thinking { thinking, .. } => {
                Some(thinking.clone())
            }
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn persisted_content_blocks_images(
    content: &[crate::session::event::PersistedContentBlock],
) -> Vec<crate::events::CodingAgentImageContent> {
    content
        .iter()
        .filter_map(|block| match block {
            crate::session::event::PersistedContentBlock::Image { mime_type, data } => {
                Some(crate::events::CodingAgentImageContent {
                    mime_type: mime_type.clone(),
                    data: data.clone(),
                })
            }
            _ => None,
        })
        .collect()
}
