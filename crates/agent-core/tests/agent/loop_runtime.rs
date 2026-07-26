use crate::common;
use agent_core::api::agent::{
    Agent, AgentAdmissionError, AgentConfig, AgentConfigError, AgentEvent, AgentMessage,
    CompactionConfig, CompactionSettings, MAX_COMPACTION_TOKEN_BUDGET,
};
use agent_core::api::tool::{AgentTool, AgentToolOutput, ToolExecutionMode};
use ai::api::conversation::{AssistantMessage, ContentBlock, Context, Message, StopReason};
use ai::api::model::{Model, ModelCost, ModelInput};
use ai::api::stream::AssistantMessageEvent;
use common::{ProviderGuard, ScriptedTurn, TestProvider, text_turn, tool_use_turn};
use futures::StreamExt;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};

fn test_model(api_key: &str) -> Model {
    Model {
        id: "test-model".into(),
        name: "Test Model".into(),
        api: api_key.into(),
        provider: "test".into(),
        base_url: "".into(),
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
        context_window: 0,
        max_tokens: 0,
        headers: None,
        compat: None,
    }
}

fn test_config(api_key: &str, provider: Option<&ProviderGuard>) -> AgentConfig {
    let base = provider
        .map(|provider| provider.agent_config(test_model(api_key)))
        .unwrap_or_else(|| common::agent_config(test_model(api_key)));
    AgentConfig {
        model: test_model(api_key),
        system_prompt: Some("Be helpful.".into()),
        max_turns: Some(5),
        stream_options: None,
        ..base
    }
}

fn done_text_message(response_id: &str, text: &str) -> AssistantMessage {
    let mut message = AssistantMessage::empty("test", "test-model");
    message.response_id = Some(response_id.into());
    message.content.push(ContentBlock::Text {
        text: text.into(),
        text_signature: None,
    });
    message.stop_reason = StopReason::Stop;
    message
}

fn tool_use_message(response_id: &str, tool_id: &str, tool_name: &str) -> AssistantMessage {
    let mut message = AssistantMessage::empty("test", "test-model");
    message.response_id = Some(response_id.into());
    message.content.push(ContentBlock::ToolCall {
        id: tool_id.into(),
        name: tool_name.into(),
        arguments: serde_json::json!({}),
        thought_signature: None,
    });
    message.stop_reason = StopReason::ToolUse;
    message
}

fn string_encoded_tool_use_turn(tool_id: &str, tool_name: &str, arguments: &str) -> ScriptedTurn {
    let mut partial = AssistantMessage::empty("test", "test-model");
    partial.content.push(ContentBlock::ToolCall {
        id: tool_id.into(),
        name: tool_name.into(),
        arguments: serde_json::Value::String(arguments.into()),
        thought_signature: None,
    });
    ScriptedTurn {
        events: vec![AssistantMessageEvent::ToolcallEnd {
            content_index: 0,
            partial,
        }],
        stop_reason: StopReason::ToolUse,
        response_id: format!("resp_{tool_id}"),
        model_name: "test-model".into(),
    }
}

fn two_tool_use_turn_with_ids(first_id: &str, second_id: &str) -> ScriptedTurn {
    let mut partial = AssistantMessage::empty("test", "test-model");
    partial.content = vec![
        ContentBlock::ToolCall {
            id: first_id.into(),
            name: "echo".into(),
            arguments: serde_json::json!({}),
            thought_signature: None,
        },
        ContentBlock::ToolCall {
            id: second_id.into(),
            name: "echo".into(),
            arguments: serde_json::json!({}),
            thought_signature: None,
        },
    ];
    ScriptedTurn {
        events: vec![AssistantMessageEvent::ToolcallEnd {
            content_index: 1,
            partial,
        }],
        stop_reason: StopReason::ToolUse,
        response_id: "resp_two_tools".into(),
        model_name: "test-model".into(),
    }
}

fn context_contains_user_text(context: &Context, expected: &str) -> bool {
    context.messages.iter().any(|message| {
        matches!(
            message,
            Message::User { content }
                if content.iter().any(|block| {
                    matches!(block, ContentBlock::Text { text, .. } if text == expected)
                })
        )
    })
}

#[tokio::test]
async fn single_turn_text_response() {
    let api_key = "test-api-1";
    let provider = Arc::new(TestProvider::new(vec![text_turn("Hello, world!")]));
    let _provider_guard = ProviderGuard::register(api_key, provider);

    let agent = Agent::new(test_config(api_key, Some(&_provider_guard)));

    let stream = agent.prompt("hi");
    let events: Vec<_> = stream.collect().await;

    let has_done = events
        .iter()
        .any(|e| matches!(e, AgentEvent::AgentDone { .. }));
    assert!(has_done, "should have AgentDone event");

    let has_text = events.iter().any(|e| {
        matches!(
            e,
            AgentEvent::LlmEvent(AssistantMessageEvent::TextDelta { .. })
        )
    });
    assert!(has_text, "should have text delta event");

    let msgs = agent.messages();
    assert_eq!(msgs.len(), 2); // UserText + Assistant
    assert!(matches!(&msgs[0], AgentMessage::UserText { .. }));
    assert!(matches!(&msgs[1], AgentMessage::Assistant { .. }));
}

#[tokio::test]
async fn llm_events_stream_before_provider_done() {
    let (release_tx, release_rx) = oneshot::channel::<()>();
    let release_rx = Arc::new(Mutex::new(Some(release_rx)));
    let mut config = test_config("live-stream-provider", None);
    config.provider_streamer = Some(Arc::new(move |_model, _context, _opts| {
        let release_rx = release_rx.clone();
        Box::pin(async_stream::stream! {
            let mut partial = AssistantMessage::empty("test", "test-model");
            partial.content.push(ContentBlock::Text {
                text: "partial".into(),
                text_signature: None,
            });
            yield AssistantMessageEvent::TextDelta {
                content_index: 0,
                delta: "partial".into(),
                partial: partial.clone(),
            };
            let release_rx = {
                release_rx
                    .lock()
                    .unwrap()
                    .take()
                    .expect("release receiver should be available")
            };
            let _ = release_rx.await;
            yield AssistantMessageEvent::Done {
                reason: StopReason::Stop,
                message: done_text_message("resp_live_stream", "done"),
            };
        })
    }));

    let agent = Agent::new(config);
    let mut stream = agent.prompt("hi");

    tokio::time::timeout(Duration::from_millis(200), async {
        while let Some(event) = stream.next().await {
            if matches!(
                event,
                AgentEvent::LlmEvent(AssistantMessageEvent::TextDelta { delta, .. })
                    if delta == "partial"
            ) {
                return;
            }
        }
        panic!("stream ended before partial LLM event");
    })
    .await
    .expect("partial LLM event should arrive before provider completes");

    release_tx.send(()).unwrap();
    while stream.next().await.is_some() {}
}

