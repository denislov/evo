use crate::session::event::PersistedDelegationStatus;
use crate::session::replay::{MessageStatus, ToolCallStatus, TranscriptItem};
use crate::session::view::CodingAgentSessionTranscriptItem;

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
