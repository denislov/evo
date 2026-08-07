use super::*;

#[tokio::test]
async fn fifth_open_session_is_rejected_without_disturbing_the_existing_four() {
    let temp = tempfile::tempdir().unwrap();
    let (_env, mut bridge, first) = start_isolated_runtime(&temp).await;
    let first_session = first.session.session.session_id;
    for command_id in 111..114 {
        runtime_commands(&bridge)
            .try_create_session(command_id)
            .unwrap();
        assert!(matches!(
            bridge.next_update().await,
            Some(DesktopRuntimeUpdate::SessionChanged { command_id: completed, .. })
                if completed == command_id
        ));
    }

    runtime_commands(&bridge).try_create_session(114).unwrap();
    assert!(matches!(
        bridge.next_update().await,
        Some(DesktopRuntimeUpdate::CommandRejected {
            command_id: 114,
            command: DesktopRuntimeCommandKind::CreateSession,
            ref code,
            ..
        }) if code == "session_limit_reached"
    ));

    runtime_commands(&bridge)
        .try_close_session(115, &first_session)
        .unwrap();
    assert!(matches!(
        bridge.next_update().await,
        Some(DesktopRuntimeUpdate::SessionClosed {
            command_id: 115,
            session_id,
        }) if session_id == first_session
    ));
    bridge.shutdown().await.unwrap();
}

#[tokio::test]
async fn sessionless_prompt_atomically_creates_and_accepts_one_session() {
    let temp = tempfile::tempdir().unwrap();
    let (_env, options) = isolated_options(&temp);
    let (mut bridge, _) = DesktopRuntimeBridge::spawn(options)
        .unwrap()
        .wait_blocking()
        .unwrap();

    runtime_commands(&bridge)
        .try_submit_prompt_with_attachments(
            13,
            new_project_prompt_target(&temp),
            "first desktop prompt",
            &[],
            None,
        )
        .unwrap();
    let Some(DesktopRuntimeUpdate::PromptAcceptedWithSession {
        command_id: 13,
        snapshot: created,
    }) = bridge.next_update().await
    else {
        panic!("first prompt should atomically publish its created session");
    };
    let session_id = created.session.session.session_id.clone();
    assert!(created.transcript.items.is_empty());
    let mut projection = DesktopProjection::new(created).unwrap();

    let finished = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let update = bridge.next_update().await.unwrap();
            if let Some(event) = crate::application::reducer::projection_event(update.clone()) {
                assert!(!matches!(
                    projection.apply(event),
                    DesktopProjectionApply::NeedsResync
                ));
            }
            if let DesktopRuntimeUpdate::PromptFinished {
                command_id: 13,
                snapshot,
                ..
            } = update
            {
                assert_eq!(snapshot.session.session.session_id, session_id);
                assert!(snapshot.transcript.items.iter().any(|item| matches!(
                    item,
                    CodingAgentSessionTranscriptItem::User { text, .. }
                        if text == "first desktop prompt"
                )));
                break;
            }
        }
    })
    .await;
    assert!(finished.is_ok(), "first sessionless prompt did not finish");

    runtime_commands(&bridge).try_list_sessions(14).unwrap();
    let Some(DesktopRuntimeUpdate::SessionsListed { sessions, .. }) = bridge.next_update().await
    else {
        panic!("created prompt session should be visible in the catalog");
    };
    assert_eq!(
        sessions
            .iter()
            .filter(|session| session.session_id == session_id)
            .count(),
        1
    );
    bridge.shutdown().await.unwrap();
}

