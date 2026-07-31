fn isolated_options(temp: &tempfile::TempDir) -> (ProcessEnvGuard, CodingAgentEmbeddingOptions) {
    let global = temp.path().join("global");
    let project = temp.path().join("project");
    let sessions = temp.path().join("sessions");
    std::fs::create_dir_all(&global).unwrap();
    std::fs::create_dir_all(&project).unwrap();
    let env = ProcessEnvGuard::isolated(&global);
    let options = CodingAgentEmbeddingOptions::for_workspace(
        CodingAgentWorkspaceSelection::project(&project),
    )
    .unwrap()
    .with_session_dir(&sessions)
    .with_model_id("claude-sonnet-4-5");
    (env, options)
}

fn new_project_prompt_target(temp: &tempfile::TempDir) -> DesktopPromptTarget {
    DesktopPromptTarget::new(
        CodingAgentWorkspaceSelection::project(temp.path().join("project")),
        "claude-sonnet-4-5",
        "default",
    )
}

fn existing_prompt_target(session_id: impl Into<String>) -> DesktopPromptTarget {
    DesktopPromptTarget::existing(session_id)
}

fn home_owner_target() -> DesktopRuntimeOwnerTarget {
    DesktopRuntimeOwnerTarget::home()
}

fn session_owner_target(session_id: impl Into<String>) -> DesktopRuntimeOwnerTarget {
    DesktopRuntimeOwnerTarget::session(session_id)
}

fn write_workspace_fixture(project: &std::path::Path, id: &str, thinking: &str) {
    let skill_dir = project.join(".evo/skills").join(format!("{id}-skill"));
    let agents_dir = project.join(".evo/agents");
    std::fs::create_dir_all(&skill_dir).unwrap();
    std::fs::create_dir_all(&agents_dir).unwrap();
    std::fs::write(
        project.join(".evo/settings.toml"),
        format!("default_thinking_level = \"{thinking}\"\n"),
    )
    .unwrap();
    std::fs::write(project.join("AGENTS.md"), format!("{id} context")).unwrap();
    std::fs::write(
        skill_dir.join("SKILL.md"),
        format!(
            "---\nname: {id}-skill\ndescription: {id} skill description\n---\n{id} skill body\n"
        ),
    )
    .unwrap();
    std::fs::write(
        agents_dir.join(format!("{id}.toml")),
        format!("schema_version = 1\nid = \"{id}\"\ndisplay_name = \"{id}\"\n"),
    )
    .unwrap();
}

fn runtime_commands(bridge: &DesktopRuntimeBridge) -> &RuntimeCommandClient {
    bridge
        .command_client
        .as_ref()
        .expect("test bridge must retain its command client before splitting")
}

fn cross_adapter_fixture_events() -> Vec<CodingAgentProductEvent> {
    serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../coding-agent/tests/fixtures/client_projection/cross-adapter-events.json"
    )))
    .expect("the shared client-projection fixture must deserialize")
}

async fn start_runtime(
    options: CodingAgentEmbeddingOptions,
) -> (DesktopRuntimeBridge, DesktopRuntimeHydratedSnapshot) {
    let (mut bridge, _) = DesktopRuntimeBridge::spawn(options)
        .unwrap()
        .wait_blocking()
        .unwrap();
    runtime_commands(&bridge)
        .try_create_session(u64::MAX)
        .unwrap();
    let DesktopRuntimeUpdate::SessionChanged { snapshot, .. } = bridge.next_update().await.unwrap()
    else {
        panic!("test runtime session creation should publish a hydrated snapshot");
    };
    (bridge, snapshot)
}

async fn start_isolated_runtime(
    temp: &tempfile::TempDir,
) -> (
    ProcessEnvGuard,
    DesktopRuntimeBridge,
    DesktopRuntimeHydratedSnapshot,
) {
    let (env, options) = isolated_options(temp);
    let (bridge, snapshot) = start_runtime(options).await;
    (env, bridge, snapshot)
}
