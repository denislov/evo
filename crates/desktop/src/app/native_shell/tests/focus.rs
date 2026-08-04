use super::*;

#[gpui::test]
fn native_shell_focus_and_responsive_bounds_are_stable(cx: &mut TestAppContext) {
    initialize_visual_test(cx);
    let (shell, cx) = add_visual_shell_with_preferences(
        cx,
        DesktopRuntimeBridge::disconnected_for_test(),
        visual_test_projection(),
        visual_preferences_with_inspector(),
    );

    cx.simulate_resize(size(px(1_300.), px(900.)));
    cx.run_until_parked();
    let wide_before_focus = desktop_region_bounds(cx);
    assert!(wide_before_focus.iter().all(Option::is_some));
    assert_eq!(
        shell.read_with(cx, |shell, _| shell.ui.focus.active()),
        FocusTarget::Composer
    );
    cx.dispatch_action(FocusNextRegion);
    cx.run_until_parked();
    assert_eq!(
        shell.read_with(cx, |shell, _| shell.ui.focus.active()),
        FocusTarget::Inspector
    );
    cx.dispatch_action(FocusNextRegion);
    cx.run_until_parked();
    assert_eq!(
        shell.read_with(cx, |shell, _| shell.ui.focus.active()),
        FocusTarget::CenterHeader
    );
    assert_eq!(desktop_region_bounds(cx), wide_before_focus);

    cx.simulate_resize(size(px(1_000.), px(900.)));
    cx.run_until_parked();
    let medium = desktop_region_bounds(cx);
    assert!(medium[0].is_some());
    assert!(medium[1].is_some());
    assert!(medium[2].is_some());
    assert!(medium[3].is_none());
    assert_eq!(f32::from(medium[0].unwrap().size.width), 240.);
    assert_eq!(f32::from(medium[1].unwrap().size.width), 760.);
    assert_eq!(f32::from(medium[2].unwrap().size.width), 760.);

    cx.simulate_resize(size(px(700.), px(900.)));
    cx.run_until_parked();
    let narrow = desktop_region_bounds(cx);
    assert!(narrow[0].is_none());
    assert!(narrow[1].is_some());
    assert!(narrow[2].is_some());
    assert!(narrow[3].is_none());
    assert_eq!(f32::from(narrow[1].unwrap().size.width), 700.);
    assert_eq!(f32::from(narrow[2].unwrap().size.width), 700.);

    for (window_width, expected_workspace_width) in
        [(1_080., 520.), (1_079., 839.), (760., 520.), (759., 759.)]
    {
        cx.simulate_resize(size(px(window_width), px(900.)));
        cx.run_until_parked();
        let actual = cx
            .debug_bounds("desktop-conversation-panel")
            .expect("workspace remains visible at every responsive breakpoint");
        assert_eq!(f32::from(actual.size.width), expected_workspace_width);
    }

    let medium_layout = ShellLayout::resolve(1_000, 900, PanelVisibility::default());
    assert!(medium_layout.sidebar.is_some());
    assert!(medium_layout.inspector.is_none());
    let narrow_layout = ShellLayout::resolve(700, 900, PanelVisibility::default());
    assert!(narrow_layout.sidebar.is_none());
    assert!(narrow_layout.inspector.is_none());
}

