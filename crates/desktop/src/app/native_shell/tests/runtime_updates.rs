use super::*;

#[test]
fn composer_draft_is_scoped_to_the_active_session() {
    let projection = visual_test_projection();
    let project = projection.project().clone();
    let mut session_a = make_session_workspace(project.clone(), Some(projection), None);
    let mut session_b = make_session_workspace(project, None, None);
    session_a.composer.edit("draft a");
    session_b.composer.edit("draft b");

    assert_eq!(session_a.composer.draft(), "draft a");
    assert_eq!(session_b.composer.draft(), "draft b");
}

#[test]
fn inspector_section_selection_is_scoped_to_the_session() {
    let projection = visual_test_projection();
    let project = projection.project().clone();
    let mut session_a = make_session_workspace(project.clone(), Some(projection), None);
    let mut session_b = make_session_workspace(project, None, None);
    session_a.presentation.inspector_section = InspectorSection::Runtime;
    session_b.presentation.inspector_section = InspectorSection::Task;

    assert_eq!(
        session_a.presentation.inspector_section,
        InspectorSection::Runtime
    );
    assert_eq!(
        session_b.presentation.inspector_section,
        InspectorSection::Task
    );
}

#[test]
fn interleaved_live_rows_keep_event_order_instead_of_sinking_tools_to_the_tail() {
    // One provider response alternates assistant content and a server-side
    // tool: message segment A1 → T1 → message segment A2.
    // Both fold onto independent queues, so rendering one queue after the
    // other dropped every running tool below the newest message, and shifted
    // it down again on each new message. The tool cards visibly jumped to the
    // bottom mid-turn and only snapped back when the durable transcript
    // replaced the live tail at the end of the operation.
    let live_event = |sequence: u64, payload: serde_json::Value| {
        serde_json::from_value::<coding_agent::api::event::CodingAgentProductEvent>(
            serde_json::json!({
                "stream_id": "desktop-visual-test-stream",
                "sequence": sequence,
                "event": payload,
                "operation_id": "operation-1",
                "session_id": "desktop-visual-test",
                "terminal_status": null,
                "terminal_operation": null,
                "durability": {"state": "live_only"},
                "delivery_class": "data",
            }),
        )
        .expect("the live overlay fixture must deserialize")
    };
    let message_started = |sequence: u64, turn_id: &str, message_id: &str| {
        live_event(
            sequence,
            serde_json::json!({
                "family": "message",
                "payload": {
                    "kind": "started",
                    "operation_id": "operation-1",
                    "turn_id": turn_id,
                    "message_id": message_id,
                },
            }),
        )
    };
    let tool_started = |sequence: u64, tool_call_id: &str| {
        live_event(
            sequence,
            serde_json::json!({
                "family": "tool",
                "payload": {
                    "kind": "started",
                    "operation_id": "operation-1",
                    "turn_id": "turn-1",
                    "tool_call_id": tool_call_id,
                    "name": "read",
                    "arguments_json": "{}",
                },
            }),
        )
    };

    let mut projection = visual_test_projection();
    for event in [
        message_started(1, "turn-1", "message-1"),
        tool_started(2, "tool-1"),
        message_started(3, "turn-1", "message-1:segment:1"),
    ] {
        assert!(
            projection
                .apply(ProjectionEvent::Product(event))
                .is_applied()
        );
    }

    let mut controller = ConversationController::default();
    let source = ConversationSource::new(&projection, None);
    controller.apply_projection_delta(true, None, 3);
    controller.prepare_rows(&source, 900);
    let row_ids = |controller: &ConversationController| {
        (0..controller.row_count())
            .map(|index| {
                controller
                    .row_at(index)
                    .expect("every counted row is resolvable")
                    .item_key
                    .row_id()
                    .to_owned()
            })
            .collect::<Vec<_>>()
    };
    let expected = [
        "assistant:message-1".to_owned(),
        "tool:tool-1".to_owned(),
        "assistant:message-1:segment:1".to_owned(),
    ];
    assert_eq!(row_ids(&controller), expected);

    // The incremental sequence path resolves the same order as the rebuild,
    // so a streaming delta cannot reshuffle the tail behind the rebuild's back.
    let mut streamed = projection.clone();
    assert!(
        streamed
            .apply(ProjectionEvent::Product(live_event(
                4,
                serde_json::json!({
                    "family": "message",
                    "payload": {
                        "kind": "delta",
                        "operation_id": "operation-1",
                        "turn_id": "turn-1",
                        "message_id": "message-1:segment:1",
                        "text": "streaming",
                    },
                }),
            )))
            .is_applied()
    );
    let streamed_source = ConversationSource::new(&streamed, None);
    controller.apply_projection_delta(
        false,
        Some(&desktop::projection::DesktopProjectionDelta {
            conversation: true,
            ..Default::default()
        }),
        4,
    );
    controller.prepare_rows(&streamed_source, 900);
    assert_eq!(row_ids(&controller), expected);
}

