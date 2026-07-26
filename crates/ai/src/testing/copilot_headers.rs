use std::collections::BTreeMap;

use crate::protocol::{ContentBlock, Message};

pub fn infer_copilot_initiator(messages: &[Message]) -> &'static str {
    match messages.last() {
        Some(Message::User { .. }) => "user",
        Some(_) => "agent",
        None => "user",
    }
}

pub fn has_copilot_vision_input(messages: &[Message]) -> bool {
    messages.iter().any(|message| match message {
        Message::User { content } | Message::ToolResult { content, .. } => content
            .iter()
            .any(|block| matches!(block, ContentBlock::Image { .. })),
        Message::Assistant { .. } => false,
    })
}

pub fn build_dynamic_headers(messages: &[Message], has_images: bool) -> BTreeMap<String, String> {
    let mut headers = BTreeMap::new();
    headers.insert(
        "X-Initiator".into(),
        infer_copilot_initiator(messages).into(),
    );
    headers.insert("Openai-Intent".into(), "conversation-edits".into());
    if has_images {
        headers.insert("Copilot-Vision-Request".into(), "true".into());
    }
    headers
}
