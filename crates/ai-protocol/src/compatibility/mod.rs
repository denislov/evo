pub mod anthropic;
pub mod openai_completions;
pub mod openai_responses;
pub mod thinking;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;

pub use anthropic::AnthropicMessagesCompat;
pub use openai_completions::{OpenAICompletionsCompat, OpenRouterRouting, VercelGatewayRouting};
pub use openai_responses::OpenAIResponsesCompat;
pub use thinking::{CacheControlFormat, ThinkingFormat, ThinkingLevelMap, ThinkingLevelValue};

/// Declares where a retained model compatibility field has observable effect.
/// `CatalogOnly` fields are descriptive metadata and do not alter requests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompatibilityDisposition {
    Request,
    Transport,
    Parser,
    CatalogOnly,
}

/// Returns the registered runtime disposition for a serialized compatibility
/// field name. Unknown fields return `None` and fail generated-catalog audits.
pub fn compatibility_field_disposition(field: &str) -> Option<CompatibilityDisposition> {
    use CompatibilityDisposition::{CatalogOnly, Request};
    Some(match field {
        "supportsTemperature"
        | "forceAdaptiveThinking"
        | "supportsCacheControlOnTools"
        | "supportsUsageInStreaming"
        | "supportsDeveloperRole"
        | "supportsReasoningEffort"
        | "maxTokensField"
        | "requiresReasoningContentOnAssistantMessages"
        | "thinkingFormat"
        | "supportsStrictMode" => Request,
        "sendSessionAffinityHeaders"
        | "sendSessionIdHeader"
        | "supportsEagerToolInputStreaming"
        | "zaiToolStream"
        | "supportsLongCacheRetention"
        | "supportsStore"
        | "allowEmptySignature"
        | "requiresToolResultName"
        | "requiresAssistantAfterToolResult"
        | "requiresThinkingAsText"
        | "openRouterRouting"
        | "vercelGatewayRouting"
        | "cacheControlFormat" => CatalogOnly,
        _ => return None,
    })
}

#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq)]
pub enum ModelCompat {
    OpenAICompletions(OpenAICompletionsCompat),
    OpenAIResponses(OpenAIResponsesCompat),
    AnthropicMessages(AnthropicMessagesCompat),
}

impl ModelCompat {
    /// Parse compatibility metadata into its provider-family representation.
    pub fn from_json(value: Value) -> Self {
        if looks_like_anthropic_compat(&value) {
            return Self::AnthropicMessages(serde_json::from_value(value).unwrap_or_default());
        }
        if looks_like_openai_responses_compat(&value) {
            return Self::OpenAIResponses(serde_json::from_value(value).unwrap_or_default());
        }
        Self::OpenAICompletions(serde_json::from_value(value).unwrap_or_default())
    }
}

impl Serialize for ModelCompat {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self {
            Self::OpenAICompletions(compat) => compat.serialize(serializer),
            Self::OpenAIResponses(compat) => compat.serialize(serializer),
            Self::AnthropicMessages(compat) => compat.serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for ModelCompat {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = Value::deserialize(deserializer)?;
        Ok(Self::from_json(value))
    }
}

fn looks_like_anthropic_compat(value: &Value) -> bool {
    let Some(object) = value.as_object() else {
        return false;
    };
    [
        "supportsEagerToolInputStreaming",
        "supportsCacheControlOnTools",
        "supportsTemperature",
        "forceAdaptiveThinking",
        "allowEmptySignature",
    ]
    .iter()
    .any(|key| object.contains_key(*key))
}

fn looks_like_openai_responses_compat(value: &Value) -> bool {
    let Some(object) = value.as_object() else {
        return false;
    };
    object.contains_key("sendSessionIdHeader")
        && object.keys().all(|key| {
            matches!(
                key.as_str(),
                "sendSessionIdHeader" | "supportsLongCacheRetention"
            )
        })
}
