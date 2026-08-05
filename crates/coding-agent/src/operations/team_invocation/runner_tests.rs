use std::sync::Arc;

use tokio_util::sync::CancellationToken;
use tool_contract::api::definition::ToolId;
use workspace_runtime::api::{WorkspaceAccessHandle, WorktreeRegistry};

use super::*;
use crate::app::bootstrap::PromptInvocation;
use crate::application::capability::OperationCapabilitySnapshot;
use crate::application::operation::control::OperationControl;
use crate::application::snapshot::SnapshotCoordinator;
use crate::kernel::capability::{
    ActorId, CapabilityGeneration, CommandCapabilitySet, ToolCapabilitySet,
};
use crate::operations::prompt::context::PromptTurnOptions;
use crate::profiles::{AgentProfile, DelegationPolicy, ProfileSource, SupervisionPolicy};
use crate::services::event::EventService;

fn writer_profile() -> AgentProfile {
    AgentProfile {
        schema_version: 1,
        id: ProfileId::from("writer"),
        display_name: "Writer".into(),
        description: None,
        model: None,
        system_prompt: None,
        tools: vec![ToolId::new("write").expect("write id")],
        skills: Vec::new(),
        supervision: SupervisionPolicy::Session,
        delegation: DelegationPolicy::default(),
        source: ProfileSource::BuiltIn,
        path: None,
    }
}

#[tokio::test]
async fn cancelled_team_member_creation_releases_registry_record() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().join("workspace");
    std::fs::create_dir_all(&workspace).expect("workspace");
    let registry = WorktreeRegistry::open_with_capacity(temp.path().join("registry"), Some(4))
        .expect("registry");
    let control =
        OperationControl::with_snapshot_coordinator(Arc::new(SnapshotCoordinator::default()))
            .with_worktree_registry(Arc::new(registry.clone()));
    let event_service =
        EventService::with_snapshot_coordinator(Arc::new(SnapshotCoordinator::default()));
    let parent = OperationCapabilitySnapshot {
        generation: CapabilityGeneration::new(1),
        operation_id: "team-parent".into(),
        actor: ActorId::Client,
        model: None,
        tools: ToolCapabilitySet::from_ids([
            ToolId::new("read").expect("read id"),
            ToolId::new("write").expect("write id"),
        ]),
        commands: CommandCapabilitySet::default(),
        workspace: Some(WorkspaceAccessHandle::open_source(workspace).expect("workspace handle")),
        session_read: None,
        session_write: None,
        ui: None,
    };
    let mut context = AgentTeamContext::new(
        AgentTeamOptions::new(
            "team",
            "write a file",
            PromptTurnOptions::new(PromptInvocation::Text("unused".into())),
        ),
        ProfileRegistry::default(),
        event_service,
        control,
        "team-parent".into(),
    )
    .with_parent_capability_snapshot(parent);
    context.member_profiles = vec![writer_profile()];

    let cancellation = CancellationToken::new();
    cancellation.cancel();
    assert!(matches!(
        context.run_member_agents(Some(&cancellation)).await,
        Err(CodingSessionError::Cancelled)
    ));
    assert!(registry.load_all().expect("load records").is_empty());
}
