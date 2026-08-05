//! ARC-330: child workspace isolation tests.
//!
//! Write-capable delegated children must run in their own managed worktree
//! (never the parent's directory), and every terminal path must release that
//! worktree. Read-only and projectless children follow their own policies.

use std::path::Path;
use std::sync::Arc;

use agent_core::api::agent::AgentResources;
use ai::api::provider::faux::{FauxProvider, FauxResponse, FauxToolCall};
use ai_protocol::api::conversation::StopReason;
use ai_protocol::api::model::{Model, ModelCost, ModelInput};
use tokio_util::sync::CancellationToken;
use tool_contract::api::definition::ToolId;

use super::worktree::{
    ChildWorkspaceBinding, ChildWorkspacePolicy, ChildWorktreeLease, bind_child_workspace,
};
use crate::app::bootstrap::{PromptInvocation, SessionRunOptions};
use crate::app::prompt_runtime::PromptRuntimeOptions;
use crate::application::capability::OperationCapabilitySnapshot;
use crate::application::operation::control::OperationControl;
use crate::application::snapshot::SnapshotCoordinator;
use crate::kernel::capability::{
    ActorId, CapabilityGeneration, CommandCapabilitySet, ModelCapability, ToolCapabilitySet,
};
use crate::kernel::error::CodingSessionError;
use crate::operations::agent_invocation::runner::{
    AgentInvocationContext, AgentInvocationOptions, AgentInvocationRunner,
};
use crate::operations::prompt::context::PromptTurnOptions;
use crate::profiles::{
    AgentProfile, DelegationConfirmationMode, DelegationPolicy, ProfileId, ProfileRegistry,
    ProfileRegistryOptions, ProfileSource, SupervisionPolicy,
};
use crate::services::event::EventService;
use crate::test_support::ProviderGuard;
use workspace_runtime::api::{WorkspaceAccessHandle, WorkspaceKind, WorktreeRegistry};

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

fn parent_snapshot(root: &Path) -> OperationCapabilitySnapshot {
    OperationCapabilitySnapshot {
        generation: CapabilityGeneration::new(1),
        operation_id: "parent-operation".into(),
        actor: ActorId::Client,
        model: Some(ModelCapability {
            profile_id: Some(ProfileId::from("default")),
        }),
        tools: ToolCapabilitySet::from_ids([
            ToolId::new("read").unwrap(),
            ToolId::new("write").unwrap(),
            ToolId::new("bash").unwrap(),
        ]),
        commands: CommandCapabilitySet::default(),
        workspace: Some(
            WorkspaceAccessHandle::open_source(root.to_path_buf())
                .expect("parent workspace handle"),
        ),
        session_read: None,
        session_write: None,
        ui: None,
    }
}

fn write_profile() -> AgentProfile {
    AgentProfile {
        schema_version: 1,
        id: ProfileId::from("writer"),
        display_name: "Writer".into(),
        description: None,
        model: None,
        system_prompt: None,
        tools: vec![ToolId::new("write").unwrap()],
        skills: Vec::new(),
        supervision: SupervisionPolicy::Session,
        delegation: DelegationPolicy {
            allow_delegate_agent: false,
            allow_delegate_team: false,
            max_depth: 0,
            max_parallel_children: 1,
            require_confirmation: DelegationConfirmationMode::Never,
            allowed_agents: Vec::new(),
            allowed_teams: Vec::new(),
        },
        source: ProfileSource::BuiltIn,
        path: None,
    }
}

fn control_with_registry(registry: WorktreeRegistry) -> OperationControl {
    OperationControl::with_snapshot_coordinator(Arc::new(SnapshotCoordinator::default()))
        .with_worktree_registry(Arc::new(registry))
}

fn profile_registry_with_writer(root: &Path) -> ProfileRegistry {
    let agents = root.join("profiles").join("agents");
    std::fs::create_dir_all(&agents).expect("profile dir");
    std::fs::write(
        agents.join("writer.toml"),
        r#"schema_version = 1
id = "writer"
display_name = "Writer"
tools = ["write"]
"#,
    )
    .expect("writer profile");
    ProfileRegistry::load(ProfileRegistryOptions::new().with_user_root(root.join("profiles")))
        .expect("profiles load")
}

