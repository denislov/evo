#![allow(deprecated)]

use crate::internal_tests::support;

use std::fs;
use std::path::Path;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

use agent_core::api::tool::AgentTool;
use ai::api::conversation::{AssistantMessage, ContentBlock, Context, Message, StopReason};
use ai::api::model::Model;
use ai::api::provider::ApiProvider;
use ai::api::stream::{AssistantMessageEvent, EventStream, StreamOptions};
use async_stream::stream;
use coding_agent::api::client::{CodingAgentClientId, CodingAgentOperationStatus};
use coding_agent::api::event::{CodingAgentProductEvent, CodingAgentProductEventReceiver};
use coding_agent::api::operation::{
    CodingAgentOperation, CodingAgentOperationOutcome, PromptTurnOptions, PromptTurnOutcome,
};
use coding_agent::api::runtime::{CodingAgentSession, CodingAgentSessionOptions};
use coding_agent::api::view::CodingAgentSessionExportItem;
use support::{EnvGuard, ProviderGuard as RegistryProviderGuard};
use tempfile::tempdir;

const OPERATION_TREE_ABORT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

#[tokio::test]
async fn parent_abort_drops_stalled_child_stream_and_prevents_late_continuation() {
    let temp = tempdir().unwrap();
    let cwd = temp.path().join("workspace");
    let global = temp.path().join("global");
    fs::create_dir_all(&global).unwrap();
    write_file(
        cwd.join(".evo/agents/delegating-planner.toml"),
        r#"
schema_version = 1
id = "delegating-planner"
display_name = "Delegating Planner"

[delegation]
allow_delegate_agent = true
max_depth = 1
max_parallel_children = 1
require_confirmation = "never"
allowed_agents = ["coder"]
"#,
    );
    write_file(
        cwd.join(".evo/agents/coder.toml"),
        r#"
schema_version = 1
id = "coder"
display_name = "Coder"
"#,
    );
    let _env_guard = EnvGuard::with_evo_dir(global);

    let api = "delegation-stalled-child-abort-api";
    let provider = Arc::new(StalledChildProvider::default());
    let _provider_guard = RegistryProviderGuard::register(api, provider.clone());
    let mut session = CodingAgentSession::non_persistent(
        CodingAgentSessionOptions::new()
            .with_ai_client(_provider_guard.ai_client())
            .with_cwd(&cwd)
            .with_default_agent_profile_id("delegating-planner"),
    )
    .await
    .unwrap();
    let capability_control = session.capability_control();
    let run_cwd = cwd.clone();
    let running = tokio::spawn(async move {
        let outcome = session
            .run(CodingAgentOperation::Prompt(prompt_options(
                &run_cwd,
                api,
                "delegate stalled work",
            )))
            .await;
        (session, outcome)
    });

    tokio::time::timeout(
        OPERATION_TREE_ABORT_TIMEOUT,
        provider.child_started.notified(),
    )
    .await
    .expect("delegated child provider did not start");
    let cancellation_started = std::time::Instant::now();
    let revocation = capability_control.revoke_older_operations().unwrap();
    assert!(
        !revocation.cancellation_requested_operation_ids.is_empty(),
        "capability revocation must reach the running operation tree"
    );

    let (session, outcome) = tokio::time::timeout(OPERATION_TREE_ABORT_TIMEOUT, running)
        .await
        .expect("parent Prompt did not converge after abort")
        .unwrap();
    assert!(matches!(
        outcome,
        Ok(CodingAgentOperationOutcome::Prompt(
            PromptTurnOutcome::Aborted { .. }
        ))
    ));
    assert!(provider.child_stream_dropped.load(Ordering::SeqCst));
    assert_eq!(
        provider.calls.load(Ordering::SeqCst),
        2,
        "parent must not issue a continuation after cancellation wins"
    );
    assert!(session.snapshot().active_operation.is_none());
    let snapshot = session
        .connect(CodingAgentClientId::new("delegation-abort-terminal-check"))
        .unwrap()
        .state()
        .unwrap();
    assert!(
        snapshot
            .context
            .operations
            .iter()
            .all(|operation| operation.status != CodingAgentOperationStatus::Running),
        "aborted delegation must terminalize every projected child operation: {:#?}",
        snapshot.context.operations
    );
    println!(
        "operation_tree_baseline\tcase=stalled_child_cancellation\telapsed_us={}",
        cancellation_started.elapsed().as_micros()
    );
}

#[tokio::test]
async fn child_partial_then_error_stays_child_scoped_and_parent_receives_one_failure() {
    let temp = tempdir().unwrap();
    let cwd = temp.path().join("workspace");
    let global = temp.path().join("global");
    fs::create_dir_all(&global).unwrap();
    write_file(
        cwd.join(".evo/agents/delegating-planner.toml"),
        r#"
schema_version = 1
id = "delegating-planner"
display_name = "Delegating Planner"

[delegation]
allow_delegate_agent = true
max_depth = 1
max_parallel_children = 1
require_confirmation = "never"
allowed_agents = ["coder"]
"#,
    );
    write_file(
        cwd.join(".evo/agents/coder.toml"),
        r#"
schema_version = 1
id = "coder"
display_name = "Coder"
"#,
    );
    let _env_guard = EnvGuard::with_evo_dir(global);

    let api = "delegation-child-partial-error-api";
    let calls = Arc::new(Mutex::new(Vec::new()));
    let _provider_guard = ProviderGuard::register(
        api,
        calls.clone(),
        vec![
            ScriptedResponse::tool_call(
                "tool_delegate_partial_error",
                "delegate_agent",
                serde_json::json!({"agent_id": "coder", "task": "fail after partial"}),
            ),
            ScriptedResponse::PartialThenError {
                partial: "child secret partial".into(),
                error: "child provider failed".into(),
            },
            ScriptedResponse::text("parent recovered from child failure"),
        ],
    );
    let mut session = CodingAgentSession::non_persistent(
        CodingAgentSessionOptions::new()
            .with_ai_client(_provider_guard.ai_client())
            .with_cwd(&cwd)
            .with_default_agent_profile_id("delegating-planner"),
    )
    .await
    .unwrap();
    let mut events = session.subscribe_product_events_public();

    let outcome = session
        .run(CodingAgentOperation::Prompt(prompt_options(
            &cwd,
            api,
            "delegate faulting child",
        )))
        .await
        .unwrap();
    assert_eq!(
        outcome.final_text(),
        Some("parent recovered from child failure")
    );
    let calls = calls.lock().unwrap();
    assert_eq!(calls.len(), 3);
    let parent_continuation = context_texts(&calls[2].context);
    assert!(
        parent_continuation
            .iter()
            .any(|text| text.contains("child provider failed")),
        "parent must receive the typed child failure result: {parent_continuation:#?}"
    );
    assert!(
        parent_continuation
            .iter()
            .all(|text| !text.contains("child secret partial")),
        "uncommitted child partial output leaked into parent context: {parent_continuation:#?}"
    );
    drop(calls);

    let events = drain_events(&mut events);
    assert_event_count(&events, "Delegation(Failed)", 1);
    assert_event_count(&events, "Delegation(Completed)", 0);
    assert_event_count(&events, "Delegation(Started)", 1);
    assert!(session.snapshot().active_operation.is_none());
}

#[tokio::test]
async fn prompt_executes_approved_agent_delegation_before_parent_continues() {
    let temp = tempdir().unwrap();
    let cwd = temp.path().join("workspace");
    let global = temp.path().join("global");
    fs::create_dir_all(&global).unwrap();
    write_file(
        cwd.join(".evo/agents/delegating-planner.toml"),
        r#"
schema_version = 1
id = "delegating-planner"
display_name = "Delegating Planner"

[delegation]
allow_delegate_agent = true
max_depth = 1
max_parallel_children = 1
require_confirmation = "never"
allowed_agents = ["coder"]
"#,
    );
    write_file(
        cwd.join(".evo/agents/coder.toml"),
        r#"
schema_version = 1
id = "coder"
display_name = "Coder"
system_prompt = "Coder child instructions."
"#,
    );
    let _env_guard = EnvGuard::with_evo_dir(global);

    let api = "delegation-execution-agent-api";
    let calls = Arc::new(Mutex::new(Vec::new()));
    let _provider_guard = ProviderGuard::register(
        api,
        calls.clone(),
        vec![
            ScriptedResponse::tool_call(
                "tool_delegate_agent",
                "delegate_agent",
                serde_json::json!({"agent_id": "coder", "task": "implement parser"}),
            ),
            ScriptedResponse::text("child result"),
            ScriptedResponse::text("parent ready after child"),
        ],
    );

    let mut session = CodingAgentSession::non_persistent(
        CodingAgentSessionOptions::new()
            .with_ai_client(_provider_guard.ai_client())
            .with_cwd(&cwd)
            .with_default_agent_profile_id("delegating-planner"),
    )
    .await
    .unwrap();
    let mut events = session.subscribe_product_events_public();

    let outcome = session
        .run(CodingAgentOperation::Prompt(prompt_options(
            &cwd,
            api,
            "plan feature",
        )))
        .await
        .unwrap();

    assert_eq!(outcome.final_text(), Some("parent ready after child"));
    let calls = calls.lock().unwrap();
    assert_eq!(
        calls.len(),
        3,
        "expected parent tool, parent final, and child calls"
    );
    assert_eq!(user_texts(&calls[0].context), vec!["plan feature"]);
    assert_eq!(user_texts(&calls[1].context), vec!["implement parser"]);
    assert_eq!(
        calls[1].context.system_prompt.as_deref(),
        Some("Coder child instructions.")
    );
    assert!(
        context_texts(&calls[2].context)
            .iter()
            .any(|text| text.contains("child result")),
        "parent continuation did not receive child terminal result: {:?}",
        context_texts(&calls[2].context)
    );

    let events = drain_events(&mut events);
    assert_event_count(&events, "Delegation(Requested)", 0);
    assert_event_count(&events, "Delegation(Approved)", 1);
    assert_event_count(&events, "Delegation(Started)", 1);
    assert_event_count(&events, "Delegation(Completed)", 1);
    let child_prompt_terminal = events
        .iter()
        .find(|event| {
            event.kind_name() == "prompt_completed" && event.parent_operation_id().is_some()
        })
        .expect("delegated child Prompt publishes a lineage-bound terminal event");
    assert!(
        child_prompt_terminal.terminal_operation().is_some(),
        "delegated child Prompt terminal must be published before its permit releases event context"
    );

    let snapshot = session
        .connect(CodingAgentClientId::new("delegation-terminal-check"))
        .unwrap()
        .state()
        .unwrap();
    let child_prompt = snapshot
        .context
        .operations
        .iter()
        .find(|operation| operation.kind == "prompt" && operation.parent_operation_id.is_some())
        .expect("delegated AgentInvocation exposes its internal Prompt operation");
    assert_eq!(child_prompt.status, CodingAgentOperationStatus::Completed);
    assert!(
        snapshot
            .context
            .operations
            .iter()
            .all(|operation| operation.status != CodingAgentOperationStatus::Running),
        "completed delegation must not leave a projected running operation: {:#?}",
        snapshot.context.operations
    );
}

