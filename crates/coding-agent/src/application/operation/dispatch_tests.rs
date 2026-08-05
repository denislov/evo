//! Dispatch routing and error-fidelity guards.
//!
//! [`CodingAgentSession::run_internal`] selects a dispatcher purely from
//! `descriptor.dispatch_mode`. These tests drive a real session through all
//! three dispatch families and assert that each one both reaches its runner and
//! surfaces the runner's own error rather than masking it as a routing failure.
//! CAG-311 removes the defensive error arms from the handlers; these tests are
//! what prove the removal changed nothing observable.
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use agent_core::api::agent::AgentResources;
use ai::api::provider::faux::{FauxProvider, FauxResponse, FauxToolCall};
use ai_protocol::api::conversation::StopReason;
use ai_protocol::api::model::{Model, ModelCost, ModelInput};
use tool_contract::api::definition::{
    AuthorizationRisk, ToolBehaviorVersion, ToolCapabilities, ToolDefinition, ToolExecutionMode,
    ToolId, ToolKind,
};
use tool_contract::api::output::{ToolContent, ToolOutput};
use tool_runtime::api::{DynamicTool, FunctionTool, ToolFuture};

use super::OperationDispatchMode;
use super::contract::{CodingAgentOperation, CodingAgentOperationOutcome};
use crate::app::bootstrap::{PromptInvocation, SessionRunOptions};
use crate::app::prompt_runtime::PromptRuntimeOptions;
use crate::authorization::{ToolAuthorizationDecision, ToolAuthorizationMode};
use crate::kernel::error::CodingSessionError;
use crate::operations::prompt::context::PromptTurnOptions;
use crate::operations::self_healing_edit::runner::{
    SelfHealingEditReplacement, SelfHealingEditRequest,
};
use crate::runtime::facade::{
    CodingAgentClientId, CodingAgentSession, CodingAgentSessionOptions, CodingAgentShutdownOutcome,
};
use crate::test_support::{ProcessFixture, ProviderGuard};

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

fn prompt_options(api: &str, prompt: &str) -> PromptTurnOptions {
    prompt_options_with_tools(api, prompt, Vec::new())
}

fn prompt_options_with_tools(
    api: &str,
    prompt: &str,
    tools: Vec<Arc<dyn DynamicTool>>,
) -> PromptTurnOptions {
    PromptTurnOptions::from_prompt_runtime_options(PromptRuntimeOptions {
        model: model(api),
        api_key: None,
        auth_diagnostics: Vec::new(),
        system_prompt: Some("system".into()),
        max_turns: Some(2),
        tools,
        register_builtins: false,
        ai_client: None,
        session: Some(SessionRunOptions::disabled(".".into())),
        session_target: None,
        session_name: None,
        thinking_level: None,
        tool_execution: None,
        resources: AgentResources::default(),
        settings: None,
        invocation: PromptInvocation::Text(prompt.into()),
    })
}

fn side_effect_tool(executed: Arc<AtomicBool>) -> Arc<dyn DynamicTool> {
    let definition = ToolDefinition {
        id: ToolId::new("authorized_side_effect").unwrap(),
        kind: ToolKind::Function,
        description: "Exercise tool authorization.".into(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {},
            "additionalProperties": false
        }),
        capabilities: ToolCapabilities {
            read_only: false,
            execution: ToolExecutionMode::Parallel,
            cancel: false,
            timeout: false,
            streaming: false,
            provider_executed: false,
        },
        behavior: ToolBehaviorVersion::V1,
        authorization_risk: AuthorizationRisk::SideEffect,
        requirements: Vec::new(),
    };
    FunctionTool::new(definition, move |_context, _arguments| {
        let executed = executed.clone();
        Box::pin(async move {
            executed.store(true, Ordering::Release);
            Ok(ToolOutput {
                content: vec![ToolContent::Text {
                    text: "tool result".into(),
                }],
                details: None,
                terminate: false,
            })
        }) as ToolFuture
    })
}

