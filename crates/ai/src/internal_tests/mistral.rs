use super::support;

use ai::api::provider::ApiProvider;
use ai::model::{Model, ModelCost, ModelInput};
use ai::protocol::{
    AssistantMessageEvent, ContentBlock, Context, Message, StopReason, StreamOptions,
    ThinkingConfig, Tool,
};
use ai::providers::mistral;
use bytes::Bytes;
use futures::stream;
use support::EnvGuard;

fn test_model() -> Model {
    Model {
        id: "mistral-small-latest".into(),
        name: "Mistral Small".into(),
        api: "mistral-conversations".into(),
        provider: "mistral".into(),
        base_url: "https://api.mistral.ai".into(),
        reasoning: true,
        thinking_level_map: None,
        input: vec![ModelInput::Text, ModelInput::Image],
        cost: ModelCost {
            known: true,
            input: 0.0,
            output: 0.0,
            cache_read: 0.0,
            cache_write: 0.0,
        },
        context_window: 256000,
        max_tokens: 8192,
        headers: None,
        compat: None,
    }
}

fn fixture_bytes(path: &str) -> Vec<Bytes> {
    let content = std::fs::read_to_string(path).unwrap();
    vec![Bytes::from(content)]
}

#[test]
fn mistral_request_maps_context_tools_options_and_reasoning() {
    let model = test_model();
    let ctx = Context {
        system_prompt: Some("Be direct.".into()),
        messages: vec![
            Message::User {
                content: vec![
                    ContentBlock::Text {
                        text: "Describe this".into(),
                        text_signature: None,
                    },
                    ContentBlock::Image {
                        data: "abc123".into(),
                        mime_type: "image/png".into(),
                    },
                ],
            },
            Message::Assistant {
                content: vec![ContentBlock::ToolCall {
                    id: "call_abcdefghi".into(),
                    name: "read".into(),
                    arguments: serde_json::json!({"path": "Cargo.toml"}),
                    thought_signature: None,
                }],
            },
            Message::ToolResult {
                tool_call_id: "call_abcdefghi".into(),
                tool_name: Some("read".into()),
                is_error: Some(true),
                content: vec![ContentBlock::Text {
                    text: "missing".into(),
                    text_signature: None,
                }],
            },
        ],
        tools: Some(vec![Tool {
            name: "read".into(),
            description: Some("Read a file".into()),
            parameters: serde_json::json!({"type": "object", "properties": {"path": {"type": "string"}}}),
        }]),
    };
    let opts = StreamOptions {
        temperature: Some(0.2),
        max_tokens: Some(2048),
        tool_choice: Some(serde_json::json!("any")),
        thinking: Some(ThinkingConfig {
            enabled: true,
            budget_tokens: None,
            effort: Some("high".into()),
        }),
        ..Default::default()
    };

    let req = mistral::convert::build_request(&model, &ctx, &Some(opts));
    let json = serde_json::to_value(&req).unwrap();

    assert_eq!(json["model"], "mistral-small-latest");
    assert_eq!(json["stream"], true);
    assert_eq!(json["temperature"], 0.2);
    assert_eq!(json["max_tokens"], 2048);
    assert_eq!(json["tool_choice"], "any");
    assert_eq!(json["reasoning_effort"], "high");
    assert_eq!(json["messages"][0]["role"], "system");
    assert_eq!(json["messages"][0]["content"], "Be direct.");
    assert_eq!(json["messages"][1]["content"][1]["type"], "image_url");
    assert_eq!(
        json["messages"][1]["content"][1]["image_url"],
        "data:image/png;base64,abc123"
    );
    assert_eq!(
        json["messages"][2]["tool_calls"][0]["function"]["name"],
        "read"
    );
    assert_eq!(
        json["messages"][3]["content"][0]["text"],
        "[tool error] missing"
    );
    assert_eq!(json["tools"][0]["function"]["strict"], false);
}

#[test]
fn mistral_headers_include_session_affinity_without_overriding_explicit_header() {
    let mut model = test_model();
    model.headers = Some(serde_json::json!({"x-model": "yes"}));
    let opts = StreamOptions {
        session_id: Some("session-1".into()),
        headers: Some(serde_json::json!({"x-extra": "yes"})),
        ..Default::default()
    };

    let headers = mistral::build_headers(&model, &Some(opts));

    assert_eq!(headers.get("x-model").map(String::as_str), Some("yes"));
    assert_eq!(headers.get("x-extra").map(String::as_str), Some("yes"));
    assert_eq!(
        headers.get("x-affinity").map(String::as_str),
        Some("session-1")
    );

    let opts = StreamOptions {
        session_id: Some("session-1".into()),
        headers: Some(serde_json::json!({"x-affinity": "explicit"})),
        ..Default::default()
    };
    let headers = mistral::build_headers(&model, &Some(opts));
    assert_eq!(
        headers.get("x-affinity").map(String::as_str),
        Some("explicit")
    );
}