#[tokio::test]
async fn built_in_default_profile_auto_approves_read_only_helper_delegation() {
    let temp = tempdir().unwrap();
    let cwd = temp.path().join("workspace");
    let global = temp.path().join("global");
    fs::create_dir_all(&global).unwrap();
    write_file(cwd.join("README.md"), "delegated read fixture\n");
    let _env_guard = EnvGuard::with_evo_dir(global);

    let api = "delegation-built-in-default-helper-api";
    let calls = Arc::new(Mutex::new(Vec::new()));
    let _provider_guard = ProviderGuard::register(
        api,
        calls.clone(),
        vec![
            ScriptedResponse::tool_call(
                "tool_delegate_explore",
                "delegate_agent",
                serde_json::json!({"agent_id": "explore", "task": "inspect replay"}),
            ),
            ScriptedResponse::tool_call(
                "tool_child_read",
                "read",
                serde_json::json!({"path": "README.md"}),
            ),
            ScriptedResponse::text("explore result"),
            ScriptedResponse::text("parent ready"),
        ],
    );

    let mut session = CodingAgentSession::non_persistent(
        CodingAgentSessionOptions::new()
            .with_ai_client(_provider_guard.ai_client())
            .with_cwd(&cwd),
    )
    .await
    .unwrap();
    let mut events = session.subscribe_product_events_public();

    let outcome = session
        .run(CodingAgentOperation::Prompt(prompt_options_with_tools(
            &cwd,
            api,
            "plan feature",
            vec![parent_only_tool(), read_fixture_tool(&cwd)],
        )))
        .await
        .unwrap();

    assert_eq!(outcome.final_text(), Some("parent ready"));
    let calls = calls.lock().unwrap();
    assert_eq!(
        calls.len(),
        4,
        "expected parent delegation, helper read/continuation, and parent final calls"
    );
    assert_eq!(user_texts(&calls[0].context), vec!["plan feature"]);
    assert_eq!(user_texts(&calls[1].context), vec!["inspect replay"]);
    assert_eq!(
        calls[1].context.system_prompt.as_deref(),
        Some(
            "You are a read-only exploration helper. Gather context and summarize findings without making changes."
        )
    );

    let parent_tools = tool_names(&calls[0].context);
    assert!(
        parent_tools.iter().any(|tool| tool == "delegate_agent"),
        "default parent profile should expose built-in agent delegation: {parent_tools:#?}"
    );
    assert!(
        parent_tools.iter().any(|tool| tool == "parent_only"),
        "parent should keep explicitly configured runtime tools: {parent_tools:#?}"
    );
    let helper_tools = tool_names(&calls[1].context);
    assert_eq!(helper_tools, ["read"]);
    assert!(
        !helper_tools.iter().any(|tool| tool == "parent_only"),
        "built-in helper must not inherit parent runtime tools: {helper_tools:#?}"
    );
    assert!(
        !helper_tools.iter().any(|tool| tool == "delegate_agent"),
        "built-in helper must not inherit delegation authority: {helper_tools:#?}"
    );
    assert!(
        !helper_tools
            .iter()
            .any(|tool| matches!(tool.as_str(), "write" | "edit" | "bash")),
        "built-in helper must not receive mutation or shell tools: {helper_tools:#?}"
    );
    assert!(
        context_texts(&calls[2].context)
            .iter()
            .any(|text| text.contains("delegated read fixture")),
        "the delegated helper should receive the workspace read result: {:#?}",
        context_texts(&calls[2].context)
    );
    drop(calls);

    let events = drain_events(&mut events);
    assert_event_count(&events, "Delegation(Approved)", 1);
    assert_event_count(&events, "Delegation(Started)", 1);
    assert_event_count(&events, "Delegation(Completed)", 1);
    assert_event_count(&events, "Delegation(ConfirmationRequired)", 0);
}

#[tokio::test]
async fn delegated_helper_receives_minimal_context_without_parent_transcript() {
    let temp = tempdir().unwrap();
    let cwd = temp.path().join("workspace");
    let global = temp.path().join("global");
    let sessions = temp.path().join("sessions");
    fs::create_dir_all(&cwd).unwrap();
    fs::create_dir_all(&global).unwrap();
    let _env_guard = EnvGuard::with_evo_dir(global);

    let api = "delegation-helper-minimal-context-api";
    let calls = Arc::new(Mutex::new(Vec::new()));
    let _provider_guard = ProviderGuard::register(
        api,
        calls.clone(),
        vec![
            ScriptedResponse::text("parent history answer"),
            ScriptedResponse::tool_call(
                "tool_delegate_explore",
                "delegate_agent",
                serde_json::json!({"agent_id": "explore", "task": "inspect replay"}),
            ),
            ScriptedResponse::text("explore result"),
            ScriptedResponse::text("parent ready"),
        ],
    );

    let mut session = CodingAgentSession::create(
        CodingAgentSessionOptions::new()
            .with_ai_client(_provider_guard.ai_client())
            .with_cwd(&cwd)
            .with_session_log_root(&sessions)
            .with_session_id("sess_default_helper_minimal_context"),
    )
    .await
    .unwrap();

    let first = session
        .run(CodingAgentOperation::Prompt(prompt_options(
            &cwd,
            api,
            "parent secret context",
        )))
        .await
        .unwrap();
    assert_eq!(first.final_text(), Some("parent history answer"));

    let second = session
        .run(CodingAgentOperation::Prompt(prompt_options(
            &cwd,
            api,
            "plan with prior history",
        )))
        .await
        .unwrap();
    assert_eq!(second.final_text(), Some("parent ready"));

    let calls = calls.lock().unwrap();
    assert_eq!(
        calls.len(),
        4,
        "expected first prompt, parent delegation tool/final, and helper calls"
    );
    let second_parent_texts = context_texts(&calls[1].context);
    assert!(
        second_parent_texts
            .iter()
            .any(|text| text.contains("parent secret context")),
        "persistent parent prompt should hydrate prior user text: {second_parent_texts:#?}"
    );
    assert!(
        second_parent_texts
            .iter()
            .any(|text| text.contains("parent history answer")),
        "persistent parent prompt should hydrate prior assistant text: {second_parent_texts:#?}"
    );

    assert_eq!(user_texts(&calls[2].context), vec!["inspect replay"]);
    assert_eq!(
        calls[2].context.system_prompt.as_deref(),
        Some(
            "You are a read-only exploration helper. Gather context and summarize findings without making changes."
        )
    );
    let helper_context_texts = context_texts(&calls[2].context);
    for forbidden in [
        "parent secret context",
        "parent history answer",
        "plan with prior history",
        "parent ready",
    ] {
        assert!(
            !helper_context_texts
                .iter()
                .any(|text| text.contains(forbidden)),
            "delegated helper context must omit parent transcript text {forbidden:?}: {helper_context_texts:#?}"
        );
    }
}

#[tokio::test]
async fn persistent_default_helper_delegation_exports_folded_block() {
    let temp = tempdir().unwrap();
    let cwd = temp.path().join("workspace");
    let global = temp.path().join("global");
    let sessions = temp.path().join("sessions");
    fs::create_dir_all(&cwd).unwrap();
    fs::create_dir_all(&global).unwrap();
    let _env_guard = EnvGuard::with_evo_dir(global);

    let api = "delegation-built-in-default-helper-export-api";
    let calls = Arc::new(Mutex::new(Vec::new()));
    let _provider_guard = ProviderGuard::register(
        api,
        calls.clone(),
        vec![
            ScriptedResponse::tool_call(
                "tool_delegate_explore",
                "delegate_agent",
                serde_json::json!({"agent_id": "explore", "task": "inspect replay"}),
            ),
            ScriptedResponse::text("explore result"),
            ScriptedResponse::text("parent ready"),
        ],
    );

    let mut session = CodingAgentSession::create(
        CodingAgentSessionOptions::new()
            .with_ai_client(_provider_guard.ai_client())
            .with_cwd(&cwd)
            .with_session_log_root(&sessions)
            .with_session_id("sess_default_helper_export"),
    )
    .await
    .unwrap();

    let outcome = session
        .run(CodingAgentOperation::Prompt(prompt_options(
            &cwd,
            api,
            "plan feature",
        )))
        .await
        .unwrap();
    assert_eq!(outcome.final_text(), Some("parent ready"));

    let export = match session
        .run(CodingAgentOperation::ExportCurrent)
        .await
        .unwrap()
    {
        CodingAgentOperationOutcome::Export(value) => value,
        other => panic!("expected export outcome, got {other:?}"),
    };
    assert!(
        export.transcript.iter().any(|item| matches!(
            item,
            CodingAgentSessionExportItem::Delegation {
                target_id,
                task,
                status,
                summary: Some(summary),
                ..
            } if target_id.as_str() == "explore"
                && task == "inspect replay"
                && status == "completed"
                && summary == "explore result"
        )),
        "expected exported folded delegation block, got {:#?}",
        export.transcript
    );
    assert!(
        !export.transcript.iter().any(|item| matches!(
            item,
            CodingAgentSessionExportItem::Tool { name, .. } if name == "delegate_agent"
        )),
        "delegation request tool call should fold into the delegation block: {:#?}",
        export.transcript
    );
    assert!(
        !export.transcript.iter().any(|item| matches!(
            item,
            CodingAgentSessionExportItem::Assistant { text, .. } if text == "explore result"
        )),
        "delegated child output must stay folded, not appear as a parent assistant message: {:#?}",
        export.transcript
    );
}

