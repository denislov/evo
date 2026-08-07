use super::*;

#[gpui::test]
fn idle_sessions_drawer_renders_new_conversation_skills_and_history(cx: &mut TestAppContext) {
    initialize_visual_test(cx);
    let (shell, cx) = add_idle_visual_shell(cx);
    shell.update(cx, |shell, cx| {
        shell.app.catalog.replace_catalog(
            vec![desktop::runtime::DesktopSessionCatalogEntry {
                session_id: "idle-recent-session".into(),
                name: Some("Idle recent session".into()),
                updated_at: "2026-07-29T08:00:00Z".into(),
                ..Default::default()
            }],
            0,
        );
        shell.refresh_views(UiChangeSet::one(UiRegion::Sessions), cx);
    });
    cx.simulate_resize(size(px(700.), px(800.)));
    cx.run_until_parked();

    let toggle = cx
        .debug_bounds("desktop-hit-toggle-sessions")
        .expect("idle Header exposes the Sessions drawer toggle");
    cx.simulate_click(toggle.center(), gpui::Modifiers::default());
    cx.run_until_parked();

    assert!(
        cx.debug_bounds("desktop-new-conversation-section")
            .is_some()
    );
    assert!(cx.debug_bounds("desktop-hit-skills").is_some());
    assert!(cx.debug_bounds("desktop-skill-row-0").is_none());
    assert!(cx.debug_bounds("desktop-projects-section").is_some());
    assert!(cx.debug_bounds("desktop-project-row-0").is_some());
    assert!(cx.debug_bounds("desktop-session-row-0").is_some());
    assert!(cx.debug_bounds("desktop-hit-global-search").is_some());
    assert!(cx.debug_bounds("sessions-search").is_none());
    assert_eq!(
        shell.read_with(cx, |shell, _| shell.ui.active_drawer),
        Some(CenterDrawerKind::Sessions)
    );
}

#[gpui::test]
fn typed_navigation_switches_skills_session_and_home_without_runtime_commands(
    cx: &mut TestAppContext,
) {
    initialize_visual_test(cx);
    let (runtime, mut runtime_harness) = DesktopRuntimeBridge::instrumented_for_test();
    let (shell, cx) = add_visual_shell(cx, runtime, visual_test_projection());
    cx.run_until_parked();
    runtime_harness.drain_command_kinds();
    shell.update(cx, |shell, cx| {
        shell.app.catalog.replace_catalog(
            vec![desktop::runtime::DesktopSessionCatalogEntry {
                session_id: "desktop-visual-test".into(),
                name: Some("Active visual session".into()),
                updated_at: "2026-07-29T08:00:00Z".into(),
                ..Default::default()
            }],
            0,
        );
        shell.refresh_views(UiChangeSet::one(UiRegion::Sessions), cx);
    });
    cx.simulate_resize(size(px(1_300.), px(900.)));
    cx.run_until_parked();

    assert!(
        cx.debug_bounds("desktop-new-conversation-section")
            .is_some()
    );
    assert!(cx.debug_bounds("desktop-hit-skills").is_some());
    assert!(cx.debug_bounds("desktop-projects-section").is_some());
    assert!(cx.debug_bounds("desktop-session-row-0").is_some());

    let skills = cx
        .debug_bounds("desktop-hit-skills")
        .expect("the panel exposes the Skills route");
    cx.simulate_click(skills.center(), gpui::Modifiers::default());
    cx.run_until_parked();

    assert!(cx.debug_bounds("desktop-skills-workspace").is_some());
    assert!(cx.debug_bounds("desktop-skills-pane").is_some());
    assert!(cx.debug_bounds("desktop-skill-row-0").is_some());
    assert!(cx.debug_bounds("desktop-conversation-panel").is_none());
    assert!(cx.debug_bounds("desktop-composer-panel").is_none());
    assert!(shell.read_with(cx, |shell, _| {
        shell.ui.center_surface == CenterSurface::Skills
            && sessions_pane::view_model(&shell.app, &shell.ui).skills_active
    }));
    assert_eq!(runtime_harness.drain_command_kinds(), []);

    let active_session = cx
        .debug_bounds("desktop-session-row-0")
        .expect("the active session remains a typed navigation target");
    cx.simulate_click(active_session.center(), gpui::Modifiers::default());
    cx.run_until_parked();

    assert!(cx.debug_bounds("desktop-conversation-panel").is_some());
    assert!(cx.debug_bounds("desktop-skills-workspace").is_none());
    assert_eq!(runtime_harness.drain_command_kinds(), []);

    let skills = cx
        .debug_bounds("desktop-hit-skills")
        .expect("the Skills route remains available");
    cx.simulate_click(skills.center(), gpui::Modifiers::default());
    cx.run_until_parked();

    let new_conversation = cx
        .debug_bounds("desktop-hit-new-conversation")
        .expect("the panel exposes the new-conversation row");
    cx.simulate_click(new_conversation.center(), gpui::Modifiers::default());
    cx.run_until_parked();

    assert!(shell.read_with(cx, |shell, _| {
        shell.app.workspaces.active().projection.is_none()
    }));
    assert!(shell.read_with(cx, |shell, _| {
        workspace_for_session(shell, "desktop-visual-test").is_some()
    }));
    assert!(cx.debug_bounds("desktop-home-workspace").is_some());
    assert_eq!(
        runtime_harness.drain_command_kinds(),
        [],
        "entering Home must not dispatch any runtime command or touch session persistence"
    );
}

