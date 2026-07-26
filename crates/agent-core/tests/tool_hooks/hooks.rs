use crate::common;
use agent_core::api::agent::{
    AfterToolCallResult, Agent, AgentEvent, AgentMessage, BeforeToolCallResult, QueueMode,
};
use agent_core::api::tool::{AgentTool, AgentToolOutput};
use ai::api::conversation::{AssistantMessage, ContentBlock, Context, Message, StopReason};
use ai::api::model::Model;
use ai::api::provider::ApiProvider;
use ai::api::stream::{AssistantMessageEvent, EventStream, StreamOptions};
use ai::api::testing::FauxProvider;
use common::{ProviderGuard, TestProvider, faux_model, tool_use_turn};
use futures::StreamExt;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

fn simple_text_tool(name: &str, text: &str) -> AgentTool {
    let text = text.to_string();
    let name = name.to_string();
    AgentTool {
        name,
        description: "simple".into(),
        parameters: serde_json::json!({"type": "object"}),
        execution_mode: None,
        execute: Arc::new(move |_, _, _on_update| {
            let text = text.clone();
            Box::pin(async move {
                Ok(AgentToolOutput::new(vec![ContentBlock::Text {
                    text,
                    text_signature: None,
                }]))
            })
        }),
    }
}

fn script_tool_then_stop(api: &str, tool_name: &str, args: serde_json::Value) -> ProviderGuard {
    let json_str = args.to_string();
    ProviderGuard::register(
        api,
        Arc::new(FauxProvider::with_call_queue(vec![
            FauxProvider::single_call(
                vec![ai::api::testing::FauxResponse {
                    text_deltas: vec![],
                    thinking_deltas: vec![],
                    tool_calls: vec![ai::api::testing::FauxToolCall {
                        id: "tool_1".into(),
                        name: tool_name.into(),
                        deltas: vec![json_str],
                        final_arguments: args,
                    }],
                }],
                StopReason::ToolUse,
            ),
            FauxProvider::text_call("I'm done.", StopReason::Stop),
        ])),
    )
}

#[tokio::test]
async fn before_hook_blocks_tool_execution() {
    let api = "hooks-before";
    let calls = Arc::new(AtomicUsize::new(0));
    let calls_real = calls.clone();
    let _provider_guard = script_tool_then_stop(api, "echo", serde_json::json!({}));
    let mut config = _provider_guard.agent_config(faux_model(api));
    config.hooks.before_tool_call = Some(Arc::new(move |ctx| {
        assert_eq!(ctx.tool_name, "echo");
        assert_eq!(ctx.execution_context.tool_call_id(), "tool_1");
        assert_eq!(ctx.execution_context.tool_name(), "echo");
        Box::pin(async move {
            Ok(Some(BeforeToolCallResult {
                block: true,
                reason: Some("blocked by test".into()),
            }))
        })
    }));

    let agent = Agent::new(config);
    let calls_for_tool = calls_real.clone();
    agent
        .add_tool(AgentTool {
            name: "echo".into(),
            description: "echo".into(),
            parameters: serde_json::json!({"type": "object"}),
            execution_mode: None,
            execute: Arc::new(move |_, _, _on_update| {
                calls_for_tool.fetch_add(1, Ordering::SeqCst);
                Box::pin(async {
                    Ok(AgentToolOutput::new(vec![ContentBlock::Text {
                        text: "executed".into(),
                        text_signature: None,
                    }]))
                })
            }),
        })
        .unwrap();

    let mut stream = agent.prompt("run");
    let mut saw_blocked_result = false;
    let mut saw_tool_start = false;
    while let Some(event) = stream.next().await {
        match event {
            AgentEvent::ToolCallStart { .. } => saw_tool_start = true,
            AgentEvent::ToolCallEnd { result, .. } => {
                saw_blocked_result = result.is_error
                    && result
                        .content
                        .iter()
                        .any(|block| matches!(block, ContentBlock::Text { text, .. } if text == "blocked by test"));
            }
            _ => {}
        }
    }

    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert!(saw_blocked_result);
    assert!(!saw_tool_start);
}