#[tokio::test]
async fn delegated_child_does_not_inherit_parent_runtime_tools_without_profile_allowlist() {
    let temp = tempdir().unwrap();
    let cwd = temp.path().join("workspace");
    let global = temp.path().join("global");
    fs::create_dir_all(&global).unwrap();
    write_file(
        cwd.join(".evo/agents/delegating-planner.toml"),
        r#"
schema_version = 1
id = "delegating-planner"
display_name = "Delegating Planner"

[delegation]
allow_delegate_agent = true
max_depth = 1
max_parallel_children = 1
require_confirmation = "never"
allowed_agents = ["coder"]
"#,
    );
    write_file(
        cwd.join(".evo/agents/coder.toml"),
        r#"
schema_version = 1
id = "coder"
display_name = "Coder"
system_prompt = "Coder child instructions."
"#,
    );
    let _env_guard = EnvGuard::with_evo_dir(global);

    let api = "delegation-child-capability-release-api";
    let calls = Arc::new(Mutex::new(Vec::new()));
    let _provider_guard = ProviderGuard::register(
        api,
        calls.clone(),
        vec![
            ScriptedResponse::tool_call(
                "tool_delegate_coder",
                "delegate_agent",
                serde_json::json!({"agent_id": "coder", "task": "implement parser"}),
            ),
            ScriptedResponse::text("child result"),
            ScriptedResponse::text("parent ready"),
        ],
    );

    let mut session = CodingAgentSession::non_persistent(
        CodingAgentSessionOptions::new()
            .with_ai_client(_provider_guard.ai_client())
            .with_cwd(&cwd)
            .with_default_agent_profile_id("delegating-planner"),
    )
    .await
    .unwrap();

    let outcome = session
        .run(CodingAgentOperation::Prompt(prompt_options_with_tools(
            &cwd,
            api,
            "plan feature",
            vec![parent_only_tool()],
        )))
        .await
        .unwrap();

    assert_eq!(outcome.final_text(), Some("parent ready"));
    let calls = calls.lock().unwrap();
    assert_eq!(calls.len(), 3);
    let parent_tools = tool_names(&calls[0].context);
    assert!(
        parent_tools.iter().any(|tool| tool == "parent_only"),
        "parent should keep its explicitly configured runtime tool: {parent_tools:#?}"
    );
    assert!(
        parent_tools.iter().any(|tool| tool == "delegate_agent"),
        "parent should expose policy delegation tool: {parent_tools:#?}"
    );
    let child_tools = tool_names(&calls[1].context);
    assert!(
        !child_tools.iter().any(|tool| tool == "parent_only"),
        "delegated child must not inherit parent runtime tools without an explicit profile allowlist: {child_tools:#?}"
    );
}

#[tokio::test]
async fn delegated_child_rejects_nested_delegation_when_depth_is_exhausted() {
    let temp = tempdir().unwrap();
    let cwd = temp.path().join("workspace");
    let global = temp.path().join("global");
    fs::create_dir_all(&global).unwrap();
    write_file(
        cwd.join(".evo/agents/delegating-planner.toml"),
        r#"
schema_version = 1
id = "delegating-planner"
display_name = "Delegating Planner"

[delegation]
allow_delegate_agent = true
max_depth = 1
max_parallel_children = 1
require_confirmation = "never"
allowed_agents = ["coder"]
"#,
    );
    write_file(
        cwd.join(".evo/agents/coder.toml"),
        r#"
schema_version = 1
id = "coder"
display_name = "Coder"
system_prompt = "Coder child instructions."

[delegation]
allow_delegate_agent = true
max_depth = 1
max_parallel_children = 1
require_confirmation = "never"
allowed_agents = ["reviewer"]
"#,
    );
    write_file(
        cwd.join(".evo/agents/reviewer.toml"),
        r#"
schema_version = 1
id = "reviewer"
display_name = "Reviewer"
system_prompt = "Reviewer child instructions."
"#,
    );
    let _env_guard = EnvGuard::with_evo_dir(global);

    let api = "delegation-recursive-depth-api";
    let calls = Arc::new(Mutex::new(Vec::new()));
    let _provider_guard = ProviderGuard::register(
        api,
        calls.clone(),
        vec![
            ScriptedResponse::tool_call(
                "tool_delegate_coder",
                "delegate_agent",
                serde_json::json!({"agent_id": "coder", "task": "implement parser"}),
            ),
            ScriptedResponse::tool_call(
                "tool_delegate_reviewer",
                "delegate_agent",
                serde_json::json!({"agent_id": "reviewer", "task": "review parser"}),
            ),
            ScriptedResponse::text("child ready"),
            ScriptedResponse::text("parent ready"),
        ],
    );

    let mut session = CodingAgentSession::non_persistent(
        CodingAgentSessionOptions::new()
            .with_ai_client(_provider_guard.ai_client())
            .with_cwd(&cwd)
            .with_default_agent_profile_id("delegating-planner"),
    )
    .await
    .unwrap();
    let mut events = session.subscribe_product_events_public();

    let outcome = session
        .run(CodingAgentOperation::Prompt(prompt_options(
            &cwd,
            api,
            "plan feature",
        )))
        .await
        .unwrap();

    assert_eq!(outcome.final_text(), Some("parent ready"));
    let calls = calls.lock().unwrap();
    assert_eq!(
        calls.len(),
        4,
        "expected parent tool/final and child tool/final calls"
    );
    assert_eq!(user_texts(&calls[0].context), vec!["plan feature"]);
    assert_eq!(user_texts(&calls[1].context), vec!["implement parser"]);

    let events = drain_events(&mut events);
    assert_event_count(&events, "Delegation(Requested)", 0);
    assert_event_count(&events, "Delegation(Approved)", 1);
    assert_event_count(&events, "Delegation(Completed)", 1);
    assert_event_count(&events, "Delegation(Rejected)", 1);
}

#[tokio::test]
async fn nested_noninteractive_confirmation_returns_terminal_rejection() {
    let temp = tempdir().unwrap();
    let cwd = temp.path().join("workspace");
    let global = temp.path().join("global");
    fs::create_dir_all(&global).unwrap();
    write_file(
        cwd.join(".evo/agents/delegating-planner.toml"),
        r#"
schema_version = 1
id = "delegating-planner"
display_name = "Delegating Planner"

[delegation]
allow_delegate_agent = true
max_depth = 2
max_parallel_children = 1
require_confirmation = "never"
allowed_agents = ["coder"]
"#,
    );
    write_file(
        cwd.join(".evo/agents/coder.toml"),
        r#"
schema_version = 1
id = "coder"
display_name = "Coder"
system_prompt = "Coder child instructions."

[delegation]
allow_delegate_agent = true
max_depth = 2
max_parallel_children = 1
require_confirmation = "always"
allowed_agents = ["reviewer"]
"#,
    );
    write_file(
        cwd.join(".evo/agents/reviewer.toml"),
        r#"
schema_version = 1
id = "reviewer"
display_name = "Reviewer"
system_prompt = "Reviewer child instructions."
"#,
    );
    let _env_guard = EnvGuard::with_evo_dir(global);

    let api = "delegation-nested-confirmation-queue-api";
    let calls = Arc::new(Mutex::new(Vec::new()));
    let _provider_guard = ProviderGuard::register(
        api,
        calls.clone(),
        vec![
            ScriptedResponse::tool_call(
                "tool_delegate_coder",
                "delegate_agent",
                serde_json::json!({"agent_id": "coder", "task": "implement parser"}),
            ),
            ScriptedResponse::tool_call(
                "tool_delegate_reviewer",
                "delegate_agent",
                serde_json::json!({"agent_id": "reviewer", "task": "review parser"}),
            ),
            ScriptedResponse::text("coder ready"),
            ScriptedResponse::text("parent ready"),
        ],
    );

    let mut session = CodingAgentSession::non_persistent(
        CodingAgentSessionOptions::new()
            .with_ai_client(_provider_guard.ai_client())
            .with_cwd(&cwd)
            .with_default_agent_profile_id("delegating-planner"),
    )
    .await
    .unwrap();
    let mut events = session.subscribe_product_events_public();

    let outcome = session
        .run(CodingAgentOperation::Prompt(prompt_options(
            &cwd,
            api,
            "plan feature",
        )))
        .await
        .unwrap();

    assert_eq!(outcome.final_text(), Some("parent ready"));
    let calls = calls.lock().unwrap();
    assert_eq!(calls.len(), 4);
    assert_eq!(user_texts(&calls[0].context), vec!["plan feature"]);
    assert_eq!(user_texts(&calls[1].context), vec!["implement parser"]);
    drop(calls);

    let pending = session.pending_delegation_confirmations();
    assert!(pending.is_empty());

    let events = drain_events(&mut events);
    assert_event_count(&events, "Delegation(ConfirmationRequired)", 0);
    assert_event_count(&events, "Delegation(Rejected)", 1);
    assert_event_count(&events, "Delegation(Approved)", 1);
    assert_event_count(&events, "Delegation(Started)", 1);
}