#[test]
fn a_metadata_reload_mid_turn_keeps_the_streaming_rows_mounted() {
    // A metadata reload carries no transcript, so wiping the live tail here
    // unmounted the streaming assistant and running tool rows with nothing to
    // take their place until the operation finished and rehydrated.
    let mut projection = visual_test_projection();
    let started = serde_json::from_value::<coding_agent::api::event::CodingAgentProductEvent>(
        serde_json::json!({
            "stream_id": "desktop-visual-test-stream",
            "sequence": 1,
            "event": {
                "family": "tool",
                "payload": {
                    "kind": "started",
                    "operation_id": "operation-1",
                    "turn_id": "turn-1",
                    "tool_call_id": "tool-1",
                    "name": "read",
                    "arguments_json": "{}",
                },
            },
            "operation_id": "operation-1",
            "session_id": "desktop-visual-test",
            "terminal_status": null,
            "terminal_operation": null,
            "durability": {"state": "live_only"},
            "delivery_class": "data",
        }),
    )
    .expect("the live overlay fixture must deserialize");
    assert!(
        projection
            .apply(ProjectionEvent::Product(started))
            .is_applied()
    );
    assert_eq!(projection.tools().len(), 1);

    let fixture = visual_test_snapshot();
    let mut session = projection.snapshot().clone();
    session.cursor = projection.cursor().clone();
    assert!(
        projection
            .apply(ProjectionEvent::Metadata(
                desktop::runtime::DesktopRuntimeMetadataSnapshot {
                    project: fixture.project,
                    session: Some(session),
                },
            ))
            .is_replaced()
    );

    assert_eq!(
        projection.tools().len(),
        1,
        "a transcript-less reload must not unmount the running tool row"
    );
    let source = ConversationSource::new(&projection, None);
    let mut controller = ConversationController::default();
    controller.apply_projection_delta(true, None, 1);
    controller.prepare_rows(&source, 900);
    assert_eq!(
        controller
            .row_at(0)
            .expect("the running tool row stays mounted")
            .item_key
            .row_id(),
        "tool:tool-1"
    );
}

