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
    CodingAgentClientId, CodingAgentDraft, CodingAgentDraftId, CodingAgentDraftKind,
    CodingAgentSession, CodingAgentSessionOptions, CodingAgentShutdownOutcome,
};
use crate::session::replay::TranscriptItem;
use crate::session::service::SessionPersistence;
use crate::test_support::{ProcessFixture, ProviderGuard};
use crate::workspace::CodingAgentWorkspaceSelection;

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

#[tokio::test]
async fn durable_rewind_restores_workspace_tracker_branch_and_client_state() {
    let api = "coding-session-durable-rewind";
    let provider_guard = ProviderGuard::register(
        api,
        Arc::new(FauxProvider::with_call_queue(vec![
            FauxProvider::text_call("checkpoint answer", StopReason::Stop),
            FauxProvider::text_call("excluded answer", StopReason::Stop),
            FauxProvider::text_call("continued answer", StopReason::Stop),
        ])),
    );
    let temp = tempfile::tempdir().unwrap();
    let resolved_workspace = CodingAgentWorkspaceSelection::projectless("rewind-workspace")
        .resolve(temp.path().join("config"))
        .unwrap();
    let workspace = resolved_workspace.execution_cwd.clone();
    std::fs::write(workspace.join("tracked.txt"), "base\n").unwrap();
    std::fs::write(workspace.join("first-edit.txt"), "checkpoint value\n").unwrap();
    std::fs::write(workspace.join("deleted.txt"), "restore me\n").unwrap();
    let session_root = temp.path().join("sessions");
    let options = CodingAgentSessionOptions::new()
        .with_ai_client(provider_guard.ai_client())
        .with_resolved_workspace(resolved_workspace)
        .with_session_id("sess_durable_rewind")
        .with_session_log_root(&session_root);
    let mut session = CodingAgentSession::create_internal(options.clone())
        .await
        .unwrap();
    let old_connection = session
        .connect_internal(CodingAgentClientId::new("rewind-client"))
        .unwrap();

    session
        .run_internal(CodingAgentOperation::Prompt(prompt_options(
            api,
            "checkpoint prompt",
        )))
        .await
        .unwrap();
    record_tracked_edit(
        &session,
        &workspace,
        "tracked.txt",
        "base",
        "checkpoint state",
    )
    .await;
    let outcome = session
        .run_internal(CodingAgentOperation::CreateRewindCheckpoint)
        .await
        .unwrap();
    let CodingAgentOperationOutcome::RewindCheckpointCreated {
        checkpoint_id,
        branch_id: source_branch_id,
        session_sequence: checkpoint_sequence,
        ..
    } = outcome
    else {
        panic!("checkpoint operation returned an unexpected outcome: {outcome:?}");
    };
    let checkpoint_tracker = session.runtime_host.review_service.latest().unwrap();
    let checkpoint_hunk_id = checkpoint_tracker
        .files
        .iter()
        .find(|file| file.path == std::path::Path::new("tracked.txt"))
        .unwrap_or_else(|| panic!("tracked change missing: {checkpoint_tracker:?}"))
        .hunks[0]
        .id
        .clone();
    let sidecar = session_root
        .join("sess_durable_rewind/rewind")
        .join(format!("{checkpoint_id}.json"));
    assert!(sidecar.is_file());

    record_tracked_edit(
        &session,
        &workspace,
        "tracked.txt",
        "checkpoint state",
        "after checkpoint",
    )
    .await;
    record_tracked_edit(
        &session,
        &workspace,
        "first-edit.txt",
        "checkpoint value",
        "changed later",
    )
    .await;
    std::fs::write(workspace.join("created.txt"), "remove me\n").unwrap();
    std::fs::remove_file(workspace.join("deleted.txt")).unwrap();
    session
        .run_internal(CodingAgentOperation::Prompt(prompt_options(
            api,
            "excluded prompt",
        )))
        .await
        .unwrap();

    old_connection
        .set_prompt_draft_internal(CodingAgentDraftId("prompt-draft".into()), "draft prompt")
        .unwrap();
    for (id, kind) in [
        ("steer-draft", CodingAgentDraftKind::Steer),
        ("follow-up-draft", CodingAgentDraftKind::FollowUp),
    ] {
        old_connection
            .enqueue_control_draft(CodingAgentDraft {
                id: CodingAgentDraftId(id.into()),
                kind,
                text: id.into(),
            })
            .unwrap();
    }
    let old_state = old_connection.state_internal().unwrap();
    assert_eq!(old_state.drafts.len(), 3);

    let before_failed_rewind = session.runtime_host.review_service.latest().unwrap();
    let event_log = session_root.join("sess_durable_rewind/events.jsonl");
    let event_log_backup = session_root.join("sess_durable_rewind/events.backup");
    std::fs::rename(&event_log, &event_log_backup).unwrap();
    std::fs::create_dir(&event_log).unwrap();
    let failed_rewind = session
        .run_internal(CodingAgentOperation::Rewind {
            checkpoint_id: checkpoint_id.clone(),
        })
        .await;
    std::fs::remove_dir(&event_log).unwrap();
    std::fs::rename(&event_log_backup, &event_log).unwrap();
    assert!(failed_rewind.is_err());
    assert_eq!(
        std::fs::read(workspace.join("tracked.txt")).unwrap(),
        b"after checkpoint\n"
    );
    assert_eq!(
        std::fs::read(workspace.join("first-edit.txt")).unwrap(),
        b"changed later\n"
    );
    assert!(workspace.join("created.txt").is_file());
    assert!(!workspace.join("deleted.txt").exists());
    assert_eq!(
        session.runtime_host.review_service.latest().unwrap(),
        before_failed_rewind
    );

    let outcome = session
        .run_internal(CodingAgentOperation::Rewind {
            checkpoint_id: checkpoint_id.clone(),
        })
        .await
        .unwrap();
    let CodingAgentOperationOutcome::Rewound { new_branch_id, .. } = outcome else {
        panic!("rewind operation returned an unexpected outcome: {outcome:?}");
    };
    assert_ne!(new_branch_id, source_branch_id);
    assert_eq!(
        std::fs::read(workspace.join("tracked.txt")).unwrap(),
        b"checkpoint state\n"
    );
    assert_eq!(
        std::fs::read(workspace.join("first-edit.txt")).unwrap(),
        b"checkpoint value\n"
    );
    assert_eq!(
        std::fs::read(workspace.join("deleted.txt")).unwrap(),
        b"restore me\n"
    );
    assert!(!workspace.join("created.txt").exists());
    let restored_tracker = session.runtime_host.review_service.latest().unwrap();
    assert_eq!(restored_tracker, checkpoint_tracker);
    assert_eq!(restored_tracker.files[0].hunks[0].id, checkpoint_hunk_id);

    assert!(matches!(
        old_connection.state_internal(),
        Err(CodingSessionError::Lifecycle {
            reason: crate::kernel::error::CodingAgentLifecycleRejection::StaleGeneration
        })
    ));
    let new_connection = session
        .connect_internal(CodingAgentClientId::new("rewind-client"))
        .unwrap();
    let reset_state = new_connection.state_internal().unwrap();
    assert_ne!(reset_state.cursor.stream_id, old_state.cursor.stream_id);
    assert!(reset_state.cursor.capability_generation > old_state.cursor.capability_generation);
    assert_eq!(reset_state.cursor.last_event_sequence, 0);
    assert_eq!(
        reset_state.cursor.last_session_sequence,
        checkpoint_sequence
    );
    assert!(reset_state.drafts.is_empty());
    assert!(matches!(
        new_connection.reconnect_from_cursor_internal(&old_state.cursor),
        Err(CodingSessionError::Input { .. })
    ));

    session
        .run_internal(CodingAgentOperation::Prompt(prompt_options(
            api,
            "continued prompt",
        )))
        .await
        .unwrap();
    let (active_inputs, source_inputs) = match &session.runtime_host.session_coordinator.persistence
    {
        SessionPersistence::Persistent(service) => (
            user_inputs(service.replay().unwrap().transcript),
            user_inputs(service.replay_branch(&source_branch_id).unwrap().transcript),
        ),
        SessionPersistence::NonPersistent(_) => panic!("rewind test requires persistence"),
    };
    assert_eq!(active_inputs, ["checkpoint prompt", "continued prompt"]);
    assert_eq!(source_inputs, ["checkpoint prompt", "excluded prompt"]);

    session.shutdown_internal().await.unwrap();
    drop(session);
    let export = temp.path().join("source-branch.html");
    CodingAgentSession::export_session_branch_html_internal(
        options.clone(),
        &source_branch_id,
        &export,
    )
    .unwrap();
    let exported = std::fs::read_to_string(export).unwrap();
    assert!(exported.contains("excluded prompt"));
    assert!(!exported.contains("continued prompt"));

    let sidecar_bytes = std::fs::read(&sidecar).unwrap();
    std::fs::remove_file(&sidecar).unwrap();
    assert!(
        CodingAgentSession::open_internal(options.clone())
            .await
            .is_err()
    );
    std::fs::write(&sidecar, b"not-json").unwrap();
    assert!(
        CodingAgentSession::open_internal(options.clone())
            .await
            .is_err()
    );
    let mut wrong_owner: serde_json::Value = serde_json::from_slice(&sidecar_bytes).unwrap();
    wrong_owner["session_id"] = serde_json::Value::String("another-session".into());
    std::fs::write(&sidecar, serde_json::to_vec(&wrong_owner).unwrap()).unwrap();
    assert!(
        CodingAgentSession::open_internal(options.clone())
            .await
            .is_err()
    );
    std::fs::write(&sidecar, &sidecar_bytes).unwrap();
    let orphan = sidecar.parent().unwrap().join(".orphan.tmp-123");
    std::fs::write(&orphan, "orphan").unwrap();

    let mut reopened = CodingAgentSession::open_internal(options.clone())
        .await
        .unwrap();
    assert!(!orphan.exists());
    assert_eq!(
        reopened.runtime_host.review_service.latest().unwrap(),
        checkpoint_tracker
    );
    reopened.shutdown_internal().await.unwrap();
    drop(reopened);

    std::fs::write(workspace.join("tracked.txt"), "external after shutdown\n").unwrap();
    let error = CodingAgentSession::open_internal(options)
        .await
        .expect_err("stale workspace must reject startup rewind restoration");
    assert!(matches!(error, CodingSessionError::Stale { .. }));
    assert_eq!(
        std::fs::read(workspace.join("tracked.txt")).unwrap(),
        b"external after shutdown\n"
    );
}

