use super::*;

#[gpui::test]
fn native_shell_markdown_code_action_copies_exact_block(cx: &mut TestAppContext) {
    initialize_visual_test(cx);
    let mut snapshot = visual_test_snapshot();
    snapshot.transcript.items.push(
        coding_agent::api::view::CodingAgentSessionTranscriptItem::Assistant {
            id: "message-with-code".into(),
            text: "Before\n\n```rust\nfn main() { println!(\"exact\"); }\n```\n\nAfter".into(),
            thinking: String::new(),
            images: Vec::new(),
            done: true,
            reasoning_duration_millis: None,
            model_id: None,
            completed_at: None,
        },
    );
    let projection = DesktopProjection::new(snapshot)
        .expect("code-copy visual fixture is a valid product projection");
    let (shell, cx) = add_visual_shell(
        cx,
        DesktopRuntimeBridge::disconnected_for_test(),
        projection,
    );
    cx.run_until_parked();
    cx.refresh()
        .expect("final Markdown renders in the first refreshed frame");
    cx.run_until_parked();
    let notice_before_copy = shell.read_with(cx, |shell, _| {
        shell.app.workspaces.active().preference_notice.clone()
    });

    let bounds = cx
        .debug_bounds("desktop-copy-markdown-code")
        .expect("final Markdown code block exposes a copy action");
    assert_minimum_hit_target(cx, "desktop-copy-markdown-code");
    cx.simulate_click(bounds.center(), gpui::Modifiers::default());
    cx.run_until_parked();

    assert_eq!(
        cx.read_from_clipboard().and_then(|item| item.text()),
        Some("fn main() { println!(\"exact\"); }".into())
    );
    assert!(
        cx.debug_bounds("desktop-conversation-copy-announcement")
            .is_some(),
        "Copy feedback is announced near the conversation instead of occupying the status bar"
    );
    assert_eq!(
        shell.read_with(cx, |shell, _| shell
            .app
            .workspaces
            .active()
            .preference_notice
            .clone()),
        notice_before_copy,
        "Copy feedback must not replace a persistent runtime or preference notice"
    );
    cx.executor()
        .advance_clock(Duration::from_secs(2) + Duration::from_millis(1));
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("desktop-conversation-copy-announcement")
            .is_none(),
        "Copy announcement expires instead of becoming persistent chrome"
    );
}

#[gpui::test]
fn native_shell_command_palette_smoke_uses_modal_focus_and_restores_it(cx: &mut TestAppContext) {
    initialize_visual_test(cx);
    let (runtime, mut runtime_harness) = DesktopRuntimeBridge::instrumented_for_test();
    let (shell, cx) = add_visual_shell(cx, runtime, visual_test_projection());
    cx.run_until_parked();
    runtime_harness.drain_command_kinds();

    cx.dispatch_action(OpenCommandPalette);
    cx.run_until_parked();
    assert_eq!(
        shell.read_with(cx, |shell, _| shell.ui.active_modal),
        Some(DesktopModalKind::CommandPalette)
    );
    assert_eq!(
        shell.read_with(cx, |shell, _| shell.ui.focus.active()),
        FocusTarget::Modal
    );
    cx.dispatch_action(EscapeHierarchy);
    cx.run_until_parked();
    assert_eq!(shell.read_with(cx, |shell, _| shell.ui.active_modal), None);
    assert!(cx.debug_bounds("desktop-authorization-actions").is_none());
    assert_ne!(
        shell.read_with(cx, |shell, _| shell.ui.focus.active()),
        FocusTarget::Modal
    );
}

