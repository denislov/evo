use super::*;

#[gpui::test]
fn final_long_markdown_tail_is_inside_measured_row_at_all_viewports(cx: &mut TestAppContext) {
    initialize_visual_test(cx);
    let (shell, cx) = add_visual_shell(
        cx,
        DesktopRuntimeBridge::disconnected_for_test(),
        clipping_regression_projection(),
    );

    for (width, height) in [(1_300., 900.), (1_000., 800.), (700., 800.)] {
        cx.simulate_resize(size(px(width), px(height)));
        cx.executor().advance_clock(Duration::from_millis(100));
        for _ in 0..4 {
            cx.update(|window, _| window.refresh());
            cx.run_until_parked();
        }

        let shell_state = cx.update(|_, app| {
            let shell = shell.read(app);
            (
                shell
                    .app
                    .workspaces
                    .active()
                    .presentation
                    .conversation_controller
                    .row_count(),
                shell
                    .app
                    .workspaces
                    .active()
                    .presentation
                    .conversation_controller
                    .render_heights_for_tests(),
                shell
                    .app
                    .workspaces
                    .active()
                    .presentation
                    .conversation_controller
                    .scroll
                    .offset(),
            )
        });
        let row = cx.debug_bounds("conversation-last-row").unwrap_or_else(|| {
                panic!(
                    "final virtual row is mounted: state={shell_state:?}, card={:?}, tail={:?}, panel={:?}",
                    cx.debug_bounds("conversation-last-card"),
                    cx.debug_bounds("conversation-tail-marker"),
                    cx.debug_bounds("desktop-conversation-panel"),
                )
            });
        let card = cx
            .debug_bounds("conversation-last-card")
            .expect("final conversation card is laid out");
        let tail = cx
            .debug_bounds("conversation-tail-marker")
            .expect("tail layout marker is laid out");
        let composer = cx
            .debug_bounds("desktop-composer-panel")
            .expect("composer remains visible");

        assert!(
            f32::from(card.size.height) > TRANSCRIPT_COLLAPSED_PREVIEW_MAX_HEIGHT,
            "{width}px fixture must exceed the former silent clipping limit"
        );
        assert!(
            (f32::from(row.size.height)
                - (f32::from(card.size.height) + CONVERSATION_ROW_VERTICAL_PADDING_PX as f32))
                .abs()
                <= 1.,
            "{width}px virtual row must match actual card bounds: row={row:?}, card={card:?}"
        );
        assert!(
            tail.bottom() <= row.bottom() + px(1.),
            "{width}px tail marker must remain inside the virtual row"
        );
        assert!(
            tail.bottom() <= composer.top() + px(1.),
            "{width}px final tail must not be hidden below the Composer: tail={tail:?}, composer={composer:?}, row={row:?}"
        );
    }
}

#[gpui::test]
fn final_long_user_tail_is_inside_its_measured_row(cx: &mut TestAppContext) {
    initialize_visual_test(cx);
    let (_, cx) = add_visual_shell(
        cx,
        DesktopRuntimeBridge::disconnected_for_test(),
        projection_with_last_item(CodingAgentSessionTranscriptItem::User {
            text: long_integrity_text("User"),
        }),
    );
    cx.simulate_resize(size(px(700.), px(800.)));
    settle_visual_measurements(cx);

    assert_last_row_matches_card_and_tail(cx, "User");
    assert!(
        f32::from(
            cx.debug_bounds("conversation-last-card")
                .expect("User card remains mounted")
                .size
                .height
        ) > TRANSCRIPT_COLLAPSED_PREVIEW_MAX_HEIGHT,
        "long User content must not inherit the former silent height cap"
    );
}

