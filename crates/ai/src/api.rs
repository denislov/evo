/// Provider-backed model catalog lookup.
pub mod model {
    pub use crate::model::{all_models, get_model, get_models, get_providers, lookup_model};
}

/// Scoped AI client construction. Registry mutation and provider
/// registration remain separate explicit categories.
pub mod client {
    pub use crate::client::AiClient;
}

/// Provider authentication inputs, resolvers, and secret-free diagnostics.
pub mod auth {
    pub use crate::registry::env::env_api_key;
    pub use crate::registry::{EnvProviderAuthResolver, ProviderAuth, ProviderAuthResolver};
}

/// Provider registration contracts and built-in provider installation.
/// Low-level agent runtimes must not depend on this category.
pub mod provider {
    pub use crate::providers::faux;
    pub use crate::providers::{
        WEB_SEARCH_PROVIDER_APIS, builtin_provider_apis, model_supports_web_search,
        register_builtins_into,
    };
    pub use crate::registry::{ApiProvider, ProviderRegistry};
}

/// Provider-neutral error classification.
pub mod error {
    pub use crate::transport::error::{ProviderError, ProviderErrorKind};
}

/// Transport policy values that are stable for product composition. HTTP,
/// SSE, and header implementations remain private.
pub mod transport {
    pub use crate::transport::client::TransportConfig;
    pub use crate::transport::retry::{RetryConfig, is_retryable_status, parse_retry_after_ms};
}

/// Provider resilience primitives: circuit breaking, secret redaction, and
/// injectable clocks for deterministic tests. Consumers outside `ai` feed
/// [`SecretsScrubber`](SecretsScrubber) output into logs, telemetry, hook
/// payloads, and crash reports.
pub mod resilience {
    pub use crate::scrub::{SecretStore, SecretsScrubber, scrub_text};
    pub use crate::transport::circuit_breaker::{
        BreakerKey, BreakerStateName, BreakerVerdict, CircuitBreaker, CircuitBreakerConfig,
        CircuitBreakerRegistry, Clock, SystemClock,
    };
}

/// SSRF-hardened HTTP fetch for product tools: every hop re-validates the
/// resolved address, redirects are budgeted, response bodies are bounded,
/// and successful fetches may be cached in memory.
pub mod fetch {
    pub use crate::transport::fetch::{
        CacheConfig, FetchCache, FetchClient, FetchClientConfig, FetchError, FetchErrorKind,
        FetchRequest, FetchResult, OutputFormat,
    };
}