#[tokio::test]
async fn follow_up_queued_during_provider_turn_is_not_lost_and_continues() {
    let calls = Arc::new(AtomicUsize::new(0));
    let calls_for_streamer = calls.clone();
    let (started_tx, mut started_rx) = mpsc::unbounded_channel::<()>();
    let (release_tx, release_rx) = oneshot::channel::<()>();
    let release_rx = Arc::new(Mutex::new(Some(release_rx)));
    let mut config = test_config("live-follow-up-provider", None);
    config.provider_streamer = Some(Arc::new(move |_model, context, _opts| {
        let call = calls_for_streamer.fetch_add(1, Ordering::SeqCst) + 1;
        let started_tx = started_tx.clone();
        let release_rx = release_rx.clone();
        Box::pin(async_stream::stream! {
            if call == 1 {
                let _ = started_tx.send(());
                let release_rx = {
                    release_rx
                        .lock()
                        .unwrap()
                        .take()
                        .expect("release receiver should be available")
                };
                let _ = release_rx.await;
                yield AssistantMessageEvent::Done {
                    reason: StopReason::Stop,
                    message: done_text_message("resp_first", "first"),
                };
            } else {
                assert!(
                    context_contains_user_text(&context, "queued during provider"),
                    "follow-up queued while provider awaited should reach the next provider call"
                );
                yield AssistantMessageEvent::Done {
                    reason: StopReason::Stop,
                    message: done_text_message("resp_second", "second"),
                };
            }
        })
    }));

    let agent = Agent::new(config);
    let collect_task = {
        let stream = agent.prompt("first");
        tokio::spawn(async move { stream.collect::<Vec<_>>().await })
    };

    started_rx
        .recv()
        .await
        .expect("first provider call should start");
    agent.follow_up("queued during provider").unwrap();
    release_tx.send(()).unwrap();

    let events = collect_task.await.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert!(events.iter().any(|event| {
        matches!(
            event,
            AgentEvent::AgentDone { message }
                if message.content.iter().any(|block| {
                    matches!(block, ContentBlock::Text { text, .. } if text == "second")
                })
        )
    }));
    assert!(agent.messages().iter().any(|message| {
        matches!(message, AgentMessage::UserText { text, .. } if text == "queued during provider")
    }));
}

#[tokio::test]
async fn steer_queued_during_tool_turn_is_not_lost_before_next_provider_call() {
    let calls = Arc::new(AtomicUsize::new(0));
    let calls_for_streamer = calls.clone();
    let saw_steer = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let saw_steer_for_streamer = saw_steer.clone();
    let mut config = test_config("live-steer-tool", None);
    config.provider_streamer = Some(Arc::new(move |_model, context, _opts| {
        let call = calls_for_streamer.fetch_add(1, Ordering::SeqCst) + 1;
        let saw_steer_for_streamer = saw_steer_for_streamer.clone();
        Box::pin(async_stream::stream! {
            if call == 1 {
                yield AssistantMessageEvent::Done {
                    reason: StopReason::ToolUse,
                    message: tool_use_message("resp_tool", "tool_1", "blocking"),
                };
            } else {
                if context_contains_user_text(&context, "steered during tool") {
                    saw_steer_for_streamer.store(true, Ordering::SeqCst);
                }
                yield AssistantMessageEvent::Done {
                    reason: StopReason::Stop,
                    message: done_text_message("resp_done", "done"),
                };
            }
        })
    }));

    let agent = Agent::new(config);
    let (tool_started_tx, mut tool_started_rx) = mpsc::unbounded_channel::<()>();
    let (tool_release_tx, tool_release_rx) = oneshot::channel::<()>();
    let tool_release_rx = Arc::new(Mutex::new(Some(tool_release_rx)));
    agent
        .add_tool(AgentTool {
            name: "blocking".into(),
            description: "blocks until released".into(),
            parameters: serde_json::json!({"type": "object"}),
            execution_mode: None,
            execute: Arc::new(move |_, _, _on_update| {
                let tool_started_tx = tool_started_tx.clone();
                let tool_release_rx = tool_release_rx.clone();
                Box::pin(async move {
                    let _ = tool_started_tx.send(());
                    let release_rx = {
                        tool_release_rx
                            .lock()
                            .unwrap()
                            .take()
                            .expect("tool release receiver should be available")
                    };
                    let _ = release_rx.await;
                    Ok(AgentToolOutput::new(vec![ContentBlock::Text {
                        text: "tool done".into(),
                        text_signature: None,
                    }]))
                })
            }),
        })
        .unwrap();

    let collect_task = {
        let stream = agent.prompt("use tool");
        tokio::spawn(async move { stream.collect::<Vec<_>>().await })
    };

    tool_started_rx.recv().await.expect("tool should start");
    agent.steer("steered during tool").unwrap();
    tool_release_tx.send(()).unwrap();
    let _events = collect_task.await.unwrap();

    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert!(
        saw_steer.load(Ordering::SeqCst),
        "steer queued while tool awaited should reach the next provider call"
    );
}

