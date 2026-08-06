//! ARC-730：subagent 事件产品接线测试。
//!
//! 驱动 `delegation::execution::execute_agent`（真实 child agent 运行），
//! 断言 `subagent_start` / `subagent_stop` 经 `ExtensionEventDispatch`
//! 到达 extension 事件 sink。挂载点：`operations::delegation`。

use std::sync::Arc;

use crate::mutex::MutexExt;
use ai::api::provider::faux::FauxProvider;
use ai_protocol::api::conversation::StopReason;
use ai_protocol::api::model::{Model, ModelCost, ModelInput};

use super::execution::execute_agent;
use super::worktree_tests::{control_with_registry, parent_snapshot, profile_registry_with_writer};
use crate::app::bootstrap::{PromptInvocation, SessionRunOptions};
use crate::app::prompt_runtime::PromptRuntimeOptions;
use crate::application::snapshot::SnapshotCoordinator;
use crate::kernel::capability::CapabilityGeneration;
use crate::operations::prompt::context::{DelegationRequest, PromptTurnOptions};
use crate::profiles::{ProfileId, ProfileKind};
use crate::services::event::EventService;
use crate::services::ports::{ExtensionEventDispatch, ExtensionEventSink};
use crate::test_support::ProviderGuard;
use extension_host::api::{ExtensionEventKind, ExtensionEventPayload, HookGate};
use workspace_runtime::api::WorktreeRegistry;

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

/// 记录 extension 事件的 sink（测试断言）。
#[derive(Debug, Clone, Default)]
struct RecordingExtensionSink {
    events: Arc<std::sync::Mutex<Vec<(ExtensionEventKind, ExtensionEventPayload)>>>,
}

impl ExtensionEventSink for RecordingExtensionSink {
    fn submit(
        &self,
        kind: ExtensionEventKind,
        _session_id: &str,
        _workspace_root: &str,
        payload: ExtensionEventPayload,
    ) {
        self.events
            .lock_or_recover("recording extension sink")
            .push((kind, payload));
    }

    fn hook_gate(&self) -> Option<Arc<HookGate>> {
        None
    }
}

#[tokio::test]
async fn delegated_agent_emits_subagent_start_and_stop_events() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().join("workspace");
    std::fs::create_dir_all(&workspace).expect("workspace dir");

    let registry = WorktreeRegistry::open_with_capacity(temp.path().join("registry"), Some(4))
        .expect("registry opens");
    let control = control_with_registry(registry.clone());
    let _parent_guard = control
        .begin_root_with_capability_generation(
            crate::application::operation::OperationClass::NonSessionRoot,
            crate::kernel::operation::OperationKind::Prompt,
            "parent-operation".into(),
            CapabilityGeneration::new(1),
        )
        .expect("parent operation is active");
    let event_service =
        EventService::with_snapshot_coordinator(Arc::new(SnapshotCoordinator::default()));
    let parent = parent_snapshot(&workspace);

    // child agent 的 provider 响应（writer 子代跑一轮即停）。
    let api = "subagent-events-child";
    let provider_guard = ProviderGuard::register(
        api,
        Arc::new(FauxProvider::with_call_queue(vec![
            FauxProvider::text_call("delegation done", StopReason::Stop),
        ])),
    );
    let prompt_options = PromptTurnOptions::from_prompt_runtime_options(PromptRuntimeOptions {
        model: model(api),
        api_key: None,
        auth_diagnostics: Vec::new(),
        system_prompt: Some("system".into()),
        max_turns: Some(2),
        tools: Vec::new(),
        register_builtins: true,
        ai_client: Some(provider_guard.ai_client()),
        session: Some(SessionRunOptions::enabled(workspace.clone())),
        session_target: None,
        session_name: None,
        thinking_level: None,
        tool_execution: None,
        resources: agent_core::api::agent::AgentResources::default(),
        settings: None,
        invocation: PromptInvocation::Text("write isolated.txt".into()),
    });

    let profiles = profile_registry_with_writer(temp.path());
    let request = DelegationRequest {
        operation_id: "parent-operation".into(),
        turn_id: "parent-turn".into(),
        tool_call_id: "call-delegate".into(),
        requesting_profile_id: ProfileId::from("default"),
        target_kind: ProfileKind::Agent,
        target_id: ProfileId::from("writer"),
        task: "write isolated.txt".into(),
    };
    let sink = Arc::new(RecordingExtensionSink::default());
    let dispatch = ExtensionEventDispatch::from_parts(
        Some(sink.clone() as Arc<dyn ExtensionEventSink>),
        "subagent-session",
        workspace.to_string_lossy(),
    );

    let outcome = execute_agent(
        profiles,
        event_service,
        control.clone(),
        &request,
        prompt_options,
        1,
        Vec::new(),
        Some(parent),
        None,
        None,
        dispatch,
    )
    .await;
    outcome.execution.expect("delegated agent completes");

    let events = sink
        .events
        .lock_or_recover("recording extension sink")
        .clone();
    let starts = events
        .iter()
        .filter(|(kind, _)| *kind == ExtensionEventKind::SubagentStart)
        .collect::<Vec<_>>();
    let stops = events
        .iter()
        .filter(|(kind, _)| *kind == ExtensionEventKind::SubagentStop)
        .collect::<Vec<_>>();
    assert_eq!(starts.len(), 1, "exactly one subagent_start: {events:?}");
    assert_eq!(stops.len(), 1, "exactly one subagent_stop: {events:?}");
    match &starts[0].1 {
        ExtensionEventPayload::SubagentStart { subagent_type } => {
            assert_eq!(subagent_type, "writer");
        }
        other => panic!("expected SubagentStart, got {other:?}"),
    }
    match &stops[0].1 {
        ExtensionEventPayload::SubagentStop {
            subagent_type,
            phase,
            stop_reason,
        } => {
            assert_eq!(subagent_type, "writer");
            assert_eq!(*phase, extension_host::api::SubagentStopPhase::Gate);
            assert_eq!(stop_reason.as_deref(), Some("completed"));
        }
        other => panic!("expected SubagentStop, got {other:?}"),
    }
}

