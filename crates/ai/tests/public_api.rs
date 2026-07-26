mod support;

use ai::api::auth::{
    EnvProviderAuthResolver, ProviderAuth, ProviderAuthDiagnostic, ProviderAuthResolver,
    env_api_key,
};
use ai::api::client::AiClient;
use ai::api::compatibility::{
    AnthropicMessagesCompat, CacheControlFormat, CompatibilityDisposition, ModelCompat,
    OpenAICompletionsCompat, OpenAIResponsesCompat, OpenRouterRouting, ThinkingFormat,
    ThinkingLevelMap, ThinkingLevelValue, VercelGatewayRouting, compatibility_field_disposition,
};
use ai::api::conversation::{
    AssistantMessage, ContentBlock, Context, Cost, Message, StopReason, Tool, Usage,
};
use ai::api::error::{ProviderError, ProviderErrorKind};
use ai::api::hooks::{
    ProviderPayloadHook, ProviderPayloadHookFuture, ProviderResponseHook,
    ProviderResponseHookFuture, ProviderResponseInfo, ProviderStreamHooks,
};
use ai::api::model::{
    Model, ModelCost, ModelInput, ThinkingConfig, all_models, calculate_cost, get_model,
    get_models, get_providers, lookup_model,
};
use ai::api::provider::{
    ApiProvider, ProviderRegistry, builtin_provider_apis, register_builtins_into,
};
use ai::api::stream::{AssistantMessageEvent, EventStream, StreamOptions, complete};
use ai::api::transport::{RetryConfig, is_retryable_status, parse_retry_after_ms};
use async_stream::stream;

#[test]
fn categorized_api_facade_exposes_intentional_contract_groups() {
    use ai::api::auth::{EnvProviderAuthResolver, ProviderAuthDiagnostic};
    use ai::api::client::AiClient;
    use ai::api::compatibility::ModelCompat;
    use ai::api::conversation::{AssistantMessage, ContentBlock, Context, Usage};
    use ai::api::error::{ProviderError, ProviderErrorKind};
    use ai::api::hooks::{ProviderResponseInfo, ProviderStreamHooks};
    use ai::api::model::{Model, ThinkingConfig, lookup_model};
    use ai::api::provider::{ApiProvider, ProviderRegistry};
    use ai::api::stream::json::{parse_streaming_json, parse_terminal_json, repair_json};
    use ai::api::stream::{AssistantMessageEvent, EventStream, StreamOptions, complete};
    use ai::api::transport::RetryConfig;

    fn accepts<T>() {}

    accepts::<AiClient>();
    accepts::<EnvProviderAuthResolver>();
    accepts::<ProviderAuthDiagnostic>();
    accepts::<ModelCompat>();
    accepts::<AssistantMessage>();
    accepts::<ContentBlock>();
    accepts::<Context>();
    accepts::<Usage>();
    accepts::<ProviderError>();
    accepts::<ProviderErrorKind>();
    accepts::<ProviderResponseInfo>();
    accepts::<ProviderStreamHooks>();
    accepts::<Model>();
    accepts::<ThinkingConfig>();
    accepts::<ProviderRegistry>();
    accepts::<AssistantMessageEvent>();
    accepts::<EventStream>();
    accepts::<StreamOptions>();
    accepts::<RetryConfig>();

    let _ = lookup_model as fn(&str) -> Option<Model>;
    let _ = complete;
    let _ = parse_streaming_json as fn(&str) -> serde_json::Value;
    let _ = parse_terminal_json as fn(&str) -> Result<serde_json::Value, String>;
    let _ = repair_json as fn(&str) -> String;
    let _ = std::mem::size_of::<Option<&dyn ApiProvider>>();
}
use std::sync::{Arc, Mutex};
use support::EnvGuard;

