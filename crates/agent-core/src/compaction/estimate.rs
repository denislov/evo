use crate::agent::types::AgentMessage;
use ai_protocol::api::conversation::{ContentBlock, StopReason, Usage};

/// Per-model token estimation tuning.
///
/// Compaction estimates tokens from byte length using a fixed
/// `bytes_per_token` ratio. Most providers target ~4 bytes/token for
/// English-heavy text; models with different tokenizers can override this so
/// compaction triggers at the right context pressure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TokenEstimationConfig {
    pub bytes_per_token: u32,
}

impl Default for TokenEstimationConfig {
    fn default() -> Self {
        Self { bytes_per_token: 4 }
    }
}

pub fn estimate_tokens(messages: &[AgentMessage], bytes_per_token: u32) -> u32 {
    let mut total: u32 = 0;

    for msg in messages {
        total = total.saturating_add(match msg {
            AgentMessage::UserText { text, .. } => estimate_text_tokens(text, bytes_per_token),
            AgentMessage::Assistant { message, .. } => {
                estimate_content_tokens(&message.content, bytes_per_token)
            }
            AgentMessage::ToolResult { content, .. } => {
                estimate_content_tokens(content, bytes_per_token)
            }
            AgentMessage::SystemPrompt { text, .. } => estimate_text_tokens(text, bytes_per_token),
            AgentMessage::CompactionSummary { summary, .. } => {
                estimate_text_tokens(summary, bytes_per_token)
            }
            AgentMessage::BashExecution {
                command,
                output,
                exclude_from_context,
                ..
            } => {
                if !exclude_from_context {
                    estimate_text_tokens(command, bytes_per_token)
                        .saturating_add(estimate_text_tokens(output, bytes_per_token))
                } else {
                    0
                }
            }
            AgentMessage::Custom { content, .. } => {
                estimate_content_tokens(content, bytes_per_token)
            }
            AgentMessage::BranchSummary { summary, .. } => {
                estimate_text_tokens(summary, bytes_per_token)
            }
        });
    }

    total
}

fn estimate_text_tokens(text: &str, bytes_per_token: u32) -> u32 {
    let bpt = bytes_per_token.max(1) as usize;
    u32::try_from(text.len().div_ceil(bpt)).unwrap_or(u32::MAX)
}

fn estimate_content_tokens(content: &[ContentBlock], bytes_per_token: u32) -> u32 {
    content
        .iter()
        .map(|b| estimate_block_tokens(b, bytes_per_token))
        .fold(0u32, u32::saturating_add)
}

fn estimate_block_tokens(block: &ContentBlock, bytes_per_token: u32) -> u32 {
    match block {
        ContentBlock::Text { text, .. } => estimate_text_tokens(text, bytes_per_token),
        ContentBlock::ToolCall {
            name, arguments, ..
        } => estimate_text_tokens(name, bytes_per_token).saturating_add(estimate_text_tokens(
            &arguments.to_string(),
            bytes_per_token,
        )),
        ContentBlock::Thinking { thinking, .. } => estimate_text_tokens(thinking, bytes_per_token),
        ContentBlock::Image { .. } => 4800u32.div_ceil(bytes_per_token.max(1)),
        ContentBlock::ProviderItem { item, .. } => {
            estimate_text_tokens(&item.to_string(), bytes_per_token)
        }
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
pub fn estimate_context_tokens(
    messages: &[AgentMessage],
    bytes_per_token: u32,
) -> ContextUsageEstimate {
    let Some((usage, index)) = last_valid_assistant_usage(messages) else {
        let trailing = estimate_tokens(messages, bytes_per_token);
        return ContextUsageEstimate {
            tokens: trailing,
            usage_tokens: 0,
            trailing_tokens: trailing,
            last_usage_index: None,
        };
    };

    let usage_tokens = calculate_context_tokens(&usage);
    let trailing_tokens = if index + 1 < messages.len() {
        estimate_tokens(&messages[index + 1..], bytes_per_token)
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
    use ai_protocol::api::conversation::AssistantMessage;

    fn user(text: &str) -> AgentMessage {
        AgentMessage::UserText {
            message_id: "u".into(),
            text: text.into(),
        }
    }

    fn assistant_with_usage(usage: Usage, stop_reason: StopReason) -> AgentMessage {
        let mut message = AssistantMessage::empty("api", "model");
        message.usage = usage;
        message.stop_reason = stop_reason;
        AgentMessage::Assistant {
            message_id: "a".into(),
            message,
        }
    }

    fn usage(input: u32, output: u32, total: u32) -> Usage {
        Usage {
            input,
            output,
            reasoning_tokens: 0,
            cache_read: 0,
            cache_write: 0,
            total_tokens: total,
            cost: Default::default(),
        }
    }

    #[test]
    fn token_estimate_is_roughly_bytes_over_four() {
        assert_eq!(estimate_text_tokens("abcdefgh", 4), 2);
        assert_eq!(estimate_text_tokens("abcde", 4), 2);
    }

    #[test]
    fn estimate_tokens_with_custom_bytes_per_token() {
        let messages = vec![user("abcdefgh")];
        assert_eq!(estimate_tokens(&messages, 4), 2);
        assert_eq!(estimate_tokens(&messages, 2), 4);
        assert_eq!(estimate_tokens(&messages, 8), 1);
    }

    #[test]
    fn usage_anchor_dominates_the_estimate() {
        let messages = vec![
            assistant_with_usage(usage(100, 50, 150), StopReason::Stop),
            user("trailing text"),
        ];
        let estimate = estimate_context_tokens(&messages, 4);
        assert_eq!(estimate.usage_tokens, 150);
        assert!(estimate.trailing_tokens > 0);
        assert_eq!(
            estimate.tokens,
            estimate.usage_tokens + estimate.trailing_tokens
        );
        assert_eq!(estimate.last_usage_index, Some(0));
    }

    #[test]
    fn aborted_and_zero_usages_are_not_anchors() {
        let messages = vec![
            assistant_with_usage(usage(0, 0, 0), StopReason::Stop),
            assistant_with_usage(usage(10, 20, 30), StopReason::Aborted),
            user("text"),
        ];
        let estimate = estimate_context_tokens(&messages, 4);
        assert_eq!(estimate.usage_tokens, 0);
        assert_eq!(estimate.last_usage_index, None);
        assert_eq!(estimate.tokens, estimate.trailing_tokens);
    }

    #[test]
    fn newest_valid_usage_wins() {
        let messages = vec![
            assistant_with_usage(usage(100, 100, 200), StopReason::Stop),
            user("x"),
            assistant_with_usage(usage(200, 100, 300), StopReason::Stop),
        ];
        let estimate = estimate_context_tokens(&messages, 4);
        assert_eq!(estimate.usage_tokens, 300);
        assert_eq!(estimate.last_usage_index, Some(2));
        assert_eq!(estimate.trailing_tokens, 0);
    }

    #[test]
    fn calculate_context_tokens_falls_back_to_component_sum() {
        let with_total = Usage {
            input: 1,
            output: 2,
            reasoning_tokens: 0,
            cache_read: 3,
            cache_write: 4,
            total_tokens: 100,
            cost: Default::default(),
        };
        assert_eq!(calculate_context_tokens(&with_total), 100);

        let without_total = Usage {
            input: 1,
            output: 2,
            reasoning_tokens: 0,
            cache_read: 3,
            cache_write: 4,
            total_tokens: 0,
            cost: Default::default(),
        };
        assert_eq!(calculate_context_tokens(&without_total), 10);
    }
}
