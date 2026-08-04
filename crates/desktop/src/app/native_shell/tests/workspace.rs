use super::*;

#[gpui::test]
fn idle_session_catalog_is_loaded_only_by_explicit_refresh(cx: &mut TestAppContext) {
    initialize_visual_test(cx);
    let (runtime, mut runtime_harness) = DesktopRuntimeBridge::instrumented_for_test();
    let (shell, cx) = add_idle_visual_shell_with_runtime(cx, runtime);

    cx.run_until_parked();
    assert_eq!(
        shell.read_with(cx, |shell, _| shell.app.catalog.state().clone()),
        ProjectCatalogState::NotLoaded
    );
    assert_eq!(
        runtime_harness.drain_command_kinds(),
        [],
        "NativeShell::new must not auto-load the session catalog"
    );
    cx.executor().advance_clock(Duration::from_secs(60));
    cx.run_until_parked();
    assert_eq!(
        runtime_harness.drain_command_kinds(),
        [],
        "an idle shell must not arm a catalog refresh timer"
    );

    cx.simulate_resize(size(px(700.), px(800.)));
    cx.run_until_parked();
    let toggle = cx
        .debug_bounds("desktop-hit-toggle-sessions")
        .expect("idle Header exposes the Sessions drawer toggle");
    cx.simulate_click(toggle.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    assert_eq!(
        runtime_harness.drain_command_kinds(),
        [],
        "opening the Sessions surface must remain read-free"
    );

    assert!(
        cx.debug_bounds("desktop-projects-state-not-loaded")
            .is_some(),
        "the unloaded catalog has a local Projects state"
    );
    let refresh = cx
        .debug_bounds("desktop-hit-refresh-projects")
        .expect("Projects exposes its direct explicit refresh action");
    cx.simulate_click(refresh.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    assert_eq!(
        runtime_harness.drain_command_kinds(),
        [desktop::runtime::DesktopRuntimeCommandKind::ListSessions]
    );
    assert_eq!(
        shell.read_with(cx, |shell, _| shell.app.catalog.state().clone()),
        ProjectCatalogState::Loading
    );
    assert!(
        cx.debug_bounds("desktop-projects-state-loading").is_some(),
        "the pending catalog has a local loading state"
    );
    shell.update(cx, |shell, cx| shell.request_session_catalog(cx));
    assert_eq!(
        runtime_harness.drain_command_kinds(),
        [],
        "a pending explicit refresh must be deduplicated"
    );
    shell.update(cx, |shell, cx| {
        let command_id = shell
            .app
            .commands
            .command_id_for(
                shell.app.workspaces.active_key(),
                &DesktopCommandIntent::ListSessions,
            )
            .expect("the explicit refresh remains pending");
        shell.connection.runtime_updates.push_back(
            desktop::runtime::DesktopRuntimeUpdate::SessionsListed {
                command_id,
                sessions: vec![desktop::runtime::DesktopSessionCatalogEntry {
                    session_id: "explicit-refresh-session".into(),
                    ..Default::default()
                }],
                omitted: 0,
            },
        );
        assert!(shell.poll_runtime_for_test(cx));
        assert_eq!(
            shell.app.catalog.catalog()[0].session_id,
            "explicit-refresh-session"
        );
        assert_eq!(shell.app.catalog.state(), &ProjectCatalogState::Ready);
        assert!(shell.app.workspaces.active().preference_notice.is_none());
    });
    cx.run_until_parked();
    assert!(cx.debug_bounds("desktop-projects-tree").is_some());
    assert!(cx.debug_bounds("desktop-projects-state-loading").is_none());
    cx.executor().advance_clock(Duration::from_secs(60));
    cx.run_until_parked();
    assert_eq!(
        runtime_harness.drain_command_kinds(),
        [],
        "a successful explicit refresh must not schedule another load"
    );
}

#[gpui::test]
fn observed_automatic_name_updates_the_local_session_catalog(cx: &mut TestAppContext) {
    initialize_visual_test(cx);
    let (shell, cx) = add_idle_visual_shell(cx);
    shell.update(cx, |shell, cx| {
        shell.app.catalog.replace_catalog(
            vec![desktop::runtime::DesktopSessionCatalogEntry {
                session_id: "auto-named-session".into(),
                name: None,
                ..Default::default()
            }],
            0,
        );
        shell.connection.runtime_updates.push_back(
            desktop::runtime::DesktopRuntimeUpdate::SessionNameObserved {
                session_id: "auto-named-session".into(),
                name: Some("询问助手名字".into()),
                updated_at: "2026-07-30T02:24:11Z".into(),
            },
        );

        assert!(shell.poll_runtime_for_test(cx));
        let session = &shell.app.catalog.catalog()[0];
        assert_eq!(session.name.as_deref(), Some("询问助手名字"));
        assert_eq!(session.updated_at, "2026-07-30T02:24:11Z");
        assert!(shell.app.workspaces.active().preference_notice.is_none());
    });
}

#[gpui::test]
fn explicit_session_catalog_refresh_failure_reports_error_without_retry(cx: &mut TestAppContext) {
    initialize_visual_test(cx);
    let (shell, cx) = add_idle_visual_shell(cx);

    shell.update(cx, |shell, cx| {
        shell.request_session_catalog(cx);
        assert_eq!(
            shell.app.workspaces.active().preference_notice.as_deref(),
            Some("desktop runtime command queue is closed")
        );
        assert_eq!(
            shell.app.catalog.state(),
            &ProjectCatalogState::Error {
                message: "desktop runtime command queue is closed".into()
            }
        );
        assert!(
            !shell.active_command_contains(&DesktopCommandIntent::ListSessions),
            "failed admission must release the pending refresh"
        );
    });
    cx.executor().advance_clock(Duration::from_secs(60));
    cx.run_until_parked();
    shell.update(cx, |shell, _cx| {
        assert_eq!(
            shell.app.workspaces.active().preference_notice.as_deref(),
            Some("desktop runtime command queue is closed")
        );
        assert!(matches!(
            shell.app.catalog.state(),
            ProjectCatalogState::Error { .. }
        ));
        assert!(
            !shell.active_command_contains(&DesktopCommandIntent::ListSessions),
            "failed refresh must not schedule another attempt"
        );
    });
    cx.run_until_parked();
    assert!(cx.debug_bounds("desktop-projects-state-error").is_some());
}

#[gpui::test]
fn rejected_session_catalog_refresh_keeps_typed_error_state(cx: &mut TestAppContext) {
    initialize_visual_test(cx);
    let (runtime, mut runtime_harness) = DesktopRuntimeBridge::instrumented_for_test();
    let (shell, cx) = add_idle_visual_shell_with_runtime(cx, runtime);
    cx.run_until_parked();

    shell.update(cx, |shell, cx| shell.request_session_catalog(cx));
    assert_eq!(
        runtime_harness.drain_command_kinds(),
        [desktop::runtime::DesktopRuntimeCommandKind::ListSessions]
    );
    shell.update(cx, |shell, cx| {
        let command_id = shell
            .app
            .commands
            .command_id_for(
                shell.app.workspaces.active_key(),
                &DesktopCommandIntent::ListSessions,
            )
            .expect("refresh is pending before rejection");
        shell.connection.runtime_updates.push_back(
            desktop::runtime::DesktopRuntimeUpdate::CommandRejected {
                command_id,
                command: desktop::runtime::DesktopRuntimeCommandKind::ListSessions,
                code: "catalog_unavailable".into(),
                message: "private runtime detail must not become catalog state".into(),
            },
        );
        assert!(shell.poll_runtime_for_test(cx));
        assert_eq!(
            shell.app.catalog.state(),
            &ProjectCatalogState::Error {
                message: "ListSessions rejected (catalog_unavailable)".into()
            }
        );
        assert!(
            !shell
                .app
                .catalog
                .state()
                .error_message()
                .unwrap()
                .contains("private runtime detail")
        );
    });
}

#[gpui::test]
fn projects_local_empty_omitted_and_legacy_states_are_explicit(cx: &mut TestAppContext) {
    initialize_visual_test(cx);
    let (shell, cx) = add_idle_visual_shell(cx);
    cx.simulate_resize(size(px(1_300.), px(900.)));
    cx.run_until_parked();

    shell.update(cx, |shell, cx| {
        shell.app.catalog.replace_catalog(Vec::new(), 0);
        shell.refresh_views(UiChangeSet::one(UiRegion::Sessions), cx);
    });
    cx.run_until_parked();
    assert!(cx.debug_bounds("desktop-projects-state-empty").is_some());
    assert!(cx.debug_bounds("desktop-projects-tree").is_none());

    shell.update(cx, |shell, cx| {
        shell.app.catalog.replace_catalog(
            vec![desktop::runtime::DesktopSessionCatalogEntry {
                session_id: "legacy-visible-session".into(),
                name: Some("Legacy visible session".into()),
                updated_at: "2026-07-29T08:00:00Z".into(),
                ..Default::default()
            }],
            4,
        );
        shell.refresh_views(UiChangeSet::one(UiRegion::Sessions), cx);
    });
    cx.run_until_parked();
    assert!(cx.debug_bounds("desktop-projects-state-empty").is_none());
    assert!(cx.debug_bounds("desktop-projects-tree").is_some());
    assert!(cx.debug_bounds("desktop-project-row-0").is_some());
    assert!(cx.debug_bounds("desktop-session-row-0").is_some());
    assert!(cx.debug_bounds("desktop-projects-state-omitted").is_some());
    assert_eq!(
        shell.read_with(cx, |shell, _| shell.app.catalog.project_groups()[0]
            .workspace
            .kind),
        coding_agent::api::view::CodingAgentWorkspaceKind::Legacy
    );
}

#[gpui::test]
fn projectless_sessions_share_a_conversations_section_and_projects_offer_new_conversation(
    cx: &mut TestAppContext,
) {
    initialize_visual_test(cx);
    let (shell, cx) = add_idle_visual_shell(cx);
    let project_path = PathBuf::from("/work/catalog-project");
    shell.update(cx, |shell, cx| {
        let projectless_workspace =
            |group_id: &str| coding_agent::api::view::CodingAgentWorkspaceOverview {
                group_id: group_id.into(),
                kind: coding_agent::api::view::CodingAgentWorkspaceKind::Projectless,
                display_name: "Projectless".into(),
                display_path: None,
            };
        let project_workspace = coding_agent::api::view::CodingAgentWorkspaceOverview {
            group_id: "project:catalog".into(),
            kind: coding_agent::api::view::CodingAgentWorkspaceKind::Project,
            display_name: "catalog-project".into(),
            display_path: Some(project_path.clone()),
        };
        shell.app.catalog.replace_catalog(
            vec![
                desktop::runtime::DesktopSessionCatalogEntry {
                    session_id: "conversation-a".into(),
                    workspace: projectless_workspace("projectless:session-a"),
                    ..Default::default()
                },
                desktop::runtime::DesktopSessionCatalogEntry {
                    session_id: "conversation-b".into(),
                    workspace: projectless_workspace("projectless:session-b"),
                    ..Default::default()
                },
                desktop::runtime::DesktopSessionCatalogEntry {
                    session_id: "project-session".into(),
                    workspace: project_workspace,
                    ..Default::default()
                },
            ],
            0,
        );
        shell.refresh_views(UiChangeSet::one(UiRegion::Sessions), cx);
    });

    cx.simulate_resize(size(px(1_300.), px(900.)));
    cx.run_until_parked();
    assert!(cx.debug_bounds("desktop-conversations-section").is_some());
    assert!(cx.debug_bounds("desktop-conversation-sessions").is_some());
    assert!(cx.debug_bounds("desktop-session-row-0").is_some());
    assert!(cx.debug_bounds("desktop-session-row-1").is_some());
    assert!(cx.debug_bounds("desktop-project-row-0").is_none());
    assert!(cx.debug_bounds("desktop-project-row-1").is_some());
    let new_project_conversation = cx
        .debug_bounds("desktop-hit-new-project-conversation-1")
        .expect("each project row exposes a direct new-conversation action");
    cx.simulate_click(
        new_project_conversation.center(),
        gpui::Modifiers::default(),
    );
    cx.run_until_parked();

    assert!(shell.read_with(cx, |shell, _| {
        shell.app.workspaces.active_key() == &WorkspaceKey::Home
            && shell.app.workspaces.active().draft_workspace_selection
                == CodingAgentWorkspaceSelection::project(project_path.clone())
    }));
}

#[gpui::test]
fn projects_tree_disclosure_preserves_order_at_minimum_width_and_in_drawer(
    cx: &mut TestAppContext,
) {
    initialize_visual_test(cx);
    let preferences = DesktopPreferences {
        sessions_panel_width: SESSION_PANEL_MIN_WIDTH,
        ..DesktopPreferences::default()
    };
    let (shell, cx) = add_idle_visual_shell_with_preferences(
        cx,
        DesktopRuntimeBridge::disconnected_for_test(),
        preferences,
    );
    shell.update(cx, |shell, cx| {
        shell.app.catalog.replace_catalog(
            vec![
                desktop::runtime::DesktopSessionCatalogEntry {
                    session_id: "stable-first-session".into(),
                    name: Some("First session with a long label".into()),
                    updated_at: "2026-07-29T09:00:00Z".into(),
                    ..Default::default()
                },
                desktop::runtime::DesktopSessionCatalogEntry {
                    session_id: "stable-second-session".into(),
                    name: Some("Second session".into()),
                    updated_at: "2026-07-29T08:00:00Z".into(),
                    ..Default::default()
                },
            ],
            0,
        );
        shell.refresh_views(UiChangeSet::one(UiRegion::Sessions), cx);
    });

    cx.simulate_resize(size(px(1_300.), px(900.)));
    cx.run_until_parked();
    let panel = cx
        .debug_bounds("desktop-sessions-panel")
        .expect("minimum-width Sidebar remains docked");
    assert_eq!(f32::from(panel.size.width), SESSION_PANEL_MIN_WIDTH as f32);
    let new_conversation = cx
        .debug_bounds("desktop-hit-new-conversation")
        .expect("New conversation remains first");
    let skills = cx
        .debug_bounds("desktop-hit-skills")
        .expect("Skills remains second");
    let project = cx
        .debug_bounds("desktop-project-row-0")
        .expect("project disclosure follows fixed navigation");
    let session = cx
        .debug_bounds("desktop-session-row-0")
        .expect("nested session follows its project");
    assert!(new_conversation.origin.y < skills.origin.y);
    assert!(skills.origin.y < project.origin.y);
    assert!(project.origin.y < session.origin.y);
    for selector in [
        "desktop-hit-refresh-projects",
        "desktop-project-row-0",
        "desktop-session-row-0",
        "desktop-hit-session-actions-0",
    ] {
        assert_minimum_hit_target(cx, selector);
        let bounds = cx.debug_bounds(selector).unwrap();
        assert!(bounds.origin.x >= panel.origin.x);
        assert!(bounds.origin.x + bounds.size.width <= panel.origin.x + panel.size.width);
    }

    cx.simulate_click(project.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    assert!(cx.debug_bounds("desktop-project-sessions-0").is_none());
    assert!(cx.debug_bounds("desktop-session-row-0").is_none());
    assert!(shell.read_with(cx, |shell, _| {
        shell.app.catalog.project_groups()[0].collapsed
    }));

    let collapsed_project = cx.debug_bounds("desktop-project-row-0").unwrap();
    cx.simulate_click(collapsed_project.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    assert!(cx.debug_bounds("desktop-project-sessions-0").is_some());
    assert!(cx.debug_bounds("desktop-session-row-0").is_some());
    assert!(!shell.read_with(cx, |shell, _| {
        shell.app.catalog.project_groups()[0].collapsed
    }));

    cx.simulate_resize(size(px(700.), px(900.)));
    cx.run_until_parked();
    let toggle = cx.debug_bounds("desktop-hit-toggle-sessions").unwrap();
    cx.simulate_click(toggle.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    assert!(cx.debug_bounds("desktop-sessions-drawer").is_some());
    assert!(cx.debug_bounds("desktop-sidebar-evo-mark").is_some());
    assert!(cx.debug_bounds("desktop-project-row-0").is_some());
    assert!(cx.debug_bounds("desktop-session-row-1").is_some());
    assert_minimum_hit_target(cx, "desktop-hit-close-narrow-sessions");
    assert_minimum_hit_target(cx, "desktop-hit-refresh-projects");
    assert_minimum_hit_target(cx, "desktop-hit-session-actions-1");
}

#[gpui::test]
fn idle_shell_constructs_all_bounded_view_models_without_session_facts(cx: &mut TestAppContext) {
    initialize_visual_test(cx);
    let (shell, cx) = add_idle_visual_shell(cx);
    shell.update(cx, |shell, cx| {
        assert!(shell.app.workspaces.active().projection.is_none());
        assert!(
            sessions_pane::view_model(&shell.app, &shell.ui)
                .active_session_id
                .is_empty()
        );
        assert!(!composer_pane::view_model(shell.app.workspaces.active()).composer_running);
        let inspector =
            inspector_pane::view_model(&shell.app, &shell.ui, shell.global_skills.len());
        assert_eq!(inspector.active_operation, "—");
        assert_eq!(inspector.stream_id, "—");
        assert!(
            root_modal_host::view_model(&shell.app, &shell.ui)
                .authorization
                .is_none()
        );
        assert!(shell.views.toast_host.read(cx).messages().len() <= 3);
        assert_eq!(
            conversation_pane::view_model(shell.app.workspaces.active(), &shell.ui).visible_count,
            0
        );
        let header = conversation_header::view_model(&shell.app, &shell.ui);
        assert_eq!(header.profile.as_ref(), "Default");
        assert_eq!(header.current_profile_id.as_ref(), "default");
        assert_eq!(
            skills_pane::view_model(&shell.global_skills).skills.len(),
            1
        );
        assert!(!sessions_pane::view_model(&shell.app, &shell.ui).skills_active);
    });

    for (width, height, expected_center_width, sidebar_visible) in [
        (1_300., 900., 1_060., true),
        (900., 800., 660., true),
        (700., 800., 700., false),
    ] {
        cx.simulate_resize(size(px(width), px(height)));
        cx.run_until_parked();
        let home = cx
            .debug_bounds("desktop-home-workspace")
            .expect("idle workspace is visible");
        assert_eq!(f32::from(home.size.width), expected_center_width);
        assert_eq!(
            cx.debug_bounds("desktop-sessions-panel").is_some(),
            sidebar_visible
        );
        assert!(cx.debug_bounds("desktop-conversation-panel").is_none());
        assert!(cx.debug_bounds("desktop-inspector-panel").is_none());
        let header = cx
            .debug_bounds("desktop-conversation-header")
            .expect("center header remains mounted on Home");
        let body = cx
            .debug_bounds("desktop-center-body")
            .expect("center body remains mounted on Home");
        assert_eq!(f32::from(header.size.width), expected_center_width);
        assert_eq!(
            f32::from(header.size.height),
            crate::ui::shell::CENTER_HEADER_HEIGHT as f32
        );
        assert_eq!(f32::from(body.size.width), expected_center_width);
        assert_eq!(
            f32::from(body.origin.y - header.origin.y),
            crate::ui::shell::CENTER_HEADER_HEIGHT as f32
        );
        assert!(cx.debug_bounds("desktop-evo-wordmark").is_some());
        assert!(cx.debug_bounds("desktop-composer-panel").is_some());
    }
}

#[gpui::test]
fn feature_presenters_are_pure_and_repeatable(cx: &mut TestAppContext) {
    initialize_visual_test(cx);
    let (shell, cx) = add_idle_visual_shell(cx);
    shell.update(cx, |shell, _| {
        assert_eq!(
            sessions_pane::view_model(&shell.app, &shell.ui),
            sessions_pane::view_model(&shell.app, &shell.ui)
        );
        assert_eq!(
            composer_pane::view_model(shell.app.workspaces.active()),
            composer_pane::view_model(shell.app.workspaces.active())
        );
        assert_eq!(
            conversation_header::view_model(&shell.app, &shell.ui),
            conversation_header::view_model(&shell.app, &shell.ui)
        );
        assert_eq!(
            root_modal_host::view_model(&shell.app, &shell.ui),
            root_modal_host::view_model(&shell.app, &shell.ui)
        );
        assert_eq!(
            center_drawer_host::view_model(&shell.app, &shell.ui),
            center_drawer_host::view_model(&shell.app, &shell.ui)
        );
        assert_eq!(
            skills_pane::view_model(&shell.global_skills),
            skills_pane::view_model(&shell.global_skills)
        );

        let first_inspector =
            inspector_pane::view_model(&shell.app, &shell.ui, shell.global_skills.len());
        let second_inspector =
            inspector_pane::view_model(&shell.app, &shell.ui, shell.global_skills.len());
        assert_eq!(first_inspector, second_inspector);

        let first_conversation =
            conversation_pane::view_model(shell.app.workspaces.active(), &shell.ui);
        let second_conversation =
            conversation_pane::view_model(shell.app.workspaces.active(), &shell.ui);
        assert_eq!(
            first_conversation.snapshot(),
            second_conversation.snapshot()
        );
    });
}

#[gpui::test]
fn home_hero_scales_across_idle_viewports_and_yields_height_to_the_composer(
    cx: &mut TestAppContext,
) {
    initialize_visual_test(cx);
    let (_, cx) = add_idle_visual_shell(cx);

    for (width, height, expected_wordmark_width) in [
        (1_300., 900., 360.),
        (900., 800., 320.),
        (700., 800., 280.),
        (1_300., 480., 280.),
    ] {
        cx.simulate_resize(size(px(width), px(height)));
        cx.run_until_parked();

        let body = cx.debug_bounds("desktop-center-body").unwrap();
        let home = cx.debug_bounds("desktop-home-pane").unwrap();
        let hero = cx.debug_bounds("desktop-home-hero").unwrap();
        let wordmark = cx.debug_bounds("desktop-evo-wordmark").unwrap();
        let headline = cx.debug_bounds("desktop-home-headline").unwrap();
        let description = cx.debug_bounds("desktop-home-description").unwrap();
        let composer = cx.debug_bounds("desktop-composer-panel").unwrap();

        assert_eq!(f32::from(wordmark.size.width), expected_wordmark_width);
        assert!(
            (f32::from(wordmark.size.height) - expected_wordmark_width * 128. / 360.).abs() <= 1.,
            "wordmark must retain its vector aspect ratio at {width}x{height}"
        );
        assert!(hero.top() >= home.top());
        assert!(wordmark.top() >= hero.top());
        assert!(headline.top() >= wordmark.bottom());
        assert!(description.top() >= headline.bottom());
        assert!(description.bottom() <= hero.bottom());
        assert!(home.bottom() <= composer.top() + px(1.));
        assert!(composer.bottom() <= body.bottom() + px(1.));
        assert!(f32::from(composer.size.height) >= COMPOSER_MIN_HEIGHT as f32);
    }
}

#[gpui::test]
fn home_geometry_is_independent_of_sidebar_catalog_refresh_state(cx: &mut TestAppContext) {
    initialize_visual_test(cx);
    let (shell, cx) = add_idle_visual_shell(cx);
    cx.simulate_resize(size(px(1_300.), px(900.)));
    cx.run_until_parked();

    let selectors = [
        "desktop-home-pane",
        "desktop-home-hero",
        "desktop-evo-wordmark",
        "desktop-home-headline",
        "desktop-home-description",
        "desktop-composer-panel",
    ];
    let initial = selectors.map(|selector| {
        cx.debug_bounds(selector)
            .unwrap_or_else(|| panic!("missing Home geometry selector {selector}"))
    });

    shell.update(cx, |shell, cx| {
        shell.app.catalog.begin_refresh();
        shell.refresh_views(UiChangeSet::one(UiRegion::Sessions), cx);
        cx.notify();
    });
    cx.run_until_parked();
    assert!(cx.debug_bounds("desktop-projects-state-loading").is_some());
    let loading = selectors.map(|selector| cx.debug_bounds(selector).unwrap());
    assert_eq!(loading, initial);

    shell.update(cx, |shell, cx| {
        shell.app.catalog.replace_catalog(
            vec![desktop::runtime::DesktopSessionCatalogEntry {
                session_id: "catalog-layout-probe".into(),
                name: Some("Catalog layout probe".into()),
                ..Default::default()
            }],
            0,
        );
        shell.refresh_views(UiChangeSet::one(UiRegion::Sessions), cx);
        cx.notify();
    });
    cx.run_until_parked();
    assert!(cx.debug_bounds("desktop-projects-tree").is_some());
    let ready = selectors.map(|selector| cx.debug_bounds(selector).unwrap());
    assert_eq!(ready, initial);
}

#[gpui::test]
fn home_respects_an_explicit_inspector_preference(cx: &mut TestAppContext) {
    initialize_visual_test(cx);
    let preferences = visual_preferences_with_inspector();
    let (shell, cx) = add_idle_visual_shell_with_preferences(
        cx,
        DesktopRuntimeBridge::disconnected_for_test(),
        preferences,
    );
    cx.simulate_resize(size(px(1_300.), px(900.)));
    cx.run_until_parked();

    assert!(cx.debug_bounds("desktop-sessions-panel").is_some());
    assert!(cx.debug_bounds("desktop-inspector-panel").is_some());
    assert_eq!(
        f32::from(
            cx.debug_bounds("desktop-home-workspace")
                .expect("Home center remains visible")
                .size
                .width
        ),
        740.
    );
    assert!(shell.read_with(cx, |shell, _| {
        shell.app.preferences.context_panel_visible
    }));
}

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
            operation_id: "operation-session-b".into(),
            tool_call_id: None,
            updated_sequence: 3,
            first_changed_line: Some(4),
            added_lines: Some(1),
            removed_lines: Some(0),
            diff: None,
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
