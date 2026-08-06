#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderErrorKind {
    MissingCredentials,
    Network,
    Timeout,
    Cancelled,
    HttpStatus,
    CircuitOpen,
    RetryAfterTooLong,
    HookFailed,
    StreamParse,
    UnsupportedOption,
}

#[derive(Debug, Clone)]
pub struct ProviderError {
    kind: ProviderErrorKind,
    api: String,
    provider: Option<String>,
    model: Option<String>,
    status: Option<u16>,
    message: String,
    retry_after_ms: Option<u64>,
}

impl ProviderError {
    pub fn missing_credentials(api: &str, model_id: &str, provider: &str) -> Self {
        Self {
            kind: ProviderErrorKind::MissingCredentials,
            api: api.to_string(),
            provider: Some(provider.to_string()),
            model: Some(model_id.to_string()),
            status: None,
            message: format!(
                "No API key found for provider {}. Set the appropriate env var or pass apiKey in options.",
                provider
            ),
            retry_after_ms: None,
        }
    }

    pub fn network(api: &str, model_id: &str, provider: &str) -> Self {
        Self {
            kind: ProviderErrorKind::Network,
            api: api.to_string(),
            provider: Some(provider.to_string()),
            model: Some(model_id.to_string()),
            status: None,
            message: "HTTP request failed".to_string(),
            retry_after_ms: None,
        }
    }

    pub fn timeout(api: &str, model_id: &str, provider: &str, ms: u64) -> Self {
        Self {
            kind: ProviderErrorKind::Timeout,
            api: api.to_string(),
            provider: Some(provider.to_string()),
            model: Some(model_id.to_string()),
            status: None,
            message: format!("Request timed out after {}ms", ms),
            retry_after_ms: None,
        }
    }

    pub fn http_status(api: &str, model_id: &str, provider: &str, status: u16) -> Self {
        Self {
            kind: ProviderErrorKind::HttpStatus,
            api: api.to_string(),
            provider: Some(provider.to_string()),
            model: Some(model_id.to_string()),
            status: Some(status),
            message: format!("HTTP request failed with status {status}"),
            retry_after_ms: None,
        }
    }

    pub fn cancelled(api: &str, model_id: &str, provider: &str) -> Self {
        Self {
            kind: ProviderErrorKind::Cancelled,
            api: api.to_string(),
            provider: Some(provider.to_string()),
            model: Some(model_id.to_string()),
            status: None,
            message: "cancelled".to_string(),
            retry_after_ms: None,
        }
    }

    pub(crate) fn retry_after_too_long(
        api: &str,
        model_id: &str,
        provider: &str,
        message: impl Into<String>,
    ) -> Self {
        Self {
            kind: ProviderErrorKind::RetryAfterTooLong,
            api: api.to_string(),
            provider: Some(provider.to_string()),
            model: Some(model_id.to_string()),
            status: None,
            message: message.into(),
            retry_after_ms: None,
        }
    }

    pub(crate) fn unsupported_option(
        api: &str,
        model_id: &str,
        provider: &str,
        message: impl Into<String>,
    ) -> Self {
        Self {
            kind: ProviderErrorKind::UnsupportedOption,
            api: api.to_string(),
            provider: Some(provider.to_string()),
            model: Some(model_id.to_string()),
            status: None,
            message: message.into(),
            retry_after_ms: None,
        }
    }

    pub(crate) fn circuit_open(
        api: &str,
        model_id: &str,
        provider: &str,
        retry_after_ms: u64,
    ) -> Self {
        Self {
            kind: ProviderErrorKind::CircuitOpen,
            api: api.to_string(),
            provider: Some(provider.to_string()),
            model: Some(model_id.to_string()),
            status: None,
            message: format!(
                "provider circuit is open, no request was sent; retry in {retry_after_ms}ms"
            ),
            retry_after_ms: Some(retry_after_ms),
        }
    }

    pub const fn kind(&self) -> ProviderErrorKind {
        self.kind
    }

    pub fn api(&self) -> &str {
        &self.api
    }

    pub fn provider(&self) -> Option<&str> {
        self.provider.as_deref()
    }

    pub fn model(&self) -> Option<&str> {
        self.model.as_deref()
    }

    pub const fn status(&self) -> Option<u16> {
        self.status
    }

    pub fn message(&self) -> &str {
        &self.message
    }

    pub const fn retry_after_ms(&self) -> Option<u64> {
        self.retry_after_ms
    }
}

impl std::fmt::Display for ProviderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}