#[tokio::test]
async fn after_hook_replaces_tool_result() {
    let api = "hooks-after";
    let _provider_guard = script_tool_then_stop(api, "echo", serde_json::json!({}));
    let mut config = _provider_guard.agent_config(faux_model(api));
    config.hooks.after_tool_call = Some(Arc::new(|ctx| {
        assert_eq!(ctx.tool_name, "echo");
        assert!(!ctx.result.is_error);
        Box::pin(async move {
            Ok(Some(AfterToolCallResult {
                content: Some(vec![ContentBlock::Text {
                    text: "rewritten".into(),
                    text_signature: None,
                }]),
                is_error: Some(true),
                terminate: Some(false),
            }))
        })
    }));

    let agent = Agent::new(config);
    agent
        .add_tool(simple_text_tool("echo", "original"))
        .unwrap();

    let mut stream = agent.prompt("run");
    while stream.next().await.is_some() {}

    let messages = agent.messages();
    assert!(messages.iter().any(|msg| matches!(
        msg,
        agent_core::api::agent::AgentMessage::ToolResult { is_error: true, content, .. }
            if content.iter().any(|block| matches!(block, ContentBlock::Text { text, .. } if text == "rewritten"))
    )));
}

#[tokio::test]
async fn after_hook_terminate_stops_loop_after_tool_results() {
    let api = "hooks-after-terminate";
    let _provider_guard = ProviderGuard::register(
        api,
        Arc::new(TestProvider::new(vec![tool_use_turn(
            "tool_1",
            "echo",
            serde_json::json!({}),
        )])),
    );
    let mut config = _provider_guard.agent_config(faux_model(api));
    config.hooks.after_tool_call = Some(Arc::new(|_| {
        Box::pin(async move {
            Ok(Some(AfterToolCallResult {
                content: None,
                is_error: None,
                terminate: Some(true),
            }))
        })
    }));

    let agent = Agent::new(config);
    agent.add_tool(simple_text_tool("echo", "ok")).unwrap();
    let events: Vec<_> = agent.prompt("run").collect().await;

    assert!(
        events
            .iter()
            .any(|event| matches!(event, AgentEvent::AgentDone { .. }))
    );
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, AgentEvent::AgentError { .. }))
    );
}

#[tokio::test]
async fn should_stop_after_turn_runs_before_follow_up_queue() {
    let api = "hooks-should-stop";
    let calls = Arc::new(AtomicUsize::new(0));
    let hook_calls = calls.clone();
    let _provider_guard = ProviderGuard::register(
        api,
        Arc::new(FauxProvider::with_call_queue(vec![
            FauxProvider::text_call("first.", StopReason::Stop),
            FauxProvider::text_call("second.", StopReason::Stop),
        ])),
    );
    let mut config = _provider_guard.agent_config(faux_model(api));
    config.follow_up_mode = QueueMode::All;
    config.hooks.should_stop_after_turn = Some(Arc::new(move |_| {
        hook_calls.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move { Ok(true) })
    }));

    let agent = Agent::new(config);
    agent.follow_up("should not run").unwrap();
    let events: Vec<_> = agent.prompt("run").collect().await;

    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(
        events
            .iter()
            .any(|event| matches!(event, AgentEvent::AgentDone { .. }))
    );
    let assistant_count = agent
        .messages()
        .iter()
        .filter(|message| matches!(message, AgentMessage::Assistant { .. }))
        .count();
    assert_eq!(assistant_count, 1);
}