#[tokio::test]
async fn provider_override_set_during_inflight_turn_survives_current_writeback() {
    let calls = Arc::new(AtomicUsize::new(0));
    let calls_for_streamer = calls.clone();
    let (started_tx, mut started_rx) = mpsc::unbounded_channel::<()>();
    let (release_tx, release_rx) = oneshot::channel::<()>();
    let release_rx = Arc::new(Mutex::new(Some(release_rx)));
    let observed_system_prompts = Arc::new(Mutex::new(Vec::new()));
    let observed_for_streamer = observed_system_prompts.clone();
    let mut config = test_config("override-race-provider", None);
    config.system_prompt = None;
    config.provider_streamer = Some(Arc::new(move |_model, context, _opts| {
        let call = calls_for_streamer.fetch_add(1, Ordering::SeqCst) + 1;
        observed_for_streamer
            .lock()
            .unwrap()
            .push(context.system_prompt.clone());
        let started_tx = started_tx.clone();
        let release_rx = release_rx.clone();
        Box::pin(async_stream::stream! {
            if call == 1 {
                let _ = started_tx.send(());
                let release_rx = {
                    release_rx
                        .lock()
                        .unwrap()
                        .take()
                        .expect("release receiver should be available")
                };
                let _ = release_rx.await;
            }
            yield AssistantMessageEvent::Done {
                reason: StopReason::Stop,
                message: done_text_message(&format!("resp_{call}"), "done"),
            };
        })
    }));

    let agent = Agent::new(config);
    agent.add_message(AgentMessage::UserText {
        message_id: "user_0".into(),
        text: "first".into(),
    });
    agent.set_provider_request_override(
        Context {
            system_prompt: Some("initial override".into()),
            messages: vec![],
            tools: None,
        },
        None,
    );

    let collect_task = {
        let stream = agent.run().expect("agent should run");
        tokio::spawn(async move { stream.collect::<Vec<_>>().await })
    };
    started_rx
        .recv()
        .await
        .expect("first provider call should start");
    agent.set_provider_request_override(
        Context {
            system_prompt: Some("new override".into()),
            messages: vec![],
            tools: None,
        },
        None,
    );
    release_tx.send(()).unwrap();
    let _ = collect_task.await.unwrap();

    let second_events: Vec<_> = agent.prompt("second").collect().await;
    assert!(
        second_events
            .iter()
            .any(|event| matches!(event, AgentEvent::AgentDone { .. }))
    );

    let observed = observed_system_prompts.lock().unwrap().clone();
    assert_eq!(observed.first(), Some(&Some("initial override".into())));
    assert_eq!(observed.get(1), Some(&Some("new override".into())));
}

#[tokio::test]
async fn tool_use_turn_executes_tool() {
    let api_key = "test-api-2";
    let provider = Arc::new(TestProvider::new(vec![
        tool_use_turn("tool_1", "echo", serde_json::json!({"text": "hi"})),
        text_turn("Tool executed successfully."),
    ]));
    let _provider_guard = ProviderGuard::register(api_key, provider);

    let observed_context = Arc::new(Mutex::new(None));
    let observed_context_for_tool = observed_context.clone();
    let mut config = test_config(api_key, Some(&_provider_guard));
    config.tool_execution_scope = Some("op_tool_scope".into());
    let agent = Agent::new(config);

    let tool = AgentTool {
        name: "echo".into(),
        description: "echoes input".into(),
        parameters: serde_json::json!({"type": "object", "properties": {"text": {"type": "string"}}}),
        execution_mode: None,
        execute: Arc::new(move |context, args, _on_update| {
            *observed_context_for_tool.lock().unwrap() = Some((
                context.scope_id().map(str::to_owned),
                context.turn(),
                context.tool_call_id().to_owned(),
                context.tool_name().to_owned(),
                context.cancel_token().is_cancelled(),
            ));
            let text = args
                .get("text")
                .and_then(|v| v.as_str())
                .unwrap_or("no text");
            let result = vec![ContentBlock::Text {
                text: format!("echo: {}", text),
                text_signature: None,
            }];
            Box::pin(async move { Ok(AgentToolOutput::new(result)) })
        }),
    };
    agent.add_tool(tool).unwrap();

    let stream = agent.prompt("echo hi");
    let events: Vec<_> = stream.collect().await;

    let has_tool_start = events.iter().any(|event| {
        matches!(
            event,
            AgentEvent::ToolCallStart {
                tool_call_id,
                tool_name,
                ..
            } if tool_call_id == "tool_1" && tool_name == "echo"
        )
    });
    let has_tool_end = events.iter().any(|event| {
        matches!(
            event,
            AgentEvent::ToolCallEnd {
                tool_call_id,
                tool_name,
                ..
            } if tool_call_id == "tool_1" && tool_name == "echo"
        )
    });
    assert!(has_tool_start, "should have ToolCallStart");
    assert!(has_tool_end, "should have ToolCallEnd");

    let has_done = events
        .iter()
        .any(|e| matches!(e, AgentEvent::AgentDone { .. }));
    assert!(has_done, "should have AgentDone");

    let msgs = agent.messages();
    assert_eq!(msgs.len(), 4); // UserText, Assistant(tool_use), ToolResult, Assistant(text)
    assert!(matches!(
        &msgs[1],
        AgentMessage::Assistant { message, .. }
            if matches!(
                message.content.first(),
                Some(ContentBlock::ToolCall { id, name, .. })
                    if id == "tool_1" && name == "echo"
            )
    ));
    assert!(matches!(
        &msgs[2],
        AgentMessage::ToolResult {
            message_id,
            tool_call_id,
            tool_name,
            ..
        } if message_id == "tool_1"
            && tool_call_id == "tool_1"
            && tool_name == "echo"
    ));
    let continuation_request = events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::BeforeProviderRequest { request } => Some(request),
            _ => None,
        })
        .nth(1)
        .expect("tool result should trigger one continuation request");
    assert!(continuation_request.context.messages.iter().any(|message| {
        matches!(
            message,
            Message::Assistant { content }
                if matches!(
                    content.first(),
                    Some(ContentBlock::ToolCall { id, name, .. })
                        if id == "tool_1" && name == "echo"
                )
        )
    }));
    assert!(continuation_request.context.messages.iter().any(|message| {
        matches!(
            message,
            Message::ToolResult {
                tool_call_id,
                tool_name: Some(tool_name),
                ..
            } if tool_call_id == "tool_1" && tool_name == "echo"
        )
    }));
    assert_eq!(
        observed_context.lock().unwrap().as_ref(),
        Some(&(
            Some("op_tool_scope".to_owned()),
            1,
            "tool_1".to_owned(),
            "echo".to_owned(),
            false,
        ))
    );
}