#[tokio::test]
async fn mistral_fixture_maps_text_thinking_tool_usage_and_done() {
    let body = stream::iter(
        fixture_bytes("tests/fixtures/mistral-text-thinking-tool.sse")
            .into_iter()
            .map(Ok::<_, String>),
    );
    let model = test_model();
    let event_stream = mistral::stream::process(body, model, None);
    use futures::StreamExt;

    let events: Vec<_> = event_stream.collect().await;

    assert!(matches!(events[0], AssistantMessageEvent::Start { .. }));
    assert!(
        events
            .iter()
            .any(|e| matches!(e, AssistantMessageEvent::ThinkingStart { .. })),
        "should include thinking events"
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, AssistantMessageEvent::ToolcallStart { .. })),
        "should include tool call events"
    );

    match events.last().unwrap() {
        AssistantMessageEvent::Done { reason, message } => {
            assert_eq!(*reason, StopReason::ToolUse);
            assert_eq!(message.api, "mistral-conversations");
            assert_eq!(message.provider.as_deref(), Some("mistral"));
            assert_eq!(message.response_id.as_deref(), Some("mistral-stream-1"));
            assert_eq!(message.usage.input, 10);
            assert_eq!(message.usage.output, 5);
            assert_eq!(message.usage.total_tokens, 15);
            assert!(
                message
                    .content
                    .iter()
                    .any(|b| matches!(b, ContentBlock::Text { text, .. } if text == "Hello world"))
            );
            assert!(message.content.iter().any(
                |b| matches!(b, ContentBlock::Thinking { thinking, .. } if thinking == "consider")
            ));
            assert!(message.content.iter().any(|b| {
                matches!(b, ContentBlock::ToolCall { name, arguments, .. } if name == "read" && arguments["path"] == "file")
            }));
        }
        other => panic!("expected Done event, got {:?}", other),
    }
}

#[tokio::test]
async fn mistral_clean_truncation_before_done_marker_is_an_error() {
    let content = std::fs::read_to_string("tests/fixtures/mistral-text-thinking-tool.sse")
        .unwrap()
        .replace("data: [DONE]", "");
    let body = stream::iter(vec![Ok::<_, String>(Bytes::from(content))]);
    use futures::StreamExt;
    let events: Vec<_> = mistral::stream::process(body, test_model(), None)
        .collect()
        .await;
    assert!(matches!(
        events.last(),
        Some(AssistantMessageEvent::Error { .. })
    ));
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, AssistantMessageEvent::Done { .. }))
    );
}

#[tokio::test]
async fn mistral_rejects_repairable_but_incomplete_final_tool_arguments() {
    let content = std::fs::read_to_string("tests/fixtures/mistral-text-thinking-tool.sse")
        .unwrap()
        .replace("file\\\"}", "file\\\"");
    let body = stream::iter(vec![Ok::<_, String>(Bytes::from(content))]);
    use futures::StreamExt;
    let events: Vec<_> = mistral::stream::process(body, test_model(), None)
        .collect()
        .await;

    assert!(matches!(
        events.last(),
        Some(AssistantMessageEvent::Error {
            reason: StopReason::Error,
            ..
        })
    ));
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, AssistantMessageEvent::Done { .. }))
    );
}

#[tokio::test]
async fn mistral_rejects_nonzero_choice_and_non_function_tool_type() {
    use futures::StreamExt;

    for payload in [
        r#"{"id":"bad-choice","model":"test","choices":[{"index":1,"delta":{"content":"wrong"},"finish_reason":null}]}"#,
        r#"{"id":"bad-tool","model":"test","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call-1","type":"custom","function":{"name":"run","arguments":"{}"}}]},"finish_reason":null}]}"#,
    ] {
        let body = stream::iter(vec![Ok::<_, String>(Bytes::from(format!(
            "data: {payload}\n\ndata: [DONE]\n\n"
        )))]);
        let events: Vec<_> = mistral::stream::process(body, test_model(), None)
            .collect()
            .await;
        assert!(matches!(
            events.last(),
            Some(AssistantMessageEvent::Error {
                reason: StopReason::Error,
                ..
            })
        ));
    }
}

#[tokio::test]
async fn mistral_provider_missing_key_returns_error_event() {
    let provider = mistral::MistralProvider::new(None);
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

    let env = EnvGuard::new(&["MISTRAL_API_KEY"]);
    env.remove("MISTRAL_API_KEY");

    let event_stream = provider.stream(&model, ctx, None);
    use futures::StreamExt;
    let events: Vec<_> = event_stream.collect().await;
    assert_eq!(events.len(), 1);
    assert!(matches!(events[0], AssistantMessageEvent::Error { .. }));
}