#[test]
fn public_api_symbols_are_importable_from_api_facade() {
    let _ = all_models as fn() -> &'static [Model];
    let _ = get_models as fn(&str) -> Vec<Model>;
    let _ = get_providers as fn() -> Vec<String>;
    let _ = get_model as fn(&str, &str) -> Option<Model>;
    let _ = lookup_model as fn(&str) -> Option<Model>;
    let _ = calculate_cost as fn(&Model, &mut Usage);
    let _ = env_api_key as fn(&str) -> Option<String>;
    let _ = builtin_provider_apis as fn() -> &'static [&'static str];

    fn accepts<T>() {}
    accepts::<AssistantMessage>();
    accepts::<AssistantMessageEvent>();
    accepts::<ContentBlock>();
    accepts::<Context>();
    accepts::<Cost>();
    accepts::<Message>();
    accepts::<ModelCompat>();
    accepts::<ModelCost>();
    accepts::<ModelInput>();
    accepts::<OpenAICompletionsCompat>();
    accepts::<OpenRouterRouting>();
    accepts::<ProviderPayloadHook>();
    accepts::<ProviderPayloadHookFuture>();
    accepts::<ProviderAuthDiagnostic>();
    accepts::<ProviderResponseInfo>();
    accepts::<ProviderResponseHook>();
    accepts::<ProviderResponseHookFuture>();
    accepts::<ProviderStreamHooks>();
    accepts::<StopReason>();
    accepts::<StreamOptions>();
    accepts::<ThinkingConfig>();
    accepts::<ThinkingFormat>();
    accepts::<ThinkingLevelMap>();
    accepts::<ThinkingLevelValue>();
    accepts::<Tool>();
    accepts::<Usage>();
    accepts::<VercelGatewayRouting>();
    accepts::<CacheControlFormat>();
    accepts::<EventStream>();

    let _ = complete;
}

#[test]
fn retry_policy_helpers_are_importable_from_api_facade() {
    let options = StreamOptions {
        max_retries: Some(3),
        timeout_ms: Some(2_000),
        max_retry_delay_ms: Some(1_500),
        ..StreamOptions::default()
    };
    let retry_config = RetryConfig::from_options(Some(&options));

    assert_eq!(retry_config.max_retries, 3);
    assert_eq!(retry_config.timeout_ms, Some(2_000));
    assert_eq!(retry_config.max_retry_delay_ms, 1_500);
    assert!(is_retryable_status(429));
    assert!(is_retryable_status(503));
    assert!(!is_retryable_status(404));
    assert_eq!(parse_retry_after_ms(Some("1.25"), &retry_config), Ok(1_250));
    assert!(
        parse_retry_after_ms(Some("2"), &retry_config)
            .unwrap_err()
            .contains("exceeds max_retry_delay_ms")
    );
}

#[test]
fn provider_error_surface_is_importable_from_api_facade() {
    let error = ProviderError::http_status("openai-responses", "gpt-test", "openai", 429);

    assert_eq!(error.kind(), ProviderErrorKind::HttpStatus);
    assert_eq!(error.api(), "openai-responses");
    assert_eq!(error.provider(), Some("openai"));
    assert_eq!(error.model(), Some("gpt-test"));
    assert_eq!(error.status(), Some(429));
    assert!(error.to_string().contains("status 429"));
}

#[test]
fn compat_surface_is_importable_from_api_facade() {
    assert_eq!(
        compatibility_field_disposition("supportsTemperature"),
        Some(CompatibilityDisposition::Request)
    );
    assert_eq!(
        compatibility_field_disposition("sendSessionIdHeader"),
        Some(CompatibilityDisposition::CatalogOnly)
    );
    assert_eq!(compatibility_field_disposition("unknownField"), None);

    let anthropic = ModelCompat::AnthropicMessages(AnthropicMessagesCompat {
        supports_temperature: Some(true),
        supports_cache_control_on_tools: Some(false),
        ..AnthropicMessagesCompat::default()
    });
    let ModelCompat::AnthropicMessages(anthropic_compat) = anthropic else {
        panic!("expected anthropic messages compat");
    };
    assert_eq!(anthropic_compat.supports_temperature, Some(true));
    assert_eq!(
        anthropic_compat.supports_cache_control_on_tools,
        Some(false)
    );

    let responses = ModelCompat::OpenAIResponses(OpenAIResponsesCompat {
        send_session_id_header: Some(true),
        supports_long_cache_retention: Some(true),
    });
    let ModelCompat::OpenAIResponses(responses_compat) = responses else {
        panic!("expected openai responses compat");
    };
    assert_eq!(responses_compat.send_session_id_header, Some(true));
    assert_eq!(responses_compat.supports_long_cache_retention, Some(true));

    let thinking_map = ThinkingLevelMap {
        high: Some(ThinkingLevelValue::String("provider-high".into())),
        low: Some(ThinkingLevelValue::Null),
        ..ThinkingLevelMap::default()
    };
    assert_eq!(thinking_map.resolve("high"), Some("provider-high".into()));
    assert_eq!(thinking_map.resolve("low"), None);
    assert_eq!(thinking_map.resolve("medium"), Some("medium".into()));

    let compat = ModelCompat::OpenAICompletions(OpenAICompletionsCompat {
        thinking_format: Some(ThinkingFormat::OpenRouter),
        cache_control_format: Some(CacheControlFormat::Anthropic),
        open_router_routing: Some(OpenRouterRouting {
            allow_fallbacks: Some(false),
            ..OpenRouterRouting::default()
        }),
        vercel_gateway_routing: Some(VercelGatewayRouting {
            only: Some(vec!["openai".into()]),
            ..VercelGatewayRouting::default()
        }),
        ..OpenAICompletionsCompat::default()
    });
    let ModelCompat::OpenAICompletions(openai_compat) = compat else {
        panic!("expected openai completions compat");
    };
    assert_eq!(
        openai_compat.thinking_format,
        Some(ThinkingFormat::OpenRouter)
    );
    assert_eq!(
        openai_compat.cache_control_format,
        Some(CacheControlFormat::Anthropic)
    );
    assert_eq!(
        openai_compat
            .open_router_routing
            .and_then(|routing| routing.allow_fallbacks),
        Some(false)
    );
    assert_eq!(
        openai_compat
            .vercel_gateway_routing
            .and_then(|routing| routing.only)
            .unwrap(),
        vec!["openai".to_string()]
    );
}