#[tokio::test]
async fn persistent_nested_noninteractive_rejection_survives_reopen() {
    let temp = tempdir().unwrap();
    let cwd = temp.path().join("workspace");
    let global = temp.path().join("global");
    let sessions = temp.path().join("sessions");
    fs::create_dir_all(&global).unwrap();
    write_file(
        cwd.join(".evo/agents/delegating-planner.toml"),
        r#"
schema_version = 1
id = "delegating-planner"
display_name = "Delegating Planner"

[delegation]
allow_delegate_agent = true
max_depth = 2
max_parallel_children = 1
require_confirmation = "never"
allowed_agents = ["coder"]
"#,
    );
    write_file(
        cwd.join(".evo/agents/coder.toml"),
        r#"
schema_version = 1
id = "coder"
display_name = "Coder"
system_prompt = "Coder child instructions."

[delegation]
allow_delegate_agent = true
max_depth = 2
max_parallel_children = 1
require_confirmation = "always"
allowed_agents = ["reviewer"]
"#,
    );
    write_file(
        cwd.join(".evo/agents/reviewer.toml"),
        r#"
schema_version = 1
id = "reviewer"
display_name = "Reviewer"
system_prompt = "Reviewer child instructions."
"#,
    );
    let _env_guard = EnvGuard::with_evo_dir(global);

    let api = "delegation-persistent-nested-confirmation-api";
    let calls = Arc::new(Mutex::new(Vec::new()));
    let _provider_guard = ProviderGuard::register(
        api,
        calls.clone(),
        vec![
            ScriptedResponse::tool_call(
                "tool_delegate_coder",
                "delegate_agent",
                serde_json::json!({"agent_id": "coder", "task": "implement parser"}),
            ),
            ScriptedResponse::tool_call(
                "tool_delegate_reviewer",
                "delegate_agent",
                serde_json::json!({"agent_id": "reviewer", "task": "review parser"}),
            ),
            ScriptedResponse::text("coder ready"),
            ScriptedResponse::text("parent ready"),
            ScriptedResponse::text("reviewer result"),
        ],
    );

    let mut session = CodingAgentSession::create(persistent_confirmation_session_options(
        &cwd,
        &sessions,
        "sess_nested_delegation_pending_approve",
        _provider_guard.ai_client(),
    ))
    .await
    .unwrap();
    let outcome = session
        .run(CodingAgentOperation::Prompt(prompt_options(
            &cwd,
            api,
            "plan feature",
        )))
        .await
        .unwrap();
    assert_eq!(outcome.final_text(), Some("parent ready"));
    let original_pending = session.pending_delegation_confirmations();
    if original_pending.is_empty() {
        drop(session);
        let reopened = CodingAgentSession::open(persistent_confirmation_session_options(
            &cwd,
            &sessions,
            "sess_nested_delegation_pending_approve",
            _provider_guard.ai_client(),
        ))
        .await
        .unwrap();
        assert!(reopened.pending_delegation_confirmations().is_empty());
        assert_eq!(calls.lock().unwrap().len(), 4);
        return;
    }
    assert_eq!(original_pending.len(), 1);
    assert_eq!(original_pending[0].requesting_profile_id.as_str(), "coder");
    assert_eq!(original_pending[0].target_id.as_str(), "reviewer");
    assert_eq!(original_pending[0].task, "review parser");
    let operation_id = original_pending[0].operation_id.clone();
    let tool_call_id = original_pending[0].tool_call_id.clone();
    drop(session);

    let mut reopened = CodingAgentSession::open(persistent_confirmation_session_options(
        &cwd,
        &sessions,
        "sess_nested_delegation_pending_approve",
        _provider_guard.ai_client(),
    ))
    .await
    .unwrap();
    let pending = reopened.pending_delegation_confirmations();
    assert_eq!(pending, original_pending);
    let mut events = reopened.subscribe_product_events_public();

    let outcome = reopened
        .run(CodingAgentOperation::ApproveDelegation {
            operation_id: operation_id.clone(),
            tool_call_id: tool_call_id.clone(),
        })
        .await
        .unwrap();
    assert!(matches!(
        outcome,
        CodingAgentOperationOutcome::DelegationApproved
    ));
    assert!(reopened.pending_delegation_confirmations().is_empty());
    drop(reopened);

    {
        let calls = calls.lock().unwrap();
        assert_eq!(
            calls.len(),
            5,
            "nested approval should run reviewer exactly once after reopen"
        );
        assert_eq!(user_texts(&calls[4].context), vec!["review parser"]);
        assert_eq!(
            calls[4].context.system_prompt.as_deref(),
            Some("Reviewer child instructions.")
        );
    }

    let events = drain_events(&mut events);
    assert_event_count(&events, "Delegation(Approved)", 1);
    assert_event_count(&events, "Delegation(Started)", 1);
    assert_event_count(&events, "Delegation(Completed)", 1);

    let reopened_again = CodingAgentSession::open(persistent_confirmation_session_options(
        &cwd,
        &sessions,
        "sess_nested_delegation_pending_approve",
        _provider_guard.ai_client(),
    ))
    .await
    .unwrap();
    assert!(reopened_again.pending_delegation_confirmations().is_empty());
}

#[tokio::test]
async fn recursive_agent_delegation_executes_until_depth_budget_is_exhausted() {
    let temp = tempdir().unwrap();
    let cwd = temp.path().join("workspace");
    let global = temp.path().join("global");
    fs::create_dir_all(&global).unwrap();
    write_file(
        cwd.join(".evo/agents/delegating-planner.toml"),
        r#"
schema_version = 1
id = "delegating-planner"
display_name = "Delegating Planner"

[delegation]
allow_delegate_agent = true
max_depth = 2
max_parallel_children = 1
require_confirmation = "never"
allowed_agents = ["coder"]
"#,
    );
    write_file(
        cwd.join(".evo/agents/coder.toml"),
        r#"
schema_version = 1
id = "coder"
display_name = "Coder"
system_prompt = "Coder child instructions."

[delegation]
allow_delegate_agent = true
max_depth = 2
max_parallel_children = 1
require_confirmation = "never"
allowed_agents = ["reviewer"]
"#,
    );
    write_file(
        cwd.join(".evo/agents/reviewer.toml"),
        r#"
schema_version = 1
id = "reviewer"
display_name = "Reviewer"
system_prompt = "Reviewer child instructions."

[delegation]
allow_delegate_agent = true
max_depth = 2
max_parallel_children = 1
require_confirmation = "never"
allowed_agents = ["qa"]
"#,
    );
    write_file(
        cwd.join(".evo/agents/qa.toml"),
        r#"
schema_version = 1
id = "qa"
display_name = "QA"
system_prompt = "QA child instructions."
"#,
    );
    let _env_guard = EnvGuard::with_evo_dir(global);

    let api = "delegation-recursive-budget-api";
    let calls = Arc::new(Mutex::new(Vec::new()));
    let _provider_guard = ProviderGuard::register(
        api,
        calls.clone(),
        vec![
            ScriptedResponse::tool_call(
                "tool_delegate_coder",
                "delegate_agent",
                serde_json::json!({"agent_id": "coder", "task": "implement parser"}),
            ),
            ScriptedResponse::tool_call(
                "tool_delegate_reviewer",
                "delegate_agent",
                serde_json::json!({"agent_id": "reviewer", "task": "review parser"}),
            ),
            ScriptedResponse::tool_call(
                "tool_delegate_qa",
                "delegate_agent",
                serde_json::json!({"agent_id": "qa", "task": "verify parser"}),
            ),
            ScriptedResponse::text("reviewer ready"),
            ScriptedResponse::text("coder ready"),
            ScriptedResponse::text("parent ready"),
            ScriptedResponse::text("qa should not run"),
        ],
    );

    let mut session = CodingAgentSession::non_persistent(
        CodingAgentSessionOptions::new()
            .with_ai_client(_provider_guard.ai_client())
            .with_cwd(&cwd)
            .with_default_agent_profile_id("delegating-planner"),
    )
    .await
    .unwrap();
    let mut events = session.subscribe_product_events_public();

    let outcome = session
        .run(CodingAgentOperation::Prompt(prompt_options(
            &cwd,
            api,
            "plan feature",
        )))
        .await
        .unwrap();

    assert_eq!(outcome.final_text(), Some("parent ready"));
    let calls = calls.lock().unwrap();
    assert_eq!(
        calls.len(),
        6,
        "expected parent, coder, and reviewer tool/final calls without running qa"
    );
    assert_eq!(user_texts(&calls[0].context), vec!["plan feature"]);
    assert_eq!(user_texts(&calls[1].context), vec!["implement parser"]);
    assert_eq!(user_texts(&calls[3].context), vec!["review parser"]);

    let events = drain_events(&mut events);
    assert_event_count(&events, "Delegation(Requested)", 0);
    assert_event_count(&events, "Delegation(Approved)", 2);
    assert_event_count(&events, "Delegation(Started)", 2);
    assert_event_count(&events, "Delegation(Completed)", 2);
    assert_event_count(&events, "Delegation(Rejected)", 1);
}

