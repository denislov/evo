use super::events::CodingProtocolEventAdapter;
use super::types::ProtocolEvent;
use coding_agent::api::error::{
    CodingAgentErrorCategory, CodingAgentErrorContext, CodingAgentPublicError,
};
use coding_agent::api::event::{
    CodingAgentAgentProductEvent, CodingAgentCapabilityProductEvent,
    CodingAgentDelegationEventContext, CodingAgentDelegationProductEvent,
    CodingAgentMessageProductEvent, CodingAgentProductEventCapabilityRevocation,
    CodingAgentProductEventCheckOutput, CodingAgentProductEventDiagnostic,
    CodingAgentProductEventKind, CodingAgentProductEventProfileKind,
    CodingAgentProductEventReplacement, CodingAgentProductEventUsage,
    CodingAgentRuntimeProductEvent, CodingAgentSessionProductEvent,
    CodingAgentSessionWriteFailureStatus, CodingAgentTeamProductEvent, CodingAgentToolProductEvent,
    CodingAgentWorkflowProductEvent,
};
use coding_agent::api::operation::{
    SelfHealingEditCheckOutput, SelfHealingEditDiagnostic, SelfHealingEditReplacement,
};
use serde_json::{Value, json};

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
    serde_json::from_value(json!({
        "stream_id": "cli-protocol-test",
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

fn product_usage() -> CodingAgentProductEventUsage {
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

fn product_error(
    category: CodingAgentErrorCategory,
    code: &str,
    retryable: bool,
    summary: &str,
) -> coding_agent::api::event::CodingAgentProductEventError {
    CodingAgentPublicError {
        category,
        code: code.into(),
        retryable,
        summary: summary.into(),
        context: CodingAgentErrorContext::None,
    }
}

fn product_replacement(
    replacement: SelfHealingEditReplacement,
) -> CodingAgentProductEventReplacement {
    CodingAgentProductEventReplacement {
        old_text: replacement.old_text,
        new_text: replacement.new_text,
    }
}

fn product_diagnostic(diagnostic: SelfHealingEditDiagnostic) -> CodingAgentProductEventDiagnostic {
    CodingAgentProductEventDiagnostic {
        message: diagnostic.message,
    }
}

fn product_check_output(output: SelfHealingEditCheckOutput) -> CodingAgentProductEventCheckOutput {
    CodingAgentProductEventCheckOutput {
        command: output.command,
        stdout: output.stdout,
        stderr: output.stderr,
        exit_code: output.exit_code,
    }
}

const FLOW_NODE_FIELD_NAMES: &[&str] = &[
    "flowNode",
    "flowNodeId",
    "flowNodeName",
    "lastNode",
    "nodeId",
];

fn protocol_json(event: &ProtocolEvent) -> Value {
    serde_json::to_value(event).expect("protocol event should serialize")
}

fn contains_protocol_type(events: &[ProtocolEvent], event_type: &str) -> bool {
    events
        .iter()
        .any(|event| protocol_json(event)["type"] == event_type)
}

#[test]
fn coding_event_adapter_maps_session_write_failure_state() {
    let mut adapter = CodingProtocolEventAdapter::new_with_provider(
        "test".into(),
        "test-provider".into(),
        "test-model".into(),
    );
    let event = product_event(CodingAgentProductEventKind::Session(
        CodingAgentSessionProductEvent::WriteFailed {
            operation_id: "op-write".into(),
            reason: "append result is uncertain".into(),
            status: CodingAgentSessionWriteFailureStatus::Uncertain,
            failure_reason: None,
        },
    ));

    assert_eq!(
        serde_json::to_value(adapter.push_product_event(&event)).unwrap(),
        json!([{
            "type": "session_write_failed",
            "operationId": "op-write",
            "status": "uncertain",
            "reason": "append result is uncertain"
        }])
    );
}

fn assert_no_flow_node_fields(value: &Value) {
    match value {
        Value::Object(map) => {
            for (key, nested) in map {
                assert!(
                    !FLOW_NODE_FIELD_NAMES.contains(&key.as_str()),
                    "protocol event exposed Flow node field `{key}` in {value}"
                );
                assert_no_flow_node_fields(nested);
            }
        }
        Value::Array(items) => {
            for item in items {
                assert_no_flow_node_fields(item);
            }
        }
        _ => {}
    }
}

#[test]
fn coding_event_adapter_maps_prompt_sequence_to_protocol_events() {
    let mut adapter = CodingProtocolEventAdapter::new_with_provider(
        "faux".into(),
        "faux-provider".into(),
        "faux-model".into(),
    );

    let mut events = Vec::new();
    for event in [
        CodingAgentProductEventKind::Agent(CodingAgentAgentProductEvent::TurnStarted {
            operation_id: "op_1".into(),
            turn_id: "turn_1".into(),
            agent_turn: 1,
        }),
        CodingAgentProductEventKind::Agent(CodingAgentAgentProductEvent::ProviderRequestStarted {
            operation_id: "op_1".into(),
            turn_id: "turn_1".into(),
            provider: "typed-provider".into(),
            model: "typed-model".into(),
            context_window: Some(128_000),
        }),
        CodingAgentProductEventKind::Message(CodingAgentMessageProductEvent::Started {
            operation_id: "op_1".into(),
            turn_id: "turn_1".into(),
            message_id: Some("msg_1".into()),
        }),
        CodingAgentProductEventKind::Message(CodingAgentMessageProductEvent::ThinkingDelta {
            operation_id: "op_1".into(),
            turn_id: "turn_1".into(),
            message_id: Some("msg_1".into()),
            text: "think".into(),
        }),
        CodingAgentProductEventKind::Message(CodingAgentMessageProductEvent::Delta {
            operation_id: "op_1".into(),
            turn_id: "turn_1".into(),
            message_id: Some("msg_1".into()),
            text: "hello".into(),
        }),
        CodingAgentProductEventKind::Message(CodingAgentMessageProductEvent::Completed {
            operation_id: "op_1".into(),
            turn_id: "turn_1".into(),
            message_id: Some("msg_1".into()),
            final_text: "hello".into(),
            images: Vec::new(),
            reasoning_duration_millis: None,
            usage: product_usage(),
        }),
        CodingAgentProductEventKind::Workflow(CodingAgentWorkflowProductEvent::PromptCompleted {
            operation_id: "op_1".into(),
            turn_id: "turn_1".into(),
        }),
    ] {
        events.extend(adapter.push_product_event(&product_event(event)));
    }

    assert_eq!(protocol_json(&events[0])["type"], "turn_start");
    assert!(events.iter().map(protocol_json).any(|event| {
        event["type"] == "message_start"
            && event["message"]["provider"] == "typed-provider"
            && event["message"]["model"] == "typed-model"
    }));
    assert!(contains_protocol_type(&events, "message_update"));
    assert!(events.iter().map(protocol_json).any(|event| {
        event["type"] == "message_update"
            && event["assistantMessageEvent"]["type"] == "thinking_delta"
            && event["assistantMessageEvent"]["delta"] == "think"
            && event["assistantMessageEvent"]["partial"]["content"][0]["type"] == "thinking"
            && event["assistantMessageEvent"]["partial"]["content"][0]["thinking"] == "think"
    }));
    assert!(contains_protocol_type(&events, "turn_end"));
    assert!(contains_protocol_type(&events, "agent_end"));
}

#[test]
fn product_event_protocol_adapter_does_not_emit_flow_node_fields() {
    let mut adapter = CodingProtocolEventAdapter::new_with_provider(
        "faux".into(),
        "faux-provider".into(),
        "faux-model".into(),
    );
    let check_output = SelfHealingEditCheckOutput {
        command: "cargo check".into(),
        stdout: "ok".into(),
        stderr: String::new(),
        exit_code: 0,
    };

    let events = [
        CodingAgentProductEventKind::Agent(CodingAgentAgentProductEvent::TurnStarted {
            operation_id: "op_prompt".into(),
            turn_id: "turn_prompt".into(),
            agent_turn: 1,
        }),
        CodingAgentProductEventKind::Message(CodingAgentMessageProductEvent::Started {
            operation_id: "op_prompt".into(),
            turn_id: "turn_prompt".into(),
            message_id: Some("msg_prompt".into()),
        }),
        CodingAgentProductEventKind::Message(CodingAgentMessageProductEvent::Delta {
            operation_id: "op_prompt".into(),
            turn_id: "turn_prompt".into(),
            message_id: Some("msg_prompt".into()),
            text: "hello".into(),
        }),
        CodingAgentProductEventKind::Message(CodingAgentMessageProductEvent::Completed {
            operation_id: "op_prompt".into(),
            turn_id: "turn_prompt".into(),
            message_id: Some("msg_prompt".into()),
            final_text: "hello".into(),
            images: Vec::new(),
            reasoning_duration_millis: None,
            usage: product_usage(),
        }),
        CodingAgentProductEventKind::Workflow(CodingAgentWorkflowProductEvent::PromptCompleted {
            operation_id: "op_prompt".into(),
            turn_id: "turn_prompt".into(),
        }),
        CodingAgentProductEventKind::Runtime(CodingAgentRuntimeProductEvent::CompactionCompleted {
            operation_id: "op_prompt".into(),
            turn_id: "turn_prompt".into(),
            summary: "runtime summary".into(),
            first_kept_message_id: "msg_prompt".into(),
            tokens_before: 120,
        }),
        CodingAgentProductEventKind::Session(CodingAgentSessionProductEvent::CompactionCompleted {
            operation_id: "op_compact".into(),
            turn_id: "turn_compact".into(),
            summary: "manual summary".into(),
            first_kept_message_id: "msg_prompt".into(),
            tokens_before: 100,
        }),
        CodingAgentProductEventKind::Workflow(
            CodingAgentWorkflowProductEvent::SelfHealingEditStarted {
                operation_id: "op_edit".into(),
                path: "src/app.txt".into(),
                replacements: 1,
            },
        ),
        CodingAgentProductEventKind::Workflow(
            CodingAgentWorkflowProductEvent::SelfHealingEditRepairAttempted {
                operation_id: "op_edit".into(),
                path: "src/app.txt".into(),
                attempt: 1,
                replacements: vec![product_replacement(SelfHealingEditReplacement::new(
                    "old", "new",
                ))],
                diagnostics: vec![product_diagnostic(SelfHealingEditDiagnostic {
                    message: "fixed".into(),
                })],
                check_output: Some(product_check_output(check_output.clone())),
            },
        ),
        CodingAgentProductEventKind::Workflow(
            CodingAgentWorkflowProductEvent::SelfHealingEditCompleted {
                operation_id: "op_edit".into(),
                path: "src/app.txt".into(),
                attempts: 2,
                first_changed_line: Some(2),
                check_output: Some(product_check_output(check_output)),
            },
        ),
        CodingAgentProductEventKind::Delegation(CodingAgentDelegationProductEvent::Requested {
            context: CodingAgentDelegationEventContext {
                operation_id: "op_parent".into(),
                turn_id: "turn_parent".into(),
                tool_call_id: "tool_delegate".into(),
                requesting_profile_id: "planner".into(),
                target_kind: CodingAgentProductEventProfileKind::Agent,
                target_id: "coder".into(),
                task: "implement parser".into(),
            },
        }),
    ]
    .into_iter()
    .flat_map(|event| adapter.push_product_event(&product_event(event)))
    .map(|event| serde_json::to_value(event).unwrap())
    .collect::<Vec<_>>();

    assert!(!events.is_empty());
    for event in events {
        assert_no_flow_node_fields(&event);
    }
}

#[test]
fn coding_event_adapter_maps_agent_invocation_lifecycle_to_protocol_events() {
    let mut adapter = CodingProtocolEventAdapter::new_with_provider(
        "faux".into(),
        "faux-provider".into(),
        "faux-model".into(),
    );

    let events = [
        CodingAgentProductEventKind::Agent(CodingAgentAgentProductEvent::InvocationStarted {
            operation_id: "op_parent".into(),
            child_operation_id: "op_child".into(),
            profile_id: "coder".into(),
            task: "do work".into(),
        }),
        CodingAgentProductEventKind::Agent(CodingAgentAgentProductEvent::InvocationCompleted {
            operation_id: "op_parent".into(),
            child_operation_id: "op_child".into(),
            profile_id: "coder".into(),
            final_text: "done".into(),
        }),
    ]
    .into_iter()
    .flat_map(|event| adapter.push_product_event(&product_event(event)))
    .map(|event| serde_json::to_value(event).unwrap())
    .collect::<Vec<_>>();

    assert_eq!(
        events,
        vec![
            json!({
                "type": "agent_invocation_start",
                "operationId": "op_parent",
                "childOperationId": "op_child",
                "profileId": "coder",
                "task": "do work"
            }),
            json!({
                "type": "agent_invocation_end",
                "operationId": "op_parent",
                "childOperationId": "op_child",
                "profileId": "coder",
                "finalText": "done"
            })
        ]
    );
}

#[test]
fn coding_event_adapter_maps_agent_team_lifecycle_to_protocol_events() {
    let mut adapter = CodingProtocolEventAdapter::new_with_provider(
        "faux".into(),
        "faux-provider".into(),
        "faux-model".into(),
    );

    let events = [
        CodingAgentProductEventKind::Team(CodingAgentTeamProductEvent::Started {
            operation_id: "op_team".into(),
            team_id: "implementation".into(),
            task: "ship feature".into(),
        }),
        CodingAgentProductEventKind::Team(CodingAgentTeamProductEvent::MemberStarted {
            operation_id: "op_team".into(),
            child_operation_id: "op_member".into(),
            team_id: "implementation".into(),
            profile_id: "coder".into(),
            task: "ship feature".into(),
        }),
        CodingAgentProductEventKind::Team(CodingAgentTeamProductEvent::MemberCompleted {
            operation_id: "op_team".into(),
            child_operation_id: "op_member".into(),
            team_id: "implementation".into(),
            profile_id: "coder".into(),
            final_text: "member done".into(),
        }),
        CodingAgentProductEventKind::Team(CodingAgentTeamProductEvent::Completed {
            operation_id: "op_team".into(),
            team_id: "implementation".into(),
            final_text: "team done".into(),
        }),
    ]
    .into_iter()
    .flat_map(|event| adapter.push_product_event(&product_event(event)))
    .map(|event| serde_json::to_value(event).unwrap())
    .collect::<Vec<_>>();

    assert_eq!(
        events,
        vec![
            json!({
                "type": "agent_team_start",
                "operationId": "op_team",
                "teamId": "implementation",
                "task": "ship feature"
            }),
            json!({
                "type": "agent_team_member_start",
                "operationId": "op_team",
                "childOperationId": "op_member",
                "teamId": "implementation",
                "profileId": "coder",
                "task": "ship feature"
            }),
            json!({
                "type": "agent_team_member_end",
                "operationId": "op_team",
                "childOperationId": "op_member",
                "teamId": "implementation",
                "profileId": "coder",
                "finalText": "member done"
            }),
            json!({
                "type": "agent_team_end",
                "operationId": "op_team",
                "teamId": "implementation",
                "finalText": "team done"
            })
        ]
    );
}

#[test]
fn coding_event_adapter_maps_delegation_lifecycle_to_protocol_events() {
    let mut adapter = CodingProtocolEventAdapter::new_with_provider(
        "faux".into(),
        "faux-provider".into(),
        "faux-model".into(),
    );

    let events = [
        CodingAgentProductEventKind::Delegation(CodingAgentDelegationProductEvent::Requested {
            context: CodingAgentDelegationEventContext {
                operation_id: "op_parent".into(),
                turn_id: "turn_parent".into(),
                tool_call_id: "tool_delegate".into(),
                requesting_profile_id: "planner".into(),
                target_kind: CodingAgentProductEventProfileKind::Agent,
                target_id: "coder".into(),
                task: "implement parser".into(),
            },
        }),
        CodingAgentProductEventKind::Delegation(CodingAgentDelegationProductEvent::Rejected {
            context: CodingAgentDelegationEventContext {
                operation_id: "op_parent".into(),
                turn_id: "turn_parent".into(),
                tool_call_id: "tool_delegate_team".into(),
                requesting_profile_id: "planner".into(),
                target_kind: CodingAgentProductEventProfileKind::Team,
                target_id: "review-team".into(),
                task: "review parser".into(),
            },
            reason: "delegation target is not allowed".into(),
        }),
        CodingAgentProductEventKind::Delegation(CodingAgentDelegationProductEvent::Approved {
            context: CodingAgentDelegationEventContext {
                operation_id: "op_parent".into(),
                turn_id: "turn_parent".into(),
                tool_call_id: "tool_delegate".into(),
                requesting_profile_id: "planner".into(),
                target_kind: CodingAgentProductEventProfileKind::Agent,
                target_id: "coder".into(),
                task: "implement parser".into(),
            },
        }),
        CodingAgentProductEventKind::Delegation(
            CodingAgentDelegationProductEvent::ConfirmationRequired {
                context: CodingAgentDelegationEventContext {
                    operation_id: "op_parent".into(),
                    turn_id: "turn_parent".into(),
                    tool_call_id: "tool_delegate_team".into(),
                    requesting_profile_id: "planner".into(),
                    target_kind: CodingAgentProductEventProfileKind::Team,
                    target_id: "review-team".into(),
                    task: "review parser".into(),
                },
                reason: "team delegation requires confirmation under writes policy".into(),
            },
        ),
        CodingAgentProductEventKind::Delegation(CodingAgentDelegationProductEvent::Started {
            context: CodingAgentDelegationEventContext {
                operation_id: "op_parent".into(),
                turn_id: "turn_parent".into(),
                tool_call_id: "tool_delegate".into(),
                requesting_profile_id: "planner".into(),
                target_kind: CodingAgentProductEventProfileKind::Agent,
                target_id: "coder".into(),
                task: "implement parser".into(),
            },
            child_operation_id: "op_child".into(),
        }),
        CodingAgentProductEventKind::Delegation(CodingAgentDelegationProductEvent::Completed {
            context: CodingAgentDelegationEventContext {
                operation_id: "op_parent".into(),
                turn_id: "turn_parent".into(),
                tool_call_id: "tool_delegate".into(),
                requesting_profile_id: "planner".into(),
                target_kind: CodingAgentProductEventProfileKind::Agent,
                target_id: "coder".into(),
                task: "implement parser".into(),
            },
            child_operation_id: "op_child".into(),
            final_text: "child result".into(),
        }),
        CodingAgentProductEventKind::Delegation(CodingAgentDelegationProductEvent::Failed {
            context: CodingAgentDelegationEventContext {
                operation_id: "op_parent".into(),
                turn_id: "turn_parent".into(),
                tool_call_id: "tool_delegate_failed".into(),
                requesting_profile_id: "planner".into(),
                target_kind: CodingAgentProductEventProfileKind::Agent,
                target_id: "missing-coder".into(),
                task: "implement parser".into(),
            },
            child_operation_id: "op_child_failed".into(),
            error: product_error(
                CodingAgentErrorCategory::Input,
                "input",
                false,
                "The request is invalid.",
            ),
        }),
    ]
    .into_iter()
    .flat_map(|event| adapter.push_product_event(&product_event(event)))
    .map(|event| serde_json::to_value(event).unwrap())
    .collect::<Vec<_>>();

    assert_eq!(
        events,
        vec![
            json!({
                "type": "delegation_requested",
                "operationId": "op_parent",
                "turnId": "turn_parent",
                "toolCallId": "tool_delegate",
                "requestingProfileId": "planner",
                "targetKind": "agent",
                "targetId": "coder",
                "task": "implement parser",
                "foldedBlock": {
                    "toolCallId": "tool_delegate",
                    "targetKind": "agent",
                    "targetId": "coder",
                    "task": "implement parser",
                    "status": "requested",
                    "summary": "requested",
                    "isError": false
                }
            }),
            json!({
                "type": "delegation_rejected",
                "operationId": "op_parent",
                "turnId": "turn_parent",
                "toolCallId": "tool_delegate_team",
                "requestingProfileId": "planner",
                "targetKind": "team",
                "targetId": "review-team",
                "task": "review parser",
                "reason": "delegation target is not allowed",
                "foldedBlock": {
                    "toolCallId": "tool_delegate_team",
                    "targetKind": "team",
                    "targetId": "review-team",
                    "task": "review parser",
                    "status": "rejected",
                    "summary": "rejected: delegation target is not allowed",
                    "isError": true
                }
            }),
            json!({
                "type": "delegation_approved",
                "operationId": "op_parent",
                "turnId": "turn_parent",
                "toolCallId": "tool_delegate",
                "requestingProfileId": "planner",
                "targetKind": "agent",
                "targetId": "coder",
                "task": "implement parser",
                "foldedBlock": {
                    "toolCallId": "tool_delegate",
                    "targetKind": "agent",
                    "targetId": "coder",
                    "task": "implement parser",
                    "status": "approved",
                    "summary": "approved",
                    "isError": false
                }
            }),
            json!({
                "type": "delegation_confirmation_required",
                "operationId": "op_parent",
                "turnId": "turn_parent",
                "toolCallId": "tool_delegate_team",
                "requestingProfileId": "planner",
                "targetKind": "team",
                "targetId": "review-team",
                "task": "review parser",
                "reason": "team delegation requires confirmation under writes policy",
                "foldedBlock": {
                    "toolCallId": "tool_delegate_team",
                    "targetKind": "team",
                    "targetId": "review-team",
                    "task": "review parser",
                    "status": "confirmation_required",
                    "summary": "confirmation required: team delegation requires confirmation under writes policy",
                    "isError": false
                }
            }),
            json!({
                "type": "delegation_started",
                "operationId": "op_parent",
                "turnId": "turn_parent",
                "toolCallId": "tool_delegate",
                "requestingProfileId": "planner",
                "targetKind": "agent",
                "targetId": "coder",
                "task": "implement parser",
                "childOperationId": "op_child",
                "foldedBlock": {
                    "toolCallId": "tool_delegate",
                    "targetKind": "agent",
                    "targetId": "coder",
                    "task": "implement parser",
                    "status": "running",
                    "childOperationId": "op_child",
                    "summary": "running",
                    "isError": false
                }
            }),
            json!({
                "type": "delegation_completed",
                "operationId": "op_parent",
                "turnId": "turn_parent",
                "toolCallId": "tool_delegate",
                "requestingProfileId": "planner",
                "targetKind": "agent",
                "targetId": "coder",
                "task": "implement parser",
                "childOperationId": "op_child",
                "finalText": "child result",
                "foldedBlock": {
                    "toolCallId": "tool_delegate",
                    "targetKind": "agent",
                    "targetId": "coder",
                    "task": "implement parser",
                    "status": "completed",
                    "childOperationId": "op_child",
                    "summary": "completed: child result",
                    "isError": false
                }
            }),
            json!({
                "type": "delegation_failed",
                "operationId": "op_parent",
                "turnId": "turn_parent",
                "toolCallId": "tool_delegate_failed",
                "requestingProfileId": "planner",
                "targetKind": "agent",
                "targetId": "missing-coder",
                "task": "implement parser",
                "childOperationId": "op_child_failed",
                "error": "The request is invalid.",
                "foldedBlock": {
                    "toolCallId": "tool_delegate_failed",
                    "targetKind": "agent",
                    "targetId": "missing-coder",
                    "task": "implement parser",
                    "status": "failed",
                    "childOperationId": "op_child_failed",
                    "summary": "failed: The request is invalid.",
                    "isError": true
                }
            })
        ]
    );
}

#[test]
fn coding_event_adapter_maps_self_healing_edit_lifecycle_to_protocol_events() {
    let mut adapter = CodingProtocolEventAdapter::new_with_provider(
        "faux".into(),
        "faux-provider".into(),
        "faux-model".into(),
    );

    let check_output = SelfHealingEditCheckOutput {
        command: "cargo check".into(),
        stdout: "fixed".into(),
        stderr: String::new(),
        exit_code: 0,
    };
    let events = [
        CodingAgentProductEventKind::Workflow(
            CodingAgentWorkflowProductEvent::SelfHealingEditStarted {
                operation_id: "op_edit".into(),
                path: "src/app.txt".into(),
                replacements: 1,
            },
        ),
        CodingAgentProductEventKind::Workflow(
            CodingAgentWorkflowProductEvent::SelfHealingEditRepairAttempted {
                operation_id: "op_edit".into(),
                path: "src/app.txt".into(),
                attempt: 1,
                replacements: vec![product_replacement(SelfHealingEditReplacement::new(
                    "deux", "dos",
                ))],
                diagnostics: vec![product_diagnostic(SelfHealingEditDiagnostic {
                    message: "compile error".into(),
                })],
                check_output: Some(product_check_output(check_output.clone())),
            },
        ),
        CodingAgentProductEventKind::Workflow(
            CodingAgentWorkflowProductEvent::SelfHealingEditCompleted {
                operation_id: "op_edit".into(),
                path: "src/app.txt".into(),
                attempts: 2,
                first_changed_line: Some(2),
                check_output: Some(product_check_output(check_output)),
            },
        ),
        CodingAgentProductEventKind::Workflow(
            CodingAgentWorkflowProductEvent::SelfHealingEditFailed {
                operation_id: "op_edit_failed".into(),
                path: "src/bad.txt".into(),
                error: product_error(
                    CodingAgentErrorCategory::Input,
                    "input",
                    false,
                    "The request is invalid.",
                ),
            },
        ),
    ]
    .into_iter()
    .flat_map(|event| adapter.push_product_event(&product_event(event)))
    .map(|event| serde_json::to_value(event).unwrap())
    .collect::<Vec<_>>();

    assert_eq!(
        events,
        vec![
            json!({
                "type": "self_healing_edit_start",
                "operationId": "op_edit",
                "path": "src/app.txt",
                "replacements": 1
            }),
            json!({
                "type": "self_healing_edit_repair_attempt",
                "operationId": "op_edit",
                "path": "src/app.txt",
                "attempt": 1,
                "edits": [{"oldText": "deux", "newText": "dos"}],
                "diagnostics": ["compile error"],
                "checkOutput": {
                    "command": "cargo check",
                    "stdout": "fixed",
                    "stderr": "",
                    "exitCode": 0
                }
            }),
            json!({
                "type": "self_healing_edit_end",
                "operationId": "op_edit",
                "path": "src/app.txt",
                "attempts": 2,
                "firstChangedLine": 2,
                "checkOutput": {
                    "command": "cargo check",
                    "stdout": "fixed",
                    "stderr": "",
                    "exitCode": 0
                }
            }),
            json!({
                "type": "self_healing_edit_error",
                "operationId": "op_edit_failed",
                "path": "src/bad.txt",
                "error": "The request is invalid."
            })
        ]
    );
}

#[test]
fn coding_event_adapter_maps_tool_events_to_protocol_events() {
    let mut adapter = CodingProtocolEventAdapter::new_with_provider(
        "faux".into(),
        "faux-provider".into(),
        "faux-model".into(),
    );

    let start = adapter.push_product_event(&product_event(CodingAgentProductEventKind::Tool(
        CodingAgentToolProductEvent::Started {
            operation_id: "op_1".into(),
            turn_id: "turn_1".into(),
            tool_call_id: "tool_1".into(),
            name: "read".into(),
            arguments_json: r#"{"path":"Cargo.toml"}"#.into(),
        },
    )));
    let start = protocol_json(&start[0]);
    assert_eq!(start["type"], "tool_execution_start");
    assert_eq!(start["toolCallId"], "tool_1");
    assert_eq!(start["toolName"], "read");
    assert_eq!(start["args"], json!({"path": "Cargo.toml"}));

    let invalid = adapter.push_product_event(&product_event(CodingAgentProductEventKind::Tool(
        CodingAgentToolProductEvent::Started {
            operation_id: "op_1".into(),
            turn_id: "turn_1".into(),
            tool_call_id: "tool_invalid".into(),
            name: "read".into(),
            arguments_json: "not-json".into(),
        },
    )));
    let invalid = protocol_json(&invalid[0]);
    assert_eq!(invalid["type"], "tool_execution_start");
    assert_eq!(invalid["args"], Value::Null);

    let update = adapter.push_product_event(&product_event(CodingAgentProductEventKind::Tool(
        CodingAgentToolProductEvent::Updated {
            operation_id: "op_1".into(),
            turn_id: "turn_1".into(),
            tool_call_id: "tool_1".into(),
            name: "read".into(),
            message: "reading".into(),
        },
    )));
    assert_eq!(protocol_json(&update[0])["type"], "tool_execution_update");

    let completed = adapter.push_product_event(&product_event(CodingAgentProductEventKind::Tool(
        CodingAgentToolProductEvent::Completed {
            operation_id: "op_1".into(),
            turn_id: "turn_1".into(),
            tool_call_id: "tool_1".into(),
            name: "read".into(),
            summary: "file".into(),
        },
    )));
    let completed = protocol_json(&completed[0]);
    assert_eq!(completed["type"], "tool_execution_end");
    assert_eq!(completed["isError"], false);
}

#[test]
fn coding_event_adapter_maps_session_compaction_as_manual_protocol_events() {
    let mut adapter = CodingProtocolEventAdapter::new_with_provider(
        "faux".into(),
        "faux-provider".into(),
        "faux-model".into(),
    );

    let events = adapter.push_product_event(&product_event(CodingAgentProductEventKind::Session(
        CodingAgentSessionProductEvent::CompactionCompleted {
            operation_id: "op_1".into(),
            turn_id: "turn_1".into(),
            summary: "manual summary".into(),
            first_kept_message_id: "msg_2".into(),
            tokens_before: 1200,
        },
    )));

    assert_eq!(
        serde_json::to_value(events).unwrap(),
        json!([
            {
                "type": "compaction_start",
                "reason": "manual"
            },
            {
                "type": "compaction_end",
                "reason": "manual",
                "result": {
                    "summary": "manual summary",
                    "firstKeptMessageId": "msg_2",
                    "tokensBefore": 1200,
                    "details": null
                },
                "aborted": false,
                "willRetry": false
            }
        ])
    );
}

#[test]
fn coding_event_adapter_maps_prompt_failure_with_provider() {
    let mut adapter = CodingProtocolEventAdapter::new_with_provider(
        "faux".into(),
        "faux-provider".into(),
        "faux-model".into(),
    );

    let events = adapter.push_product_event(&product_event(CodingAgentProductEventKind::Workflow(
        CodingAgentWorkflowProductEvent::PromptFailed {
            operation_id: "op_1".into(),
            error: product_error(
                CodingAgentErrorCategory::Provider,
                "provider",
                true,
                "The model provider request failed.",
            ),
        },
    )));

    let first = protocol_json(&events[0]);
    assert_eq!(first["type"], "message_start");
    assert_eq!(first["message"]["provider"], "faux-provider");
    assert_eq!(first["message"]["stopReason"], "error");
    assert_eq!(
        first["message"]["errorMessage"],
        "The model provider request failed."
    );
    assert!(events.iter().map(protocol_json).any(|event| {
        event["type"] == "turn_end"
            && event["message"]["provider"] == "faux-provider"
            && event["message"]["stopReason"] == "error"
    }));
}

#[test]
fn coding_event_adapter_maps_capability_changed_to_payloaded_protocol_event() {
    let mut adapter = CodingProtocolEventAdapter::new_with_provider(
        "faux".into(),
        "faux-provider".into(),
        "faux-model".into(),
    );

    let events = adapter.push_product_event(&product_event(
        CodingAgentProductEventKind::Capability(CodingAgentCapabilityProductEvent::Changed {
            generation: 7,
            revocation: CodingAgentProductEventCapabilityRevocation::RequestCancelOlderOperations,
            cancellation_requested_operation_ids: Vec::new(),
        }),
    ));

    assert_eq!(
        serde_json::to_value(events).unwrap(),
        json!([{
            "type": "capability_changed",
            "generation": 7,
            "revocation": "request_cancel_older_operations"
        }])
    );
}
