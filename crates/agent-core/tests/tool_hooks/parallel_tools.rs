use crate::common;
use agent_core::api::agent::{Agent, AgentEvent};
use agent_core::api::tool::{AgentTool, AgentToolOutput, ToolExecutionMode};
use ai::api::conversation::{ContentBlock, StopReason};
use ai::api::provider::ApiProvider;
use ai::api::testing::FauxProvider;
use common::{ProviderGuard, faux_model};
use futures::StreamExt;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};

const PARALLEL_TOOLS_SHARED_DELAY_MS: u64 = 100;
const PARALLEL_TOOLS_FAST_DELAY_MS: u64 = 10;
const PARALLEL_TOOLS_SHARED_ADVANCE: Duration =
    Duration::from_millis(PARALLEL_TOOLS_SHARED_DELAY_MS);

#[derive(Clone)]
struct ToolProbe {
    started: UnboundedSender<String>,
    finished: UnboundedSender<String>,
}

fn tool_probe() -> (
    ToolProbe,
    UnboundedReceiver<String>,
    UnboundedReceiver<String>,
) {
    let (started, started_rx) = mpsc::unbounded_channel();
    let (finished, finished_rx) = mpsc::unbounded_channel();
    (ToolProbe { started, finished }, started_rx, finished_rx)
}

fn delayed_tool(name: &str, delay_ms: u64, output_text: &str) -> AgentTool {
    let (probe, _, _) = tool_probe();
    probed_delayed_tool(name, delay_ms, output_text, probe)
}

fn probed_delayed_tool(
    name: &str,
    delay_ms: u64,
    output_text: &str,
    probe: ToolProbe,
) -> AgentTool {
    let name = name.to_string();
    let text = output_text.to_string();
    let tool_name = name.clone();
    AgentTool {
        name,
        description: format!("delayed {}ms", delay_ms),
        parameters: serde_json::json!({"type": "object"}),
        execution_mode: None,
        execute: Arc::new(move |_, _, _on_update| {
            let text = text.clone();
            let probe = probe.clone();
            let tool_name = tool_name.clone();
            Box::pin(async move {
                let _ = probe.started.send(tool_name.clone());
                tokio::time::sleep(Duration::from_millis(delay_ms)).await;
                let _ = probe.finished.send(tool_name);
                Ok(AgentToolOutput::new(vec![ContentBlock::Text {
                    text,
                    text_signature: None,
                }]))
            })
        }),
    }
}

async fn collect_prompt_events(agent: Agent) -> Vec<AgentEvent> {
    let mut stream = agent.prompt("go");
    let mut events = Vec::new();
    while let Some(event) = stream.next().await {
        events.push(event);
    }
    events
}

async fn recv_tool_signal(rx: &mut UnboundedReceiver<String>) -> String {
    rx.recv().await.expect("tool signal should be sent")
}

fn assert_no_tool_signal(rx: &mut UnboundedReceiver<String>) {
    assert!(matches!(
        rx.try_recv(),
        Err(mpsc::error::TryRecvError::Empty)
    ));
}

fn tool_end_names(events: &[AgentEvent]) -> Vec<String> {
    events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::ToolCallEnd { tool_name, .. } => Some(tool_name.clone()),
            _ => None,
        })
        .collect()
}

fn two_tool_call_provider(call1_name: &str, call2_name: &str) -> Arc<dyn ApiProvider> {
    let json_args = serde_json::json!({}).to_string();
    Arc::new(FauxProvider::with_call_queue(vec![
        FauxProvider::single_call(
            vec![ai::api::testing::FauxResponse {
                text_deltas: vec![],
                thinking_deltas: vec![],
                tool_calls: vec![
                    ai::api::testing::FauxToolCall {
                        id: "tool_slow".into(),
                        name: call1_name.into(),
                        deltas: vec![json_args.clone()],
                        final_arguments: serde_json::json!({}),
                    },
                    ai::api::testing::FauxToolCall {
                        id: "tool_fast".into(),
                        name: call2_name.into(),
                        deltas: vec![json_args],
                        final_arguments: serde_json::json!({}),
                    },
                ],
            }],
            StopReason::ToolUse,
        ),
        FauxProvider::text_call("done.", StopReason::Stop),
    ]))
}

