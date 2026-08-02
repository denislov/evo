#![cfg(test)]

use crate::protocol::{AssistantMessageEvent, ContentBlock, Context, StopReason, ToolCallKind};
use futures::StreamExt;

#[test]
fn empty_tool_arguments_parse_as_empty_object() {
    let parsed = crate::protocol::json::parse_terminal_json("");
    assert!(parsed.is_err());
    let parsed = crate::providers::common::parse_terminal_tool_arguments("").expect("empty -> {}");
    assert_eq!(parsed, serde_json::json!({}));
    let parsed =
        crate::providers::common::parse_terminal_tool_arguments("{\"a\":1}").expect("valid args");
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
    assert_eq!(
        errors,
        Vec::<String>::new(),
        "reasoning events must not error the stream: {errors:?}"
    );
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
async fn deepseek_reasoning_and_tool_call_are_preserved() {
    let sse_body = r#"data: {"type":"response.created","response":{"id":"r_1","status":"in_progress"}}

data: {"type":"response.output_item.added","output_index":0,"item":{"id":"rs_1","type":"reasoning","status":"in_progress","summary":[],"content":[]}}

data: {"type":"response.reasoning_text.delta","item_id":"rs_1","output_index":0,"content_index":0,"delta":"need "}

data: {"type":"response.reasoning_text.delta","item_id":"rs_1","output_index":0,"content_index":0,"delta":"weather"}

data: {"type":"response.reasoning_text.done","item_id":"rs_1","output_index":0,"content_index":0,"text":"need weather"}

data: {"type":"response.output_item.done","output_index":0,"item":{"id":"rs_1","type":"reasoning","status":"completed","summary":[],"content":[{"type":"reasoning_text","text":"need weather"}]}}

data: {"type":"response.output_item.added","output_index":1,"item":{"id":"fc_1","type":"function_call","status":"in_progress","call_id":"call_1","name":"weather","arguments":""}}

data: {"type":"response.function_call_arguments.delta","item_id":"fc_1","output_index":1,"delta":"{\"city\":\"杭州\"}"}

data: {"type":"response.output_item.done","output_index":1,"item":{"id":"fc_1","type":"function_call","status":"completed","call_id":"call_1","name":"weather","arguments":"{\"city\":\"杭州\"}"}}

data: {"type":"response.completed","response":{"id":"r_1","status":"completed","usage":{"input_tokens":20,"output_tokens":8,"total_tokens":28,"input_tokens_details":{"cached_tokens":5}}}}

"#;
    let model = crate::model::get_model("deepseek", "deepseek-v4-flash")
        .expect("DeepSeek V4 Flash is in the catalog");
    let body = futures::stream::iter(vec![Ok::<_, String>(bytes::Bytes::from(sse_body))]);
    let mut stream = crate::providers::openai::responses::stream::process_with_api_name(
        body,
        model,
        None,
        "deepseek-responses",
    );
    let mut events = Vec::new();
    while let Some(event) = stream.next().await {
        events.push(event);
    }

    let thinking: String = events
        .iter()
        .filter_map(|event| match event {
            AssistantMessageEvent::ThinkingDelta { delta, .. } => Some(delta.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(thinking, "need weather");
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, AssistantMessageEvent::ThinkingStart { .. }))
            .count(),
        1
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, AssistantMessageEvent::ThinkingEnd { .. }))
            .count(),
        1
    );

    let done = events
        .iter()
        .find_map(|event| match event {
            AssistantMessageEvent::Done { message, .. } => Some(message),
            _ => None,
        })
        .expect("stream completes successfully");
    assert_eq!(done.api, "deepseek-responses");
    assert_eq!(done.stop_reason, StopReason::ToolUse);
    assert_eq!(done.usage.input, 15);
    assert_eq!(done.usage.cache_read, 5);
    assert!(matches!(
        &done.content[0],
        ContentBlock::Thinking {
            thinking,
            provider_metadata: Some(metadata),
            ..
        } if thinking == "need weather"
            && metadata.api == "deepseek-responses"
            && metadata.item_id.as_deref() == Some("rs_1")
    ));
    assert!(matches!(
        &done.content[1],
        ContentBlock::ToolCall {
            id,
            name,
            arguments,
            ..
        } if id == "call_1" && name == "weather" && arguments == &serde_json::json!({"city": "杭州"})
    ));
}