#[gpui::test]
fn preference_notices_preserve_repeated_messages_and_bound_the_toast_stack(
    cx: &mut TestAppContext,
) {
    initialize_visual_test(cx);
    let (shell, cx) = add_idle_visual_shell(cx);
    shell.update(cx, |shell, cx| {
        shell
            .app
            .workspaces
            .active_mut()
            .set_preference_notice("Repeated notice".into());
        shell.refresh_views(UiChangeSet::one(UiRegion::Toast), cx);
        shell
            .app
            .workspaces
            .active_mut()
            .set_preference_notice("Repeated notice".into());
        shell.refresh_views(UiChangeSet::one(UiRegion::Toast), cx);

        let repeated = shell.views.toast_host.read(cx).messages();
        assert_eq!(
            repeated
                .iter()
                .rev()
                .take(2)
                .map(AsRef::as_ref)
                .collect::<Vec<_>>(),
            ["Repeated notice", "Repeated notice"]
        );

        shell
            .app
            .workspaces
            .active_mut()
            .set_preference_notice("Third notice".into());
        shell.refresh_views(UiChangeSet::one(UiRegion::Toast), cx);
        shell
            .app
            .workspaces
            .active_mut()
            .set_preference_notice("Fourth notice".into());
        shell.refresh_views(UiChangeSet::one(UiRegion::Toast), cx);

        let bounded = shell.views.toast_host.read(cx).messages();
        assert_eq!(bounded.len(), 3);
        assert_eq!(
            bounded.iter().map(AsRef::as_ref).collect::<Vec<_>>(),
            ["Repeated notice", "Third notice", "Fourth notice"]
        );
    });
}