#[tokio::test]
async fn admission_failure_creates_no_session_owner_or_manifest() {
    let temp = tempfile::tempdir().unwrap();
    let global = temp.path().join("global");
    let home = temp.path().join("home");
    let target = temp.path().join("target");
    let sessions = temp.path().join("sessions");
    std::fs::create_dir_all(&global).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&target).unwrap();
    let _env = ProcessEnvGuard::isolated(&global);
    let home_options =
        CodingAgentEmbeddingOptions::for_workspace(CodingAgentWorkspaceSelection::project(&home))
            .unwrap()
            .with_session_dir(&sessions)
            .with_model_id("claude-sonnet-4-5");
    let mut state = RuntimeState {
        home: HomeRuntimeContext::load(home_options).unwrap(),
        workspaces: std::collections::HashMap::new(),
        focused_session_id: None,
        fail_next_prompt_start: false,
    };

    for (command_id, (model_id, pre_fail)) in [
        (131, ("missing-desktop-context-model", None)),
        (
            132,
            (
                "claude-sonnet-4-5",
                Some(|| std::fs::remove_dir(&target).unwrap()),
            ),
        ),
    ] {
        let prompt_target = DesktopPromptTarget::new(
            CodingAgentWorkspaceSelection::project(&target),
            model_id,
            "default",
        );
        validate_prompt_target(&prompt_target).expect("the target is valid at admission time");
        if let Some(pre_fail) = pre_fail {
            pre_fail();
        }
        let mut active = std::collections::HashMap::new();
        let update = dispatch_command(
            &mut state,
            &mut active,
            DesktopRuntimeCommand::SubmitPrompt {
                command_id,
                target: prompt_target,
                prompt: "the runtime must resolve the target again".into(),
                attachments: Vec::new(),
                thinking_level: None,
            },
        )
        .await;

        assert!(
            matches!(
                update,
                DesktopRuntimeUpdate::CommandRejected {
                    command: DesktopRuntimeCommandKind::SubmitPrompt,
                    ..
                }
            ),
            "command {command_id} should be rejected"
        );
        assert!(state.workspaces.is_empty());
        assert!(active.is_empty());
        assert!(!sessions.exists());
    }
}

#[tokio::test]
async fn new_prompt_binds_model_profile_and_sanitized_thinking_before_persistence() {
    let temp = tempfile::tempdir().unwrap();
    let (_env, options) = isolated_options(&temp);
    let mut state = RuntimeState {
        home: HomeRuntimeContext::load(options).unwrap(),
        workspaces: std::collections::HashMap::new(),
        focused_session_id: None,
        fail_next_prompt_start: false,
    };

    let created = state
        .create_session_for_workspace(
            CodingAgentWorkspaceSelection::project(temp.path().join("project")),
            "gpt-5".into(),
            "review".into(),
            Some(CodingAgentThinkingLevel::Off),
            0,
        )
        .await
        .unwrap();

    assert_eq!(
        created.thinking_level, None,
        "unsupported Off must fall back to Auto"
    );
    let session_id = created.session_id;
    let owner = state.workspaces.get(&session_id).unwrap();
    assert_eq!(owner.context.snapshot().selected_model_id, "gpt-5");
    assert_eq!(
        owner.context.snapshot().default_agent_profile_id.as_str(),
        "review"
    );
    assert_eq!(
        owner
            .session
            .view()
            .expect("session view")
            .default_agent_profile_id
            .as_str(),
        "review"
    );
    state.close_idle_session(&session_id).await.unwrap();
}

#[tokio::test]
async fn prompt_prepare_failure_retains_the_persisted_scoped_session() {
    let temp = tempfile::tempdir().unwrap();
    let (_env, options) = isolated_options(&temp);
    let attachment = temp.path().join("project/deleted-attachment.txt");
    std::fs::write(&attachment, "prepare me").unwrap();
    let mut state = RuntimeState {
        home: HomeRuntimeContext::load(options).unwrap(),
        workspaces: std::collections::HashMap::new(),
        focused_session_id: None,
        fail_next_prompt_start: false,
    };
    validate_prompt_with_attachments("prepare failure", std::slice::from_ref(&attachment))
        .expect("the attachment path is admitted before it disappears");
    std::fs::remove_file(&attachment).unwrap();
    let mut active = std::collections::HashMap::new();

    let update = dispatch_command(
        &mut state,
        &mut active,
        DesktopRuntimeCommand::SubmitPrompt {
            command_id: 133,
            target: new_project_prompt_target(&temp),
            prompt: "prepare failure".into(),
            attachments: vec![attachment],
            thinking_level: None,
        },
    )
    .await;
    let DesktopRuntimeUpdate::PromptRejectedWithSession {
        command_id: 133,
        snapshot,
        ..
    } = update
    else {
        panic!("a post-persistence prepare failure must install the created session");
    };

    let session_id = snapshot.session.session.session_id.clone();
    let owner = state.workspaces.get(&session_id).unwrap();
    let resolved = snapshot.project.workspace.as_ref().unwrap();
    assert_eq!(&snapshot.project, owner.context.snapshot());
    assert_eq!(resolved.scope, owner.scope);
    assert_eq!(resolved.execution_cwd, snapshot.project.cwd);
    assert!(snapshot.transcript.items.is_empty());
    assert!(active.is_empty());
    let overview = state
        .session_catalog()
        .unwrap()
        .0
        .into_iter()
        .find(|overview| overview.session_id == session_id)
        .expect("the rejected prompt session remains durable");
    assert_eq!(overview.workspace, resolved.overview);
    state.close_idle_session(&session_id).await.unwrap();
}

