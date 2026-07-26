use crate::agent::types::AgentMessage;
use ai::api::conversation::{ContentBlock, StopReason, Usage};

pub fn estimate_tokens(messages: &[AgentMessage]) -> u32 {
    let mut total: u32 = 0;

    for msg in messages {
        total = total.saturating_add(match msg {
            AgentMessage::UserText { text, .. } => estimate_text_tokens(text),
            AgentMessage::Assistant { message, .. } => estimate_content_tokens(&message.content),
            AgentMessage::ToolResult { content, .. } => estimate_content_tokens(content),
            AgentMessage::SystemPrompt { text, .. } => estimate_text_tokens(text),
            AgentMessage::CompactionSummary { summary, .. } => estimate_text_tokens(summary),
            AgentMessage::BashExecution {
                command,
                output,
                exclude_from_context,
                ..
            } => {
                if !exclude_from_context {
                    estimate_text_tokens(command).saturating_add(estimate_text_tokens(output))
                } else {
                    0
                }
            }
            AgentMessage::Custom { content, .. } => estimate_content_tokens(content),
            AgentMessage::BranchSummary { summary, .. } => estimate_text_tokens(summary),
        });
    }

    total
}

fn estimate_text_tokens(text: &str) -> u32 {
    u32::try_from(text.len().div_ceil(4)).unwrap_or(u32::MAX)
}

fn estimate_content_tokens(content: &[ContentBlock]) -> u32 {
    content
        .iter()
        .map(estimate_block_tokens)
        .fold(0u32, u32::saturating_add)
}

fn estimate_block_tokens(block: &ContentBlock) -> u32 {
    match block {
        ContentBlock::Text { text, .. } => estimate_text_tokens(text),
        ContentBlock::ToolCall {
            name, arguments, ..
        } => {
            estimate_text_tokens(name).saturating_add(estimate_text_tokens(&arguments.to_string()))
        }
        ContentBlock::Thinking { thinking, .. } => estimate_text_tokens(thinking),
        ContentBlock::Image { .. } => 4800u32.div_ceil(4),
    }
}

// ── Context usage estimation (TS parity) ───────────

/// Total context tokens implied by a provider [`Usage`].
///
/// Mirrors TS `calculateContextTokens` in `compaction.ts`: prefer the
/// native `total_tokens` field, falling back to the component sum.
pub fn calculate_context_tokens(usage: &Usage) -> u32 {
    if usage.total_tokens > 0 {
        usage.total_tokens
    } else {
        usage
            .input
            .saturating_add(usage.output)
            .saturating_add(usage.cache_read)
            .saturating_add(usage.cache_write)
    }
}

/// Result of estimating active context usage from a message history.
///
/// Mirrors TS `ContextUsageEstimate` in `compaction.ts`. The last valid
/// assistant usage (if any) anchors the estimate; messages after that
/// anchor are estimated heuristically.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextUsageEstimate {
    pub tokens: u32,
    pub usage_tokens: u32,
    pub trailing_tokens: u32,
    pub last_usage_index: Option<usize>,
}

/// Find the last assistant message with valid usage, walking newest first.
///
/// Mirrors TS `getAssistantUsage` / `getLastAssistantUsageInfo`: skip
/// aborted, error, and all-zero usages, since they carry no reliable
/// context-size signal.
fn last_valid_assistant_usage(messages: &[AgentMessage]) -> Option<(Usage, usize)> {
    for (index, msg) in messages.iter().enumerate().rev() {
        if let AgentMessage::Assistant { message, .. } = msg {
            if message.stop_reason == StopReason::Error
                || message.stop_reason == StopReason::Aborted
            {
                continue;
            }
            let tokens = calculate_context_tokens(&message.usage);
            if tokens > 0 {
                return Some((message.usage.clone(), index));
            }
        }
    }
    None
}