#[tokio::test]
async fn terminal_string_arguments_are_strictly_parsed_before_hooks_and_execution() {
    let api_key = "test-api-terminal-string-arguments";
    let provider = Arc::new(TestProvider::new(vec![
        string_encoded_tool_use_turn("tool_string", "echo", r#"{"text":"hi"}"#),
        text_turn("Tool executed successfully."),
    ]));
    let _provider_guard = ProviderGuard::register(api_key, provider);

    let observed_arguments = Arc::new(Mutex::new(None));
    let observed_arguments_for_tool = observed_arguments.clone();
    let agent = Agent::new(test_config(api_key, Some(&_provider_guard)));
    agent
        .add_tool(AgentTool {
            name: "echo".into(),
            description: "echoes input".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {"text": {"type": "string"}},
                "required": ["text"],
                "additionalProperties": false
            }),
            execution_mode: None,
            execute: Arc::new(move |_, arguments, _| {
                *observed_arguments_for_tool.lock().unwrap() = Some(arguments);
                Box::pin(async {
                    Ok(AgentToolOutput::new(vec![ContentBlock::Text {
                        text: "ok".into(),
                        text_signature: None,
                    }]))
                })
            }),
        })
        .unwrap();

    let events = agent.prompt("echo hi").collect::<Vec<_>>().await;

    assert_eq!(
        observed_arguments.lock().unwrap().as_ref(),
        Some(&serde_json::json!({"text": "hi"}))
    );
    assert!(events.iter().any(|event| matches!(
        event,
        AgentEvent::ToolCallStart { arguments, .. }
            if arguments == &serde_json::json!({"text": "hi"})
    )));
    assert!(matches!(
        agent.messages().get(1),
        Some(AgentMessage::Assistant { message, .. })
            if matches!(
                message.content.first(),
                Some(ContentBlock::ToolCall { arguments, .. })
                    if arguments == &serde_json::json!({"text": "hi"})
            )
    ));
}

#[tokio::test]
async fn malformed_terminal_string_arguments_fail_before_history_hooks_or_execution() {
    for (case, arguments) in [
        ("truncated", r#"{"text":"unterminated}"#),
        ("trailing", r#"{"text":"hi"} trailing"#),
        ("duplicate", r#"{"text":"first","text":"second"}"#),
    ] {
        let api_key = format!("test-api-malformed-terminal-arguments-{case}");
        let provider = Arc::new(TestProvider::new(vec![string_encoded_tool_use_turn(
            "tool_malformed",
            "echo",
            arguments,
        )]));
        let provider_guard = ProviderGuard::register(api_key.clone(), provider);

        let executions = Arc::new(AtomicUsize::new(0));
        let executions_for_tool = executions.clone();
        let hook_calls = Arc::new(AtomicUsize::new(0));
        let hook_calls_for_hook = hook_calls.clone();
        let mut config = test_config(&api_key, Some(&provider_guard));
        config.hooks.before_tool_call = Some(Arc::new(move |_| {
            hook_calls_for_hook.fetch_add(1, Ordering::SeqCst);
            Box::pin(async { Ok(None) })
        }));
        let agent = Agent::new(config);
        agent
            .add_tool(AgentTool {
                name: "echo".into(),
                description: "echoes input".into(),
                parameters: serde_json::json!({"type": "object"}),
                execution_mode: None,
                execute: Arc::new(move |_, _, _| {
                    executions_for_tool.fetch_add(1, Ordering::SeqCst);
                    Box::pin(async { Ok(AgentToolOutput::new(Vec::new())) })
                }),
            })
            .unwrap();

        let events = agent.prompt("echo hi").collect::<Vec<_>>().await;

        assert_eq!(executions.load(Ordering::SeqCst), 0, "{case}");
        assert_eq!(hook_calls.load(Ordering::SeqCst), 0, "{case}");
        assert!(
            events.iter().any(|event| matches!(
                event,
                AgentEvent::AgentError { error } if error == "invalid terminal tool arguments"
            )),
            "{case}"
        );
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, AgentEvent::ToolCallStart { .. })),
            "{case}"
        );
        assert!(
            !agent
                .messages()
                .iter()
                .any(|message| matches!(message, AgentMessage::Assistant { .. })),
            "{case}"
        );
    }
}

#[tokio::test]
async fn schema_mismatch_returns_tool_error_before_hooks_or_execution() {
    let api_key = "test-api-schema-mismatch";
    let provider = Arc::new(TestProvider::new(vec![tool_use_turn(
        "tool_schema",
        "echo",
        serde_json::json!({"text": 7}),
    )]));
    let provider_guard = ProviderGuard::register(api_key, provider);
    let hook_calls = Arc::new(AtomicUsize::new(0));
    let hook_calls_for_hook = hook_calls.clone();
    let executions = Arc::new(AtomicUsize::new(0));
    let executions_for_tool = executions.clone();
    let mut config = test_config(api_key, Some(&provider_guard));
    config.hooks.before_tool_call = Some(Arc::new(move |_| {
        hook_calls_for_hook.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Ok(None) })
    }));
    let agent = Agent::new(config);
    agent
        .add_tool(AgentTool {
            name: "echo".into(),
            description: "echoes text".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {"text": {"type": "string"}},
                "required": ["text"],
                "additionalProperties": false
            }),
            execution_mode: None,
            execute: Arc::new(move |_, _, _| {
                executions_for_tool.fetch_add(1, Ordering::SeqCst);
                Box::pin(async { Ok(AgentToolOutput::new(Vec::new())) })
            }),
        })
        .unwrap();

    let events = agent.prompt("echo").collect::<Vec<_>>().await;

    assert_eq!(hook_calls.load(Ordering::SeqCst), 0);
    assert_eq!(executions.load(Ordering::SeqCst), 0);
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, AgentEvent::ToolCallStart { .. }))
    );
    assert!(events.iter().any(|event| matches!(
        event,
        AgentEvent::ToolCallEnd { result, .. }
            if result.is_error
                && matches!(
                    result.content.first(),
                    Some(ContentBlock::Text { text, .. })
                        if text == "tool arguments do not match the registered schema for echo"
                )
    )));
    assert!(
        agent
            .messages()
            .iter()
            .any(|message| matches!(message, AgentMessage::ToolResult { is_error: true, .. }))
    );
}

