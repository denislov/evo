use super::*;
use crate::kernel::capability::{
    CapabilityGeneration, CommandCapabilitySet, SessionReadCapability, SessionWriteCapability,
    UiCapability,
};
use crate::profiles::{ProfileSource, SupervisionPolicy};
use tool_contract::api::definition::ToolId;
use workspace_runtime::api::WorkspaceAccessHandle;

fn parent_snapshot(root: &std::path::Path) -> OperationCapabilitySnapshot {
    OperationCapabilitySnapshot {
        generation: CapabilityGeneration::new(7),
        operation_id: "parent-operation".into(),
        actor: ActorId::Client,
        model: Some(ModelCapability {
            profile_id: Some(ProfileId::from("parent")),
        }),
        tools: ToolCapabilitySet::from_ids([
            ToolId::new("read").unwrap(),
            ToolId::new("edit").unwrap(),
            ToolId::new("bash").unwrap(),
            ToolId::new("web_search").unwrap(),
        ]),
        commands: CommandCapabilitySet::default(),
        workspace: Some(
            WorkspaceAccessHandle::open_source(root.to_path_buf())
                .expect("workspace access handle"),
        ),
        session_read: Some(SessionReadCapability { persistent: true }),
        session_write: Some(SessionWriteCapability { persistent: true }),
        ui: Some(UiCapability),
    }
}

#[test]
fn child_operation_snapshot_preserves_workspace_handles_but_drops_session_and_ui() {
    let temp = tempfile::tempdir().expect("tempdir");
    let parent = parent_snapshot(temp.path());
    let child = capability_snapshot_for_child_operation(&parent, "child-operation");

    assert_eq!(child.generation, parent.generation);
    assert_eq!(child.operation_id, "child-operation");
    assert_eq!(
        child.actor,
        ActorId::ChildOperation("parent-operation".into())
    );
    assert_eq!(
        child.workspace.as_ref().map(|handle| handle.identity()),
        parent.workspace.as_ref().map(|handle| handle.identity())
    );
    assert!(child.session_read.is_none());
    assert!(child.session_write.is_none());
    assert!(child.ui.is_none());
}

#[test]
fn delegated_profile_snapshot_intersects_explicit_tools_and_inherits_parent_server_tools() {
    let temp = tempfile::tempdir().expect("tempdir");
    let parent = parent_snapshot(temp.path());
    let profile = AgentProfile {
        schema_version: 1,
        id: ProfileId::from("reviewer"),
        display_name: "Reviewer".into(),
        description: None,
        model: None,
        system_prompt: None,
        tools: vec![
            ToolId::new("read").unwrap(),
            ToolId::new("write").unwrap(),
            ToolId::new("bash").unwrap(),
        ],
        skills: Vec::new(),
        supervision: SupervisionPolicy::Session,
        delegation: DelegationPolicy::default(),
        source: ProfileSource::BuiltIn,
        path: None,
    };

    let child = capability_snapshot_for_delegated_profile(
        &parent,
        "delegated-operation",
        &profile,
        ActorId::ChildOperation(parent.operation_id.clone()),
    );

    assert!(child.tools.allows(&ToolId::new("read").unwrap()));
    assert!(child.tools.allows(&ToolId::new("bash").unwrap()));
    assert!(!child.tools.allows(&ToolId::new("write").unwrap()));
    assert!(!child.tools.allows(&ToolId::new("edit").unwrap()));
    assert!(child.tools.allows(&ToolId::new("web_search").unwrap()));
    assert_eq!(
        child
            .model
            .as_ref()
            .and_then(|model| model.profile_id.as_ref()),
        Some(&profile.id)
    );
    assert!(child.workspace.is_some());
    assert!(child.session_read.is_none());
    assert!(child.session_write.is_none());
    assert!(child.ui.is_none());
}