#[test]
fn builtins_can_register_into_scoped_provider_registry() {
    let registry = ProviderRegistry::new();
    assert!(registry.lookup("openai-responses").is_none());

    register_builtins_into(&registry);

    for api in builtin_provider_apis() {
        assert!(
            registry.lookup(api).is_some(),
            "expected built-in api {api} to be registered into the scoped registry"
        );
    }
}

#[test]
fn builtin_provider_catalog_matches_scoped_registration_set() {
    let registry = ProviderRegistry::new();
    register_builtins_into(&registry);

    let expected = builtin_provider_apis()
        .iter()
        .map(|api| (*api).to_string())
        .collect::<Vec<_>>();
    assert_eq!(registry.registered_apis(), expected);
}

#[test]
fn scoped_ai_client_can_register_builtin_providers_directly() {
    let client = AiClient::new();
    assert!(client.lookup_provider("openai-responses").is_none());

    client.register_builtins();

    let expected = builtin_provider_apis()
        .iter()
        .map(|api| (*api).to_string())
        .collect::<Vec<_>>();
    assert_eq!(client.provider_registry().registered_apis(), expected);
    for api in builtin_provider_apis() {
        assert!(
            client.lookup_provider(api).is_some(),
            "expected built-in api {api} to be registered into the scoped AiClient"
        );
    }
}

#[test]
fn builtin_provider_catalog_is_stable_and_sorted() {
    assert_eq!(
        builtin_provider_apis(),
        &[
            "anthropic-messages",
            "azure-openai-responses",
            "google-generative-ai",
            "mistral-conversations",
            "openai-codex-responses",
            "openai-completions",
            "openai-responses",
        ]
    );
}

#[tokio::test]
async fn scoped_ai_client_streams_without_global_registration() {
    let client = AiClient::new();
    let model = scoped_model("scoped-only-api", "scoped-provider");
    client.register_provider(
        "scoped-only-api",
        Arc::new(StaticProvider::new("scoped response")),
    );

    let message = complete(client.stream_model(&model, empty_context(), None))
        .await
        .unwrap();
    assert_eq!(message_text(&message), "scoped response");

    let isolated_error = complete(AiClient::new().stream_model(&model, empty_context(), None))
        .await
        .unwrap_err();
    assert!(isolated_error.contains("unknown provider api: scoped-only-api"));
}

#[tokio::test]
async fn parallel_scoped_ai_clients_with_the_same_api_do_not_cross_talk() {
    let api = "shared-scoped-api";
    let model = scoped_model(api, "scoped-provider");
    let first = AiClient::new();
    let second = AiClient::new();
    first.register_provider(api, Arc::new(StaticProvider::new("first registry")));
    second.register_provider(api, Arc::new(StaticProvider::new("second registry")));

    let (first_result, second_result) = tokio::join!(
        complete(first.stream_model(&model, empty_context(), None)),
        complete(second.stream_model(&model, empty_context(), None)),
    );

    assert_eq!(message_text(&first_result.unwrap()), "first registry");
    assert_eq!(message_text(&second_result.unwrap()), "second registry");
}