fn many_tool_call_provider(tool_name: &str, count: usize) -> Arc<dyn ApiProvider> {
    let tool_calls = (0..count)
        .map(|index| ai::api::testing::FauxToolCall {
            id: format!("tool_{index}"),
            name: tool_name.into(),
            deltas: vec!["{}".into()],
            final_arguments: serde_json::json!({}),
        })
        .collect();
    Arc::new(FauxProvider::with_call_queue(vec![
        FauxProvider::single_call(
            vec![ai::api::testing::FauxResponse {
                text_deltas: vec![],
                thinking_deltas: vec![],
                tool_calls,
            }],
            StopReason::ToolUse,
        ),
        FauxProvider::text_call("done.", StopReason::Stop),
    ]))
}

fn make_two_tool_call_provider(api: &str, call1_name: &str, call2_name: &str) -> ProviderGuard {
    ProviderGuard::register(api, two_tool_call_provider(call1_name, call2_name))
}

#[tokio::test(start_paused = true)]
async fn parallel_tools_share_one_virtual_delay_while_sequential_tools_wait_per_tool() {
    let api_par = "parallel-faster-par";
    let api_seq = "parallel-faster-seq";

    let _provider_guard = ProviderGuard::register_many(vec![
        (api_par.to_string(), two_tool_call_provider("slow", "fast")),
        (api_seq.to_string(), two_tool_call_provider("slow", "fast")),
    ]);

    let mut config_par = _provider_guard.agent_config(faux_model(api_par));
    config_par.tool_execution = ToolExecutionMode::Parallel;
    config_par.max_turns = Some(5);

    let agent_par = Agent::new(config_par);
    let (par_probe, mut par_started, mut par_finished) = tool_probe();
    agent_par
        .add_tool(probed_delayed_tool(
            "slow",
            PARALLEL_TOOLS_SHARED_DELAY_MS,
            "slow_result",
            par_probe.clone(),
        ))
        .unwrap();
    agent_par
        .add_tool(probed_delayed_tool(
            "fast",
            PARALLEL_TOOLS_SHARED_DELAY_MS,
            "fast_result",
            par_probe,
        ))
        .unwrap();

    let par_task = tokio::spawn(collect_prompt_events(agent_par));
    assert_eq!(recv_tool_signal(&mut par_started).await, "slow");
    assert_eq!(recv_tool_signal(&mut par_started).await, "fast");
    assert_no_tool_signal(&mut par_finished);

    tokio::time::advance(PARALLEL_TOOLS_SHARED_ADVANCE).await;
    let mut parallel_finished = vec![
        recv_tool_signal(&mut par_finished).await,
        recv_tool_signal(&mut par_finished).await,
    ];
    parallel_finished.sort();
    assert_eq!(parallel_finished, vec!["fast", "slow"]);
    let mut parallel_end_events = tool_end_names(&par_task.await.unwrap());
    parallel_end_events.sort();
    assert_eq!(parallel_end_events, vec!["fast", "slow"]);

    let mut config_seq = _provider_guard.agent_config(faux_model(api_seq));
    config_seq.tool_execution = ToolExecutionMode::Sequential;
    config_seq.max_turns = Some(5);

    let agent_seq = Agent::new(config_seq);
    let (seq_probe, mut seq_started, mut seq_finished) = tool_probe();
    agent_seq
        .add_tool(probed_delayed_tool(
            "slow",
            PARALLEL_TOOLS_SHARED_DELAY_MS,
            "slow_result",
            seq_probe.clone(),
        ))
        .unwrap();
    agent_seq
        .add_tool(probed_delayed_tool(
            "fast",
            PARALLEL_TOOLS_SHARED_DELAY_MS,
            "fast_result",
            seq_probe,
        ))
        .unwrap();

    let seq_task = tokio::spawn(collect_prompt_events(agent_seq));
    assert_eq!(recv_tool_signal(&mut seq_started).await, "slow");
    assert_no_tool_signal(&mut seq_started);

    tokio::time::advance(PARALLEL_TOOLS_SHARED_ADVANCE).await;
    assert_eq!(recv_tool_signal(&mut seq_finished).await, "slow");
    assert_eq!(recv_tool_signal(&mut seq_started).await, "fast");
    assert!(!seq_task.is_finished());

    tokio::time::advance(PARALLEL_TOOLS_SHARED_ADVANCE).await;
    assert_eq!(recv_tool_signal(&mut seq_finished).await, "fast");
    assert_eq!(
        tool_end_names(&seq_task.await.unwrap()),
        vec!["slow", "fast"]
    );
}

