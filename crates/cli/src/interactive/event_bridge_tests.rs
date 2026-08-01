//! Internal owner tests for the interactive event bridge.

use super::event_bridge::CodingEventBridge;
use super::{Transcript, TranscriptItem, UiEvent};
use coding_agent::api::error::{
    CodingAgentErrorCategory, CodingAgentErrorContext, CodingAgentPublicError,
};
use coding_agent::api::event::{
    CodingAgentCapabilityProductEvent, CodingAgentDelegationEventContext,
    CodingAgentDelegationProductEvent, CodingAgentImageContent, CodingAgentMessageProductEvent,
    CodingAgentProductEventCapabilityRevocation, CodingAgentProductEventKind,
    CodingAgentProductEventProfileKind, CodingAgentProductEventUsage,
    CodingAgentRuntimeProductEvent, CodingAgentSessionProductEvent, CodingAgentToolProductEvent,
    CodingAgentWorkflowProductEvent,
};
use coding_agent::api::operation::{
    SelfHealingEditCheckOutput, SelfHealingEditDiagnostic, SelfHealingEditReplacement,
};

fn product_event(
    event: CodingAgentProductEventKind,
) -> coding_agent::api::event::CodingAgentProductEvent {
    let delivery_class = match &event {
        CodingAgentProductEventKind::Workflow(
            CodingAgentWorkflowProductEvent::OperationRecovered { .. },
        ) => "recovery",
        CodingAgentProductEventKind::Capability(_)
        | CodingAgentProductEventKind::Runtime(CodingAgentRuntimeProductEvent::ShutDown) => {
            "control"
        }
        _ => "data",
    };
    serde_json::from_value(serde_json::json!({
        "stream_id": "cli-interactive-test",
        "sequence": 1,
        "event": event,
        "operation_id": null,
        "terminal_status": null,
        "terminal_operation": null,
        "durability": { "state": "live_only" },
        "delivery_class": delivery_class,
    }))
    .expect("public product-event fixture must deserialize")
}

fn empty_usage() -> CodingAgentProductEventUsage {
    CodingAgentProductEventUsage {
        input: 0,
        output: 0,
        reasoning_tokens: 0,
        cache_read: 0,
        cache_write: 0,
        total_tokens: 0,
        cost_known: true,
        input_cost: 0.0,
        output_cost: 0.0,
        cache_read_cost: 0.0,
        cache_write_cost: 0.0,
    }
}

fn public_error(
    category: CodingAgentErrorCategory,
    code: &str,
    retryable: bool,
    summary: &str,
) -> CodingAgentPublicError {
    CodingAgentPublicError {
        category,
        code: code.into(),
        retryable,
        summary: summary.into(),
        context: CodingAgentErrorContext::None,
    }
}

#[test]
fn coding_event_bridge_maps_assistant_events() {
    let mut bridge = CodingEventBridge::new();

    let delta = bridge.push_product_event(&product_event(CodingAgentProductEventKind::Message(
        CodingAgentMessageProductEvent::Delta {
            operation_id: "op_1".to_string(),
            turn_id: "turn_1".to_string(),
            message_id: Some("msg_1".to_string()),
            text: "hello".to_string(),
        },
    )));
    assert_eq!(
        delta,
        vec![UiEvent::AssistantDelta {
            text: "hello".to_string()
        }]
    );
    let thinking = bridge.push_product_event(&product_event(CodingAgentProductEventKind::Message(
        CodingAgentMessageProductEvent::ThinkingDelta {
            operation_id: "op_1".to_string(),
            turn_id: "turn_1".to_string(),
            message_id: Some("msg_1".to_string()),
            text: "thinking".to_string(),
        },
    )));
    assert_eq!(
        thinking,
        vec![UiEvent::ThinkingDelta {
            text: "thinking".to_string()
        }]
    );

    let done = bridge.push_product_event(&product_event(CodingAgentProductEventKind::Message(
        CodingAgentMessageProductEvent::Completed {
            operation_id: "op_1".to_string(),
            turn_id: "turn_1".to_string(),
            message_id: Some("msg_1".to_string()),
            final_text: "hello".to_string(),
            images: Vec::new(),
            reasoning_duration_millis: None,
            usage: CodingAgentProductEventUsage {
                input: 100,
                output: 50,
                reasoning_tokens: 0,
                cache_read: 0,
                cache_write: 0,
                total_tokens: 150,
                cost_known: true,
                input_cost: 0.125,
                output_cost: 0.125,
                cache_read_cost: 0.0,
                cache_write_cost: 0.0,
            },
        },
    )));
    assert_eq!(
        done,
        vec![
            UiEvent::AssistantDone,
            UiEvent::UsageUpdate {
                input: 100,
                output: 50,
                cache_read: 0,
                cache_write: 0,
                cost: 0.25,
                context_tokens: Some(150),
            },
        ]
    );

    // A second assistant message is a separate delta; the bridge no longer
    // accumulates (the receiver does). Each UsageUpdate carries per-event
    // usage only; context_tokens reflects the latest message (mirrors TS
    // getContextUsage using the most recent usage).
    let done2 = bridge.push_product_event(&product_event(CodingAgentProductEventKind::Message(
        CodingAgentMessageProductEvent::Completed {
            operation_id: "op_1".to_string(),
            turn_id: "turn_1".to_string(),
            message_id: Some("msg_2".to_string()),
            final_text: "world".to_string(),
            images: Vec::new(),
            reasoning_duration_millis: None,
            usage: CodingAgentProductEventUsage {
                input: 30,
                output: 20,
                reasoning_tokens: 0,
                cache_read: 5,
                cache_write: 0,
                total_tokens: 0,
                cost_known: true,
                input_cost: 0.0625,
                output_cost: 0.0625,
                cache_read_cost: 0.0,
                cache_write_cost: 0.0,
            },
        },
    )));
    assert_eq!(
        done2,
        vec![
            UiEvent::AssistantDone,
            UiEvent::UsageUpdate {
                input: 30,
                output: 20,
                cache_read: 5,
                cache_write: 0,
                cost: 0.125,
                context_tokens: Some(55),
            },
        ]
    );
}