#[tokio::test]
async fn scoped_ai_client_uses_injected_auth_resolver_without_stream_options() {
    let seen_api_key = Arc::new(Mutex::new(None));
    let client = AiClient::with_auth_resolver(Arc::new(StaticAuthResolver));
    let model = scoped_model("scoped-auth-none-api", "scoped-provider");
    client.register_provider(
        "scoped-auth-none-api",
        Arc::new(StaticProvider::recording(
            "auth response",
            Arc::clone(&seen_api_key),
        )),
    );

    complete(client.stream_model(&model, empty_context(), None))
        .await
        .unwrap();

    assert_eq!(
        seen_api_key.lock().unwrap().as_deref(),
        Some("scoped-key:scoped-provider")
    );
}

#[tokio::test]
async fn scoped_ai_client_uses_injected_auth_resolver() {
    let seen_api_key = Arc::new(Mutex::new(None));
    let client = AiClient::with_auth_resolver(Arc::new(StaticAuthResolver));
    let model = scoped_model("scoped-auth-api", "scoped-provider");
    client.register_provider(
        "scoped-auth-api",
        Arc::new(StaticProvider::recording(
            "auth response",
            Arc::clone(&seen_api_key),
        )),
    );

    complete(client.stream_model(&model, empty_context(), Some(StreamOptions::default())))
        .await
        .unwrap();

    assert_eq!(
        seen_api_key.lock().unwrap().as_deref(),
        Some("scoped-key:scoped-provider")
    );
}

#[tokio::test]
async fn env_auth_resolver_applies_azure_runtime_material_for_model() {
    let env = EnvGuard::new(&[
        "AZURE_OPENAI_API_KEY",
        "AZURE_OPENAI_API_VERSION",
        "AZURE_OPENAI_BASE_URL",
        "AZURE_OPENAI_RESOURCE_NAME",
        "AZURE_OPENAI_DEPLOYMENT_NAME_MAP",
    ]);
    env.set("AZURE_OPENAI_API_KEY", "azure-env-key");
    env.set("AZURE_OPENAI_API_VERSION", "2026-02-01");
    env.remove("AZURE_OPENAI_BASE_URL");
    env.set("AZURE_OPENAI_RESOURCE_NAME", "env-resource");
    env.set(
        "AZURE_OPENAI_DEPLOYMENT_NAME_MAP",
        "other-model=other-deployment,gpt-4o=gpt-4o-env",
    );

    let seen_options = Arc::new(Mutex::new(None));
    let client = AiClient::with_auth_resolver(Arc::new(EnvProviderAuthResolver));
    let mut model = scoped_model("env-azure-auth-api", "azure-openai-responses");
    model.id = "gpt-4o".into();
    client.register_provider(
        "env-azure-auth-api",
        Arc::new(RecordingOptionsProvider {
            seen_options: Arc::clone(&seen_options),
        }),
    );

    complete(client.stream_model(&model, empty_context(), None))
        .await
        .unwrap();

    let options = seen_options
        .lock()
        .unwrap()
        .clone()
        .expect("provider should receive resolver-populated stream options");
    assert_eq!(options.api_key.as_deref(), Some("azure-env-key"));
    assert_eq!(options.azure_api_version.as_deref(), Some("2026-02-01"));
    assert_eq!(options.azure_resource_name.as_deref(), Some("env-resource"));
    assert_eq!(options.azure_deployment_name.as_deref(), Some("gpt-4o-env"));
    let diagnostics = options
        .auth_diagnostics
        .iter()
        .map(|diagnostic| (diagnostic.field.as_str(), diagnostic.source.as_str()))
        .collect::<Vec<_>>();
    assert_eq!(
        diagnostics,
        vec![
            ("api_key", "AZURE_OPENAI_API_KEY"),
            ("azure_api_version", "AZURE_OPENAI_API_VERSION"),
            ("azure_resource_name", "AZURE_OPENAI_RESOURCE_NAME"),
            ("azure_deployment_name", "AZURE_OPENAI_DEPLOYMENT_NAME_MAP",),
        ]
    );
    let diagnostic_json = serde_json::to_string(&options.auth_diagnostics).unwrap();
    assert!(!diagnostic_json.contains("azure-env-key"));
    assert!(!diagnostic_json.contains("env-resource"));
    assert!(!diagnostic_json.contains("gpt-4o-env"));
}

