use std::time::Duration;

use tokio::sync::mpsc;
use tokio::time::timeout;

use crate::agent::Agent;
use crate::agent::command::{AgentActorError, AgentHandle};
use crate::agent::types::{
    AgentConfig, AgentEvent, AgentMessage, AgentQueueError, ProviderStreamer,
};
use ai::api::client::AiClient;
use ai::api::provider::faux::{FauxCall, FauxProvider, FauxResponse, FauxToolCall};
use ai_protocol::api::conversation::{ContentBlock, StopReason};
use ai_protocol::api::model::{Model, ModelCost, ModelInput};
use ai_protocol::api::stream::{AssistantMessageEvent, EventStream};
use futures::StreamExt;
use schemars::JsonSchema;
use serde::Deserialize;
use std::sync::Arc;
use tool_contract::api::definition::{
    AuthorizationRisk, ToolBehaviorVersion, ToolCapabilities, ToolDefinition, ToolId, ToolKind,
};
use tool_contract::api::output::{ToolContent, ToolOutput};
use tool_contract::api::schema::schema_for;
use tool_runtime::api::{ToolRegistry, ToolRuntime, TypedTool};

#[derive(Deserialize, JsonSchema)]
struct RuntimeTestArgs {}

fn test_model() -> Model {
    Model {
        id: "faux-model".into(),
        name: "Faux Model".into(),
        api: "faux-api".into(),
        provider: "faux".into(),
        base_url: String::new(),
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

fn test_agent(calls: Vec<FauxCall>) -> Agent {
    let provider = Arc::new(FauxProvider::with_call_queue(calls));
    let ai_client = Arc::new(AiClient::new());
    ai_client.register_provider("faux-api", provider);
    let mut config = AgentConfig::new(test_model());
    config.provider_streamer = Some(Arc::new({
        let ai_client = Arc::clone(&ai_client);
        move |model, context, options| ai_client.stream_model(model, context, options)
    }));
    Agent::new(config)
}

fn hanging_agent() -> Agent {
    let streamer: ProviderStreamer = Arc::new(move |_model, _ctx, _opts| -> EventStream {
        Box::pin(futures::stream::pending::<AssistantMessageEvent>())
    });
    let mut config = AgentConfig::new(test_model());
    config.provider_streamer = Some(streamer);
    Agent::new(config)
}

fn text_call(text: &str, stop_reason: StopReason) -> FauxCall {
    FauxProvider::text_call(text, stop_reason)
}

fn tool_call(text: &str) -> FauxCall {
    FauxProvider::single_call(
        vec![FauxResponse {
            text_deltas: vec![text.to_string()],
            thinking_deltas: vec![],
            tool_calls: vec![FauxToolCall {
                id: "call_1".into(),
                name: "test_tool".into(),
                deltas: vec![],
                final_arguments: serde_json::json!({}),
            }],
        }],
        StopReason::ToolUse,
    )
}

async fn install_test_tool(agent: &Agent) {
    let definition = ToolDefinition {
        id: ToolId::new("test_tool").unwrap(),
        kind: ToolKind::Function,
        description: "Typed test tool".into(),
        parameters: schema_for::<RuntimeTestArgs>().unwrap(),
        capabilities: ToolCapabilities::default(),
        behavior: ToolBehaviorVersion::V1,
        authorization_risk: AuthorizationRisk::None,
        requirements: Vec::new(),
    };
    let tool = TypedTool::<RuntimeTestArgs>::new(definition, |_context, _args| {
        Box::pin(async {
            Ok(ToolOutput {
                content: vec![ToolContent::Text {
                    text: "typed result".into(),
                }],
                details: Some(serde_json::json!({"runtime": true})),
                terminate: false,
            })
        })
    })
    .unwrap();
    let mut registry = ToolRegistry::default();
    registry.register(Arc::new(tool)).unwrap();
    agent
        .set_tool_runtime(ToolRuntime::new(registry).unwrap())
        .await
        .unwrap();
}

#[tokio::test]
async fn complete_consumption_yields_terminal_events() {
    let agent = test_agent(vec![text_call("answer is 42", StopReason::Stop)]);
    let mut stream = agent.prompt("hello");
    let mut turns = 0;
    let mut saw_done = false;
    while let Some(event) = stream.next().await {
        match &event {
            AgentEvent::TurnStart { .. } => turns += 1,
            AgentEvent::AgentDone { .. } => saw_done = true,
            _ => {}
        }
    }
    assert_eq!(turns, 1);
    assert!(saw_done);
    assert_eq!(agent.messages().await.len(), 2);
}

#[tokio::test]
async fn typed_runtime_tools_are_declared_and_executed_without_legacy_registration() {
    let agent = test_agent(vec![
        tool_call("typed"),
        text_call("found", StopReason::Stop),
    ]);
    install_test_tool(&agent).await;

    let request = agent.provider_request_snapshot().await.0;
    assert!(request.tools.as_ref().is_some_and(|tools| {
        tools.iter().any(|tool| {
            tool.name == "test_tool" && tool.description.as_deref() == Some("Typed test tool")
        })
    }));

    let mut stream = agent.prompt("hello");
    let mut result = None;
    while let Some(event) = stream.next().await {
        if let AgentEvent::ToolCallEnd {
            result: tool_result,
            ..
        } = event
        {
            result = Some(tool_result);
        }
    }
    let result = result.expect("typed tool result event");
    assert!(matches!(
        result.content.as_slice(),
        [ContentBlock::Text { text, .. }] if text == "typed result"
    ));
    assert_eq!(result.details, Some(serde_json::json!({"runtime": true})));
}

#[tokio::test]
async fn dropping_stream_mid_turn_commits_messages_and_releases_run() {
    let agent = test_agent(vec![
        text_call("I'll check.", StopReason::ToolUse),
        text_call("done", StopReason::Stop),
    ]);
    let mut stream = agent.prompt("hello");
    assert!(matches!(
        stream.next().await,
        Some(AgentEvent::TurnStart { .. })
    ));
    drop(stream);
    assert!(
        agent
            .messages()
            .await
            .iter()
            .any(|message| matches!(message, AgentMessage::UserText { .. })),
        "the user prompt must survive an early drop"
    );
    let mut second = agent.prompt("next question");
    assert!(
        matches!(second.next().await, Some(AgentEvent::TurnStart { .. })),
        "a new run must be admitted after the stream is dropped"
    );
}

#[tokio::test]
async fn dropping_after_tool_turn_preserves_tool_results() {
    let agent = test_agent(vec![
        tool_call("searching"),
        text_call("found", StopReason::Stop),
    ]);
    install_test_tool(&agent).await;
    let mut stream = agent.prompt("hello");
    while let Some(event) = stream.next().await {
        if matches!(event, AgentEvent::ToolCallEnd { .. }) {
            break;
        }
    }
    drop(stream);
    // In the bounded actor model the turn runner may complete the next
    // turn before the consumer's drop is observed, so the exact count is
    // timing-dependent. The invariant that matters is that the tool
    // result survives the early drop.
    let messages = agent.messages().await;
    let has_tool_result = messages
        .iter()
        .any(|message| matches!(message, AgentMessage::ToolResult { .. }));
    assert!(has_tool_result, "tool result must survive an early drop");
}

#[tokio::test]
async fn clear_queues_during_turn_empties_queued_input() {
    let agent = test_agent(vec![
        tool_call("searching"),
        text_call("found", StopReason::Stop),
    ]);
    install_test_tool(&agent).await;
    let mut stream = agent.prompt("hello");
    while let Some(event) = stream.next().await {
        if matches!(event, AgentEvent::ToolCallEnd { .. }) {
            break;
        }
    }
    agent.steer("late input").expect("queue accepts");
    agent.clear_queues();
    while stream.next().await.is_some() {}
    assert!(agent.drain_steering_queue().await.is_empty());
    assert!(
        !agent.messages().await.iter().any(
            |message| matches!(message, AgentMessage::UserText { text, .. } if text == "late input")
        ),
        "cleared steering input must not reach the conversation"
    );
}

#[tokio::test]
async fn steering_during_turn_is_consumed_by_the_current_turn() {
    let agent = test_agent(vec![
        tool_call("searching"),
        text_call("found", StopReason::Stop),
    ]);
    install_test_tool(&agent).await;
    let mut stream = agent.prompt("hello");
    while let Some(event) = stream.next().await {
        if matches!(event, AgentEvent::ToolCallEnd { .. }) {
            break;
        }
    }
    agent.steer("steer during turn").expect("queue accepts");
    while stream.next().await.is_some() {}
    assert!(
        agent
            .messages()
            .await
            .iter()
            .any(|message| matches!(message, AgentMessage::UserText { text, .. } if text == "steer during turn")),
        "steering input enqueued mid-turn must be consumed by the current turn"
    );
}

#[tokio::test]
#[ignore = "release performance baseline"]
async fn agent_core_release_faux_first_text_delta_baseline() {
    const FIRST_DELTA_BUDGET_MICROS: u128 = 50_000;

    let agent = test_agent(vec![text_call("first delta", StopReason::Stop)]);
    let started = std::time::Instant::now();
    let mut stream = agent.prompt("hello");
    let first_delta_micros = loop {
        let event = stream.next().await.expect("faux stream has a text delta");
        if matches!(
            event,
            AgentEvent::LlmEvent(AssistantMessageEvent::TextDelta { .. })
        ) {
            break started.elapsed().as_micros();
        }
    };

    println!("agent_perf\tfaux_first_text_delta_us={first_delta_micros}");
    assert!(
        first_delta_micros <= FIRST_DELTA_BUDGET_MICROS,
        "local agent pipeline first TextDelta exceeded 50 ms: {first_delta_micros} us"
    );
}

#[tokio::test]
async fn edit_queue_entry_with_correct_version_succeeds() {
    let agent = test_agent(vec![text_call("answer", StopReason::Stop)]);
    agent.steer("original").expect("queue accepts");
    let new_message = AgentMessage::UserText {
        message_id: "steer_0".into(),
        text: "edited".into(),
    };
    agent
        .edit_queue_entry("steer_0", 0, new_message)
        .await
        .expect("edit succeeds");
    let drained = agent.drain_steering_queue().await;
    assert_eq!(drained.len(), 1);
    assert!(matches!(
        &drained[0],
        AgentMessage::UserText { text, .. } if text == "edited"
    ));
}

#[tokio::test]
async fn edit_queue_entry_with_stale_version_returns_conflict() {
    let agent = test_agent(vec![text_call("answer", StopReason::Stop)]);
    agent.steer("original").expect("queue accepts");
    let new_message = AgentMessage::UserText {
        message_id: "steer_0".into(),
        text: "edited".into(),
    };
    agent
        .edit_queue_entry("steer_0", 0, new_message)
        .await
        .expect("first edit succeeds");
    let stale_message = AgentMessage::UserText {
        message_id: "steer_0".into(),
        text: "stale".into(),
    };
    let result = agent.edit_queue_entry("steer_0", 0, stale_message).await;
    assert!(matches!(result, Err(AgentQueueError::StaleVersion { .. })));
}

#[tokio::test]
async fn edit_queue_entry_not_found_for_unknown_id() {
    let agent = test_agent(vec![text_call("answer", StopReason::Stop)]);
    let new_message = AgentMessage::UserText {
        message_id: "ghost".into(),
        text: "ghost".into(),
    };
    let result = agent.edit_queue_entry("ghost", 0, new_message).await;
    assert!(matches!(result, Err(AgentQueueError::NotFound { .. })));
}

#[tokio::test]
async fn remove_queue_entry_succeeds_with_correct_version() {
    let agent = test_agent(vec![text_call("answer", StopReason::Stop)]);
    agent.steer("original").expect("queue accepts");
    agent
        .remove_queue_entry("steer_0", 0)
        .await
        .expect("remove succeeds");
    assert!(agent.drain_steering_queue().await.is_empty());
}

#[tokio::test]
async fn interjection_drains_before_steering_at_turn_start() {
    let agent = test_agent(vec![text_call("answer", StopReason::Stop)]);
    agent.steer("steer input").expect("queue accepts");
    agent.interject("interject input").expect("queue accepts");
    let mut stream = agent.prompt("hello");
    while let Some(event) = stream.next().await {
        if matches!(event, AgentEvent::AgentDone { .. }) {
            break;
        }
    }
    let messages = agent.messages().await;
    let interject_idx = messages.iter().position(
        |m| matches!(m, AgentMessage::UserText { text, .. } if text == "interject input"),
    );
    let steer_idx = messages
        .iter()
        .position(|m| matches!(m, AgentMessage::UserText { text, .. } if text == "steer input"));
    assert!(interject_idx.is_some(), "interjection must be drained");
    assert!(steer_idx.is_some(), "steering must be drained");
    assert!(
        interject_idx < steer_idx,
        "interjection must drain before steering"
    );
}

#[tokio::test]
async fn clear_queues_empties_all_three_queues() {
    let agent = test_agent(vec![text_call("answer", StopReason::Stop)]);
    agent.steer("steer").expect("queue accepts");
    agent.follow_up("followup").expect("queue accepts");
    agent.interject("interject").expect("queue accepts");
    agent.clear_queues();
    let mut stream = agent.prompt("hello");
    while stream.next().await.is_some() {}
    let messages = agent.messages().await;
    assert!(
        !messages.iter().any(|m| matches!(m,
            AgentMessage::UserText { text, .. }
            if text == "steer" || text == "followup" || text == "interject"
        )),
        "cleared queues must not reach the conversation"
    );
}

#[tokio::test]
async fn steering_mid_tool_turn_does_not_break_tool_pairing() {
    let agent = test_agent(vec![
        tool_call("searching"),
        text_call("found", StopReason::Stop),
    ]);
    install_test_tool(&agent).await;
    let mut stream = agent.prompt("hello");
    while let Some(event) = stream.next().await {
        if matches!(event, AgentEvent::ToolCallEnd { .. }) {
            break;
        }
    }
    agent.steer("steer mid tool").expect("queue accepts");
    while stream.next().await.is_some() {}
    let messages = agent.messages().await;
    let tool_result_idx = messages
        .iter()
        .position(|m| matches!(m, AgentMessage::ToolResult { .. }));
    let steer_idx = messages
        .iter()
        .position(|m| matches!(m, AgentMessage::UserText { text, .. } if text == "steer mid tool"));
    assert!(tool_result_idx.is_some());
    assert!(steer_idx.is_some());
    assert!(
        tool_result_idx < steer_idx,
        "steering must appear after tool result, not between request and result"
    );
}

#[tokio::test]
async fn actor_closed_returns_error_after_actor_task_panics() {
    let (commands, mut receiver) = mpsc::channel(8);
    let handle = AgentHandle { commands };

    let join = tokio::spawn(async move {
        let _ = receiver.recv().await;
        panic!("simulated actor panic");
    });

    let error = handle
        .messages()
        .await
        .expect_err("request fails after actor panic");
    assert_eq!(error, AgentActorError::Closed);

    let join_result = join.await;
    assert!(join_result.is_err(), "spawned task should have panicked");
    assert!(join_result.unwrap_err().is_panic());

    let error = handle
        .steer("after panic".into())
        .expect_err("fire fails after actor panic");
    assert_eq!(error, AgentQueueError::ActorClosed);
}

#[tokio::test]
async fn provider_hang_does_not_freeze_actor() {
    let agent = hanging_agent();
    let mut stream = agent.prompt("hello");

    let event = timeout(Duration::from_secs(2), stream.next())
        .await
        .expect("TurnStart arrives within timeout")
        .expect("stream is alive");
    assert!(matches!(event, AgentEvent::TurnStart { .. }));

    let messages = timeout(Duration::from_secs(2), agent.messages())
        .await
        .expect("messages query completes during provider hang");
    assert!(!messages.is_empty(), "user prompt must be visible");

    agent.abort();

    timeout(Duration::from_secs(5), async {
        while stream.next().await.is_some() {}
    })
    .await
    .expect("turn terminates after abort without deadlock");

    let result = timeout(Duration::from_secs(2), agent.try_prompt("next"))
        .await
        .expect("try_prompt completes after abort");
    assert!(result.is_ok(), "actor admits a new prompt after abort");
}

#[tokio::test]
async fn steer_and_follow_up_during_provider_hang_are_preserved_after_abort() {
    let agent = hanging_agent();
    let mut stream = agent.prompt("hello");

    let event = timeout(Duration::from_secs(2), stream.next())
        .await
        .expect("TurnStart arrives within timeout")
        .expect("stream is alive");
    assert!(matches!(event, AgentEvent::TurnStart { .. }));

    agent
        .steer("steer during hang")
        .expect("steer accepts during provider hang");
    agent
        .follow_up("followup during hang")
        .expect("follow_up accepts during provider hang");
    agent.abort();

    timeout(Duration::from_secs(5), async {
        while stream.next().await.is_some() {}
    })
    .await
    .expect("turn terminates after abort without deadlock");

    let steering = timeout(Duration::from_secs(2), agent.drain_steering_queue())
        .await
        .expect("drain_steering_queue completes");
    assert!(
        steering.iter().any(
            |m| matches!(m, AgentMessage::UserText { text, .. } if text == "steer during hang")
        ),
        "steering enqueued during provider hang must survive abort"
    );

    let follow_ups = timeout(Duration::from_secs(2), agent.drain_follow_up_queue())
        .await
        .expect("drain_follow_up_queue completes");
    assert!(
        follow_ups.iter().any(|m| matches!(m,
            AgentMessage::UserText { text, .. } if text == "followup during hang"
        )),
        "follow-up enqueued during provider hang must survive abort"
    );

    let result = timeout(Duration::from_secs(2), agent.try_prompt("next"))
        .await
        .expect("try_prompt completes after abort");
    assert!(result.is_ok(), "actor admits a new prompt after abort");
}

#[tokio::test]
async fn concurrent_steering_follow_up_and_abort_do_not_corrupt_state() {
    let agent = test_agent(vec![
        tool_call("working"),
        text_call("done", StopReason::Stop),
    ]);
    install_test_tool(&agent).await;

    let mut stream = agent.prompt("hello");
    while let Some(event) = stream.next().await {
        if matches!(event, AgentEvent::ToolCallEnd { .. }) {
            break;
        }
    }

    agent.steer("concurrent steer").expect("steer accepts");
    agent
        .follow_up("concurrent followup")
        .expect("follow_up accepts");
    agent.abort();

    timeout(Duration::from_secs(5), async {
        while stream.next().await.is_some() {}
    })
    .await
    .expect("turn terminates after concurrent commands without deadlock");

    let messages = timeout(Duration::from_secs(2), agent.messages())
        .await
        .expect("messages query completes");
    assert!(!messages.is_empty(), "user prompt must survive");

    let result = timeout(Duration::from_secs(2), agent.try_prompt("next"))
        .await
        .expect("try_prompt completes");
    assert!(
        result.is_ok(),
        "actor admits a new prompt after concurrent commands"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn current_thread_runtime_does_not_freeze() {
    let agent = test_agent(vec![
        tool_call("working"),
        text_call("done", StopReason::Stop),
    ]);
    install_test_tool(&agent).await;

    let mut stream = agent.prompt("hello");
    let mut saw_tool = false;

    timeout(Duration::from_secs(5), async {
        while let Some(event) = stream.next().await {
            match event {
                AgentEvent::ToolCallEnd { .. } => {
                    saw_tool = true;
                    agent.steer("steer mid turn").expect("steer accepts");
                }
                AgentEvent::AgentDone { .. } => break,
                _ => {}
            }
        }
    })
    .await
    .expect("turn completes without deadlock in current-thread runtime");

    assert!(saw_tool, "tool must execute");
    let messages = timeout(Duration::from_secs(2), agent.messages())
        .await
        .expect("messages query completes");
    assert!(
        messages.iter().any(|m| matches!(m,
            AgentMessage::UserText { text, .. } if text == "steer mid turn"
        )),
        "steering must be consumed in current-thread runtime"
    );
}

#[tokio::test]
async fn shutdown_releases_actor_task() {
    let agent = test_agent(vec![text_call("answer", StopReason::Stop)]);
    agent.shutdown();

    let error = agent
        .handle
        .messages()
        .await
        .expect_err("messages fails after shutdown");
    assert_eq!(error, AgentActorError::Closed);

    let error = agent
        .steer("after shutdown")
        .expect_err("steer fails after shutdown");
    assert_eq!(error, AgentQueueError::ActorClosed);
}

#[tokio::test]
async fn slow_consumer_receives_all_events_without_loss() {
    let delta_count = 100;
    let deltas: Vec<String> = (0..delta_count).map(|i| format!("d{i}")).collect();
    let call = FauxProvider::single_call(
        vec![FauxResponse {
            text_deltas: deltas.clone(),
            thinking_deltas: vec![],
            tool_calls: vec![],
        }],
        StopReason::Stop,
    );
    let agent = test_agent(vec![call]);
    let mut stream = agent.prompt("hello");

    let mut received = Vec::new();
    timeout(Duration::from_secs(15), async {
        while let Some(event) = stream.next().await {
            if let AgentEvent::LlmEvent(AssistantMessageEvent::TextDelta { delta, .. }) = event {
                received.push(delta);
            }
            tokio::time::sleep(Duration::from_micros(200)).await;
        }
    })
    .await
    .expect("stream completes despite bounded-channel backpressure");

    assert_eq!(received.len(), delta_count);
    assert_eq!(received, deltas);
}
