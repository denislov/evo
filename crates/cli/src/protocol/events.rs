use crate::protocol::types::{
    CompactionProtocolResult, CompactionReason, ProtocolDelegationFoldedBlock, ProtocolEvent,
    ProtocolEventPayload, ProtocolSelfHealingEditCheckOutput, ProtocolSelfHealingEditReplacement,
    ToolExecutionResult,
};
use coding_agent::api::event::{
    CodingAgentAgentProductEvent, CodingAgentCapabilityProductEvent,
    CodingAgentDelegationEventContext, CodingAgentDelegationProductEvent,
    CodingAgentDiagnosticProductEvent, CodingAgentMessageProductEvent, CodingAgentProductEvent,
    CodingAgentProductEventCapabilityRevocation, CodingAgentProductEventCheckOutput,
    CodingAgentProductEventKind, CodingAgentProductEventProfileKind,
    CodingAgentProductEventReplacement, CodingAgentProfileProductEvent,
    CodingAgentRecoveryResolution, CodingAgentRuntimeProductEvent, CodingAgentSessionProductEvent,
    CodingAgentSessionWriteFailureStatus, CodingAgentTeamProductEvent, CodingAgentToolProductEvent,
    CodingAgentWorkflowProductEvent,
};
use serde::Serialize;

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(tag = "type")]
enum WireContentBlock {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "thinking")]
    Thinking { thinking: String },
    #[serde(rename = "image")]
    Image {
        data: String,
        #[serde(rename = "mimeType")]
        mime_type: String,
    },
}

#[derive(Debug, Clone, Serialize, PartialEq)]
struct WireUsageCost {
    #[serde(skip_serializing_if = "wire_is_true")]
    known: bool,
    input: f64,
    output: f64,
    #[serde(rename = "cacheRead")]
    cache_read: f64,
    #[serde(rename = "cacheWrite")]
    cache_write: f64,
}

impl Default for WireUsageCost {
    fn default() -> Self {
        Self {
            known: true,
            input: 0.0,
            output: 0.0,
            cache_read: 0.0,
            cache_write: 0.0,
        }
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Default)]
struct WireProviderUsage {
    input: u32,
    output: u32,
    #[serde(rename = "cacheRead")]
    cache_read: u32,
    #[serde(rename = "cacheWrite")]
    cache_write: u32,
    #[serde(rename = "totalTokens")]
    total_tokens: u32,
    cost: WireUsageCost,
}

#[derive(Debug, Clone, Serialize, PartialEq, Default)]
struct WireStoredUsage {
    input: u32,
    output: u32,
    #[serde(rename = "cacheRead")]
    cache_read: u32,
    #[serde(rename = "cacheWrite")]
    cache_write: u32,
    total: u32,
    cost: WireUsageCost,
}