#[test]
fn coding_event_bridge_marks_zero_usage_context_unknown() {
    let mut bridge = CodingEventBridge::new();

    let done = bridge.push_product_event(&product_event(CodingAgentProductEventKind::Message(
        CodingAgentMessageProductEvent::Completed {
            operation_id: "op_1".to_string(),
            turn_id: "turn_1".to_string(),
            message_id: Some("msg_1".to_string()),
            final_text: "hello".to_string(),
            images: Vec::new(),
            reasoning_duration_millis: None,
            usage: empty_usage(),
        },
    )));

    assert_eq!(
        done,
        vec![
            UiEvent::AssistantDone,
            UiEvent::UsageUpdate {
                input: 0,
                output: 0,
                cache_read: 0,
                cache_write: 0,
                cost: 0.0,
                context_tokens: None,
            },
        ]
    );
}

#[test]
fn coding_event_bridge_projects_assistant_images_as_structured_blocks() {
    let mut bridge = CodingEventBridge::new();
    let image = CodingAgentImageContent {
        mime_type: "image/png".into(),
        data: "cG5n".into(),
    };
    let events = bridge.push_product_event(&product_event(CodingAgentProductEventKind::Message(
        CodingAgentMessageProductEvent::Completed {
            operation_id: "op_1".into(),
            turn_id: "turn_1".into(),
            message_id: Some("msg_1".into()),
            final_text: "caption".into(),
            images: vec![image.clone()],
            reasoning_duration_millis: None,
            usage: empty_usage(),
        },
    )));

    assert!(matches!(events[0], UiEvent::AssistantDone));
    assert_eq!(
        events[1],
        UiEvent::AssistantImages {
            images: vec![image]
        }
    );
    let mut transcript = Transcript::new();
    for event in events {
        transcript.apply_event(event);
    }
    assert!(matches!(
        transcript.items().last(),
        Some(TranscriptItem::Image { mime_type, data })
            if mime_type == "image/png" && data == "cG5n"
    ));
}