#[gpui::test]
fn first_session_change_rekeys_the_home_workspace_and_completes_its_command(
    cx: &mut TestAppContext,
) {
    initialize_visual_test(cx);
    let (runtime, mut runtime_harness) = DesktopRuntimeBridge::instrumented_for_test();
    let (shell, cx) = add_idle_visual_shell_with_runtime(cx, runtime);
    cx.run_until_parked();
    assert_eq!(runtime_harness.drain_command_kinds(), []);
    shell.update(cx, |shell, cx| {
        shell
            .app
            .workspaces
            .active_mut()
            .composer
            .edit("home draft");
        let intent = DesktopCommandIntent::OpenSession {
            session_id: "session-first".into(),
        };
        let command_id = shell
            .app
            .commands
            .reserve(WorkspaceKey::session("session-first"), intent.clone())
            .expect("the first open command fits the global tracker");
        shell.ui.runtime_ui_notification_count = 0;
        shell.connection.runtime_updates.push_back(
            desktop::runtime::DesktopRuntimeUpdate::SessionChanged {
                command_id,
                snapshot: visual_test_snapshot_for("session-first"),
            },
        );

        assert!(shell.poll_runtime_for_test(cx));
        assert_eq!(active_session_id(shell), Some("session-first"));
        assert_eq!(shell.app.workspaces.active().composer.draft(), "home draft");
        assert!(shell.app.commands.pending(command_id).is_none());
        assert!(shell.ui.runtime_ui_notification_count > 0);
    });
    assert_eq!(
        runtime_harness.drain_command_kinds(),
        [],
        "opening a session must not trigger a full catalog request"
    );
}

#[gpui::test]
fn runtime_command_owner_mismatch_is_rejected_and_requires_resync(cx: &mut TestAppContext) {
    initialize_visual_test(cx);
    let projection = DesktopProjection::new(visual_test_snapshot_for("owner-session-a"))
        .expect("owner session A fixture is valid");
    let (shell, cx) = add_visual_shell(
        cx,
        DesktopRuntimeBridge::disconnected_for_test(),
        projection,
    );

    shell.update(cx, |shell, cx| {
        let owner = WorkspaceKey::session("owner-session-a");
        let command_id = shell
            .app
            .commands
            .reserve(owner.clone(), DesktopCommandIntent::Reload)
            .expect("reload command fits the global tracker");
        let mut foreign = visual_test_snapshot_for("owner-session-b");
        foreign.project.selected_model_id = "foreign-model-must-not-apply".into();
        shell.connection.runtime_updates.push_back(
            desktop::runtime::DesktopRuntimeUpdate::Reloaded {
                command_id,
                metadata: desktop::runtime::DesktopRuntimeMetadataSnapshot {
                    project: foreign.project,
                    session: Some(foreign.session),
                },
            },
        );

        assert!(shell.poll_runtime_for_test(cx));
        assert!(
            shell
                .app
                .commands
                .matches(command_id, &owner, &DesktopCommandIntent::Reload,)
        );
        assert_ne!(
            shell.app.workspaces.active().project.selected_model_id,
            "foreign-model-must-not-apply"
        );
        let projection = shell
            .app
            .workspaces
            .active()
            .projection
            .as_ref()
            .expect("owner session remains hydrated");
        assert_eq!(
            projection.lifecycle(),
            DesktopProjectionLifecycle::NeedsResync
        );
        assert!(
            projection
                .issues()
                .iter()
                .any(|issue| issue.code == "command_owner_mismatch")
        );
    });
}

#[gpui::test]
fn create_and_resync_update_local_state_without_loading_the_catalog(cx: &mut TestAppContext) {
    initialize_visual_test(cx);
    let (runtime, mut runtime_harness) = DesktopRuntimeBridge::instrumented_for_test();
    let (shell, cx) = add_idle_visual_shell_with_runtime(cx, runtime);
    cx.run_until_parked();
    assert_eq!(runtime_harness.drain_command_kinds(), []);

    shell.update(cx, |shell, cx| {
        let create_id = shell
            .reserve_command(DesktopCommandIntent::CreateSession)
            .expect("create command fits the Home ledger");
        shell.connection.runtime_updates.push_back(
            desktop::runtime::DesktopRuntimeUpdate::SessionChanged {
                command_id: create_id,
                snapshot: visual_test_snapshot_for("session-created-locally"),
            },
        );
        assert!(shell.poll_runtime_for_test(cx));
        assert_eq!(
            shell.app.catalog.catalog()[0].session_id,
            "session-created-locally"
        );

        let resync_id = shell
            .reserve_command(DesktopCommandIntent::Resync)
            .expect("resync command fits the session ledger");
        shell.connection.runtime_updates.push_back(
            desktop::runtime::DesktopRuntimeUpdate::Resynced {
                command_id: resync_id,
                replacement: desktop::runtime::DesktopRuntimeResyncSnapshot::Hydrated(
                    visual_test_snapshot_for("session-created-locally"),
                ),
            },
        );
        assert!(shell.poll_runtime_for_test(cx));
    });
    assert_eq!(
        runtime_harness.drain_command_kinds(),
        [],
        "create and resync completions must use local state only"
    );
}

