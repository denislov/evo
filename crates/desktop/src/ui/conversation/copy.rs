//! Bounded copy projection.
//!
//! Copy text is the unsanitized product truth, unlike the Markdown preview:
//! the only transformation is joining the block's detail and enforcing a byte
//! cap, so what a reader copies is what the model produced.

pub const MAX_COPY_BYTES: usize = 1024 * 1024;

pub(super) fn truncate_bytes(mut text: String, max_bytes: usize) -> (String, bool) {
    if text.len() <= max_bytes {
        return (text, false);
    }
    let mut boundary = max_bytes;
    while boundary > 0 && !text.is_char_boundary(boundary) {
        boundary -= 1;
    }
    text.truncate(boundary);
    (text, true)
}

pub fn conversation_copy_text(text: &str, detail: &str) -> String {
    let mut text = text.to_owned();
    if !detail.is_empty() {
        if !text.is_empty() {
            text.push('\n');
        }
        text.push_str(detail);
    }
    truncate_bytes(text, MAX_COPY_BYTES).0
}

#[cfg(test)]
mod tests {
    use super::super::*;
    use coding_agent::api::view::{
        CodingAgentSessionTranscriptItem, CodingAgentTranscriptSnapshot,
    };

    #[test]
    fn tool_copy_includes_arguments_and_result_without_exceeding_the_copy_cap() {
        let projection = ConversationProjection::hydrate(CodingAgentTranscriptSnapshot {
            session_id: "session-1".into(),
            active_leaf_id: Some("leaf-1".into()),
            items: vec![CodingAgentSessionTranscriptItem::Tool {
                call_id: "call-1".into(),
                name: "shell".into(),
                args: serde_json::json!({"command": "x".repeat(MAX_TOOL_ARGUMENT_BYTES)}),
                result: Some("界".repeat(MAX_BLOCK_TEXT_BYTES)),
                is_error: false,
                duration_millis: Some(1_240),
            }],
        });
        let block = projection.blocks().front().unwrap();
        assert_eq!(block.title, "Tool · shell · 1.2 s");
        let copied = block.copy_text();
        assert!(copied.len() <= MAX_COPY_BYTES);
        assert!(copied.is_char_boundary(copied.len()));
        assert!(block.truncated);
    }

    #[test]
    fn live_row_copy_uses_the_same_bounded_utf8_safe_projection() {
        assert_eq!(conversation_copy_text("answer", "detail"), "answer\ndetail");
        let copied = conversation_copy_text("", &"界".repeat(MAX_COPY_BYTES));
        assert!(copied.len() <= MAX_COPY_BYTES);
        assert!(copied.is_char_boundary(copied.len()));
    }
}
