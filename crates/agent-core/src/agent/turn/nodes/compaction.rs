use ai_protocol::api::conversation::Usage;

use crate::agent::types::{AgentEvent, AgentMessage};
use crate::compaction::estimate::estimate_context_tokens;
use crate::compaction::prepare::{prepare_compaction, should_compact};
use crate::compaction::summarize::summarize_with_provider_streamer;

use super::super::context::{AgentTurnContext, RuntimeCompactionState};
use super::{AgentTurnError, unique_message_id};

pub(crate) async fn maybe_compact_runtime_context(
    ctx: &mut AgentTurnContext,
) -> Result<(), AgentTurnError> {
    let Some(config) = ctx.config.compaction.clone() else {
        return Ok(());
    };

    let bytes_per_token = config.settings.token_estimation.bytes_per_token;
    let usage_estimate = estimate_context_tokens(&ctx.messages, bytes_per_token);
    let tokens_before = usage_estimate.tokens;
    if !should_compact(
        tokens_before,
        ctx.config.model.context_window,
        &config.settings,
    ) {
        return Ok(());
    }

    let (mut to_summarize, mut keep) = prepare_compaction(&ctx.messages, &config.settings);
    if to_summarize.is_empty() {
        (to_summarize, keep) =
            split_after_usage_anchor(&ctx.messages, usage_estimate.last_usage_index);
    }
    if to_summarize.is_empty() {
        return Ok(());
    }

    let summary = match summarize_with_provider_streamer(
        &ctx.config.model,
        &to_summarize,
        config.custom_instructions.as_deref(),
        ctx.config.stream_options.clone(),
        Some(ctx.cancel_token.clone()),
        ctx.config.provider_streamer.clone(),
        Some(&config.settings.sampler),
        config.settings.summary_max_chars,
    )
    .await
    {
        Ok(summary) => summary,
        Err(error) => {
            format!(
                "Compaction fallback: summarization failed ({}). History truncated without summary.",
                error
            )
        }
    };

    let first_kept_message_id = keep.first().map(message_id).unwrap_or("none").to_string();
    for message in &mut keep {
        clear_assistant_usage(message);
    }

    let mut compacted = Vec::with_capacity(1 + keep.len());
    compacted.push(AgentMessage::CompactionSummary {
        message_id: unique_message_id(&ctx.messages, format!("compaction_{}", tokens_before)),
        summary: summary.clone(),
        tokens_before,
    });
    compacted.extend(keep);
    ctx.messages = compacted;

    ctx.runtime_compaction = RuntimeCompactionState {
        summary: Some(summary.clone()),
        first_kept_message_id: Some(first_kept_message_id.clone()),
        tokens_before: Some(tokens_before),
    };
    ctx.emit(AgentEvent::SessionCompacted {
        summary,
        first_kept_message_id,
        tokens_before,
        details: None,
    });

    Ok(())
}

fn message_id(message: &AgentMessage) -> &str {
    match message {
        AgentMessage::UserText { message_id, .. }
        | AgentMessage::Assistant { message_id, .. }
        | AgentMessage::ToolResult { message_id, .. }
        | AgentMessage::SystemPrompt { message_id, .. }
        | AgentMessage::CompactionSummary { message_id, .. }
        | AgentMessage::BashExecution { message_id, .. }
        | AgentMessage::Custom { message_id, .. }
        | AgentMessage::BranchSummary { message_id, .. } => message_id,
    }
}

fn clear_assistant_usage(message: &mut AgentMessage) {
    if let AgentMessage::Assistant { message, .. } = message {
        message.usage = Usage::default();
    }
}

fn split_after_usage_anchor(
    messages: &[AgentMessage],
    anchor_index: Option<usize>,
) -> (Vec<AgentMessage>, Vec<AgentMessage>) {
    let Some(anchor_index) = anchor_index else {
        return (vec![], messages.to_vec());
    };
    if messages.is_empty() {
        return (vec![], vec![]);
    }

    let mut split = anchor_index.saturating_add(1).min(messages.len());
    while split < messages.len() && matches!(messages[split], AgentMessage::ToolResult { .. }) {
        split += 1;
    }

    (messages[..split].to_vec(), messages[split..].to_vec())
}