/// A complete persistent prompt, including an interactive tool authorization
/// round-trip and durable terminal commit, must make progress on a one-thread
/// Tokio scheduler. This guards against reintroducing blocking writer replies
/// anywhere in the prompt or authorization hot path.
#[tokio::test(flavor = "current_thread")]
async fn persistent_prompt_with_tool_authorization_completes_on_current_thread_runtime() {
    let api = "coding-session-current-thread-authorization";
    let provider_guard = ProviderGuard::register(
        api,
        Arc::new(FauxProvider::with_call_queue(vec![
            FauxProvider::single_call(
                vec![FauxResponse {
                    text_deltas: Vec::new(),
                    thinking_deltas: Vec::new(),
                    tool_calls: vec![FauxToolCall {
                        id: "tool-call-current-thread".into(),
                        name: "authorized_side_effect".into(),
                        deltas: vec!["{}".into()],
                        final_arguments: serde_json::json!({}),
                    }],
                }],
                StopReason::ToolUse,
            ),
            FauxProvider::text_call("authorized tool completed", StopReason::Stop),
        ])),
    );
    let tool_executed = Arc::new(AtomicBool::new(false));
    let tool = side_effect_tool(tool_executed.clone());
    let temp = tempfile::tempdir().unwrap();
    let mut session = CodingAgentSession::create_internal(
        CodingAgentSessionOptions::new()
            .with_ai_client(provider_guard.ai_client())
            .with_tool_authorization_mode(ToolAuthorizationMode::Ask)
            .with_session_id("sess_current_thread_authorization")
            .with_session_log_root(temp.path()),
    )
    .await
    .unwrap();
    let connection = session
        .connect_internal(CodingAgentClientId::new("current-thread-test"))
        .unwrap();
    let operation = CodingAgentOperation::Prompt(prompt_options_with_tools(
        api,
        "run the authorized tool",
        vec![tool],
    ));

    let prompt = session.run_internal(operation);
    let authorize = async {
        let request = tokio::time::timeout(std::time::Duration::from_secs(3), async {
            loop {
                if let Some(request) = connection
                    .pending_tool_authorizations()
                    .expect("authorization snapshot")
                    .into_iter()
                    .next()
                {
                    break request;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("prompt should publish an authorization request");
        connection
            .decide_tool_authorization(&request.identity(), ToolAuthorizationDecision::AllowOnce)
            .await
            .expect("authorization decision should persist asynchronously");
    };

    let (outcome, ()) = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        tokio::join!(prompt, authorize)
    })
    .await
    .expect("prompt and authorization should not block the current-thread runtime");
    let outcome = outcome.expect("authorized prompt should complete");
    assert!(matches!(outcome, CodingAgentOperationOutcome::Prompt(_)));
    assert!(tool_executed.load(Ordering::Acquire));
    assert!(
        connection
            .pending_tool_authorizations()
            .expect("final authorization snapshot")
            .is_empty()
    );
}

/// Yolo mode auto-approves every risky tool: the side-effecting tool executes
/// directly and no pending authorization request is ever published.
#[tokio::test]
async fn yolo_mode_auto_approves_side_effecting_tools() {
    let api = "coding-session-yolo-mode";
    let provider_guard = ProviderGuard::register(
        api,
        Arc::new(FauxProvider::with_call_queue(vec![
            FauxProvider::single_call(
                vec![FauxResponse {
                    text_deltas: Vec::new(),
                    thinking_deltas: Vec::new(),
                    tool_calls: vec![FauxToolCall {
                        id: "tool-call-yolo".into(),
                        name: "authorized_side_effect".into(),
                        deltas: vec!["{}".into()],
                        final_arguments: serde_json::json!({}),
                    }],
                }],
                StopReason::ToolUse,
            ),
            FauxProvider::text_call("tool ran without a prompt", StopReason::Stop),
        ])),
    );
    let tool_executed = Arc::new(AtomicBool::new(false));
    let tool = side_effect_tool(tool_executed.clone());
    let temp = tempfile::tempdir().unwrap();
    let mut session = CodingAgentSession::create_internal(
        CodingAgentSessionOptions::new()
            .with_ai_client(provider_guard.ai_client())
            .with_tool_authorization_mode(ToolAuthorizationMode::Yolo)
            .with_session_id("sess_yolo_mode")
            .with_session_log_root(temp.path()),
    )
    .await
    .unwrap();
    let connection = session
        .connect_internal(CodingAgentClientId::new("yolo-test"))
        .unwrap();
    let operation = CodingAgentOperation::Prompt(prompt_options_with_tools(
        api,
        "run the authorized tool",
        vec![tool],
    ));

    let outcome = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        session.run_internal(operation),
    )
    .await
    .expect("yolo prompt should complete")
    .expect("yolo prompt outcome");
    assert!(matches!(outcome, CodingAgentOperationOutcome::Prompt(_)));
    assert!(
        tool_executed.load(Ordering::Acquire),
        "yolo mode must execute the tool without prompting"
    );
    assert!(
        connection
            .pending_tool_authorizations()
            .expect("final authorization snapshot")
            .is_empty(),
        "yolo mode must never publish a pending authorization"
    );
}

/// Plan mode is read-only: a side-effecting tool is denied without prompting
/// and the model receives the read-only reason instead of executing it.
#[tokio::test]
async fn plan_mode_denies_mutating_tools_without_prompting() {
    let api = "coding-session-plan-mode";
    let provider_guard = ProviderGuard::register(
        api,
        Arc::new(FauxProvider::with_call_queue(vec![
            FauxProvider::single_call(
                vec![FauxResponse {
                    text_deltas: Vec::new(),
                    thinking_deltas: Vec::new(),
                    tool_calls: vec![FauxToolCall {
                        id: "tool-call-plan".into(),
                        name: "authorized_side_effect".into(),
                        deltas: vec!["{}".into()],
                        final_arguments: serde_json::json!({}),
                    }],
                }],
                StopReason::ToolUse,
            ),
            FauxProvider::text_call("mutating tool was refused", StopReason::Stop),
        ])),
    );
    let tool_executed = Arc::new(AtomicBool::new(false));
    let tool = side_effect_tool(tool_executed.clone());
    let temp = tempfile::tempdir().unwrap();
    let mut session = CodingAgentSession::create_internal(
        CodingAgentSessionOptions::new()
            .with_ai_client(provider_guard.ai_client())
            .with_tool_authorization_mode(ToolAuthorizationMode::Plan)
            .with_session_id("sess_plan_mode")
            .with_session_log_root(temp.path()),
    )
    .await
    .unwrap();
    let connection = session
        .connect_internal(CodingAgentClientId::new("plan-test"))
        .unwrap();
    let operation = CodingAgentOperation::Prompt(prompt_options_with_tools(
        api,
        "run the authorized tool",
        vec![tool],
    ));

    let outcome = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        session.run_internal(operation),
    )
    .await
    .expect("plan prompt should complete")
    .expect("plan prompt outcome");
    assert!(matches!(outcome, CodingAgentOperationOutcome::Prompt(_)));
    assert!(
        !tool_executed.load(Ordering::Acquire),
        "plan mode must deny mutating tools"
    );
    assert!(
        connection
            .pending_tool_authorizations()
            .expect("final authorization snapshot")
            .is_empty(),
        "plan mode must deny without prompting"
    );
}