#[tokio::test]
async fn source_workspace_rejects_rewind_checkpoint_before_sidecar_creation() {
    let temp = tempfile::tempdir().unwrap();
    let workspace = temp.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    std::fs::write(workspace.join("notes.txt"), "do not rewind\n").unwrap();
    let session_root = temp.path().join("sessions");
    let mut session = CodingAgentSession::create_internal(
        CodingAgentSessionOptions::new()
            .with_cwd(&workspace)
            .with_session_id("sess_source_rewind")
            .with_session_log_root(&session_root),
    )
    .await
    .unwrap();

    let error = session
        .run_internal(CodingAgentOperation::CreateRewindCheckpoint)
        .await
        .expect_err("source workspace rewind checkpoint must be rejected");
    assert!(matches!(
        error,
        CodingSessionError::UnsupportedCapability { ref capability }
            if capability.contains("Source")
    ));
    assert!(!session_root.join("sess_source_rewind/rewind").exists());
    assert_eq!(
        std::fs::read(workspace.join("notes.txt")).unwrap(),
        b"do not rewind\n"
    );
    session.shutdown_internal().await.unwrap();
}

async fn record_tracked_edit(
    session: &CodingAgentSession,
    workspace: &std::path::Path,
    path: &str,
    old: &str,
    new: &str,
) {
    let before = format!("{old}\n").into_bytes();
    let after = format!("{new}\n").into_bytes();
    let tracking = session
        .runtime_host
        .review_service
        .mutation_tracking(
            "sess_durable_rewind",
            format!("turn-{path}"),
            format!("operation-{path}"),
        )
        .unwrap();
    std::fs::write(workspace.join(path), &after).unwrap();
    tracking
        .record(
            &format!("tool-{path}"),
            crate::tools::filesystem::mutation_receipt::receipt(
                path.into(),
                format!("target-{path}"),
                Some(&before),
                Some(&after),
                "edit",
                Some(format!("@@ -1,1 +1,1 @@\n-{old}\n+{new}\n")),
            ),
        )
        .await
        .unwrap();
}