const fn wire_is_true(value: &bool) -> bool {
    *value
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum WireStopReason {
    Stop,
    Error,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
struct WireAssistantMessage {
    content: Vec<WireContentBlock>,
    api: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    provider: Option<String>,
    model: String,
    #[serde(rename = "responseModel", skip_serializing_if = "Option::is_none")]
    response_model: Option<String>,
    #[serde(rename = "responseId", skip_serializing_if = "Option::is_none")]
    response_id: Option<String>,
    usage: WireProviderUsage,
    #[serde(rename = "stopReason")]
    stop_reason: WireStopReason,
    #[serde(rename = "errorMessage", skip_serializing_if = "Option::is_none")]
    error_message: Option<String>,
    timestamp: u64,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(tag = "role")]
enum WireStoredAgentMessage {
    #[serde(rename = "assistant")]
    Assistant {
        content: Vec<WireContentBlock>,
        api: String,
        provider: String,
        model: String,
        #[serde(rename = "responseModel", skip_serializing_if = "Option::is_none")]
        response_model: Option<String>,
        #[serde(rename = "responseId", skip_serializing_if = "Option::is_none")]
        response_id: Option<String>,
        usage: WireStoredUsage,
        #[serde(rename = "stopReason")]
        stop_reason: WireStopReason,
        #[serde(rename = "errorMessage", skip_serializing_if = "Option::is_none")]
        error_message: Option<String>,
        timestamp: u64,
    },
    #[serde(rename = "toolResult")]
    ToolResult {
        #[serde(rename = "toolCallId")]
        tool_call_id: String,
        #[serde(rename = "toolName")]
        tool_name: String,
        content: Vec<WireContentBlock>,
        #[serde(rename = "isError")]
        is_error: bool,
        timestamp: u64,
    },
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(tag = "type")]
enum WireAssistantMessageEvent {
    #[serde(rename = "text_delta")]
    TextDelta {
        #[serde(rename = "contentIndex")]
        content_index: u32,
        delta: String,
        partial: WireAssistantMessage,
    },
    #[serde(rename = "thinking_delta")]
    ThinkingDelta {
        #[serde(rename = "contentIndex")]
        content_index: u32,
        delta: String,
        partial: WireAssistantMessage,
    },
}

pub struct CodingProtocolEventAdapter {
    api: String,
    provider: String,
    model: String,
    messages: Vec<WireStoredAgentMessage>,
    current_assistant: Option<WireAssistantMessage>,
    current_tool_results: Vec<WireStoredAgentMessage>,
    assistant_open: bool,
}

impl CodingProtocolEventAdapter {
    pub fn new_with_provider(api: String, provider: String, model: String) -> Self {
        Self {
            api,
            provider,
            model,
            messages: Vec::new(),
            current_assistant: None,
            current_tool_results: Vec::new(),
            assistant_open: false,
        }
    }

    pub fn push_product_event(&mut self, event: &CodingAgentProductEvent) -> Vec<ProtocolEvent> {
        self.push_typed(event.event())
            .into_iter()
            .map(ProtocolEvent::from)
            .collect()
    }

    pub fn push_prompt_failure(&mut self, message: &str) -> Vec<ProtocolEvent> {
        self.push_prompt_failed_message(message)
            .into_iter()
            .map(ProtocolEvent::from)
            .collect()
    }

    fn push_typed(&mut self, event: &CodingAgentProductEventKind) -> Vec<ProtocolEventPayload> {
        match event {
            CodingAgentProductEventKind::Agent(CodingAgentAgentProductEvent::TurnStarted {
                ..
            }) => {
                let mut events = self.finish_current_turn();
                events.push(ProtocolEventPayload::TurnStart);
                events
            }
            CodingAgentProductEventKind::Agent(
                CodingAgentAgentProductEvent::ProviderRequestStarted {
                    provider, model, ..
                },
            ) => {
                self.provider = provider.clone();
                self.model = model.clone();
                Vec::new()
            }
            CodingAgentProductEventKind::Message(CodingAgentMessageProductEvent::Started {
                ..
            }) => {
                if self.assistant_open {
                    return Vec::new();
                }
                let message = self.ensure_assistant();
                self.assistant_open = true;
                vec![ProtocolEventPayload::MessageStart {
                    message: wire_value(stored_assistant(&message)),
                }]
            }
            CodingAgentProductEventKind::Message(CodingAgentMessageProductEvent::Delta {
                text,
                ..
            }) => {
                let (content_index, message) = self.append_assistant_text(text);
                let mut events = Vec::new();
                if !self.assistant_open {
                    self.assistant_open = true;
                    events.push(ProtocolEventPayload::MessageStart {
                        message: wire_value(stored_assistant(&message)),
                    });
                }
                events.push(ProtocolEventPayload::MessageUpdate {
                    message: wire_value(stored_assistant(&message)),
                    assistant_message_event: wire_value(WireAssistantMessageEvent::TextDelta {
                        content_index,
                        delta: text.clone(),
                        partial: message,
                    }),
                });
                events
            }
            CodingAgentProductEventKind::Message(
                CodingAgentMessageProductEvent::ThinkingDelta { text, .. },
            ) => {
                let (content_index, message) = self.append_assistant_thinking(text);
                let mut events = Vec::new();
                if !self.assistant_open {
                    self.assistant_open = true;
                    events.push(ProtocolEventPayload::MessageStart {
                        message: wire_value(stored_assistant(&message)),
                    });
                }
                events.push(ProtocolEventPayload::MessageUpdate {
                    message: wire_value(stored_assistant(&message)),
                    assistant_message_event: wire_value(WireAssistantMessageEvent::ThinkingDelta {
                        content_index,
                        delta: text.clone(),
                        partial: message,
                    }),
                });
                events
            }
            CodingAgentProductEventKind::Message(CodingAgentMessageProductEvent::Completed {
                final_text,
                images,
                ..
            }) => {
                let mut message = self.ensure_assistant();
                if message.content.is_empty() && !final_text.is_empty() {
                    message.content = text_content(final_text);
                }
                message
                    .content
                    .extend(images.iter().map(|image| WireContentBlock::Image {
                        data: image.data.clone(),
                        mime_type: image.mime_type.clone(),
                    }));
                let mut events = Vec::new();
                if !self.assistant_open {
                    self.assistant_open = true;
                    events.push(ProtocolEventPayload::MessageStart {
                        message: wire_value(stored_assistant(&message)),
                    });
                }
                self.current_assistant = Some(message);
                events
            }
            CodingAgentProductEventKind::Tool(CodingAgentToolProductEvent::Started {
                tool_call_id,
                name,
                arguments_json,
                ..
            }) => vec![ProtocolEventPayload::ToolExecutionStart {
                tool_call_id: tool_call_id.clone(),
                tool_name: name.clone(),
                args: serde_json::from_str(arguments_json).unwrap_or(serde_json::Value::Null),
            }],
            CodingAgentProductEventKind::Tool(
                CodingAgentToolProductEvent::AuthorizationRequired { request },
            ) => vec![ProtocolEventPayload::ToolAuthorizationRequired {
                request: request.clone(),
            }],
            CodingAgentProductEventKind::Tool(
                CodingAgentToolProductEvent::AuthorizationApproved {
                    authorization_id,
                    operation_id,
                    tool_call_id,
                    decision,
                },
            ) => vec![ProtocolEventPayload::ToolAuthorizationApproved {
                authorization_id: authorization_id.clone(),
                operation_id: operation_id.clone(),
                tool_call_id: tool_call_id.clone(),
                decision: decision.clone(),
            }],
            CodingAgentProductEventKind::Tool(
                CodingAgentToolProductEvent::AuthorizationDenied {
                    authorization_id,
                    operation_id,
                    tool_call_id,
                    reason,
                },
            ) => vec![ProtocolEventPayload::ToolAuthorizationDenied {
                authorization_id: authorization_id.clone(),
                operation_id: operation_id.clone(),
                tool_call_id: tool_call_id.clone(),
                reason: reason.clone(),
            }],
            CodingAgentProductEventKind::Tool(
                CodingAgentToolProductEvent::AuthorizationCancelled {
                    authorization_id,
                    operation_id,
                    tool_call_id,
                    reason,
                },
            ) => vec![ProtocolEventPayload::ToolAuthorizationCancelled {
                authorization_id: authorization_id.clone(),
                operation_id: operation_id.clone(),
                tool_call_id: tool_call_id.clone(),
                reason: reason.clone(),
            }],
            CodingAgentProductEventKind::Tool(CodingAgentToolProductEvent::Updated {
                tool_call_id,
                name,
                message,
                ..
            }) => vec![ProtocolEventPayload::ToolExecutionUpdate {
                tool_call_id: tool_call_id.clone(),
                tool_name: name.clone(),
                result: ToolExecutionResult::new(wire_values(text_content(message)), false, None),
            }],
            CodingAgentProductEventKind::Tool(CodingAgentToolProductEvent::Completed {
                tool_call_id,
                name,
                summary,
                ..
            }) => self.push_tool_result(tool_call_id, name, summary, false),
            CodingAgentProductEventKind::Tool(CodingAgentToolProductEvent::Failed {
                tool_call_id,
                name,
                message,
                ..
            }) => self.push_tool_result(tool_call_id, name, message, true),
            CodingAgentProductEventKind::Runtime(
                CodingAgentRuntimeProductEvent::CompactionCompleted {
                    summary,
                    first_kept_message_id,
                    tokens_before,
                    ..
                },
            ) => Self::compaction_events(
                CompactionReason::Threshold,
                summary,
                first_kept_message_id,
                *tokens_before,
            ),
            CodingAgentProductEventKind::Runtime(CodingAgentRuntimeProductEvent::ShutDown) => {
                Vec::new()
            }
            CodingAgentProductEventKind::Session(
                CodingAgentSessionProductEvent::CompactionCompleted {
                    summary,
                    first_kept_message_id,
                    tokens_before,
                    ..
                },
            ) => Self::compaction_events(
                CompactionReason::Manual,
                summary,
                first_kept_message_id,
                *tokens_before,
            ),
            CodingAgentProductEventKind::Workflow(
                CodingAgentWorkflowProductEvent::PromptCompleted { .. },
            ) => {
                let mut events = self.finish_current_turn();
                events.push(ProtocolEventPayload::AgentEnd {
                    messages: wire_values(self.messages.clone()),
                });
                events
            }
            CodingAgentProductEventKind::Workflow(
                CodingAgentWorkflowProductEvent::PromptFailed { error, .. },
            ) => self.push_prompt_failed_message(&error.summary),
            CodingAgentProductEventKind::Workflow(
                CodingAgentWorkflowProductEvent::PromptAborted { reason, .. },
            ) => self.push_prompt_failed_message(reason),
            CodingAgentProductEventKind::Profile(
                CodingAgentProfileProductEvent::DefaultChanged { profile_id },
            ) => {
                vec![ProtocolEventPayload::DefaultAgentProfileChanged {
                    profile_id: profile_id.as_str().to_string(),
                }]
            }
            CodingAgentProductEventKind::Capability(
                CodingAgentCapabilityProductEvent::Changed {
                    generation,
                    revocation,
                    ..
                },
            ) => vec![ProtocolEventPayload::CapabilityChanged {
                generation: *generation,
                revocation: capability_revocation_to_protocol(*revocation).to_owned(),
            }],
            CodingAgentProductEventKind::Workflow(
                CodingAgentWorkflowProductEvent::OperationRecoveryPending {
                    operation_id,
                    recovery_id,
                    reason,
                    record_version,
                    descriptor_revision,
                    capability_generation,
                    attempt_count,
                    last_attempt_at,
                    next_attempt_at,
                },
            ) => vec![ProtocolEventPayload::OperationRecoveryPending {
                operation_id: operation_id.clone(),
                recovery_id: recovery_id.clone(),
                reason: reason.clone(),
                record_version: *record_version,
                descriptor_revision: *descriptor_revision,
                capability_generation: *capability_generation,
                attempt_count: *attempt_count,
                last_attempt_at: last_attempt_at.clone(),
                next_attempt_at: next_attempt_at.clone(),
            }],
            CodingAgentProductEventKind::Workflow(
                CodingAgentWorkflowProductEvent::OperationRecoveryResolved {
                    operation_id,
                    recovery_id,
                    resolution,
                    reason,
                    record_version,
                    descriptor_revision,
                    capability_generation,
                },
            ) => vec![ProtocolEventPayload::OperationRecoveryResolved {
                operation_id: operation_id.clone(),
                recovery_id: recovery_id.clone(),
                resolution: match resolution {
                    CodingAgentRecoveryResolution::Failed => "failed",
                    CodingAgentRecoveryResolution::Aborted => "aborted",
                }
                .into(),
                reason: reason.clone(),
                record_version: *record_version,
                descriptor_revision: *descriptor_revision,
                capability_generation: *capability_generation,
            }],
            CodingAgentProductEventKind::Workflow(
                CodingAgentWorkflowProductEvent::OperationRecovered {
                    operation_id,
                    recovery_id,
                    reason,
                },
            ) => vec![ProtocolEventPayload::OperationRecovered {
                operation_id: operation_id.clone(),
                recovery_id: recovery_id.clone(),
                reason: reason.clone(),
            }],
            CodingAgentProductEventKind::Workflow(
                CodingAgentWorkflowProductEvent::SelfHealingEditStarted {
                    operation_id,
                    path,
                    replacements,
                },
            ) => vec![ProtocolEventPayload::SelfHealingEditStart {
                operation_id: operation_id.clone(),
                path: path.clone(),
                replacements: *replacements,
            }],
            CodingAgentProductEventKind::Workflow(
                CodingAgentWorkflowProductEvent::SelfHealingEditRepairAttempted {
                    operation_id,
                    path,
                    attempt,
                    replacements,
                    diagnostics,
                    check_output,
                },
            ) => vec![ProtocolEventPayload::SelfHealingEditRepairAttempt {
                operation_id: operation_id.clone(),
                path: path.clone(),
                attempt: *attempt,
                edits: protocol_self_healing_replacements(replacements),
                diagnostics: diagnostics
                    .iter()
                    .map(|diagnostic| diagnostic.message.clone())
                    .collect(),
                check_output: check_output
                    .as_ref()
                    .map(protocol_self_healing_check_output),
            }],
            CodingAgentProductEventKind::Workflow(
                CodingAgentWorkflowProductEvent::SelfHealingEditCompleted {
                    operation_id,
                    path,
                    attempts,
                    first_changed_line,
                    check_output,
                },
            ) => vec![ProtocolEventPayload::SelfHealingEditEnd {
                operation_id: operation_id.clone(),
                path: path.clone(),
                attempts: *attempts,
                first_changed_line: *first_changed_line,
                check_output: check_output
                    .as_ref()
                    .map(protocol_self_healing_check_output),
            }],
            CodingAgentProductEventKind::Workflow(
                CodingAgentWorkflowProductEvent::SelfHealingEditFailed {
                    operation_id,
                    path,
                    error,
                },
            ) => vec![ProtocolEventPayload::SelfHealingEditError {
                operation_id: operation_id.clone(),
                path: path.clone(),
                error: error.summary.clone(),
            }],
            CodingAgentProductEventKind::Workflow(
                CodingAgentWorkflowProductEvent::SelfHealingEditAborted {
                    operation_id,
                    path,
                    reason,
                },
            ) => vec![ProtocolEventPayload::SelfHealingEditAbort {
                operation_id: operation_id.clone(),
                path: path.clone(),
                reason: reason.clone(),
            }],
            CodingAgentProductEventKind::Delegation(
                CodingAgentDelegationProductEvent::Requested {
                    context:
                        CodingAgentDelegationEventContext {
                            operation_id,
                            turn_id,
                            tool_call_id,
                            requesting_profile_id,
                            target_kind,
                            target_id,
                            task,
                        },
                },
            ) => vec![ProtocolEventPayload::DelegationRequested {
                operation_id: operation_id.clone(),
                turn_id: turn_id.clone(),
                tool_call_id: tool_call_id.clone(),
                requesting_profile_id: requesting_profile_id.as_str().to_string(),
                target_kind: profile_kind_to_protocol(*target_kind).to_string(),
                target_id: target_id.as_str().to_string(),
                task: task.clone(),
                folded_block: delegation_folded_block(
                    tool_call_id,
                    *target_kind,
                    target_id.as_str(),
                    task,
                    "requested",
                    None,
                    Some("requested".into()),
                    false,
                ),
            }],
            CodingAgentProductEventKind::Delegation(
                CodingAgentDelegationProductEvent::Rejected {
                    context:
                        CodingAgentDelegationEventContext {
                            operation_id,
                            turn_id,
                            tool_call_id,
                            requesting_profile_id,
                            target_kind,
                            target_id,
                            task,
                        },
                    reason,
                },
            ) => vec![ProtocolEventPayload::DelegationRejected {
                operation_id: operation_id.clone(),
                turn_id: turn_id.clone(),
                tool_call_id: tool_call_id.clone(),
                requesting_profile_id: requesting_profile_id.as_str().to_string(),
                target_kind: profile_kind_to_protocol(*target_kind).to_string(),
                target_id: target_id.as_str().to_string(),
                task: task.clone(),
                reason: reason.clone(),
                folded_block: delegation_folded_block(
                    tool_call_id,
                    *target_kind,
                    target_id.as_str(),
                    task,
                    "rejected",
                    None,
                    Some(format!("rejected: {reason}")),
                    true,
                ),
            }],
            CodingAgentProductEventKind::Delegation(
                CodingAgentDelegationProductEvent::Approved {
                    context:
                        CodingAgentDelegationEventContext {
                            operation_id,
                            turn_id,
                            tool_call_id,
                            requesting_profile_id,
                            target_kind,
                            target_id,
                            task,
                        },
                },
            ) => vec![ProtocolEventPayload::DelegationApproved {
                operation_id: operation_id.clone(),
                turn_id: turn_id.clone(),
                tool_call_id: tool_call_id.clone(),
                requesting_profile_id: requesting_profile_id.as_str().to_string(),
                target_kind: profile_kind_to_protocol(*target_kind).to_string(),
                target_id: target_id.as_str().to_string(),
                task: task.clone(),
                folded_block: delegation_folded_block(
                    tool_call_id,
                    *target_kind,
                    target_id.as_str(),
                    task,
                    "approved",
                    None,
                    Some("approved".into()),
                    false,
                ),
            }],
            CodingAgentProductEventKind::Delegation(
                CodingAgentDelegationProductEvent::ConfirmationRequired {
                    context:
                        CodingAgentDelegationEventContext {
                            operation_id,
                            turn_id,
                            tool_call_id,
                            requesting_profile_id,
                            target_kind,
                            target_id,
                            task,
                        },
                    reason,
                },
            ) => vec![ProtocolEventPayload::DelegationConfirmationRequired {
                operation_id: operation_id.clone(),
                turn_id: turn_id.clone(),
                tool_call_id: tool_call_id.clone(),
                requesting_profile_id: requesting_profile_id.as_str().to_string(),
                target_kind: profile_kind_to_protocol(*target_kind).to_string(),
                target_id: target_id.as_str().to_string(),
                task: task.clone(),
                reason: reason.clone(),
                folded_block: delegation_folded_block(
                    tool_call_id,
                    *target_kind,
                    target_id.as_str(),
                    task,
                    "confirmation_required",
                    None,
                    Some(format!("confirmation required: {reason}")),
                    false,
                ),
            }],
            CodingAgentProductEventKind::Delegation(
                CodingAgentDelegationProductEvent::Started {
                    context:
                        CodingAgentDelegationEventContext {
                            operation_id,
                            turn_id,
                            tool_call_id,
                            requesting_profile_id,
                            target_kind,
                            target_id,
                            task,
                        },
                    child_operation_id,
                },
            ) => vec![ProtocolEventPayload::DelegationStarted {
                operation_id: operation_id.clone(),
                turn_id: turn_id.clone(),
                tool_call_id: tool_call_id.clone(),
                requesting_profile_id: requesting_profile_id.as_str().to_string(),
                target_kind: profile_kind_to_protocol(*target_kind).to_string(),
                target_id: target_id.as_str().to_string(),
                task: task.clone(),
                child_operation_id: child_operation_id.clone(),
                folded_block: delegation_folded_block(
                    tool_call_id,
                    *target_kind,
                    target_id.as_str(),
                    task,
                    "running",
                    Some(child_operation_id.clone()),
                    Some("running".into()),
                    false,
                ),
            }],
            CodingAgentProductEventKind::Delegation(
                CodingAgentDelegationProductEvent::Completed {
                    context:
                        CodingAgentDelegationEventContext {
                            operation_id,
                            turn_id,
                            tool_call_id,
                            requesting_profile_id,
                            target_kind,
                            target_id,
                            task,
                        },
                    child_operation_id,
                    final_text,
                },
            ) => vec![ProtocolEventPayload::DelegationCompleted {
                operation_id: operation_id.clone(),
                turn_id: turn_id.clone(),
                tool_call_id: tool_call_id.clone(),
                requesting_profile_id: requesting_profile_id.as_str().to_string(),
                target_kind: profile_kind_to_protocol(*target_kind).to_string(),
                target_id: target_id.as_str().to_string(),
                task: task.clone(),
                child_operation_id: child_operation_id.clone(),
                final_text: final_text.clone(),
                folded_block: delegation_folded_block(
                    tool_call_id,
                    *target_kind,
                    target_id.as_str(),
                    task,
                    "completed",
                    Some(child_operation_id.clone()),
                    Some(format!("completed: {final_text}")),
                    false,
                ),
            }],
            CodingAgentProductEventKind::Delegation(
                CodingAgentDelegationProductEvent::Failed {
                    context:
                        CodingAgentDelegationEventContext {
                            operation_id,
                            turn_id,
                            tool_call_id,
                            requesting_profile_id,
                            target_kind,
                            target_id,
                            task,
                        },
                    child_operation_id,
                    error,
                },
            ) => vec![ProtocolEventPayload::DelegationFailed {
                operation_id: operation_id.clone(),
                turn_id: turn_id.clone(),
                tool_call_id: tool_call_id.clone(),
                requesting_profile_id: requesting_profile_id.as_str().to_string(),
                target_kind: profile_kind_to_protocol(*target_kind).to_string(),
                target_id: target_id.as_str().to_string(),
                task: task.clone(),
                child_operation_id: child_operation_id.clone(),
                error: error.summary.clone(),
                folded_block: delegation_folded_block(
                    tool_call_id,
                    *target_kind,
                    target_id.as_str(),
                    task,
                    "failed",
                    Some(child_operation_id.clone()),
                    Some(format!("failed: {}", error.summary)),
                    true,
                ),
            }],
            CodingAgentProductEventKind::Agent(
                CodingAgentAgentProductEvent::InvocationStarted {
                    operation_id,
                    child_operation_id,
                    profile_id,
                    task,
                },
            ) => vec![ProtocolEventPayload::AgentInvocationStart {
                operation_id: operation_id.clone(),
                child_operation_id: child_operation_id.clone(),
                profile_id: profile_id.as_str().to_string(),
                task: task.clone(),
            }],
            CodingAgentProductEventKind::Agent(
                CodingAgentAgentProductEvent::InvocationCompleted {
                    operation_id,
                    child_operation_id,
                    profile_id,
                    final_text,
                },
            ) => vec![ProtocolEventPayload::AgentInvocationEnd {
                operation_id: operation_id.clone(),
                child_operation_id: child_operation_id.clone(),
                profile_id: profile_id.as_str().to_string(),
                final_text: final_text.clone(),
            }],
            CodingAgentProductEventKind::Agent(
                CodingAgentAgentProductEvent::InvocationFailed {
                    operation_id,
                    child_operation_id,
                    profile_id,
                    error,
                },
            ) => vec![ProtocolEventPayload::AgentInvocationError {
                operation_id: operation_id.clone(),
                child_operation_id: child_operation_id.clone(),
                profile_id: profile_id.as_str().to_string(),
                error: error.summary.clone(),
            }],
            CodingAgentProductEventKind::Agent(
                CodingAgentAgentProductEvent::InvocationAborted {
                    operation_id,
                    child_operation_id,
                    profile_id,
                    reason,
                },
            ) => vec![ProtocolEventPayload::AgentInvocationAbort {
                operation_id: operation_id.clone(),
                child_operation_id: child_operation_id.clone(),
                profile_id: profile_id.as_str().to_string(),
                reason: reason.clone(),
            }],
            CodingAgentProductEventKind::Team(CodingAgentTeamProductEvent::Started {
                operation_id,
                team_id,
                task,
            }) => vec![ProtocolEventPayload::AgentTeamStart {
                operation_id: operation_id.clone(),
                team_id: team_id.as_str().to_string(),
                task: task.clone(),
            }],
            CodingAgentProductEventKind::Team(CodingAgentTeamProductEvent::MemberStarted {
                operation_id,
                child_operation_id,
                team_id,
                profile_id,
                task,
            }) => vec![ProtocolEventPayload::AgentTeamMemberStart {
                operation_id: operation_id.clone(),
                child_operation_id: child_operation_id.clone(),
                team_id: team_id.as_str().to_string(),
                profile_id: profile_id.as_str().to_string(),
                task: task.clone(),
            }],
            CodingAgentProductEventKind::Team(CodingAgentTeamProductEvent::MemberCompleted {
                operation_id,
                child_operation_id,
                team_id,
                profile_id,
                final_text,
            }) => vec![ProtocolEventPayload::AgentTeamMemberEnd {
                operation_id: operation_id.clone(),
                child_operation_id: child_operation_id.clone(),
                team_id: team_id.as_str().to_string(),
                profile_id: profile_id.as_str().to_string(),
                final_text: final_text.clone(),
            }],
            CodingAgentProductEventKind::Team(CodingAgentTeamProductEvent::Completed {
                operation_id,
                team_id,
                final_text,
            }) => vec![ProtocolEventPayload::AgentTeamEnd {
                operation_id: operation_id.clone(),
                team_id: team_id.as_str().to_string(),
                final_text: final_text.clone(),
            }],
            CodingAgentProductEventKind::Team(CodingAgentTeamProductEvent::Failed {
                operation_id,
                team_id,
                error,
            }) => vec![ProtocolEventPayload::AgentTeamError {
                operation_id: operation_id.clone(),
                team_id: team_id.as_str().to_string(),
                error: error.summary.clone(),
            }],
            CodingAgentProductEventKind::Team(CodingAgentTeamProductEvent::Aborted {
                operation_id,
                team_id,
                reason,
            }) => vec![ProtocolEventPayload::AgentTeamAbort {
                operation_id: operation_id.clone(),
                team_id: team_id.as_str().to_string(),
                reason: reason.clone(),
            }],
            CodingAgentProductEventKind::Session(CodingAgentSessionProductEvent::Opened {
                ..
            })
            | CodingAgentProductEventKind::Session(
                CodingAgentSessionProductEvent::WritePending { .. },
            )
            | CodingAgentProductEventKind::Session(
                CodingAgentSessionProductEvent::WriteCommitted { .. },
            )
            | CodingAgentProductEventKind::Session(
                CodingAgentSessionProductEvent::WriteSkipped { .. },
            )
            | CodingAgentProductEventKind::Workflow(
                CodingAgentWorkflowProductEvent::PromptStarted { .. },
            )
            | CodingAgentProductEventKind::Diagnostic(
                CodingAgentDiagnosticProductEvent::Diagnostic { .. },
            ) => Vec::new(),
            CodingAgentProductEventKind::Session(CodingAgentSessionProductEvent::WriteFailed {
                operation_id,
                reason,
                status,
            }) => vec![ProtocolEventPayload::SessionWriteFailed {
                operation_id: operation_id.clone(),
                status: match status {
                    CodingAgentSessionWriteFailureStatus::Definite => "definite",
                    CodingAgentSessionWriteFailureStatus::Uncertain => "uncertain",
                }
                .into(),
                reason: reason.clone(),
            }],
        }
    }

    fn ensure_assistant(&mut self) -> WireAssistantMessage {
        if self.current_assistant.is_none() {
            self.current_assistant = Some(self.assistant_message(""));
        }
        self.current_assistant
            .clone()
            .expect("assistant was inserted when missing")
    }

    fn append_assistant_text(&mut self, text: &str) -> (u32, WireAssistantMessage) {
        let mut message = self.ensure_assistant();
        let content_index = append_text_content(&mut message, text);
        self.current_assistant = Some(message.clone());
        (content_index, message)
    }

    fn append_assistant_thinking(&mut self, text: &str) -> (u32, WireAssistantMessage) {
        let mut message = self.ensure_assistant();
        let content_index = append_thinking_content(&mut message, text);
        self.current_assistant = Some(message.clone());
        (content_index, message)
    }

    fn assistant_message(&self, text: &str) -> WireAssistantMessage {
        WireAssistantMessage {
            content: text_content(text),
            api: self.api.clone(),
            provider: (!self.provider.is_empty()).then(|| self.provider.clone()),
            model: self.model.clone(),
            response_model: None,
            response_id: None,
            usage: WireProviderUsage::default(),
            stop_reason: WireStopReason::Stop,
            error_message: None,
            timestamp: 0,
        }
    }

    fn compaction_events(
        reason: CompactionReason,
        summary: &str,
        first_kept_message_id: &str,
        tokens_before: u32,
    ) -> Vec<ProtocolEventPayload> {
        vec![
            ProtocolEventPayload::CompactionStart { reason },
            ProtocolEventPayload::CompactionEnd {
                reason,
                result: Some(CompactionProtocolResult {
                    summary: summary.to_owned(),
                    first_kept_message_id: first_kept_message_id.to_owned(),
                    tokens_before,
                    details: None,
                }),
                aborted: false,
                will_retry: false,
                error_message: None,
            },
        ]
    }

    fn push_tool_result(
        &mut self,
        tool_call_id: &str,
        tool_name: &str,
        text: &str,
        is_error: bool,
    ) -> Vec<ProtocolEventPayload> {
        let content = text_content(text);
        let tool_result = WireStoredAgentMessage::ToolResult {
            tool_call_id: tool_call_id.to_string(),
            tool_name: tool_name.to_string(),
            content: content.clone(),
            is_error,
            timestamp: 0,
        };
        self.current_tool_results.push(tool_result.clone());

        vec![
            ProtocolEventPayload::ToolExecutionEnd {
                tool_call_id: tool_call_id.to_string(),
                tool_name: tool_name.to_string(),
                result: ToolExecutionResult::new(wire_values(content), false, None),
                is_error,
            },
            ProtocolEventPayload::MessageStart {
                message: wire_value(tool_result.clone()),
            },
            ProtocolEventPayload::MessageEnd {
                message: wire_value(tool_result),
            },
        ]
    }

    fn push_prompt_failed_message(&mut self, error: &str) -> Vec<ProtocolEventPayload> {
        let message = stored_error_assistant(&self.api, &self.provider, &self.model, error);
        self.messages.push(message.clone());
        vec![
            ProtocolEventPayload::MessageStart {
                message: wire_value(message.clone()),
            },
            ProtocolEventPayload::MessageEnd {
                message: wire_value(message.clone()),
            },
            ProtocolEventPayload::TurnEnd {
                message: wire_value(message),
                tool_results: Vec::new(),
            },
            ProtocolEventPayload::AgentEnd {
                messages: wire_values(self.messages.clone()),
            },
        ]
    }

    fn finish_current_turn(&mut self) -> Vec<ProtocolEventPayload> {
        let Some(message) = self.current_assistant.take() else {
            return Vec::new();
        };

        let stored = stored_assistant(&message);
        if !self.messages.contains(&stored) {
            self.messages.push(stored.clone());
        }
        for tool_result in &self.current_tool_results {
            if !self.messages.contains(tool_result) {
                self.messages.push(tool_result.clone());
            }
        }

        let events = vec![
            ProtocolEventPayload::MessageEnd {
                message: wire_value(stored.clone()),
            },
            ProtocolEventPayload::TurnEnd {
                message: wire_value(stored),
                tool_results: wire_values(self.current_tool_results.clone()),
            },
        ];
        self.current_tool_results.clear();
        self.assistant_open = false;
        events
    }
}

fn wire_value<T: serde::Serialize>(value: T) -> serde_json::Value {
    serde_json::to_value(value).expect("protocol projection value should serialize")
}

fn wire_values<T: serde::Serialize>(values: impl IntoIterator<Item = T>) -> Vec<serde_json::Value> {
    values.into_iter().map(wire_value).collect()
}

fn protocol_self_healing_replacements(
    replacements: &[CodingAgentProductEventReplacement],
) -> Vec<ProtocolSelfHealingEditReplacement> {
    replacements
        .iter()
        .map(|replacement| ProtocolSelfHealingEditReplacement {
            old_text: replacement.old_text.clone(),
            new_text: replacement.new_text.clone(),
        })
        .collect()
}

fn protocol_self_healing_check_output(
    output: &CodingAgentProductEventCheckOutput,
) -> ProtocolSelfHealingEditCheckOutput {
    ProtocolSelfHealingEditCheckOutput {
        command: output.command.clone(),
        stdout: output.stdout.clone(),
        stderr: output.stderr.clone(),
        exit_code: output.exit_code,
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "protocol projection keeps the complete typed delegation record explicit"
)]
fn delegation_folded_block(
    tool_call_id: &str,
    target_kind: CodingAgentProductEventProfileKind,
    target_id: &str,
    task: &str,
    status: &str,
    child_operation_id: Option<String>,
    summary: Option<String>,
    is_error: bool,
) -> ProtocolDelegationFoldedBlock {
    ProtocolDelegationFoldedBlock {
        tool_call_id: tool_call_id.to_string(),
        target_kind: profile_kind_to_protocol(target_kind).to_string(),
        target_id: target_id.to_string(),
        task: task.to_string(),
        status: status.to_string(),
        child_operation_id,
        summary,
        is_error,
    }
}

fn profile_kind_to_protocol(kind: CodingAgentProductEventProfileKind) -> &'static str {
    match kind {
        CodingAgentProductEventProfileKind::Agent => "agent",
        CodingAgentProductEventProfileKind::Team => "team",
    }
}

fn capability_revocation_to_protocol(
    revocation: CodingAgentProductEventCapabilityRevocation,
) -> &'static str {
    match revocation {
        CodingAgentProductEventCapabilityRevocation::FutureOnly => "future_only",
        CodingAgentProductEventCapabilityRevocation::RequestCancelOlderOperations => {
            "request_cancel_older_operations"
        }
    }
}