#[tokio::test]
async fn responses_max_output_tokens_incomplete_is_length() {
    let sse_body = r#"data: {"type":"response.created","response":{"id":"r_2","status":"in_progress"}}

data: {"type":"response.incomplete","response":{"id":"r_2","status":"incomplete","incomplete_details":{"reason":"max_output_tokens"},"usage":{"input_tokens":3,"output_tokens":2,"total_tokens":5}}}

"#;
    let model = crate::model::get_model("deepseek", "deepseek-v4-flash")
        .expect("DeepSeek V4 Flash is in the catalog");
    let body = futures::stream::iter(vec![Ok::<_, String>(bytes::Bytes::from(sse_body))]);
    let mut stream = crate::providers::openai::responses::stream::process_with_api_name(
        body,
        model,
        None,
        "deepseek-responses",
    );
    let mut events = Vec::new();
    while let Some(event) = stream.next().await {
        events.push(event);
    }

    assert!(
        !events
            .iter()
            .any(|event| matches!(event, AssistantMessageEvent::Error { .. }))
    );
    let done = events
        .iter()
        .find_map(|event| match event {
            AssistantMessageEvent::Done { reason, message } => Some((reason, message)),
            _ => None,
        })
        .expect("incomplete max tokens is a successful length terminal");
    assert_eq!(done.0, &StopReason::Length);
    assert_eq!(done.1.stop_reason, StopReason::Length);
    assert_eq!(done.1.usage.total_tokens, 5);
}

#[test]
fn deepseek_catalog_routes_only_flash_to_responses() {
    let flash = crate::model::get_model("deepseek", "deepseek-v4-flash")
        .expect("DeepSeek V4 Flash is in the catalog");
    let pro = crate::model::get_model("deepseek", "deepseek-v4-pro")
        .expect("DeepSeek V4 Pro is in the catalog");
    assert_eq!(flash.api, "deepseek-responses");
    assert_eq!(pro.api, "openai-completions");
}