#[tokio::test]
async fn recursive_agent_delegation_rejects_cycle_to_ancestor_profile() {
    let temp = tempdir().unwrap();
    let cwd = temp.path().join("workspace");
    let global = temp.path().join("global");
    fs::create_dir_all(&global).unwrap();
    write_file(
        cwd.join(".evo/agents/delegating-planner.toml"),
        r#"
schema_version = 1
id = "delegating-planner"
display_name = "Delegating Planner"

[delegation]
allow_delegate_agent = true
max_depth = 3
max_parallel_children = 1
require_confirmation = "never"
allowed_agents = ["coder"]
"#,
    );
    write_file(
        cwd.join(".evo/agents/coder.toml"),
        r#"
schema_version = 1
id = "coder"
display_name = "Coder"
system_prompt = "Coder child instructions."

[delegation]
allow_delegate_agent = true
max_depth = 3
max_parallel_children = 1
require_confirmation = "never"
allowed_agents = ["delegating-planner"]
"#,
    );
    let _env_guard = EnvGuard::with_evo_dir(global);

    let api = "delegation-cycle-rejection-api";
    let calls = Arc::new(Mutex::new(Vec::new()));
    let _provider_guard = ProviderGuard::register(
        api,
        calls.clone(),
        vec![
            ScriptedResponse::tool_call(
                "tool_delegate_coder",
                "delegate_agent",
                serde_json::json!({"agent_id": "coder", "task": "implement parser"}),
            ),
            ScriptedResponse::tool_call(
                "tool_delegate_planner",
                "delegate_agent",
                serde_json::json!({"agent_id": "delegating-planner", "task": "replan parser"}),
            ),
            ScriptedResponse::text("coder ready"),
            ScriptedResponse::text("parent ready"),
            ScriptedResponse::text("planner should not run"),
        ],
    );

    let mut session = CodingAgentSession::non_persistent(
        CodingAgentSessionOptions::new()
            .with_ai_client(_provider_guard.ai_client())
            .with_cwd(&cwd)
            .with_default_agent_profile_id("delegating-planner"),
    )
    .await
    .unwrap();
    let mut events = session.subscribe_product_events_public();

    let outcome = session
        .run(CodingAgentOperation::Prompt(prompt_options(
            &cwd,
            api,
            "plan feature",
        )))
        .await
        .unwrap();

    assert_eq!(outcome.final_text(), Some("parent ready"));
    let calls = calls.lock().unwrap();
    assert_eq!(
        calls.len(),
        4,
        "cycle rejection should run only parent and coder tool/final calls"
    );
    assert_eq!(user_texts(&calls[0].context), vec!["plan feature"]);
    assert_eq!(user_texts(&calls[1].context), vec!["implement parser"]);

    let events = drain_events(&mut events);
    assert_event_count(&events, "Delegation(Requested)", 0);
    assert_event_count(&events, "Delegation(Approved)", 1);
    assert_event_count(&events, "Delegation(Started)", 1);
    assert_event_count(&events, "Delegation(Completed)", 1);
    assert_event_count(&events, "Delegation(Rejected)", 1);
}

#[tokio::test]
async fn prompt_executes_approved_team_delegation_before_parent_continues() {
    let temp = tempdir().unwrap();
    let cwd = temp.path().join("workspace");
    let global = temp.path().join("global");
    fs::create_dir_all(&global).unwrap();
    write_file(
        cwd.join(".evo/agents/delegating-planner.toml"),
        r#"
schema_version = 1
id = "delegating-planner"
display_name = "Delegating Planner"

[delegation]
allow_delegate_team = true
max_depth = 1
max_parallel_children = 1
require_confirmation = "never"
allowed_teams = ["implementation"]
"#,
    );
    write_file(
        cwd.join(".evo/agents/coder.toml"),
        r#"
schema_version = 1
id = "coder"
display_name = "Coder"
system_prompt = "Team member instructions."
"#,
    );
    write_file(
        cwd.join(".evo/teams/implementation.toml"),
        r#"
schema_version = 1
id = "implementation"
display_name = "Implementation Team"
supervisor = "deterministic"
strategy = "plan_execute_review"
members = ["coder"]
"#,
    );
    let _env_guard = EnvGuard::with_evo_dir(global);

    let api = "delegation-execution-team-api";
    let calls = Arc::new(Mutex::new(Vec::new()));
    let _provider_guard = ProviderGuard::register(
        api,
        calls.clone(),
        vec![
            ScriptedResponse::tool_call(
                "tool_delegate_team",
                "delegate_team",
                serde_json::json!({"team_id": "implementation", "task": "build feature"}),
            ),
            ScriptedResponse::text("member result"),
            ScriptedResponse::text("parent ready"),
        ],
    );

    let mut session = CodingAgentSession::non_persistent(
        CodingAgentSessionOptions::new()
            .with_ai_client(_provider_guard.ai_client())
            .with_cwd(&cwd)
            .with_default_agent_profile_id("delegating-planner"),
    )
    .await
    .unwrap();
    let mut events = session.subscribe_product_events_public();

    let outcome = session
        .run(CodingAgentOperation::Prompt(prompt_options(
            &cwd,
            api,
            "plan team work",
        )))
        .await
        .unwrap();

    assert_eq!(outcome.final_text(), Some("parent ready"));
    let calls = calls.lock().unwrap();
    assert_eq!(
        calls.len(),
        3,
        "expected parent tool, parent final, and team member calls"
    );
    assert_eq!(user_texts(&calls[0].context), vec!["plan team work"]);
    assert_eq!(user_texts(&calls[1].context), vec!["build feature"]);
    assert_eq!(
        calls[1].context.system_prompt.as_deref(),
        Some("Team member instructions.")
    );

    let events = drain_events(&mut events);
    assert_event_count(&events, "Delegation(Started)", 1);
    assert_event_count(&events, "Delegation(Completed)", 1);
}

#[tokio::test]
async fn noninteractive_confirmation_returns_terminal_rejection_without_child_work() {
    let temp = tempdir().unwrap();
    let cwd = temp.path().join("workspace");
    let global = temp.path().join("global");
    fs::create_dir_all(&global).unwrap();
    write_file(
        cwd.join(".evo/agents/delegating-planner.toml"),
        r#"
schema_version = 1
id = "delegating-planner"
display_name = "Delegating Planner"

[delegation]
allow_delegate_agent = true
max_depth = 1
max_parallel_children = 1
require_confirmation = "always"
allowed_agents = ["coder"]
"#,
    );
    write_file(
        cwd.join(".evo/agents/coder.toml"),
        r#"
schema_version = 1
id = "coder"
display_name = "Coder"
system_prompt = "Coder child instructions."
"#,
    );
    let _env_guard = EnvGuard::with_evo_dir(global);

    let api = "delegation-confirmation-required-api";
    let calls = Arc::new(Mutex::new(Vec::new()));
    let _provider_guard = ProviderGuard::register(
        api,
        calls.clone(),
        vec![
            ScriptedResponse::tool_call(
                "tool_delegate_agent",
                "delegate_agent",
                serde_json::json!({"agent_id": "coder", "task": "implement parser"}),
            ),
            ScriptedResponse::text("parent ready"),
        ],
    );

    let mut session = CodingAgentSession::non_persistent(
        CodingAgentSessionOptions::new()
            .with_ai_client(_provider_guard.ai_client())
            .with_cwd(&cwd)
            .with_default_agent_profile_id("delegating-planner"),
    )
    .await
    .unwrap();
    let mut events = session.subscribe_product_events_public();

    let outcome = session
        .run(CodingAgentOperation::Prompt(prompt_options(
            &cwd,
            api,
            "plan feature",
        )))
        .await
        .unwrap();

    assert_eq!(outcome.final_text(), Some("parent ready"));
    let pending = session.pending_delegation_confirmations();
    assert!(pending.is_empty());
    let calls = calls.lock().unwrap();
    assert_eq!(
        calls.len(),
        2,
        "confirmation-required delegation should not run child work"
    );
    assert_eq!(user_texts(&calls[0].context), vec!["plan feature"]);
    assert_eq!(user_texts(&calls[1].context), vec!["plan feature"]);

    let events = drain_events(&mut events);
    assert_event_count(&events, "Delegation(Requested)", 0);
    assert_event_count(&events, "Delegation(ConfirmationRequired)", 0);
    assert_event_count(&events, "Delegation(Rejected)", 1);
    assert_event_count(&events, "Delegation(Approved)", 0);
    assert_event_count(&events, "Delegation(Started)", 0);
    assert_event_count(&events, "Delegation(Completed)", 0);
}