/// Drives one operation from each dispatch family through the real router on a
/// real persistent session. Every family must reach its runner and produce its
/// own outcome variant.
#[tokio::test]
async fn run_internal_routes_every_dispatch_family_to_its_runner() {
    let api = "coding-session-dispatch-family-routing";
    let provider_guard = ProviderGuard::register(
        api,
        Arc::new(FauxProvider::with_call_queue(vec![
            FauxProvider::text_call("async answer", StopReason::Stop),
        ])),
    );
    let temp = tempfile::tempdir().unwrap();
    let mut session = CodingAgentSession::create_internal(
        CodingAgentSessionOptions::new()
            .with_ai_client(provider_guard.ai_client())
            .with_session_id("sess_dispatch_family_routing")
            .with_session_log_root(temp.path()),
    )
    .await
    .unwrap();

    // Async family.
    let async_operation = CodingAgentOperation::Prompt(prompt_options(api, "async prompt"));
    assert_eq!(
        async_operation.descriptor().dispatch_mode,
        OperationDispatchMode::Async
    );
    let async_outcome = session.run_internal(async_operation).await.unwrap();
    assert!(
        matches!(async_outcome, CodingAgentOperationOutcome::Prompt(_)),
        "async family must produce a prompt outcome, got {async_outcome:?}"
    );

    // Sync read-only family.
    let read_only_operation = CodingAgentOperation::ExportCurrent;
    assert_eq!(
        read_only_operation.descriptor().dispatch_mode,
        OperationDispatchMode::SyncReadOnly
    );
    let read_only_outcome = session.run_internal(read_only_operation).await.unwrap();
    assert!(
        matches!(read_only_outcome, CodingAgentOperationOutcome::Export(_)),
        "sync read-only family must produce an export outcome, got {read_only_outcome:?}"
    );

    // Sync mutable family.
    let sync_mut_operation = CodingAgentOperation::SetSessionName {
        name: Some("routed".into()),
    };
    assert_eq!(
        sync_mut_operation.descriptor().dispatch_mode,
        OperationDispatchMode::SyncMutable
    );
    let sync_mut_outcome = session.run_internal(sync_mut_operation).await.unwrap();
    assert!(
        matches!(
            sync_mut_outcome,
            CodingAgentOperationOutcome::SessionNameChanged { .. }
        ),
        "sync mutable family must produce a session-name outcome, got {sync_mut_outcome:?}"
    );
}

