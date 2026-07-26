use serde::{Deserialize, Serialize};

/// Responses-family compatibility metadata. The currently retained fields are
/// catalog-only; explicit session/cache behavior is controlled by
/// [`crate::protocol::StreamOptions`] and validated per provider.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct OpenAIResponsesCompat {
    #[serde(rename = "sendSessionIdHeader", default)]
    pub send_session_id_header: Option<bool>,
    #[serde(rename = "supportsLongCacheRetention", default)]
    pub supports_long_cache_retention: Option<bool>,
}