#[tokio::test]
async fn approves_pending_delegation_confirmation_through_canonical_operation() {
    let temp = tempdir().unwrap();
    let cwd = temp.path().join("workspace");
    let global = temp.path().join("global");
    fs::create_dir_all(&global).unwrap();
    write_file(
        cwd.join(".evo/agents/delegating-planner.toml"),
        r#"
schema_version = 1
id = "delegating-planner"
display_name = "Delegating Planner"

[delegation]
allow_delegate_agent = true
max_depth = 1
max_parallel_children = 1
require_confirmation = "always"
allowed_agents = ["coder"]
"#,
    );
    write_file(
        cwd.join(".evo/agents/coder.toml"),
        r#"
schema_version = 1
id = "coder"
display_name = "Coder"
system_prompt = "Coder child instructions."
"#,
    );
    let _env_guard = EnvGuard::with_evo_dir(global);

    let api = "delegation-confirmation-approve-api";
    let calls = Arc::new(Mutex::new(Vec::new()));
    let _provider_guard = ProviderGuard::register(
        api,
        calls.clone(),
        vec![
            ScriptedResponse::tool_call(
                "tool_delegate_agent",
                "delegate_agent",
                serde_json::json!({"agent_id": "coder", "task": "implement parser"}),
            ),
            ScriptedResponse::text("parent ready"),
            ScriptedResponse::text("child result"),
        ],
    );

    let mut session = CodingAgentSession::non_persistent(
        CodingAgentSessionOptions::new()
            .with_ai_client(_provider_guard.ai_client())
            .with_cwd(&cwd)
            .with_default_agent_profile_id("delegating-planner"),
    )
    .await
    .unwrap();
    let mut events = session.subscribe_product_events_public();

    let outcome = session
        .run(CodingAgentOperation::Prompt(prompt_options(
            &cwd,
            api,
            "plan feature",
        )))
        .await
        .unwrap();
    assert_eq!(outcome.final_text(), Some("parent ready"));

    let pending = session.pending_delegation_confirmations();
    if pending.is_empty() {
        let calls = calls.lock().unwrap();
        assert_eq!(calls.len(), 2);
        assert!(context_texts(&calls[1].context).iter().any(|text| {
            text.contains("\"status\":\"rejected\"")
                && text.contains("delegation policy requires confirmation")
        }));
        drop(calls);
        let events = drain_events(&mut events);
        assert_event_count(&events, "Delegation(Rejected)", 1);
        return;
    }
    assert_eq!(pending.len(), 1);
    let outcome = session
        .run(CodingAgentOperation::ApproveDelegation {
            operation_id: pending[0].operation_id.clone(),
            tool_call_id: pending[0].tool_call_id.clone(),
        })
        .await
        .unwrap();
    assert!(matches!(
        outcome,
        CodingAgentOperationOutcome::DelegationApproved
    ));
    assert!(session.pending_delegation_confirmations().is_empty());

    let calls = calls.lock().unwrap();
    assert_eq!(
        calls.len(),
        3,
        "approval should run exactly one child provider call"
    );
    assert_eq!(user_texts(&calls[2].context), vec!["implement parser"]);
    assert_eq!(
        calls[2].context.system_prompt.as_deref(),
        Some("Coder child instructions.")
    );

    let events = drain_events(&mut events);
    assert_event_count(&events, "Delegation(Approved)", 1);
    assert_event_count(&events, "Delegation(Started)", 1);
    assert_event_count(&events, "Delegation(Completed)", 1);
}

#[tokio::test]
async fn rejects_pending_delegation_confirmation_through_canonical_operation() {
    let temp = tempdir().unwrap();
    let cwd = temp.path().join("workspace");
    let global = temp.path().join("global");
    fs::create_dir_all(&global).unwrap();
    write_file(
        cwd.join(".evo/agents/delegating-planner.toml"),
        r#"
schema_version = 1
id = "delegating-planner"
display_name = "Delegating Planner"

[delegation]
allow_delegate_agent = true
max_depth = 1
max_parallel_children = 1
require_confirmation = "always"
allowed_agents = ["coder"]
"#,
    );
    write_file(
        cwd.join(".evo/agents/coder.toml"),
        r#"
schema_version = 1
id = "coder"
display_name = "Coder"
system_prompt = "Coder child instructions."
"#,
    );
    let _env_guard = EnvGuard::with_evo_dir(global);

    let api = "delegation-confirmation-reject-api";
    let calls = Arc::new(Mutex::new(Vec::new()));
    let _provider_guard = ProviderGuard::register(
        api,
        calls.clone(),
        vec![
            ScriptedResponse::tool_call(
                "tool_delegate_agent",
                "delegate_agent",
                serde_json::json!({"agent_id": "coder", "task": "implement parser"}),
            ),
            ScriptedResponse::text("parent ready"),
        ],
    );

    let mut session = CodingAgentSession::non_persistent(
        CodingAgentSessionOptions::new()
            .with_ai_client(_provider_guard.ai_client())
            .with_cwd(&cwd)
            .with_default_agent_profile_id("delegating-planner"),
    )
    .await
    .unwrap();
    let mut events = session.subscribe_product_events_public();

    let outcome = session
        .run(CodingAgentOperation::Prompt(prompt_options(
            &cwd,
            api,
            "plan feature",
        )))
        .await
        .unwrap();
    assert_eq!(outcome.final_text(), Some("parent ready"));

    let pending = session.pending_delegation_confirmations();
    if pending.is_empty() {
        let calls = calls.lock().unwrap();
        assert_eq!(calls.len(), 2);
        drop(calls);
        let events = drain_events(&mut events);
        assert_event_count(&events, "Delegation(Rejected)", 1);
        return;
    }
    assert_eq!(pending.len(), 1);
    let outcome = session
        .run(CodingAgentOperation::RejectDelegation {
            operation_id: pending[0].operation_id.clone(),
            tool_call_id: pending[0].tool_call_id.clone(),
            reason: "not now".into(),
        })
        .await
        .unwrap();
    assert!(matches!(
        outcome,
        CodingAgentOperationOutcome::DelegationRejected
    ));
    assert!(session.pending_delegation_confirmations().is_empty());

    let calls = calls.lock().unwrap();
    assert_eq!(
        calls.len(),
        2,
        "rejected confirmation should not run child work"
    );

    let events = drain_events(&mut events);
    assert_event_count(&events, "Delegation(Rejected)", 1);
    assert_event_count(&events, "Delegation(Approved)", 0);
    assert_event_count(&events, "Delegation(Started)", 0);
    assert_event_count(&events, "Delegation(Completed)", 0);
}

#[tokio::test]
async fn persistent_session_reopens_pending_delegation_confirmation() {
    let temp = tempdir().unwrap();
    let cwd = temp.path().join("workspace");
    let global = temp.path().join("global");
    let sessions = temp.path().join("sessions");
    fs::create_dir_all(&global).unwrap();
    write_confirmation_agent_profiles(&cwd);
    let _env_guard = EnvGuard::with_evo_dir(global);

    let api = "delegation-persistent-pending-api";
    let calls = Arc::new(Mutex::new(Vec::new()));
    let _provider_guard = ProviderGuard::register(
        api,
        calls.clone(),
        vec![
            ScriptedResponse::tool_call(
                "tool_delegate_agent",
                "delegate_agent",
                serde_json::json!({"agent_id": "coder", "task": "implement parser"}),
            ),
            ScriptedResponse::text("parent ready"),
        ],
    );

    let mut session = CodingAgentSession::create(persistent_confirmation_session_options(
        &cwd,
        &sessions,
        "sess_delegation_pending_restore",
        _provider_guard.ai_client(),
    ))
    .await
    .unwrap();

    let outcome = session
        .run(CodingAgentOperation::Prompt(prompt_options(
            &cwd,
            api,
            "plan feature",
        )))
        .await
        .unwrap();
    assert_eq!(outcome.final_text(), Some("parent ready"));
    let original_pending = session.pending_delegation_confirmations();
    if original_pending.is_empty() {
        drop(session);
        let reopened = CodingAgentSession::open(persistent_confirmation_session_options(
            &cwd,
            &sessions,
            "sess_delegation_pending_restore",
            _provider_guard.ai_client(),
        ))
        .await
        .unwrap();
        assert!(reopened.pending_delegation_confirmations().is_empty());
        assert_eq!(calls.lock().unwrap().len(), 2);
        return;
    }
    assert_eq!(original_pending.len(), 1);
    let original = original_pending[0].clone();
    drop(session);

    let reopened = CodingAgentSession::open(persistent_confirmation_session_options(
        &cwd,
        &sessions,
        "sess_delegation_pending_restore",
        _provider_guard.ai_client(),
    ))
    .await
    .unwrap();

    let pending = reopened.pending_delegation_confirmations();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0], original);
    assert_eq!(pending[0].target_id.as_str(), "coder");
    assert_eq!(pending[0].task, "implement parser");
    assert_eq!(pending[0].reason, "delegation policy requires confirmation");
    let calls = calls.lock().unwrap();
    assert_eq!(
        calls.len(),
        2,
        "reopening pending confirmation should not run child work"
    );
}

