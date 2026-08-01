use super::message::Message;
use crate::protocol::hooks::ProviderStreamHooks;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Context {
    #[serde(rename = "systemPrompt", skip_serializing_if = "Option::is_none")]
    pub system_prompt: Option<String>,
    pub messages: Vec<Message>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<Tool>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Tool {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub parameters: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProviderAuthDiagnostic {
    pub field: String,
    pub source: String,
}

/// Provider-neutral controls for one model invocation.
///
/// `timeout_ms` is one end-to-end deadline covering payload hooks, credential
/// resolution owned by a provider, every request attempt, response hooks,
/// retry delays, and body streaming through the provider terminal event.
/// Retries occur only before any provider-neutral response event is exposed.
/// `cancel` wins a race with the deadline and produces one aborted terminal.
/// Explicit fields unsupported by the selected API are rejected before its
/// HTTP request is sent rather than silently ignored.
#[derive(Clone, Serialize, Deserialize, Default)]
pub struct StreamOptions {
    /// Sampling temperature when supported by the model compatibility record.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    /// Transport selection. Built-in streaming APIs accept only `"sse"`;
    /// WebSocket and unknown values are rejected.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transport: Option<String>,
    /// Requested maximum output tokens, mapped to the provider's wire field.
    #[serde(rename = "maxTokens", skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    /// Runtime API credential. It is excluded from serialization and redacted
    /// from `Debug`.
    #[serde(skip)]
    pub api_key: Option<String>,
    /// Provider-specific cache retention, when supported by the selected API.
    #[serde(rename = "cacheRetention", skip_serializing_if = "Option::is_none")]
    pub cache_retention: Option<serde_json::Value>,
    /// Provider-neutral reasoning/thinking request.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking: Option<ThinkingConfig>,
    /// Provider tool-selection payload, validated and mapped by the API family.
    #[serde(rename = "toolChoice", skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<serde_json::Value>,
    /// Request/session affinity identifier for APIs that explicitly support it.
    #[serde(rename = "sessionId", skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(skip)]
    pub headers: Option<serde_json::Value>,
    /// Cooperative cancellation token checked at every async transport wait.
    #[serde(skip)]
    pub cancel: Option<tokio_util::sync::CancellationToken>,
    /// End-to-end invocation deadline in milliseconds. `0` times out before
    /// hooks or network I/O; values above one hour are rejected.
    #[serde(rename = "timeoutMs", skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
    /// Maximum retries before response events are exposed. Defaults to zero
    /// and is capped at eight.
    #[serde(rename = "maxRetries", skip_serializing_if = "Option::is_none")]
    pub max_retries: Option<u32>,
    /// Upper bound for a server `Retry-After` delay. Excessive values fail the
    /// invocation instead of sleeping beyond the configured policy. Values
    /// above one minute are rejected.
    #[serde(rename = "maxRetryDelayMs", skip_serializing_if = "Option::is_none")]
    pub max_retry_delay_ms: Option<u64>,
    #[serde(default, skip)]
    pub auth_diagnostics: Vec<ProviderAuthDiagnostic>,
    #[serde(skip)]
    pub hooks: Option<ProviderStreamHooks>,
}

impl std::fmt::Debug for StreamOptions {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("StreamOptions")
            .field("temperature", &self.temperature)
            .field("transport", &self.transport)
            .field("max_tokens", &self.max_tokens)
            .field("api_key", &self.api_key.as_ref().map(|_| "[REDACTED]"))
            .field("cache_retention", &self.cache_retention)
            .field("thinking", &self.thinking)
            .field("tool_choice", &self.tool_choice)
            .field("session_id", &self.session_id)
            .field("headers", &self.headers.as_ref().map(|_| "[REDACTED]"))
            .field("cancel", &self.cancel.is_some())
            .field("timeout_ms", &self.timeout_ms)
            .field("max_retries", &self.max_retries)
            .field("max_retry_delay_ms", &self.max_retry_delay_ms)
            .field("auth_diagnostics", &self.auth_diagnostics)
            .field("hooks", &self.hooks)
            .finish()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ThinkingConfig {
    pub enabled: bool,
    #[serde(rename = "budgetTokens", skip_serializing_if = "Option::is_none")]
    pub budget_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
}