fn append_text_content(message: &mut WireAssistantMessage, text: &str) -> u32 {
    let last_index = message.content.len().saturating_sub(1) as u32;
    match message.content.last_mut() {
        Some(WireContentBlock::Text { text: existing }) => {
            existing.push_str(text);
            last_index
        }
        _ => {
            let index = message.content.len() as u32;
            message.content.push(WireContentBlock::Text {
                text: text.to_string(),
            });
            index
        }
    }
}

fn append_thinking_content(message: &mut WireAssistantMessage, text: &str) -> u32 {
    let last_index = message.content.len().saturating_sub(1) as u32;
    match message.content.last_mut() {
        Some(WireContentBlock::Thinking { thinking }) => {
            thinking.push_str(text);
            last_index
        }
        _ => {
            let index = message.content.len() as u32;
            message.content.push(WireContentBlock::Thinking {
                thinking: text.to_string(),
            });
            index
        }
    }
}

fn text_content(text: &str) -> Vec<WireContentBlock> {
    if text.is_empty() {
        Vec::new()
    } else {
        vec![WireContentBlock::Text {
            text: text.to_string(),
        }]
    }
}

fn stored_assistant(message: &WireAssistantMessage) -> WireStoredAgentMessage {
    WireStoredAgentMessage::Assistant {
        content: message.content.clone(),
        api: message.api.clone(),
        provider: message.provider.clone().unwrap_or_default(),
        model: message.model.clone(),
        response_model: message.response_model.clone(),
        response_id: message.response_id.clone(),
        usage: WireStoredUsage {
            input: message.usage.input,
            output: message.usage.output,
            cache_read: message.usage.cache_read,
            cache_write: message.usage.cache_write,
            total: message.usage.total_tokens,
            cost: WireUsageCost {
                known: message.usage.cost.known,
                input: message.usage.cost.input,
                output: message.usage.cost.output,
                cache_read: message.usage.cost.cache_read,
                cache_write: message.usage.cost.cache_write,
            },
        },
        stop_reason: message.stop_reason.clone(),
        error_message: message.error_message.clone(),
        timestamp: message.timestamp,
    }
}

fn stored_error_assistant(
    api: &str,
    provider: &str,
    model: &str,
    error: &str,
) -> WireStoredAgentMessage {
    WireStoredAgentMessage::Assistant {
        content: Vec::new(),
        api: api.to_string(),
        provider: provider.to_string(),
        model: model.to_string(),
        response_model: None,
        response_id: None,
        usage: WireStoredUsage::default(),
        stop_reason: WireStopReason::Error,
        error_message: Some(error.to_string()),
        timestamp: 0,
    }
}