/// Estimate active context tokens from a message history.
///
/// Mirrors TS `estimateContextTokens` in `compaction.ts`:
/// - Prefer the last successful assistant usage as the context anchor.
/// - Add heuristic estimates only for messages after that usage.
/// - Fall back to heuristic estimation for all messages when no valid
///   usage exists.
///
/// [`estimate_tokens`] is deliberately heuristic and does not read assistant
/// usage; this function is the only compaction estimator that should use
/// provider usage, and only for the newest valid anchor.
pub fn estimate_context_tokens(messages: &[AgentMessage]) -> ContextUsageEstimate {
    let Some((usage, index)) = last_valid_assistant_usage(messages) else {
        let trailing = estimate_tokens(messages);
        return ContextUsageEstimate {
            tokens: trailing,
            usage_tokens: 0,
            trailing_tokens: trailing,
            last_usage_index: None,
        };
    };

    let usage_tokens = calculate_context_tokens(&usage);
    let trailing_tokens = if index + 1 < messages.len() {
        estimate_tokens(&messages[index + 1..])
    } else {
        0
    };

    ContextUsageEstimate {
        tokens: usage_tokens.saturating_add(trailing_tokens),
        usage_tokens,
        trailing_tokens,
        last_usage_index: Some(index),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn estimates_text_from_chars() {
        let msgs = vec![AgentMessage::UserText {
            message_id: "1".into(),
            text: "hello world this is a test".into(),
        }];
        let tokens = estimate_tokens(&msgs);
        assert!(tokens > 0);
    }

    #[test]
    fn estimate_tokens_uses_assistant_content_not_provider_usage() {
        use ai::api::conversation::AssistantMessage;
        let mut msg = AssistantMessage::empty("test", "test-model");
        msg.usage.total_tokens = 42;
        msg.content.push(ContentBlock::Text {
            text: "tiny".into(),
            text_signature: None,
        });
        let msgs = vec![AgentMessage::Assistant {
            message_id: "2".into(),
            message: msg,
        }];
        let tokens = estimate_tokens(&msgs);
        assert_eq!(tokens, 1);
    }

    // ---- estimate_context_tokens (TS-parity context usage) ----

    use ai::api::conversation::{AssistantMessage, StopReason};

    fn user_msg(text: &str) -> AgentMessage {
        AgentMessage::UserText {
            message_id: "u".into(),
            text: text.into(),
        }
    }

    fn assistant_with_usage(total_tokens: u32, stop_reason: StopReason) -> AgentMessage {
        let mut msg = AssistantMessage::empty("test", "test-model");
        msg.usage.total_tokens = total_tokens;
        msg.stop_reason = stop_reason;
        AgentMessage::Assistant {
            message_id: "a".into(),
            message: msg,
        }
    }

    fn assistant_with_components(
        input: u32,
        output: u32,
        cache_read: u32,
        cache_write: u32,
        stop_reason: StopReason,
    ) -> AgentMessage {
        let mut msg = AssistantMessage::empty("test", "test-model");
        msg.usage.input = input;
        msg.usage.output = output;
        msg.usage.cache_read = cache_read;
        msg.usage.cache_write = cache_write;
        msg.stop_reason = stop_reason;
        AgentMessage::Assistant {
            message_id: "a".into(),
            message: msg,
        }
    }

    #[test]
    fn estimate_context_uses_last_valid_assistant_usage_as_anchor() {
        let msgs = vec![
            assistant_with_usage(40, StopReason::Stop),
            user_msg("hello there trailing text"),
        ];
        let est = estimate_context_tokens(&msgs);
        assert_eq!(est.usage_tokens, 40);
        assert_eq!(est.last_usage_index, Some(0));
        let trailing_heuristic = estimate_tokens(&[msgs[1].clone()]);
        assert_eq!(est.trailing_tokens, trailing_heuristic);
        assert_eq!(est.tokens, 40 + trailing_heuristic);
    }

    #[test]
    fn estimate_context_skips_aborted_assistant_usage() {
        let msgs = vec![
            assistant_with_usage(100, StopReason::Stop),
            assistant_with_usage(200, StopReason::Aborted),
        ];
        let est = estimate_context_tokens(&msgs);
        assert_eq!(est.usage_tokens, 100);
        assert_eq!(est.last_usage_index, Some(0));
        assert_eq!(est.trailing_tokens, 0);
        assert_eq!(est.tokens, 100);
    }

    #[test]
    fn estimate_context_skips_error_assistant_usage() {
        let msgs = vec![
            assistant_with_usage(100, StopReason::Stop),
            assistant_with_usage(200, StopReason::Error),
        ];
        let est = estimate_context_tokens(&msgs);
        assert_eq!(est.usage_tokens, 100);
        assert_eq!(est.last_usage_index, Some(0));
        assert_eq!(est.trailing_tokens, 0);
        assert_eq!(est.tokens, 100);
    }

    #[test]
    fn estimate_context_skips_all_zero_usage() {
        let mut zero = AssistantMessage::empty("test", "test-model");
        // usage stays default (all zero), stop_reason is Stop (valid reason)
        zero.stop_reason = StopReason::Stop;
        let msgs = vec![
            assistant_with_usage(50, StopReason::Stop),
            AgentMessage::Assistant {
                message_id: "a2".into(),
                message: zero,
            },
        ];
        let est = estimate_context_tokens(&msgs);
        assert_eq!(est.usage_tokens, 50);
        assert_eq!(est.last_usage_index, Some(0));
        assert_eq!(est.trailing_tokens, 0);
        assert_eq!(est.tokens, 50);
    }

    #[test]
    fn estimate_context_uses_component_sum_when_total_zero() {
        // total_tokens is 0 but component sum > 0 → still valid usage.
        let msgs = vec![assistant_with_components(30, 10, 5, 0, StopReason::Stop)];
        let est = estimate_context_tokens(&msgs);
        assert_eq!(est.usage_tokens, 45); // 30 + 10 + 5 + 0
        assert_eq!(est.last_usage_index, Some(0));
    }

    #[test]
    fn estimate_context_falls_back_to_heuristic_when_no_valid_usage() {
        let msgs = vec![
            user_msg("hello world this is some text"),
            assistant_with_usage(0, StopReason::Stop), // zero usage, skipped
        ];
        let est = estimate_context_tokens(&msgs);
        assert_eq!(est.usage_tokens, 0);
        assert_eq!(est.last_usage_index, None);
        let heuristic = estimate_tokens(&msgs);
        assert_eq!(est.tokens, heuristic);
        assert_eq!(est.trailing_tokens, heuristic);
    }

    #[test]
    fn estimate_context_fallback_does_not_count_error_usage_as_message_size() {
        let msgs = vec![assistant_with_usage(10_000, StopReason::Error)];

        let est = estimate_context_tokens(&msgs);

        assert_eq!(est.last_usage_index, None);
        assert_eq!(est.usage_tokens, 0);
        assert_eq!(est.trailing_tokens, 0);
        assert_eq!(est.tokens, 0);
    }

    #[test]
    fn estimate_context_picks_newest_valid_usage_when_multiple() {
        // Mirrors the inflation bug: the newest valid usage wins, older
        // assistant usages are NOT summed in.
        let msgs = vec![
            assistant_with_usage(4_000, StopReason::Stop),
            assistant_with_usage(8_000, StopReason::Stop),
            assistant_with_usage(12_000, StopReason::Stop),
        ];
        let est = estimate_context_tokens(&msgs);
        assert_eq!(est.usage_tokens, 12_000);
        assert_eq!(est.last_usage_index, Some(2));
        assert_eq!(est.trailing_tokens, 0);
        assert_eq!(est.tokens, 12_000);
    }
}