#[tokio::test]
async fn duplicate_and_cross_turn_tool_call_ids_fail_before_history_writeback() {
    let duplicate_api = "test-api-duplicate-tool-call-id";
    let duplicate_provider = Arc::new(TestProvider::new(vec![two_tool_use_turn_with_ids(
        "duplicate",
        "duplicate",
    )]));
    let duplicate_guard = ProviderGuard::register(duplicate_api, duplicate_provider);
    let duplicate_agent = Agent::new(test_config(duplicate_api, Some(&duplicate_guard)));
    let duplicate_events = duplicate_agent
        .prompt("duplicate")
        .collect::<Vec<_>>()
        .await;
    assert!(duplicate_events.iter().any(|event| matches!(
        event,
        AgentEvent::AgentError { error }
            if error == "duplicate or reused terminal tool-call identity"
    )));
    assert!(
        !duplicate_agent
            .messages()
            .iter()
            .any(|message| matches!(message, AgentMessage::Assistant { .. }))
    );

    let reused_api = "test-api-reused-tool-call-id";
    let reused_provider = Arc::new(TestProvider::new(vec![
        tool_use_turn("reused", "echo", serde_json::json!({})),
        tool_use_turn("reused", "echo", serde_json::json!({})),
    ]));
    let reused_guard = ProviderGuard::register(reused_api, reused_provider);
    let executions = Arc::new(AtomicUsize::new(0));
    let executions_for_tool = executions.clone();
    let reused_agent = Agent::new(test_config(reused_api, Some(&reused_guard)));
    reused_agent
        .add_tool(AgentTool {
            name: "echo".into(),
            description: "echo".into(),
            parameters: serde_json::json!({"type": "object"}),
            execution_mode: None,
            execute: Arc::new(move |_, _, _| {
                executions_for_tool.fetch_add(1, Ordering::SeqCst);
                Box::pin(async { Ok(AgentToolOutput::new(Vec::new())) })
            }),
        })
        .unwrap();
    let reused_events = reused_agent.prompt("reuse").collect::<Vec<_>>().await;
    assert_eq!(executions.load(Ordering::SeqCst), 1);
    assert!(reused_events.iter().any(|event| matches!(
        event,
        AgentEvent::AgentError { error }
            if error == "duplicate or reused terminal tool-call identity"
    )));
    let assistant_count = reused_agent
        .messages()
        .iter()
        .filter(|message| matches!(message, AgentMessage::Assistant { .. }))
        .count();
    assert_eq!(assistant_count, 1);
}

#[tokio::test(start_paused = true)]
async fn tool_update_events_stream_before_tool_end() {
    let api_key = "test-api-tool-updates";
    let provider = Arc::new(TestProvider::new(vec![
        tool_use_turn("tool_1", "streaming", serde_json::json!({})),
        text_turn("done"),
    ]));
    let _provider_guard = ProviderGuard::register(api_key, provider);

    let mut config = test_config(api_key, Some(&_provider_guard));
    config.tool_execution = ToolExecutionMode::Sequential;
    let agent = Agent::new(config);
    agent
        .add_tool(AgentTool {
            name: "streaming".into(),
            description: "streams updates".into(),
            parameters: serde_json::json!({"type": "object"}),
            execution_mode: None,
            execute: Arc::new(|_, _, on_update| {
                Box::pin(async move {
                    if let Some(on_update) = on_update {
                        on_update(AgentToolOutput::new(vec![ContentBlock::Text {
                            text: "partial".into(),
                            text_signature: None,
                        }]));
                    }
                    Ok(AgentToolOutput::new(vec![ContentBlock::Text {
                        text: "final".into(),
                        text_signature: None,
                    }]))
                })
            }),
        })
        .unwrap();

    let events: Vec<_> = agent.prompt("go").collect().await;
    let update_index = events
        .iter()
        .position(|event| {
            matches!(
                event,
                AgentEvent::ToolCallUpdate {
                    tool_call_id,
                    update,
                    ..
                } if tool_call_id == "tool_1"
                    && matches!(
                        update.content.first(),
                        Some(ContentBlock::Text { text, .. }) if text == "partial"
                    )
            )
        })
        .expect("expected tool update event");
    let end_index = events
        .iter()
        .position(|event| matches!(event, AgentEvent::ToolCallEnd { tool_call_id, .. } if tool_call_id == "tool_1"))
        .expect("expected tool end event");

    assert!(update_index < end_index);
}

#[tokio::test(start_paused = true)]
async fn tool_execution_deadline_cancels_the_invocation_and_returns_a_tool_error() {
    let api_key = "test-api-tool-deadline";
    let provider = Arc::new(TestProvider::new(vec![
        tool_use_turn("tool_1", "pending", serde_json::json!({})),
        text_turn("recovered"),
    ]));
    let _provider_guard = ProviderGuard::register(api_key, provider);

    let mut config = test_config(api_key, Some(&_provider_guard));
    config.tool_execution = ToolExecutionMode::Sequential;
    let agent = Agent::new(config);
    agent
        .add_tool(AgentTool {
            name: "pending".into(),
            description: "waits until cancelled".into(),
            parameters: serde_json::json!({"type": "object"}),
            execution_mode: None,
            execute: Arc::new(|context, _, _| {
                Box::pin(async move {
                    context.cancel_token().cancelled().await;
                    Err("cancelled by deadline".into())
                })
            }),
        })
        .unwrap();

    let task = tokio::spawn({
        let agent = agent.clone();
        async move { agent.prompt("go").collect::<Vec<_>>().await }
    });
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_secs(30 * 60)).await;
    let events = task.await.unwrap();

    assert!(events.iter().any(|event| matches!(
        event,
        AgentEvent::ToolCallEnd { result, .. }
            if result.is_error
                && matches!(
                    result.content.first(),
                    Some(ContentBlock::Text { text, .. })
                        if text == "tool execution deadline exceeded"
                )
    )));
    assert!(
        events
            .iter()
            .any(|event| matches!(event, AgentEvent::AgentDone { .. }))
    );
}

#[tokio::test]
async fn tool_progress_is_bounded_before_it_enters_the_event_channel() {
    let api_key = "test-api-tool-progress-limit";
    let provider = Arc::new(TestProvider::new(vec![
        tool_use_turn("tool_1", "noisy", serde_json::json!({})),
        text_turn("recovered"),
    ]));
    let _provider_guard = ProviderGuard::register(api_key, provider);

    let mut config = test_config(api_key, Some(&_provider_guard));
    config.tool_execution = ToolExecutionMode::Sequential;
    let agent = Agent::new(config);
    agent
        .add_tool(AgentTool {
            name: "noisy".into(),
            description: "emits excessive progress".into(),
            parameters: serde_json::json!({"type": "object"}),
            execution_mode: None,
            execute: Arc::new(|_, _, on_update| {
                Box::pin(async move {
                    let on_update = on_update.expect("sequential tools receive progress callback");
                    for index in 0..65 {
                        on_update(AgentToolOutput::new(vec![ContentBlock::Text {
                            text: format!("progress-{index}"),
                            text_signature: None,
                        }]));
                    }
                    Ok(AgentToolOutput::new(vec![ContentBlock::Text {
                        text: "final".into(),
                        text_signature: None,
                    }]))
                })
            }),
        })
        .unwrap();

    let events = agent.prompt("go").collect::<Vec<_>>().await;
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, AgentEvent::ToolCallUpdate { .. }))
            .count(),
        64
    );
    assert!(events.iter().any(|event| matches!(
        event,
        AgentEvent::ToolCallEnd { result, .. }
            if result.is_error
                && matches!(
                    result.content.first(),
                    Some(ContentBlock::Text { text, .. })
                        if text == "tool progress exceeds the retention limit"
                )
    )));
}

