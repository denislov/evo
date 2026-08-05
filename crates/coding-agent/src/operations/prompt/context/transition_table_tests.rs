use super::*;
use crate::kernel::error::SessionWriteFailureReason;

fn context() -> PromptTurnContext {
    PromptTurnContext::new(
        PromptTurnIds::new("operation", "turn"),
        PromptTurnOptions::new(PromptInvocation::Text("test".into())),
    )
}

fn assistant_message(text: &str) -> AssistantMessage {
    let mut message = AssistantMessage::empty("test-api", "test-model");
    message.content.push(ContentBlock::Text {
        text: text.into(),
        text_signature: None,
    });
    message
}

#[test]
fn prompt_input_preparation_transition_table() {
    #[derive(Debug)]
    enum Expected {
        Text(&'static str),
        Error(&'static str),
    }

    let cases = [
        (
            "text",
            PromptInvocation::Text("hello".into()),
            Expected::Text("hello"),
        ),
        (
            "empty text",
            PromptInvocation::Text(String::new()),
            Expected::Error("input"),
        ),
        (
            "content",
            PromptInvocation::Content(vec![ContentBlock::Text {
                text: "content".into(),
                text_signature: None,
            }]),
            Expected::Text("content"),
        ),
        (
            "empty content",
            PromptInvocation::Content(Vec::new()),
            Expected::Error("input"),
        ),
        (
            "skill",
            PromptInvocation::Skill {
                name: "review".into(),
                additional_instructions: Some("focus on safety".into()),
            },
            Expected::Text("skill:review\nfocus on safety"),
        ),
        (
            "prompt template",
            PromptInvocation::PromptTemplate {
                name: "release".into(),
                args: vec!["v1".into(), "stable".into()],
            },
            Expected::Text("prompt_template:release\nv1\nstable"),
        ),
        (
            "manual compaction",
            PromptInvocation::Compact {
                custom_instructions: None,
            },
            Expected::Error("unsupported_capability"),
        ),
    ];

    for (name, invocation, expected) in cases {
        match (
            persisted_content_blocks_from_invocation(&invocation),
            expected,
        ) {
            (Ok(blocks), Expected::Text(expected_text)) => assert!(
                matches!(
                    blocks.as_slice(),
                    [PersistedContentBlock::Text { text }] if text == expected_text
                ),
                "{name}: {blocks:?}"
            ),
            (Err(error), Expected::Error(expected_code)) => {
                assert_eq!(error.code(), expected_code, "{name}")
            }
            (actual, expected) => panic!("{name}: expected {expected:?}, got {actual:?}"),
        }
    }
}

#[test]
fn prompt_completion_recording_transition_table() {
    #[derive(Debug, Clone, Copy)]
    enum Action {
        Complete,
        RecordFinal,
    }

    let mut context = context();
    let cases = [
        (Action::Complete, false, false),
        (Action::RecordFinal, true, false),
        (Action::Complete, true, true),
        (Action::Complete, true, true),
    ];

    for (action, expected_ok, expected_recorded) in cases {
        let result = match action {
            Action::Complete => context.record_prompt_completed(),
            Action::RecordFinal => {
                context.record_final_message(assistant_message("done"));
                Ok(())
            }
        };
        assert_eq!(result.is_ok(), expected_ok, "{action:?}");
        assert_eq!(context.completion_recorded, expected_recorded, "{action:?}");
    }
}

#[test]
fn prompt_outcome_transition_table() {
    #[derive(Debug, Clone, Copy)]
    enum Action {
        SuccessWithoutMessage,
        Success,
        Abort,
        Fail,
        FailQueueSaturated,
    }

    #[derive(Debug, Clone, Copy)]
    enum Expected {
        Error,
        Success,
        Aborted,
        Failed { diagnostics: usize },
    }

    let cases = [
        (Action::SuccessWithoutMessage, Expected::Error),
        (Action::Success, Expected::Success),
        (Action::Abort, Expected::Aborted),
        (Action::Fail, Expected::Failed { diagnostics: 0 }),
        (
            Action::FailQueueSaturated,
            Expected::Failed { diagnostics: 1 },
        ),
    ];

    for (action, expected) in cases {
        let mut context = context();
        let outcome = match action {
            Action::SuccessWithoutMessage => context.finish_success(None, None),
            Action::Success => {
                context.record_final_message(assistant_message("done"));
                context.finish_success(Some("session".into()), Some("leaf".into()))
            }
            Action::Abort => Ok(context.finish_abort("cancelled", Some("session".into()))),
            Action::Fail => Ok(context.finish_failure(CodingSessionError::Provider {
                message: "provider failed".into(),
            })),
            Action::FailQueueSaturated => Ok(context.finish_failure(
                CodingSessionError::SessionWriteFailure {
                    reason: SessionWriteFailureReason::QueueSaturated,
                    message: "writer queue is full".into(),
                },
            )),
        };

        match (outcome, expected) {
            (Err(_), Expected::Error) => {}
            (
                Ok(InternalPromptTurnOutcome::Success {
                    final_text,
                    session_id,
                    leaf_id,
                    ..
                }),
                Expected::Success,
            ) => {
                assert_eq!(final_text, "done");
                assert_eq!(session_id.as_deref(), Some("session"));
                assert_eq!(leaf_id.as_deref(), Some("leaf"));
            }
            (Ok(InternalPromptTurnOutcome::Aborted { reason, .. }), Expected::Aborted) => {
                assert_eq!(reason, "cancelled")
            }
            (
                Ok(InternalPromptTurnOutcome::Failed { diagnostics, .. }),
                Expected::Failed {
                    diagnostics: expected_diagnostics,
                },
            ) => assert_eq!(diagnostics.len(), expected_diagnostics),
            (actual, expected) => {
                panic!("{action:?}: expected {expected:?}, got {actual:?}")
            }
        }
    }
}

#[test]
fn queue_saturation_adds_an_operation_diagnostic() {
    let context = PromptTurnContext::new(
        PromptTurnIds::new("operation-queue-saturated", "turn-queue-saturated"),
        PromptTurnOptions::new(PromptInvocation::Text("test".into())),
    );
    let outcome = context.finish_failure(CodingSessionError::SessionWriteFailure {
        reason: SessionWriteFailureReason::QueueSaturated,
        message: "bounded queue timeout".into(),
    });
    let InternalPromptTurnOutcome::Failed { diagnostics, .. } = outcome else {
        panic!("queue saturation must remain a typed failed prompt outcome");
    };
    assert!(diagnostics.iter().any(|diagnostic| {
        diagnostic
            .message
            .contains("Session persistence is lagging")
    }));
}