#[tokio::test]
async fn write_child_gets_its_own_worktree_and_releases_it_on_success() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().join("workspace");
    std::fs::create_dir_all(&workspace).expect("workspace dir");
    std::fs::write(workspace.join("file.txt"), "v1").expect("source file");

    let registry = WorktreeRegistry::open_with_capacity(temp.path().join("registry"), Some(4))
        .expect("registry opens");
    let control = control_with_registry(registry.clone());
    let event_service =
        EventService::with_snapshot_coordinator(Arc::new(SnapshotCoordinator::default()));
    let parent = parent_snapshot(&workspace);

    let api = "worktree-isolation-writer";
    let provider_guard = ProviderGuard::register(
        api,
        Arc::new(FauxProvider::with_call_queue(vec![
            FauxProvider::single_call(
                vec![FauxResponse {
                    text_deltas: Vec::new(),
                    thinking_deltas: Vec::new(),
                    tool_calls: vec![FauxToolCall {
                        id: "call-write".into(),
                        name: "write".into(),
                        deltas: vec!["{}".into()],
                        final_arguments: serde_json::json!({
                            "path": "isolated.txt",
                            "content": "written by child"
                        }),
                    }],
                }],
                StopReason::ToolUse,
            ),
            FauxProvider::text_call("delegation done", StopReason::Stop),
        ])),
    );

    let prompt_options = PromptTurnOptions::from_prompt_runtime_options(PromptRuntimeOptions {
        model: model(api),
        api_key: None,
        auth_diagnostics: Vec::new(),
        system_prompt: Some("system".into()),
        max_turns: Some(4),
        tools: Vec::new(),
        register_builtins: true,
        ai_client: Some(provider_guard.ai_client()),
        session: Some(SessionRunOptions::enabled(workspace.clone())),
        session_target: None,
        session_name: None,
        thinking_level: None,
        tool_execution: None,
        resources: AgentResources::default(),
        settings: None,
        invocation: PromptInvocation::Text("write isolated.txt".into()),
    });

    let profiles = profile_registry_with_writer(temp.path());
    let _parent_guard = control
        .begin_root_with_capability_generation(
            crate::application::operation::OperationClass::NonSessionRoot,
            crate::kernel::operation::OperationKind::Prompt,
            "parent-operation".into(),
            CapabilityGeneration::new(1),
        )
        .expect("parent operation is active");
    let mut context = AgentInvocationContext::new(
        AgentInvocationOptions::new(
            ProfileId::from("writer"),
            "write isolated.txt",
            prompt_options,
        ),
        profiles,
        event_service,
        control,
        "parent-operation".into(),
    )
    .with_parent_capability_snapshot(parent);

    AgentInvocationRunner::new()
        .expect("runner")
        .run_typed(&mut context, None)
        .await
        .expect("delegated write child completes");

    let child_capability = context
        .child_capability_snapshot()
        .expect("child capability captured");
    let child_root = child_capability
        .workspace
        .as_ref()
        .expect("child has an isolated workspace")
        .cwd();
    assert!(
        child_root.starts_with(registry.worktrees_root()),
        "child workspace {} must live under the registry worktrees root",
        child_root.display()
    );
    assert_ne!(
        child_root,
        workspace.as_path(),
        "child must not share parent cwd"
    );

    let child_cwd = context
        .child_context()
        .expect("child context retained")
        .options()
        .runtime()
        .expect("child runtime")
        .cwd()
        .expect("child runtime bound to worktree");
    assert_eq!(child_cwd, child_root);

    assert!(
        !workspace.join("isolated.txt").exists(),
        "parent workspace must stay byte-identical during child isolation"
    );
    assert!(
        child_root.exists(),
        "successful child worktree must be retained for review and merge"
    );
    let records = registry.load_all().expect("registry load");
    assert_eq!(
        records.len(),
        1,
        "successful child leaves one durable record"
    );
    assert_eq!(
        records[0].lifecycle,
        workspace_runtime::api::WorkspaceLifecycle::MergePending,
        "successful child worktree awaits an explicit merge or discard"
    );
}