#[tokio::test]
async fn oversized_tool_results_are_replaced_before_event_and_history_retention() {
    let api_key = "test-api-tool-result-limit";
    let provider = Arc::new(TestProvider::new(vec![
        tool_use_turn("tool_1", "oversized", serde_json::json!({})),
        text_turn("recovered"),
    ]));
    let _provider_guard = ProviderGuard::register(api_key, provider);

    let agent = Agent::new(test_config(api_key, Some(&_provider_guard)));
    agent
        .add_tool(AgentTool {
            name: "oversized".into(),
            description: "returns excessive content".into(),
            parameters: serde_json::json!({"type": "object"}),
            execution_mode: None,
            execute: Arc::new(|_, _, _| {
                Box::pin(async {
                    Ok(AgentToolOutput::new(vec![ContentBlock::Text {
                        text: "x".repeat(600 * 1024),
                        text_signature: None,
                    }]))
                })
            }),
        })
        .unwrap();

    let events = agent.prompt("go").collect::<Vec<_>>().await;
    assert!(events.iter().any(|event| matches!(
        event,
        AgentEvent::ToolCallEnd { result, .. }
            if result.is_error
                && matches!(
                    result.content.first(),
                    Some(ContentBlock::Text { text, .. })
                        if text == "tool result exceeds the retention limit"
                )
    )));
    assert!(agent.messages().iter().any(|message| matches!(
        message,
        AgentMessage::ToolResult {
            is_error: true,
            content,
            ..
        } if matches!(
            content.first(),
            Some(ContentBlock::Text { text, .. })
                if text == "tool result exceeds the retention limit"
        )
    )));
}

#[tokio::test]
async fn unknown_tool_yields_error_content_and_continues() {
    let api_key = "test-api-3";
    let provider = Arc::new(TestProvider::new(vec![
        tool_use_turn("tool_1", "nonexistent", serde_json::json!({})),
        text_turn("I tried but the tool was not found."),
    ]));
    let _provider_guard = ProviderGuard::register(api_key, provider);

    let agent = Agent::new(test_config(api_key, Some(&_provider_guard)));

    let stream = agent.prompt("use nonexistent tool");
    let events: Vec<_> = stream.collect().await;

    let tool_end = events
        .iter()
        .find_map(|e| match e {
            AgentEvent::ToolCallEnd { result, .. } => Some(result.clone()),
            _ => None,
        })
        .unwrap();
    assert!(tool_end.is_error);
    assert!(
        tool_end
            .content
            .iter()
            .any(|b| matches!(b, ContentBlock::Text { text, .. } if text.contains("unknown tool")))
    );

    let has_done = events
        .iter()
        .any(|e| matches!(e, AgentEvent::AgentDone { .. }));
    assert!(has_done);
}

#[tokio::test]
async fn max_turns_exceeded_yields_error() {
    let api_key = "test-api-4";
    let mut turns = Vec::new();
    for index in 0..10 {
        turns.push(tool_use_turn(
            &format!("tool_{index}"),
            "echo",
            serde_json::json!({"text": "x"}),
        ));
    }
    let provider = Arc::new(TestProvider::new(turns));
    let _provider_guard = ProviderGuard::register(api_key, provider);

    let mut config = test_config(api_key, Some(&_provider_guard));
    config.max_turns = Some(2);

    let agent = Agent::new(config);
    let tool = AgentTool {
        name: "echo".into(),
        description: "echo".into(),
        parameters: serde_json::json!({"type": "object"}),
        execution_mode: None,
        execute: Arc::new(|_, _, _on_update| {
            Box::pin(async {
                Ok(AgentToolOutput::new(vec![ContentBlock::Text {
                    text: "ok".into(),
                    text_signature: None,
                }]))
            })
        }),
    };
    agent.add_tool(tool).unwrap();

    let stream = agent.prompt("go");
    let events: Vec<_> = stream.collect().await;

    let has_error = events
        .iter()
        .any(|e| matches!(e, AgentEvent::AgentError { error } if error.contains("max turns")));
    assert!(has_error, "should have max turns error");
}

#[tokio::test]
async fn unlimited_max_turns_runs_to_natural_completion() {
    // Parity check with TS `pi/packages/agent`: when `max_turns` is `None`,
    // the loop must keep running until the model stops producing tool calls
    // (or another stop condition fires), with no hard turn ceiling.
    let api_key = "test-api-no-cap";
    let mut turns = Vec::new();
    for index in 0..40 {
        turns.push(tool_use_turn(
            &format!("tool_{index}"),
            "echo",
            serde_json::json!({"text": "x"}),
        ));
    }
    turns.push(text_turn("Done after many tool calls."));
    let provider = Arc::new(TestProvider::new(turns));
    let _provider_guard = ProviderGuard::register(api_key, provider);

    let mut config = test_config(api_key, Some(&_provider_guard));
    config.max_turns = None;

    let agent = Agent::new(config);
    let tool = AgentTool {
        name: "echo".into(),
        description: "echo".into(),
        parameters: serde_json::json!({"type": "object"}),
        execution_mode: None,
        execute: Arc::new(|_, _, _on_update| {
            Box::pin(async {
                Ok(AgentToolOutput::new(vec![ContentBlock::Text {
                    text: "ok".into(),
                    text_signature: None,
                }]))
            })
        }),
    };
    agent.add_tool(tool).unwrap();

    let stream = agent.prompt("go");
    let events: Vec<_> = stream.collect().await;

    let has_done = events
        .iter()
        .any(|e| matches!(e, AgentEvent::AgentDone { .. }));
    assert!(
        has_done,
        "AgentDone should be emitted; events without a turn cap should not be aborted"
    );
    let has_max_turns_error = events
        .iter()
        .any(|e| matches!(e, AgentEvent::AgentError { error } if error.contains("max turns")));
    assert!(
        !has_max_turns_error,
        "max_turns: None must not yield a max-turns error"
    );
}

