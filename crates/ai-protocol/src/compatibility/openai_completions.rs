use super::thinking::{CacheControlFormat, ThinkingFormat};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct OpenRouterRouting {
    #[serde(
        rename = "allow_fallbacks",
        alias = "allowFallbacks",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub allow_fallbacks: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub require_parameters: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data_collection: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub zdr: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enforce_distillable_text: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub order: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub only: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ignore: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quantizations: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sort: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_price: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preferred_min_throughput: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preferred_max_latency: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct VercelGatewayRouting {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub only: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub order: Option<Vec<String>>,
}

/// OpenAI-compatible chat-completions metadata. Use
/// [`super::compatibility_field_disposition`] to distinguish request-enforced
/// flags from catalog-only documentation.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct OpenAICompletionsCompat {
    #[serde(rename = "supportsStore", default)]
    pub supports_store: Option<bool>,
    #[serde(rename = "supportsUsageInStreaming", default)]
    pub supports_usage_in_streaming: Option<bool>,
    #[serde(rename = "supportsDeveloperRole", default)]
    pub supports_developer_role: Option<bool>,
    #[serde(rename = "supportsReasoningEffort", default)]
    pub supports_reasoning_effort: Option<bool>,
    #[serde(rename = "maxTokensField", default)]
    pub max_tokens_field: Option<String>,
    #[serde(rename = "requiresToolResultName", default)]
    pub requires_tool_result_name: Option<bool>,
    #[serde(rename = "requiresAssistantAfterToolResult", default)]
    pub requires_assistant_after_tool_result: Option<bool>,
    #[serde(rename = "requiresThinkingAsText", default)]
    pub requires_thinking_as_text: Option<bool>,
    #[serde(rename = "requiresReasoningContentOnAssistantMessages", default)]
    pub requires_reasoning_content_on_assistant_messages: Option<bool>,
    #[serde(rename = "thinkingFormat", default)]
    pub thinking_format: Option<ThinkingFormat>,
    #[serde(rename = "openRouterRouting", default)]
    pub open_router_routing: Option<OpenRouterRouting>,
    #[serde(rename = "vercelGatewayRouting", default)]
    pub vercel_gateway_routing: Option<VercelGatewayRouting>,
    #[serde(rename = "zaiToolStream", default)]
    pub zai_tool_stream: Option<bool>,
    #[serde(rename = "supportsStrictMode", default)]
    pub supports_strict_mode: Option<bool>,
    #[serde(rename = "cacheControlFormat", default)]
    pub cache_control_format: Option<CacheControlFormat>,
    #[serde(rename = "sendSessionAffinityHeaders", default)]
    pub send_session_affinity_headers: Option<bool>,
    #[serde(rename = "supportsLongCacheRetention", default)]
    pub supports_long_cache_retention: Option<bool>,
}

impl OpenAICompletionsCompat {
    pub fn from_model(model: &crate::model::Model) -> Self {
        match model.compat.as_ref() {
            Some(super::ModelCompat::OpenAICompletions(compat)) => compat.clone(),
            Some(compat) => serde_json::to_value(compat)
                .ok()
                .and_then(|value| serde_json::from_value(value).ok())
                .unwrap_or_default(),
            None => Self::default(),
        }
    }
}