#[tokio::test]
async fn failed_child_releases_its_worktree() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().join("workspace");
    std::fs::create_dir_all(&workspace).expect("workspace dir");
    std::fs::write(workspace.join("file.txt"), "v1").expect("source file");

    let registry = WorktreeRegistry::open_with_capacity(temp.path().join("registry"), Some(4))
        .expect("registry opens");
    let control = control_with_registry(registry.clone());
    let event_service =
        EventService::with_snapshot_coordinator(Arc::new(SnapshotCoordinator::default()));
    let parent = parent_snapshot(&workspace);

    let api = "worktree-isolation-failure";
    let provider_guard = ProviderGuard::register(
        api,
        Arc::new(FauxProvider::with_call_queue(vec![
            FauxProvider::text_call("will not finish", StopReason::ToolUse),
        ])),
    );
    let prompt_options = PromptTurnOptions::from_prompt_runtime_options(PromptRuntimeOptions {
        model: model(api),
        api_key: None,
        auth_diagnostics: Vec::new(),
        system_prompt: Some("system".into()),
        max_turns: Some(1),
        tools: Vec::new(),
        register_builtins: true,
        ai_client: Some(provider_guard.ai_client()),
        session: Some(SessionRunOptions::enabled(workspace.clone())),
        session_target: None,
        session_name: None,
        thinking_level: None,
        tool_execution: None,
        resources: AgentResources::default(),
        settings: None,
        invocation: PromptInvocation::Text("unfinished".into()),
    });
    let profiles = profile_registry_with_writer(temp.path());
    let _parent_guard = control
        .begin_root_with_capability_generation(
            crate::application::operation::OperationClass::NonSessionRoot,
            crate::kernel::operation::OperationKind::Prompt,
            "parent-operation".into(),
            CapabilityGeneration::new(1),
        )
        .expect("parent operation is active");
    let mut context = AgentInvocationContext::new(
        AgentInvocationOptions::new(ProfileId::from("writer"), "unfinished", prompt_options),
        profiles,
        event_service,
        control,
        "parent-operation".into(),
    )
    .with_parent_capability_snapshot(parent);

    let error = AgentInvocationRunner::new()
        .expect("runner")
        .run_typed(&mut context, None)
        .await
        .expect_err("child turn without a terminal assistant message fails");
    assert!(
        error.to_string().contains("tool") || error.to_string().contains("assistant"),
        "failure must surface the child turn error, got: {error}"
    );

    assert!(
        registry.load_all().expect("registry load").is_empty(),
        "failed child must release its worktree instead of retaining it"
    );
}

#[tokio::test]
async fn acquisition_fails_closed_without_a_registry() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().join("workspace");
    std::fs::create_dir_all(&workspace).expect("workspace dir");
    let control =
        OperationControl::with_snapshot_coordinator(Arc::new(SnapshotCoordinator::default()));
    let parent = parent_snapshot(&workspace);

    let error = bind_child_workspace(
        &control,
        &parent,
        "op-no-registry",
        None,
        &CancellationToken::new(),
        ChildWorkspacePolicy::Managed,
    )
    .await
    .expect_err("missing registry must fail closed");
    assert!(matches!(
        error,
        CodingSessionError::UnsupportedCapability { .. }
    ));
}

#[tokio::test]
async fn acquisition_fails_when_registry_capacity_is_exhausted() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().join("workspace");
    std::fs::create_dir_all(&workspace).expect("workspace dir");
    let registry = WorktreeRegistry::open_with_capacity(temp.path().join("registry"), Some(1))
        .expect("registry opens");
    let control = control_with_registry(registry.clone());
    let parent = parent_snapshot(&workspace);

    let first = bind_child_workspace(
        &control,
        &parent,
        "op-capacity-1",
        None,
        &CancellationToken::new(),
        ChildWorkspacePolicy::Managed,
    )
    .await
    .expect("first acquisition fits")
    .expect("lease");
    let second = bind_child_workspace(
        &control,
        &parent,
        "op-capacity-2",
        None,
        &CancellationToken::new(),
        ChildWorkspacePolicy::Managed,
    )
    .await;
    let error = second.expect_err("second acquisition must fail closed");
    assert!(
        error.to_string().contains("capacity"),
        "capacity exhaustion must be explicit, got: {error}"
    );
    let mut first = first;
    first.release().expect("first lease releases");
}

