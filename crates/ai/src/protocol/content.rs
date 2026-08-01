use serde::{Deserialize, Serialize};

/// Opaque identity required to replay an item through the API that produced it.
///
/// `api` prevents item identifiers or encrypted payloads from being forwarded
/// across incompatible providers. `thinking_signature` remains on
/// [`ContentBlock::Thinking`] solely for backward compatibility with existing
/// Anthropic-style transcripts.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProviderMetadata {
    pub api: String,
    #[serde(rename = "itemId", skip_serializing_if = "Option::is_none")]
    pub item_id: Option<String>,
    #[serde(rename = "encryptedContent", skip_serializing_if = "Option::is_none")]
    pub encrypted_content: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ToolCallKind {
    #[default]
    Function,
    Custom,
}

fn is_function_tool_call(kind: &ToolCallKind) -> bool {
    *kind == ToolCallKind::Function
}

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
        /// Legacy provider signature. New Responses implementations use
        /// `provider_metadata` instead.
        #[serde(skip_serializing_if = "Option::is_none")]
        thinking_signature: Option<String>,
        #[serde(
            rename = "providerMetadata",
            default,
            skip_serializing_if = "Option::is_none"
        )]
        provider_metadata: Option<ProviderMetadata>,
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
        #[serde(default, skip_serializing_if = "is_function_tool_call")]
        kind: ToolCallKind,
        #[serde(skip_serializing_if = "Option::is_none")]
        thought_signature: Option<String>,
    },
    /// A server-side provider item that must be replayed verbatim on the next
    /// stateless request. Consumers must only forward it to the matching `api`.
    #[serde(rename = "providerItem")]
    ProviderItem {
        api: String,
        item: serde_json::Value,
    },
}
