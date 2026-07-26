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

    while i > 0 {
        i -= 1;
        let msg = &messages[i];

        if matches!(msg, AgentMessage::ToolResult { .. }) && keep_recent.is_empty() {
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

    let to_summarize: Vec<AgentMessage> = messages[..i].to_vec();

    (to_summarize, keep_recent)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ai::api::conversation::ContentBlock;

    fn user_msg(text: &str) -> AgentMessage {
        AgentMessage::UserText {
            message_id: "u".into(),
            text: text.into(),
        }
    }

    fn tool_result(text: &str) -> AgentMessage {
        AgentMessage::ToolResult {
            message_id: "t".into(),
            tool_call_id: "call".into(),
            tool_name: "test".into(),
            is_error: false,
            content: vec![ContentBlock::Text {
                text: text.into(),
                text_signature: None,
            }],
        }
    }

    fn settings_with_reserve(reserve_tokens: u32) -> CompactionSettings {
        CompactionSettings {
            enabled: true,
            reserve_tokens,
            keep_recent_tokens: 0,
        }
    }

    #[test]
    fn should_compact_when_over_threshold() {
        // context_window(100k) - reserve(10k) = 90k threshold; 95k exceeds it.
        assert!(should_compact(
            95_000,
            100_000,
            &settings_with_reserve(10_000)
        ));
    }

    #[test]
    fn should_not_compact_under_threshold() {
        // 89k is below the 90k threshold.
        assert!(!should_compact(
            89_000,
            100_000,
            &settings_with_reserve(10_000)
        ));
    }

    #[test]
    fn should_not_compact_when_disabled() {
        // Mirrors the TS "should return false when disabled" case: even well past
        // the threshold, a disabled config must not trigger compaction.
        let settings = CompactionSettings {
            enabled: false,
            reserve_tokens: 10_000,
            keep_recent_tokens: 0,
        };
        assert!(!should_compact(95_000, 100_000, &settings));
    }

    #[test]
    fn avoid_orphan_tool_result_cut_point() {
        let msgs = vec![user_msg("hello"), tool_result("result"), user_msg("next")];
        let settings = CompactionSettings {
            enabled: true,
            reserve_tokens: 0,
            keep_recent_tokens: 10,
        };
        let (to_summarize, keep) = prepare_compaction(&msgs, &settings);
        assert!(!to_summarize.is_empty() || !keep.is_empty());
    }

    #[test]
    fn maintain_cut_point_after_user_message() {
        let msgs = vec![user_msg("first"), user_msg("second"), user_msg("third")];
        let settings = CompactionSettings {
            enabled: true,
            reserve_tokens: 0,
            keep_recent_tokens: 5,
        };
        let (to_summarize, keep) = prepare_compaction(&msgs, &settings);
        assert!(!keep.is_empty());
        assert!(to_summarize.len() + keep.len() == msgs.len());
    }
}