#[gpui::test]
fn shell_header_and_toast_host_stay_bounded_at_all_viewports(cx: &mut TestAppContext) {
    initialize_visual_test(cx);
    let (_, cx) = add_visual_shell(
        cx,
        DesktopRuntimeBridge::disconnected_for_test(),
        visual_test_projection(),
    );

    for width in [1_300., 1_000., 700.] {
        cx.simulate_resize(size(px(width), px(900.)));
        cx.run_until_parked();

        let header = cx
            .debug_bounds("desktop-conversation-header")
            .expect("conversation header remains visible");
        let identity = cx
            .debug_bounds("desktop-header-identity")
            .expect("header identity region remains visible");
        let title = cx.debug_bounds("desktop-header-session-title");
        let actions = cx
            .debug_bounds("desktop-header-actions")
            .expect("header actions remain visible");
        let runtime_slot = cx
            .debug_bounds("desktop-header-runtime-status-slot")
            .expect("the attention-only status slot remains reserved");
        assert!(identity.right() <= actions.left());
        if let Some(title) = &title {
            assert!(title.left() >= identity.left() && title.right() <= identity.right());
        }
        if width == 700. {
            assert!(
                title.is_none(),
                "narrow chrome prioritizes its action cluster"
            );
        }
        assert!(runtime_slot.left() >= actions.left() && runtime_slot.right() <= actions.right());
        assert_eq!(
            f32::from(runtime_slot.size.width),
            header_runtime_status_slot_width(width as u32)
        );
        assert!(
            cx.debug_bounds("desktop-header-runtime-status").is_none(),
            "idle does not render a status indicator"
        );
        assert!(
            actions.right() <= header.right(),
            "Header actions must stay bounded at {width}px: header={header:?}, actions={actions:?}"
        );

        assert!(cx.debug_bounds("desktop-status-panel").is_none());
        assert!(cx.debug_bounds("desktop-status-primary").is_none());
        assert!(cx.debug_bounds("desktop-status-secondary").is_none());

        let composer = cx
            .debug_bounds("desktop-composer-panel")
            .expect("Composer remains visible");
        assert_eq!(composer.bottom(), px(900.));
        let toast_host = cx
            .debug_bounds("desktop-toast-host")
            .expect("the transient notice host remains mounted");
        assert!(toast_host.left() >= px(0.));
        assert!(toast_host.right() <= px(width));
        assert!(toast_host.bottom() <= px(900.));
        assert!(
            cx.debug_bounds("desktop-composer-model-selector").is_some(),
            "the model selector remains available in the Composer"
        );
        assert!(
            cx.debug_bounds("desktop-composer-thinking-selector")
                .is_some()
        );
        assert!(
            cx.debug_bounds("desktop-composer-profile-selector")
                .is_some()
        );
    }
}

#[gpui::test]
fn idle_and_running_header_status_keep_every_other_control_stationary(cx: &mut TestAppContext) {
    initialize_visual_test(cx);
    let (shell, cx) = add_visual_shell(
        cx,
        DesktopRuntimeBridge::disconnected_for_test(),
        visual_test_projection(),
    );
    let stable_selectors = [
        "desktop-header-identity",
        "desktop-header-actions",
        "desktop-hit-toggle-inspector",
        "desktop-header-overflow",
    ];

    for width in [1_300., 1_000., 700.] {
        shell.update(cx, |shell, cx| {
            let mut view_model = conversation_header::view_model(&shell.app, &shell.ui);
            view_model.status = SemanticStatus::Idle;
            shell.views.conversation_header.update(cx, |header, cx| {
                header.set_view_model(view_model);
                cx.notify();
            });
        });
        cx.simulate_resize(size(px(width), px(900.)));
        cx.run_until_parked();
        assert!(cx.debug_bounds("desktop-header-runtime-status").is_none());
        let idle_slot = cx
            .debug_bounds("desktop-header-runtime-status-slot")
            .expect("idle keeps the horizontal status reservation");
        let idle_bounds = stable_selectors.map(|selector| {
            cx.debug_bounds(selector)
                .unwrap_or_else(|| panic!("idle header is missing {selector} at {width}px"))
        });

        for status in [
            SemanticStatus::Running,
            SemanticStatus::Authorization,
            SemanticStatus::Warning,
            SemanticStatus::Error,
        ] {
            shell.update(cx, |shell, cx| {
                let mut view_model = conversation_header::view_model(&shell.app, &shell.ui);
                view_model.status = status;
                // Isolate the status transition from the independently
                // conditional Abort action so this regression measures only
                // the attention indicator's geometry contract.
                view_model.composer_running = false;
                shell.views.conversation_header.update(cx, |header, cx| {
                    header.set_view_model(view_model);
                    cx.notify();
                });
            });
            cx.run_until_parked();
            let indicator = cx
                .debug_bounds("desktop-header-runtime-status")
                .unwrap_or_else(|| panic!("{status:?} renders the attention indicator"));
            let slot = cx
                .debug_bounds("desktop-header-runtime-status-slot")
                .expect("attention states keep the reserved status slot");
            assert!(
                indicator.left() >= slot.left() && indicator.right() <= slot.right(),
                "{status:?} indicator must fit its slot: indicator={indicator:?}, slot={slot:?}"
            );
            assert_eq!(idle_slot.left(), slot.left());
            assert_eq!(idle_slot.size.width, slot.size.width);
            let attention_bounds = stable_selectors.map(|selector| {
                cx.debug_bounds(selector).unwrap_or_else(|| {
                    panic!("{status:?} header is missing {selector} at {width}px")
                })
            });
            assert_eq!(
                idle_bounds, attention_bounds,
                "{status:?} appearance must not move any other Header control at {width}px"
            );
        }
    }
}

