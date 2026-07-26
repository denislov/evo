use ai::model::{Model, ModelCost, ModelInput};
use ai::protocol::{AssistantMessageEvent, ContentBlock, StopReason};
use ai::providers::openai::responses;
use bytes::Bytes;
use futures::stream;
fn test_model() -> Model {
    Model {
        id: "gpt-4.1".into(),
        name: "GPT-4.1".into(),
        api: "openai-responses".into(),
        provider: "openai".into(),
        base_url: "https://api.openai.com/v1".into(),
        reasoning: false,
        thinking_level_map: None,
        input: vec![ModelInput::Text, ModelInput::Image],
        cost: ModelCost {
            known: true,
            input: 2.0,
            output: 8.0,
            cache_read: 0.5,
            cache_write: 0.0,
        },
        context_window: 1_047_576,
        max_tokens: 32_768,
        headers: None,
        compat: None,
    }
}

fn fixture_bytes(path: &str) -> Vec<Bytes> {
    let content = std::fs::read_to_string(path).unwrap();
    vec![Bytes::from(content)]
}

#[tokio::test]
async fn responses_fixture_maps_text_tool_and_done() {
    let body = stream::iter(
        fixture_bytes("tests/fixtures/openai-responses-text-tool.sse")
            .into_iter()
            .map(Ok::<_, String>),
    );
    let model = test_model();
    let event_stream = responses::stream::process(body, model, None);
    use futures::StreamExt;

    let events: Vec<_> = event_stream.collect().await;

    assert!(!events.is_empty(), "should have events");
    assert!(
        matches!(events[0], AssistantMessageEvent::Start { .. }),
        "first event should be Start"
    );

    let has_text = events
        .iter()
        .any(|e| matches!(e, AssistantMessageEvent::TextDelta { .. }));
    let has_toolcall = events
        .iter()
        .any(|e| matches!(e, AssistantMessageEvent::ToolcallDelta { .. }));
    assert!(has_text, "should have text delta events");
    assert!(has_toolcall, "should have tool call delta events");

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
async fn complete_smoke_test() {
    let body = stream::iter(
        fixture_bytes("tests/fixtures/openai-responses-text-tool.sse")
            .into_iter()
            .map(Ok::<_, String>),
    );
    let model = test_model();
    let event_stream = responses::stream::process(body, model, None);
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

async fn collect_sse(input: String) -> Vec<AssistantMessageEvent> {
    use futures::StreamExt;
    responses::stream::process(
        stream::iter(vec![Ok::<_, String>(Bytes::from(input))]),
        test_model(),
        None,
    )
    .collect()
    .await
}

#[tokio::test]
async fn responses_clean_truncation_before_completed_is_an_error() {
    let content = std::fs::read_to_string("tests/fixtures/openai-responses-text-tool.sse").unwrap();
    let truncated = content
        .split("event: response.completed")
        .next()
        .expect("fixture should contain response.completed")
        .to_string();
    let events = collect_sse(truncated).await;
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
async fn responses_failed_incomplete_and_cancelled_are_error_terminals() {
    for (event_type, expected_reason) in [
        ("response.failed", StopReason::Error),
        ("response.incomplete", StopReason::Error),
        ("response.cancelled", StopReason::Aborted),
    ] {
        let input = format!(
            "data: {{\"type\":\"response.created\",\"response\":{{\"id\":\"resp-1\"}}}}\n\n\
             data: {{\"type\":\"{event_type}\",\"response\":{{\"id\":\"resp-1\",\"status\":\"failed\",\"error\":{{\"type\":\"server_error\",\"code\":\"bad\",\"message\":\"request failed\"}}}}}}\n\n"
        );
        let events = collect_sse(input).await;
        assert!(matches!(
            events.last(),
            Some(AssistantMessageEvent::Error { reason, .. }) if *reason == expected_reason
        ));
    }
}

#[tokio::test]
async fn responses_unknown_bookkeeping_is_ignored_but_significant_unknown_fails() {
    let harmless = concat!(
        "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp-1\"}}\n\n",
        "data: {\"type\":\"response.custom_bookkeeping\",\"sequence\":1}\n\n",
        "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp-1\",\"status\":\"completed\"}}\n\n"
    );
    let events = collect_sse(harmless.into()).await;
    assert!(matches!(
        events.last(),
        Some(AssistantMessageEvent::Done { .. })
    ));

    let significant = concat!(
        "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp-1\"}}\n\n",
        "data: {\"type\":\"response.future.delta\",\"delta\":\"lost\"}\n\n"
    );
    let events = collect_sse(significant.into()).await;
    assert!(matches!(
        events.last(),
        Some(AssistantMessageEvent::Error { .. })
    ));
}

#[tokio::test]
async fn responses_interleaved_output_items_preserve_content_and_tool_arguments() {
    let input = concat!(
        "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp-1\"}}\n\n",
        "data: {\"type\":\"response.content_part.added\",\"item_id\":\"msg-1\",\"part\":{\"type\":\"output_text\"}}\n\n",
        "data: {\"type\":\"response.output_item.added\",\"item\":{\"id\":\"tool-1\",\"type\":\"function_call\",\"name\":\"read\",\"call_id\":\"call-1\"}}\n\n",
        "data: {\"type\":\"response.output_text.delta\",\"item_id\":\"msg-1\",\"delta\":\"hello\"}\n\n",
        "data: {\"type\":\"response.function_call_arguments.delta\",\"item_id\":\"tool-1\",\"delta\":\"{\\\"path\\\":\\\"Cargo.toml\\\"}\"}\n\n",
        "data: {\"type\":\"response.output_item.done\",\"item\":{\"id\":\"msg-1\",\"type\":\"message\"}}\n\n",
        "data: {\"type\":\"response.output_item.done\",\"item\":{\"id\":\"tool-1\",\"type\":\"function_call\"}}\n\n",
        "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp-1\",\"status\":\"completed\"}}\n\n"
    );
    let events = collect_sse(input.into()).await;
    let Some(AssistantMessageEvent::Done { message, .. }) = events.last() else {
        panic!("expected Done event: {events:?}");
    };
    assert!(
        message
            .content
            .iter()
            .any(|block| matches!(block, ContentBlock::Text { text, .. } if text == "hello"))
    );
    assert!(message.content.iter().any(|block| matches!(
        block,
        ContentBlock::ToolCall { arguments, .. } if arguments["path"] == "Cargo.toml"
    )));
}

#[tokio::test]
async fn responses_malformed_final_tool_arguments_fail_closed() {
    for arguments in [
        "{bad",
        r#"{"path":"Cargo.toml""#,
        r#"{"path":"bad\x"}"#,
        r#"{"path":"Cargo.toml"} trailing"#,
        r#"[1,2,"#,
    ] {
        let delta = serde_json::json!({
            "type": "response.function_call_arguments.delta",
            "item_id": "tool-1",
            "delta": arguments
        });
        let input = format!(
            "data: {{\"type\":\"response.created\",\"response\":{{\"id\":\"resp-1\"}}}}\n\n\
             data: {{\"type\":\"response.output_item.added\",\"item\":{{\"id\":\"tool-1\",\"type\":\"function_call\",\"name\":\"read\"}}}}\n\n\
             data: {delta}\n\n\
             data: {{\"type\":\"response.completed\",\"response\":{{\"id\":\"resp-1\",\"status\":\"completed\"}}}}\n\n"
        );
        let events = collect_sse(input).await;
        assert!(
            matches!(events.last(), Some(AssistantMessageEvent::Error { .. })),
            "terminal arguments were accepted: {arguments:?}"
        );
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, AssistantMessageEvent::Done { .. }))
        );
    }
}
// Internal OpenAI response tests.