#[gpui::test]
fn authorization_modal_preempts_the_drawer_and_restores_its_root_focus_owner(
    cx: &mut TestAppContext,
) {
    initialize_visual_test(cx);
    let (shell, cx) = add_visual_shell(
        cx,
        DesktopRuntimeBridge::disconnected_for_test(),
        visual_test_projection(),
    );
    cx.simulate_resize(size(px(1_000.), px(900.)));
    cx.run_until_parked();

    cx.dispatch_action(ToggleInspectorPanel);
    cx.run_until_parked();
    assert_eq!(
        shell.read_with(cx, |shell, _| shell.ui.active_drawer),
        Some(CenterDrawerKind::Inspector)
    );
    assert_eq!(
        shell.read_with(cx, |shell, _| shell.ui.focus.active()),
        FocusTarget::Composer,
        "drawer focus remains independent from the logical root focus owner"
    );

    let mut authorization_snapshot = visual_test_snapshot();
    authorization_snapshot
        .session
        .pending_authorizations
        .push(ToolAuthorizationRequest {
            authorization_id: "authorization-drawer-preemption".into(),
            operation_id: "operation-drawer-preemption".into(),
            turn_id: "turn-drawer-preemption".into(),
            tool_call_id: "tool-drawer-preemption".into(),
            tool_name: "bash".into(),
            risk: ToolAuthorizationRisk::ShellExecution,
            scope: ToolAuthorizationScope::Shell {
                cwd: "/desktop-visual-test".into(),
                command_fingerprint: "drawer-preemption-fingerprint".into(),
            },
            preview: ToolAuthorizationPreview {
                summary: "Authorize after opening the Inspector drawer".into(),
                path: None,
                command: Some("true".into()),
                cwd: Some("/desktop-visual-test".into()),
                content_preview: None,
            },
            capability_generation: 0,
            requested_at: "2026-07-30T00:00:00Z".into(),
        });
    let authorization_projection = DesktopProjection::new(authorization_snapshot)
        .expect("authorization drawer fixture is a valid product projection");
    shell.update(cx, |shell, cx| {
        shell.app.workspaces.active_mut().projection = Some(authorization_projection);
        cx.notify();
    });
    cx.run_until_parked();
    assert_eq!(shell.read_with(cx, |shell, _| shell.ui.active_drawer), None);
    assert_eq!(
        shell.read_with(cx, |shell, _| shell.ui.active_modal),
        Some(DesktopModalKind::Authorization)
    );
    assert_eq!(
        shell.read_with(cx, |shell, _| shell.ui.focus.active()),
        FocusTarget::Modal
    );
    assert!(
        cx.debug_bounds("desktop-authorization-actions").is_some(),
        "the authorization projection mounts the real root modal after closing the drawer"
    );

    shell.update(cx, |shell, cx| {
        shell.app.workspaces.active_mut().projection = Some(visual_test_projection());
        cx.notify();
    });
    cx.run_until_parked();
    assert_eq!(shell.read_with(cx, |shell, _| shell.ui.active_modal), None);
    assert!(cx.debug_bounds("desktop-authorization-actions").is_none());
    assert_eq!(
        shell.read_with(cx, |shell, _| shell.ui.focus.active()),
        FocusTarget::Composer
    );
}

