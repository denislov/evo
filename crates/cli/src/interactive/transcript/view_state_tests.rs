use super::*;

fn tool(call_id: &str, is_error: bool) -> TranscriptItem {
    TranscriptItem::Tool {
        call_id: call_id.into(),
        name: "bash".into(),
        args: serde_json::json!({"command": "test"}),
        result: Some("one\ntwo\nthree\nfour".into()),
        is_error,
    }
}

#[test]
fn selection_uses_stable_item_identity_across_streaming_revisions() {
    let mut transcript = Transcript::new();
    transcript.apply_event(UiEvent::ThinkingDelta {
        text: "first".into(),
    });
    let mut view = TranscriptViewState::default();
    view.sync(&transcript);
    let selected = view.selected().unwrap();

    transcript.apply_event(UiEvent::ThinkingDelta {
        text: " second".into(),
    });
    view.sync(&transcript);

    assert_eq!(view.selected(), Some(selected));
    assert_eq!(
        view.snapshot()
            .display_state(selected, transcript.item_for_block(selected).unwrap()),
        TranscriptDisplayState::Preview
    );
}

#[test]
fn image_only_completion_does_not_create_an_empty_assistant_block() {
    let mut transcript = Transcript::new();
    transcript.apply_event(UiEvent::AssistantDone);
    transcript.apply_event(UiEvent::AssistantImages {
        images: vec![coding_agent::api::event::CodingAgentImageContent {
            mime_type: "image/png".into(),
            data: "cG5n".into(),
        }],
    });

    assert_eq!(transcript.items().len(), 1);
    assert!(matches!(
        &transcript.items()[0],
        TranscriptItem::Image { mime_type, data }
            if mime_type == "image/png" && data == "cG5n"
    ));

    let mut view = TranscriptViewState::default();
    view.sync(&transcript);
    assert!(view.selected().is_some());
}

#[test]
fn selection_moves_between_non_system_blocks_and_follows_new_tail() {
    let mut transcript = Transcript::new();
    transcript.push(TranscriptItem::system("notice"));
    transcript.push(TranscriptItem::user("question"));
    transcript.push(tool("call-1", false));
    let mut view = TranscriptViewState::default();
    view.sync(&transcript);
    let tool_id = view.selected().unwrap();

    assert!(view.select_previous(&transcript));
    let user_id = view.selected().unwrap();
    assert_ne!(user_id, tool_id);
    transcript.push(tool("call-2", false));
    view.sync(&transcript);
    assert_eq!(view.selected(), Some(user_id));

    assert!(view.select_next(&transcript));
    assert_eq!(view.selected(), Some(tool_id));
    assert!(view.select_next(&transcript));
    let new_tail = view.selected().unwrap();
    transcript.push(tool("call-3", false));
    view.sync(&transcript);
    assert_ne!(view.selected(), Some(new_tail));
}

#[test]
fn disclosure_cycles_per_block_and_expand_all_returns_to_defaults() {
    let mut transcript = Transcript::new();
    transcript.push(tool("call-1", false));
    transcript.push(tool("call-2", true));
    let mut view = TranscriptViewState::default();
    view.sync(&transcript);
    let error_id = view.selected().unwrap();

    assert!(view.toggle_selected(&transcript));
    assert_eq!(
        view.snapshot()
            .display_state(error_id, transcript.item_for_block(error_id).unwrap()),
        TranscriptDisplayState::Collapsed
    );
    assert!(view.toggle_all(&transcript));
    for (id, item) in transcript
        .view_entries()
        .filter(|(_, item)| item.foldable())
    {
        assert_eq!(
            view.snapshot().display_state(id, item),
            TranscriptDisplayState::Expanded
        );
    }
    assert!(view.toggle_all(&transcript));
    let first = transcript.view_entries().next().unwrap();
    assert_eq!(
        view.snapshot().display_state(first.0, first.1),
        TranscriptDisplayState::Preview
    );
}

#[test]
fn tool_arguments_have_independent_disclosure_state() {
    let mut transcript = Transcript::new();
    transcript.push(tool("call-1", false));
    let mut view = TranscriptViewState::default();
    view.sync(&transcript);
    let selected = view.selected().unwrap();
    let item = transcript.item_for_block(selected).unwrap();

    assert_eq!(
        view.snapshot().tool_argument_state(selected, item),
        TranscriptDisplayState::Collapsed
    );
    assert!(view.toggle_selected_arguments(&transcript));
    assert_eq!(
        view.snapshot().tool_argument_state(selected, item),
        TranscriptDisplayState::Preview
    );
    assert_eq!(
        view.snapshot().display_state(selected, item),
        TranscriptDisplayState::Preview
    );
}

#[test]
fn replacing_transcript_discards_old_selection_and_display_state() {
    let mut first = Transcript::new();
    first.push(tool("call-1", false));
    let mut view = TranscriptViewState::default();
    view.sync(&first);
    let old = view.selected().unwrap();
    assert!(view.toggle_selected(&first));

    let mut second = Transcript::new();
    second.push(tool("call-2", false));
    view.sync(&second);

    assert_ne!(view.selected(), Some(old));
    let selected = view.selected().unwrap();
    assert_eq!(
        view.snapshot()
            .display_state(selected, second.item_for_block(selected).unwrap()),
        TranscriptDisplayState::Preview
    );
}

#[test]
fn view_only_row_changes_preserve_anchor_without_marking_new_output() {
    let mut transcript = Transcript::new();
    transcript.scroll_page_up(4);

    transcript.preserve_scrolled_view_after_row_change(4, 20, 25);
    assert_eq!(transcript.scroll_offset(), 9);
    assert!(!transcript.has_new_output_below());

    transcript.preserve_scrolled_view_after_row_change(9, 25, 22);
    assert_eq!(transcript.scroll_offset(), 6);
    assert!(!transcript.has_new_output_below());
}

#[test]
fn ensuring_a_row_range_visible_scrolls_in_both_directions() {
    let mut transcript = Transcript::new();

    transcript.ensure_row_range_visible(30, 4, 7, 6);
    assert_eq!(transcript.scroll_offset(), 20);

    transcript.ensure_row_range_visible(30, 25, 28, 6);
    assert_eq!(transcript.scroll_offset(), 2);

    transcript.ensure_row_range_visible(30, 24, 30, 6);
    assert_eq!(transcript.scroll_offset(), 0);
}