fn user_inputs(transcript: Vec<TranscriptItem>) -> Vec<String> {
    transcript
        .into_iter()
        .filter_map(|item| match item {
            TranscriptItem::UserInput { text, .. } => Some(text),
            _ => None,
        })
        .collect()
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

/// After shutdown completes, the runtime must reject any new operation at
/// admission. `run_internal` checks `ensure_runtime_running` before dispatch,
/// so a post-shutdown operation surfaces a structured lifecycle rejection
/// rather than reaching a runner.
#[tokio::test]
async fn shutdown_rejects_new_operations_after_completion() {
    let mut session = CodingAgentSession::non_persistent_internal(CodingAgentSessionOptions::new())
        .await
        .unwrap();
    session.shutdown_internal().await.unwrap();
    let error = session
        .run_internal(CodingAgentOperation::SetSessionName { name: None })
        .await
        .expect_err("operation after shutdown must be rejected");
    assert!(matches!(
        error,
        CodingSessionError::Lifecycle {
            reason: crate::kernel::error::CodingAgentLifecycleRejection::RuntimeShutDown
        }
    ));
}

/// A control command enqueued after shutdown must be rejected with a
/// `RuntimeShutDown` reason. The client connection was attached before
/// shutdown, but `finish_shutdown` advances the runtime lifecycle to
/// `ShutDown`, so `enqueue_control` fails at `validate_runtime`.
#[tokio::test]
async fn shutdown_rejects_control_commands_after_completion() {
    use crate::runtime::facade::{CodingAgentControlId, CodingAgentControlRejectionReason};
    let mut session = CodingAgentSession::non_persistent_internal(CodingAgentSessionOptions::new())
        .await
        .unwrap();
    let connection = session
        .connect_internal(CodingAgentClientId::new("post-shutdown-control"))
        .unwrap();
    let control = connection.prompt_control("op-post-shutdown-control");
    session.shutdown_internal().await.unwrap();
    let rejection = control
        .steer(
            CodingAgentControlId("steer-after-shutdown".into()),
            "steer after shutdown",
        )
        .expect_err("control command after shutdown must be rejected");
    assert_eq!(
        rejection.reason,
        CodingAgentControlRejectionReason::RuntimeShutDown
    );
}

/// `shutdown_internal` commits the terminal runtime event before draining the
/// writer. A subscriber that registered before shutdown must observe the
/// `Runtime::ShutDown` product event, proving `emit_runtime_shutdown` ran
/// ahead of `shutdown_writer` and `finish_shutdown`.
#[tokio::test]
async fn shutdown_emits_terminal_event_before_draining_writer() {
    use crate::runtime::facade::{CodingAgentProductEventKind, CodingAgentRuntimeProductEvent};
    let mut session = CodingAgentSession::non_persistent_internal(CodingAgentSessionOptions::new())
        .await
        .unwrap();
    let mut receiver = session.subscribe_product_events().unwrap();
    let outcome = session.shutdown_internal().await.unwrap();
    assert_eq!(outcome, CodingAgentShutdownOutcome::ShutDown);
    let mut saw_shutdown = false;
    loop {
        match receiver.try_recv() {
            Ok(Some(event)) => {
                if matches!(
                    event.event(),
                    CodingAgentProductEventKind::Runtime(CodingAgentRuntimeProductEvent::ShutDown)
                ) {
                    saw_shutdown = true;
                    break;
                }
            }
            Ok(None) => break,
            Err(CodingSessionError::Cancelled) => break,
            Err(_) => break,
        }
    }
    assert!(
        saw_shutdown,
        "runtime shutdown terminal event must be emitted to product event subscribers"
    );
}
