use super::support;

use ai::api::provider::ApiProvider;
use ai::model::{Model, ModelCost, ModelInput};
use ai::protocol::{
    AssistantMessageEvent, ContentBlock, Context, Message, StreamOptions, ThinkingConfig, Tool,
};
use ai::providers::openai_codex_responses as codex;
use support::EnvGuard;

fn test_model() -> Model {
    Model {
        id: "gpt-5-codex".into(),
        name: "GPT-5 Codex".into(),
        api: "openai-codex-responses".into(),
        provider: "openai-codex".into(),
        base_url: "https://chatgpt.com/backend-api".into(),
        reasoning: true,
        thinking_level_map: None,
        input: vec![ModelInput::Text, ModelInput::Image],
        cost: ModelCost {
            known: true,
            input: 1.25,
            output: 10.0,
            cache_read: 0.125,
            cache_write: 0.0,
        },
        context_window: 400000,
        max_tokens: 100000,
        headers: None,
        compat: None,
    }
}

#[test]
fn codex_request_body_maps_context_tools_reasoning_and_cache() {
    let model = test_model();
    let ctx = Context {
        system_prompt: Some("You are Codex.".into()),
        messages: vec![Message::User {
            content: vec![ContentBlock::Text {
                text: "edit file".into(),
                text_signature: None,
            }],
        }],
        tools: Some(vec![Tool {
            name: "read".into(),
            description: Some("Read file".into()),
            parameters: serde_json::json!({"type": "object"}),
        }]),
    };
    let opts = StreamOptions {
        session_id: Some("session-xyz".into()),
        thinking: Some(ThinkingConfig {
            enabled: true,
            budget_tokens: None,
            effort: Some("high".into()),
        }),
        ..Default::default()
    };

    let req = codex::convert::build_request(&model, &ctx, &Some(opts));
    let json = serde_json::to_value(&req).unwrap();

    assert_eq!(json["model"], "gpt-5-codex");
    assert_eq!(json["store"], false);
    assert_eq!(json["stream"], true);
    assert_eq!(json["instructions"], "You are Codex.");
    assert_eq!(json["text"]["verbosity"], "low");
    assert_eq!(
        json["include"],
        serde_json::json!(["reasoning.encrypted_content"])
    );
    assert_eq!(json["prompt_cache_key"], "session-xyz");
    assert_eq!(json["tool_choice"], "auto");
    assert_eq!(json["parallel_tool_calls"], true);
    assert_eq!(json["reasoning"]["effort"], "high");
    assert_eq!(json["reasoning"]["summary"], "auto");
    assert_eq!(json["tools"][0]["name"], "read");
}

#[test]
fn codex_sse_url_matches_protocol_shape() {
    assert_eq!(
        codex::resolve_codex_url("https://chatgpt.com/backend-api"),
        "https://chatgpt.com/backend-api/codex/responses"
    );
}

#[test]
fn codex_headers_extract_account_id_and_session_affinity() {
    let token = "header.eyJodHRwczovL2FwaS5vcGVuYWkuY29tL2F1dGgiOnsiY2hhdGdwdF9hY2NvdW50X2lkIjoiYWNjdF8xMjMifX0.sig";
    let headers = codex::build_sse_headers(None, None, token, Some("session-1")).unwrap();

    assert_eq!(
        headers.get("chatgpt-account-id").map(String::as_str),
        Some("acct_123")
    );
    assert_eq!(
        headers.get("authorization").map(String::as_str),
        Some(format!("Bearer {}", token).as_str())
    );
    assert_eq!(
        headers.get("session-id").map(String::as_str),
        Some("session-1")
    );
    assert_eq!(
        headers.get("x-client-request-id").map(String::as_str),
        Some("session-1")
    );
}

#[tokio::test]
async fn codex_provider_missing_key_returns_error_event() {
    let provider = codex::OpenAICodexResponsesProvider::new(None);
    let model = test_model();
    let ctx = Context {
        system_prompt: None,
        messages: vec![],
        tools: None,
    };

    let env = EnvGuard::new(&["OPENAI_CODEX_API_KEY", "OPENAI_API_KEY"]);
    env.remove("OPENAI_CODEX_API_KEY");
    env.remove("OPENAI_API_KEY");

    let event_stream = provider.stream(&model, ctx, None);
    use futures::StreamExt;
    let events: Vec<_> = event_stream.collect().await;
    assert_eq!(events.len(), 1);
    assert!(matches!(events[0], AssistantMessageEvent::Error { .. }));
}