#[tokio::test]
async fn scoped_ai_client_applies_injected_auth_material() {
    let seen_options = Arc::new(Mutex::new(None));
    let client = AiClient::with_auth_resolver(Arc::new(RichAuthResolver));
    let model = scoped_model("scoped-auth-material-api", "scoped-provider");
    client.register_provider(
        "scoped-auth-material-api",
        Arc::new(RecordingOptionsProvider {
            seen_options: Arc::clone(&seen_options),
        }),
    );

    complete(client.stream_model(
        &model,
        empty_context(),
        Some(StreamOptions {
            headers: Some(serde_json::json!({
                "x-explicit": "user",
                "x-auth": "explicit-overrides-auth",
            })),
            azure_api_version: Some("2026-01-01".into()),
            ..StreamOptions::default()
        }),
    ))
    .await
    .unwrap();

    let options = seen_options
        .lock()
        .unwrap()
        .clone()
        .expect("provider should receive stream options");
    assert_eq!(options.api_key.as_deref(), Some("rich-key:scoped-provider"));
    assert_eq!(
        options.azure_base_url.as_deref(),
        Some("https://scoped-resource.openai.azure.com/openai/v1")
    );
    assert_eq!(options.azure_api_version.as_deref(), Some("2026-01-01"));
    let headers = options
        .headers
        .as_ref()
        .and_then(|headers| headers.as_object())
        .expect("merged auth headers should be an object");
    assert_eq!(headers["x-auth"], "explicit-overrides-auth");
    assert_eq!(headers["x-extra"], "auth-extra");
    assert_eq!(headers["x-explicit"], "user");
}

struct StaticAuthResolver;

impl ProviderAuthResolver for StaticAuthResolver {
    fn resolve_api_key(&self, provider: &str) -> Option<String> {
        Some(format!("scoped-key:{provider}"))
    }
}

struct RichAuthResolver;

impl ProviderAuthResolver for RichAuthResolver {
    fn resolve_auth(&self, provider: &str) -> ProviderAuth {
        ProviderAuth {
            api_key: Some(format!("rich-key:{provider}")),
            headers: Some(serde_json::json!({
                "x-auth": "auth-default",
                "x-extra": "auth-extra",
            })),
            azure_base_url: Some("https://scoped-resource.openai.azure.com/openai/v1".into()),
            azure_api_version: Some("2025-12-01".into()),
            ..ProviderAuth::default()
        }
    }
}

struct StaticProvider {
    text: &'static str,
    seen_api_key: Option<Arc<Mutex<Option<String>>>>,
}

impl StaticProvider {
    fn new(text: &'static str) -> Self {
        Self {
            text,
            seen_api_key: None,
        }
    }

    fn recording(text: &'static str, seen_api_key: Arc<Mutex<Option<String>>>) -> Self {
        Self {
            text,
            seen_api_key: Some(seen_api_key),
        }
    }
}

impl ApiProvider for StaticProvider {
    fn stream(&self, model: &Model, _ctx: Context, opts: Option<StreamOptions>) -> EventStream {
        if let Some(seen_api_key) = &self.seen_api_key {
            *seen_api_key.lock().unwrap() = opts.as_ref().and_then(|opts| opts.api_key.clone());
        }
        let text = self.text.to_string();
        let mut message = AssistantMessage::empty(&model.api, &model.id);
        message.content.push(ContentBlock::Text {
            text,
            text_signature: None,
        });
        Box::pin(stream! {
            yield AssistantMessageEvent::Done {
                reason: StopReason::Stop,
                message,
            };
        })
    }
}

struct RecordingOptionsProvider {
    seen_options: Arc<Mutex<Option<StreamOptions>>>,
}

impl ApiProvider for RecordingOptionsProvider {
    fn stream(&self, model: &Model, _ctx: Context, opts: Option<StreamOptions>) -> EventStream {
        *self.seen_options.lock().unwrap() = opts;
        let mut message = AssistantMessage::empty(&model.api, &model.id);
        message.content.push(ContentBlock::Text {
            text: "recorded".into(),
            text_signature: None,
        });
        Box::pin(stream! {
            yield AssistantMessageEvent::Done {
                reason: StopReason::Stop,
                message,
            };
        })
    }
}

fn empty_context() -> Context {
    Context {
        system_prompt: None,
        messages: vec![],
        tools: None,
    }
}

fn scoped_model(api: &str, provider: &str) -> Model {
    Model {
        id: "scoped-model".into(),
        name: "Scoped Model".into(),
        api: api.into(),
        provider: provider.into(),
        base_url: String::new(),
        reasoning: false,
        thinking_level_map: None,
        input: vec![ModelInput::Text],
        cost: ModelCost::default(),
        context_window: 8_000,
        max_tokens: 1_024,
        headers: None,
        compat: None,
    }
}

fn message_text(message: &AssistantMessage) -> &str {
    match &message.content[0] {
        ContentBlock::Text { text, .. } => text.as_str(),
        _ => panic!("expected text content"),
    }
}
