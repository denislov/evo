use ai_protocol::api::conversation::ContentBlock;

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

    let bytes_per_token = settings.token_estimation.bytes_per_token;
    let estimated = estimate_tokens(messages, bytes_per_token);
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

        let msg_tokens = estimate_tokens(std::slice::from_ref(msg), bytes_per_token);
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

    let mut to_summarize: Vec<AgentMessage> = messages[..i].to_vec();

    // Move orphaned leading ToolResults from keep_recent to to_summarize.
    // An orphaned ToolResult is one whose corresponding Assistant(tool_call)
    // is not in keep_recent (it is in to_summarize). Keeping such a
    // ToolResult without its Assistant would leave a dangling tool result in
    // the active context and split a tool pair across the compaction cut.
    while let Some(first) = keep_recent.first() {
        if matches!(first, AgentMessage::ToolResult { .. }) {
            let has_assistant_with_tool_call = keep_recent.iter().any(|m| {
                matches!(
                    m,
                    AgentMessage::Assistant { message, .. }
                    if message
                        .content
                        .iter()
                        .any(|b| matches!(b, ContentBlock::ToolCall { .. }))
                )
            });
            if !has_assistant_with_tool_call {
                let orphan = keep_recent.remove(0);
                to_summarize.push(orphan);
            } else {
                break;
            }
        } else {
            break;
        }
    }

    (to_summarize, keep_recent)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ai_protocol::api::conversation::{AssistantMessage, ToolCallKind};

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
            content: vec![ContentBlock::Text {
                text: text.into(),
                text_signature: None,
            }],
        }
    }

    fn assistant(message_id: &str, text: &str) -> AgentMessage {
        let mut message = AssistantMessage::empty("api", "model");
        message.content = vec![ContentBlock::Text {
            text: text.into(),
            text_signature: None,
        }];
        AgentMessage::Assistant {
            message_id: message_id.into(),
            message,
        }
    }

    fn assistant_with_tool_call(message_id: &str, call_id: &str, name: &str) -> AgentMessage {
        let mut message = AssistantMessage::empty("api", "model");
        message.content = vec![ContentBlock::ToolCall {
            id: call_id.into(),
            name: name.into(),
            arguments: serde_json::Value::Object(serde_json::Map::new()),
            kind: ToolCallKind::Function,
            thought_signature: None,
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
            ..CompactionSettings::default()
        }
    }

    #[test]
    fn disabled_compaction_never_triggers() {
        let settings = CompactionSettings {
            enabled: false,
            ..CompactionSettings::default()
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

    #[test]
    fn compaction_split_does_not_break_middle_tool_pair() {
        // [user(big), assistant(tool_call), tool_result, user(big), assistant(tool_call), tool_result]
        // With a small keep budget the natural cut lands between the first
        // tool_result and the second user message, which would orphan the
        // second tool pair's leading assistant. The cut must move so neither
        // pair is split.
        let messages = vec![
            user("u1", &"x".repeat(20_000)),
            assistant_with_tool_call("a1", "c1", "tool"),
            tool_result("c1", "first"),
            user("u2", &"x".repeat(20_000)),
            assistant_with_tool_call("a2", "c2", "tool"),
            tool_result("c2", "second"),
        ];
        let (to_summarize, keep) = prepare_compaction(&messages, &settings(2000));

        // Every ToolResult kept must have its Assistant(tool_call) kept too.
        for msg in &keep {
            if let AgentMessage::ToolResult { tool_call_id, .. } = msg {
                let has_call = keep.iter().any(|m| match m {
                    AgentMessage::Assistant { message, .. } => message.content.iter().any(
                        |b| matches!(b, ContentBlock::ToolCall { id, .. } if id == tool_call_id),
                    ),
                    _ => false,
                });
                assert!(
                    has_call,
                    "kept ToolResult {tool_call_id} is missing its Assistant(tool_call)"
                );
            }
        }
        // And every ToolResult in to_summarize must have its Assistant there too.
        for msg in &to_summarize {
            if let AgentMessage::ToolResult { tool_call_id, .. } = msg {
                let has_call = to_summarize.iter().any(|m| match m {
                    AgentMessage::Assistant { message, .. } => message.content.iter().any(
                        |b| matches!(b, ContentBlock::ToolCall { id, .. } if id == tool_call_id),
                    ),
                    _ => false,
                });
                assert!(
                    has_call,
                    "summarized ToolResult {tool_call_id} is missing its Assistant(tool_call)"
                );
            }
        }
    }

    #[test]
    fn compaction_split_does_not_break_trailing_tool_pair() {
        // Trailing tool results are kept with their assistant via the existing
        // skip path; this confirms that path still holds with a mid-history cut.
        let messages = vec![
            user("u1", &"x".repeat(20_000)),
            assistant_with_tool_call("a1", "c1", "tool"),
            tool_result("c1", "first"),
            user("u2", "small"),
            assistant_with_tool_call("a2", "c2", "tool"),
            tool_result("c2", "second"),
        ];
        let (to_summarize, keep) = prepare_compaction(&messages, &settings(2000));
        assert!(!to_summarize.is_empty());
        // The second tool pair must be fully kept.
        assert!(keep.iter().any(|m| matches!(
            m,
            AgentMessage::Assistant { message, .. }
            if message.content.iter().any(|b| matches!(b, ContentBlock::ToolCall { id, .. } if id == "c2"))
        )));
        assert!(keep.iter().any(
            |m| matches!(m, AgentMessage::ToolResult { tool_call_id, .. } if tool_call_id == "c2")
        ));
    }
}
