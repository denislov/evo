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
