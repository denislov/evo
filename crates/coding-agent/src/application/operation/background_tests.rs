//! End-to-end background task product tests: a real prompt drives the typed
//! `bash` tool in background mode, the tool returns a task id, and the
//! session-level background facade queries, waits on, and terminates the task.
use std::sync::Arc;
use std::time::Duration;

use agent_core::api::agent::AgentResources;
use ai::api::provider::faux::{FauxProvider, FauxResponse, FauxToolCall};
use ai_protocol::api::conversation::StopReason;
use ai_protocol::api::model::{Model, ModelCost, ModelInput};

use super::contract::{CodingAgentOperation, CodingAgentOperationOutcome};
use crate::app::bootstrap::{PromptInvocation, SessionRunOptions};
use crate::app::prompt_runtime::PromptRuntimeOptions;
use crate::authorization::ToolAuthorizationMode;
use crate::operations::prompt::context::PromptTurnOptions;
use crate::runtime::facade::{
    CodingAgentBackgroundTaskState, CodingAgentSession, CodingAgentSessionOptions,
};
use crate::test_support::ProviderGuard;

fn model(api: &str) -> Model {
    Model {
        id: "test-model".into(),
        name: "Test Model".into(),
        api: api.into(),
        provider: "test".into(),
        base_url: String::new(),
        reasoning: false,
        thinking_level_map: None,
        input: vec![ModelInput::Text],
        cost: ModelCost::default(),
        context_window: 0,
        max_tokens: 0,
        headers: None,
        compat: None,
    }
}

fn background_prompt(api: &str, command: &str) -> PromptTurnOptions {
    PromptTurnOptions::from_prompt_runtime_options(PromptRuntimeOptions {
        model: model(api),
        api_key: None,
        auth_diagnostics: Vec::new(),
        system_prompt: Some("system".into()),
        max_turns: Some(2),
        tools: Vec::new(),
        register_builtins: true,
        ai_client: None,
        session: Some(SessionRunOptions::disabled(".".into())),
        session_target: None,
        session_name: None,
        thinking_level: None,
        tool_execution: None,
        resources: AgentResources::default(),
        settings: None,
        invocation: PromptInvocation::Text(format!(
            "run this command in the background: {command}"
        )),
    })
}