#[tokio::test]
async fn abort_mid_turn_yields_error() {
    let api_key = "test-api-5";
    let provider = Arc::new(TestProvider::new(vec![text_turn("Hello")]));
    let _provider_guard = ProviderGuard::register(api_key, provider);

    let agent = Agent::new(test_config(api_key, Some(&_provider_guard)));

    let stream = agent.prompt("hi");
    agent.abort();

    let events: Vec<_> = stream.collect().await;
    let has_abort_error = events
        .iter()
        .any(|e| matches!(e, AgentEvent::AgentError { error } if error.contains("aborted")));
    assert!(has_abort_error, "should have aborted error");
}

#[tokio::test]
async fn concurrent_prompt_returns_typed_busy_admission_error() {
    let api_key = "test-concurrent-prompt-admission";
    let _provider_guard = ProviderGuard::register(api_key, Arc::new(TestProvider::new(vec![])));
    let agent = Agent::new(test_config(api_key, Some(&_provider_guard)));
    let first = agent.prompt("first");
    let error = match agent.try_prompt("second") {
        Ok(_) => panic!("concurrent prompt should be rejected"),
        Err(error) => error,
    };
    assert!(matches!(
        error,
        agent_core::api::agent::AgentAdmissionError::Busy {
            operation: "prompt"
        }
    ));
    drop(first);
}

#[test]
fn queued_message_ids_remain_unique_across_existing_messages_and_queues() {
    let api_key = "test-queued-message-id-uniqueness";
    let _provider_guard = ProviderGuard::register(api_key, Arc::new(TestProvider::new(vec![])));
    let agent = Agent::new(test_config(api_key, Some(&_provider_guard)));
    agent.add_message(AgentMessage::UserText {
        message_id: "steer_0".into(),
        text: "existing".into(),
    });
    agent.steer("one").unwrap();
    agent.steer("two").unwrap();
    let queued = agent.drain_steering_queue();
    let ids: Vec<_> = queued
        .iter()
        .map(|message| match message {
            AgentMessage::UserText { message_id, .. } => message_id.as_str(),
            _ => "unexpected",
        })
        .collect();
    assert_eq!(ids, vec!["steer_1", "steer_2"]);
}

#[test]
fn adding_a_duplicate_message_id_is_normalized() {
    let api_key = "test-duplicate-message-normalization";
    let _provider_guard = ProviderGuard::register(api_key, Arc::new(TestProvider::new(vec![])));
    let agent = Agent::new(test_config(api_key, Some(&_provider_guard)));
    agent.add_message(AgentMessage::UserText {
        message_id: "replay_user_0".into(),
        text: "first".into(),
    });
    agent.add_message(AgentMessage::UserText {
        message_id: "replay_user_0".into(),
        text: "second".into(),
    });
    let messages = agent.messages();
    let ids: Vec<_> = messages
        .iter()
        .map(|message| match message {
            AgentMessage::UserText { message_id, .. } => message_id.as_str(),
            _ => "unexpected",
        })
        .collect();
    assert_eq!(ids, vec!["replay_user_0", "replay_user_0_1"]);
}

#[tokio::test]
async fn provider_error_event_preserves_error_message() {
    let api_key = "test-api-provider-error";
    let mut message = AssistantMessage::empty("test", "test-model");
    message.error_message = Some("provider failed".into());
    message.stop_reason = StopReason::Error;
    let provider = Arc::new(TestProvider::new(vec![ScriptedTurn {
        events: vec![AssistantMessageEvent::Error {
            reason: StopReason::Error,
            message,
        }],
        stop_reason: StopReason::Error,
        response_id: "resp_error".into(),
        model_name: "test-model".into(),
    }]));
    let _provider_guard = ProviderGuard::register(api_key, provider);

    let agent = Agent::new(test_config(api_key, Some(&_provider_guard)));

    let stream = agent.prompt("hi");
    let events: Vec<_> = stream.collect().await;
    let has_provider_error = events.iter().any(|event| {
        matches!(
            event,
            AgentEvent::AgentError { error } if error.contains("provider failed")
        )
    });
    assert!(has_provider_error, "should preserve provider error");
}

#[tokio::test]
async fn partial_delta_then_provider_error_is_visible_but_never_committed() {
    let mut config = test_config("partial-then-error", None);
    config.provider_streamer = Some(Arc::new(|_model, _context, _opts| {
        Box::pin(async_stream::stream! {
            let mut partial = AssistantMessage::empty("test", "test-model");
            partial.content.push(ContentBlock::Text {
                text: "uncommitted partial".into(),
                text_signature: None,
            });
            yield AssistantMessageEvent::TextDelta {
                content_index: 0,
                delta: "uncommitted partial".into(),
                partial,
            };
            let mut failed = AssistantMessage::empty("test", "test-model");
            failed.stop_reason = StopReason::Error;
            failed.error_message = Some("provider failed after partial output".into());
            yield AssistantMessageEvent::Error {
                reason: StopReason::Error,
                message: failed,
            };
        })
    }));
    let agent = Agent::new(config);

    let events = agent.prompt("hi").collect::<Vec<_>>().await;
    assert!(events.iter().any(|event| matches!(
        event,
        AgentEvent::LlmEvent(AssistantMessageEvent::TextDelta { delta, .. })
            if delta == "uncommitted partial"
    )));
    assert!(events.iter().any(|event| matches!(
        event,
        AgentEvent::AgentError { error } if error == "provider failed after partial output"
    )));
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, AgentEvent::AgentDone { .. }))
    );
    assert!(
        agent
            .messages()
            .iter()
            .all(|message| !matches!(message, AgentMessage::Assistant { .. })),
        "partial provider output must not enter committed model context"
    );
}

#[tokio::test]
async fn truncated_tool_arguments_end_as_error_without_tool_execution() {
    let mut config = test_config("truncated-tool-arguments", None);
    config.provider_streamer = Some(Arc::new(|_model, _context, _opts| {
        Box::pin(async_stream::stream! {
            let mut partial = AssistantMessage::empty("test", "test-model");
            partial.content.push(ContentBlock::ToolCall {
                id: "tool-truncated".into(),
                name: "read".into(),
                arguments: serde_json::json!("{\"path\":"),
                thought_signature: None,
            });
            yield AssistantMessageEvent::ToolcallStart {
                content_index: 0,
                partial: partial.clone(),
            };
            yield AssistantMessageEvent::ToolcallDelta {
                content_index: 0,
                delta: "{\"path\":".into(),
                partial,
            };
        })
    }));
    let agent = Agent::new(config);

    let events = agent.prompt("hi").collect::<Vec<_>>().await;
    assert!(events.iter().any(|event| matches!(
        event,
        AgentEvent::AgentError { error } if error == "LLM stream ended without Done event"
    )));
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, AgentEvent::ToolCallStart { .. }))
    );
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, AgentEvent::AgentDone { .. }))
    );
    assert!(
        agent
            .messages()
            .iter()
            .all(|message| !matches!(message, AgentMessage::Assistant { .. }))
    );
}

