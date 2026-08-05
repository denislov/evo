/// Provider-neutral model metadata and cost calculation.
pub mod model {
    pub use crate::model::{Model, ModelCost, ModelInput, calculate_cost};
    pub use crate::protocol::ThinkingConfig;
}

/// Provider-neutral request, response, message, tool, and usage values.
pub mod conversation {
    pub use crate::protocol::{
        AssistantMessage, AssistantMessageDiagnostic, ContentBlock, Context, Cost,
        DiagnosticErrorInfo, Message, ProviderMetadata, ResponsesTextFormat, StopReason, Tool,
        ToolCallKind, ToolKind, Usage, saturating_token_total,
    };
}

/// Streaming request options, events, collection, and incremental JSON decoding.
pub mod stream {
    pub use crate::protocol::stream::{EventStream, complete};
    pub use crate::protocol::{AssistantMessageEvent, ResponsesOptions, StreamOptions};

    pub mod json {
        pub use crate::protocol::json::{parse_streaming_json, parse_terminal_json, repair_json};
    }
}

/// Provider request/response hook contracts.
pub mod hooks {
    pub use crate::protocol::hooks::{
        ProviderPayloadHook, ProviderPayloadHookFuture, ProviderResponseHook,
        ProviderResponseHookFuture,
    };
    pub use crate::protocol::{ProviderResponseInfo, ProviderStreamHooks};
}

/// Provider authentication diagnostics carried by invocation options.
pub mod auth {
    pub use crate::protocol::ProviderAuthDiagnostic;
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