#[gpui::test]
fn native_shell_authorization_smoke_traps_focus_and_submits_a_typed_decision(
    cx: &mut TestAppContext,
) {
    initialize_visual_test(cx);
    let mut snapshot = visual_test_snapshot();
    snapshot
        .session
        .pending_authorizations
        .push(ToolAuthorizationRequest {
            authorization_id: "authorization-visual-test".into(),
            operation_id: "operation-visual-test".into(),
            turn_id: "turn-visual-test".into(),
            tool_call_id: "tool-call-visual-test".into(),
            tool_name: "bash".into(),
            risk: ToolAuthorizationRisk::ShellExecution,
            scope: ToolAuthorizationScope::Shell {
                cwd: "/desktop-visual-test".into(),
                command_fingerprint: "command-fingerprint".into(),
            },
            preview: ToolAuthorizationPreview {
                summary: "Run a visual-test command".into(),
                path: None,
                command: Some("true".into()),
                cwd: Some("/desktop-visual-test".into()),
                content_preview: None,
            },
            capability_generation: 0,
            requested_at: "2026-07-27T00:00:00Z".into(),
        });
    let projection = DesktopProjection::new(snapshot)
        .expect("authorization visual fixture is a valid product projection");
    let (runtime, mut runtime_harness) = DesktopRuntimeBridge::instrumented_for_test();
    let (shell, cx) = add_visual_shell(cx, runtime, projection);
    cx.run_until_parked();
    runtime_harness.drain_command_kinds();

    assert_eq!(
        shell.read_with(cx, |shell, _| shell.ui.active_modal),
        Some(DesktopModalKind::Authorization)
    );
    assert_eq!(
        shell.read_with(cx, |shell, _| shell.ui.focus.active()),
        FocusTarget::Modal
    );
    let term_left = f32::from(
        cx.debug_bounds("desktop-authorization-term-operation")
            .expect("authorization operation term is visible")
            .left(),
    );
    let value_left = f32::from(
        cx.debug_bounds("desktop-authorization-value-operation")
            .expect("authorization operation value is visible")
            .left(),
    );
    for (term, term_selector, value_selector) in [
        (
            "tool",
            "desktop-authorization-term-tool",
            "desktop-authorization-value-tool",
        ),
        (
            "risk",
            "desktop-authorization-term-risk",
            "desktop-authorization-value-risk",
        ),
        (
            "scope",
            "desktop-authorization-term-scope",
            "desktop-authorization-value-scope",
        ),
        (
            "cwd",
            "desktop-authorization-term-cwd",
            "desktop-authorization-value-cwd",
        ),
        (
            "command",
            "desktop-authorization-term-command",
            "desktop-authorization-value-command",
        ),
    ] {
        let term_bounds = cx
            .debug_bounds(term_selector)
            .unwrap_or_else(|| panic!("authorization {term} term is visible"));
        let value_bounds = cx
            .debug_bounds(value_selector)
            .unwrap_or_else(|| panic!("authorization {term} value is visible"));
        assert_eq!(f32::from(term_bounds.left()), term_left);
        assert_eq!(f32::from(value_bounds.left()), value_left);
        assert!(term_bounds.right() <= value_bounds.left());
    }
    for selector in [
        "desktop-hit-deny-authorization",
        "desktop-hit-allow-authorization-once",
        "desktop-hit-allow-authorization-operation",
    ] {
        assert_minimum_hit_target(cx, selector);
        let bounds = cx
            .debug_bounds(selector)
            .unwrap_or_else(|| panic!("missing critical action {selector}"));
        assert_eq!(f32::from(bounds.size.height), 40.);
    }

    cx.dispatch_action(AuthorizationDeny);
    cx.run_until_parked();
    assert!(
        runtime_harness
            .drain_command_kinds()
            .contains(&desktop::runtime::DesktopRuntimeCommandKind::DecideToolAuthorization)
    );
    assert!(shell.read_with(cx, |shell, _| {
        shell.active_command_contains_where(|intent| {
            matches!(
                intent,
                DesktopCommandIntent::Authorization {
                    authorization_id,
                    ..
                } if authorization_id == "authorization-visual-test"
            )
        })
    }));
}