#[gpui::test]
fn rejected_new_prompt_promotes_home_owner_with_background_sessions(cx: &mut TestAppContext) {
    initialize_visual_test(cx);
    let (shell, cx) = add_idle_visual_shell(cx);
    shell.update(cx, |shell, cx| {
        let background_snapshot = visual_test_snapshot_for("session-background");
        let background_projection = DesktopProjection::new(background_snapshot.clone())
            .expect("the background fixture is valid");
        insert_session_workspace(
            shell,
            "session-background",
            make_session_workspace(
                background_snapshot.project,
                Some(background_projection),
                None,
            ),
        );
        shell.app.catalog.replace_catalog(
            vec![desktop::runtime::DesktopSessionCatalogEntry {
                session_id: "close-session-b".into(),
                ..Default::default()
            }],
            0,
        );
        shell
            .app
            .workspaces
            .active_mut()
            .composer
            .edit("retain this exact Home draft");
        shell
            .app
            .workspaces
            .active_mut()
            .composer_attachments
            .push(PathBuf::from("/tmp/retained-home-attachment.txt"));
        let command_id = shell
            .reserve_command(DesktopCommandIntent::Prompt)
            .expect("the Home prompt command fits the ledger");
        shell
            .app
            .workspaces
            .active_mut()
            .composer
            .begin_submit(command_id, ComposerSubmissionKind::Prompt)
            .expect("the Home draft enters pending admission");
        shell.connection.runtime_updates.push_back(
            desktop::runtime::DesktopRuntimeUpdate::PromptRejectedWithSession {
                command_id,
                snapshot: visual_test_snapshot_for("session-created"),
                error: desktop::runtime::DesktopRuntimeError {
                    code: "prompt_prepare".into(),
                    message: "the created session retained the rejected prompt".into(),
                },
            },
        );

        assert!(shell.poll_runtime_for_test(cx));
        assert_eq!(active_session_id(shell), Some("session-created"));
        assert!(workspace_for_session(shell, "session-background").is_some());
        assert!(shell.app.workspaces.get(&WorkspaceKey::Home).is_some());
        assert_eq!(
            shell.app.workspaces.active().composer.draft(),
            "retain this exact Home draft"
        );
        assert_eq!(
            shell.app.workspaces.active().composer_attachments,
            [PathBuf::from("/tmp/retained-home-attachment.txt")]
        );
        assert_eq!(
            shell.app.workspaces.active().composer.admission(),
            &ComposerAdmission::Idle
        );
        assert!(shell.app.workspaces.active().composer.rejection().is_some());
        assert!(shell.app.commands.pending(command_id).is_none());
        assert!(
            shell
                .app
                .workspaces
                .active()
                .projection
                .as_ref()
                .unwrap()
                .issues()
                .iter()
                .any(|issue| issue.code == "prompt_prepare")
        );
        assert_eq!(
            shell.app.catalog.catalog()[0].session_id,
            "session-created",
            "the first prompt must add its newly-created session locally"
        );
    });
}

