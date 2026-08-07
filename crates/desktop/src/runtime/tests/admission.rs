use super::*;
mod prompts;

#[tokio::test]
async fn bootstrap_can_be_polled_without_waiting_on_runtime_initialization() {
    let temp = tempfile::tempdir().unwrap();
    let (_env, options) = isolated_options(&temp);
    let mut bootstrap = DesktopRuntimeBridge::spawn(options).unwrap();

    let (bridge, snapshot) = loop {
        if let Some(ready) = bootstrap.try_ready().unwrap() {
            break ready;
        }
        tokio::task::yield_now().await;
    };
    assert!(!snapshot.project.selected_model_id.is_empty());
    bridge.shutdown().await.unwrap();
}

#[tokio::test]
async fn sessionless_startup_supports_project_commands_and_rejects_session_commands() {
    use coding_agent::api::review::{CodingAgentFileChangeIdentity, CodingAgentFileRevision};

    let temp = tempfile::tempdir().unwrap();
    let sessions_dir = temp.path().join("sessions");
    let (_env, options) = isolated_options(&temp);
    let (mut bridge, ready) = DesktopRuntimeBridge::spawn(options)
        .unwrap()
        .wait_blocking()
        .unwrap();

    assert_eq!(ready.project.selected_model_id, "claude-sonnet-4-5");
    assert!(!sessions_dir.exists());

    runtime_commands(&bridge)
        .try_reload(1, home_owner_target())
        .unwrap();
    let Some(DesktopRuntimeUpdate::Reloaded {
        command_id: 1,
        metadata,
    }) = bridge.next_update().await
    else {
        panic!("sessionless reload should return project metadata");
    };
    assert!(metadata.session.is_none());

    runtime_commands(&bridge).try_list_sessions(2).unwrap();
    let Some(DesktopRuntimeUpdate::SessionsListed {
        command_id: 2,
        sessions,
        omitted: 0,
    }) = bridge.next_update().await
    else {
        panic!("sessionless catalog query should return a typed empty catalog");
    };
    assert!(sessions.is_empty());

    runtime_commands(&bridge)
        .try_select_model(
            3,
            home_owner_target(),
            "claude-3-5-haiku-latest",
            Some(CodingAgentThinkingLevel::High),
        )
        .unwrap();
    let Some(DesktopRuntimeUpdate::SelectionChanged {
        command_id: 3,
        selection: DesktopRuntimeSelectionKind::Model,
        thinking_level,
        thinking_fallback,
        metadata,
    }) = bridge.next_update().await
    else {
        panic!("sessionless model selection should return project metadata");
    };
    assert_eq!(
        metadata.project.selected_model_id,
        "claude-3-5-haiku-latest"
    );
    assert!(metadata.session.is_none());
    assert_eq!(thinking_level, None);
    assert!(thinking_fallback);

    runtime_commands(&bridge)
        .try_select_session_profile(30, home_owner_target(), "review")
        .unwrap();
    let Some(DesktopRuntimeUpdate::SelectionChanged {
        command_id: 30,
        selection: DesktopRuntimeSelectionKind::SessionProfile,
        metadata,
        ..
    }) = bridge.next_update().await
    else {
        panic!("sessionless profile selection should return Home metadata");
    };
    assert_eq!(metadata.project.default_agent_profile_id.as_str(), "review");
    assert!(metadata.session.is_none());

    runtime_commands(&bridge).try_resync(4).unwrap();
    assert!(matches!(
        bridge.next_update().await,
        Some(DesktopRuntimeUpdate::CommandRejected {
            command_id: 4,
            command: DesktopRuntimeCommandKind::Resync,
            code,
            message,
        }) if code == "session" && message == "desktop runtime has no idle session owner"
    ));

    let review = CodingAgentFileReviewRequest::new(
        CodingAgentFileChangeIdentity {
            operation_id: "operation-sessionless-review".into(),
            tool_call_id: Some("call-sessionless-review".into()),
            path: "src/lib.rs".into(),
        },
        CodingAgentFileRevision::new(1),
    );
    let (commands, mut events, shutdown) = bridge.into_parts();
    commands
        .try_open_change(5, "missing-session", &review)
        .unwrap();
    assert!(matches!(
        events.next_update().await,
        Some(DesktopRuntimeUpdate::CommandRejected {
            command_id: 5,
            command: DesktopRuntimeCommandKind::OpenChange,
            code,
            message,
        }) if code == "session_target" && message == "session missing-session is not open"
    ));

    let recovery = DesktopRecoveryIdentity {
        operation_id: "operation-sessionless-recovery".into(),
        recovery_id: "recovery-sessionless".into(),
        record_version: 1,
        descriptor_revision: 1,
        capability_generation: Some(1),
        attempt_count: 0,
    };
    commands.try_retry_recovery(6, &recovery).unwrap();
    assert!(matches!(
        events.next_update().await,
        Some(DesktopRuntimeUpdate::CommandRejected {
            command_id: 6,
            command: DesktopRuntimeCommandKind::RetryRecovery,
            code,
            message,
        }) if code == "session" && message == "desktop runtime has no idle session owner"
    ));

    commands
        .try_list_merge_proposals(7, "missing-session")
        .unwrap();
    assert!(matches!(
        events.next_update().await,
        Some(DesktopRuntimeUpdate::CommandRejected {
            command_id: 7,
            command: DesktopRuntimeCommandKind::ListMergeProposals,
            code,
            message,
        }) if code == "session_target" && message == "session missing-session is not open"
    ));
    commands
        .try_merge_child_worktree(8, "missing-session", "worktree-1")
        .unwrap();
    assert!(matches!(
        events.next_update().await,
        Some(DesktopRuntimeUpdate::CommandRejected {
            command_id: 8,
            command: DesktopRuntimeCommandKind::MergeChildWorktree,
            code,
            ..
        }) if code == "session_target"
    ));
    commands
        .try_discard_child_worktree(9, "missing-session", "worktree-1")
        .unwrap();
    assert!(matches!(
        events.next_update().await,
        Some(DesktopRuntimeUpdate::CommandRejected {
            command_id: 9,
            command: DesktopRuntimeCommandKind::DiscardChildWorktree,
            code,
            ..
        }) if code == "session_target"
    ));
    assert!(matches!(
        commands.try_merge_child_worktree(10, "missing-session", ""),
        Err(DesktopCommandAdmissionError::InvalidSelectionId { .. })
    ));

    assert!(!sessions_dir.exists());
    drop(commands);
    shutdown.shutdown(&mut events).await.unwrap();
}