/// A sync-mutable operation that its runner rejects must surface the runner's
/// own error. The message must stay the session-writer capability message, not
/// become a dispatcher-selection error.
#[tokio::test]
async fn sync_mutable_runner_errors_are_not_masked_as_routing_failures() {
    let mut session = CodingAgentSession::non_persistent_internal(CodingAgentSessionOptions::new())
        .await
        .unwrap();

    let error = session
        .run_internal(CodingAgentOperation::SetSessionName {
            name: Some("no durable session".into()),
        })
        .await
        .expect_err("a non-persistent session cannot commit a durable name change");

    let CodingSessionError::UnsupportedCapability { capability } = &error else {
        panic!("SetSessionName must fail on the missing session writer, got {error:?}");
    };
    assert_eq!(
        capability, "session names require a persistent Rust-native session",
        "the sync-mutable handler must surface the runner's capability error"
    );
    assert_dispatch_error_is_not_a_routing_fallback(&error, "SetSessionName");
}

/// Same guarantee for the async family: a compact request whose invocation the
/// runner rejects must preserve the runner's typed input error.
#[tokio::test]
async fn async_runner_errors_are_not_masked_as_routing_failures() {
    let mut session = CodingAgentSession::non_persistent_internal(CodingAgentSessionOptions::new())
        .await
        .unwrap();

    let error = session
        .run_internal(CodingAgentOperation::Compact(PromptTurnOptions::new(
            PromptInvocation::Text("compact without a compaction invocation".into()),
        )))
        .await
        .expect_err("compact requires a compaction invocation");

    let CodingSessionError::Input { message } = &error else {
        panic!("Compact must fail on its own input validation, got {error:?}");
    };
    assert_eq!(
        message, "compact operation requires a compaction invocation",
        "the async handler must surface the runner's input error"
    );
    assert_dispatch_error_is_not_a_routing_fallback(&error, "Compact");
}

/// A check command runs after the edit's atomic mutation section. Runtime
/// shutdown must still cancel that process, drain the active operation, and
/// finish rather than waiting for the check's normal timeout.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn self_healing_check_cancellation_allows_runtime_shutdown_to_finish() {
    let temp = tempfile::tempdir().unwrap();
    let workspace = temp.path().join("workspace");
    tokio::fs::create_dir(&workspace).await.unwrap();
    tokio::fs::write(workspace.join("target.txt"), "before\n")
        .await
        .unwrap();
    let mut session = CodingAgentSession::create_internal(
        CodingAgentSessionOptions::new()
            .with_cwd(&workspace)
            .with_session_id("sess_self_healing_shutdown")
            .with_session_log_root(temp.path().join("sessions")),
    )
    .await
    .unwrap();
    let snapshots = session.runtime_host.client_projection.snapshots.clone();
    let operation_control = session.runtime_host.operation_supervisor.control.clone();
    let fixture = ProcessFixture::new().expect("process fixture");
    let pid_file = fixture.pid_file().to_path_buf();
    let operation = CodingAgentOperation::SelfHealingEdit(
        SelfHealingEditRequest::new(
            "target.txt",
            vec![SelfHealingEditReplacement::new("before", "after")],
        )
        .with_check_command(fixture.descendant_command()),
    );
    let operation_task = tokio::spawn(async move {
        let outcome = session.run_internal(operation).await;
        (session, outcome)
    });

    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            if tokio::fs::try_exists(&pid_file).await.unwrap_or(false) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("check command should start");

    snapshots
        .request_shutdown()
        .expect("snapshot shutdown request should succeed");
    let cancelled = operation_control
        .cancel_open_operations_for_shutdown()
        .expect("shutdown cancellation should succeed");
    assert_eq!(cancelled.len(), 1);
    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        snapshots.wait_for_active_operation_to_drain(),
    )
    .await
    .expect("shutdown should drain the cancelled check operation")
    .expect("shutdown drain should not encounter a resource error");
    let (mut session, outcome) =
        tokio::time::timeout(std::time::Duration::from_secs(2), operation_task)
            .await
            .expect("cancelled operation task should return")
            .expect("operation task should join");
    assert!(matches!(outcome, Err(CodingSessionError::Cancelled)));
    let shutdown = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        session.shutdown_internal(),
    )
    .await
    .expect("runtime shutdown should return")
    .expect("runtime shutdown should succeed");
    assert_eq!(shutdown, CodingAgentShutdownOutcome::ShutDown);
}

/// The removed routing fallback used the phrase "requires ... dispatcher".
/// Any such error means the runner's own error was masked during dispatch.
fn assert_dispatch_error_is_not_a_routing_fallback(error: &CodingSessionError, name: &str) {
    if let CodingSessionError::UnsupportedCapability { capability } = error {
        assert!(
            !capability.contains("dispatcher"),
            "{name}: router sent the operation to the wrong dispatcher: {capability}"
        );
    }
}
