use ai::model::{Model, ModelCost, ModelInput};
use ai::protocol::{AssistantMessageEvent, ContentBlock, StopReason};
use bytes::Bytes;
use futures::stream;

fn test_model() -> Model {
    Model {
        id: "claude-sonnet-4-5".into(),
        name: "Claude Sonnet 4.5".into(),
        api: "anthropic-messages".into(),
        provider: "anthropic".into(),
        base_url: "https://api.anthropic.com".into(),
        reasoning: true,
        thinking_level_map: None,
        input: vec![ModelInput::Text],
        cost: ModelCost {
            known: true,
            input: 3.0,
            output: 15.0,
            cache_read: 0.30,
            cache_write: 3.75,
        },
        context_window: 200000,
        max_tokens: 8192,
        headers: None,
        compat: None,
    }
}

fn fixture_bytes(path: &str) -> Vec<Bytes> {
    let content = std::fs::read_to_string(path).unwrap();
    vec![Bytes::from(content)]
}

#[tokio::test]
async fn text_only_stream() {
    let body = stream::iter(
        fixture_bytes("tests/fixtures/anthropic-text.sse")
            .into_iter()
            .map(Ok::<_, String>),
    );
    let model = test_model();
    let event_stream = ai::providers::anthropic::stream::process(body, model, None);
    use futures::StreamExt;

    let events: Vec<_> = event_stream.collect().await;
    assert!(
        events.len() >= 3,
        "expected at least 3 events, got {}",
        events.len()
    );

    assert!(matches!(events[0], AssistantMessageEvent::Start { .. }));

    let has_text_start = events
        .iter()
        .any(|e| matches!(e, AssistantMessageEvent::TextStart { .. }));
    let has_text_delta = events
        .iter()
        .any(|e| matches!(e, AssistantMessageEvent::TextDelta { .. }));
    let has_text_end = events
        .iter()
        .any(|e| matches!(e, AssistantMessageEvent::TextEnd { .. }));
    assert!(has_text_start);
    assert!(has_text_delta);
    assert!(has_text_end);

    let last = events.last().unwrap();
    match last {
        AssistantMessageEvent::Done { reason, message } => {
            assert_eq!(*reason, StopReason::Stop);
            assert!(!message.content.is_empty());
            if let ContentBlock::Text { text, .. } = &message.content[0] {
                assert!(text.contains("Hello"));
            } else {
                panic!("expected text content block");
            }
        }
        _ => panic!("expected Done event, got {:?}", last),
    }
}

#[tokio::test]
async fn thinking_and_tool_use_stream() {
    let body = stream::iter(
        fixture_bytes("tests/fixtures/anthropic-thinking-tooluse.sse")
            .into_iter()
            .map(Ok::<_, String>),
    );
    let model = test_model();
    let event_stream = ai::providers::anthropic::stream::process(body, model, None);
    use futures::StreamExt;

    let events: Vec<_> = event_stream.collect().await;

    let has_thinking = events
        .iter()
        .any(|e| matches!(e, AssistantMessageEvent::ThinkingStart { .. }));
    let has_toolcall = events
        .iter()
        .any(|e| matches!(e, AssistantMessageEvent::ToolcallStart { .. }));
    let has_text = events
        .iter()
        .any(|e| matches!(e, AssistantMessageEvent::TextStart { .. }));
    assert!(has_thinking, "should have thinking events");
    assert!(has_toolcall, "should have tool use events");
    assert!(has_text, "should have text events");

    match events.last().unwrap() {
        AssistantMessageEvent::Done { reason, message } => {
            assert_eq!(*reason, StopReason::ToolUse);
            let has_complete_tool = message.content.iter().any(|b| {
                matches!(b, ContentBlock::ToolCall { arguments, .. } if arguments.as_object().is_some_and(|o| o.contains_key("city")))
            });
            assert!(has_complete_tool, "tool call should have parsed arguments");
        }
        _ => panic!("expected Done event"),
    }
}

#[tokio::test]
async fn anthropic_clean_truncation_is_an_error_terminal() {
    let content = std::fs::read_to_string("tests/fixtures/anthropic-text.sse").unwrap();
    let truncated = content
        .split("event: message_stop")
        .next()
        .expect("fixture should contain message_stop");
    let body = stream::iter(vec![Ok::<_, String>(Bytes::from(truncated.to_string()))]);
    use futures::StreamExt;
    let events: Vec<_> = ai::providers::anthropic::stream::process(body, test_model(), None)
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
async fn anthropic_rejects_repairable_but_incomplete_final_tool_arguments() {
    let content = std::fs::read_to_string("tests/fixtures/anthropic-thinking-tooluse.sse")
        .unwrap()
        .replace(" York\\\"}", " York\\\"");
    let body = stream::iter(vec![Ok::<_, String>(Bytes::from(content))]);
    use futures::StreamExt;
    let events: Vec<_> = ai::providers::anthropic::stream::process(body, test_model(), None)
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
async fn anthropic_rejects_interleaved_block_indices() {
    let body = stream::iter(vec![Ok::<_, String>(Bytes::from(concat!(
        "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
        "data: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"text_delta\",\"text\":\"wrong\"}}\n\n"
    )))]);
    use futures::StreamExt;
    let events: Vec<_> = ai::providers::anthropic::stream::process(body, test_model(), None)
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
// Internal Anthropic mapping tests.