#[tokio::test]
async fn sessionless_runtime_opens_an_existing_session_without_an_intermediate_session() {
    let temp = tempfile::tempdir().unwrap();
    let (_env, options) = isolated_options(&temp);
    let (creator, created) = start_runtime(options.clone()).await;
    let session_id = created.session.session.session_id.clone();
    creator.shutdown().await.unwrap();

    let (mut bridge, ready) = DesktopRuntimeBridge::spawn(options)
        .unwrap()
        .wait_blocking()
        .unwrap();
    assert_eq!(ready.project.selected_model_id, "claude-sonnet-4-5");

    runtime_commands(&bridge)
        .try_open_session(7, &session_id)
        .unwrap();
    let Some(DesktopRuntimeUpdate::SessionChanged {
        command_id: 7,
        snapshot,
    }) = bridge.next_update().await
    else {
        panic!("sessionless open should install the requested existing session");
    };
    assert_eq!(snapshot.session.session.session_id, session_id);
    bridge.shutdown().await.unwrap();
}

#[tokio::test]
async fn runtime_owns_context_and_switches_sessions_over_bounded_queues() {
    let temp = tempfile::tempdir().unwrap();
    let (_env, mut bridge, initial) = start_isolated_runtime(&temp).await;
    assert_eq!(
        initial.transcript.session_id,
        initial.session.session.session_id
    );
    let initial_session_id = initial.session.session.session_id.clone();

    runtime_commands(&bridge).try_create_session(1).unwrap();
    let DesktopRuntimeUpdate::SessionChanged {
        command_id,
        snapshot,
    } = bridge.next_update().await.unwrap()
    else {
        panic!("create session should publish a replacement snapshot");
    };
    assert_eq!(command_id, 1);
    assert_ne!(snapshot.session.session.session_id, initial_session_id);

    runtime_commands(&bridge)
        .try_open_session(2, &initial_session_id)
        .unwrap();
    let DesktopRuntimeUpdate::SessionChanged {
        command_id,
        snapshot,
    } = bridge.next_update().await.unwrap()
    else {
        panic!("open session should publish a replacement snapshot");
    };
    assert_eq!(command_id, 2);
    assert_eq!(snapshot.session.session.session_id, initial_session_id);

    runtime_commands(&bridge)
        .try_open_session(3, "missing-session")
        .unwrap();
    let DesktopRuntimeUpdate::CommandRejected {
        command_id,
        command,
        ..
    } = bridge.next_update().await.unwrap()
    else {
        panic!("missing session should be rejected");
    };
    assert_eq!(command_id, 3);
    assert_eq!(command, DesktopRuntimeCommandKind::OpenSession);

    runtime_commands(&bridge)
        .try_reload(4, session_owner_target(&initial_session_id))
        .unwrap();
    let DesktopRuntimeUpdate::Reloaded {
        command_id,
        metadata,
    } = bridge.next_update().await.unwrap()
    else {
        panic!("reload should publish the retained current session");
    };
    assert_eq!(command_id, 4);
    assert_eq!(
        metadata.session.as_ref().unwrap().session.session_id,
        initial_session_id
    );

    runtime_commands(&bridge).try_resync(5).unwrap();
    let DesktopRuntimeUpdate::Resynced {
        command_id,
        replacement,
    } = bridge.next_update().await.unwrap()
    else {
        panic!("idle resync should publish a consistent runtime snapshot");
    };
    assert_eq!(command_id, 5);
    let DesktopRuntimeResyncSnapshot::Hydrated(snapshot) = replacement else {
        panic!("idle resync must hydrate durable state");
    };
    assert_eq!(snapshot.session.session.session_id, initial_session_id);

    runtime_commands(&bridge).try_list_sessions(6).unwrap();
    let DesktopRuntimeUpdate::SessionsListed {
        command_id,
        sessions,
        omitted,
    } = bridge.next_update().await.unwrap()
    else {
        panic!("session catalog should use a typed bounded update");
    };
    assert_eq!(command_id, 6);
    assert_eq!(omitted, 0);
    assert!(sessions.len() >= 2);
    assert!(sessions.len() <= MAX_DESKTOP_SESSION_CATALOG);
    assert!(
        sessions
            .iter()
            .any(|session| session.session_id == initial_session_id)
    );

    bridge.shutdown().await.unwrap();
}

