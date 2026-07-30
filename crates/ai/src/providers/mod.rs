pub mod anthropic;
pub mod azure_openai_responses;
pub mod common;
pub mod faux;
pub mod google;
pub mod mistral;
pub mod openai;
pub mod openai_codex_responses;

use crate::registry::{ApiProvider, ProviderRegistry};
use std::sync::Arc;

pub const BUILTIN_PROVIDER_APIS: &[&str] = &[
    "anthropic-messages",
    "azure-openai-responses",
    "google-generative-ai",
    "mistral-conversations",
    "openai-codex-responses",
    "openai-completions",
    "openai-responses",
];

pub fn builtin_provider_apis() -> &'static [&'static str] {
    BUILTIN_PROVIDER_APIS
}

fn register_each_builtin(mut register: impl FnMut(&'static str, Arc<dyn ApiProvider>)) {
    register(
        "anthropic-messages",
        Arc::new(anthropic::AnthropicProvider::new(None)),
    );
    register(
        "openai-completions",
        Arc::new(openai::completions::OpenAICompletionsProvider::new(None)),
    );
    register(
        "openai-responses",
        Arc::new(openai::responses::OpenAIResponsesProvider::new(None)),
    );
    register(
        "openai-codex-responses",
        Arc::new(openai_codex_responses::OpenAICodexResponsesProvider::new(
            None,
        )),
    );
    register(
        "azure-openai-responses",
        Arc::new(azure_openai_responses::AzureOpenAIResponsesProvider::new(
            None,
        )),
    );
    register(
        "google-generative-ai",
        Arc::new(google::GoogleGenerativeAiProvider::new(None)),
    );
    register(
        "mistral-conversations",
        Arc::new(mistral::MistralProvider::new(None)),
    );
}

/// Register all built-in providers in the given scoped registry.
pub fn register_builtins_into(registry: &ProviderRegistry) {
    register_each_builtin(|api, provider| registry.register(api, provider));
}
