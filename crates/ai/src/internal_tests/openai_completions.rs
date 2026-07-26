use super::support;

use ai::api::provider::ApiProvider;
use ai::compatibility::{
    ModelCompat, OpenAICompletionsCompat, ThinkingFormat, ThinkingLevelMap, ThinkingLevelValue,
};
use ai::model::{Model, ModelCost, ModelInput};
use ai::protocol::{
    AssistantMessageEvent, ContentBlock, Context, Message, StopReason, StreamOptions,
    ThinkingConfig, Tool,
};
use ai::providers::openai::completions;
use bytes::Bytes;
use futures::stream;
use support::EnvGuard;

fn test_model() -> Model {
    Model {
        id: "deepseek-v4-flash".into(),
        name: "DeepSeek V4 Flash".into(),
        api: "openai-completions".into(),
        provider: "deepseek".into(),
        base_url: "https://api.deepseek.com/v1".into(),
        reasoning: false,
        thinking_level_map: None,
        input: vec![ModelInput::Text],
        cost: ModelCost {
            known: true,
            input: 0.0,
            output: 0.0,
            cache_read: 0.0,
            cache_write: 0.0,
        },
        context_window: 128000,
        max_tokens: 8192,
        headers: None,
        compat: Some(ModelCompat::OpenAICompletions(OpenAICompletionsCompat {
            max_tokens_field: Some("max_tokens".into()),
            supports_usage_in_streaming: Some(true),
            supports_developer_role: Some(false),
            ..Default::default()
        })),
    }
}

fn fixture_bytes(path: &str) -> Vec<Bytes> {
    let content = std::fs::read_to_string(path).unwrap();
    vec![Bytes::from(content)]
}

fn deepseek_reasoning_model() -> Model {
    let mut model = test_model();
    model.reasoning = true;
    model.thinking_level_map = Some(ThinkingLevelMap {
        minimal: Some(ThinkingLevelValue::String("high".into())),
        low: Some(ThinkingLevelValue::String("high".into())),
        medium: Some(ThinkingLevelValue::String("high".into())),
        high: Some(ThinkingLevelValue::String("high".into())),
        xhigh: Some(ThinkingLevelValue::String("max".into())),
    });
    model.compat = Some(ModelCompat::OpenAICompletions(OpenAICompletionsCompat {
        thinking_format: Some(ThinkingFormat::DeepSeek),
        max_tokens_field: Some("max_tokens".into()),
        supports_usage_in_streaming: Some(true),
        supports_developer_role: Some(false),
        requires_reasoning_content_on_assistant_messages: Some(true),
        ..Default::default()
    }));
    model
}

