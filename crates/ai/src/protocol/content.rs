use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type")]
pub enum ContentBlock {
    #[serde(rename = "text")]
    Text {
        text: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        text_signature: Option<String>,
    },
    #[serde(rename = "thinking")]
    Thinking {
        thinking: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        thinking_signature: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        redacted: Option<bool>,
    },
    #[serde(rename = "image")]
    Image {
        data: String,
        #[serde(rename = "mimeType")]
        mime_type: String,
    },
    #[serde(rename = "toolCall")]
    ToolCall {
        id: String,
        name: String,
        arguments: serde_json::Value,
        #[serde(skip_serializing_if = "Option::is_none")]
        thought_signature: Option<String>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_block_text_roundtrip() {
        let cb = ContentBlock::Text {
            text: "hello".into(),
            text_signature: None,
        };
        let json = serde_json::to_string(&cb).unwrap();
        assert_eq!(json, r#"{"type":"text","text":"hello"}"#);
        let back: ContentBlock = serde_json::from_str(&json).unwrap();
        assert_eq!(back, cb);
    }

    #[test]
    fn content_block_toolcall_roundtrip() {
        let cb = ContentBlock::ToolCall {
            id: "toolu_01".into(),
            name: "read".into(),
            arguments: serde_json::json!({"path": "/x"}),
            thought_signature: None,
        };
        let json = serde_json::to_string(&cb).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["type"], "toolCall");
        assert_eq!(parsed["id"], "toolu_01");
        let back: ContentBlock = serde_json::from_str(&json).unwrap();
        assert_eq!(back, cb);
    }
}