#[test]
fn repository_model_overrides_match_bundled_catalog() {
    let overrides: serde_json::Value =
        serde_json::from_str(include_str!("../tools/model_overrides.json"))
            .expect("model overrides are valid JSON");
    for override_value in overrides.as_array().expect("overrides are an array") {
        let provider = override_value["provider"]
            .as_str()
            .expect("override provider is a string");
        let id = override_value["id"]
            .as_str()
            .expect("override id is a string");
        let model = crate::model::get_model(provider, id)
            .unwrap_or_else(|| panic!("override target {provider}/{id} exists"));
        let model_value = serde_json::to_value(model).expect("model serializes");
        for (field, expected) in override_value["set"]
            .as_object()
            .expect("override set is an object")
        {
            assert_eq!(
                model_value.get(field),
                Some(expected),
                "override field {provider}/{id}.{field} drifted"
            );
        }
        for field in override_value["remove"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(serde_json::Value::as_str)
        {
            assert!(
                model_value.get(field).is_none(),
                "removed override field {provider}/{id}.{field} returned"
            );
        }
    }
}

#[test]
fn openai_responses_replays_structured_reasoning_metadata() {
    let model = crate::model::get_model("openai", "gpt-4o").expect("gpt-4o in catalog");
    let context = Context {
        system_prompt: None,
        messages: vec![crate::protocol::Message::Assistant {
            content: vec![ContentBlock::Thinking {
                thinking: "reasoning text".into(),
                thinking_signature: None,
                provider_metadata: Some(crate::protocol::ProviderMetadata {
                    api: "openai-responses".into(),
                    item_id: Some("reasoning-item-1".into()),
                    encrypted_content: Some("opaque-ciphertext".into()),
                }),
                redacted: None,
            }],
        }],
        tools: None,
    };
    let request =
        crate::providers::openai::responses::convert::build_request(&model, &context, &None);
    let value = serde_json::to_value(request).expect("request serializes");
    assert_eq!(value["input"][0]["type"], "reasoning");
    assert_eq!(value["input"][0]["id"], "reasoning-item-1");
    assert_eq!(value["input"][0]["encrypted_content"], "opaque-ciphertext");
}

#[tokio::test]
async fn responses_reasoning_keeps_encrypted_content_when_done_omits_it() {
    let sse_body = r#"data: {"type":"response.created","response":{"id":"r_encrypted","status":"in_progress","model":"gpt-4o"}}

data: {"type":"response.output_item.added","item":{"type":"reasoning","id":"reasoning_encrypted","encrypted_content":"opaque-ciphertext"}}

data: {"type":"response.output_item.done","item":{"type":"reasoning","id":"reasoning_encrypted"}}

data: {"type":"response.completed","response":{"id":"r_encrypted","status":"completed","model":"gpt-4o","usage":{"input_tokens":1,"output_tokens":1,"total_tokens":2}}}

"#;
    let model = crate::model::get_model("openai", "gpt-4o").expect("gpt-4o in catalog");
    let body = futures::stream::iter(vec![Ok::<_, String>(bytes::Bytes::from(sse_body))]);
    let message = crate::protocol::stream::complete(
        crate::providers::responses::stream::process_with_api_name(
            body,
            model,
            None,
            "openai-responses",
        ),
    )
    .await
    .expect("Responses stream completes");
    assert!(matches!(
        &message.content[0],
        ContentBlock::Thinking {
            provider_metadata: Some(metadata),
            ..
        } if metadata.encrypted_content.as_deref() == Some("opaque-ciphertext")
    ));
}

#[test]
fn legacy_thinking_blocks_deserialize_without_provider_metadata() {
    let block: ContentBlock = serde_json::from_value(serde_json::json!({
        "type": "thinking",
        "thinking": "legacy",
        "thinking_signature": "anthropic-signature"
    }))
    .expect("legacy thinking block remains readable");
    assert!(matches!(
        block,
        ContentBlock::Thinking {
            thinking_signature: Some(signature),
            provider_metadata: None,
            ..
        } if signature == "anthropic-signature"
    ));
}

#[tokio::test]
async fn unknown_incomplete_reason_remains_a_provider_error() {
    let sse_body = r#"data: {"type":"response.created","response":{"id":"r_unknown","status":"in_progress"}}

data: {"type":"response.incomplete","response":{"id":"r_unknown","status":"incomplete","incomplete_details":{"reason":"future_policy_reason"}}}

"#;
    let model = crate::model::get_model("deepseek", "deepseek-v4-flash")
        .expect("DeepSeek V4 Flash is in the catalog");
    let body = futures::stream::iter(vec![Ok::<_, String>(bytes::Bytes::from(sse_body))]);
    let mut stream = crate::providers::responses::stream::process_with_api_name(
        body,
        model,
        None,
        "deepseek-responses",
    );
    let mut terminal = None;
    while let Some(event) = stream.next().await {
        if matches!(
            event,
            AssistantMessageEvent::Done { .. } | AssistantMessageEvent::Error { .. }
        ) {
            terminal = Some(event);
        }
    }
    assert!(matches!(
        terminal,
        Some(AssistantMessageEvent::Error { .. })
    ));
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
        AssistantMessageEvent::ToolcallDelta { partial, .. } => {
            partial.content.iter().find_map(|b| match b {
                ContentBlock::ToolCall { arguments, .. } => Some(arguments.clone()),
                _ => None,
            })
        }
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
        kind: ToolCallKind::Function,
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

async fn collect_deepseek_fixture(sse_body: &'static str) -> Vec<AssistantMessageEvent> {
    let model = crate::model::get_model("deepseek", "deepseek-v4-flash")
        .expect("DeepSeek V4 Flash is in the catalog");
    let body = futures::stream::iter(vec![Ok::<_, String>(bytes::Bytes::from_static(
        sse_body.as_bytes(),
    ))]);
    let mut stream = crate::providers::responses::stream::process_with_api_name(
        body,
        model,
        None,
        "deepseek-responses",
    );
    let mut events = Vec::new();
    while let Some(event) = stream.next().await {
        events.push(event);
    }
    events
}

#[tokio::test]
async fn recorded_deepseek_reasoning_function_fixture_preserves_metadata_and_usage() {
    let events = collect_deepseek_fixture(include_str!(
        "providers/deepseek/fixtures/reasoning_function.sse"
    ))
    .await;
    let done = events
        .iter()
        .find_map(|event| match event {
            AssistantMessageEvent::Done { message, .. } => Some(message),
            _ => None,
        })
        .expect("fixture has a successful terminal");
    assert_eq!(done.response_model.as_deref(), Some("deepseek-v4-flash"));
    assert_eq!(done.usage.input, 258);
    assert_eq!(done.usage.cache_read, 32);
    assert_eq!(done.usage.reasoning_tokens, 20);
    assert!(matches!(
        &done.content[0],
        ContentBlock::Thinking {
            provider_metadata: Some(metadata),
            ..
        } if metadata.api == "deepseek-responses"
            && metadata.item_id.as_deref() == Some("reasoning_1")
    ));
}

#[tokio::test]
async fn recorded_deepseek_custom_tool_fixture_preserves_raw_input() {
    let events =
        collect_deepseek_fixture(include_str!("providers/deepseek/fixtures/custom_tool.sse")).await;
    let done = events
        .iter()
        .find_map(|event| match event {
            AssistantMessageEvent::Done { message, .. } => Some(message),
            _ => None,
        })
        .expect("fixture has a successful terminal");
    assert_eq!(done.stop_reason, StopReason::ToolUse);
    assert!(matches!(
        &done.content[0],
        ContentBlock::ToolCall {
            id,
            name,
            arguments: serde_json::Value::String(input),
            kind: ToolCallKind::Custom,
            ..
        } if id == "call_custom_1"
            && name == "apply_patch"
            && input == "*** Begin Patch\n*** End Patch\n"
    ));
}

#[tokio::test]
async fn recorded_deepseek_web_search_fixture_is_replayable() {
    let events =
        collect_deepseek_fixture(include_str!("providers/deepseek/fixtures/web_search.sse")).await;
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, AssistantMessageEvent::ProviderItemStart { .. }))
            .count(),
        1
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, AssistantMessageEvent::ProviderItemDelta { .. }))
            .count(),
        3
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, AssistantMessageEvent::ProviderItemEnd { .. }))
            .count(),
        1
    );
    let done = events
        .iter()
        .find_map(|event| match event {
            AssistantMessageEvent::Done { message, .. } => Some(message),
            _ => None,
        })
        .expect("fixture has a successful terminal");
    assert_eq!(done.stop_reason, StopReason::Stop);
    assert!(matches!(
        &done.content[0],
        ContentBlock::ProviderItem { api, item }
            if api == "deepseek-responses"
                && item.get("type").and_then(serde_json::Value::as_str)
                    == Some("web_search_call")
                && item.get("status").and_then(serde_json::Value::as_str)
                    == Some("completed")
    ));
    assert!(matches!(
        &done.content[1],
        ContentBlock::Text { text, .. } if text == "api-docs.deepseek.com"
    ));
}