async fn session_with_background_tool(
    provider_guard: &ProviderGuard,
) -> (CodingAgentSession, tempfile::TempDir) {
    let temp = tempfile::tempdir().expect("temp dir");
    let session = CodingAgentSession::create_internal(
        CodingAgentSessionOptions::new()
            .with_ai_client(provider_guard.ai_client())
            .with_tool_authorization_mode(ToolAuthorizationMode::Yolo)
            .with_session_id(format!(
                "background-session-{}",
                provider_guard
                    .ai_client()
                    .provider_registry()
                    .registered_apis()
                    .len()
            ))
            .with_session_log_root(temp.path()),
    )
    .await
    .expect("session");
    (session, temp)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn prompt_background_tool_returns_a_task_queryable_through_the_session_facade() {
    let api = "background-task-e2e";
    let provider_guard = ProviderGuard::register(
        api,
        Arc::new(FauxProvider::with_call_queue(vec![
            FauxProvider::single_call(
                vec![FauxResponse {
                    text_deltas: Vec::new(),
                    thinking_deltas: Vec::new(),
                    tool_calls: vec![FauxToolCall {
                        id: "tool-call-background".into(),
                        name: "bash".into(),
                        deltas: Vec::new(),
                        final_arguments: serde_json::json!({
                            "command": "sleep 0.2; printf 'background-done'",
                            "background": true,
                        }),
                    }],
                }],
                StopReason::ToolUse,
            ),
            FauxProvider::text_call("background task started", StopReason::Stop),
        ])),
    );
    let (mut session, _temp) = session_with_background_tool(&provider_guard).await;

    let outcome = session
        .run_internal(CodingAgentOperation::Prompt(background_prompt(
            api,
            "sleep 0.2; printf background-done",
        )))
        .await
        .expect("prompt completes");
    assert!(matches!(outcome, CodingAgentOperationOutcome::Prompt(_)));

    let tasks = session.background_task_list();
    assert_eq!(
        tasks.len(),
        1,
        "the background task is registered on the session"
    );
    assert!(tasks[0].owner.starts_with("operation:"));
    let task_id = tasks[0].task_id.clone();

    let snapshot = session
        .background_task_snapshot(&task_id)
        .expect("snapshot");
    assert_eq!(snapshot.task_id, task_id);

    let report = tokio::time::timeout(
        Duration::from_secs(5),
        session.background_task_wait(&task_id),
    )
    .await
    .expect("task completes")
    .expect("wait");
    assert_eq!(
        report.state,
        CodingAgentBackgroundTaskState::Completed { exit_code: Some(0) }
    );
    assert!(report.output.contains("background-done"));
    assert_eq!(report.dropped_bytes, None);

    let mut cursor = 0;
    let mut seen = String::new();
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    while !seen.contains("background-done") && std::time::Instant::now() < deadline {
        let chunk = session
            .background_task_output(&task_id, cursor)
            .expect("output chunk");
        assert_eq!(chunk.dropped_bytes, None);
        seen.push_str(&chunk.text);
        cursor = chunk.next_cursor;
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(seen.contains("background-done"));

    session.shutdown_internal().await.expect("shutdown");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn session_close_terminates_background_tasks() {
    let api = "background-task-shutdown";
    let provider_guard = ProviderGuard::register(
        api,
        Arc::new(FauxProvider::with_call_queue(vec![
            FauxProvider::single_call(
                vec![FauxResponse {
                    text_deltas: Vec::new(),
                    thinking_deltas: Vec::new(),
                    tool_calls: vec![FauxToolCall {
                        id: "tool-call-background-close".into(),
                        name: "bash".into(),
                        deltas: Vec::new(),
                        final_arguments: serde_json::json!({
                            "command": "sleep 300",
                            "background": true,
                        }),
                    }],
                }],
                StopReason::ToolUse,
            ),
            FauxProvider::text_call("background task started", StopReason::Stop),
        ])),
    );
    let (mut session, _temp) = session_with_background_tool(&provider_guard).await;
    session
        .run_internal(CodingAgentOperation::Prompt(background_prompt(
            api,
            "sleep 300",
        )))
        .await
        .expect("prompt completes");
    let tasks = session.background_task_list();
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0].state, CodingAgentBackgroundTaskState::Running);

    session.shutdown_internal().await.expect("shutdown");
    assert_eq!(
        session.background_task_list()[0].state,
        CodingAgentBackgroundTaskState::Cancelled,
        "session close terminates background tasks"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn explicit_cancel_terminates_a_background_task() {
    let api = "background-task-cancel";
    let provider_guard = ProviderGuard::register(
        api,
        Arc::new(FauxProvider::with_call_queue(vec![
            FauxProvider::single_call(
                vec![FauxResponse {
                    text_deltas: Vec::new(),
                    thinking_deltas: Vec::new(),
                    tool_calls: vec![FauxToolCall {
                        id: "tool-call-background-cancel".into(),
                        name: "bash".into(),
                        deltas: Vec::new(),
                        final_arguments: serde_json::json!({
                            "command": "sleep 300",
                            "background": true,
                        }),
                    }],
                }],
                StopReason::ToolUse,
            ),
            FauxProvider::text_call("background task started", StopReason::Stop),
        ])),
    );
    let (mut session, _temp) = session_with_background_tool(&provider_guard).await;
    session
        .run_internal(CodingAgentOperation::Prompt(background_prompt(
            api,
            "sleep 300",
        )))
        .await
        .expect("prompt completes");
    let task_id = session.background_task_list()[0].task_id.clone();
    assert!(session.background_task_cancel(&task_id));
    let report = tokio::time::timeout(
        Duration::from_secs(2),
        session.background_task_wait(&task_id),
    )
    .await
    .expect("cancel resolves")
    .expect("wait");
    assert_eq!(report.state, CodingAgentBackgroundTaskState::Cancelled);
    session.shutdown_internal().await.expect("shutdown");
}