#[test]
fn completions_request_maps_context_tools_and_options() {
    let model = test_model();
    let ctx = Context {
        system_prompt: Some("Be helpful.".into()),
        messages: vec![
            Message::User {
                content: vec![ContentBlock::Text {
                    text: "Hello".into(),
                    text_signature: None,
                }],
            },
            Message::Assistant {
                content: vec![ContentBlock::Text {
                    text: "Hi!".into(),
                    text_signature: None,
                }],
            },
            Message::ToolResult {
                tool_call_id: "call_test".into(),
                tool_name: Some("read".into()),
                is_error: None,
                content: vec![ContentBlock::Text {
                    text: "file contents".into(),
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
        temperature: Some(0.7),
        max_tokens: Some(1024),
        tool_choice: Some(serde_json::json!("auto")),
        ..Default::default()
    };

    let req = completions::convert::build_request(&model, &ctx, &Some(opts));

    assert_eq!(req.model, "deepseek-v4-flash");
    assert!(req.stream);
    assert!(req.stream_options.as_ref().unwrap().include_usage);
    assert_eq!(req.temperature, Some(0.7));
    assert_eq!(req.max_tokens, Some(1024));
    assert_eq!(
        req.tool_choice.as_ref().unwrap(),
        &serde_json::json!("auto")
    );

    // Messages: system + user + assistant + tool
    assert_eq!(req.messages.len(), 4);
    assert_eq!(req.messages[0].role, "system");
    assert_eq!(req.messages[1].role, "user");
    assert_eq!(req.messages[2].role, "assistant");
    assert_eq!(req.messages[3].role, "tool");
    assert_eq!(req.messages[3].tool_call_id.as_deref(), Some("call_test"));

    assert!(req.tools.is_some());
    assert_eq!(req.tools.unwrap().len(), 1);
}

#[test]
fn completions_request_enables_deepseek_thinking() {
    let model = deepseek_reasoning_model();
    let ctx = Context {
        system_prompt: None,
        messages: vec![Message::User {
            content: vec![ContentBlock::Text {
                text: "Think carefully.".into(),
                text_signature: None,
            }],
        }],
        tools: None,
    };
    let opts = StreamOptions {
        thinking: Some(ThinkingConfig {
            enabled: true,
            budget_tokens: Some(16384),
            effort: Some("xhigh".into()),
        }),
        ..Default::default()
    };

    let req = completions::convert::build_request(&model, &ctx, &Some(opts));
    let json = serde_json::to_value(&req).unwrap();

    assert_eq!(json["thinking"], serde_json::json!({ "type": "enabled" }));
    assert_eq!(json["reasoning_effort"], "max");
}

#[test]
fn completions_request_maps_the_complete_deepseek_thinking_matrix() {
    let model = deepseek_reasoning_model();
    let ctx = Context {
        system_prompt: None,
        messages: vec![],
        tools: None,
    };

    for (level, expected_effort) in [
        ("minimal", "high"),
        ("low", "high"),
        ("medium", "high"),
        ("high", "high"),
        ("xhigh", "max"),
    ] {
        let options = StreamOptions {
            temperature: Some(0.7),
            thinking: Some(ThinkingConfig {
                enabled: true,
                budget_tokens: None,
                effort: Some(level.into()),
            }),
            ..Default::default()
        };
        let request = completions::convert::build_request(&model, &ctx, &Some(options));
        let json = serde_json::to_value(request).unwrap();

        assert_eq!(
            json["thinking"],
            serde_json::json!({ "type": "enabled" }),
            "thinking payload for {level}"
        );
        assert_eq!(
            json["reasoning_effort"], expected_effort,
            "reasoning effort for {level}"
        );
        assert!(
            json.get("temperature").is_none(),
            "temperature must be omitted while DeepSeek thinking is enabled for {level}"
        );
    }
}

#[test]
fn completions_request_explicitly_disables_deepseek_thinking() {
    let model = deepseek_reasoning_model();
    let ctx = Context {
        system_prompt: None,
        messages: vec![],
        tools: None,
    };
    let options = StreamOptions {
        temperature: Some(0.7),
        thinking: Some(ThinkingConfig {
            enabled: false,
            budget_tokens: None,
            effort: None,
        }),
        ..Default::default()
    };

    let request = completions::convert::build_request(&model, &ctx, &Some(options));
    let json = serde_json::to_value(request).unwrap();

    assert_eq!(json["thinking"], serde_json::json!({ "type": "disabled" }));
    assert!(json.get("reasoning_effort").is_none());
    assert_eq!(json["temperature"], 0.7);
}

#[test]
fn completions_request_replays_complete_deepseek_tool_history() {
    let model = deepseek_reasoning_model();
    let ctx = Context {
        system_prompt: None,
        messages: vec![
            Message::User {
                content: vec![ContentBlock::Text {
                    text: "Check the weather.".into(),
                    text_signature: None,
                }],
            },
            Message::Assistant {
                content: vec![
                    ContentBlock::Thinking {
                        thinking: "I need the weather tool.".into(),
                        thinking_signature: None,
                        redacted: None,
                    },
                    ContentBlock::Text {
                        text: "I'll check.".into(),
                        text_signature: None,
                    },
                    ContentBlock::ToolCall {
                        id: "call_weather".into(),
                        name: "get_weather".into(),
                        arguments: serde_json::json!({"city": "Hangzhou"}),
                        thought_signature: None,
                    },
                ],
            },
            Message::ToolResult {
                tool_call_id: "call_weather".into(),
                tool_name: Some("get_weather".into()),
                is_error: Some(false),
                content: vec![ContentBlock::Text {
                    text: "sunny".into(),
                    text_signature: None,
                }],
            },
            Message::User {
                content: vec![ContentBlock::Text {
                    text: "And tomorrow?".into(),
                    text_signature: None,
                }],
            },
        ],
        tools: Some(vec![Tool {
            name: "get_weather".into(),
            description: Some("Get weather".into()),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {"city": {"type": "string"}},
                "required": ["city"]
            }),
        }]),
    };

    let req = completions::convert::build_request(&model, &ctx, &None);
    let json = serde_json::to_value(&req).unwrap();
    let assistant = &json["messages"][1];
    let tool_result = &json["messages"][2];

    assert_eq!(assistant["role"], "assistant");
    assert_eq!(assistant["reasoning_content"], "I need the weather tool.");
    assert_eq!(assistant["content"][0]["text"], "I'll check.");
    assert_eq!(assistant["tool_calls"][0]["id"], "call_weather");
    assert_eq!(
        assistant["tool_calls"][0]["function"]["name"],
        "get_weather"
    );
    assert_eq!(
        assistant["tool_calls"][0]["function"]["arguments"],
        r#"{"city":"Hangzhou"}"#
    );
    assert_eq!(tool_result["role"], "tool");
    assert_eq!(tool_result["tool_call_id"], "call_weather");
    assert_eq!(tool_result["content"], "sunny");
}

#[test]
fn completions_request_replays_deepseek_assistant_reasoning_without_tools() {
    let model = deepseek_reasoning_model();
    let ctx = Context {
        system_prompt: None,
        messages: vec![Message::Assistant {
            content: vec![
                ContentBlock::Thinking {
                    thinking: "prior reasoning".into(),
                    thinking_signature: None,
                    redacted: None,
                },
                ContentBlock::Text {
                    text: "prior answer".into(),
                    text_signature: None,
                },
            ],
        }],
        tools: None,
    };

    let req = completions::convert::build_request(&model, &ctx, &None);
    let json = serde_json::to_value(&req).unwrap();

    assert_eq!(json["messages"][0]["reasoning_content"], "prior reasoning");
}

#[tokio::test]
async fn completions_fixture_maps_text_tool_usage_and_done() {
    let body = stream::iter(
        fixture_bytes("tests/fixtures/openai-completions-text-tool.sse")
            .into_iter()
            .map(Ok::<_, String>),
    );
    let model = test_model();
    let event_stream = completions::stream::process(body, model, None);
    use futures::StreamExt;

    let events: Vec<_> = event_stream.collect().await;

    assert!(!events.is_empty(), "should have events");
    assert!(
        matches!(events[0], AssistantMessageEvent::Start { .. }),
        "first event should be Start"
    );

    let has_text = events
        .iter()
        .any(|e| matches!(e, AssistantMessageEvent::TextStart { .. }));
    let has_toolcall = events
        .iter()
        .any(|e| matches!(e, AssistantMessageEvent::ToolcallStart { .. }));
    assert!(has_text, "should have text events");
    assert!(has_toolcall, "should have tool call events");

    let last = events.last().unwrap();
    match last {
        AssistantMessageEvent::Done { reason, message } => {
            assert_eq!(*reason, StopReason::ToolUse);
            assert!(message.usage.total_tokens > 0);
            assert!(
                message.content.iter().any(|b| {
                    matches!(b, ContentBlock::ToolCall { name, .. } if name == "read")
                })
            );
        }
        _ => panic!("expected Done event, got {:?}", last),
    }
}

#[tokio::test]
async fn completions_stream_maps_reasoning_content_to_thinking_events() {
    let body = stream::iter(
        vec![Bytes::from(
            concat!(
                "data: {\"id\":\"chatcmpl-thinking\",\"model\":\"deepseek-v4-flash\",\"choices\":[{\"index\":0,\"delta\":{\"reasoning_content\":\"consider\"},\"finish_reason\":null}],\"usage\":null}\n\n",
                "data: {\"id\":\"chatcmpl-thinking\",\"model\":\"deepseek-v4-flash\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"answer\"},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":3,\"completion_tokens\":5,\"total_tokens\":8}}\n\n",
                "data: [DONE]\n\n"
            ),
        )]
        .into_iter()
        .map(Ok::<_, String>),
    );
    let model = deepseek_reasoning_model();
    let event_stream = completions::stream::process(body, model, None);
    use futures::StreamExt;

    let events: Vec<_> = event_stream.collect().await;

    assert!(events.iter().any(|event| matches!(
        event,
        AssistantMessageEvent::ThinkingDelta { delta, .. } if delta == "consider"
    )));

    match events.last().unwrap() {
        AssistantMessageEvent::Done { message, .. } => {
            assert!(message.content.iter().any(|block| matches!(
                block,
                ContentBlock::Thinking { thinking, .. } if thinking == "consider"
            )));
            assert!(message.content.iter().any(|block| matches!(
                block,
                ContentBlock::Text { text, .. } if text == "answer"
            )));
        }
        other => panic!("expected Done event, got {other:?}"),
    }
}

#[tokio::test]
async fn deepseek_stream_preserves_fragmented_tool_name_across_empty_argument_delta() {
    let body = stream::iter(
        vec![Bytes::from(concat!(
            "data: {\"id\":\"deepseek-tool\",\"model\":\"deepseek-v4-flash\",\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_bash\",\"type\":\"function\",\"function\":{\"name\":\"ba\",\"arguments\":\"{\\\"command\\\":\"}}]},\"finish_reason\":null}]}\n\n",
            "data: {\"id\":\"deepseek-tool\",\"model\":\"deepseek-v4-flash\",\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"name\":\"sh\",\"arguments\":\"\\\"pwd\\\"\"}}]},\"finish_reason\":null}]}\n\n",
            "data: {\"id\":\"deepseek-tool\",\"model\":\"deepseek-v4-flash\",\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"name\":\"\",\"arguments\":\"}\"}}]},\"finish_reason\":\"tool_calls\"}],\"usage\":{\"prompt_tokens\":3,\"completion_tokens\":5,\"total_tokens\":8}}\n\n",
            "data: [DONE]\n\n"
        ))]
        .into_iter()
        .map(Ok::<_, String>),
    );
    use futures::StreamExt;
    let events: Vec<_> = completions::stream::process(body, deepseek_reasoning_model(), None)
        .collect()
        .await;

    match events.last().unwrap() {
        AssistantMessageEvent::Done { message, .. } => {
            assert!(message.content.iter().any(|block| matches!(
                block,
                ContentBlock::ToolCall {
                    id,
                    name,
                    arguments,
                    ..
                } if id == "call_bash"
                    && name == "bash"
                    && arguments == &serde_json::json!({"command": "pwd"})
            )))
        }
        other => panic!("expected completed DeepSeek tool call, got {other:?}"),
    }
}

#[tokio::test]
async fn completions_clean_truncation_before_done_marker_is_an_error() {
    let content = std::fs::read_to_string("tests/fixtures/openai-completions-text-tool.sse")
        .unwrap()
        .replace("data: [DONE]", "");
    let body = stream::iter(vec![Ok::<_, String>(Bytes::from(content))]);
    use futures::StreamExt;
    let events: Vec<_> = completions::stream::process(body, test_model(), None)
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
async fn completions_reject_repairable_but_incomplete_final_tool_arguments() {
    let content = std::fs::read_to_string("tests/fixtures/openai-completions-text-tool.sse")
        .unwrap()
        .replace(r#":\"src/lib.rs\"}"#, r#":\"src/lib.rs\""#);
    let body = stream::iter(vec![Ok::<_, String>(Bytes::from(content))]);
    use futures::StreamExt;
    let events: Vec<_> = completions::stream::process(body, test_model(), None)
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
async fn completions_reject_nonzero_choice_and_non_function_tool_type() {
    use futures::StreamExt;

    for payload in [
        r#"{"id":"bad-choice","model":"test","choices":[{"index":1,"delta":{"content":"wrong"},"finish_reason":null}]}"#,
        r#"{"id":"bad-tool","model":"test","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call-1","type":"custom","function":{"name":"run","arguments":"{}"}}]},"finish_reason":null}]}"#,
    ] {
        let body = stream::iter(vec![Ok::<_, String>(Bytes::from(format!(
            "data: {payload}\n\ndata: [DONE]\n\n"
        )))]);
        let events: Vec<_> = completions::stream::process(body, test_model(), None)
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
async fn completions_provider_missing_key_returns_error_event() {
    let provider = completions::OpenAICompletionsProvider::new(None);
    let model = Model {
        id: "test-model".into(),
        name: "Test".into(),
        api: "openai-completions".into(),
        provider: "test".into(),
        base_url: "https://api.example.com".into(),
        reasoning: false,
        thinking_level_map: None,
        input: vec![ModelInput::Text],
        cost: ModelCost {
            known: true,
            input: 0.0,
            output: 0.0,
            cache_read: 0.0,
            cache_write: 0.0,
        },
        context_window: 128000,
        max_tokens: 8192,
        headers: None,
        compat: None,
    };
    let ctx = Context {
        system_prompt: Some("hello".into()),
        messages: vec![Message::User {
            content: vec![ContentBlock::Text {
                text: "hi".into(),
                text_signature: None,
            }],
        }],
        tools: None,
    };

    let env = EnvGuard::new(&["DEEPSEEK_API_KEY", "DEEPSEEK_KEY", "OPENAI_API_KEY"]);
    env.remove("DEEPSEEK_API_KEY");
    env.remove("DEEPSEEK_KEY");
    env.remove("OPENAI_API_KEY");

    let event_stream = provider.stream(&model, ctx, None);
    use futures::StreamExt;
    let events: Vec<_> = event_stream.collect().await;
    assert_eq!(events.len(), 1);
    assert!(
        matches!(events[0], AssistantMessageEvent::Error { .. }),
        "should be error event"
    );
}

#[tokio::test]
async fn complete_smoke_test() {
    let body = stream::iter(
        fixture_bytes("tests/fixtures/openai-completions-text-tool.sse")
            .into_iter()
            .map(Ok::<_, String>),
    );
    let model = test_model();
    let event_stream = completions::stream::process(body, model, None);
    let result = ai::protocol::stream::complete(event_stream).await;
    assert!(result.is_ok(), "complete() should return Ok");
    let msg = result.unwrap();
    assert!(!msg.content.is_empty());
    assert!(
        msg.content
            .iter()
            .any(|b| { matches!(b, ContentBlock::ToolCall { name, .. } if name == "read") })
    );
}
// Internal OpenAI completion tests.