#[gpui::test]
fn final_long_diagnostic_tail_is_inside_its_measured_row(cx: &mut TestAppContext) {
    initialize_visual_test(cx);
    let (_, cx) = add_visual_shell(
        cx,
        DesktopRuntimeBridge::disconnected_for_test(),
        projection_with_last_item(CodingAgentSessionTranscriptItem::Diagnostic {
            message: long_integrity_text("Diagnostic"),
        }),
    );
    cx.simulate_resize(size(px(700.), px(800.)));
    settle_visual_measurements(cx);

    assert_last_row_matches_card_and_tail(cx, "Diagnostic");
    assert!(
        f32::from(
            cx.debug_bounds("conversation-last-card")
                .expect("Diagnostic card remains mounted")
                .size
                .height
        ) > TRANSCRIPT_COLLAPSED_PREVIEW_MAX_HEIGHT,
        "long Diagnostic content must not inherit the former silent height cap"
    );
}

#[gpui::test]
fn final_long_tool_expands_without_losing_its_tail(cx: &mut TestAppContext) {
    initialize_visual_test(cx);
    let (shell, cx) = add_visual_shell(
        cx,
        DesktopRuntimeBridge::disconnected_for_test(),
        projection_with_last_item(CodingAgentSessionTranscriptItem::Tool {
            call_id: "long-tool-output".into(),
            name: "shell".into(),
            args: serde_json::json!({
                "command": "cargo test --workspace",
                "notes": "参数 中文 🙂".repeat(80),
            }),
            result: Some(long_integrity_text("Tool output")),
            is_error: false,
            duration_millis: Some(1_240),
        }),
    );
    cx.simulate_resize(size(px(700.), px(800.)));
    settle_visual_measurements(cx);
    assert_last_row_matches_card_and_tail(cx, "collapsed Tool");
    let collapsed_height = f32::from(
        cx.debug_bounds("conversation-last-card")
            .expect("collapsed Tool card is laid out")
            .size
            .height,
    );

    let block_id = shell.read_with(cx, |shell, _| {
        shell
            .app
            .workspaces
            .active()
            .presentation
            .conversation_controller
            .last_row_id_for_tests()
            .expect("Tool row exists")
    });
    assert_minimum_hit_target(cx, "desktop-toggle-tool-details");
    let tool_header = cx
        .debug_bounds("desktop-tool-toggle-header")
        .expect("the complete tool header is a disclosure action");
    cx.simulate_click(tool_header.center(), gpui::Modifiers::default());
    settle_visual_measurements(cx);

    assert_eq!(
        shell.read_with(cx, |shell, _| {
            shell
                .app
                .workspaces
                .active()
                .presentation
                .conversation_controller
                .selected_block_id()
                .map(str::to_owned)
        }),
        Some(block_id.clone()),
        "clicking the tool disclosure preserves the typed row-selection path"
    );
    assert_last_row_matches_card_and_tail(cx, "expanded Tool");
    let expanded_height = f32::from(
        cx.debug_bounds("conversation-last-card")
            .expect("expanded Tool card is laid out")
            .size
            .height,
    );
    assert!(
        expanded_height > collapsed_height + 100.,
        "expanded Tool output must contribute its real content height: collapsed={collapsed_height}, expanded={expanded_height}"
    );
    let output_region = cx
        .debug_bounds("desktop-tool-output-region")
        .expect("expanded Tool output uses its dedicated region");
    assert!(
        f32::from(output_region.size.height) <= 402.,
        "expanded Tool output must stay height-bounded and scroll internally: region={output_region:?}"
    );
}