#[tokio::test]
async fn changed_file_review_command_is_typed_and_preserves_product_error_codes() {
    use coding_agent::api::review::{CodingAgentFileChangeIdentity, CodingAgentFileRevision};

    let temp = tempfile::tempdir().unwrap();
    let (_env, bridge, initial) = start_isolated_runtime(&temp).await;
    let session_id = initial.session.session.session_id;
    let (commands, mut events, shutdown) = bridge.into_parts();
    let request = CodingAgentFileReviewRequest::new(
        CodingAgentFileChangeIdentity {
            operation_id: "operation-review".into(),
            tool_call_id: Some("call-review".into()),
            path: "src/lib.rs".into(),
        },
        CodingAgentFileRevision::new(7),
    );

    commands.try_open_change(41, &session_id, &request).unwrap();
    let update = events.next_update().await.unwrap();
    assert!(matches!(
        update,
        DesktopRuntimeUpdate::CommandRejected {
            command_id: 41,
            command: DesktopRuntimeCommandKind::OpenChange,
            code,
            ..
        } if code == "file_review_change_unauthorized"
    ));

    let mut oversized = request;
    oversized.change.path = "x".repeat(MAX_FILE_REVIEW_PATH_BYTES + 1);
    assert!(matches!(
        commands.try_open_change(42, &session_id, &oversized),
        Err(DesktopCommandAdmissionError::InvalidFileReview { .. })
    ));

    drop(commands);
    shutdown.shutdown(&mut events).await.unwrap();
}

