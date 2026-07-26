use ai::model::{Model, ModelCost, ModelInput};
use ai::protocol::{AssistantMessageEvent, ContentBlock, Context, Message, StopReason};
use ai::providers::google;
use bytes::Bytes;
use futures::stream;
fn test_model() -> Model {
    Model {
        id: "gemini-2.5-flash".into(),
        name: "Gemini 2.5 Flash".into(),
        api: "google-generative-ai".into(),
        provider: "google".into(),
        base_url: "https://generativelanguage.googleapis.com/v1beta".into(),
        reasoning: false,
        thinking_level_map: None,
        input: vec![ModelInput::Text, ModelInput::Image],
        cost: ModelCost {
            known: true,
            input: 0.15,
            output: 0.6,
            cache_read: 0.0,
            cache_write: 0.0,
        },
        context_window: 1_048_576,
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
async fn google_fixture_maps_thinking_tool_and_done() {
    let body = stream::iter(
        fixture_bytes("tests/fixtures/google-text-tool.sse")
            .into_iter()
            .map(Ok::<_, String>),
    );
    let model = test_model();
    let event_stream = google::stream::process(body, model, None);
    use futures::StreamExt;

    let events: Vec<_> = event_stream.collect().await;

    assert!(!events.is_empty(), "should have events");
    assert!(
        matches!(events[0], AssistantMessageEvent::Start { .. }),
        "first event should be Start"
    );

    let has_thinking = events
        .iter()
        .any(|e| matches!(e, AssistantMessageEvent::ThinkingStart { .. }));
    let has_toolcall = events
        .iter()
        .any(|e| matches!(e, AssistantMessageEvent::ToolcallStart { .. }));
    assert!(has_thinking, "should have thinking events");
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
async fn complete_smoke_test() {
    let body = stream::iter(
        fixture_bytes("tests/fixtures/google-text-tool.sse")
            .into_iter()
            .map(Ok::<_, String>),
    );
    let model = test_model();
    let event_stream = google::stream::process(body, model, None);
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

#[tokio::test]
async fn google_clean_truncation_without_finish_reason_is_an_error() {
    let body = stream::iter(vec![Ok::<_, String>(Bytes::from_static(
        b"data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"partial\"}]}}]}\n\n",
    ))]);
    use futures::StreamExt;
    let events: Vec<_> = google::stream::process(body, test_model(), None)
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

#[test]
fn request_serializes_camelcase_for_google_api() {
    let model = test_model();
    let ctx = Context {
        system_prompt: Some("Be concise.".into()),
        messages: vec![Message::User {
            content: vec![ContentBlock::Text {
                text: "hi".into(),
                text_signature: None,
            }],
        }],
        tools: None,
    };
    let opts = None;
    let req = google::convert::build_request(&model, &ctx, &opts);
    let json = serde_json::to_value(req).unwrap();
    assert_eq!(json["systemInstruction"]["parts"][0]["text"], "Be concise.");
    assert_eq!(json["contents"][0]["parts"][0]["text"], "hi");
}
// Internal Google provider tests.