#[tokio::test]
async fn explicit_new_prompt_target_creates_a_session_when_another_is_open() {
    let temp = tempfile::tempdir().unwrap();
    let (_env, mut bridge, first) = start_isolated_runtime(&temp).await;
    let first_session_id = first.session.session.session_id;

    runtime_commands(&bridge)
        .try_submit_prompt_with_attachments(
            14,
            new_project_prompt_target(&temp),
            "start a distinct conversation",
            &[],
            None,
        )
        .unwrap();
    let Some(DesktopRuntimeUpdate::PromptAcceptedWithSession {
        command_id: 14,
        snapshot: second,
    }) = bridge.next_update().await
    else {
        panic!("an explicit New target must publish the newly created session");
    };
    assert_ne!(second.session.session.session_id, first_session_id);

    bridge.shutdown().await.unwrap();
}

#[tokio::test]
async fn projectless_first_prompt_records_the_global_only_scratch_cwd() {
    let temp = tempfile::tempdir().unwrap();
    let global = temp.path().join("global");
    let scratch = global.join("scratch/workspace-runtime-test");
    let sessions = temp.path().join("sessions");
    std::fs::create_dir_all(scratch.join(".evo")).unwrap();
    std::fs::write(
        scratch.join(".evo/settings.toml"),
        "default_thinking_level = \"high\"\n",
    )
    .unwrap();
    let _env = ProcessEnvGuard::isolated(&global);
    let options = CodingAgentEmbeddingOptions::for_workspace(
        CodingAgentWorkspaceSelection::projectless("workspace-runtime-test"),
    )
    .unwrap()
    .with_session_dir(&sessions)
    .with_model_id("claude-sonnet-4-5");
    let (mut bridge, ready) = DesktopRuntimeBridge::spawn(options)
        .unwrap()
        .wait_blocking()
        .unwrap();
    assert_eq!(ready.project.cwd, scratch);
    assert_ne!(
        ready.project.settings.default_thinking_level.as_deref(),
        Some("high"),
        "scratch-local project settings must not enter a global-only context"
    );

    runtime_commands(&bridge)
        .try_submit_prompt_with_attachments(
            16,
            DesktopPromptTarget::new(
                CodingAgentWorkspaceSelection::projectless("workspace-runtime-test"),
                "claude-sonnet-4-5",
                "default",
            ),
            "scratch workspace prompt",
            &[],
            None,
        )
        .unwrap();
    let Some(DesktopRuntimeUpdate::PromptAcceptedWithSession { snapshot, .. }) =
        bridge.next_update().await
    else {
        panic!("the first scratch prompt should atomically create its session");
    };
    let session_id = snapshot.session.session.session_id;
    let catalog =
        coding_agent::api::embedding::CodingAgentSessionQuery::from_session_root(&sessions)
            .overviews()
            .unwrap();
    let overview = catalog
        .overviews
        .iter()
        .find(|overview| overview.session_id == session_id)
        .expect("the scratch session should be visible in the durable overview");
    assert_eq!(
        overview.workspace.kind,
        coding_agent::api::view::CodingAgentWorkspaceKind::Projectless
    );
    assert_eq!(overview.workspace.display_path, None);

    bridge.shutdown().await.unwrap();
}