#[tokio::test]
async fn first_terminal_drops_provider_stream_before_duplicate_terminal_or_late_delta() {
    let polls = Arc::new(AtomicUsize::new(0));
    let stream_polls = polls.clone();
    let mut config = test_config("duplicate-provider-terminal", None);
    config.provider_streamer = Some(Arc::new(move |_model, _context, _opts| {
        let stream_polls = stream_polls.clone();
        Box::pin(async_stream::stream! {
            stream_polls.fetch_add(1, Ordering::SeqCst);
            yield AssistantMessageEvent::Done {
                reason: StopReason::Stop,
                message: done_text_message("first-terminal", "committed once"),
            };
            stream_polls.fetch_add(1, Ordering::SeqCst);
            let mut late = AssistantMessage::empty("test", "test-model");
            late.content.push(ContentBlock::Text {
                text: "late delta".into(),
                text_signature: None,
            });
            yield AssistantMessageEvent::TextDelta {
                content_index: 0,
                delta: "late delta".into(),
                partial: late,
            };
            yield AssistantMessageEvent::Done {
                reason: StopReason::Stop,
                message: done_text_message("duplicate-terminal", "must not be observed"),
            };
        })
    }));
    let agent = Agent::new(config);

    let events = agent.prompt("hi").collect::<Vec<_>>().await;
    assert_eq!(polls.load(Ordering::SeqCst), 1);
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(
                event,
                AgentEvent::LlmEvent(AssistantMessageEvent::Done { .. })
            ))
            .count(),
        1
    );
    assert!(!events.iter().any(|event| matches!(
        event,
        AgentEvent::LlmEvent(AssistantMessageEvent::TextDelta { delta, .. })
            if delta == "late delta"
    )));
    assert_eq!(
        agent
            .messages()
            .iter()
            .filter(|message| matches!(message, AgentMessage::Assistant { .. }))
            .count(),
        1
    );
}

#[tokio::test]
async fn provider_timeout_error_is_not_committed_as_a_successful_turn() {
    let api_key = "test-api-provider-timeout";
    let mut message = AssistantMessage::empty("test", "test-model");
    message.error_message = Some("provider invocation timed out after 10 ms".into());
    message.stop_reason = StopReason::Error;
    let provider = Arc::new(TestProvider::new(vec![ScriptedTurn {
        events: vec![AssistantMessageEvent::Error {
            reason: StopReason::Error,
            message,
        }],
        stop_reason: StopReason::Error,
        response_id: "resp_timeout".into(),
        model_name: "test-model".into(),
    }]));
    let _provider_guard = ProviderGuard::register(api_key, provider);
    let agent = Agent::new(test_config(api_key, Some(&_provider_guard)));

    let events = agent.prompt("hi").collect::<Vec<_>>().await;
    assert!(events.iter().any(|event| matches!(
        event,
        AgentEvent::AgentError { error } if error.contains("timed out after 10 ms")
    )));
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, AgentEvent::AgentDone { .. })),
        "timeout must not commit a successful assistant turn"
    );
}

#[tokio::test]
async fn run_returns_error_when_no_messages() {
    let api_key = "test-run-empty";
    let _provider_guard = ProviderGuard::register(api_key, Arc::new(TestProvider::new(vec![])));
    let agent = Agent::new(test_config(api_key, Some(&_provider_guard)));
    let result = agent.run();
    assert!(result.is_err());
    let err = result.err().unwrap();
    assert!(err.contains("no messages"), "got: {}", err);
}

#[test]
fn invalid_numeric_config_is_rejected_before_prompt_mutation() {
    let mut config = test_config("invalid-numeric-config", None);
    config.max_turns = Some(0);
    let agent = Agent::new(config);

    assert!(matches!(
        agent.try_prompt("must not be retained"),
        Err(AgentAdmissionError::InvalidConfig { .. })
    ));
    assert!(agent.messages().is_empty());
}

#[test]
fn aggregate_compaction_budget_is_checked_without_overflow() {
    let mut config = test_config("invalid-compaction-budget", None);
    config.compaction = Some(CompactionConfig {
        settings: CompactionSettings {
            enabled: true,
            reserve_tokens: MAX_COMPACTION_TOKEN_BUDGET,
            keep_recent_tokens: u32::MAX,
        },
        custom_instructions: None,
    });

    assert!(matches!(
        config.validate(),
        Err(AgentConfigError::CompactionTokenBudget { total })
            if total == u64::from(MAX_COMPACTION_TOKEN_BUDGET) + u64::from(u32::MAX)
    ));
}

#[tokio::test]
async fn run_returns_error_when_last_message_is_assistant() {
    let api_key = "test-run-assistant-tail";
    let _provider_guard = ProviderGuard::register(api_key, Arc::new(TestProvider::new(vec![])));
    let agent = Agent::new(test_config(api_key, Some(&_provider_guard)));
    agent.add_message(AgentMessage::UserText {
        message_id: "u".into(),
        text: "hi".into(),
    });
    agent.add_message(AgentMessage::Assistant {
        message_id: "a".into(),
        message: AssistantMessage::empty("test", "test-model"),
    });
    let result = agent.run();
    assert!(result.is_err());
    let err = result.err().unwrap();
    assert!(err.contains("assistant"), "got: {}", err);
}

#[tokio::test]
async fn run_succeeds_when_last_message_is_user() {
    let api_key = "test-run-user-tail";
    let _provider_guard =
        ProviderGuard::register(api_key, Arc::new(TestProvider::new(vec![text_turn("ok")])));
    let agent = Agent::new(test_config(api_key, Some(&_provider_guard)));
    agent.add_message(AgentMessage::UserText {
        message_id: "u".into(),
        text: "hi".into(),
    });
    let stream = agent.run();
    assert!(stream.is_ok());
    let mut s = stream.unwrap();
    while s.next().await.is_some() {}
}
