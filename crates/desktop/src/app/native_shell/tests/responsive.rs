use super::*;
mod detail;

#[gpui::test]
fn streaming_markdown_growth_uses_natural_height_and_keeps_the_tail_pinned(
    cx: &mut TestAppContext,
) {
    initialize_visual_test(cx);
    let projection_for = |text: String| {
        let mut snapshot = visual_test_snapshot();
        snapshot.transcript.items = vec![CodingAgentSessionTranscriptItem::Assistant {
            id: "streaming-natural-height".into(),
            text,
            thinking: String::new(),
            images: Vec::new(),
            done: false,
            reasoning_duration_millis: None,
            model_id: None,
            completed_at: None,
        }];
        DesktopProjection::new(snapshot).expect("streaming fixture is a valid projection")
    };

    let mut body = "# Streaming answer\n\n".to_owned();
    let (shell, cx) = add_visual_shell(
        cx,
        DesktopRuntimeBridge::disconnected_for_test(),
        projection_for(body.clone()),
    );
    cx.simulate_resize(size(px(700.), px(800.)));

    let mut previous_height = 0.;
    let mut previous_bottom: Option<f32> = None;
    let mut pinned_growth_frames = 0;
    for revision in 1..=6 {
        body.push_str(&format!(
            "## Chunk {revision}\n\n{}\n\n",
            "A streamed Markdown sentence with **stable formatting**. ".repeat(18)
        ));
        let projection = projection_for(body.clone());
        shell.update(cx, |shell, cx| {
            let workspace = shell.app.workspaces.active_mut();
            workspace.projection = Some(projection);
            workspace
                .presentation
                .conversation_controller
                .apply_projection_delta(true, None, revision);
            shell.refresh_conversation_rows_at_width(600, cx);
        });
        settle_visual_measurements(cx);

        let row = cx
            .debug_bounds("conversation-last-row")
            .expect("the streaming row remains mounted");
        let card = cx
            .debug_bounds("conversation-last-card")
            .expect("the streaming Markdown card is laid out");
        let composer = cx
            .debug_bounds("desktop-composer-panel")
            .expect("the Composer remains visible below the transcript");
        let row_height = f32::from(row.size.height);

        assert!(
            (row_height
                - (f32::from(card.size.height) + CONVERSATION_ROW_VERTICAL_PADDING_PX as f32))
                .abs()
                <= 1.,
            "chunk {revision} must use the card's natural height in the same frame: row={row:?}, card={card:?}"
        );
        assert!(
            row_height > previous_height,
            "each parsed chunk must grow the row without an estimate collapse: {previous_height} -> {row_height}"
        );
        assert!(
            row.bottom() <= composer.top() + px(1.),
            "the followed tail must stay above the Composer: row={row:?}, composer={composer:?}"
        );
        if let Some(previous_bottom) = previous_bottom
            && (previous_bottom - f32::from(composer.top())).abs() <= 1.
        {
            assert!(
                (f32::from(row.bottom()) - previous_bottom).abs() <= 1.,
                "once content overflows, tail following must absorb row growth without vertical oscillation: {previous_bottom} -> {} (row={row:?}, composer={composer:?})",
                f32::from(row.bottom())
            );
            pinned_growth_frames += 1;
        }
        previous_height = row_height;
        previous_bottom = Some(f32::from(row.bottom()));
    }
    assert!(
        pinned_growth_frames >= 3,
        "the fixture must exercise several overflowing streaming growth frames"
    );
}

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
fn final_long_transcript_tail_is_inside_its_measured_row(cx: &mut TestAppContext) {
    for item in [
        CodingAgentSessionTranscriptItem::User {
            text: long_integrity_text("User"),
            started_at: None,
        },
        CodingAgentSessionTranscriptItem::Diagnostic {
            message: long_integrity_text("Diagnostic"),
        },
    ] {
        initialize_visual_test(cx);
        let (_, cx) = add_visual_shell(
            cx,
            DesktopRuntimeBridge::disconnected_for_test(),
            projection_with_last_item(item.clone()),
        );
        cx.simulate_resize(size(px(700.), px(800.)));
        settle_visual_measurements(cx);

        let label = match &item {
            CodingAgentSessionTranscriptItem::User { .. } => "User",
            CodingAgentSessionTranscriptItem::Diagnostic { .. } => "Diagnostic",
            _ => unreachable!(),
        };
        assert_last_row_matches_card_and_tail(cx, label);
        assert!(
            f32::from(
                cx.debug_bounds("conversation-last-card")
                    .expect("transcript card remains mounted")
                    .size
                    .height
            ) > TRANSCRIPT_COLLAPSED_PREVIEW_MAX_HEIGHT,
            "long {label} content must not inherit the former silent height cap"
        );
    }
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
fn assistant_segments_and_tool_rows_share_one_copy_action(cx: &mut TestAppContext) {
    let assistant_after_tool = CodingAgentSessionTranscriptItem::Assistant {
        id: "identity-answer".into(),
        text: "This answer is part of the same assistant output.".into(),
        thinking: String::new(),
        images: Vec::new(),
        done: true,
        reasoning_duration_millis: None,
        model_id: None,
        completed_at: None,
    };
    let tool_row = |call_id: &str| CodingAgentSessionTranscriptItem::Tool {
        call_id: call_id.into(),
        name: "shell".into(),
        args: serde_json::json!({ "command": "git status" }),
        result: Some("working tree clean".into()),
        is_error: false,
        duration_millis: Some(20),
    };

    // An Assistant answer after a Tool row continues the identity group:
    // no repeated header, and the group keeps its copy action.
    initialize_visual_test(cx);
    let (_, cx) = add_visual_shell(
        cx,
        DesktopRuntimeBridge::disconnected_for_test(),
        projection_with_items(vec![
            tool_row("identity-tool"),
            assistant_after_tool.clone(),
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

    // An Assistant segment immediately before a Tool row must not paint an
    // in-between copy action.
    initialize_visual_test(cx);
    let (_, cx) = add_visual_shell(
        cx,
        DesktopRuntimeBridge::disconnected_for_test(),
        projection_with_items(vec![assistant_after_tool, tool_row("copy-group-tool")]),
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
                model_id: None,
                completed_at: None,
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
fn width_refresh_remeasures_the_natural_row_without_an_estimate_frame(cx: &mut TestAppContext) {
    initialize_visual_test(cx);
    let (shell, cx) = add_visual_shell(
        cx,
        DesktopRuntimeBridge::disconnected_for_test(),
        clipping_regression_projection(),
    );
    cx.simulate_resize(size(px(1_000.), px(800.)));
    settle_visual_measurements(cx);

    let active_width = cx.update(|_, app| {
        let controller = &shell
            .read(app)
            .app
            .workspaces
            .active()
            .presentation
            .conversation_controller;
        controller.active_width_bucket()
    });
    let active_width = active_width.expect("the transcript has rendered at a width");

    // One bucket narrower. The native dynamic list invalidates the affected
    // items and measures their real elements during this layout pass; there is
    // no controller-owned estimate to paint for an intermediate frame.
    let narrower = conversation_width_bucket(active_width - 1);
    assert_ne!(narrower, active_width);
    shell.update(cx, |shell, cx| {
        shell.refresh_conversation_rows_at_width(narrower, cx);
    });
    settle_visual_measurements(cx);
    let row = cx
        .debug_bounds("conversation-last-row")
        .expect("final virtual row is mounted after the width refresh");
    let card = cx
        .debug_bounds("conversation-last-card")
        .expect("final conversation card is laid out after the width refresh");
    assert!(
        (f32::from(row.size.height)
            - (f32::from(card.size.height) + CONVERSATION_ROW_VERTICAL_PADDING_PX as f32))
            .abs()
            <= 1.,
        "the resized native-list row must match its card in the same settled frame: row={row:?}, card={card:?}"
    );
}

#[gpui::test]
fn assistant_reasoning_expansion_keeps_the_followed_tail_above_the_composer(
    cx: &mut TestAppContext,
) {
    initialize_visual_test(cx);
    let (_, cx) = add_visual_shell(
        cx,
        DesktopRuntimeBridge::disconnected_for_test(),
        projection_with_last_item(CodingAgentSessionTranscriptItem::Assistant {
            id: "reasoning-layout".into(),
            text: "Final answer tail remains visible.".into(),
            thinking: long_integrity_text("Reasoning"),
            images: Vec::new(),
            done: true,
            reasoning_duration_millis: Some(2_430),
            model_id: None,
            completed_at: None,
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
    assert_last_row_matches_card_and_tail(cx, "expanded Reasoning");
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
    assert!(
        card.top() < collapsed_top,
        "once expansion overflows the viewport, the followed row must grow upward"
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
            model_id: None,
            completed_at: None,
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
            model_id: None,
            completed_at: None,
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