#[tokio::test]
async fn failed_reload_retains_the_previous_runtime_context() {
    let temp = tempfile::tempdir().unwrap();
    let (_env, _) = isolated_options(&temp);
    let options = CodingAgentEmbeddingOptions::for_workspace(
        CodingAgentWorkspaceSelection::project(temp.path().join("project")),
    )
    .unwrap()
    .with_session_dir(temp.path().join("sessions"));
    let (mut bridge, initial) = start_runtime(options).await;
    std::fs::write(
        temp.path().join("global").join("settings.toml"),
        "default_model = \"missing-desktop-reload-model\"\n",
    )
    .unwrap();

    runtime_commands(&bridge)
        .try_reload(6, session_owner_target(&initial.session.session.session_id))
        .unwrap();
    let reload_update = bridge.next_update().await;
    assert!(
        matches!(
            &reload_update,
            Some(DesktopRuntimeUpdate::CommandRejected {
                command_id: 6,
                command: DesktopRuntimeCommandKind::Reload,
                code,
                ..
            }) if code == "config"
        ),
        "unexpected reload result: {reload_update:?}"
    );

    runtime_commands(&bridge).try_resync(7).unwrap();
    let Some(DesktopRuntimeUpdate::Resynced {
        command_id: 7,
        replacement,
    }) = bridge.next_update().await
    else {
        panic!("resync after a failed reload must return the retained context");
    };
    let DesktopRuntimeResyncSnapshot::Hydrated(snapshot) = replacement else {
        panic!("idle resync must hydrate durable state");
    };
    assert_eq!(snapshot.project, initial.project);
    assert_eq!(
        snapshot.session.session.session_id,
        initial.session.session.session_id
    );
    bridge.shutdown().await.unwrap();
}

#[tokio::test]
async fn idle_model_selection_is_typed_and_session_profile_selection_is_locked() {
    let temp = tempfile::tempdir().unwrap();
    let (_env, mut bridge, initial) = start_isolated_runtime(&temp).await;
    let session_id = initial.session.session.session_id.clone();
    let mut projection = DesktopProjection::new(initial).unwrap();
    let conversation = projection.conversation().clone();
    let product_snapshot = projection.snapshot().clone();
    assert!(
        projection
            .apply(ProjectionEvent::Metadata(DesktopRuntimeMetadataSnapshot {
                project: projection.project().clone(),
                session: None,
            }))
            .is_replaced()
    );
    assert_eq!(projection.snapshot(), &product_snapshot);
    assert_eq!(projection.conversation(), &conversation);

    runtime_commands(&bridge)
        .try_select_model(
            8,
            session_owner_target(&session_id),
            "claude-3-5-haiku-latest",
            Some(CodingAgentThinkingLevel::High),
        )
        .unwrap();
    let update = bridge.next_update().await.unwrap();
    let DesktopRuntimeUpdate::SelectionChanged {
        command_id: 8,
        selection: DesktopRuntimeSelectionKind::Model,
        thinking_level,
        thinking_fallback,
        metadata,
    } = &update
    else {
        panic!("idle model selection must return a typed replacement snapshot");
    };
    assert_eq!(
        metadata.project.selected_model_id,
        "claude-3-5-haiku-latest"
    );
    assert_eq!(*thinking_level, None);
    assert!(*thinking_fallback);
    assert_eq!(
        metadata.session.as_ref().unwrap().session.session_id,
        session_id
    );
    assert!(
        projection
            .apply(
                crate::application::reducer::projection_event(update)
                    .expect("selection update must map to projection metadata"),
            )
            .is_replaced()
    );
    assert_eq!(projection.conversation(), &conversation);

    runtime_commands(&bridge)
        .try_select_session_profile(9, session_owner_target(&session_id), "review")
        .unwrap();
    let update = bridge.next_update().await.unwrap();
    let DesktopRuntimeUpdate::CommandRejected {
        command_id: 9,
        command: DesktopRuntimeCommandKind::SelectSessionProfile,
        ..
    } = update
    else {
        panic!("session profile selection must be rejected once a session exists");
    };

    runtime_commands(&bridge)
        .try_select_model(
            10,
            session_owner_target(&session_id),
            "missing-desktop-model",
            None,
        )
        .unwrap();
    assert!(matches!(
        bridge.next_update().await,
        Some(DesktopRuntimeUpdate::CommandRejected {
            command_id: 10,
            command: DesktopRuntimeCommandKind::SelectModel,
            ..
        })
    ));
    runtime_commands(&bridge)
        .try_select_session_profile(11, session_owner_target(&session_id), "missing-profile")
        .unwrap();
    assert!(matches!(
        bridge.next_update().await,
        Some(DesktopRuntimeUpdate::CommandRejected {
            command_id: 11,
            command: DesktopRuntimeCommandKind::SelectSessionProfile,
            ..
        })
    ));

    runtime_commands(&bridge).try_resync(12).unwrap();
    let Some(DesktopRuntimeUpdate::Resynced { replacement, .. }) = bridge.next_update().await
    else {
        panic!("resync must expose the last successful selector state");
    };
    let DesktopRuntimeResyncSnapshot::Hydrated(snapshot) = replacement else {
        panic!("idle resync must hydrate durable state");
    };
    assert_eq!(
        snapshot.project.selected_model_id,
        "claude-3-5-haiku-latest"
    );
    bridge.shutdown().await.unwrap();
}