#[tokio::test]
async fn convert_to_llm_hook_overrides_default_message_conversion() {
    let api = "hooks-convert-to-llm";
    let captured: Arc<std::sync::Mutex<Vec<Message>>> = Arc::new(std::sync::Mutex::new(Vec::new()));
    let captured_for_provider = captured.clone();

    struct CapturingProvider {
        captured: Arc<std::sync::Mutex<Vec<Message>>>,
    }
    impl ApiProvider for CapturingProvider {
        fn stream(
            &self,
            _model: &Model,
            ctx: Context,
            _opts: Option<StreamOptions>,
        ) -> EventStream {
            *self.captured.lock().unwrap() = ctx.messages.clone();
            Box::pin(async_stream::stream! {
                let mut msg = AssistantMessage::empty("test", "test-model");
                msg.content.push(ContentBlock::Text {
                    text: "ok".into(),
                    text_signature: None,
                });
                msg.stop_reason = StopReason::Stop;
                yield AssistantMessageEvent::Done {
                    reason: StopReason::Stop,
                    message: msg,
                };
            })
        }
    }

    let _provider_guard = ProviderGuard::register(
        api,
        Arc::new(CapturingProvider {
            captured: captured_for_provider,
        }),
    );

    let mut config = _provider_guard.agent_config(faux_model(api));
    config.hooks.convert_to_llm = Some(Arc::new(|messages, _resources| {
        Box::pin(async move {
            let combined = messages
                .iter()
                .filter_map(|m| match m {
                    AgentMessage::UserText { text, .. } => Some(text.clone()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("|");
            Ok(vec![Message::User {
                content: vec![ContentBlock::Text {
                    text: format!("merged:{}", combined),
                    text_signature: None,
                }],
            }])
        })
    }));

    let agent = Agent::new(config);
    agent.add_message(AgentMessage::UserText {
        message_id: "u0".into(),
        text: "first".into(),
    });
    let mut stream = agent.prompt("second");
    while stream.next().await.is_some() {}

    let messages_seen = captured.lock().unwrap().clone();
    assert_eq!(messages_seen.len(), 1);
    match &messages_seen[0] {
        Message::User { content } => match &content[0] {
            ContentBlock::Text { text, .. } => assert_eq!(text, "merged:first|second"),
            _ => panic!("expected text"),
        },
        _ => panic!("expected user"),
    }
}

#[tokio::test]
async fn transform_context_hook_rewrites_messages_before_llm_call() {
    let api = "hooks-transform-context";
    let captured: Arc<std::sync::Mutex<Vec<Message>>> = Arc::new(std::sync::Mutex::new(Vec::new()));
    let captured_for_provider = captured.clone();

    struct CapturingProvider {
        captured: Arc<std::sync::Mutex<Vec<Message>>>,
    }
    impl ApiProvider for CapturingProvider {
        fn stream(
            &self,
            _model: &Model,
            ctx: Context,
            _opts: Option<StreamOptions>,
        ) -> EventStream {
            *self.captured.lock().unwrap() = ctx.messages.clone();
            Box::pin(async_stream::stream! {
                let mut msg = AssistantMessage::empty("test", "test-model");
                msg.content.push(ContentBlock::Text {
                    text: "ok".into(),
                    text_signature: None,
                });
                msg.stop_reason = StopReason::Stop;
                yield AssistantMessageEvent::Done {
                    reason: StopReason::Stop,
                    message: msg,
                };
            })
        }
    }

    let _provider_guard = ProviderGuard::register(
        api,
        Arc::new(CapturingProvider {
            captured: captured_for_provider,
        }),
    );

    let mut config = _provider_guard.agent_config(faux_model(api));
    config.hooks.transform_context = Some(Arc::new(|messages| {
        Box::pin(async move {
            let replaced = vec![AgentMessage::UserText {
                message_id: "transformed".into(),
                text: format!("transformed:{}", messages.len()),
            }];
            Ok(replaced)
        })
    }));

    let agent = Agent::new(config);
    agent.add_message(AgentMessage::UserText {
        message_id: "u0".into(),
        text: "original-1".into(),
    });

    let mut stream = agent.prompt("original-2");
    while stream.next().await.is_some() {}

    let messages_seen = captured.lock().unwrap().clone();
    assert_eq!(messages_seen.len(), 1);
    match &messages_seen[0] {
        Message::User { content } => match &content[0] {
            ContentBlock::Text { text, .. } => assert_eq!(text, "transformed:2"),
            _ => panic!("expected text block"),
        },
        _ => panic!("expected user message"),
    }

    let stored = agent.messages();
    let user_count = stored
        .iter()
        .filter(|m| matches!(m, AgentMessage::UserText { .. }))
        .count();
    assert_eq!(user_count, 2, "transform must not mutate stored messages");
}

#[tokio::test]
async fn prepare_next_turn_can_replace_messages_before_follow_up_turn() {
    let api = "hooks-prepare-next-turn";
    let calls = Arc::new(AtomicUsize::new(0));
    let hook_calls = calls.clone();
    let _provider_guard = ProviderGuard::register(
        api,
        Arc::new(FauxProvider::with_call_queue(vec![
            FauxProvider::text_call("first.", StopReason::Stop),
            FauxProvider::text_call("second.", StopReason::Stop),
        ])),
    );
    let mut config = _provider_guard.agent_config(faux_model(api));
    config.follow_up_mode = QueueMode::All;
    config.hooks.prepare_next_turn = Some(Arc::new(move |ctx| {
        hook_calls.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move {
            if ctx.turn == 1 {
                Ok(Some(agent_core::api::agent::AgentLoopTurnUpdate {
                    messages: Some(vec![AgentMessage::UserText {
                        message_id: "prepared".into(),
                        text: "prepared context".into(),
                    }]),
                    ..Default::default()
                }))
            } else {
                Ok(None)
            }
        })
    }));

    let agent = Agent::new(config);
    agent.follow_up("follow-up").unwrap();
    let events: Vec<_> = agent.prompt("run").collect().await;

    assert!(calls.load(Ordering::SeqCst) >= 1);
    assert!(
        events
            .iter()
            .any(|event| matches!(event, AgentEvent::AgentDone { .. }))
    );
    assert!(agent.messages().iter().any(|message| {
        matches!(message, AgentMessage::UserText { text, .. } if text == "prepared context")
    }));
}
