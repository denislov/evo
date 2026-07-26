use super::support;

use ai::api::provider::ApiProvider;
use ai::model::{Model, ModelCost, ModelInput};
use ai::protocol::{AssistantMessageEvent, ContentBlock, Context, Message, StreamOptions};
use ai::providers::azure_openai_responses;
use bytes::Bytes;
use futures::stream;
use support::EnvGuard;

fn test_model() -> Model {
    Model {
        id: "gpt-4o".into(),
        name: "GPT-4o".into(),
        api: "azure-openai-responses".into(),
        provider: "azure-openai-responses".into(),
        base_url: "".into(),
        reasoning: false,
        thinking_level_map: None,
        input: vec![ModelInput::Text, ModelInput::Image],
        cost: ModelCost {
            known: true,
            input: 2.5,
            output: 10.0,
            cache_read: 0.0,
            cache_write: 0.0,
        },
        context_window: 128000,
        max_tokens: 16384,
        headers: None,
        compat: None,
    }
}

fn fixture_bytes(path: &str) -> Vec<Bytes> {
    let content = std::fs::read_to_string(path).unwrap();
    vec![Bytes::from(content)]
}

#[test]
fn azure_target_normalizes_resource_url_api_version_and_deployment() {
    let model = test_model();
    let opts = StreamOptions {
        azure_resource_name: Some("evo-test".into()),
        azure_api_version: Some("2024-10-21".into()),
        azure_deployment_name: Some("gpt-4o-prod".into()),
        ..Default::default()
    };

    let target = azure_openai_responses::resolve_target(&model, &Some(opts)).unwrap();

    assert_eq!(target.deployment_name, "gpt-4o-prod");
    assert_eq!(
        target.url,
        "https://evo-test.openai.azure.com/openai/v1/responses?api-version=2024-10-21"
    );
}

#[test]
fn azure_request_uses_deployment_and_prompt_cache_key() {
    let model = test_model();
    let ctx = Context {
        system_prompt: Some("Be concise.".into()),
        messages: vec![Message::User {
            content: vec![ContentBlock::Text {
                text: "hello".into(),
                text_signature: None,
            }],
        }],
        tools: None,
    };
    let opts = StreamOptions {
        max_tokens: Some(1024),
        session_id: Some("session-abc".into()),
        azure_deployment_name: Some("gpt-4o-prod".into()),
        ..Default::default()
    };

    let req = azure_openai_responses::build_request(&model, &ctx, &Some(opts));
    let json = serde_json::to_value(&req).unwrap();

    assert_eq!(json["model"], "gpt-4o-prod");
    assert_eq!(json["instructions"], "Be concise.");
    assert_eq!(json["max_output_tokens"], 1024);
    assert_eq!(json["prompt_cache_key"], "session-abc");
}

#[tokio::test]
async fn azure_reuses_responses_stream_parser_with_azure_api_name() {
    let body = stream::iter(
        fixture_bytes("tests/fixtures/openai-responses-text-tool.sse")
            .into_iter()
            .map(Ok::<_, String>),
    );
    let model = test_model();
    let event_stream = ai::providers::openai::responses::stream::process_with_api_name(
        body,
        model,
        None,
        "azure-openai-responses",
    );
    use futures::StreamExt;

    let events: Vec<_> = event_stream.collect().await;
    match events.last().unwrap() {
        AssistantMessageEvent::Done { message, .. } => {
            assert_eq!(message.api, "azure-openai-responses");
            assert_eq!(message.provider.as_deref(), Some("azure-openai-responses"));
        }
        other => panic!("expected Done event, got {:?}", other),
    }
}

#[tokio::test]
async fn azure_provider_missing_key_returns_error_event() {
    let provider = azure_openai_responses::AzureOpenAIResponsesProvider::new(None);
    let model = test_model();
    let ctx = Context {
        system_prompt: None,
        messages: vec![Message::User {
            content: vec![ContentBlock::Text {
                text: "hi".into(),
                text_signature: None,
            }],
        }],
        tools: None,
    };

    let env = EnvGuard::new(&[
        "AZURE_OPENAI_API_KEY",
        "AZURE_OPENAI_API_VERSION",
        "AZURE_OPENAI_BASE_URL",
        "AZURE_OPENAI_RESOURCE_NAME",
        "AZURE_OPENAI_DEPLOYMENT_NAME_MAP",
    ]);
    env.remove("AZURE_OPENAI_API_KEY");
    env.remove("AZURE_OPENAI_API_VERSION");
    env.remove("AZURE_OPENAI_BASE_URL");
    env.remove("AZURE_OPENAI_RESOURCE_NAME");
    env.remove("AZURE_OPENAI_DEPLOYMENT_NAME_MAP");

    let event_stream = provider.stream(&model, ctx, None);
    use futures::StreamExt;
    let events: Vec<_> = event_stream.collect().await;
    assert_eq!(events.len(), 1);
    assert!(matches!(events[0], AssistantMessageEvent::Error { .. }));
}