#[test]
fn runtime_model_thinking_admission_never_retains_an_unsupported_explicit_level() {
    let temp = tempfile::tempdir().unwrap();
    let (_env, options) = isolated_options(&temp);
    let context = CodingAgentEmbeddingContext::load(options).unwrap();

    let (thinking_level, fallback) = admitted_model_thinking(
        &context,
        "claude-3-5-haiku-latest",
        Some(CodingAgentThinkingLevel::High),
    )
    .unwrap();
    assert_eq!(thinking_level, None);
    assert!(fallback);

    let selected_model_id = context.snapshot().selected_model_id.clone();
    let (thinking_level, fallback) = admitted_model_thinking(
        &context,
        &selected_model_id,
        Some(CodingAgentThinkingLevel::High),
    )
    .unwrap();
    assert_eq!(thinking_level, Some(CodingAgentThinkingLevel::High));
    assert!(!fallback);
}

#[tokio::test]
async fn persisted_projectless_session_restores_its_managed_scratch_workspace() {
    let temp = tempfile::tempdir().unwrap();
    let global = temp.path().join("global");
    let home = temp.path().join("home");
    let sessions = temp.path().join("sessions");
    std::fs::create_dir_all(&global).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    let _env = ProcessEnvGuard::isolated(&global);
    let scratch_options = CodingAgentEmbeddingOptions::for_workspace(
        CodingAgentWorkspaceSelection::projectless("workspace-reopen"),
    )
    .unwrap()
    .with_session_dir(&sessions)
    .with_model_id("claude-sonnet-4-5");
    let scratch = scratch_options.cwd().to_path_buf();
    let scratch_context = CodingAgentEmbeddingContext::load(scratch_options).unwrap();
    let mut session = scratch_context.create_session().await.unwrap();
    let session_id = session.view().expect("session view").session_id;
    session.shutdown().await.unwrap();
    std::fs::remove_dir(&scratch).unwrap();
    let home_options =
        CodingAgentEmbeddingOptions::for_workspace(CodingAgentWorkspaceSelection::project(&home))
            .unwrap()
            .with_session_dir(&sessions)
            .with_model_id("claude-sonnet-4-5");
    let (mut bridge, _) = DesktopRuntimeBridge::spawn(home_options)
        .unwrap()
        .wait_blocking()
        .unwrap();

    runtime_commands(&bridge)
        .try_open_session(116, &session_id)
        .unwrap();
    let Some(DesktopRuntimeUpdate::SessionChanged { snapshot, .. }) = bridge.next_update().await
    else {
        panic!("projectless session should recreate and reopen its managed scratch");
    };
    assert_eq!(snapshot.project.cwd, scratch);
    assert!(scratch.is_dir());
    assert!(matches!(
        snapshot.project.workspace.as_ref().map(|workspace| &workspace.scope),
        Some(coding_agent::api::embedding::CodingAgentWorkspaceScope::Projectless {
            workspace_id
        }) if workspace_id == "workspace-reopen"
    ));
    bridge.shutdown().await.unwrap();
}