#[tokio::test]
async fn projectless_and_read_only_policies_skip_worktree_acquisition() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().join("workspace");
    std::fs::create_dir_all(&workspace).expect("workspace dir");
    let registry = WorktreeRegistry::open_with_capacity(temp.path().join("registry"), Some(4))
        .expect("registry opens");
    let control = control_with_registry(registry.clone());
    let parent = parent_snapshot(&workspace);
    let read_only = AgentProfile {
        tools: vec![ToolId::new("read").unwrap()],
        ..write_profile()
    };

    assert_eq!(
        ChildWorkspacePolicy::decide(&parent, &read_only),
        ChildWorkspacePolicy::ReadOnlyShared
    );
    let shared = bind_child_workspace(
        &control,
        &parent,
        "op-read-only",
        None,
        &CancellationToken::new(),
        ChildWorkspacePolicy::ReadOnlyShared,
    )
    .await
    .expect("read-only child needs no lease");
    assert!(shared.is_none());
    assert!(registry.load_all().expect("load").is_empty());

    let mut projectless_parent = parent_snapshot(&workspace);
    projectless_parent.workspace = None;
    let projectless = bind_child_workspace(
        &control,
        &projectless_parent,
        "op-projectless",
        None,
        &CancellationToken::new(),
        ChildWorkspacePolicy::Projectless,
    )
    .await
    .expect("projectless child needs no lease");
    assert!(projectless.is_none());
}

#[test]
fn lease_drop_retries_reclamation() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().join("workspace");
    std::fs::create_dir_all(&workspace).expect("workspace dir");
    let registry = WorktreeRegistry::open_with_capacity(temp.path().join("registry"), Some(4))
        .expect("registry opens");
    let control = control_with_registry(registry.clone());
    let parent = parent_snapshot(&workspace);

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    let lease = rt.block_on(async {
        bind_child_workspace(
            &control,
            &parent,
            "op-drop",
            None,
            &CancellationToken::new(),
            ChildWorkspacePolicy::Managed,
        )
        .await
        .expect("lease acquired")
        .expect("lease")
    });
    let root = lease.root().to_path_buf();
    assert!(root.exists(), "worktree materialized before drop");
    drop(rt);
    assert!(registry.load_all().expect("load").len() == 1);

    drop(lease);
    assert!(
        registry.load_all().expect("load").is_empty(),
        "dropped lease must reclaim its record"
    );
    assert!(!root.exists(), "dropped lease must reclaim its worktree");
}

#[test]
fn managed_binding_carries_a_typed_child_handle() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().join("workspace");
    std::fs::create_dir_all(&workspace).expect("workspace dir");
    let registry = WorktreeRegistry::open_with_capacity(temp.path().join("registry"), Some(4))
        .expect("registry opens");
    let control = control_with_registry(registry.clone());
    let parent = parent_snapshot(&workspace);

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    rt.block_on(async {
        let mut lease = bind_child_workspace(
            &control,
            &parent,
            "op-typed",
            None,
            &CancellationToken::new(),
            ChildWorkspacePolicy::Managed,
        )
        .await
        .expect("lease acquired")
        .expect("lease");
        let _: &ChildWorktreeLease = &lease;
        let handle = lease.handle().expect("typed handle");
        assert_eq!(handle.kind(), WorkspaceKind::ManagedChild);
        assert_eq!(handle.root(), lease.root());
        let binding = ChildWorkspaceBinding::Managed(handle.clone());
        assert_eq!(binding.as_managed(), Some(&handle));
        lease.release().expect("releases");
    });
}
