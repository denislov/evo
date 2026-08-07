use super::*;

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
            )
            | CodingAgentProductEventKind::Merge(_)
            | CodingAgentProductEventKind::Review(_) => Vec::new(),
            CodingAgentProductEventKind::Session(CodingAgentSessionProductEvent::WriteFailed {
                operation_id,
                reason,
                status,
                ..
            }) => vec![ProtocolEventPayload::SessionWriteFailed {
                operation_id: operation_id.clone(),
                status: match status {
                    CodingAgentSessionWriteFailureStatus::Definite => "definite",
                    CodingAgentSessionWriteFailureStatus::Uncertain => "uncertain",
                }
                .into(),
                reason: reason.clone(),
            }],
            CodingAgentProductEventKind::BackgroundTask(_) => Vec::new(),
        }
    }
}