#[test]
fn coding_event_bridge_maps_tool_events() {
    let mut bridge = CodingEventBridge::new();

    let start = bridge.push_product_event(&product_event(CodingAgentProductEventKind::Tool(
        CodingAgentToolProductEvent::Started {
            operation_id: "op_1".to_string(),
            turn_id: "turn_1".to_string(),
            tool_call_id: "tool_1".to_string(),
            name: "read".to_string(),
            arguments_json: r#"{"path":"src/lib.rs"}"#.to_string(),
        },
    )));
    assert_eq!(
        start,
        vec![UiEvent::ToolStarted {
            call_id: "tool_1".to_string(),
            name: "read".to_string(),
            args: serde_json::json!({"path": "src/lib.rs"}),
        }]
    );

    let update = bridge.push_product_event(&product_event(CodingAgentProductEventKind::Tool(
        CodingAgentToolProductEvent::Updated {
            operation_id: "op_1".to_string(),
            turn_id: "turn_1".to_string(),
            tool_call_id: "tool_1".to_string(),
            name: "read".to_string(),
            message: "reading".to_string(),
        },
    )));
    assert_eq!(
        update,
        vec![UiEvent::ToolUpdated {
            call_id: "tool_1".to_string(),
            result: "reading".to_string(),
        }]
    );

    let completed = bridge.push_product_event(&product_event(CodingAgentProductEventKind::Tool(
        CodingAgentToolProductEvent::Completed {
            operation_id: "op_1".to_string(),
            turn_id: "turn_1".to_string(),
            tool_call_id: "tool_1".to_string(),
            name: "read".to_string(),
            summary: "ok".to_string(),
        },
    )));
    assert_eq!(
        completed,
        vec![UiEvent::ToolFinished {
            call_id: "tool_1".to_string(),
            result: "ok".to_string(),
            is_error: false,
        }]
    );

    let failed = bridge.push_product_event(&product_event(CodingAgentProductEventKind::Tool(
        CodingAgentToolProductEvent::Failed {
            operation_id: "op_1".to_string(),
            turn_id: "turn_1".to_string(),
            tool_call_id: "tool_2".to_string(),
            name: "read".to_string(),
            message: "denied".to_string(),
        },
    )));
    assert_eq!(
        failed,
        vec![UiEvent::ToolFinished {
            call_id: "tool_2".to_string(),
            result: "denied".to_string(),
            is_error: true,
        }]
    );
}

#[test]
fn coding_event_bridge_preserves_malformed_tool_arguments() {
    let mut bridge = CodingEventBridge::new();

    let events = bridge.push_product_event(&product_event(CodingAgentProductEventKind::Tool(
        CodingAgentToolProductEvent::Started {
            operation_id: "op_1".to_string(),
            turn_id: "turn_1".to_string(),
            tool_call_id: "tool_1".to_string(),
            name: "bash".to_string(),
            arguments_json: "{bad json".to_string(),
        },
    )));

    assert_eq!(
        events,
        vec![UiEvent::ToolStarted {
            call_id: "tool_1".to_string(),
            name: "bash".to_string(),
            args: serde_json::Value::String("{bad json".to_string()),
        }]
    );
}

#[test]
fn coding_event_bridge_maps_failure_abort_and_compaction() {
    let mut bridge = CodingEventBridge::new();

    let failed = bridge.push_product_event(&product_event(CodingAgentProductEventKind::Workflow(
        CodingAgentWorkflowProductEvent::PromptFailed {
            operation_id: "op_1".to_string(),
            error: public_error(
                CodingAgentErrorCategory::Provider,
                "provider",
                true,
                "The model provider request failed.",
            ),
        },
    )));
    assert_eq!(
        failed,
        vec![UiEvent::AgentError {
            error: "The model provider request failed.".to_string(),
        }]
    );

    let aborted = bridge.push_product_event(&product_event(CodingAgentProductEventKind::Workflow(
        CodingAgentWorkflowProductEvent::PromptAborted {
            operation_id: "op_1".to_string(),
            reason: "user cancelled".to_string(),
        },
    )));
    assert_eq!(
        aborted,
        vec![UiEvent::AgentError {
            error: "prompt aborted: user cancelled".to_string(),
        }]
    );

    let compacted = bridge.push_product_event(&product_event(
        CodingAgentProductEventKind::Session(CodingAgentSessionProductEvent::CompactionCompleted {
            operation_id: "op_1".to_string(),
            turn_id: "turn_1".to_string(),
            summary: "kept recent context".to_string(),
            first_kept_message_id: "msg_2".to_string(),
            tokens_before: 1200,
        }),
    ));
    assert_eq!(
        compacted,
        vec![
            UiEvent::CompactionNotice {
                summary: "kept recent context".to_string(),
            },
            UiEvent::UsageUpdate {
                input: 0,
                output: 0,
                cache_read: 0,
                cache_write: 0,
                cost: 0.0,
                context_tokens: None,
            },
        ]
    );
}

