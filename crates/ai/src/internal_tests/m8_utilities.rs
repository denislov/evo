use ai::compatibility::{
    ModelCompat, OpenAICompletionsCompat, ThinkingFormat, ThinkingLevelMap, ThinkingLevelValue,
};
use ai::model::{Model, ModelCost, ModelInput};
use ai::protocol::{AssistantMessage, ContentBlock, Message};

#[test]
fn short_hash_matches_typescript_vector() {
    assert_eq!(ai::testing::hash::short_hash("hello"), "1h6qa0qrowduu");
    assert_eq!(
        ai::testing::hash::short_hash("M8-provider-breadth"),
        "10smtgvmd1v6u"
    );
}

#[test]
fn pkce_challenge_matches_rfc_vector_and_pages_escape_html() {
    let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
    assert_eq!(
        ai::testing::oauth::pkce_challenge(verifier),
        "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
    );

    let html = ai::testing::oauth::success_html("<token ok>");
    assert!(html.contains("&lt;token ok&gt;"));
    assert!(!html.contains("<token ok>"));

    let html = ai::testing::oauth::error_html("failed", Some("<secret>"));
    assert!(html.contains("&lt;secret&gt;"));
}

#[test]
fn diagnostics_are_structured_and_append_to_message() {
    let mut msg = AssistantMessage::empty("test-api", "test-model");
    let diagnostic = ai::testing::diagnostics::create(
        "provider_transport_failure",
        "websocket failed",
        Some(serde_json::json!({"fallbackTransport": "sse"})),
    );

    ai::testing::diagnostics::append(&mut msg, diagnostic);

    let diagnostics = msg.diagnostics.as_ref().unwrap();
    assert_eq!(diagnostics.len(), 1);
    assert_eq!(diagnostics[0].diagnostic_type, "provider_transport_failure");
    assert_eq!(
        diagnostics[0].error.as_ref().unwrap().message,
        "websocket failed"
    );
    assert_eq!(
        diagnostics[0].details.as_ref().unwrap()["fallbackTransport"],
        "sse"
    );
}

#[test]
fn cloudflare_placeholders_and_copilot_headers_match_ts_behavior() {
    let resolved = ai::testing::cloudflare::resolve_base_url_with(
        "https://gateway.ai.cloudflare.com/v1/{CLOUDFLARE_ACCOUNT_ID}/{CLOUDFLARE_GATEWAY_ID}/compat",
        |name| match name {
            "CLOUDFLARE_ACCOUNT_ID" => Some("acct"),
            "CLOUDFLARE_GATEWAY_ID" => Some("gw"),
            _ => None,
        },
    )
    .unwrap();
    assert_eq!(
        resolved,
        "https://gateway.ai.cloudflare.com/v1/acct/gw/compat"
    );

    let messages = vec![Message::User {
        content: vec![ContentBlock::Image {
            data: "abc".into(),
            mime_type: "image/png".into(),
        }],
    }];
    assert_eq!(
        ai::testing::copilot_headers::infer_copilot_initiator(&messages),
        "user"
    );
    assert!(ai::testing::copilot_headers::has_copilot_vision_input(
        &messages
    ));
    let headers = ai::testing::copilot_headers::build_dynamic_headers(&messages, true);
    assert_eq!(headers["X-Initiator"], "user");
    assert_eq!(headers["Copilot-Vision-Request"], "true");
}

#[test]
fn typed_compat_parses_openai_responses_and_openrouter_routing() {
    let model = Model {
        id: "openrouter/auto".into(),
        name: "OpenRouter Auto".into(),
        api: "openai-completions".into(),
        provider: "openrouter".into(),
        base_url: "https://openrouter.ai/api/v1".into(),
        reasoning: true,
        thinking_level_map: Some(ThinkingLevelMap {
            low: Some(ThinkingLevelValue::String("low".into())),
            medium: Some(ThinkingLevelValue::String("medium".into())),
            high: Some(ThinkingLevelValue::String("high".into())),
            ..Default::default()
        }),
        input: vec![ModelInput::Text],
        cost: ModelCost::default(),
        context_window: 100,
        max_tokens: 10,
        headers: None,
        compat: Some(ModelCompat::OpenAICompletions(OpenAICompletionsCompat {
            supports_usage_in_streaming: Some(false),
            thinking_format: Some(ThinkingFormat::OpenRouter),
            open_router_routing: Some(ai::compatibility::OpenRouterRouting {
                allow_fallbacks: Some(false),
                order: Some(vec!["anthropic".into(), "openai".into()]),
                ..Default::default()
            }),
            ..Default::default()
        })),
    };

    let compat = ai::compatibility::OpenAICompletionsCompat::from_model(&model);
    assert_eq!(compat.supports_usage_in_streaming, Some(false));
    assert_eq!(
        compat.thinking_format,
        Some(ai::compatibility::ThinkingFormat::OpenRouter)
    );
    assert_eq!(
        compat.open_router_routing.unwrap().order,
        Some(vec!["anthropic".into(), "openai".into()])
    );
    assert_eq!(
        model.thinking_level_map.unwrap().high,
        Some(ThinkingLevelValue::String("high".into()))
    );
}
// Internal provider utility tests.