#[gpui::test]
fn runtime_stop_rejects_the_pending_composer_admission(cx: &mut TestAppContext) {
    initialize_visual_test(cx);
    let (shell, cx) = add_idle_visual_shell(cx);
    shell.update(cx, |shell, _| {
        let owner = shell.app.workspaces.active_key().clone();
        let command_id = shell
            .app
            .commands
            .reserve(owner, DesktopCommandIntent::Prompt)
            .expect("test prompt fits the command tracker");
        let workspace = shell.app.workspaces.active_mut();
        workspace.composer.edit("retain this exact draft");
        workspace
            .composer
            .begin_submit(command_id, ComposerSubmissionKind::Prompt)
            .expect("test prompt enters admission");

        let transition = shell.with_controller(|controller, shell| {
            controller.reduce_runtime(
                &mut shell.app,
                desktop::runtime::DesktopRuntimeUpdate::Stopped,
            )
        });

        assert!(transition.changes().contains(UiRegion::Sessions));
        assert_eq!(
            shell.app.workspaces.active().composer.draft(),
            "retain this exact draft"
        );
        assert!(matches!(
            shell.app.workspaces.active().composer.admission(),
            ComposerAdmission::Idle
        ));
        assert_eq!(
            shell.app.workspaces.active().composer.rejection(),
            Some("desktop runtime stopped")
        );
        assert!(!shell.app.commands.contains_anywhere(|_| true));
    });
}