#[test]
fn web_search_support_tracks_apis_that_can_declare_and_replay_it() {
    use crate::providers::model_supports_web_search;

    let deepseek_flash =
        crate::model::get_model("deepseek", "deepseek-v4-flash").expect("catalog model exists");
    assert_eq!(deepseek_flash.api, "deepseek-responses");
    assert!(model_supports_web_search(&deepseek_flash));

    // Same provider, different API family: declaration support follows the
    // API, not the vendor.
    let deepseek_pro =
        crate::model::get_model("deepseek", "deepseek-v4-pro").expect("catalog model exists");
    assert_eq!(deepseek_pro.api, "openai-completions");
    assert!(!model_supports_web_search(&deepseek_pro));

    let gemini = crate::model::get_model("google", "gemini-2.5-pro").expect("gemini in catalog");
    assert!(!model_supports_web_search(&gemini));
}

#[tokio::test]
async fn web_search_declared_to_an_unsupporting_api_fails_loudly() {
    let model = crate::model::get_model("google", "gemini-2.5-pro").expect("gemini in catalog");
    let context = Context {
        system_prompt: None,
        messages: vec![crate::protocol::Message::User {
            content: vec![ContentBlock::Text {
                text: "search the web".into(),
                text_signature: None,
            }],
        }],
        tools: Some(vec![crate::protocol::Tool::web_search()]),
    };

    // The tool must not be silently dropped from the outgoing request: a
    // caller that declared web_search and got a plain answer back has no way
    // to tell the search never happened.
    let error = crate::providers::google::convert::build_request(&model, &context, &None)
        .expect_err("google cannot express web_search");
    assert!(error.contains("web_search"), "unexpected error: {error}");
    assert!(
        error.contains("google-generative-ai"),
        "error should name the API: {error}"
    );
}

#[test]
fn function_tools_still_convert_on_every_provider() {
    let context = Context {
        system_prompt: None,
        messages: Vec::new(),
        tools: Some(vec![crate::protocol::Tool::function(
            "read",
            Some("Read a file".into()),
            serde_json::json!({"type": "object", "properties": {}}),
        )]),
    };

    let gemini = crate::model::get_model("google", "gemini-2.5-pro").expect("gemini in catalog");
    assert!(crate::providers::google::convert::build_request(&gemini, &context, &None).is_ok());

    let gpt = crate::model::get_model("openai", "gpt-4o").expect("gpt-4o in catalog");
    assert!(
        crate::providers::openai::completions::convert::build_request(&gpt, &context, &None)
            .is_ok()
    );
}