#[test]
fn coding_event_bridge_maps_delegation_confirmation_events() {
    let mut bridge = CodingEventBridge::new();

    let events =
        bridge.push_product_event(&product_event(CodingAgentProductEventKind::Delegation(
            CodingAgentDelegationProductEvent::ConfirmationRequired {
                context: CodingAgentDelegationEventContext {
                    operation_id: "op_1".to_string(),
                    turn_id: "turn_1".to_string(),
                    tool_call_id: "tool_delegate_agent".to_string(),
                    requesting_profile_id: "planner".into(),
                    target_kind: CodingAgentProductEventProfileKind::Agent,
                    target_id: "coder".into(),
                    task: "implement parser".to_string(),
                },
                reason: "profile policy requires confirmation".to_string(),
            },
        )));

    let [
        UiEvent::DelegationBlock {
            call_id,
            target_kind,
            target_id,
            task,
            status,
            summary,
            ..
        },
        UiEvent::DelegationConfirmationRequired { pending },
    ] = events.as_slice()
    else {
        panic!("expected delegation block and automatic confirmation, got {events:?}");
    };
    assert_eq!(call_id, "tool_delegate_agent");
    assert_eq!(target_kind, "agent");
    assert_eq!(target_id, "coder");
    assert_eq!(task, "implement parser");
    assert_eq!(status, "confirmation_required");
    let text = summary.as_deref().expect("confirmation summary");
    assert!(text.contains("confirmation required"), "{text}");
    assert_eq!(pending.operation_id, "op_1");
    assert_eq!(pending.tool_call_id, "tool_delegate_agent");
    assert_eq!(pending.target_id.as_str(), "coder");
    assert_eq!(pending.reason, "profile policy requires confirmation");

    let completed = bridge.push_product_event(&product_event(
        CodingAgentProductEventKind::Delegation(CodingAgentDelegationProductEvent::Completed {
            context: CodingAgentDelegationEventContext {
                operation_id: "op_1".to_string(),
                turn_id: "turn_1".to_string(),
                tool_call_id: "tool_delegate_agent".to_string(),
                requesting_profile_id: "planner".into(),
                target_kind: CodingAgentProductEventProfileKind::Agent,
                target_id: "coder".into(),
                task: "implement parser".to_string(),
            },
            child_operation_id: "op_child".to_string(),
            final_text: "child result".to_string(),
        }),
    ));

    let [
        UiEvent::DelegationBlock {
            call_id,
            target_kind,
            target_id,
            status,
            child_operation_id,
            summary,
            is_error,
            ..
        },
    ] = completed.as_slice()
    else {
        panic!("expected one delegation block, got {completed:?}");
    };
    assert_eq!(call_id, "tool_delegate_agent");
    assert_eq!(target_kind, "agent");
    assert_eq!(target_id, "coder");
    assert_eq!(status, "completed");
    assert_eq!(child_operation_id.as_deref(), Some("op_child"));
    assert_eq!(summary.as_deref(), Some("completed: child result"));
    assert!(!is_error);
}

#[test]
fn coding_event_bridge_folds_delegation_lifecycle_into_one_transcript_item() {
    let mut bridge = CodingEventBridge::new();
    let mut transcript = Transcript::new();

    for event in [
        CodingAgentProductEventKind::Delegation(CodingAgentDelegationProductEvent::Started {
            context: CodingAgentDelegationEventContext {
                operation_id: "op_1".to_string(),
                turn_id: "turn_1".to_string(),
                tool_call_id: "tool_delegate_agent".to_string(),
                requesting_profile_id: "planner".into(),
                target_kind: CodingAgentProductEventProfileKind::Agent,
                target_id: "coder".into(),
                task: "implement parser".to_string(),
            },
            child_operation_id: "op_child".to_string(),
        }),
        CodingAgentProductEventKind::Delegation(CodingAgentDelegationProductEvent::Completed {
            context: CodingAgentDelegationEventContext {
                operation_id: "op_1".to_string(),
                turn_id: "turn_1".to_string(),
                tool_call_id: "tool_delegate_agent".to_string(),
                requesting_profile_id: "planner".into(),
                target_kind: CodingAgentProductEventProfileKind::Agent,
                target_id: "coder".into(),
                task: "implement parser".to_string(),
            },
            child_operation_id: "op_child".to_string(),
            final_text: "child result".to_string(),
        }),
    ] {
        for ui_event in bridge.push_product_event(&product_event(event)) {
            transcript.apply_event(ui_event);
        }
    }

    assert_eq!(
        transcript.items(),
        &[TranscriptItem::Tool {
            call_id: "tool_delegate_agent".to_string(),
            name: "delegation".to_string(),
            args: serde_json::json!({
                "targetKind": "agent",
                "targetId": "coder",
                "task": "implement parser",
                "status": "completed",
                "childOperationId": "op_child"
            }),
            result: Some("completed: child result".to_string()),
            is_error: false,
        }]
    );
}