/// The two properties the streaming rows need from the append path.
///
/// A synchronous first parse gives the first layout real geometry; upstream
/// keeps `set_text` synchronous precisely because an async first parse would
/// leave `parsed_content` empty and let a `measure_all` list latch a ~0
/// height. Appends must then (1) retain the previous parse until the new one
/// lands, so a frame arriving mid-parse measures stale-but-valid geometry
/// rather than nothing, and (2) actually accumulate onto what came before.
///
/// Property (2) only holds with `patches/gpui-component/0001-*.patch`
/// applied: upstream's `increment_update` returns early on its synchronous
/// branch and never seeds the background accumulator, so the first
/// `push_str` appends to nothing and *replaces* the document. This drives
/// the real `on_prepaint` hook the rows measure with, drawing frames without
/// draining the background parse.
#[gpui::test]
fn async_markdown_append_retains_and_accumulates(cx: &mut TestAppContext) {
    struct ProbeRoot;
    impl Render for ProbeRoot {
        fn render(
            &mut self,
            _: &mut gpui::Window,
            _: &mut gpui::Context<Self>,
        ) -> impl IntoElement {
            div()
        }
    }

    use gpui_component::{ElementExt as _, text::TextView};

    initialize_visual_test(cx);
    let (_, visual_cx) = cx.add_window_view(|_, _| ProbeRoot);
    visual_cx.run_until_parked();

    let body = "paragraph **bold** `code`\n\n".repeat(20);
    let state = visual_cx.update(|_, cx| cx.new(|cx| TextViewState::markdown(&body, cx)));

    fn measure(
        visual_cx: &mut gpui::VisualTestContext,
        state: &gpui::Entity<TextViewState>,
    ) -> f32 {
        let observed = Rc::new(RefCell::new(0.0f32));
        let sink = Rc::clone(&observed);
        let state = state.clone();
        visual_cx.draw(
            gpui::point(px(0.), px(0.)),
            size(px(900.), px(4_000.)),
            move |_, _| {
                div().w(px(900.)).child(
                    div()
                        .w_full()
                        .on_prepaint(move |bounds, _, _| {
                            *sink.borrow_mut() = f32::from(bounds.size.height);
                        })
                        .child(TextView::new(&state)),
                )
            },
        );
        *observed.borrow()
    }

    // The constructor's full-replace parse is synchronous, so the very first
    // frame already has real geometry.
    let initial = measure(visual_cx, &state);
    assert!(
        initial > 200.,
        "a synchronous first parse must produce real geometry: {initial}"
    );

    // A streaming delta. The parse is queued to the background and has not
    // been drained, so this frame is exactly the one upstream warns about.
    let appended = "paragraph **bold** `code`\n\n".repeat(5);
    visual_cx.update(|_, cx| {
        state.update(cx, |state, cx| state.push_str(&appended, cx));
    });
    let mid_parse = measure(visual_cx, &state);

    visual_cx.run_until_parked();
    let settled = measure(visual_cx, &state);

    println!("desktop_probe\tinitial={initial}\tmid_parse={mid_parse}\tsettled={settled}");
    assert_eq!(
        mid_parse, initial,
        "an appended row keeps its previous parse until the new one lands, so a \
             frame arriving mid-parse measures stale-but-valid geometry"
    );
    // Accumulation: the appended chunk extends the seeded document instead
    // of replacing it. Without the seed patch this collapses to just the
    // appended chunk, silently dropping everything streamed before it.
    let appended_height = settled - initial;
    assert!(
        appended_height > 0.,
        "the append must extend the document, not replace it: \
             {initial} -> {settled} (a drop here means the vendored patch is missing)"
    );
    assert!(
        (appended_height - initial / 4.).abs() < initial / 8.,
        "appending a quarter of the body should grow the row by about a \
             quarter: {initial} -> {settled}"
    );
}

#[test]
fn gpui_accessibility_metadata_writes_real_accesskit_nodes() {
    let element = div()
        .id("accessibility-contract-probe")
        .role(Role::ListItem)
        .aria_label("Assistant message, streaming")
        .aria_description("Conversation item")
        .aria_selected(true)
        .aria_position_in_set(2)
        .aria_size_of_set(4);

    assert_eq!(gpui::Element::a11y_role(&element), Some(Role::ListItem));
    let mut node = gpui::accesskit::Node::new(Role::ListItem);
    gpui::Element::write_a11y_info(&element, &mut node);
    assert_eq!(node.label(), Some("Assistant message, streaming"));
    assert_eq!(node.description(), Some("Conversation item"));
    assert_eq!(node.is_selected(), Some(true));
    assert_eq!(node.position_in_set(), Some(2));
    assert_eq!(node.size_of_set(), Some(4));
}

#[test]
fn conversation_kinds_have_distinct_leading_markers() {
    let theme = SemanticTheme::GEEK_DARK;
    let user = conversation_block_visual(ConversationBlockKind::User, false, theme);
    let assistant = conversation_block_visual(ConversationBlockKind::Assistant, false, theme);
    let tool = conversation_block_visual(ConversationBlockKind::Tool, false, theme);
    let failed_tool = conversation_block_visual(ConversationBlockKind::Tool, true, theme);
    let delegation = conversation_block_visual(ConversationBlockKind::Delegation, false, theme);
    let diagnostic = conversation_block_visual(ConversationBlockKind::Diagnostic, true, theme);

    assert!(user.align_right);
    assert_eq!(user.glyph, "");
    assert!(!assistant.align_right);
    assert_ne!(tool.accent, failed_tool.accent);
    assert_eq!(tool.accent, theme.muted_text);
    assert_eq!(failed_tool.accent, theme.danger);
    assert_eq!(diagnostic.accent, theme.danger);
    assert_ne!(assistant.glyph, tool.glyph);
    assert_ne!(tool.glyph, diagnostic.glyph);
    assert_eq!(delegation.accent, theme.accent);
}

