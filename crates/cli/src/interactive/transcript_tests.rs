//! Internal owner tests for the interactive transcript.

use super::{Transcript, TranscriptItem, UiEvent};

#[test]
fn transcript_scrolls_within_bounds() {
    let mut transcript = Transcript::new();
    for i in 0..20 {
        transcript.push(TranscriptItem::user(format!("message {i}")));
    }
    transcript.scroll_page_up(5);
    assert_eq!(transcript.scroll_offset(), 5);
    transcript.scroll_page_down(2);
    assert_eq!(transcript.scroll_offset(), 3);
    transcript.scroll_page_down(usize::MAX);
    assert_eq!(transcript.scroll_offset(), 0);
}

#[test]
fn transcript_keeps_scrolled_view_locked_when_new_output_arrives() {
    let mut transcript = Transcript::new();
    for i in 0..20 {
        transcript.push(TranscriptItem::user(format!("message {i}")));
    }
    transcript.scroll_page_up(5);

    transcript.apply_event(UiEvent::AssistantDelta {
        text: "new output".to_string(),
    });

    assert!(transcript.scroll_offset() > 5);
    assert!(transcript.has_new_output_below());
    transcript.scroll_page_down(usize::MAX);
    assert_eq!(transcript.scroll_offset(), 0);
    assert!(!transcript.has_new_output_below());
}

#[test]
fn tool_event_closes_current_assistant_before_next_assistant_delta() {
    let mut transcript = Transcript::new();

    transcript.apply_event(UiEvent::AssistantDelta {
        text: "I will inspect the file.".to_string(),
    });
    transcript.apply_event(UiEvent::ToolStarted {
        call_id: "tool_1".to_string(),
        name: "read".to_string(),
        args: serde_json::json!({"path": "src/lib.rs"}),
    });
    transcript.apply_event(UiEvent::ToolFinished {
        call_id: "tool_1".to_string(),
        result: "file contents".to_string(),
        is_error: false,
    });
    transcript.apply_event(UiEvent::AssistantDelta {
        text: "The file contains a Rust module.".to_string(),
    });

    assert_eq!(
        transcript.items(),
        &[
            TranscriptItem::Assistant {
                id: "assistant_0".to_string(),
                markdown: "I will inspect the file.".to_string(),
                thinking: String::new(),
                done: true,
            },
            TranscriptItem::Tool {
                call_id: "tool_1".to_string(),
                name: "read".to_string(),
                args: serde_json::json!({"path": "src/lib.rs"}),
                result: Some("file contents".to_string()),
                is_error: false,
            },
            TranscriptItem::Assistant {
                id: "assistant_2".to_string(),
                markdown: "The file contains a Rust module.".to_string(),
                thinking: String::new(),
                done: false,
            },
        ]
    );
}

#[test]
fn turn_started_closes_current_assistant_without_creating_empty_message() {
    let mut transcript = Transcript::new();

    transcript.apply_event(UiEvent::AssistantDelta {
        text: "first turn".to_string(),
    });
    transcript.apply_event(UiEvent::TurnStarted);
    transcript.apply_event(UiEvent::AssistantDelta {
        text: "second turn".to_string(),
    });

    assert_eq!(
        transcript.items(),
        &[
            TranscriptItem::Assistant {
                id: "assistant_0".to_string(),
                markdown: "first turn".to_string(),
                thinking: String::new(),
                done: true,
            },
            TranscriptItem::Assistant {
                id: "assistant_1".to_string(),
                markdown: "second turn".to_string(),
                thinking: String::new(),
                done: false,
            },
        ]
    );
}

#[test]
fn agent_error_closes_current_assistant_before_error_item() {
    let mut transcript = Transcript::new();

    transcript.apply_event(UiEvent::AssistantDelta {
        text: "partial answer".to_string(),
    });
    transcript.apply_event(UiEvent::AgentError {
        error: "provider failed".to_string(),
    });

    assert_eq!(
        transcript.items(),
        &[
            TranscriptItem::Assistant {
                id: "assistant_0".to_string(),
                markdown: "partial answer".to_string(),
                thinking: String::new(),
                done: true,
            },
            TranscriptItem::Error {
                text: "provider failed".to_string(),
            },
        ]
    );
}

#[test]
fn system_item_is_pushed_and_rendered_as_a_line() {
    let mut transcript = Transcript::new();
    transcript.push(TranscriptItem::system("welcome to evo"));
    assert_eq!(transcript.items().len(), 1);
    match transcript.items()[0] {
        TranscriptItem::System { ref text } => assert_eq!(text, "welcome to evo"),
        _ => panic!("expected System item"),
    }
}

#[test]
fn transcript_revision_changes_only_on_real_mutation() {
    let mut transcript = Transcript::new();
    let initial = transcript.revision();

    transcript.apply_event(UiEvent::UsageUpdate {
        input: 1,
        output: 2,
        cache_read: 3,
        cache_write: 4,
        cost: 0.5,
        context_tokens: Some(10),
    });
    assert_eq!(transcript.revision(), initial);

    transcript.push(TranscriptItem::user("hello"));
    let after_push = transcript.revision();
    assert!(after_push > initial);

    transcript.scroll_page_down(1);
    assert_eq!(transcript.revision(), after_push);

    transcript.scroll_page_up(1);
    let after_scroll = transcript.revision();
    assert!(after_scroll > after_push);

    transcript.apply_event(UiEvent::AssistantDelta {
        text: "reply".to_string(),
    });
    assert!(transcript.revision() > after_scroll);
}