#[tokio::test(start_paused = true)]
async fn parallel_tool_results_are_appended_in_assistant_order() {
    let api = "parallel-order";
    let _provider_guard = make_two_tool_call_provider(api, "slow", "fast");
    let mut config = _provider_guard.agent_config(faux_model(api));
    config.tool_execution = ToolExecutionMode::Parallel;
    config.max_turns = Some(5);

    let agent = Agent::new(config);
    agent
        .add_tool(delayed_tool(
            "slow",
            PARALLEL_TOOLS_SHARED_DELAY_MS,
            "slow_result",
        ))
        .unwrap();
    agent
        .add_tool(delayed_tool(
            "fast",
            PARALLEL_TOOLS_FAST_DELAY_MS,
            "fast_result",
        ))
        .unwrap();

    let mut stream = agent.prompt("go");
    while stream.next().await.is_some() {}

    let results: Vec<_> = agent
        .messages()
        .into_iter()
        .filter_map(|msg| match msg {
            agent_core::api::agent::AgentMessage::ToolResult { tool_name, .. } => Some(tool_name),
            _ => None,
        })
        .collect();
    assert_eq!(results, vec!["slow", "fast"]);
}

#[tokio::test(start_paused = true)]
async fn parallel_tool_end_events_are_emitted_in_completion_order() {
    let api = "parallel-event-order";
    let _provider_guard = make_two_tool_call_provider(api, "slow", "fast");
    let mut config = _provider_guard.agent_config(faux_model(api));
    config.tool_execution = ToolExecutionMode::Parallel;
    config.max_turns = Some(5);

    let agent = Agent::new(config);
    agent
        .add_tool(delayed_tool(
            "slow",
            PARALLEL_TOOLS_SHARED_DELAY_MS,
            "slow_result",
        ))
        .unwrap();
    agent
        .add_tool(delayed_tool(
            "fast",
            PARALLEL_TOOLS_FAST_DELAY_MS,
            "fast_result",
        ))
        .unwrap();

    let mut stream = agent.prompt("go");
    let mut end_events = Vec::new();
    while let Some(event) = stream.next().await {
        if let agent_core::api::agent::AgentEvent::ToolCallEnd { tool_name, .. } = event {
            end_events.push(tool_name);
        }
    }

    assert_eq!(end_events, vec!["fast", "slow"]);
}