#[test]
fn delegation_status_colors_follow_the_semantic_vocabulary() {
    let theme = SemanticTheme::GEEK_DARK;
    let color = |status| delegation_status_color(status, theme);
    assert_eq!(color(DelegationStatus::Running), rgb(theme.accent.value()));
    assert_eq!(color(DelegationStatus::Failed), rgb(theme.danger.value()));
    assert_eq!(
        color(DelegationStatus::Rejected),
        rgb(theme.warning.value())
    );
    assert_eq!(
        color(DelegationStatus::Cancelled),
        rgb(theme.warning.value())
    );
    assert_eq!(
        color(DelegationStatus::ConfirmationRequired),
        rgb(theme.warning.value())
    );
    assert_eq!(
        color(DelegationStatus::Completed),
        rgb(theme.muted_text.value())
    );
    assert_eq!(
        color(DelegationStatus::Requested),
        rgb(theme.muted_text.value())
    );
    assert_eq!(
        color(DelegationStatus::Unknown),
        rgb(theme.muted_text.value())
    );
}

#[gpui::test]
fn conversation_selection_rail_preserves_card_geometry(cx: &mut TestAppContext) {
    initialize_visual_test(cx);
    let (shell, cx) = add_visual_shell(
        cx,
        DesktopRuntimeBridge::disconnected_for_test(),
        projection_with_last_item(CodingAgentSessionTranscriptItem::User {
            text: "Selection rail must preserve this row.".into(),
            started_at: None,
        }),
    );
    cx.simulate_resize(size(px(1_300.), px(900.)));
    settle_visual_measurements(cx);
    let card_before = cx
        .debug_bounds("conversation-last-card")
        .expect("the final conversation card is visible");
    shell.update(cx, |shell, cx| shell.select_adjacent_conversation(true, cx));
    settle_visual_measurements(cx);
    let rail = cx
        .debug_bounds("conversation-selected-rail")
        .expect("keyboard selection paints a dedicated rail");
    assert_eq!(f32::from(rail.size.width), CONVERSATION_RAIL_WIDTH);
    assert!(f32::from(rail.size.height) > 0.);
    assert_eq!(
        cx.debug_bounds("conversation-last-card"),
        Some(card_before),
        "the selection rail must not participate in card layout"
    );
}

#[gpui::test]
fn conversation_track_centers_without_inspector_and_keeps_ai_copy_at_bottom_left(
    cx: &mut TestAppContext,
) {
    initialize_visual_test(cx);
    let (_, cx) = add_visual_shell_with_preferences(
        cx,
        DesktopRuntimeBridge::disconnected_for_test(),
        projection_with_last_item(CodingAgentSessionTranscriptItem::Assistant {
            id: "centered-track".into(),
            text: "A short answer inside the centered transcript track.".into(),
            thinking: String::new(),
            images: Vec::new(),
            done: true,
            reasoning_duration_millis: None,
            model_id: None,
            completed_at: None,
        }),
        DesktopPreferences {
            sessions_panel_visible: false,
            context_panel_visible: false,
            ..DesktopPreferences::default()
        },
    );
    cx.simulate_resize(size(px(1_600.), px(900.)));
    settle_visual_measurements(cx);

    let panel = cx
        .debug_bounds("desktop-conversation-panel")
        .expect("conversation panel is laid out");
    let track = cx
        .debug_bounds("conversation-last-track")
        .expect("conversation row exposes its centered content track");
    let card = cx
        .debug_bounds("conversation-last-card")
        .expect("Assistant card is laid out");
    let copy = cx
        .debug_bounds("desktop-copy-conversation-row")
        .expect("Assistant copy action is laid out");
    let composer = cx
        .debug_bounds("desktop-composer-panel")
        .expect("Composer is laid out");
    let left_margin = f32::from(track.left() - panel.left());
    let right_margin = f32::from(panel.right() - track.right());

    assert!(
        (left_margin - right_margin).abs() <= 1.,
        "hidden Inspector must leave equal transcript margins: panel={panel:?}, track={track:?}"
    );
    assert!(
        (f32::from(track.size.width) - CONVERSATION_CONTENT_MAX_WIDTH as f32).abs() <= 1.,
        "wide viewports must cap the centered transcript track"
    );
    assert_eq!(
        composer.left(),
        track.left(),
        "Composer and transcript share the same centered left edge"
    );
    assert_eq!(
        composer.right(),
        track.right(),
        "Composer and transcript share the same centered right edge"
    );
    assert!(
        (f32::from(card.size.width) - desktop::ui::shell::ASSISTANT_MESSAGE_MAX_WIDTH as f32).abs()
            <= 1.,
        "Assistant content fills the bounded track interior"
    );
    assert!(
        f32::from(copy.left() - card.left()) <= 17. && copy.top() > card.top() + px(32.),
        "Assistant copy action belongs at the card's bottom-left: card={card:?}, copy={copy:?}"
    );
}

