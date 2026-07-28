//! Bounded copy projection.
//!
//! Copy text is the unsanitized product truth, unlike the Markdown preview:
//! the only transformation is joining the block's detail and enforcing a byte
//! cap, so what a reader copies is what the model produced.

use super::truncate_bytes;

pub const MAX_COPY_BYTES: usize = 1024 * 1024;

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
}