#[tokio::test]
async fn desktop_session_catalog_lists_project_and_projectless_history() {
    let temp = tempfile::tempdir().unwrap();
    let global = temp.path().join("global");
    let sessions = temp.path().join("sessions");
    let project = temp.path().join("project");
    std::fs::create_dir_all(&global).unwrap();
    std::fs::create_dir_all(&project).unwrap();
    let _env = ProcessEnvGuard::isolated(&global);
    let options = CodingAgentEmbeddingOptions::for_workspace(
        CodingAgentWorkspaceSelection::projectless("catalog-home"),
    )
    .unwrap()
    .with_session_dir(&sessions)
    .with_model_id("claude-sonnet-4-5");
    let mut state = RuntimeState {
        home: HomeRuntimeContext::load(options).unwrap(),
        workspaces: std::collections::HashMap::new(),
        focused_session_id: None,
        fail_next_prompt_start: false,
    };
    let home_projectless_group_id = state
        .home
        .context
        .snapshot()
        .workspace
        .as_ref()
        .unwrap()
        .overview
        .group_id
        .clone();

    let projectless = state
        .create_session_for_workspace(
            CodingAgentWorkspaceSelection::projectless("catalog-projectless"),
            "claude-sonnet-4-5".into(),
            "default".into(),
            None,
            0,
        )
        .await
        .unwrap();
    let second_projectless = state
        .create_session_for_workspace(
            CodingAgentWorkspaceSelection::projectless("catalog-projectless"),
            "claude-sonnet-4-5".into(),
            "default".into(),
            None,
            1,
        )
        .await
        .unwrap();
    let project_session = state
        .create_session_for_workspace(
            CodingAgentWorkspaceSelection::project(&project),
            "claude-sonnet-4-5".into(),
            "default".into(),
            None,
            2,
        )
        .await
        .unwrap();

    let (catalog, omitted) = state.session_catalog().unwrap();
    assert_eq!(omitted, 0);
    assert!(catalog.iter().any(|entry| {
        entry.session_id == projectless.session_id
            && entry.workspace.kind
                == coding_agent::api::view::CodingAgentWorkspaceKind::Projectless
    }));
    let projectless_entries = catalog
        .iter()
        .filter(|entry| {
            entry.workspace.kind == coding_agent::api::view::CodingAgentWorkspaceKind::Projectless
        })
        .collect::<Vec<_>>();
    assert_eq!(projectless_entries.len(), 2);
    assert_ne!(
        projectless_entries[0].workspace.group_id, projectless_entries[1].workspace.group_id,
        "each projectless session must own a distinct managed scratch scope"
    );
    assert!(
        projectless_entries
            .iter()
            .all(|entry| entry.workspace.group_id != home_projectless_group_id),
        "the Home draft workspace identity must not leak into durable sessions"
    );
    assert_ne!(projectless.session_id, second_projectless.session_id);
    assert!(catalog.iter().any(|entry| {
        entry.session_id == project_session.session_id
            && entry.workspace.kind == coding_agent::api::view::CodingAgentWorkspaceKind::Project
    }));

    for (_, mut workspace) in std::mem::take(&mut state.workspaces) {
        workspace.session.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn session_creation_failure_rejects_the_first_prompt_without_an_active_owner() {
    let temp = tempfile::tempdir().unwrap();
    let (_env, _) = isolated_options(&temp);
    let blocked_session_root = temp.path().join("blocked-session-root");
    std::fs::write(&blocked_session_root, "not a directory").unwrap();
    let options = CodingAgentEmbeddingOptions::for_workspace(
        CodingAgentWorkspaceSelection::project(temp.path().join("project")),
    )
    .unwrap()
    .with_session_dir(&blocked_session_root)
    .with_model_id("claude-sonnet-4-5");
    let (mut bridge, _) = DesktopRuntimeBridge::spawn(options)
        .unwrap()
        .wait_blocking()
        .unwrap();

    runtime_commands(&bridge)
        .try_submit_prompt_with_attachments(
            15,
            new_project_prompt_target(&temp),
            "cannot create this session",
            &[],
            None,
        )
        .unwrap();
    assert!(matches!(
        bridge.next_update().await,
        Some(DesktopRuntimeUpdate::CommandRejected {
            command_id: 15,
            command: DesktopRuntimeCommandKind::SubmitPrompt,
            ..
        })
    ));
    runtime_commands(&bridge).try_resync(151).unwrap();
    assert!(matches!(
        bridge.next_update().await,
        Some(DesktopRuntimeUpdate::CommandRejected {
            command_id: 151,
            command: DesktopRuntimeCommandKind::Resync,
            code,
            ..
        }) if code == "session"
    ));
    assert!(blocked_session_root.is_file());
    bridge.shutdown().await.unwrap();
}

#[tokio::test]
async fn prompt_start_failure_reports_the_session_that_was_already_created() {
    let temp = tempfile::tempdir().unwrap();
    let (_env, options) = isolated_options(&temp);
    std::fs::write(
        temp.path().join("project/scope-proof.txt"),
        "the selected context resolved this relative attachment",
    )
    .unwrap();
    let mut state = RuntimeState {
        home: HomeRuntimeContext::load(options).unwrap(),
        workspaces: std::collections::HashMap::new(),
        focused_session_id: None,
        fail_next_prompt_start: true,
    };
    let mut active = std::collections::HashMap::new();

    let update = dispatch_command(
        &mut state,
        &mut active,
        DesktopRuntimeCommand::SubmitPrompt {
            command_id: 16,
            target: new_project_prompt_target(&temp),
            prompt: "prompt start failure".into(),
            attachments: vec![std::path::PathBuf::from("scope-proof.txt")],
            thinking_level: None,
        },
    )
    .await;
    let DesktopRuntimeUpdate::PromptRejectedWithSession {
        command_id: 16,
        snapshot,
        error,
    } = update
    else {
        panic!("post-creation failure must report the retained session atomically");
    };
    assert_eq!(error.code, "session");
    assert_eq!(error.message, "injected desktop prompt start failure");
    assert!(active.is_empty());
    let retained_session_id = snapshot.session.session.session_id.clone();
    let owner = state.workspaces.get(&retained_session_id).unwrap();
    let resolved = snapshot.project.workspace.as_ref().unwrap();
    assert_eq!(&snapshot.project, owner.context.snapshot());
    assert_eq!(resolved.scope, owner.scope);
    assert_eq!(resolved.execution_cwd, snapshot.project.cwd);
    assert_eq!(
        state
            .workspaces
            .get(&retained_session_id)
            .unwrap()
            .session
            .view()
            .expect("session view")
            .session_id,
        snapshot.session.session.session_id
    );
    assert_eq!(state.session_catalog().unwrap().0.len(), 1);

    let mut workspace = state.workspaces.remove(&retained_session_id).unwrap();
    workspace.session.shutdown().await.unwrap();
}

#[tokio::test]
async fn command_queue_full_and_closed_are_typed_without_runtime_timing() {
    let (commands, _command_rx) = mpsc::channel(DESKTOP_COMMAND_QUEUE_CAPACITY);
    let client = RuntimeCommandClient { commands };
    let cloned_client = client.clone();

    for command_id in 0..DESKTOP_COMMAND_QUEUE_CAPACITY as u64 {
        client.try_reload(command_id, home_owner_target()).unwrap();
    }
    assert_eq!(
        cloned_client.try_reload(u64::MAX, home_owner_target()),
        Err(DesktopCommandAdmissionError::QueueFull)
    );
    drop(_command_rx);
    assert_eq!(
        client.try_reload(u64::MAX, home_owner_target()),
        Err(DesktopCommandAdmissionError::RuntimeClosed)
    );
}

#[tokio::test]
async fn steer_and_follow_up_races_keep_typed_command_association() {
    let temp = tempfile::tempdir().unwrap();
    let (_env, mut bridge, initial) = start_isolated_runtime(&temp).await;
    let session_id = initial.session.session.session_id;
    runtime_commands(&bridge)
        .try_submit_prompt_with_attachments(
            25,
            existing_prompt_target(&session_id),
            "control association race",
            &[],
            None,
        )
        .unwrap();

    let mut controls_sent = false;
    let mut steer_result = false;
    let mut follow_up_result = false;
    let mut prompt_finished = false;
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match bridge.next_update().await.unwrap() {
                DesktopRuntimeUpdate::PromptStarted { .. } if !controls_sent => {
                    runtime_commands(&bridge)
                        .try_steer_for_session(26, &session_id, "steer exactly")
                        .unwrap();
                    runtime_commands(&bridge)
                        .try_follow_up_for_session(27, &session_id, "follow up exactly")
                        .unwrap();
                    controls_sent = true;
                }
                DesktopRuntimeUpdate::ControlAccepted {
                    command_id: 26,
                    command: DesktopRuntimeCommandKind::Steer,
                    ..
                }
                | DesktopRuntimeUpdate::CommandRejected {
                    command_id: 26,
                    command: DesktopRuntimeCommandKind::Steer,
                    ..
                } => steer_result = true,
                DesktopRuntimeUpdate::ControlAccepted {
                    command_id: 27,
                    command: DesktopRuntimeCommandKind::FollowUp,
                    ..
                }
                | DesktopRuntimeUpdate::CommandRejected {
                    command_id: 27,
                    command: DesktopRuntimeCommandKind::FollowUp,
                    ..
                } => follow_up_result = true,
                DesktopRuntimeUpdate::PromptFinished { command_id: 25, .. } => {
                    prompt_finished = true
                }
                _ => {}
            }
            if steer_result && follow_up_result && prompt_finished {
                break;
            }
        }
    })
    .await
    .expect("control races must converge to typed results and a prompt terminal");

    assert!(controls_sent, "controls must be sent after PromptStarted");
    assert!(steer_result, "steer must receive its typed command result");
    assert!(
        follow_up_result,
        "follow-up must receive its typed command result"
    );
    assert!(prompt_finished);
    bridge.shutdown().await.unwrap();
}

