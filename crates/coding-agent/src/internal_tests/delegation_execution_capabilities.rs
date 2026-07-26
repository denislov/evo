use crate::operations::delegation::capability_snapshot_for_delegated_profile;
use crate::profiles::{
    AgentProfile, DelegationPolicy, ProfileId, ProfileRegistry, ProfileRegistryOptions,
    ProfileSource, SupervisionPolicy,
};
use crate::runtime::capability::{ActorId, ModelCapability, OperationCapabilitySnapshot};

#[test]
fn built_in_helpers_release_only_parent_granted_read_only_filesystem_capabilities() {
    let registry = ProfileRegistry::load(ProfileRegistryOptions::new()).unwrap();
    let parent = OperationCapabilitySnapshot::permissive("op_parent");

    for helper_id in ["explore", "review", "check"] {
        let profile = registry
            .agent(helper_id)
            .expect("built-in helper profile should resolve");
        let child = capability_snapshot_for_delegated_profile(
            &parent,
            format!("op_{helper_id}"),
            profile,
            ActorId::ChildOperation("op_parent".into()),
        );

        for tool in ["read", "grep", "find", "ls"] {
            assert!(
                child.tools.allows(tool),
                "built-in helper {helper_id} should receive {tool}"
            );
        }
        for tool in ["write", "edit", "bash", "delegate_agent", "delegate_team"] {
            assert!(
                !child.tools.allows(tool),
                "built-in helper {helper_id} must not receive {tool}"
            );
        }
        assert_eq!(child.filesystem, parent.filesystem);
        assert!(child.shell.is_none());
        assert!(child.session_read.is_none());
        assert!(child.session_write.is_none());
        assert!(child.ui.is_none());
    }
}

#[test]
fn delegated_operation_receives_released_tool_capabilities_only() {
    let parent = OperationCapabilitySnapshot::test_with_tools("op_parent", ["read", "bash"]);
    let target_profile = AgentProfile {
        schema_version: 1,
        id: ProfileId::from("coder"),
        display_name: "Coder".into(),
        description: None,
        model: None,
        system_prompt: None,
        tools: vec!["read".into()],
        skills: Vec::new(),
        supervision: SupervisionPolicy::Session,
        delegation: DelegationPolicy::default(),
        source: ProfileSource::BuiltIn,
        path: None,
    };

    let child = capability_snapshot_for_delegated_profile(
        &parent,
        "op_child",
        &target_profile,
        ActorId::ChildOperation("op_parent".into()),
    );

    assert!(child.tools.allows("read"));
    assert!(!child.tools.allows("bash"));
    assert_eq!(child.generation, parent.generation);
    assert_eq!(child.actor, ActorId::ChildOperation("op_parent".into()));
    assert_eq!(
        child.model,
        Some(ModelCapability {
            profile_id: Some(ProfileId::from("coder"))
        })
    );
    assert_eq!(child.filesystem, parent.filesystem);
    assert!(child.shell.is_none());
    assert!(child.session_read.is_none());
    assert!(child.session_write.is_none());
}

#[test]
fn delegated_operation_releases_delegation_tools_granted_by_policy() {
    let parent =
        OperationCapabilitySnapshot::test_with_tools("op_parent", ["read", "delegate_agent"]);
    let target_profile = AgentProfile {
        schema_version: 1,
        id: ProfileId::from("coder"),
        display_name: "Coder".into(),
        description: None,
        model: None,
        system_prompt: None,
        tools: vec!["read".into()],
        skills: Vec::new(),
        supervision: SupervisionPolicy::Session,
        delegation: DelegationPolicy {
            allow_delegate_agent: true,
            max_depth: 1,
            ..DelegationPolicy::default()
        },
        source: ProfileSource::BuiltIn,
        path: None,
    };

    let child = capability_snapshot_for_delegated_profile(
        &parent,
        "op_child",
        &target_profile,
        ActorId::ChildOperation("op_parent".into()),
    );

    assert!(child.tools.allows("read"));
    assert!(child.tools.allows("delegate_agent"));
    assert!(!child.tools.allows("delegate_team"));
    assert_eq!(child.generation, parent.generation);
}

#[test]
fn delegated_operation_from_permissive_parent_releases_all_profile_tools() {
    let parent = OperationCapabilitySnapshot::permissive("op_parent");
    let target_profile = AgentProfile {
        schema_version: 1,
        id: ProfileId::from("coder"),
        display_name: "Coder".into(),
        description: None,
        model: None,
        system_prompt: None,
        tools: vec!["read".into(), "edit".into()],
        skills: Vec::new(),
        supervision: SupervisionPolicy::Session,
        delegation: DelegationPolicy::default(),
        source: ProfileSource::BuiltIn,
        path: None,
    };

    let child = capability_snapshot_for_delegated_profile(
        &parent,
        "op_child",
        &target_profile,
        ActorId::ChildOperation("op_parent".into()),
    );

    assert!(child.tools.allows("read"));
    assert!(child.tools.allows("edit"));
    assert_eq!(child.generation, parent.generation);
}

#[test]
fn delegated_operation_does_not_release_filesystem_without_filesystem_tools() {
    let parent = OperationCapabilitySnapshot::test_with_tools("op_parent", ["bash"]);
    let target_profile = AgentProfile {
        schema_version: 1,
        id: ProfileId::from("coder"),
        display_name: "Coder".into(),
        description: None,
        model: None,
        system_prompt: None,
        tools: vec!["bash".into()],
        skills: Vec::new(),
        supervision: SupervisionPolicy::Session,
        delegation: DelegationPolicy::default(),
        source: ProfileSource::BuiltIn,
        path: None,
    };

    let child = capability_snapshot_for_delegated_profile(
        &parent,
        "op_child",
        &target_profile,
        ActorId::ChildOperation("op_parent".into()),
    );

    assert!(child.tools.allows("bash"));
    assert!(child.filesystem.is_none());
    assert!(child.shell.is_some());
}