#[gpui::test]
fn background_workspace_advances_silently_and_switching_restores_scoped_state(
    cx: &mut TestAppContext,
) {
    initialize_visual_test(cx);
    let session_a_snapshot = visual_test_snapshot_for("session-a");
    let session_a_projection = DesktopProjection::new(session_a_snapshot)
        .expect("session A fixture is a valid product projection");
    let (runtime, mut runtime_harness) = DesktopRuntimeBridge::instrumented_for_test();
    let (shell, cx) = add_visual_shell(cx, runtime, session_a_projection);
    cx.run_until_parked();
    runtime_harness.drain_command_kinds();
    shell.update(cx, |shell, cx| {
        let mut session_b_snapshot = visual_test_snapshot_for("session-b");
        session_b_snapshot.session.active_operation = Some("operation-session-b".into());
        let session_b_projection = DesktopProjection::new(session_b_snapshot.clone())
            .expect("session B fixture is a valid product projection");
        let mut session_b = make_session_workspace(
            session_b_snapshot.project.clone(),
            Some(session_b_projection),
            None,
        );
        let change = CodingAgentFileChangeSnapshot {
            path: "session-b-only.rs".into(),
            mutation_kind: "edit".into(),
            source: "agent_edit".into(),
            operation_id: "operation-session-b".into(),
            tool_call_id: None,
            session_id: Some("session-b".into()),
            turn_id: Some("turn-session-b".into()),
            updated_sequence: 3,
            before_revision: Some("before".into()),
            after_revision: "after".into(),
            after_exists: true,
            first_changed_line: Some(4),
            added_lines: Some(1),
            removed_lines: Some(0),
            diff: None,
            hunks: Vec::new(),
        };
        let review_request = CodingAgentFileReviewRequest::from(&change);
        session_b.composer.edit("draft b");
        session_b.presentation.inspector_section = InspectorSection::Task;
        session_b.file_review = Arc::new(DesktopFileReviewState::Loading(review_request.clone()));
        shell.app.workspaces.active_mut().composer.edit("draft a");
        shell
            .app
            .workspaces
            .active_mut()
            .presentation
            .inspector_section = InspectorSection::Runtime;
        insert_session_workspace(shell, "session-b", session_b);
        let review_intent = DesktopCommandIntent::FileReview {
            request: review_request.clone(),
        };
        let review_command_id = shell
            .app
            .commands
            .reserve(WorkspaceKey::session("session-b"), review_intent.clone())
            .expect("session B test command fits the global tracker");
        let sessions = sessions_pane::view_model(&shell.app, &shell.ui);
        assert_eq!(
            sessions
                .runtime_states
                .iter()
                .find(|state| state.session_id.as_ref() == "session-b")
                .map(|state| state.status),
            Some(SemanticStatus::Running)
        );
        shell.refresh_conversation_rows_at_width(800, cx);
        shell.ui.runtime_ui_notification_count = 0;
        let mut finished_snapshot = visual_test_snapshot_for("session-b");
        finished_snapshot.session.cursor.last_event_sequence = 7;
        finished_snapshot.session.cursor.last_session_sequence = 7;
        finished_snapshot.session.context.changes.push(change);
        shell.connection.runtime_updates.push_back(
            desktop::runtime::DesktopRuntimeUpdate::PromptFinished {
                command_id: 9_002,
                operation_id: "operation-session-b".into(),
                snapshot: finished_snapshot,
                error: None,
            },
        );
        assert!(shell.poll_runtime_for_test(cx));
        assert_eq!(active_session_id(shell), Some("session-a"));
        assert_eq!(shell.ui.runtime_ui_notification_count, 0);
        assert_eq!(shell.app.workspaces.active().composer.draft(), "draft a");
        assert_eq!(
            shell.app.workspaces.active().presentation.inspector_section,
            InspectorSection::Runtime
        );
        assert!(matches!(
            shell.app.workspaces.active().file_review.as_ref(),
            DesktopFileReviewState::Empty
        ));
        assert!(
            !shell
                .app
                .commands
                .contains(shell.app.workspaces.active_key(), &review_intent,)
        );
        let background = workspace_for_session(shell, "session-b")
            .expect("session B remains parked after its background update");
        assert_eq!(
            background
                .projection
                .as_ref()
                .expect("session B remains hydrated")
                .cursor()
                .last_event_sequence,
            7
        );
        assert_eq!(background.composer.draft(), "draft b");
        assert_eq!(
            background.presentation.inspector_section,
            InspectorSection::Task
        );
        assert!(matches!(
            background.file_review.as_ref(),
            DesktopFileReviewState::Loading(request) if *request == review_request
        ));
        assert!(shell.app.commands.matches(
            review_command_id,
            &WorkspaceKey::session("session-b"),
            &review_intent,
        ));
        assert!(activate_session(shell, "session-b"));
        assert_eq!(shell.app.workspaces.active().composer.draft(), "draft b");
        assert_eq!(
            shell.app.workspaces.active().presentation.inspector_section,
            InspectorSection::Task
        );
        assert!(activate_session(shell, "session-a"));
        assert_eq!(shell.app.workspaces.active().composer.draft(), "draft a");
        assert_eq!(
            shell.app.workspaces.active().presentation.inspector_section,
            InspectorSection::Runtime
        );
        for session_id in ["session-c", "session-d"] {
            let snapshot = visual_test_snapshot_for(session_id);
            let projection = DesktopProjection::new(snapshot.clone())
                .expect("workspace-cap fixture is a valid projection");
            insert_session_workspace(
                shell,
                session_id,
                make_session_workspace(snapshot.project, Some(projection), None),
            );
        }
        assert_eq!(shell.app.workspaces.session_count(), MAX_SESSION_WORKSPACES);
        let session_e = visual_test_snapshot_for("session-e");
        assert!(
            !shell
                .app
                .install_hydrated_workspace(&session_e, false, true)
        );
        assert!(workspace_for_session(shell, "session-e").is_none());
        let workspace_ids_before = session_workspace_ids(shell);
        shell.open_session("session-e".into(), cx);
        assert_eq!(session_workspace_ids(shell), workspace_ids_before);
        assert!(
            shell
                .app
                .workspaces
                .active()
                .preference_notice
                .as_deref()
                .is_some_and(|notice| notice.contains("close one first"))
        );
    });
    assert!(
        !runtime_harness
            .drain_command_kinds()
            .contains(&desktop::runtime::DesktopRuntimeCommandKind::OpenSession)
    );
}