#[gpui::test]
fn native_shell_inspector_smoke_submits_recovery_and_file_review_commands(cx: &mut TestAppContext) {
    initialize_visual_test(cx);
    let recovery = CodingAgentRecoveryPending::from_parts(
        "operation-recovery",
        "recovery-visual-test",
        Some("prompt".into()),
        3,
        2,
        Some(0),
        1,
        Some("2026-07-27T00:00:00Z".into()),
        None,
    );
    let change = CodingAgentFileChangeSnapshot {
        path: "crates/desktop/src/app/native_shell.rs".into(),
        mutation_kind: "edit".into(),
        operation_id: "operation-file-review".into(),
        tool_call_id: Some("tool-call-file-review".into()),
        updated_sequence: 7,
        first_changed_line: Some(1),
        added_lines: Some(2),
        removed_lines: Some(1),
        diff: Some("@@ -1 +1 @@".into()),
    };
    let recovery_identity = DesktopRecoveryIdentity::from(&recovery);
    let review_request = CodingAgentFileReviewRequest::from(&change);
    let mut snapshot = visual_test_snapshot();
    snapshot.pending_recoveries.push(recovery);
    snapshot.session.context.changes.push(change);
    let projection = DesktopProjection::new(snapshot)
        .expect("inspector visual fixture is a valid product projection");
    let (runtime, mut runtime_harness) = DesktopRuntimeBridge::instrumented_for_test();
    let (shell, cx) = add_visual_shell_with_preferences(
        cx,
        runtime,
        projection,
        visual_preferences_with_inspector(),
    );
    cx.run_until_parked();
    runtime_harness.drain_command_kinds();
    let inspector = shell.read_with(cx, |shell, _| shell.views.inspector_pane.clone());

    inspector.update(cx, |_, cx| {
        cx.emit(InspectorPaneEvent::Recovery {
            identity: recovery_identity,
            action: DesktopRecoveryAction::Retry,
        });
    });
    cx.run_until_parked();
    assert!(
        runtime_harness
            .drain_command_kinds()
            .contains(&desktop::runtime::DesktopRuntimeCommandKind::RetryRecovery)
    );
    assert!(shell.read_with(cx, |shell, _| {
        shell.active_command_contains_where(|intent| {
            matches!(
                intent,
                DesktopCommandIntent::Recovery {
                    recovery_id,
                    action: DesktopRecoveryAction::Retry,
                } if recovery_id == "recovery-visual-test"
            )
        })
    }));

    let changed_file = cx
        .debug_bounds("desktop-changed-file-row-0")
        .expect("changed file is a full-row review action");
    assert!(
        f32::from(changed_file.size.height) >= 40.,
        "changed-file action row retains its stable height"
    );
    cx.simulate_click(changed_file.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    assert!(
        runtime_harness
            .drain_command_kinds()
            .contains(&desktop::runtime::DesktopRuntimeCommandKind::ReviewChangedFile)
    );
    assert!(shell.read_with(cx, |shell, _| {
        matches!(
            shell.app.workspaces.active().file_review.as_ref(),
            DesktopFileReviewState::Loading(request) if request == &review_request
        )
    }));

    shell.update(cx, |shell, cx| {
        shell.app.workspaces.active_mut().file_review = Arc::new(DesktopFileReviewState::Ready(
            DesktopFileReviewDocument::from_product(CodingAgentFileReview {
                change: review_request.change.clone(),
                revision: review_request.revision,
                display_path: review_request.change.path.clone(),
                mutation_kind: "edit".into(),
                content: "fn reviewed() {}\n".into(),
                total_bytes: 17,
                line_count: 1,
                content_truncated: false,
                diff: Some("@@ -0,0 +1 @@\n+fn reviewed() {}\n".into()),
                diff_truncated: false,
                first_changed_line: Some(1),
                added_lines: Some(1),
                removed_lines: Some(0),
                external_editor_target: None,
            }),
        ));
        shell.refresh_views(UiChangeSet::one(UiRegion::Inspector), cx);
    });
    cx.run_until_parked();
    for selector in [
        "desktop-hit-copy-review-path",
        "desktop-hit-copy-file-review",
        "desktop-hit-open-external-editor",
    ] {
        assert_minimum_hit_target(cx, selector);
    }
    inspector.update(cx, |_, cx| {
        cx.emit(InspectorPaneEvent::CopyFileReview);
    });
    cx.run_until_parked();
    assert_eq!(
        shell.read_with(cx, |shell, _| shell
            .app
            .workspaces
            .active()
            .preference_notice
            .clone()),
        Some("File review copied.".into())
    );
}

#[gpui::test]
fn diagnostic_row_exposes_authoritative_recovery_action(cx: &mut TestAppContext) {
    initialize_visual_test(cx);
    let recovery = CodingAgentRecoveryPending::from_parts(
        "operation-inline-recovery",
        "recovery-inline-diagnostic",
        Some("prompt".into()),
        4,
        2,
        Some(0),
        1,
        Some("2026-07-27T00:00:00Z".into()),
        None,
    );
    let mut snapshot = visual_test_snapshot();
    snapshot.pending_recoveries.push(recovery);
    snapshot
        .transcript
        .items
        .push(CodingAgentSessionTranscriptItem::Diagnostic {
            message: "The operation requires recovery.".into(),
        });
    let projection = DesktopProjection::new(snapshot)
        .expect("inline recovery fixture is a valid product projection");
    let (runtime, mut runtime_harness) = DesktopRuntimeBridge::instrumented_for_test();
    let (shell, cx) = add_visual_shell(cx, runtime, projection);
    settle_visual_measurements(cx);
    runtime_harness.drain_command_kinds();

    let recovery_actions = [
        "desktop-retry-diagnostic",
        "desktop-mark-failed-diagnostic",
        "desktop-abort-diagnostic",
    ]
    .map(|selector| {
        cx.debug_bounds(selector)
            .unwrap_or_else(|| panic!("Diagnostic exposes {selector} in place"))
    });
    let retry = recovery_actions[0];
    for bounds in recovery_actions {
        assert_eq!(f32::from(bounds.size.height), 40.);
    }
    cx.simulate_click(retry.center(), gpui::Modifiers::default());
    cx.run_until_parked();

    assert!(
        runtime_harness
            .drain_command_kinds()
            .contains(&desktop::runtime::DesktopRuntimeCommandKind::RetryRecovery)
    );
    assert!(shell.read_with(cx, |shell, _| {
        shell.active_command_contains_where(|intent| {
            matches!(
                intent,
                DesktopCommandIntent::Recovery {
                    recovery_id,
                    action: DesktopRecoveryAction::Retry,
                } if recovery_id == "recovery-inline-diagnostic"
            )
        })
    }));
}
