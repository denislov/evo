pub mod anthropic;
pub mod common;
pub mod deepseek;
pub mod faux;
pub mod google;
pub mod mistral;
pub mod openai;
pub mod openai_codex_responses;
pub(crate) mod responses;

use crate::registry::{ApiProvider, ProviderRegistry};
use std::sync::Arc;

pub const BUILTIN_PROVIDER_APIS: &[&str] = &[
    "anthropic-messages",
    "deepseek-responses",
    "google-generative-ai",
    "mistral-conversations",
    "openai-codex-responses",
    "openai-completions",
    "openai-responses",
];

pub fn builtin_provider_apis() -> &'static [&'static str] {
    BUILTIN_PROVIDER_APIS
}

/// APIs that can express the provider-executed `web_search` tool on the wire
/// *and* whose stream parser reports its outcome as a
/// [`ContentBlock::ProviderItem`](crate::protocol::ContentBlock::ProviderItem)
/// that later turns replay verbatim. Both halves are required: declaring the
/// tool to an API that cannot replay the result would strand the search on the
/// next stateless request.
pub const WEB_SEARCH_PROVIDER_APIS: &[&str] = &["deepseek-responses", "openai-responses"];

/// Whether `model` can be sent a `web_search` tool declaration.
///
/// Resolved from the API family rather than per-model catalog metadata: the
/// support boundary we can verify is the request converter and stream parser,
/// which are per-API. A model on a supporting API that still rejects the tool
/// surfaces the provider's own error; callers must not treat `true` as a
/// guarantee the request will succeed.
pub fn model_supports_web_search(model: &crate::model::Model) -> bool {
    WEB_SEARCH_PROVIDER_APIS.contains(&model.api.as_str())
}

fn register_each_builtin(
    client: &reqwest::Client,
    mut register: impl FnMut(&'static str, Arc<dyn ApiProvider>),
) {
    register(
        "anthropic-messages",
        Arc::new(anthropic::AnthropicProvider::with_client(
            None,
            client.clone(),
        )),
    );
    register(
        "deepseek-responses",
        Arc::new(deepseek::DeepSeekResponsesProvider::with_client(
            None,
            client.clone(),
        )),
    );
    register(
        "openai-completions",
        Arc::new(openai::completions::OpenAICompletionsProvider::with_client(
            None,
            client.clone(),
        )),
    );
    register(
        "openai-responses",
        Arc::new(openai::responses::OpenAIResponsesProvider::with_client(
            None,
            client.clone(),
        )),
    );
    register(
        "openai-codex-responses",
        Arc::new(
            openai_codex_responses::OpenAICodexResponsesProvider::with_client(None, client.clone()),
        ),
    );
    register(
        "google-generative-ai",
        Arc::new(google::GoogleGenerativeAiProvider::with_client(
            None,
            client.clone(),
        )),
    );
    register(
        "mistral-conversations",
        Arc::new(mistral::MistralProvider::with_client(None, client.clone())),
    );
}

/// Register all built-in providers in the given scoped registry.
pub fn register_builtins_into(registry: &ProviderRegistry, client: &reqwest::Client) {
    register_each_builtin(client, |api, provider| registry.register(api, provider));
}