#[tokio::test(start_paused = true)]
async fn per_tool_sequential_override_forces_batch_sequential() {
    let api = "parallel-override";
    let _provider_guard = make_two_tool_call_provider(api, "slow", "fast");
    let mut config = _provider_guard.agent_config(faux_model(api));
    config.tool_execution = ToolExecutionMode::Parallel;
    config.max_turns = Some(5);

    let agent = Agent::new(config);
    let (probe, mut started, mut finished) = tool_probe();
    agent
        .add_tool(probed_delayed_tool(
            "slow",
            PARALLEL_TOOLS_SHARED_DELAY_MS,
            "slow_result",
            probe.clone(),
        ))
        .unwrap();
    let mut fast_tool =
        probed_delayed_tool("fast", PARALLEL_TOOLS_SHARED_DELAY_MS, "fast_result", probe);
    fast_tool.execution_mode = Some(ToolExecutionMode::Sequential);
    agent.add_tool(fast_tool).unwrap();

    let task = tokio::spawn(collect_prompt_events(agent));
    assert_eq!(recv_tool_signal(&mut started).await, "slow");
    assert_no_tool_signal(&mut started);

    tokio::time::advance(PARALLEL_TOOLS_SHARED_ADVANCE).await;
    assert_eq!(recv_tool_signal(&mut finished).await, "slow");
    assert_eq!(recv_tool_signal(&mut started).await, "fast");
    assert!(!task.is_finished());

    tokio::time::advance(PARALLEL_TOOLS_SHARED_ADVANCE).await;
    assert_eq!(recv_tool_signal(&mut finished).await, "fast");
    assert_eq!(tool_end_names(&task.await.unwrap()), vec!["slow", "fast"]);
}

#[tokio::test(start_paused = true)]
async fn parallel_tool_scheduler_never_polls_more_than_eight_tools() {
    const TOOL_COUNT: usize = 9;
    const MAX_CONCURRENCY: usize = 8;

    let api = "parallel-bounded-concurrency";
    let _provider_guard =
        ProviderGuard::register(api, many_tool_call_provider("bounded", TOOL_COUNT));
    let mut config = _provider_guard.agent_config(faux_model(api));
    config.tool_execution = ToolExecutionMode::Parallel;
    config.max_turns = Some(5);

    let agent = Agent::new(config);
    let (probe, mut started, mut finished) = tool_probe();
    agent
        .add_tool(probed_delayed_tool(
            "bounded",
            PARALLEL_TOOLS_SHARED_DELAY_MS,
            "done",
            probe,
        ))
        .unwrap();

    let task = tokio::spawn(collect_prompt_events(agent));
    for _ in 0..MAX_CONCURRENCY {
        recv_tool_signal(&mut started).await;
    }
    assert_no_tool_signal(&mut started);

    tokio::time::advance(PARALLEL_TOOLS_SHARED_ADVANCE).await;
    for _ in 0..MAX_CONCURRENCY {
        recv_tool_signal(&mut finished).await;
    }
    recv_tool_signal(&mut started).await;
    assert_no_tool_signal(&mut started);

    tokio::time::advance(PARALLEL_TOOLS_SHARED_ADVANCE).await;
    recv_tool_signal(&mut finished).await;
    assert_eq!(tool_end_names(&task.await.unwrap()).len(), TOOL_COUNT);
}

#[tokio::test]
async fn more_than_sixty_four_tool_calls_fail_atomically_before_execution() {
    let api = "parallel-bounded-tool-count";
    let _provider_guard = ProviderGuard::register(api, many_tool_call_provider("bounded", 65));
    let mut config = _provider_guard.agent_config(faux_model(api));
    config.tool_execution = ToolExecutionMode::Parallel;
    config.max_turns = Some(5);

    let executions = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let executions_for_tool = Arc::clone(&executions);
    let agent = Agent::new(config);
    agent
        .add_tool(AgentTool {
            name: "bounded".into(),
            description: "records execution".into(),
            parameters: serde_json::json!({"type": "object"}),
            execution_mode: None,
            execute: Arc::new(move |_, _, _| {
                executions_for_tool.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Box::pin(async { Ok(AgentToolOutput::new(Vec::new())) })
            }),
        })
        .unwrap();

    let events = collect_prompt_events(agent).await;

    assert_eq!(executions.load(std::sync::atomic::Ordering::SeqCst), 0);
    assert!(events.iter().any(|event| matches!(
        event,
        AgentEvent::AgentError { error }
            if error == "tool-call count exceeds the per-turn limit"
    )));
    assert!(!events.iter().any(|event| matches!(
        event,
        AgentEvent::ToolCallStart { .. } | AgentEvent::ToolCallEnd { .. }
    )));
}