/// 无 host（Noop sink）时接线不 panic，行为与无扩展一致。
#[tokio::test]
async fn delegation_without_extension_host_stays_noop() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().join("workspace");
    std::fs::create_dir_all(&workspace).expect("workspace dir");

    let registry = WorktreeRegistry::open_with_capacity(temp.path().join("registry"), Some(4))
        .expect("registry opens");
    let control = control_with_registry(registry.clone());
    let _parent_guard = control
        .begin_root_with_capability_generation(
            crate::application::operation::OperationClass::NonSessionRoot,
            crate::kernel::operation::OperationKind::Prompt,
            "parent-operation".into(),
            CapabilityGeneration::new(1),
        )
        .expect("parent operation is active");
    let event_service = EventService::with_snapshot_coordinator(Arc::new(
        crate::application::snapshot::SnapshotCoordinator::default(),
    ));
    let parent = parent_snapshot(&workspace);

    let api = "subagent-events-noop";
    let provider_guard = ProviderGuard::register(
        api,
        Arc::new(FauxProvider::with_call_queue(vec![
            FauxProvider::text_call("delegation done", StopReason::Stop),
        ])),
    );
    let prompt_options = PromptTurnOptions::from_prompt_runtime_options(PromptRuntimeOptions {
        model: model(api),
        api_key: None,
        auth_diagnostics: Vec::new(),
        system_prompt: Some("system".into()),
        max_turns: Some(2),
        tools: Vec::new(),
        register_builtins: true,
        ai_client: Some(provider_guard.ai_client()),
        session: Some(SessionRunOptions::enabled(workspace.clone())),
        session_target: None,
        session_name: None,
        thinking_level: None,
        tool_execution: None,
        resources: agent_core::api::agent::AgentResources::default(),
        settings: None,
        invocation: PromptInvocation::Text("write isolated.txt".into()),
    });

    let profiles = profile_registry_with_writer(temp.path());
    let request = DelegationRequest {
        operation_id: "parent-operation".into(),
        turn_id: "parent-turn".into(),
        tool_call_id: "call-delegate".into(),
        requesting_profile_id: ProfileId::from("default"),
        target_kind: ProfileKind::Agent,
        target_id: ProfileId::from("writer"),
        task: "write isolated.txt".into(),
    };
    let outcome = execute_agent(
        profiles,
        event_service,
        control.clone(),
        &request,
        prompt_options,
        1,
        Vec::new(),
        Some(parent),
        None,
        None,
        ExtensionEventDispatch::none(),
    )
    .await;
    outcome.execution.expect("delegated agent completes");
}