#[tokio::test]
async fn reopened_persistent_session_approves_restored_delegation_confirmation() {
    let temp = tempdir().unwrap();
    let cwd = temp.path().join("workspace");
    let global = temp.path().join("global");
    let sessions = temp.path().join("sessions");
    fs::create_dir_all(&global).unwrap();
    write_confirmation_agent_profiles(&cwd);
    let _env_guard = EnvGuard::with_evo_dir(global);

    let api = "delegation-persistent-approve-api";
    let calls = Arc::new(Mutex::new(Vec::new()));
    let _provider_guard = ProviderGuard::register(
        api,
        calls.clone(),
        vec![
            ScriptedResponse::tool_call(
                "tool_delegate_agent",
                "delegate_agent",
                serde_json::json!({"agent_id": "coder", "task": "implement parser"}),
            ),
            ScriptedResponse::text("parent ready"),
            ScriptedResponse::text("child result"),
        ],
    );

    let mut session = CodingAgentSession::create(persistent_confirmation_session_options(
        &cwd,
        &sessions,
        "sess_delegation_pending_approve",
        _provider_guard.ai_client(),
    ))
    .await
    .unwrap();
    let outcome = session
        .run(CodingAgentOperation::Prompt(prompt_options(
            &cwd,
            api,
            "plan feature",
        )))
        .await
        .unwrap();
    assert_eq!(outcome.final_text(), Some("parent ready"));
    let pending = session.pending_delegation_confirmations();
    if pending.is_empty() {
        drop(session);
        let mut reopened = CodingAgentSession::open(persistent_confirmation_session_options(
            &cwd,
            &sessions,
            "sess_delegation_pending_approve",
            _provider_guard.ai_client(),
        ))
        .await
        .unwrap();
        assert!(reopened.pending_delegation_confirmations().is_empty());
        let export = match reopened
            .run(CodingAgentOperation::ExportCurrent)
            .await
            .unwrap()
        {
            CodingAgentOperationOutcome::Export(value) => value,
            other => panic!("expected export outcome, got {other:?}"),
        };
        assert!(export.transcript.iter().any(|item| matches!(
            item,
            CodingAgentSessionExportItem::Delegation { status, .. } if status == "rejected"
        )));
        return;
    }
    assert_eq!(pending.len(), 1);
    let operation_id = pending[0].operation_id.clone();
    let tool_call_id = pending[0].tool_call_id.clone();
    drop(session);

    let mut reopened = CodingAgentSession::open(persistent_confirmation_session_options(
        &cwd,
        &sessions,
        "sess_delegation_pending_approve",
        _provider_guard.ai_client(),
    ))
    .await
    .unwrap();
    let mut events = reopened.subscribe_product_events_public();
    assert_eq!(reopened.pending_delegation_confirmations().len(), 1);

    let outcome = reopened
        .run(CodingAgentOperation::ApproveDelegation {
            operation_id: operation_id.clone(),
            tool_call_id: tool_call_id.clone(),
        })
        .await
        .unwrap();
    assert!(matches!(
        outcome,
        CodingAgentOperationOutcome::DelegationApproved
    ));
    assert!(reopened.pending_delegation_confirmations().is_empty());
    drop(reopened);

    {
        let calls = calls.lock().unwrap();
        assert_eq!(
            calls.len(),
            3,
            "restored approval should run exactly one child provider call"
        );
        assert_eq!(user_texts(&calls[2].context), vec!["implement parser"]);
        assert_eq!(
            calls[2].context.system_prompt.as_deref(),
            Some("Coder child instructions.")
        );
    }

    let events = drain_events(&mut events);
    assert_event_count(&events, "Delegation(Approved)", 1);
    assert_event_count(&events, "Delegation(Started)", 1);
    assert_event_count(&events, "Delegation(Completed)", 1);

    let mut reopened_again = CodingAgentSession::open(persistent_confirmation_session_options(
        &cwd,
        &sessions,
        "sess_delegation_pending_approve",
        _provider_guard.ai_client(),
    ))
    .await
    .unwrap();
    assert!(reopened_again.pending_delegation_confirmations().is_empty());
    let export = match reopened_again
        .run(CodingAgentOperation::ExportCurrent)
        .await
        .unwrap()
    {
        CodingAgentOperationOutcome::Export(value) => value,
        other => panic!("expected export outcome, got {other:?}"),
    };
    assert!(
        export.transcript.iter().any(|item| matches!(
            item,
            CodingAgentSessionExportItem::Delegation {
                target_id,
                task,
                status,
                summary: Some(summary),
                ..
            } if target_id.as_str() == "coder"
                && task == "implement parser"
                && status == "completed"
                && summary == "child result"
        )),
        "approved delegation terminal result must survive replay: {:#?}",
        export.transcript
    );
}

#[tokio::test]
async fn reopened_persistent_session_rejects_restored_delegation_confirmation() {
    let temp = tempdir().unwrap();
    let cwd = temp.path().join("workspace");
    let global = temp.path().join("global");
    let sessions = temp.path().join("sessions");
    fs::create_dir_all(&global).unwrap();
    write_confirmation_agent_profiles(&cwd);
    let _env_guard = EnvGuard::with_evo_dir(global);

    let api = "delegation-persistent-reject-api";
    let calls = Arc::new(Mutex::new(Vec::new()));
    let _provider_guard = ProviderGuard::register(
        api,
        calls.clone(),
        vec![
            ScriptedResponse::tool_call(
                "tool_delegate_agent",
                "delegate_agent",
                serde_json::json!({"agent_id": "coder", "task": "implement parser"}),
            ),
            ScriptedResponse::text("parent ready"),
        ],
    );

    let mut session = CodingAgentSession::create(persistent_confirmation_session_options(
        &cwd,
        &sessions,
        "sess_delegation_pending_reject",
        _provider_guard.ai_client(),
    ))
    .await
    .unwrap();
    let outcome = session
        .run(CodingAgentOperation::Prompt(prompt_options(
            &cwd,
            api,
            "plan feature",
        )))
        .await
        .unwrap();
    assert_eq!(outcome.final_text(), Some("parent ready"));
    let pending = session.pending_delegation_confirmations();
    if pending.is_empty() {
        drop(session);
        let reopened = CodingAgentSession::open(persistent_confirmation_session_options(
            &cwd,
            &sessions,
            "sess_delegation_pending_reject",
            _provider_guard.ai_client(),
        ))
        .await
        .unwrap();
        assert!(reopened.pending_delegation_confirmations().is_empty());
        assert_eq!(calls.lock().unwrap().len(), 2);
        return;
    }
    assert_eq!(pending.len(), 1);
    let operation_id = pending[0].operation_id.clone();
    let tool_call_id = pending[0].tool_call_id.clone();
    drop(session);

    let mut reopened = CodingAgentSession::open(persistent_confirmation_session_options(
        &cwd,
        &sessions,
        "sess_delegation_pending_reject",
        _provider_guard.ai_client(),
    ))
    .await
    .unwrap();
    let mut events = reopened.subscribe_product_events_public();
    assert_eq!(reopened.pending_delegation_confirmations().len(), 1);

    let outcome = reopened
        .run(CodingAgentOperation::RejectDelegation {
            operation_id: operation_id.clone(),
            tool_call_id: tool_call_id.clone(),
            reason: "not now".into(),
        })
        .await
        .unwrap();
    assert!(matches!(
        outcome,
        CodingAgentOperationOutcome::DelegationRejected
    ));
    assert!(reopened.pending_delegation_confirmations().is_empty());
    drop(reopened);

    {
        let calls = calls.lock().unwrap();
        assert_eq!(
            calls.len(),
            2,
            "restored rejection should not run child work"
        );
    }

    let events = drain_events(&mut events);
    assert_event_count(&events, "Delegation(Rejected)", 1);
    assert_event_count(&events, "Delegation(Approved)", 0);
    assert_event_count(&events, "Delegation(Started)", 0);
    assert_event_count(&events, "Delegation(Completed)", 0);

    let reopened_again = CodingAgentSession::open(persistent_confirmation_session_options(
        &cwd,
        &sessions,
        "sess_delegation_pending_reject",
        _provider_guard.ai_client(),
    ))
    .await
    .unwrap();
    assert!(reopened_again.pending_delegation_confirmations().is_empty());
}

#[tokio::test]
async fn prompt_emits_failed_lifecycle_for_approved_agent_delegation_failure() {
    let temp = tempdir().unwrap();
    let cwd = temp.path().join("workspace");
    let global = temp.path().join("global");
    fs::create_dir_all(&global).unwrap();
    write_file(
        cwd.join(".evo/agents/delegating-planner.toml"),
        r#"
schema_version = 1
id = "delegating-planner"
display_name = "Delegating Planner"

[delegation]
allow_delegate_agent = true
max_depth = 1
max_parallel_children = 1
require_confirmation = "never"
allowed_agents = ["available-helper"]
"#,
    );
    write_file(
        cwd.join(".evo/agents/available-helper.toml"),
        r#"
schema_version = 1
id = "available-helper"
display_name = "Available Helper"
"#,
    );
    let _env_guard = EnvGuard::with_evo_dir(global);

    let api = "delegation-child-failure-api";
    let calls = Arc::new(Mutex::new(Vec::new()));
    let _provider_guard = ProviderGuard::register(
        api,
        calls.clone(),
        vec![
            ScriptedResponse::tool_call(
                "tool_delegate_agent",
                "delegate_agent",
                serde_json::json!({"agent_id": "available-helper", "task": "implement parser"}),
            ),
            ScriptedResponse::PartialThenError {
                partial: "child attempted work".into(),
                error: "child provider failed".into(),
            },
            ScriptedResponse::text("parent ready"),
        ],
    );

    let mut session = CodingAgentSession::non_persistent(
        CodingAgentSessionOptions::new()
            .with_ai_client(_provider_guard.ai_client())
            .with_cwd(&cwd)
            .with_default_agent_profile_id("delegating-planner"),
    )
    .await
    .unwrap();
    let mut events = session.subscribe_product_events_public();

    let outcome = session
        .run(CodingAgentOperation::Prompt(prompt_options(
            &cwd,
            api,
            "plan feature",
        )))
        .await
        .unwrap();

    assert_eq!(outcome.final_text(), Some("parent ready"));
    let calls = calls.lock().unwrap();
    assert_eq!(
        calls.len(),
        3,
        "approved child failure should call the child provider exactly once"
    );
    assert_eq!(user_texts(&calls[0].context), vec!["plan feature"]);
    assert_eq!(user_texts(&calls[1].context), vec!["implement parser"]);
    assert_eq!(user_texts(&calls[2].context), vec!["plan feature"]);

    let events = drain_events(&mut events);
    assert_event_count(&events, "Delegation(Approved)", 1);
    assert_event_count(&events, "Delegation(Started)", 1);
    assert_event_count(&events, "Agent(InvocationFailed)", 1);
    assert_event_count(&events, "Delegation(Failed)", 1);
    assert_event_count(&events, "Delegation(Completed)", 0);
    let snapshot = session
        .connect(CodingAgentClientId::new(
            "delegation-failure-terminal-check",
        ))
        .unwrap()
        .state()
        .unwrap();
    assert!(
        snapshot
            .context
            .operations
            .iter()
            .all(|operation| operation.status != CodingAgentOperationStatus::Running),
        "failed delegation must terminalize every projected child operation: {:#?}",
        snapshot.context.operations
    );
}

fn prompt_options(cwd: &Path, api: &str, prompt: &str) -> PromptTurnOptions {
    prompt_options_with_tools(cwd, api, prompt, Vec::new())
}