#[gpui::test]
fn inspector_tabs_stay_on_one_line_in_docked_and_drawer_layouts(cx: &mut TestAppContext) {
    initialize_visual_test(cx);
    let (shell, cx) = add_visual_shell_with_preferences(
        cx,
        DesktopRuntimeBridge::disconnected_for_test(),
        visual_test_projection(),
        visual_preferences_with_inspector(),
    );

    for (width, open_modal) in [(1_300., false), (700., true)] {
        cx.simulate_resize(size(px(width), px(900.)));
        cx.run_until_parked();
        if open_modal {
            cx.update(|window, app| {
                shell.update(app, |shell, app| shell.toggle_context(window, app));
            });
            cx.run_until_parked();
        }

        let tabs = cx
            .debug_bounds("desktop-inspector-tabs")
            .unwrap_or_else(|| {
                panic!(
                    "Inspector tab strip is visible at width {width}; panel={:?}, details={:?}",
                    cx.debug_bounds("desktop-inspector-panel"),
                    cx.debug_bounds("inspector-details")
                )
            });
        let tab_bounds = [
            "desktop-inspector-tab-changes",
            "desktop-inspector-tab-task",
            "desktop-inspector-tab-usage",
            "desktop-inspector-tab-runtime",
        ]
        .map(|selector| {
            cx.debug_bounds(selector)
                .unwrap_or_else(|| panic!("missing Inspector tab {selector}"))
        });
        let first = tab_bounds[0];
        for bounds in tab_bounds {
            assert_eq!(bounds.top(), first.top());
            assert_eq!(bounds.bottom(), first.bottom());
            assert_eq!(f32::from(bounds.size.height), 32.);
        }
        assert!(tab_bounds[0].size.width > tab_bounds[1].size.width);
        assert!(tab_bounds[3].size.width > tab_bounds[2].size.width);
        assert!(tab_bounds[0].left() >= tabs.left());

        shell.update(cx, |shell, cx| {
            shell
                .app
                .workspaces
                .active_mut()
                .presentation
                .inspector_section = InspectorSection::Runtime;
            shell.refresh_views(UiChangeSet::one(UiRegion::Inspector), cx);
        });
        cx.run_until_parked();
        let runtime = cx
            .debug_bounds("desktop-inspector-tab-runtime")
            .expect("selected Runtime tab remains mounted");
        assert!(runtime.left() >= tabs.left() && runtime.right() <= tabs.right());
        assert!(shell.read_with(cx, |shell, cx| {
            shell.views.inspector_pane.read(cx).tab_scroll_offset().x <= px(0.)
        }));

        cx.update(|window, app| {
            shell.update(app, |shell, app| {
                shell.views.inspector_pane.update(app, |pane, app| {
                    pane.focus_tab(InspectorSection::Runtime, window, app)
                });
            });
        });
        let left = gpui::Keystroke::parse("left").expect("left is a valid keystroke");
        assert!(cx.update(|window, app| window.dispatch_keystroke(left, app)));
        cx.run_until_parked();
        assert_eq!(
            shell.read_with(cx, |shell, _| shell
                .app
                .workspaces
                .active()
                .presentation
                .inspector_section),
            InspectorSection::Usage
        );
        let usage = cx
            .debug_bounds("desktop-inspector-tab-usage")
            .expect("keyboard-selected Usage tab remains mounted");
        assert!(usage.left() >= tabs.left() && usage.right() <= tabs.right());
    }
}

