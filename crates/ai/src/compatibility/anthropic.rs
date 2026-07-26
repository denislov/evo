use serde::{Deserialize, Serialize};

/// Anthropic model compatibility metadata. Use
/// [`super::compatibility_field_disposition`] to distinguish request-enforced
/// flags from catalog-only documentation.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AnthropicMessagesCompat {
    #[serde(rename = "supportsEagerToolInputStreaming", default)]
    pub supports_eager_tool_input_streaming: Option<bool>,
    #[serde(rename = "sendSessionAffinityHeaders", default)]
    pub send_session_affinity_headers: Option<bool>,
    #[serde(rename = "supportsLongCacheRetention", default)]
    pub supports_long_cache_retention: Option<bool>,
    #[serde(rename = "supportsCacheControlOnTools", default)]
    pub supports_cache_control_on_tools: Option<bool>,
    #[serde(rename = "supportsTemperature", default)]
    pub supports_temperature: Option<bool>,
    #[serde(rename = "forceAdaptiveThinking", default)]
    pub force_adaptive_thinking: Option<bool>,
    #[serde(rename = "allowEmptySignature", default)]
    pub allow_empty_signature: Option<bool>,
}

impl AnthropicMessagesCompat {
    pub fn from_model(model: &crate::model::Model) -> Self {
        match model.compat.as_ref() {
            Some(super::ModelCompat::AnthropicMessages(compat)) => compat.clone(),
            _ => Self::default(),
        }
    }
}
