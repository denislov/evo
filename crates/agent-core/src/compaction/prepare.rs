use crate::agent::types::{AgentMessage, CompactionSettings};
use crate::compaction::estimate::estimate_tokens;

/// Check if compaction should trigger based on context usage.
///
/// Mirrors `shouldCompact` in `pi/packages/coding-agent/src/core/compaction/compaction.ts`:
/// returns `false` when compaction is disabled via `settings.enabled`, otherwise
/// returns `true` once the estimated context tokens exceed
/// `context_window - settings.reserve_tokens`.
pub fn should_compact(
    estimated_tokens: u32,
    context_window: u32,
    settings: &CompactionSettings,
) -> bool {
    if !settings.enabled {
        return false;
    }
    context_window > 0 && estimated_tokens > context_window.saturating_sub(settings.reserve_tokens)
}

pub fn prepare_compaction(
    messages: &[AgentMessage],
    settings: &CompactionSettings,
) -> (Vec<AgentMessage>, Vec<AgentMessage>) {
    if messages.is_empty() {
        return (vec![], vec![]);
    }

    let estimated = estimate_tokens(messages);
    let total_context_window = settings
        .reserve_tokens
        .saturating_add(settings.keep_recent_tokens);

    if estimated <= total_context_window {
        return (vec![], messages.to_vec());
    }

    let mut keep_recent: Vec<AgentMessage> = Vec::new();
    let mut keep_tokens: u32 = 0;
    let mut i = messages.len();
    // Tool results at the tail are skipped to locate the assistant message
    // that produced them; they are restored after it below. Skipping without
    // restoring used to drop those results from both the keep list and the
    // summarization input, leaving tool calls without their results in the
    // context.
    let mut skipped_tool_results: Vec<AgentMessage> = Vec::new();

    while i > 0 {
        i -= 1;
        let msg = &messages[i];

        if matches!(msg, AgentMessage::ToolResult { .. }) && keep_recent.is_empty() {
            skipped_tool_results.push(msg.clone());
            continue;
        }

        let msg_tokens = estimate_tokens(std::slice::from_ref(msg));
        if keep_tokens.saturating_add(msg_tokens) > settings.keep_recent_tokens
            && !keep_recent.is_empty()
        {
            i += 1;
            break;
        }

        keep_recent.insert(0, msg.clone());
        keep_tokens = keep_tokens.saturating_add(msg_tokens);
    }

    // `skipped_tool_results` is newest-first; reverse it so the results
    // follow the assistant they belong to in the final keep list.
    if !skipped_tool_results.is_empty() {
        skipped_tool_results.reverse();
        keep_recent.extend(skipped_tool_results);
    }

    let to_summarize: Vec<AgentMessage> = messages[..i].to_vec();

    (to_summarize, keep_recent)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user(message_id: &str, text: &str) -> AgentMessage {
        AgentMessage::UserText {
            message_id: message_id.into(),
            text: text.into(),
        }
    }

    fn tool_result(tool_call_id: &str, text: &str) -> AgentMessage {
        AgentMessage::ToolResult {
            message_id: tool_call_id.into(),
            tool_call_id: tool_call_id.into(),
            tool_name: "tool".into(),
            is_error: false,
            content: vec![ai::api::conversation::ContentBlock::Text {
                text: text.into(),
                text_signature: None,
            }],
        }
    }

    fn assistant(message_id: &str, text: &str) -> AgentMessage {
        let mut message = ai::api::conversation::AssistantMessage::empty("api", "model");
        message.content = vec![ai::api::conversation::ContentBlock::Text {
            text: text.into(),
            text_signature: None,
        }];
        AgentMessage::Assistant {
            message_id: message_id.into(),
            message,
        }
    }

    fn settings(keep_recent_tokens: u32) -> CompactionSettings {
        CompactionSettings {
            enabled: true,
            reserve_tokens: 1024,
            keep_recent_tokens,
        }
    }

    #[test]
    fn disabled_compaction_never_triggers() {
        let settings = CompactionSettings {
            enabled: false,
            reserve_tokens: 1024,
            keep_recent_tokens: 1024,
        };
        assert!(!should_compact(100_000, 128_000, &settings));
    }

    #[test]
    fn compaction_triggers_above_the_reserve_budget() {
        assert!(!should_compact(120_000, 128_000, &settings(20_000)));
        assert!(should_compact(128_000, 128_000, &settings(20_000)));
    }

    #[test]
    fn zero_context_window_never_triggers() {
        assert!(!should_compact(999_999, 0, &settings(20_000)));
    }

    #[test]
    fn small_histories_are_fully_kept() {
        let messages = vec![user("a", "hello"), user("b", "world")];
        let (to_summarize, keep) = prepare_compaction(&messages, &settings(10_000));
        assert!(to_summarize.is_empty());
        assert_eq!(keep.len(), 2);
    }

    #[test]
    fn trailing_tool_results_are_kept_with_their_assistant() {
        let messages = vec![
            user("a", &"x".repeat(20_000)),
            assistant("m", "let me check"),
            tool_result("t1", "first"),
            tool_result("t2", "second"),
        ];
        let (to_summarize, keep) = prepare_compaction(&messages, &settings(2000));
        assert_eq!(to_summarize.len(), 1);
        assert!(matches!(to_summarize[0], AgentMessage::UserText { .. }));
        assert_eq!(keep.len(), 3);
        assert!(matches!(keep[0], AgentMessage::Assistant { .. }));
        assert!(matches!(keep[1], AgentMessage::ToolResult { .. }));
        assert!(matches!(keep[2], AgentMessage::ToolResult { .. }));
    }

    #[test]
    fn large_single_message_still_gets_kept_as_the_anchor() {
        let messages = vec![user("a", &"y".repeat(200_000))];
        let (to_summarize, keep) = prepare_compaction(&messages, &settings(100));
        assert!(to_summarize.is_empty());
        assert_eq!(keep.len(), 1);
    }

    #[test]
    fn empty_history_produces_nothing() {
        let (to_summarize, keep) = prepare_compaction(&[], &settings(100));
        assert!(to_summarize.is_empty() && keep.is_empty());
    }
}
