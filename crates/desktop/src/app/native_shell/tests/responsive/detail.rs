use super::*;

#[gpui::test]
fn truncated_preview_opens_and_copies_the_complete_bounded_message(cx: &mut TestAppContext) {
    initialize_visual_test(cx);
    let mut snapshot = visual_test_snapshot();
    let unit = "完整消息🙂e\u{301}";
    let repeat_count =
        desktop::ui::conversation::markdown::MAX_MARKDOWN_PREVIEW_BYTES / unit.len() + 1;
    let full_text = format!(
        "BEGIN FULL MESSAGE\n{}END FULL MESSAGE",
        unit.repeat(repeat_count)
    );
    assert!(full_text.len() > desktop::ui::conversation::markdown::MAX_MARKDOWN_PREVIEW_BYTES);
    assert!(full_text.len() < MAX_COPY_BYTES);
    snapshot
        .transcript
        .items
        .push(CodingAgentSessionTranscriptItem::Assistant {
            id: "full-message-regression".into(),
            text: full_text.clone(),
            thinking: String::new(),
            images: Vec::new(),
            done: true,
            reasoning_duration_millis: None,
            model_id: None,
            completed_at: None,
        });
    let projection = DesktopProjection::new(snapshot)
        .expect("full-message fixture is a valid product projection");
    let (shell, cx) = add_visual_shell(
        cx,
        DesktopRuntimeBridge::disconnected_for_test(),
        projection,
    );
    cx.simulate_resize(size(px(700.), px(800.)));
    cx.executor().advance_clock(Duration::from_millis(100));
    for _ in 0..4 {
        cx.update(|window, _| window.refresh());
        cx.run_until_parked();
    }

    let open = cx
        .debug_bounds("desktop-open-full-message")
        .expect("truncated preview exposes an explicit full-message action");
    assert_minimum_hit_target(cx, "desktop-open-full-message");
    assert_minimum_hit_target(cx, "desktop-copy-conversation-row");
    let composer = cx
        .debug_bounds("desktop-composer-panel")
        .expect("Composer remains visible below the preview");
    let row = cx
        .debug_bounds("conversation-last-row")
        .expect("truncated preview row is mounted");
    assert!(
        open.top() >= row.top() && open.bottom() <= row.bottom() && open.bottom() <= composer.top(),
        "full-message action must be reachable inside its row and above the Composer: open={open:?}, row={row:?}, composer={composer:?}, offset={:?}",
        shell.read_with(cx, |shell, _| shell
            .app
            .workspaces
            .active()
            .presentation
            .conversation_controller
            .scroll
            .offset())
    );
    cx.simulate_click(open.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    assert_eq!(
        shell.read_with(cx, |shell, _| shell.ui.active_modal),
        Some(DesktopModalKind::FullMessage)
    );
    let dialog = cx
        .debug_bounds("desktop-full-message-dialog")
        .expect("full message uses a modal dialog");
    let scroll = cx
        .debug_bounds("desktop-full-message-scroll")
        .expect("full message uses one explicit scroll container");
    assert!(scroll.size.height < dialog.size.height);
    assert!(shell.read_with(cx, |shell, _| {
        shell
            .ui
            .conversation_full_message
            .as_ref()
            .is_some_and(|message| {
                message.text.starts_with("BEGIN FULL MESSAGE")
                    && message.text.ends_with("END FULL MESSAGE")
                    && !message.source_truncated
            })
    }));

    let copy = cx
        .debug_bounds("desktop-copy-full-message")
        .expect("full viewer exposes its complete-source copy action");
    cx.simulate_click(copy.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    assert_eq!(
        cx.read_from_clipboard().and_then(|item| item.text()),
        Some(full_text)
    );

    let close = cx
        .debug_bounds("desktop-close-full-message")
        .expect("full viewer exposes a close action");
    cx.simulate_click(close.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    assert_eq!(shell.read_with(cx, |shell, _| shell.ui.active_modal), None);
    assert!(shell.read_with(cx, |shell, _| shell.ui.conversation_full_message.is_none()));
}

#[gpui::test]
fn native_shell_primary_controls_keep_minimum_hit_targets(cx: &mut TestAppContext) {
    initialize_visual_test(cx);
    let (shell, cx) = add_visual_shell(
        cx,
        DesktopRuntimeBridge::disconnected_for_test(),
        visual_test_projection(),
    );
    shell.update(cx, |shell, cx| {
        let active_session_id = shell
            .app
            .workspaces
            .active()
            .projection
            .as_ref()
            .expect("the visual shell owns a session projection")
            .snapshot()
            .session
            .session_id
            .clone();
        shell.app.catalog.replace_catalog(
            vec![
                desktop::runtime::DesktopSessionCatalogEntry {
                    session_id: active_session_id,
                    updated_at: "2026-07-28T09:00:00Z".into(),
                    ..Default::default()
                },
                desktop::runtime::DesktopSessionCatalogEntry {
                    session_id: "recent-session-with-a-stable-action-row".into(),
                    updated_at: "2026-07-28T08:00:00Z".into(),
                    ..Default::default()
                },
            ],
            0,
        );
        shell.refresh_views(UiChangeSet::one(UiRegion::Sessions), cx);
    });

    for width in [1_300., 700.] {
        cx.simulate_resize(size(px(width), px(900.)));
        cx.run_until_parked();
        for selector in [
            "desktop-hit-toggle-sessions",
            "desktop-hit-toggle-inspector",
            "desktop-hit-submit-composer",
            "desktop-composer-model-selector",
            "desktop-composer-thinking-selector",
            "desktop-composer-profile-selector",
        ] {
            assert_minimum_hit_target(cx, selector);
        }
    }

    cx.simulate_resize(size(px(1_300.), px(900.)));
    cx.run_until_parked();
    assert_minimum_hit_target(cx, "desktop-hit-new-conversation");
    assert_minimum_hit_target(cx, "desktop-hit-refresh-projects");
    assert_minimum_hit_target(cx, "desktop-project-row-0");
    assert_minimum_hit_target(cx, "desktop-session-row-1");
    assert_minimum_hit_target(cx, "desktop-hit-session-actions-1");
}

#[gpui::test]
fn sessions_show_names_search_name_and_id_and_offer_manual_rename(cx: &mut TestAppContext) {
    initialize_visual_test(cx);
    let (runtime, mut runtime_harness) = DesktopRuntimeBridge::instrumented_for_test();
    let (shell, cx) = add_visual_shell(cx, runtime, visual_test_projection());
    cx.run_until_parked();
    runtime_harness.drain_command_kinds();
    shell.update(cx, |shell, cx| {
        shell.app.catalog.replace_catalog(
            vec![
                desktop::runtime::DesktopSessionCatalogEntry {
                    session_id: "named-session-id".into(),
                    name: Some("Release plan".into()),
                    updated_at: "9999-12-31T23:59:59Z".into(),
                    ..Default::default()
                },
                desktop::runtime::DesktopSessionCatalogEntry {
                    session_id: "unnamed-session-id".into(),
                    name: None,
                    updated_at: "9999-12-31T23:59:59Z".into(),
                    ..Default::default()
                },
            ],
            0,
        );
        shell.refresh_views(UiChangeSet::one(UiRegion::Sessions), cx);
    });
    cx.run_until_parked();
    assert!(cx.debug_bounds("desktop-session-row-0").is_some());
    assert!(cx.debug_bounds("desktop-session-row-1").is_some());

    let search = cx
        .debug_bounds("desktop-hit-global-search")
        .expect("sidebar header exposes global search");
    cx.simulate_click(search.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    assert!(cx.debug_bounds("desktop-global-search-dialog").is_some());

    cx.update(|window, app| {
        shell.update(app, |shell, app| {
            shell.views.root_modal_host.update(app, |modal, app| {
                modal.set_search_value("Release", window, app)
            });
        });
    });
    cx.run_until_parked();
    assert!(cx.debug_bounds("desktop-global-search-session-0").is_some());
    assert!(cx.debug_bounds("desktop-global-search-session-1").is_none());

    cx.update(|window, app| {
        shell.update(app, |shell, app| {
            shell.views.root_modal_host.update(app, |modal, app| {
                modal.set_search_value("unnamed-session-id", window, app)
            });
        });
    });
    cx.run_until_parked();
    assert!(cx.debug_bounds("desktop-global-search-session-0").is_some());
    assert!(cx.debug_bounds("desktop-global-search-session-1").is_none());
    let close_search = cx
        .debug_bounds("desktop-close-global-search")
        .expect("global search dialog exposes close action");
    cx.simulate_click(close_search.center(), gpui::Modifiers::default());
    cx.run_until_parked();

    let rename = cx
        .debug_bounds("desktop-hit-session-actions-1")
        .expect("unnamed session exposes its compact actions menu");
    cx.simulate_click(rename.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    choose_popup_item(cx, 0);
    assert!(cx.debug_bounds("desktop-session-rename-1").is_some());
    cx.update(|window, app| {
        shell.update(app, |shell, app| {
            shell.views.sessions_pane.update(app, |pane, app| {
                pane.set_rename_value("Recovered name", window, app)
            });
        });
    });
    cx.run_until_parked();
    let commit = cx
        .debug_bounds("desktop-hit-commit-session-rename-1")
        .expect("inline rename exposes a save action");
    cx.simulate_click(commit.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    assert_eq!(
        runtime_harness.drain_session_renames(),
        [("unnamed-session-id".into(), Some("Recovered name".into()))]
    );
}

#[gpui::test]
fn session_actions_confirm_delete_before_submitting_the_command(cx: &mut TestAppContext) {
    initialize_visual_test(cx);
    let (runtime, mut runtime_harness) = DesktopRuntimeBridge::instrumented_for_test();
    let (shell, cx) = add_visual_shell(cx, runtime, visual_test_projection());
    cx.run_until_parked();
    runtime_harness.drain_command_kinds();
    shell.update(cx, |shell, cx| {
        shell.app.catalog.replace_catalog(
            vec![desktop::runtime::DesktopSessionCatalogEntry {
                session_id: "session-to-delete".into(),
                name: Some("Release plan".into()),
                updated_at: "9999-12-31T23:59:59Z".into(),
                ..Default::default()
            }],
            0,
        );
        shell.refresh_views(UiChangeSet::one(UiRegion::Sessions), cx);
    });
    cx.run_until_parked();
    assert!(cx.debug_bounds("desktop-session-row-0").is_some());

    let open_delete_menu = |cx: &mut gpui::VisualTestContext| {
        let actions = cx
            .debug_bounds("desktop-hit-session-actions-0")
            .expect("session row exposes its compact actions menu");
        cx.simulate_click(actions.center(), gpui::Modifiers::default());
        cx.run_until_parked();
    };
    open_delete_menu(cx);
    choose_popup_item(cx, 2);
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("desktop-delete-session-dialog").is_some(),
        "delete requires a confirmation dialog"
    );
    assert_eq!(
        runtime_harness.drain_session_deletes(),
        Vec::<String>::new()
    );

    let cancel = cx
        .debug_bounds("desktop-cancel-delete-session")
        .expect("delete dialog exposes cancel");
    cx.simulate_click(cancel.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    assert!(cx.debug_bounds("desktop-delete-session-dialog").is_none());
    assert_eq!(
        runtime_harness.drain_session_deletes(),
        Vec::<String>::new()
    );

    open_delete_menu(cx);
    choose_popup_item(cx, 2);
    cx.run_until_parked();
    let confirm = cx
        .debug_bounds("desktop-confirm-delete-session")
        .expect("delete dialog exposes confirm");
    cx.simulate_click(confirm.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    assert!(cx.debug_bounds("desktop-delete-session-dialog").is_none());
    assert_eq!(
        runtime_harness.drain_session_deletes(),
        [String::from("session-to-delete")]
    );
}

#[gpui::test]
fn idle_model_selector_groups_configured_text_models_and_submits_the_exact_id(
    cx: &mut TestAppContext,
) {
    initialize_visual_test(cx);
    let (runtime, mut runtime_harness) = DesktopRuntimeBridge::instrumented_for_test();
    let (shell, cx) = add_idle_visual_shell_with_runtime(cx, runtime);
    cx.run_until_parked();
    runtime_harness.drain_command_kinds();

    let view_model = shell.read_with(cx, |shell, _| {
        conversation_header::controls_view_model(&shell.app)
    });
    assert!(shell.read_with(cx, |shell, _| {
        shell.app.workspaces.active().projection.is_none()
    }));
    assert_eq!(
        view_model
            .model_groups
            .iter()
            .map(|group| group.provider.as_ref())
            .collect::<Vec<_>>(),
        ["fixture"]
    );
    assert_eq!(
        view_model
            .model_groups
            .iter()
            .flat_map(|group| group.options.iter())
            .map(|option| option.id.as_ref())
            .collect::<Vec<_>>(),
        ["adjacent-model", "exact-target-model", "test-model"]
    );
    assert!(view_model.unavailable_current_model.is_none());

    let selector = cx
        .debug_bounds("desktop-composer-model-selector")
        .expect("the Composer model selector is visible");
    cx.simulate_click(selector.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    choose_popup_item(cx, 2);

    assert_eq!(
        runtime_harness.drain_selections(),
        [(
            desktop::runtime::DesktopRuntimeCommandKind::SelectModel,
            DesktopRuntimeOwnerTarget::home(),
            "exact-target-model".into(),
            None,
        )]
    );
}

#[gpui::test]
fn idle_profile_selector_uses_project_choices_and_submits_without_a_session(
    cx: &mut TestAppContext,
) {
    initialize_visual_test(cx);
    let (runtime, mut runtime_harness) = DesktopRuntimeBridge::instrumented_for_test();
    let (shell, cx) = add_idle_visual_shell_with_runtime(cx, runtime);
    cx.run_until_parked();
    runtime_harness.drain_command_kinds();

    let view_model = shell.read_with(cx, |shell, _| {
        conversation_header::controls_view_model(&shell.app)
    });
    assert_eq!(view_model.current_profile_id.as_ref(), "default");
    assert_eq!(
        view_model
            .profile_options
            .iter()
            .map(|option| option.id.as_ref())
            .collect::<Vec<_>>(),
        ["default", "exact-reviewer", "review-team"]
    );
    assert!(view_model.profile_options[0].selectable);
    assert!(view_model.profile_options[1].selectable);
    assert!(!view_model.profile_options[2].selectable);

    let selector = cx
        .debug_bounds("desktop-composer-profile-selector")
        .expect("the idle Composer exposes the profile selector");
    cx.simulate_click(selector.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    choose_popup_item(cx, 1);

    assert_eq!(
        runtime_harness.drain_selections(),
        [(
            desktop::runtime::DesktopRuntimeCommandKind::SelectSessionProfile,
            DesktopRuntimeOwnerTarget::home(),
            "exact-reviewer".into(),
            None,
        )]
    );
}

#[gpui::test]
fn composer_pane_render_consumes_pending_input_latency(cx: &mut TestAppContext) {
    initialize_visual_test(cx);
    let (shell, cx) = add_visual_shell(
        cx,
        DesktopRuntimeBridge::disconnected_for_test(),
        visual_test_projection(),
    );
    cx.run_until_parked();

    let changed_at = Instant::now();
    shell.update(cx, |shell, cx| {
        shell.views.composer_pane.update(cx, |pane, cx| {
            pane.latency_probe().mark_changed_at(changed_at);
            cx.notify();
        });
    });
    cx.run_until_parked();

    assert!(shell.read_with(cx, |shell, cx| {
        let pane = shell.views.composer_pane.read(cx);
        pane.latency_probe().pending_is_empty()
            && pane
                .latency_probe()
                .last_observed()
                .is_some_and(|latency| latency <= changed_at.elapsed())
    }));
}

#[gpui::test]
fn composer_auto_grows_from_one_line_to_its_bounded_maximum(cx: &mut TestAppContext) {
    initialize_visual_test(cx);
    let (shell, cx) = add_visual_shell(
        cx,
        DesktopRuntimeBridge::disconnected_for_test(),
        visual_test_projection(),
    );
    cx.simulate_resize(size(px(700.), px(800.)));
    cx.run_until_parked();

    shell.update(cx, |shell, cx| {
        shell
            .app
            .workspaces
            .active_mut()
            .composer
            .edit("one compact line");
        shell.app.workspaces.active_mut().composer_needs_sync = true;
        cx.notify();
    });
    settle_visual_measurements(cx);
    let one_line_height = f32::from(
        cx.debug_bounds("desktop-composer-panel")
            .expect("one-line Composer is laid out")
            .size
            .height,
    );
    let compact_content_height = f32::from(
        cx.debug_bounds("desktop-composer-content")
            .expect("compact Composer content is laid out")
            .size
            .height,
    );
    assert!(
        (48. ..=56.).contains(&compact_content_height),
        "empty and one-line Composer content stays compact: {compact_content_height}"
    );

    shell.update(cx, |shell, cx| {
        shell.app.workspaces.active_mut().composer.edit(
            (1..=20)
                .map(|line| format!("composer line {line} 中文 🙂"))
                .collect::<Vec<_>>()
                .join("\n"),
        );
        shell.app.workspaces.active_mut().composer_needs_sync = true;
        cx.notify();
    });
    settle_visual_measurements(cx);
    let maximum_height = f32::from(
        cx.debug_bounds("desktop-composer-panel")
            .expect("maximum-height Composer is laid out")
            .size
            .height,
    );

    shell.update(cx, |shell, cx| {
        shell.app.workspaces.active_mut().composer.edit(
            (1..=40)
                .map(|line| format!("saturation line {line}"))
                .collect::<Vec<_>>()
                .join("\n"),
        );
        shell.app.workspaces.active_mut().composer_needs_sync = true;
        cx.notify();
    });
    settle_visual_measurements(cx);
    let saturated_height = f32::from(
        cx.debug_bounds("desktop-composer-panel")
            .expect("saturated Composer is laid out")
            .size
            .height,
    );

    assert!(
        maximum_height > one_line_height,
        "Composer must grow beyond its one-line geometry: one={one_line_height}, max={maximum_height}"
    );
    assert!(
        maximum_height <= COMPOSER_MAX_HEIGHT as f32 + 1.,
        "Composer auto-grow must remain bounded: {maximum_height}"
    );
    assert!(
        (saturated_height - maximum_height).abs() <= 1.,
        "content beyond the eight-row auto-grow maximum must not keep expanding the Composer: twenty={maximum_height}, forty={saturated_height}"
    );
}

#[gpui::test]
fn project_directory_state_is_scoped_while_the_composer_omits_its_path_chip(
    cx: &mut TestAppContext,
) {
    initialize_visual_test(cx);
    let (idle_shell, cx) = add_idle_visual_shell(cx);

    assert!(idle_shell.read_with(cx, |shell, _| {
        shell.app.workspaces.active().project_directory().is_none()
    }));

    for width in [1_300., 700.] {
        cx.simulate_resize(size(px(width), px(800.)));
        settle_visual_measurements(cx);
        let attachment = cx
            .debug_bounds("desktop-hit-add-composer-attachments")
            .expect("attachment action remains in the Composer bottom-left");
        let model = cx
            .debug_bounds("desktop-composer-model-selector")
            .expect("Model follows the attachment action without a project chip");
        let submit = cx
            .debug_bounds("desktop-hit-submit-composer")
            .expect("submit action remains in the Composer bottom-right");
        assert!(attachment.right() <= model.left());
        assert!(model.right() <= submit.left());
        assert!(
            cx.debug_bounds("desktop-project-directory-control")
                .is_none()
        );
        assert_minimum_hit_target(cx, "desktop-hit-add-composer-attachments");
        assert_minimum_hit_target(cx, "desktop-hit-submit-composer");
    }

    let long_path = PathBuf::from("/工作区/这是一个需要被压缩但必须保留完整辅助信息的项目目录/evo");
    let mut session_snapshot = visual_test_snapshot();
    session_snapshot.project.cwd = long_path.clone();
    let session_projection = DesktopProjection::new(session_snapshot)
        .expect("long-path session fixture is a valid product projection");
    let (session_shell, cx) = add_visual_shell(
        cx,
        DesktopRuntimeBridge::disconnected_for_test(),
        session_projection,
    );
    assert_eq!(
        session_shell.read_with(cx, |shell, _| shell
            .app
            .workspaces
            .active()
            .project_directory()
            .map(PathBuf::from)),
        Some(long_path)
    );
    cx.simulate_resize(size(px(700.), px(800.)));
    settle_visual_measurements(cx);
    assert!(
        cx.debug_bounds("desktop-project-directory-control")
            .is_none()
    );

    let (pending_shell, cx) = add_idle_visual_shell(cx);
    pending_shell.update(cx, |shell, cx| {
        shell
            .app
            .workspaces
            .active_mut()
            .composer
            .edit("submit against the frozen project target");
        shell
            .app
            .workspaces
            .active_mut()
            .composer
            .begin_submit(401, ComposerSubmissionKind::Prompt)
            .expect("Home draft enters pending admission");
        shell.refresh_views(UiChangeSet::one(UiRegion::Composer), cx);
    });
    assert!(
        cx.debug_bounds("desktop-project-directory-control")
            .is_none()
    );
    assert!(pending_shell.read_with(cx, |shell, _| {
        !shell.app.workspaces.active().project_directory_editable()
    }));
}

#[gpui::test]
fn composer_running_authorization_and_rejection_fit_at_narrow_width(cx: &mut TestAppContext) {
    initialize_visual_test(cx);

    let mut running_snapshot = visual_test_snapshot();
    running_snapshot.session.active_operation = Some("operation-running-composer".into());
    let running_projection = DesktopProjection::new(running_snapshot)
        .expect("running Composer fixture is a valid product projection");
    let (_, cx) = add_visual_shell(
        cx,
        DesktopRuntimeBridge::disconnected_for_test(),
        running_projection,
    );
    cx.simulate_resize(size(px(700.), px(800.)));
    settle_visual_measurements(cx);
    assert_composer_regions_do_not_overlap(cx, false);
    let abort = cx
        .debug_bounds("desktop-hit-abort-operation")
        .expect("running operation exposes the critical Abort action");
    assert_eq!(f32::from(abort.size.height), 40.);
    let submit = cx
        .debug_bounds("desktop-hit-submit-composer")
        .expect("running Composer keeps one primary send action");
    assert_eq!(f32::from(submit.size.height), 36.);
    assert!(
        cx.debug_bounds("desktop-composer-running-mode-selector")
            .is_none(),
        "running Composer must not expose Steer/Follow mode state"
    );

    let mut authorization_snapshot = visual_test_snapshot();
    authorization_snapshot
        .session
        .pending_authorizations
        .push(ToolAuthorizationRequest {
            authorization_id: "authorization-composer-layout".into(),
            operation_id: "operation-composer-layout".into(),
            turn_id: "turn-composer-layout".into(),
            tool_call_id: "tool-composer-layout".into(),
            tool_name: "bash".into(),
            risk: ToolAuthorizationRisk::ShellExecution,
            scope: ToolAuthorizationScope::Shell {
                cwd: "/desktop-visual-test".into(),
                command_fingerprint: "composer-layout-fingerprint".into(),
            },
            preview: ToolAuthorizationPreview {
                summary: "Authorize the pending shell command".into(),
                path: None,
                command: Some("cargo check".into()),
                cwd: Some("/desktop-visual-test".into()),
                content_preview: None,
            },
            capability_generation: 0,
            requested_at: "2026-07-27T00:00:00Z".into(),
        });
    let authorization_projection = DesktopProjection::new(authorization_snapshot)
        .expect("authorization Composer fixture is a valid product projection");
    let (_, cx) = add_visual_shell(
        cx,
        DesktopRuntimeBridge::disconnected_for_test(),
        authorization_projection,
    );
    cx.simulate_resize(size(px(700.), px(800.)));
    settle_visual_measurements(cx);
    assert_composer_regions_do_not_overlap(cx, true);

    let (rejection_shell, cx) = add_visual_shell(
        cx,
        DesktopRuntimeBridge::disconnected_for_test(),
        visual_test_projection(),
    );
    rejection_shell.update(cx, |shell, cx| {
        shell
            .app
            .workspaces
            .active_mut()
            .composer
            .edit("retry this exact draft");
        shell
            .app
            .workspaces
            .active_mut()
            .composer
            .begin_submit(91, ComposerSubmissionKind::Prompt)
            .expect("test draft starts a pending submission");
        shell
            .app
            .workspaces
            .active_mut()
            .composer
            .rejected(
                91,
                "The submitted draft was rejected and remains available for editing.",
            )
            .expect("matching rejection is applied");
        shell.refresh_views(UiChangeSet::one(UiRegion::Composer), cx);
    });
    cx.simulate_resize(size(px(700.), px(800.)));
    settle_visual_measurements(cx);
    assert_composer_regions_do_not_overlap(cx, true);
}
