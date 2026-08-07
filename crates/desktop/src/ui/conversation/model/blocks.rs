//! Bounded conversation block construction from product transcript items.

use coding_agent::api::view::CodingAgentSessionTranscriptItem;

use super::{
    ConversationBlock, ConversationBlockKind, DELEGATION_TITLE_PREFIX, DelegationMeta,
    DelegationStatus, MAX_BLOCK_TEXT_BYTES, MAX_THINKING_TEXT_BYTES, MAX_TOOL_ARGUMENT_BYTES,
};
use crate::ui::conversation::copy::truncate_bytes;

pub(crate) fn block_from_product(
    index: usize,
    item: CodingAgentSessionTranscriptItem,
) -> ConversationBlock {
    let reasoning_duration_millis = match &item {
        CodingAgentSessionTranscriptItem::Assistant {
            reasoning_duration_millis,
            ..
        } => *reasoning_duration_millis,
        _ => None,
    };
    let model = match &item {
        CodingAgentSessionTranscriptItem::Assistant { model_id, .. } => model_id.clone(),
        _ => None,
    };
    let started_at = match &item {
        CodingAgentSessionTranscriptItem::User { started_at, .. } => started_at.clone(),
        _ => None,
    };
    let completed_at = match &item {
        CodingAgentSessionTranscriptItem::Assistant { completed_at, .. } => completed_at.clone(),
        _ => None,
    };
    let (kind, source_id, title, text, detail, done, is_error, image_count, truncated, delegation) =
        match item {
            CodingAgentSessionTranscriptItem::User { text, .. } => {
                let (text, truncated) = truncate_bytes(text, MAX_BLOCK_TEXT_BYTES);
                (
                    ConversationBlockKind::User,
                    String::new(),
                    "You".into(),
                    text,
                    String::new(),
                    true,
                    false,
                    0,
                    truncated,
                    None,
                )
            }
            CodingAgentSessionTranscriptItem::Assistant {
                id,
                text,
                thinking,
                images,
                done,
                ..
            } => {
                let (text, text_truncated) = truncate_bytes(text, MAX_BLOCK_TEXT_BYTES);
                let (thinking, thinking_truncated) =
                    truncate_bytes(thinking, MAX_THINKING_TEXT_BYTES);
                (
                    ConversationBlockKind::Assistant,
                    id,
                    "Assistant".into(),
                    text,
                    thinking,
                    done,
                    false,
                    images.len(),
                    text_truncated || thinking_truncated,
                    None,
                )
            }
            CodingAgentSessionTranscriptItem::Tool {
                call_id,
                name,
                args,
                result,
                is_error,
                duration_millis,
            } => {
                let arguments = serde_json::to_string_pretty(&args)
                    .unwrap_or_else(|_| "<invalid tool arguments>".into());
                let (arguments, args_truncated) =
                    truncate_bytes(arguments, MAX_TOOL_ARGUMENT_BYTES);
                let (result, result_truncated) =
                    truncate_bytes(result.unwrap_or_default(), MAX_BLOCK_TEXT_BYTES);
                (
                    ConversationBlockKind::Tool,
                    call_id,
                    tool_title(&name, duration_millis),
                    result,
                    arguments,
                    true,
                    is_error,
                    0,
                    args_truncated || result_truncated,
                    None,
                )
            }
            CodingAgentSessionTranscriptItem::Delegation {
                tool_call_id,
                target_kind,
                target_id,
                task,
                status,
                summary,
                ..
            } => {
                let (task, task_truncated) = truncate_bytes(task, MAX_BLOCK_TEXT_BYTES);
                let (summary, summary_truncated) =
                    truncate_bytes(summary.unwrap_or_default(), MAX_BLOCK_TEXT_BYTES);
                (
                    ConversationBlockKind::Delegation,
                    tool_call_id,
                    format!("{DELEGATION_TITLE_PREFIX}{target_kind:?}"),
                    task,
                    summary,
                    true,
                    false,
                    0,
                    task_truncated || summary_truncated,
                    Some(DelegationMeta {
                        target_id: target_id.to_string(),
                        status: DelegationStatus::parse(&status),
                    }),
                )
            }
            CodingAgentSessionTranscriptItem::CompactionSummary { summary } => summary_block(
                ConversationBlockKind::CompactionSummary,
                "Compaction",
                summary,
            ),
            CodingAgentSessionTranscriptItem::BranchSummary { summary } => summary_block(
                ConversationBlockKind::BranchSummary,
                "Branch summary",
                summary,
            ),
            CodingAgentSessionTranscriptItem::Diagnostic { message } => {
                let (message, truncated) = truncate_bytes(message, MAX_BLOCK_TEXT_BYTES);
                (
                    ConversationBlockKind::Diagnostic,
                    String::new(),
                    "Diagnostic".into(),
                    message,
                    String::new(),
                    true,
                    true,
                    0,
                    truncated,
                    None,
                )
            }
        };
    let id = if source_id.is_empty() {
        format!("{index:08}:{}", kind.key())
    } else {
        format!("{}:{source_id}", kind.key())
    };
    let mut block = ConversationBlock {
        id,
        source_revision: 0,
        kind,
        title,
        text,
        detail,
        done,
        is_error,
        image_count,
        reasoning_duration_millis,
        truncated,
        model,
        started_at,
        completed_at,
        turn: None,
        delegation,
    };
    block.refresh_source_revision();
    block
}

