#![cfg(test)]

use crate::protocol::{
    AssistantMessageEvent, ContentBlock, Context, StopReason,
};
use futures::StreamExt;

#[test]
fn empty_tool_arguments_parse_as_empty_object() {
    let parsed = crate::protocol::json::parse_terminal_json("");
    assert!(parsed.is_err());
    let parsed =
        crate::providers::common::parse_terminal_tool_arguments("").expect("empty -> {}");
    assert_eq!(parsed, serde_json::json!({}));
    let parsed = crate::providers::common::parse_terminal_tool_arguments("{\"a\":1}")
        .expect("valid args");
    assert_eq!(parsed, serde_json::json!({"a": 1}));
}

#[tokio::test]
async fn reasoning_events_do_not_break_openai_responses_stream() {
    let sse_body = r#"data: {"type":"response.created","response":{"id":"r_1","status":"in_progress"}}

data: {"type":"response.reasoning_summary_text.delta","item_id":"rs_1","output_index":0,"delta":"thinking about it"}

data: {"type":"response.reasoning_summary_text.done","item_id":"rs_1","output_index":0,"text":"thinking about it"}

data: {"type":"response.output_item.added","output_index":0,"item":{"id":"msg_1","type":"message","status":"in_progress","role":"assistant"}}

data: {"type":"response.content_part.added","item_id":"msg_1","output_index":0,"content_index":0,"part":{"type":"output_text","text":""}}

data: {"type":"response.output_text.delta","item_id":"msg_1","output_index":0,"delta":"hello"}

data: {"type":"response.output_item.done","item":{"id":"msg_1","type":"message","status":"completed","role":"assistant","content":[{"type":"output_text","text":"hello"}]}}

data: {"type":"response.completed","response":{"id":"r_1","status":"completed","usage":{"input_tokens":10,"output_tokens":5,"total_tokens":15}}}

"#;
    let model = crate::model::get_model("openai", "gpt-4o").expect("gpt-4o in catalog");
    let body = futures::stream::iter(vec![Ok::<_, String>(bytes::Bytes::from(sse_body))]);
    let stream = crate::providers::openai::responses::stream::process(body, model, None);
    let mut collected = Vec::new();
    let mut stream = Box::pin(stream);
    while let Some(event) = stream.next().await {
        collected.push(event);
    }
    let errors: Vec<String> = collected
        .iter()
        .filter_map(|e| match e {
            AssistantMessageEvent::Error { message, .. } => {
                Some(message.error_message.clone().unwrap_or_default())
            }
            _ => None,
        })
        .collect();
    let terminals = collected
        .iter()
        .filter(|e| matches!(e, AssistantMessageEvent::Done { .. }))
        .count();
    assert_eq!(errors, Vec::<String>::new(), "reasoning events must not error the stream: {errors:?}");
    assert_eq!(terminals, 1, "exactly one Done terminal");
    let text: String = collected
        .iter()
        .filter_map(|e| match e {
            AssistantMessageEvent::TextDelta { delta, .. } => Some(delta.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(text, "hello");
}

#[tokio::test]
async fn gemini_streaming_deltas_merge_into_single_block() {
    let chunks = [
        r#"{"candidates":[{"content":{"parts":[{"text":"Hel"}]},"finishReason":null}]}"#,
        r#"{"candidates":[{"content":{"parts":[{"text":"lo "}]},"finishReason":null}]}"#,
        r#"{"candidates":[{"content":{"parts":[{"text":"world"}]},"finishReason":"STOP"}],"usageMetadata":{"promptTokenCount":7,"candidatesTokenCount":5,"totalTokenCount":12,"cachedContentTokenCount":3}}"#,
    ];
    let sse_body = chunks
        .iter()
        .map(|c| format!("data: {c}\n\n"))
        .collect::<String>();
    let model = crate::model::get_model("google", "gemini-2.5-pro").expect("gemini in catalog");
    let body = futures::stream::iter(vec![Ok::<_, String>(bytes::Bytes::from(sse_body))]);
    let stream = crate::providers::google::stream::process(body, model, None);
    let mut collected = Vec::new();
    let mut stream = Box::pin(stream);
    while let Some(event) = stream.next().await {
        collected.push(event);
    }
    let starts = collected
        .iter()
        .filter(|e| matches!(e, AssistantMessageEvent::TextStart { .. }))
        .count();
    let ends = collected
        .iter()
        .filter(|e| matches!(e, AssistantMessageEvent::TextEnd { .. }))
        .count();
    assert_eq!(starts, 1, "one TextStart for merged block");
    assert_eq!(ends, 1, "one TextEnd for merged block");

    let done = collected
        .iter()
        .find_map(|e| match e {
            AssistantMessageEvent::Done { message, .. } => Some(message.clone()),
            _ => None,
        })
        .expect("Done terminal");
    assert_eq!(done.content.len(), 1);
    match &done.content[0] {
        ContentBlock::Text { text, .. } => assert_eq!(text, "Hello world"),
        other => panic!("expected a single text block, got {other:?}"),
    }
    assert_eq!(done.usage.input, 4, "input excludes 3 cached tokens");
    assert_eq!(done.usage.cache_read, 3);
}

#[tokio::test]
async fn faux_tool_deltas_parse_streaming_json() {
    use crate::providers::faux::{FauxCall, FauxProvider, FauxResponse, FauxToolCall};
    use crate::registry::ApiProvider;
    let provider = FauxProvider::with_call_queue(vec![FauxCall {
        responses: vec![FauxResponse {
            text_deltas: vec![],
            thinking_deltas: vec![],
            tool_calls: vec![FauxToolCall {
                id: "t1".into(),
                name: "f".into(),
                deltas: vec!["{\"a\"".into(), ":1}".into()],
                final_arguments: serde_json::json!({"a": 1}),
            }],
        }],
        stop_reason: StopReason::ToolUse,
    }]);
    let model = crate::model::Model {
        id: "m".into(),
        name: "m".into(),
        api: "faux".into(),
        provider: "faux".into(),
        base_url: "https://example.invalid".into(),
        reasoning: false,
        thinking_level_map: None,
        input: vec![crate::model::ModelInput::Text],
        cost: Default::default(),
        context_window: 100,
        max_tokens: 50,
        headers: None,
        compat: None,
    };
    let ctx = Context {
        system_prompt: None,
        messages: vec![],
        tools: None,
    };
    let stream = provider.stream(&model, ctx, None);
    let mut collected = Vec::new();
    let mut stream = Box::pin(stream);
    while let Some(event) = stream.next().await {
        collected.push(event);
    }
    let delta_args = collected.iter().rev().find_map(|e| match e {
        AssistantMessageEvent::ToolcallDelta { partial, .. } => partial
            .content
            .iter()
            .find_map(|b| match b {
                ContentBlock::ToolCall { arguments, .. } => Some(arguments.clone()),
                _ => None,
            }),
        _ => None,
    });
    assert_eq!(delta_args, Some(serde_json::json!({"a": 1})));
}

#[test]
fn multi_byte_normalize_tool_call_id_does_not_panic() {
    let long = "あ".repeat(22);
    let id = crate::providers::common::normalize_tool_call_id(&long, Some('あ'));
    assert!(id.chars().count() <= 64);
    assert!(id.chars().all(|c| c == 'あ'));

    let ascii = crate::providers::common::normalize_tool_call_id("call_abc123", None);
    assert_eq!(ascii, "call_abc123");

    let empty = crate::providers::common::normalize_tool_call_id("!!!", None);
    assert_eq!(empty, "tool_0");
}

#[test]
fn provider_event_budget_charges_deltas_and_still_limits() {
    use crate::providers::common::{ProviderEventBudget, ProviderEventLimits, ProviderLimit};

    let limits = ProviderEventLimits {
        events: 1000,
        content_blocks: 4,
        content_bytes: 100,
        tool_calls: 2,
        tool_argument_bytes: 50,
    };
    let mut budget = ProviderEventBudget::new(limits);
    let mut partial = crate::protocol::AssistantMessage::empty("t", "m");

    // 5 TextStarts exceed the 4-block cap.
    for _ in 0..4 {
        budget
            .observe(&AssistantMessageEvent::TextStart {
                content_index: 0,
                partial: partial.clone(),
            })
            .expect("within block cap");
    }
    let err = budget
        .observe(&AssistantMessageEvent::TextStart {
            content_index: 0,
            partial: partial.clone(),
        })
        .expect_err("block cap exceeded");
    assert_eq!(err, ProviderLimit::ContentBlocks);

    // 9 x 12-byte TextDeltas exceed the 100-byte content cap.
    let mut budget = ProviderEventBudget::new(limits);
    for _ in 0..8 {
        budget
            .observe(&AssistantMessageEvent::TextDelta {
                content_index: 0,
                delta: "x".repeat(12),
                partial: partial.clone(),
            })
            .expect("within content cap");
    }
    let err = budget
        .observe(&AssistantMessageEvent::TextDelta {
            content_index: 0,
            delta: "x".repeat(12),
            partial: partial.clone(),
        })
        .expect_err("content cap exceeded");
    assert_eq!(err, ProviderLimit::ContentBytes);

    // ToolCall deltas charge argument bytes incrementally; id/name ride the
    // start event (charged once), including a full-args shape like Google's.
    let mut budget = ProviderEventBudget::new(limits);
    partial.content.push(ContentBlock::ToolCall {
        id: "tool-1".into(),
        name: "fn".into(),
        arguments: serde_json::json!({}),
        thought_signature: None,
    });
    budget
        .observe(&AssistantMessageEvent::ToolcallStart {
            content_index: 0,
            partial: partial.clone(),
        })
        .expect("tool start within caps");
    budget
        .observe(&AssistantMessageEvent::ToolcallDelta {
            content_index: 0,
            delta: "y".repeat(26),
            partial: partial.clone(),
        })
        .expect("arguments within cap");
    let err = budget
        .observe(&AssistantMessageEvent::ToolcallDelta {
            content_index: 0,
            delta: "z".repeat(26),
            partial: partial.clone(),
        })
        .expect_err("argument cap exceeded");
    assert_eq!(err, ProviderLimit::ToolArgumentBytes);

    // A second tool call trips the tool-call cap.
    let mut budget = ProviderEventBudget::new(limits);
    budget
        .observe(&AssistantMessageEvent::ToolcallStart {
            content_index: 0,
            partial: partial.clone(),
        })
        .expect("first tool call");
    budget
        .observe(&AssistantMessageEvent::ToolcallStart {
            content_index: 0,
            partial: partial.clone(),
        })
        .expect("second tool call");
    let err = budget
        .observe(&AssistantMessageEvent::ToolcallStart {
            content_index: 0,
            partial: partial.clone(),
        })
        .expect_err("tool call cap exceeded");
    assert_eq!(err, ProviderLimit::ToolCalls);

    // Terminal events never trip the budget.
    let mut budget = ProviderEventBudget::new(limits);
    budget
        .observe(&AssistantMessageEvent::Done {
            reason: StopReason::Stop,
            message: partial.clone(),
        })
        .expect("Done never trips the budget");
}