#[tokio::test]
async fn authorization_decision_is_typed_and_rejected_without_an_active_prompt() {
    let temp = tempfile::tempdir().unwrap();
    let (_env, mut bridge, _) = start_isolated_runtime(&temp).await;
    let identity = ToolAuthorizationIdentity {
        authorization_id: "authorization-31".into(),
        operation_id: "operation-31".into(),
        turn_id: "turn-31".into(),
        tool_call_id: "tool-call-31".into(),
        capability_generation: 1,
    };
    runtime_commands(&bridge)
        .try_decide_tool_authorization(
            31,
            &identity,
            ToolAuthorizationDecision::Deny {
                reason: Some("test denial".into()),
            },
        )
        .unwrap();
    assert!(matches!(
        bridge.next_update().await,
        Some(DesktopRuntimeUpdate::CommandRejected {
            command_id: 31,
            command: DesktopRuntimeCommandKind::DecideToolAuthorization,
            ..
        })
    ));
    bridge.shutdown().await.unwrap();
}

#[test]
fn runtime_command_client_is_the_single_validation_and_admission_surface() {
    let (bridge, mut harness) = DesktopRuntimeBridge::instrumented_for_test();
    let (client, _events, _shutdown) = bridge.into_parts();

    assert!(matches!(
        client.try_open_session(904, ""),
        Err(DesktopCommandAdmissionError::InvalidSessionId { .. })
    ));
    assert!(matches!(
        client.try_steer_for_session(905, "session-control-test", ""),
        Err(DesktopCommandAdmissionError::InvalidControlText { .. })
    ));
    assert!(harness.drain_command_kinds().is_empty());

    client.clone().try_list_sessions(906).unwrap();
    assert_eq!(
        harness.drain_command_kinds(),
        [DesktopRuntimeCommandKind::ListSessions]
    );
}

#[test]
fn rename_command_is_bounded_trimmed_and_identity_preserving() {
    let (bridge, mut harness) = DesktopRuntimeBridge::instrumented_for_test();
    runtime_commands(&bridge)
        .try_rename_session(902, "session-to-rename", Some("  Release plan  "))
        .unwrap();
    assert_eq!(
        harness.drain_session_renames(),
        [("session-to-rename".into(), Some("Release plan".into()))]
    );
    assert!(
        runtime_commands(&bridge)
            .try_rename_session(
                903,
                "session-to-rename",
                Some(&"x".repeat(MAX_SESSION_NAME_BYTES + 1)),
            )
            .is_err()
    );
}