pub(crate) fn tool_title(name: &str, duration_millis: Option<u64>) -> String {
    match duration_millis {
        Some(duration_millis) => {
            format!("Tool · {name} · {}", compact_duration(duration_millis))
        }
        None => format!("Tool · {name}"),
    }
}

/// Formats an authoritative lifecycle duration using stable compact units.
pub fn compact_duration(duration_millis: u64) -> String {
    if duration_millis < 1_000 {
        return format!("{duration_millis} ms");
    }
    if duration_millis < 60_000 {
        let rounded_tenths = duration_millis.saturating_add(50) / 100;
        if rounded_tenths < 600 {
            return format!("{}.{:01} s", rounded_tenths / 10, rounded_tenths % 10);
        }
    }
    let rounded_seconds = duration_millis.saturating_add(500) / 1_000;
    format!("{}m {:02}s", rounded_seconds / 60, rounded_seconds % 60)
}

pub(crate) fn conversation_block_revision(block: &ConversationBlock) -> u64 {
    const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

    fn update(mut hash: u64, bytes: &[u8]) -> u64 {
        for byte in bytes {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(FNV_PRIME);
        }
        hash
    }

    let mut hash = FNV_OFFSET;
    for value in [
        block.id.as_bytes(),
        block.kind.key().as_bytes(),
        block.title.as_bytes(),
        block.text.as_bytes(),
        block.detail.as_bytes(),
    ] {
        hash = update(hash, &(value.len() as u64).to_le_bytes());
        hash = update(hash, value);
    }
    hash = update(hash, &[u8::from(block.done)]);
    hash = update(hash, &[u8::from(block.is_error)]);
    hash = update(hash, &(block.image_count as u64).to_le_bytes());
    hash = update(
        hash,
        &block
            .reasoning_duration_millis
            .unwrap_or(u64::MAX)
            .to_le_bytes(),
    );
    if let Some(model) = &block.model {
        hash = update(hash, &(model.len() as u64).to_le_bytes());
        hash = update(hash, model.as_bytes());
    }
    if let Some(started_at) = &block.started_at {
        hash = update(hash, &(started_at.len() as u64).to_le_bytes());
        hash = update(hash, started_at.as_bytes());
    }
    if let Some(completed_at) = &block.completed_at {
        hash = update(hash, &(completed_at.len() as u64).to_le_bytes());
        hash = update(hash, completed_at.as_bytes());
    }
    if let Some(turn) = &block.turn {
        hash = update(hash, &(turn.model.len() as u64).to_le_bytes());
        hash = update(hash, turn.model.as_bytes());
        hash = update(
            hash,
            &turn.duration_millis.unwrap_or(u64::MAX).to_le_bytes(),
        );
    }
    // A status-only transition (running -> cancelled, unchanged task and
    // summary) must still invalidate the render cache, or the header would
    // keep showing the stale status.
    if let Some(delegation) = &block.delegation {
        hash = update(hash, &(delegation.target_id.len() as u64).to_le_bytes());
        hash = update(hash, delegation.target_id.as_bytes());
        hash = update(hash, &[delegation.status as u8]);
    }
    update(hash, &[u8::from(block.truncated)])
}

pub(crate) fn summary_block(
    kind: ConversationBlockKind,
    title: &str,
    summary: String,
) -> (
    ConversationBlockKind,
    String,
    String,
    String,
    String,
    bool,
    bool,
    usize,
    bool,
    Option<DelegationMeta>,
) {
    let (summary, truncated) = truncate_bytes(summary, MAX_BLOCK_TEXT_BYTES);
    (
        kind,
        String::new(),
        title.into(),
        summary,
        String::new(),
        true,
        false,
        0,
        truncated,
        None,
    )
}