#[test]
fn coding_event_bridge_maps_self_healing_edit_events() {
    let mut bridge = CodingEventBridge::new();

    let started = bridge.push_product_event(&product_event(CodingAgentProductEventKind::Workflow(
        CodingAgentWorkflowProductEvent::SelfHealingEditStarted {
            operation_id: "op_edit".to_string(),
            path: "src/app.txt".to_string(),
            replacements: 1,
        },
    )));
    let [UiEvent::SystemNotice { text }] = started.as_slice() else {
        panic!("expected one system notice, got {started:?}");
    };
    assert!(text.contains("Self-healing edit started"), "{text}");
    assert!(text.contains("src/app.txt"), "{text}");

    let repair = bridge.push_product_event(&product_event(CodingAgentProductEventKind::Workflow(
        CodingAgentWorkflowProductEvent::SelfHealingEditRepairAttempted {
            operation_id: "op_edit".to_string(),
            path: "src/app.txt".to_string(),
            attempt: 1,
            replacements: vec![SelfHealingEditReplacement::new("deux", "dos").into()],
            diagnostics: vec![
                SelfHealingEditDiagnostic {
                    message: "compile error".to_string(),
                }
                .into(),
            ],
            check_output: Some(
                SelfHealingEditCheckOutput {
                    command: "cargo check".to_string(),
                    stdout: "fixed".to_string(),
                    stderr: String::new(),
                    exit_code: 0,
                }
                .into(),
            ),
        },
    )));
    let [UiEvent::SystemNotice { text }] = repair.as_slice() else {
        panic!("expected one system notice, got {repair:?}");
    };
    assert!(text.contains("repair attempt 1"), "{text}");
    assert!(text.contains("src/app.txt"), "{text}");
    assert!(text.contains("exit 0"), "{text}");

    let completed =
        bridge.push_product_event(&product_event(CodingAgentProductEventKind::Workflow(
            CodingAgentWorkflowProductEvent::SelfHealingEditCompleted {
                operation_id: "op_edit".to_string(),
                path: "src/app.txt".to_string(),
                attempts: 2,
                first_changed_line: Some(2),
                check_output: None,
            },
        )));
    let [UiEvent::SystemNotice { text }] = completed.as_slice() else {
        panic!("expected one system notice, got {completed:?}");
    };
    assert!(text.contains("Self-healing edit completed"), "{text}");
    assert!(text.contains("2 attempts"), "{text}");

    let failed = bridge.push_product_event(&product_event(CodingAgentProductEventKind::Workflow(
        CodingAgentWorkflowProductEvent::SelfHealingEditFailed {
            operation_id: "op_edit_failed".to_string(),
            path: "src/bad.txt".to_string(),
            error: public_error(
                CodingAgentErrorCategory::Input,
                "invalid_input",
                false,
                "The request is invalid.",
            ),
        },
    )));
    let [UiEvent::SystemNotice { text }] = failed.as_slice() else {
        panic!("expected one system notice, got {failed:?}");
    };
    assert!(text.contains("Self-healing edit failed"), "{text}");
    assert!(text.contains("The request is invalid."), "{text}");
}

#[test]
fn coding_event_bridge_ignores_session_write_and_capability_events() {
    let mut bridge = CodingEventBridge::new();

    let ignored = [
        CodingAgentProductEventKind::Session(CodingAgentSessionProductEvent::WritePending {
            operation_id: "op_1".to_string(),
        }),
        CodingAgentProductEventKind::Session(CodingAgentSessionProductEvent::WriteCommitted {
            operation_id: "op_1".to_string(),
            session_id: "session_1".to_string(),
        }),
        CodingAgentProductEventKind::Session(CodingAgentSessionProductEvent::WriteSkipped {
            operation_id: "op_1".to_string(),
            reason: "session persistence disabled".to_string(),
        }),
        CodingAgentProductEventKind::Capability(CodingAgentCapabilityProductEvent::Changed {
            generation: 1,
            revocation: CodingAgentProductEventCapabilityRevocation::RequestCancelOlderOperations,
            cancellation_requested_operation_ids: Vec::new(),
        }),
    ];

    for event in ignored {
        assert!(bridge.push_product_event(&product_event(event)).is_empty());
    }
}
