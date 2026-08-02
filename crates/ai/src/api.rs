/// Model metadata, catalog lookup, reasoning configuration, and cost
/// calculation. Consumers should use this category instead of importing
/// the flat compatibility facade.
pub mod model {
    pub use crate::model::{Model, ModelCost, ModelInput};
    pub use crate::model::{
        all_models, calculate_cost, get_model, get_models, get_providers, lookup_model,
    };
    pub use crate::protocol::ThinkingConfig;
}

/// Provider-neutral request, response, message, tool, and usage values.
pub mod conversation {
    pub use crate::protocol::{
        AssistantMessage, AssistantMessageDiagnostic, ContentBlock, Context, Cost,
        DiagnosticErrorInfo, Message, ProviderMetadata, ResponsesTextFormat, StopReason, Tool,
        ToolCallKind, ToolKind, Usage,
    };
}

/// Streaming request options, events, collection, and incremental JSON
/// decoding used at provider-neutral streaming boundaries.
pub mod stream {
    pub use crate::protocol::stream::{EventStream, complete};
    pub use crate::protocol::{AssistantMessageEvent, ResponsesOptions, StreamOptions};

    pub mod json {
        pub use crate::protocol::json::{parse_streaming_json, parse_terminal_json, repair_json};
    }
}

/// Provider request/response hook contracts. This category exposes hook
/// vocabulary, not provider clients or transports.
pub mod hooks {
    pub use crate::protocol::hooks::{
        ProviderPayloadHook, ProviderPayloadHookFuture, ProviderResponseHook,
        ProviderResponseHookFuture,
    };
    pub use crate::protocol::{ProviderResponseInfo, ProviderStreamHooks};
}

/// Scoped AI client construction. Registry mutation and provider
/// registration remain separate explicit categories.
pub mod client {
    pub use crate::client::AiClient;
}

/// Provider authentication inputs, resolvers, and secret-free diagnostics.
pub mod auth {
    pub use crate::protocol::ProviderAuthDiagnostic;
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

/// Explicit cross-provider compatibility configuration values.
pub mod compatibility {
    pub use crate::compatibility::{
        AnthropicMessagesCompat, CacheControlFormat, CompatibilityDisposition, ModelCompat,
        OpenAICompletionsCompat, OpenAIResponsesCompat, OpenRouterRouting, ThinkingFormat,
        ThinkingLevelMap, ThinkingLevelValue, VercelGatewayRouting,
        compatibility_field_disposition,
    };
}