#[tokio::test]
async fn deleted_project_session_open_is_recoverably_rejected() {
    let temp = tempfile::tempdir().unwrap();
    let global = temp.path().join("global");
    let home = temp.path().join("home");
    let project = temp.path().join("deleted-project");
    let sessions = temp.path().join("sessions");
    std::fs::create_dir_all(&global).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&project).unwrap();
    let _env = ProcessEnvGuard::isolated(&global);
    let context = CodingAgentEmbeddingContext::load(
        CodingAgentEmbeddingOptions::for_workspace(CodingAgentWorkspaceSelection::project(
            &project,
        ))
        .unwrap()
        .with_session_dir(&sessions),
    )
    .unwrap();
    let mut session = context.create_session().await.unwrap();
    let session_id = session.view().expect("session view").session_id;
    session.shutdown().await.unwrap();
    std::fs::remove_dir(&project).unwrap();
    let home_options =
        CodingAgentEmbeddingOptions::for_workspace(CodingAgentWorkspaceSelection::project(&home))
            .unwrap()
            .with_session_dir(&sessions);
    let (mut bridge, _) = DesktopRuntimeBridge::spawn(home_options)
        .unwrap()
        .wait_blocking()
        .unwrap();

    runtime_commands(&bridge)
        .try_open_session(117, &session_id)
        .unwrap();
    assert!(matches!(
        bridge.next_update().await,
        Some(DesktopRuntimeUpdate::CommandRejected {
            command_id: 117,
            command: DesktopRuntimeCommandKind::OpenSession,
            code,
            message,
        }) if code == "workspace_unavailable"
            && message == "Project workspace directory is unavailable."
    ));
    runtime_commands(&bridge).try_resync(118).unwrap();
    assert!(matches!(
        bridge.next_update().await,
        Some(DesktopRuntimeUpdate::CommandRejected {
            command_id: 118,
            command: DesktopRuntimeCommandKind::Resync,
            ..
        })
    ));
    bridge.shutdown().await.unwrap();
}

#[tokio::test]
async fn legacy_session_scope_is_migrated_before_desktop_builds_its_context() {
    let temp = tempfile::tempdir().unwrap();
    let global = temp.path().join("global");
    let home = temp.path().join("home");
    let project = temp.path().join("legacy-project");
    let sessions = temp.path().join("sessions");
    std::fs::create_dir_all(&global).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&project).unwrap();
    write_workspace_fixture(&project, "legacy-project", "high");
    let _env = ProcessEnvGuard::isolated(&global);
    let context = CodingAgentEmbeddingContext::load(
        CodingAgentEmbeddingOptions::for_workspace(CodingAgentWorkspaceSelection::project(
            &project,
        ))
        .unwrap()
        .with_session_dir(&sessions),
    )
    .unwrap();
    let mut session = context.create_session().await.unwrap();
    let session_id = session.view().expect("session view").session_id;
    session.shutdown().await.unwrap();
    let manifest_path = sessions.join(&session_id).join("session.json");
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&manifest_path).unwrap()).unwrap();
    manifest["version"] = serde_json::json!(1);
    manifest.as_object_mut().unwrap().remove("workspace_scope");
    manifest
        .as_object_mut()
        .unwrap()
        .remove("workspace_migrated_from_legacy");
    std::fs::write(
        &manifest_path,
        serde_json::to_vec_pretty(&manifest).unwrap(),
    )
    .unwrap();
    let home_options =
        CodingAgentEmbeddingOptions::for_workspace(CodingAgentWorkspaceSelection::project(&home))
            .unwrap()
            .with_session_dir(&sessions);
    let (mut bridge, _) = DesktopRuntimeBridge::spawn(home_options)
        .unwrap()
        .wait_blocking()
        .unwrap();

    runtime_commands(&bridge)
        .try_open_session(119, &session_id)
        .unwrap();
    let Some(DesktopRuntimeUpdate::SessionChanged { snapshot, .. }) = bridge.next_update().await
    else {
        panic!("recoverable legacy project session should reopen");
    };
    assert_eq!(snapshot.project.cwd, project.canonicalize().unwrap());
    assert!(
        snapshot
            .project
            .resources
            .skill_names
            .iter()
            .any(|name| name == "legacy-project-skill")
    );
    let migrated: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&manifest_path).unwrap()).unwrap();
    assert_eq!(migrated["version"], 2);
    assert_eq!(migrated["workspace_scope"]["kind"], "project");
    assert_eq!(migrated["workspace_migrated_from_legacy"], true);
    bridge.shutdown().await.unwrap();
}