fn prompt_options_with_tools(
    cwd: &Path,
    api: &str,
    prompt: &str,
    tools: Vec<AgentTool>,
) -> PromptTurnOptions {
    support::prompt_options(cwd, api, prompt, tools, 4)
}

fn persistent_confirmation_session_options(
    cwd: &Path,
    sessions: &Path,
    session_id: &str,
    ai_client: ai::api::client::AiClient,
) -> CodingAgentSessionOptions {
    CodingAgentSessionOptions::new()
        .with_ai_client(ai_client)
        .with_cwd(cwd)
        .with_session_id(session_id)
        .with_session_log_root(sessions)
        .with_default_agent_profile_id("delegating-planner")
}

fn write_confirmation_agent_profiles(cwd: &Path) {
    write_file(
        cwd.join(".evo/agents/delegating-planner.toml"),
        r#"
schema_version = 1
id = "delegating-planner"
display_name = "Delegating Planner"

[delegation]
allow_delegate_agent = true
max_depth = 1
max_parallel_children = 1
require_confirmation = "always"
allowed_agents = ["coder"]
"#,
    );
    write_file(
        cwd.join(".evo/agents/coder.toml"),
        r#"
schema_version = 1
id = "coder"
display_name = "Coder"
system_prompt = "Coder child instructions."
"#,
    );
}

trait PromptOutcomeExt {
    fn final_text(&self) -> Option<&str>;
}

impl PromptOutcomeExt for coding_agent::api::operation::PromptTurnOutcome {
    fn final_text(&self) -> Option<&str> {
        match self {
            coding_agent::api::operation::PromptTurnOutcome::Success { final_text, .. } => {
                Some(final_text.as_str())
            }
            _ => None,
        }
    }
}

impl PromptOutcomeExt for CodingAgentOperationOutcome {
    fn final_text(&self) -> Option<&str> {
        match self {
            CodingAgentOperationOutcome::Prompt(outcome) => outcome.final_text(),
            other => panic!("expected prompt outcome, got {other:?}"),
        }
    }
}

fn parent_only_tool() -> AgentTool {
    AgentTool::new_text(
        "parent_only",
        "parent-only capability",
        serde_json::json!({"type": "object"}),
        |_context, _args| async { Ok("parent-only".to_owned()) },
    )
}

fn read_fixture_tool(cwd: &Path) -> AgentTool {
    let cwd = cwd.to_path_buf();
    AgentTool::new_text(
        "read",
        "read a workspace fixture",
        serde_json::json!({
            "type": "object",
            "properties": { "path": { "type": "string" } },
            "required": ["path"]
        }),
        move |_context, arguments| {
            let cwd = cwd.clone();
            async move {
                let path = arguments["path"]
                    .as_str()
                    .expect("scripted read call should include a path");
                Ok(fs::read_to_string(cwd.join(path)).expect("fixture should be readable"))
            }
        },
    )
}

fn write_file(path: impl AsRef<Path>, content: &str) {
    let path = path.as_ref();
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, content).unwrap();
}

fn drain_events(receiver: &mut CodingAgentProductEventReceiver) -> Vec<CodingAgentProductEvent> {
    let mut events = Vec::new();
    while let Ok(Some(event)) = receiver.try_recv() {
        events.push(event);
    }
    events
}

fn assert_event_count(events: &[CodingAgentProductEvent], kind: &str, expected: usize) {
    let (expected_family, expected_kind) = kind
        .rsplit_once('(')
        .map(|(family, value)| (Some(family), value.trim_end_matches(')')))
        .unwrap_or((None, kind));
    let expected_family = expected_family.map(pascal_to_snake);
    assert_eq!(
        events
            .iter()
            .filter(|event| {
                expected_family
                    .as_deref()
                    .is_none_or(|family| event.family_typed().as_str() == family)
                    && event.kind_name() == pascal_to_snake(expected_kind)
            })
            .count(),
        expected,
        "unexpected {kind} event count: {events:#?}"
    );
}

fn pascal_to_snake(value: &str) -> String {
    value
        .chars()
        .enumerate()
        .flat_map(|(index, character)| {
            (index > 0 && character.is_ascii_uppercase())
                .then_some('_')
                .into_iter()
                .chain(std::iter::once(character.to_ascii_lowercase()))
        })
        .collect()
}

fn tool_names(context: &Context) -> Vec<String> {
    context
        .tools
        .as_ref()
        .map(|tools| tools.iter().map(|tool| tool.name.clone()).collect())
        .unwrap_or_default()
}

fn user_texts(context: &Context) -> Vec<String> {
    context
        .messages
        .iter()
        .filter_map(|message| match message {
            Message::User { content } => Some(
                content
                    .iter()
                    .filter_map(|block| match block {
                        ContentBlock::Text { text, .. } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n"),
            ),
            _ => None,
        })
        .collect()
}

fn context_texts(context: &Context) -> Vec<String> {
    let mut texts = Vec::new();
    for message in &context.messages {
        let content = match message {
            Message::User { content }
            | Message::Assistant { content }
            | Message::ToolResult { content, .. } => content,
        };
        for block in content {
            if let ContentBlock::Text { text, .. } = block {
                texts.push(text.clone());
            }
        }
    }
    texts
}

#[derive(Debug, Clone)]
struct RecordedCall {
    context: Context,
}

#[derive(Debug, Clone)]
enum ScriptedResponse {
    Text(String),
    PartialThenError {
        partial: String,
        error: String,
    },
    ToolCall {
        id: String,
        name: String,
        arguments: serde_json::Value,
    },
}

impl ScriptedResponse {
    fn text(text: impl Into<String>) -> Self {
        Self::Text(text.into())
    }

    fn tool_call(
        id: impl Into<String>,
        name: impl Into<String>,
        arguments: serde_json::Value,
    ) -> Self {
        Self::ToolCall {
            id: id.into(),
            name: name.into(),
            arguments,
        }
    }
}

struct ScriptedProvider {
    calls: Arc<Mutex<Vec<RecordedCall>>>,
    responses: Arc<Mutex<Vec<ScriptedResponse>>>,
}

#[derive(Default)]
struct StalledChildProvider {
    calls: AtomicUsize,
    child_started: tokio::sync::Notify,
    child_stream_dropped: Arc<AtomicBool>,
}

struct ChildStreamDropGuard(Arc<AtomicBool>);

impl Drop for ChildStreamDropGuard {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

impl ApiProvider for StalledChildProvider {
    fn stream(&self, model: &Model, _ctx: Context, _opts: Option<StreamOptions>) -> EventStream {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        let model_id = model.id.clone();
        match call {
            0 => Box::pin(stream! {
                let mut message = AssistantMessage::empty("operation-tree-test", &model_id);
                message.content.push(ContentBlock::ToolCall {
                    id: "tool_delegate_stalled".into(),
                    name: "delegate_agent".into(),
                    arguments: serde_json::json!({
                        "agent_id": "coder",
                        "task": "wait until cancelled"
                    }),
                    thought_signature: None,
                });
                message.stop_reason = StopReason::ToolUse;
                yield AssistantMessageEvent::Done {
                    reason: StopReason::ToolUse,
                    message,
                };
            }),
            1 => {
                self.child_started.notify_one();
                let drop_guard = ChildStreamDropGuard(self.child_stream_dropped.clone());
                Box::pin(futures::stream::poll_fn(move |_| {
                    let _keep_guard_alive = &drop_guard;
                    std::task::Poll::Pending
                }))
            }
            _ => panic!("parent continuation must not start after stalled child cancellation"),
        }
    }
}

impl ApiProvider for ScriptedProvider {
    fn stream(&self, model: &Model, ctx: Context, _opts: Option<StreamOptions>) -> EventStream {
        self.calls
            .lock()
            .unwrap()
            .push(RecordedCall { context: ctx });
        let response = self.responses.lock().unwrap().remove(0);
        let model_id = model.id.clone();
        Box::pin(stream! {
            let mut message = AssistantMessage::empty("delegation-execution-test", &model_id);
            message.provider = Some("delegation-execution-test".into());
            match response {
                ScriptedResponse::Text(text) => {
                    message.content.push(ContentBlock::Text {
                        text,
                        text_signature: None,
                    });
                    message.stop_reason = StopReason::Stop;
                    yield AssistantMessageEvent::Done {
                        reason: StopReason::Stop,
                        message,
                    };
                }
                ScriptedResponse::PartialThenError { partial: text, error } => {
                    message.content.push(ContentBlock::Text {
                        text: text.clone(),
                        text_signature: None,
                    });
                    yield AssistantMessageEvent::TextDelta {
                        content_index: 0,
                        delta: text,
                        partial: message.clone(),
                    };
                    message.stop_reason = StopReason::Error;
                    message.error_message = Some(error);
                    yield AssistantMessageEvent::Error {
                        reason: StopReason::Error,
                        message,
                    };
                }
                ScriptedResponse::ToolCall { id, name, arguments } => {
                    message.content.push(ContentBlock::ToolCall {
                        id,
                        name,
                        arguments,
                        thought_signature: None,
                    });
                    message.stop_reason = StopReason::ToolUse;
                    yield AssistantMessageEvent::Done {
                        reason: StopReason::ToolUse,
                        message,
                    };
                }
            }
        })
    }
}

struct ProviderGuard {
    _guard: RegistryProviderGuard,
}

impl ProviderGuard {
    fn ai_client(&self) -> ai::api::client::AiClient {
        self._guard.ai_client()
    }

    fn register(
        api: &str,
        calls: Arc<Mutex<Vec<RecordedCall>>>,
        responses: Vec<ScriptedResponse>,
    ) -> Self {
        let guard = RegistryProviderGuard::register(
            api,
            Arc::new(ScriptedProvider {
                calls,
                responses: Arc::new(Mutex::new(responses)),
            }),
        );
        Self { _guard: guard }
    }
}