#[gpui::test]
fn responsive_drawers_preserve_conversation_geometry_scroll_and_owner_focus(
    cx: &mut TestAppContext,
) {
    initialize_visual_test(cx);
    let (shell, cx) = add_visual_shell(
        cx,
        DesktopRuntimeBridge::disconnected_for_test(),
        projection_with_last_item(CodingAgentSessionTranscriptItem::User {
            text: "Drawer geometry must remain stable.".into(),
            started_at: None,
        }),
    );

    cx.simulate_resize(size(px(1_000.), px(900.)));
    settle_visual_measurements(cx);
    let medium_conversation = cx
        .debug_bounds("desktop-conversation-panel")
        .expect("medium conversation remains visible");
    let medium_row = cx
        .debug_bounds("conversation-last-row")
        .expect("medium conversation row remains mounted");
    let medium_scroll = shell.read_with(cx, |shell, _| {
        shell
            .app
            .workspaces
            .active()
            .presentation
            .conversation_controller
            .scroll
            .offset()
    });

    cx.dispatch_action(ToggleInspectorPanel);
    cx.run_until_parked();
    assert_eq!(
        shell.read_with(cx, |shell, _| shell.ui.active_drawer),
        Some(CenterDrawerKind::Inspector)
    );
    assert_eq!(shell.read_with(cx, |shell, _| shell.ui.active_modal), None);
    assert!(cx.debug_bounds("desktop-inspector-panel").is_some());
    assert_minimum_hit_target(cx, "desktop-hit-close-inspector");
    let center_header = cx
        .debug_bounds("desktop-conversation-header")
        .expect("center header remains mounted above the drawer host");
    let center_body = cx
        .debug_bounds("desktop-center-body")
        .expect("center body owns the drawer host");
    let inspector_drawer = cx
        .debug_bounds("desktop-inspector-drawer")
        .expect("Inspector is rendered by the center-body drawer host");
    assert_eq!(inspector_drawer.top(), center_body.top());
    assert_eq!(inspector_drawer.bottom(), center_body.bottom());
    assert_eq!(inspector_drawer.right(), center_body.right());
    assert!(center_header.bottom() <= inspector_drawer.top());
    assert_eq!(
        cx.debug_bounds("desktop-conversation-panel"),
        Some(medium_conversation)
    );
    assert_eq!(cx.debug_bounds("conversation-last-row"), Some(medium_row));
    assert_eq!(
        shell.read_with(cx, |shell, _| shell
            .app
            .workspaces
            .active()
            .presentation
            .conversation_controller
            .scroll
            .offset()),
        medium_scroll
    );

    let model_selector = cx
        .debug_bounds("desktop-composer-model-selector")
        .expect("the Composer model selector stays exposed while Inspector is open");
    cx.simulate_click(model_selector.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    let down = gpui::Keystroke::parse("down").expect("down is a valid popup keystroke");
    assert!(cx.update(|window, app| window.dispatch_keystroke(down, app)));
    let escape = gpui::Keystroke::parse("escape").expect("escape is a valid popup keystroke");
    assert!(cx.update(|window, app| window.dispatch_keystroke(escape, app)));
    cx.run_until_parked();
    assert_eq!(
        shell.read_with(cx, |shell, _| shell.ui.active_drawer),
        None,
        "Composer interaction dismisses a center-body drawer before opening its selector"
    );

    cx.simulate_click(center_body.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    assert_eq!(shell.read_with(cx, |shell, _| shell.ui.active_drawer), None);
    assert_eq!(
        shell.read_with(cx, |shell, _| shell.ui.focus.active()),
        FocusTarget::Composer
    );

    cx.simulate_resize(size(px(700.), px(900.)));
    settle_visual_measurements(cx);
    let narrow_conversation = cx
        .debug_bounds("desktop-conversation-panel")
        .expect("narrow conversation remains visible");
    let narrow_row = cx
        .debug_bounds("conversation-last-row")
        .expect("narrow conversation row remains mounted");
    let narrow_scroll = shell.read_with(cx, |shell, _| {
        shell
            .app
            .workspaces
            .active()
            .presentation
            .conversation_controller
            .scroll
            .offset()
    });
    let sessions_toggle = cx
        .debug_bounds("desktop-hit-toggle-sessions")
        .expect("narrow layout retains the Sessions drawer toggle");
    cx.simulate_click(sessions_toggle.center(), gpui::Modifiers::default());
    cx.run_until_parked();

    assert_eq!(
        shell.read_with(cx, |shell, _| shell.ui.active_drawer),
        Some(CenterDrawerKind::Sessions)
    );
    assert_eq!(shell.read_with(cx, |shell, _| shell.ui.active_modal), None);
    assert!(cx.debug_bounds("desktop-sessions-drawer").is_some());
    assert_minimum_hit_target(cx, "desktop-hit-refresh-projects");
    assert_minimum_hit_target(cx, "desktop-hit-close-narrow-sessions");
    assert!(
        cx.debug_bounds("desktop-projects-state-not-loaded")
            .is_some(),
        "narrow drawer reuses the Projects-local unloaded state"
    );
    assert!(
        cx.debug_bounds("sessions-search").is_none(),
        "search remains optional until the project catalog has entries"
    );
    assert_eq!(
        cx.debug_bounds("desktop-conversation-panel"),
        Some(narrow_conversation)
    );
    assert_eq!(cx.debug_bounds("conversation-last-row"), Some(narrow_row));
    assert_eq!(
        shell.read_with(cx, |shell, _| shell
            .app
            .workspaces
            .active()
            .presentation
            .conversation_controller
            .scroll
            .offset()),
        narrow_scroll
    );

    let inspector_toggle = cx
        .debug_bounds("desktop-hit-toggle-inspector")
        .expect("the Header keeps the primary Inspector toggle above either drawer");
    cx.simulate_click(inspector_toggle.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    assert_eq!(
        shell.read_with(cx, |shell, _| shell.ui.active_drawer),
        Some(CenterDrawerKind::Inspector)
    );
    assert!(cx.debug_bounds("desktop-sessions-drawer").is_none());
    assert!(cx.debug_bounds("desktop-inspector-drawer").is_some());

    cx.simulate_click(sessions_toggle.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    assert_eq!(
        shell.read_with(cx, |shell, _| shell.ui.active_drawer),
        Some(CenterDrawerKind::Sessions)
    );
    assert!(cx.debug_bounds("desktop-inspector-drawer").is_none());
    assert!(cx.debug_bounds("desktop-sessions-drawer").is_some());

    cx.dispatch_action(EscapeHierarchy);
    cx.run_until_parked();
    assert_eq!(shell.read_with(cx, |shell, _| shell.ui.active_drawer), None);
    assert_eq!(
        shell.read_with(cx, |shell, _| shell.ui.focus.active()),
        FocusTarget::Composer
    );
}

fn assert_profile_selector_locked_with_inspector_drawer(
    cx: &mut TestAppContext,
    viewport_width: f32,
) {
    initialize_visual_test(cx);
    let (runtime, mut runtime_harness) = DesktopRuntimeBridge::instrumented_for_test();
    let (shell, cx) = add_visual_shell(cx, runtime, visual_test_projection());
    cx.run_until_parked();
    runtime_harness.drain_command_kinds();

    cx.simulate_resize(size(px(viewport_width), px(900.)));
    settle_visual_measurements(cx);

    let inspector_toggle = cx
        .debug_bounds("desktop-hit-toggle-inspector")
        .expect("the center Header exposes the primary Inspector toggle");
    cx.simulate_click(inspector_toggle.center(), gpui::Modifiers::default());
    cx.run_until_parked();

    assert_eq!(
        shell.read_with(cx, |shell, _| shell.ui.active_drawer),
        Some(CenterDrawerKind::Inspector)
    );
    let center_header = cx
        .debug_bounds("desktop-conversation-header")
        .expect("the center Header remains mounted above the drawer host");
    let inspector_drawer = cx
        .debug_bounds("desktop-inspector-drawer")
        .expect("Inspector opens as a center-body drawer");
    assert!(center_header.bottom() <= inspector_drawer.top());
    assert!(inspector_toggle.bottom() <= inspector_drawer.top());
    assert_minimum_hit_target(cx, "desktop-hit-close-inspector");

    let profile_selector = cx
        .debug_bounds("desktop-composer-profile-selector")
        .expect("the Composer keeps the Profile selector mounted behind the drawer host");

    let close = cx
        .debug_bounds("desktop-hit-close-inspector")
        .expect("the drawer exposes its auxiliary close control");
    cx.simulate_click(close.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    assert_eq!(shell.read_with(cx, |shell, _| shell.ui.active_drawer), None,);
    assert_eq!(
        shell.read_with(cx, |shell, _| shell.ui.focus.active()),
        FocusTarget::Composer,
        "the auxiliary close control restores the pre-drawer focus owner"
    );

    cx.simulate_click(profile_selector.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    assert!(
        runtime_harness.drain_selections().is_empty(),
        "a session's profile selector is locked and must not submit selections"
    );
}

#[gpui::test]
fn profile_selector_locked_with_inspector_drawer_across_widths(cx: &mut TestAppContext) {
    for width in [1_000., 700.] {
        assert_profile_selector_locked_with_inspector_drawer(cx, width);
    }
}