#[gpui::test]
fn expanded_shell_tool_copies_the_displayed_command_and_output(cx: &mut TestAppContext) {
    initialize_visual_test(cx);
    let command = "cargo test -p desktop";
    let output = "desktop tests passed\n";
    let (shell, cx) = add_visual_shell(
        cx,
        DesktopRuntimeBridge::disconnected_for_test(),
        projection_with_last_item(CodingAgentSessionTranscriptItem::Tool {
            call_id: "tool-actions".into(),
            name: "bash".into(),
            args: serde_json::json!({ "command": command, "timeout": 120 }),
            result: Some(output.into()),
            is_error: false,
            duration_millis: Some(320),
        }),
    );
    let block_id = shell.read_with(cx, |shell, _| {
        shell
            .app
            .workspaces
            .active()
            .presentation
            .conversation_controller
            .last_row_id_for_tests()
            .expect("Tool row exists")
    });
    assert_minimum_hit_target(cx, "desktop-toggle-tool-details");
    let disclosure = cx
        .debug_bounds("desktop-toggle-tool-details")
        .expect("tool chevron exposes the typed disclosure path");
    cx.simulate_click(disclosure.center(), gpui::Modifiers::default());
    settle_visual_measurements(cx);
    assert_eq!(
        shell.read_with(cx, |shell, _| {
            shell
                .app
                .workspaces
                .active()
                .presentation
                .conversation_controller
                .selected_block_id()
                .map(str::to_owned)
        }),
        Some(block_id.clone())
    );

    assert!(
        cx.debug_bounds("desktop-tool-output-region").is_some(),
        "expanded Shell output uses the dedicated bordered region"
    );
    assert_minimum_hit_target(cx, "desktop-copy-tool-details");
    let copy_details = cx
        .debug_bounds("desktop-copy-tool-details")
        .expect("the expanded region exposes one hover copy action");
    cx.simulate_click(copy_details.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    assert_eq!(
        cx.read_from_clipboard().and_then(|item| item.text()),
        Some(format!("$ {command}\n{output}"))
    );
}

#[gpui::test]
fn read_tool_remains_a_single_collapsed_summary(cx: &mut TestAppContext) {
    initialize_visual_test(cx);
    let (_, cx) = add_visual_shell(
        cx,
        DesktopRuntimeBridge::disconnected_for_test(),
        projection_with_last_item(CodingAgentSessionTranscriptItem::Tool {
            call_id: "read-summary".into(),
            name: "read".into(),
            args: serde_json::json!({
                "path": "src/main.rs",
                "offset": 40,
                "limit": 80,
            }),
            result: Some("read output remains hidden".into()),
            is_error: false,
            duration_millis: Some(20),
        }),
    );
    settle_visual_measurements(cx);

    assert!(cx.debug_bounds("conversation-last-card").is_some());
    assert!(
        cx.debug_bounds("desktop-toggle-tool-details").is_none(),
        "Read does not expose a disclosure chevron"
    );
    assert!(
        cx.debug_bounds("desktop-tool-toggle-header").is_none(),
        "Read header is not presented as an expandable surface"
    );
    assert!(
        cx.debug_bounds("desktop-tool-output-region").is_none(),
        "Read has no expanded output region"
    );
    assert!(
        cx.debug_bounds("desktop-copy-conversation-row").is_none(),
        "Tool rows do not inherit the generic message copy footer"
    );
}

#[gpui::test]
fn assistant_after_tool_continues_without_repeating_the_identity_header(cx: &mut TestAppContext) {
    initialize_visual_test(cx);
    let (_, cx) = add_visual_shell(
        cx,
        DesktopRuntimeBridge::disconnected_for_test(),
        projection_with_items(vec![
            CodingAgentSessionTranscriptItem::Tool {
                call_id: "identity-tool".into(),
                name: "shell".into(),
                args: serde_json::json!({ "command": "git status" }),
                result: Some("working tree clean".into()),
                is_error: false,
                duration_millis: Some(20),
            },
            CodingAgentSessionTranscriptItem::Assistant {
                id: "identity-answer".into(),
                text: "This answer is part of the same assistant output.".into(),
                thinking: String::new(),
                images: Vec::new(),
                done: true,
                reasoning_duration_millis: None,
            },
        ]),
    );
    settle_visual_measurements(cx);

    assert!(
        cx.debug_bounds("conversation-last-card").is_some(),
        "the final Assistant answer remains rendered"
    );
    assert!(
        cx.debug_bounds("desktop-last-conversation-row-header")
            .is_none(),
        "a Tool row must not restart the Assistant identity group"
    );
    assert!(
        cx.debug_bounds("desktop-copy-conversation-row").is_some(),
        "the final Assistant segment keeps the group's copy action"
    );
}

#[gpui::test]
fn assistant_segment_before_tool_does_not_insert_a_middle_copy_button(cx: &mut TestAppContext) {
    initialize_visual_test(cx);
    let (_, cx) = add_visual_shell(
        cx,
        DesktopRuntimeBridge::disconnected_for_test(),
        projection_with_items(vec![
            CodingAgentSessionTranscriptItem::Assistant {
                id: "pre-tool-answer".into(),
                text: "I will inspect the workspace.".into(),
                thinking: String::new(),
                images: Vec::new(),
                done: true,
                reasoning_duration_millis: None,
            },
            CodingAgentSessionTranscriptItem::Tool {
                call_id: "copy-group-tool".into(),
                name: "shell".into(),
                args: serde_json::json!({ "command": "git status" }),
                result: Some("working tree clean".into()),
                is_error: false,
                duration_millis: Some(20),
            },
        ]),
    );
    settle_visual_measurements(cx);

    assert!(
        cx.debug_bounds("desktop-copy-conversation-row").is_none(),
        "an Assistant segment immediately before Tool must not paint an in-between copy action"
    );
}

#[gpui::test]
fn tool_content_aligns_with_assistant_and_selection_has_no_focus_rail(cx: &mut TestAppContext) {
    initialize_visual_test(cx);
    let (_, cx) = add_visual_shell(
        cx,
        DesktopRuntimeBridge::disconnected_for_test(),
        projection_with_items(vec![
            CodingAgentSessionTranscriptItem::Assistant {
                id: "alignment-answer".into(),
                text: "Assistant content alignment reference.".into(),
                thinking: String::new(),
                images: Vec::new(),
                done: true,
                reasoning_duration_millis: None,
            },
            CodingAgentSessionTranscriptItem::Tool {
                call_id: "alignment-tool".into(),
                name: "shell".into(),
                args: serde_json::json!({ "command": "git status" }),
                result: Some("working tree clean".into()),
                is_error: false,
                duration_millis: Some(20),
            },
        ]),
    );
    cx.simulate_resize(size(px(1_200.), px(900.)));
    settle_visual_measurements(cx);

    let assistant_header = cx
        .debug_bounds("desktop-conversation-row-header")
        .expect("Assistant header is available as the alignment reference");
    let tool_header = cx
        .debug_bounds("desktop-tool-toggle-header")
        .expect("Tool header is laid out");
    assert_eq!(
        (assistant_header.left(), assistant_header.right()),
        (tool_header.left(), tool_header.right()),
        "Tool and Assistant content must share the same horizontal bounds"
    );

    cx.simulate_click(tool_header.center(), gpui::Modifiers::default());
    settle_visual_measurements(cx);
    assert!(
        cx.debug_bounds("conversation-selected-rail").is_none(),
        "selecting a Tool row must not paint the conversation focus rail"
    );
    let output = cx
        .debug_bounds("desktop-tool-output-region")
        .expect("selected Tool still expands normally");
    assert_eq!(
        (output.left(), output.right()),
        (tool_header.left(), tool_header.right()),
        "expanded Tool details must stay aligned with the collapsed summary"
    );
}

#[gpui::test]
fn assistant_reasoning_expands_downward_without_moving_its_top(cx: &mut TestAppContext) {
    initialize_visual_test(cx);
    let (shell, cx) = add_visual_shell(
        cx,
        DesktopRuntimeBridge::disconnected_for_test(),
        projection_with_last_item(CodingAgentSessionTranscriptItem::Assistant {
            id: "reasoning-layout".into(),
            text: "Final answer tail remains visible.".into(),
            thinking: long_integrity_text("Reasoning"),
            images: Vec::new(),
            done: true,
            reasoning_duration_millis: Some(2_430),
        }),
    );
    cx.simulate_resize(size(px(700.), px(800.)));
    settle_visual_measurements(cx);
    assert_last_row_matches_card_and_tail(cx, "collapsed Reasoning");
    let collapsed_height = f32::from(
        cx.debug_bounds("conversation-last-card")
            .expect("collapsed reasoning card is laid out")
            .size
            .height,
    );
    let collapsed_top = cx
        .debug_bounds("conversation-last-card")
        .expect("collapsed reasoning card is laid out")
        .top();

    assert_minimum_hit_target(cx, "desktop-toggle-reasoning-details");
    let reasoning_header = cx
        .debug_bounds("desktop-reasoning-toggle-header")
        .expect("the complete reasoning header is a disclosure action");
    cx.simulate_click(reasoning_header.center(), gpui::Modifiers::default());
    settle_visual_measurements(cx);
    assert!(shell.read_with(cx, |shell, _| {
        shell
            .app
            .workspaces
            .active()
            .presentation
            .conversation_controller
            .toggle_anchor_active_for_tests()
    }));

    let row = cx
        .debug_bounds("conversation-last-row")
        .expect("expanded reasoning row remains mounted");
    let card = cx
        .debug_bounds("conversation-last-card")
        .expect("expanded reasoning card is laid out");
    let tail = cx
        .debug_bounds("conversation-tail-marker")
        .expect("expanded reasoning tail remains laid out");
    let expanded_height = f32::from(card.size.height);
    assert!(
        expanded_height > collapsed_height + 100.,
        "expanded reasoning must contribute its real content height: collapsed={collapsed_height}, expanded={expanded_height}"
    );
    assert_eq!(
        card.top(),
        collapsed_top,
        "expanding details must keep the message top fixed and grow downward"
    );
    assert!(
        (f32::from(row.size.height)
            - (expanded_height + CONVERSATION_ROW_VERTICAL_PADDING_PX as f32))
            .abs()
            <= 1.,
        "the expanded virtual row must match its measured card: row={row:?}, card={card:?}"
    );
    assert!(
        tail.bottom() <= row.bottom() + px(1.),
        "the expanded tail must remain inside its own row even when below the viewport: tail={tail:?}, row={row:?}"
    );
}

#[gpui::test]
fn assistant_reasoning_chevron_toggles_once_without_reflow(cx: &mut TestAppContext) {
    initialize_visual_test(cx);
    let (shell, cx) = add_visual_shell(
        cx,
        DesktopRuntimeBridge::disconnected_for_test(),
        projection_with_last_item(CodingAgentSessionTranscriptItem::Assistant {
            id: "reasoning-chevron".into(),
            text: "The final answer remains below the disclosure.".into(),
            thinking: "A bounded reasoning detail line.\n".repeat(12),
            images: Vec::new(),
            done: true,
            reasoning_duration_millis: Some(640),
        }),
    );
    cx.simulate_resize(size(px(700.), px(800.)));
    settle_visual_measurements(cx);
    let block_id = shell.read_with(cx, |shell, _| {
        shell
            .app
            .workspaces
            .active()
            .presentation
            .conversation_controller
            .last_row_id_for_tests()
            .expect("Assistant row exists")
    });
    let collapsed_height = f32::from(
        cx.debug_bounds("conversation-last-card")
            .expect("collapsed reasoning card is laid out")
            .size
            .height,
    );

    assert_minimum_hit_target(cx, "desktop-toggle-reasoning-details");
    let expand = cx
        .debug_bounds("desktop-toggle-reasoning-details")
        .expect("collapsed reasoning retains its trailing disclosure icon");
    cx.simulate_click(expand.center(), gpui::Modifiers::default());
    settle_visual_measurements(cx);
    assert!(shell.read_with(cx, |shell, _| {
        shell
            .app
            .workspaces
            .active()
            .presentation
            .conversation_controller
            .expanded_details()
            .contains(&block_id)
    }));
    let expanded_height = f32::from(
        cx.debug_bounds("conversation-last-card")
            .expect("expanded reasoning card is laid out")
            .size
            .height,
    );
    assert!(expanded_height > collapsed_height);

    assert_minimum_hit_target(cx, "desktop-toggle-reasoning-details");
    let collapse = cx
        .debug_bounds("desktop-toggle-reasoning-details")
        .expect("expanded reasoning retains its trailing disclosure icon");
    cx.simulate_click(collapse.center(), gpui::Modifiers::default());
    settle_visual_measurements(cx);
    let collapsed_again = f32::from(
        cx.debug_bounds("conversation-last-card")
            .expect("collapsed reasoning card remains laid out")
            .size
            .height,
    );
    assert!(
        (collapsed_again - collapsed_height).abs() <= 1.,
        "the standalone icon must emit exactly one collapse: initial={collapsed_height}, final={collapsed_again}"
    );
    assert!(
        !shell.read_with(cx, |shell, _| {
            shell
                .app
                .workspaces
                .active()
                .presentation
                .conversation_controller
                .expanded_details()
                .contains(&block_id)
        }),
        "the reasoning disclosure returns to its collapsed state"
    );
}

#[gpui::test]
fn conversation_row_copy_selection_is_typed_and_geometry_stable(cx: &mut TestAppContext) {
    initialize_visual_test(cx);
    let message = "Copy the complete bounded conversation row.";
    let (shell, cx) = add_visual_shell(
        cx,
        DesktopRuntimeBridge::disconnected_for_test(),
        projection_with_last_item(CodingAgentSessionTranscriptItem::Assistant {
            id: "row-copy-selection".into(),
            text: message.into(),
            thinking: String::new(),
            images: Vec::new(),
            done: true,
            reasoning_duration_millis: None,
        }),
    );
    cx.simulate_resize(size(px(700.), px(800.)));
    settle_visual_measurements(cx);
    let card_before_selection = cx
        .debug_bounds("conversation-last-card")
        .expect("conversation row card remains mounted");
    assert_minimum_hit_target(cx, "desktop-copy-conversation-row");

    let row_header = cx
        .debug_bounds("desktop-last-conversation-row-header")
        .expect("conversation row header exposes its typed selection path");
    cx.simulate_click(row_header.center(), gpui::Modifiers::default());
    settle_visual_measurements(cx);
    assert_eq!(
        shell.read_with(cx, |shell, _| {
            shell
                .app
                .workspaces
                .active()
                .presentation
                .conversation_controller
                .selected_block_id()
                .map(str::to_owned)
        }),
        Some("assistant:row-copy-selection".into())
    );
    assert_eq!(
        cx.debug_bounds("conversation-last-card"),
        Some(card_before_selection),
        "revealing the selected-row copy icon must not reflow the card"
    );

    assert_minimum_hit_target(cx, "desktop-copy-conversation-row");
    let copy = cx
        .debug_bounds("desktop-copy-conversation-row")
        .expect("selected row exposes its copy icon");
    cx.simulate_click(copy.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    assert_eq!(
        cx.read_from_clipboard().and_then(|item| item.text()),
        Some(message.into())
    );
}

#[gpui::test]
fn truncated_preview_opens_and_copies_the_complete_bounded_message(cx: &mut TestAppContext) {
    initialize_visual_test(cx);
    let mut snapshot = visual_test_snapshot();
    let unit = "完整消息🙂e\u{301}";
    let repeat_count = desktop::ui::conversation::MAX_MARKDOWN_PREVIEW_BYTES / unit.len() + 1;
    let full_text = format!(
        "BEGIN FULL MESSAGE\n{}END FULL MESSAGE",
        unit.repeat(repeat_count)
    );
    assert!(full_text.len() > desktop::ui::conversation::MAX_MARKDOWN_PREVIEW_BYTES);
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
            "desktop-header-model-selector",
            "desktop-header-profile-selector",
            "desktop-header-thinking-selector",
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

    cx.update(|window, app| {
        shell.update(app, |shell, app| {
            shell.views.sessions_pane.update(app, |pane, app| {
                pane.set_search_value("Release", window, app)
            });
        });
    });
    cx.run_until_parked();
    assert!(cx.debug_bounds("desktop-session-row-0").is_some());
    assert!(cx.debug_bounds("desktop-session-row-1").is_none());

    cx.update(|window, app| {
        shell.update(app, |shell, app| {
            shell
                .views
                .sessions_pane
                .update(app, |pane, app| pane.set_search_value("", window, app));
        });
    });
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
fn idle_model_selector_groups_configured_text_models_and_submits_the_exact_id(
    cx: &mut TestAppContext,
) {
    initialize_visual_test(cx);
    let (runtime, mut runtime_harness) = DesktopRuntimeBridge::instrumented_for_test();
    let (shell, cx) = add_idle_visual_shell_with_runtime(cx, runtime);
    cx.run_until_parked();
    runtime_harness.drain_command_kinds();

    let view_model = shell.read_with(cx, |shell, _| {
        conversation_header::view_model(&shell.app, &shell.ui)
    });
    assert!(view_model.idle);
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
        .debug_bounds("desktop-header-model-selector")
        .expect("the model selector is visible");
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
        conversation_header::view_model(&shell.app, &shell.ui)
    });
    assert!(view_model.idle);
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
        .debug_bounds("desktop-header-profile-selector")
        .expect("the idle header exposes the profile selector");
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
fn project_directory_control_is_scoped_locked_pending_and_narrow_safe(cx: &mut TestAppContext) {
    initialize_visual_test(cx);
    let (idle_shell, cx) = add_idle_visual_shell(cx);

    let idle_directory = idle_shell.read_with(cx, |shell, _| {
        composer_pane::view_model(shell.app.workspaces.active()).project_directory
    });
    assert_eq!(idle_directory.value.as_ref(), "无项目");
    assert_eq!(
        idle_directory.state,
        crate::ui::components::controls::DesktopProjectDirectoryState::Editable
    );

    for width in [1_300., 700.] {
        cx.simulate_resize(size(px(width), px(800.)));
        settle_visual_measurements(cx);
        let attachment = cx
            .debug_bounds("desktop-hit-add-composer-attachments")
            .expect("attachment action remains in the Composer bottom-left");
        let project = cx
            .debug_bounds("desktop-project-directory-control")
            .expect("project directory control remains in the Composer bottom-left");
        let submit = cx
            .debug_bounds("desktop-hit-submit-composer")
            .expect("submit action remains in the Composer bottom-right");
        assert!(attachment.right() <= project.left());
        assert!(project.right() <= submit.left());
        assert!(f32::from(project.size.width) <= 280.);
        assert_eq!(f32::from(project.size.height), 36.);
        assert_minimum_hit_target(cx, "desktop-hit-add-composer-attachments");
        assert_minimum_hit_target(cx, "desktop-hit-project-directory");
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
    let session_directory = session_shell.read_with(cx, |shell, _| {
        composer_pane::view_model(shell.app.workspaces.active()).project_directory
    });
    assert_eq!(
        session_directory.value.as_ref(),
        long_path.display().to_string()
    );
    assert_eq!(
        session_directory.state,
        crate::ui::components::controls::DesktopProjectDirectoryState::Locked
    );
    cx.simulate_resize(size(px(700.), px(800.)));
    settle_visual_measurements(cx);
    assert!(
        f32::from(
            cx.debug_bounds("desktop-project-directory-control")
                .expect("locked long-path pill remains visible")
                .size
                .width
        ) <= 280.
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
    assert_eq!(
        pending_shell.read_with(cx, |shell, _| {
            composer_pane::view_model(shell.app.workspaces.active())
                .project_directory
                .state
        }),
        crate::ui::components::controls::DesktopProjectDirectoryState::Pending
    );
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
    let selector = cx
        .debug_bounds("desktop-composer-running-mode-selector")
        .expect("running Composer exposes one mode selector");
    let submit = cx
        .debug_bounds("desktop-hit-submit-running-composer")
        .expect("running Composer exposes one primary submit action");
    assert!(selector.right() <= submit.left());
    assert!(
        (f32::from(selector.bottom() - submit.bottom())).abs() <= 2.1,
        "32 px selector and 36 px submit remain center-aligned: selector={selector:?}, submit={submit:?}"
    );
    assert!(cx.debug_bounds("desktop-hit-submit-composer").is_none());

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