#[gpui::test]
fn short_user_message_wraps_content_and_keeps_copy_outside_bottom_right(cx: &mut TestAppContext) {
    initialize_visual_test(cx);
    let (_, cx) = add_visual_shell_with_preferences(
        cx,
        DesktopRuntimeBridge::disconnected_for_test(),
        projection_with_last_item(CodingAgentSessionTranscriptItem::User {
            text: "Short prompt".into(),
            started_at: None,
        }),
        DesktopPreferences {
            sessions_panel_visible: false,
            context_panel_visible: false,
            ..DesktopPreferences::default()
        },
    );
    cx.simulate_resize(size(px(1_600.), px(900.)));
    settle_visual_measurements(cx);

    let track = cx
        .debug_bounds("conversation-last-track")
        .expect("User row exposes its centered content track");
    let card = cx
        .debug_bounds("conversation-last-card")
        .expect("User card is laid out");
    let copy = cx
        .debug_bounds("desktop-copy-conversation-row")
        .expect("User copy action is laid out");
    let bubble = cx
        .debug_bounds("desktop-user-message-bubble")
        .expect("User message exposes its rounded background independently");

    assert!(
        f32::from(card.size.width) < 320.,
        "short User content should determine the bubble width: card={card:?}"
    );
    assert!(
        (f32::from(track.right() - card.right())
            - desktop::ui::shell::DESKTOP_DESIGN_TOKENS.spacing.lg as f32)
            .abs()
            <= 1.,
        "User bubble remains right-aligned inside the centered track: track={track:?}, card={card:?}"
    );
    assert!(
        (f32::from(card.left() - bubble.left())).abs() <= 1.
            && (f32::from(card.right() - bubble.right())).abs() <= 1.,
        "the rounded background should span the User card independently: card={card:?}, bubble={bubble:?}"
    );
    assert!(
        copy.top() >= bubble.bottom() && f32::from(card.right() - copy.right()) <= 17.,
        "User copy action belongs outside the bubble at bottom-right: card={card:?}, bubble={bubble:?}, copy={copy:?}"
    );
    assert!(
        cx.debug_bounds("desktop-last-conversation-row-header")
            .is_none(),
        "User messages should not render a YOU identity label"
    );
}

#[gpui::test]
fn long_user_message_stops_at_max_width_and_grows_vertically(cx: &mut TestAppContext) {
    initialize_visual_test(cx);
    let (_, cx) = add_visual_shell_with_preferences(
        cx,
        DesktopRuntimeBridge::disconnected_for_test(),
        projection_with_last_item(CodingAgentSessionTranscriptItem::User {
            text: "A long prompt with enough words to wrap naturally. ".repeat(120),
            started_at: None,
        }),
        DesktopPreferences {
            sessions_panel_visible: false,
            context_panel_visible: false,
            ..DesktopPreferences::default()
        },
    );
    cx.simulate_resize(size(px(1_600.), px(900.)));
    settle_visual_measurements(cx);

    let card = cx
        .debug_bounds("conversation-last-card")
        .expect("long User card is laid out");
    assert!(
        (f32::from(card.size.width) - desktop::ui::shell::USER_MESSAGE_MAX_WIDTH as f32).abs()
            <= 1.,
        "long User content must stop at the configured maximum width: card={card:?}"
    );
    assert!(
        f32::from(card.size.height) > 160.,
        "content beyond the maximum width must wrap and grow vertically: card={card:?}"
    );
}
