use super::*;

impl PromptTurnContext {
    pub(crate) fn completed_transcript_items(&self) -> Vec<TranscriptItem> {
        let mut transcript = Vec::new();

        if let Some(input) = self.prepared_input.as_deref() {
            let text = persisted_content_blocks_text(input);
            if !text.is_empty() {
                transcript.push(TranscriptItem::UserInput {
                    turn_id: self.turn_id().to_owned(),
                    text,
                    started_at: None,
                });
            }
        }

        if let Some(message) = self.final_message.as_ref() {
            let content = persisted_assistant_content_blocks(&message.content);
            if !content.is_empty() {
                transcript.push(TranscriptItem::AssistantMessage {
                    message_id: self
                        .assistant_session_message_id
                        .clone()
                        .unwrap_or_else(|| format!("msg_{}", self.turn_id())),
                    content,
                    status: MessageStatus::Completed,
                    reasoning_duration_millis: None,
                    model_id: self
                        .final_message
                        .as_ref()
                        .and_then(|message| {
                            message
                                .response_model
                                .as_deref()
                                .or(Some(message.model.as_str()))
                        })
                        .map(str::to_owned),
                    completed_at: None,
                });
            }
        }

        transcript
    }

    pub(crate) async fn record_user_input(&mut self) -> Result<(), CodingSessionError> {
        let content = self
            .prepared_input
            .clone()
            .ok_or_else(|| CodingSessionError::Session {
                message: "prompt turn input has not been prepared".into(),
            })?;
        if let Some(transaction) = self.transaction.as_mut() {
            transaction.record_user_input(content)?;
            transaction.checkpoint().await?;
        }
        Ok(())
    }

    pub(crate) fn record_diagnostic(&mut self, diagnostic: CodingDiagnostic) {
        self.diagnostics.push(diagnostic);
    }

    pub(crate) fn record_delegation_folded_update(
        &mut self,
        request: &DelegationRequest,
        status: PersistedDelegationStatus,
        child_operation_id: Option<String>,
        summary: Option<String>,
    ) -> Result<(), CodingSessionError> {
        if let Some(transaction) = self.transaction.as_mut() {
            let session_tool_call_id = self
                .tool_session_call_ids
                .get(&request.tool_call_id)
                .cloned()
                .unwrap_or_else(|| request.tool_call_id.clone());
            transaction.record_delegation_folded_update(
                session_tool_call_id,
                request.requesting_profile_id.clone(),
                request.target_kind,
                request.target_id.clone(),
                request.task.clone(),
                status,
                child_operation_id,
                summary,
            )?;
        }
        Ok(())
    }

    pub(crate) fn request_abort(&mut self, reason: impl Into<String>) {
        self.requested_abort_reason = Some(reason.into());
    }

    pub(crate) fn abort_reason(&self) -> Option<&str> {
        self.requested_abort_reason.as_deref()
    }

    pub(crate) fn record_final_message(&mut self, message: AssistantMessage) {
        self.final_message = Some(message);
    }

    pub(crate) fn final_message(&self) -> Option<&AssistantMessage> {
        self.final_message.as_ref()
    }

    pub(crate) fn record_agent_event(
        &mut self,
        event: AgentEvent,
    ) -> Result<Vec<PromptStreamEvent>, CodingSessionError> {
        self.record_agent_event_to_transaction(&event)?;
        self.reasoning_duration.observe(&event);
        let reasoning_duration_millis = if matches!(
            event,
            AgentEvent::LlmEvent(AssistantMessageEvent::Done { .. })
        ) {
            self.reasoning_duration.take_duration_millis()
        } else {
            None
        };
        let mut mapping_context = AgentEventMappingContext::new(
            self.operation_id().to_owned(),
            self.turn_id().to_owned(),
        );
        if let Some(message_id) = self
            .assistant_session_message_id
            .clone()
            .or_else(|| self.completed_assistant_session_message_id.clone())
        {
            mapping_context = mapping_context.with_assistant_message_id(message_id);
        }
        if reasoning_duration_millis.is_some() {
            mapping_context =
                mapping_context.with_reasoning_duration_millis(reasoning_duration_millis);
        }
        let coding_events = map_agent_event(&mapping_context, &event);
        self.record_delegation_requests(&coding_events);
        self.coding_events.extend(coding_events.clone());
        if let Some(event_service) = &self.live_event_service {
            for event in &coding_events {
                event_service.publish_prompt_stream_event(event.clone())?;
            }
        }
        Ok(coding_events)
    }

    pub(crate) fn coding_events(&self) -> &[PromptStreamEvent] {
        &self.coding_events
    }

    pub(crate) fn authorize_delegation_requests(
        &mut self,
        current_depth: usize,
    ) -> Result<&[DelegationAuthorizationDecision], CodingSessionError> {
        self.authorize_delegation_requests_with_lineage(current_depth, &[])
    }

    pub(crate) fn authorize_delegation_requests_with_lineage(
        &mut self,
        current_depth: usize,
        lineage: &[DelegationLineageEntry],
    ) -> Result<&[DelegationAuthorizationDecision], CodingSessionError> {
        if self.delegation_requests.is_empty() {
            self.delegation_authorization_decisions.clear();
            return Ok(&self.delegation_authorization_decisions);
        }
        let policy = self
            .runtime
            .as_ref()
            .and_then(RuntimeSnapshot::profile_delegation_policy)
            .cloned()
            .ok_or_else(|| CodingSessionError::Config {
                message: "prompt turn cannot authorize delegation without active profile policy"
                    .into(),
            })?;
        self.delegation_authorization_decisions = authorize_delegation_requests_with_lineage(
            &self.delegation_requests,
            &policy,
            current_depth,
            lineage,
        );
        Ok(&self.delegation_authorization_decisions)
    }

    pub(super) fn record_delegation_requests(&mut self, events: &[PromptStreamEvent]) {
        for event in events {
            if let PromptStreamEvent::Delegation(event) = event
                && event.is_requested()
            {
                let context = event.context();
                self.delegation_requests.push(DelegationRequest {
                    operation_id: context.operation_id.clone(),
                    turn_id: context.turn_id.clone(),
                    tool_call_id: context.tool_call_id.clone(),
                    requesting_profile_id: context.requesting_profile_id.clone(),
                    target_kind: context.target_kind,
                    target_id: context.target_id.clone(),
                    task: context.task.clone(),
                });
            }
        }
    }

    pub(crate) fn record_prompt_completed(&mut self) -> Result<(), CodingSessionError> {
        if self.final_message.is_none() {
            return Err(CodingSessionError::Session {
                message: "prompt turn cannot emit completion without a final assistant message"
                    .into(),
            });
        }

        if self.completion_recorded {
            return Ok(());
        }

        self.completion_recorded = true;
        Ok(())
    }

    pub(super) fn record_agent_event_to_transaction(
        &mut self,
        event: &AgentEvent,
    ) -> Result<(), CodingSessionError> {
        if self.transaction.is_none() {
            return Ok(());
        }

        match event {
            AgentEvent::LlmEvent(event) => self.record_assistant_event_to_transaction(event),
            AgentEvent::ToolCallStart {
                tool_call_id,
                tool_name,
                arguments,
                ..
            } => {
                self.ensure_tool_session_call_started(tool_call_id, tool_name, Some(arguments))?;
                Ok(())
            }
            AgentEvent::ToolCallUpdate {
                tool_call_id,
                tool_name,
                update,
            } => {
                let session_tool_call_id =
                    self.ensure_tool_session_call_started(tool_call_id, tool_name, None)?;
                let message = content_blocks_text(&update.content);
                self.transaction_mut_required()?
                    .record_tool_updated(session_tool_call_id, message)
            }
            AgentEvent::ToolCallEnd {
                tool_call_id,
                tool_name,
                result,
            } => self.record_tool_result_to_transaction(tool_call_id, tool_name, result),
            AgentEvent::AgentDone { .. } => Ok(()),
            AgentEvent::AgentError { error } => self
                .transaction_mut_required()?
                .emit_diagnostic(DiagnosticLevel::Error, error.clone()),
            AgentEvent::TurnStart { .. }
            | AgentEvent::BeforeProviderRequest { .. }
            | AgentEvent::SessionCompacted { .. } => Ok(()),
        }
    }

    pub(super) fn record_assistant_event_to_transaction(
        &mut self,
        event: &AssistantMessageEvent,
    ) -> Result<(), CodingSessionError> {
        match event {
            AssistantMessageEvent::Start { .. }
            | AssistantMessageEvent::TextStart { .. }
            | AssistantMessageEvent::TextDelta { .. }
            | AssistantMessageEvent::ThinkingDelta { .. }
            | AssistantMessageEvent::ToolcallStart { .. }
            | AssistantMessageEvent::ToolcallDelta { .. }
            | AssistantMessageEvent::ToolcallEnd { .. }
            | AssistantMessageEvent::ProviderItemStart { .. }
            | AssistantMessageEvent::ProviderItemDelta { .. }
            | AssistantMessageEvent::ProviderItemEnd { .. } => {
                self.ensure_assistant_session_message_started()?;
                Ok(())
            }
            AssistantMessageEvent::ThinkingStart { content_index, .. } => {
                let message_id = self.ensure_assistant_session_message_started()?;
                self.transaction_mut_required()?
                    .start_assistant_reasoning(message_id, *content_index)
            }
            AssistantMessageEvent::ThinkingEnd { content_index, .. } => {
                let message_id = self.assistant_session_message_id.clone().ok_or_else(|| {
                    CodingSessionError::Session {
                        message: "assistant reasoning ended before its message started".into(),
                    }
                })?;
                self.transaction_mut_required()?
                    .complete_assistant_reasoning(message_id, *content_index)
            }
            AssistantMessageEvent::Done { message, .. } => {
                self.complete_current_assistant_message(message)
            }
            AssistantMessageEvent::Error { message, .. } => {
                self.transaction_mut_required()?.emit_diagnostic(
                    DiagnosticLevel::Error,
                    message
                        .error_message
                        .clone()
                        .unwrap_or_else(|| "assistant stream failed".into()),
                )
            }
            AssistantMessageEvent::TextEnd { .. } => Ok(()),
        }
    }

    pub(super) fn record_tool_result_to_transaction(
        &mut self,
        agent_tool_call_id: &str,
        tool_name: &str,
        result: &AgentToolResult,
    ) -> Result<(), CodingSessionError> {
        let session_tool_call_id =
            self.ensure_tool_session_call_started(agent_tool_call_id, tool_name, None)?;
        let delegation_update = terminal_delegation_update(tool_name, &result.content);
        if result.is_error {
            self.transaction_mut_required()?.record_tool_failed(
                session_tool_call_id.clone(),
                content_blocks_text(&result.content),
            )?;
            if let Some(update) = delegation_update {
                self.transaction_mut_required()?
                    .record_delegation_folded_update(
                        session_tool_call_id,
                        update.requesting_profile_id,
                        update.target_kind,
                        update.target_id,
                        update.task,
                        update.status,
                        update.child_operation_id,
                        update.summary,
                    )?;
            }
            Ok(())
        } else {
            self.transaction_mut_required()?.record_tool_completed(
                session_tool_call_id.clone(),
                persisted_tool_result(&result.content),
            )?;
            if let Some(update) = delegation_update {
                self.transaction_mut_required()?
                    .record_delegation_folded_update(
                        session_tool_call_id,
                        update.requesting_profile_id,
                        update.target_kind,
                        update.target_id,
                        update.task,
                        update.status,
                        update.child_operation_id,
                        update.summary,
                    )?;
            }
            Ok(())
        }
    }

    pub(super) fn ensure_assistant_session_message_started(
        &mut self,
    ) -> Result<String, CodingSessionError> {
        if let Some(message_id) = &self.assistant_session_message_id {
            return Ok(message_id.clone());
        }
        let message_id = self.transaction_mut_required()?.start_assistant_message()?;
        self.assistant_session_message_id = Some(message_id.clone());
        self.completed_assistant_session_message_id = None;
        Ok(message_id)
    }

    pub(super) fn complete_current_assistant_message(
        &mut self,
        message: &AssistantMessage,
    ) -> Result<(), CodingSessionError> {
        let message_id = self.ensure_assistant_session_message_started()?;
        let content = persisted_assistant_content_blocks(&message.content);
        let model_id = message
            .response_model
            .as_deref()
            .unwrap_or(&message.model)
            .to_owned();
        self.transaction_mut_required()?
            .complete_assistant_message(
                message_id.clone(),
                content,
                stop_reason_string(message),
                message.usage.clone(),
                Some(model_id),
            )?;
        self.assistant_session_message_id = None;
        self.completed_assistant_session_message_id = Some(message_id);
        Ok(())
    }

    pub(super) fn ensure_tool_session_call_started(
        &mut self,
        agent_tool_call_id: &str,
        tool_name: &str,
        arguments: Option<&serde_json::Value>,
    ) -> Result<String, CodingSessionError> {
        if let Some(tool_call_id) = self.tool_session_call_ids.get(agent_tool_call_id) {
            return Ok(tool_call_id.clone());
        }
        let arguments = arguments.cloned().unwrap_or_else(|| serde_json::json!({}));
        let tool_call_id = self
            .transaction_mut_required()?
            .record_tool_started(tool_name, arguments)?;
        self.tool_session_call_ids
            .insert(agent_tool_call_id.to_owned(), tool_call_id.clone());
        Ok(tool_call_id)
    }

    pub(super) fn transaction_mut_required(
        &mut self,
    ) -> Result<&mut PromptTurnTransaction, CodingSessionError> {
        self.transaction
            .as_mut()
            .ok_or_else(|| CodingSessionError::Session {
                message: "prompt turn has no active transaction".into(),
            })
    }

    pub(super) fn require_resolved_request(&self, action: &str) -> Result<(), CodingSessionError> {
        if self.request_resolved {
            return Ok(());
        }
        Err(CodingSessionError::Session {
            message: format!("prompt turn cannot {action} before request is resolved"),
        })
    }
}

#[derive(Default)]
pub(super) struct ReasoningDurationTracker {
    open: HashMap<u32, Instant>,
    completed_millis: u64,
    observed: bool,
}

impl ReasoningDurationTracker {
    fn observe(&mut self, event: &AgentEvent) {
        let AgentEvent::LlmEvent(event) = event else {
            return;
        };
        let now = Instant::now();
        match event {
            AssistantMessageEvent::ThinkingStart { content_index, .. } => {
                self.start_at(*content_index, now);
            }
            AssistantMessageEvent::ThinkingEnd { content_index, .. } => {
                self.complete_at(*content_index, now);
            }
            AssistantMessageEvent::Done { .. } => self.finish_at(now),
            AssistantMessageEvent::Error { .. } => *self = Self::default(),
            _ => {}
        }
    }

    fn start_at(&mut self, content_index: u32, now: Instant) {
        self.observed = true;
        self.open.entry(content_index).or_insert(now);
    }

    fn complete_at(&mut self, content_index: u32, now: Instant) {
        let Some(started_at) = self.open.remove(&content_index) else {
            return;
        };
        self.completed_millis = self
            .completed_millis
            .saturating_add(duration_millis(started_at, now));
    }

    fn finish_at(&mut self, now: Instant) {
        let open = std::mem::take(&mut self.open);
        for started_at in open.into_values() {
            self.completed_millis = self
                .completed_millis
                .saturating_add(duration_millis(started_at, now));
        }
    }

    fn duration_millis(&self) -> Option<u64> {
        self.observed.then_some(self.completed_millis)
    }

    fn take_duration_millis(&mut self) -> Option<u64> {
        let duration = self.duration_millis();
        *self = Self::default();
        duration
    }
}

fn duration_millis(started_at: Instant, completed_at: Instant) -> u64 {
    u64::try_from(
        completed_at
            .saturating_duration_since(started_at)
            .as_millis(),
    )
    .unwrap_or(u64::MAX)
}

fn stop_reason_string(message: &AssistantMessage) -> Option<String> {
    serde_json::to_value(&message.stop_reason)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
}

fn persisted_tool_result(content: &[ContentBlock]) -> PersistedToolResult {
    PersistedToolResult::Text {
        text: content_blocks_text(content),
    }
}

struct TerminalDelegationUpdate {
    requesting_profile_id: ProfileId,
    target_kind: ProfileKind,
    target_id: ProfileId,
    task: String,
    status: PersistedDelegationStatus,
    child_operation_id: Option<String>,
    summary: Option<String>,
}

fn terminal_delegation_update(
    tool_name: &str,
    content: &[ContentBlock],
) -> Option<TerminalDelegationUpdate> {
    if !matches!(tool_name, "delegate_agent" | "delegate_team") {
        return None;
    }
    let text = content.iter().find_map(|block| match block {
        ContentBlock::Text { text, .. } => Some(text.as_str()),
        _ => None,
    })?;
    let value: serde_json::Value = serde_json::from_str(text).ok()?;
    let status = match value.get("status")?.as_str()? {
        "completed" => PersistedDelegationStatus::Completed,
        "failed" => PersistedDelegationStatus::Failed,
        "rejected" => PersistedDelegationStatus::Rejected,
        "cancelled" => PersistedDelegationStatus::Cancelled,
        _ => return None,
    };
    let target_kind = match value.get("target_kind")?.as_str()? {
        "agent" => ProfileKind::Agent,
        "team" => ProfileKind::Team,
        _ => return None,
    };
    let requesting_profile_id =
        ProfileId::new(value.get("requesting_profile_id")?.as_str()?.to_owned()).ok()?;
    let target_id = ProfileId::new(value.get("target_id")?.as_str()?.to_owned()).ok()?;
    let summary = value
        .get("final_text")
        .or_else(|| value.get("error"))
        .or_else(|| value.get("message"))
        .and_then(|value| value.as_str())
        .map(ToOwned::to_owned);
    Some(TerminalDelegationUpdate {
        requesting_profile_id,
        target_kind,
        target_id,
        task: value.get("task")?.as_str()?.to_owned(),
        status,
        child_operation_id: value
            .get("child_operation_id")
            .and_then(|value| value.as_str())
            .map(ToOwned::to_owned),
        summary,
    })
}

pub(super) fn persisted_content_blocks_from_invocation(
    invocation: &PromptInvocation,
) -> Result<Vec<PersistedContentBlock>, CodingSessionError> {
    match invocation {
        PromptInvocation::Text(text) if !text.is_empty() => {
            Ok(vec![PersistedContentBlock::Text { text: text.clone() }])
        }
        PromptInvocation::Text(_) => Err(CodingSessionError::Input {
            message: "prompt turn requires non-empty text input".into(),
        }),
        PromptInvocation::Content(content) if !content.is_empty() => {
            Ok(content.iter().map(persisted_content_block).collect())
        }
        PromptInvocation::Content(_) => Err(CodingSessionError::Input {
            message: "prompt turn requires non-empty content input".into(),
        }),
        PromptInvocation::Skill {
            name,
            additional_instructions,
        } => {
            let text = match additional_instructions {
                Some(instructions) if !instructions.is_empty() => {
                    format!("skill:{name}\n{instructions}")
                }
                _ => format!("skill:{name}"),
            };
            Ok(vec![PersistedContentBlock::Text { text }])
        }
        PromptInvocation::PromptTemplate { name, args } => {
            let text = if args.is_empty() {
                format!("prompt_template:{name}")
            } else {
                format!("prompt_template:{name}\n{}", args.join("\n"))
            };
            Ok(vec![PersistedContentBlock::Text { text }])
        }
        PromptInvocation::Compact { .. } => Err(CodingSessionError::UnsupportedCapability {
            capability: "manual compaction in PromptTurnRunner".into(),
        }),
    }
}

fn persisted_content_block(content: &ContentBlock) -> PersistedContentBlock {
    match content {
        ContentBlock::Text { text, .. } => PersistedContentBlock::Text { text: text.clone() },
        ContentBlock::Image { mime_type, data } => PersistedContentBlock::Image {
            mime_type: mime_type.clone(),
            data: data.clone(),
        },
        ContentBlock::Thinking {
            thinking,
            thinking_signature,
            provider_metadata,
            redacted,
        } => PersistedContentBlock::Thinking {
            thinking: thinking.clone(),
            thinking_signature: thinking_signature.clone(),
            provider_metadata: provider_metadata.clone(),
            redacted: *redacted,
        },
        ContentBlock::ToolCall {
            name, arguments, ..
        } => PersistedContentBlock::Text {
            text: format!("[tool_call:{name} {arguments}]"),
        },
        ContentBlock::ProviderItem { api, item } => PersistedContentBlock::ProviderItem {
            api: api.clone(),
            item: item.clone(),
        },
    }
}

fn persisted_assistant_content_blocks(content: &[ContentBlock]) -> Vec<PersistedContentBlock> {
    content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text, .. } => {
                Some(PersistedContentBlock::Text { text: text.clone() })
            }
            ContentBlock::Thinking {
                thinking,
                thinking_signature,
                provider_metadata,
                redacted,
            } => Some(PersistedContentBlock::Thinking {
                thinking: thinking.clone(),
                thinking_signature: thinking_signature.clone(),
                provider_metadata: provider_metadata.clone(),
                redacted: *redacted,
            }),
            ContentBlock::Image { mime_type, data } => Some(PersistedContentBlock::Image {
                mime_type: mime_type.clone(),
                data: data.clone(),
            }),
            ContentBlock::ToolCall { .. } => None,
            ContentBlock::ProviderItem { api, item } => Some(PersistedContentBlock::ProviderItem {
                api: api.clone(),
                item: item.clone(),
            }),
        })
        .collect()
}

fn persisted_content_blocks_text(content: &[PersistedContentBlock]) -> String {
    content
        .iter()
        .map(|block| match block {
            PersistedContentBlock::Text { text } => text.clone(),
            PersistedContentBlock::Thinking { thinking, .. } => thinking.clone(),
            PersistedContentBlock::Image { mime_type, .. } => format!("[image:{mime_type}]"),
            PersistedContentBlock::ProviderItem { api, .. } => format!("[provider_item:{api}]"),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn content_blocks_text(content: &[ContentBlock]) -> String {
    content
        .iter()
        .map(|block| match block {
            ContentBlock::Text { text, .. } => text.clone(),
            ContentBlock::Thinking { thinking, .. } => thinking.clone(),
            ContentBlock::Image { mime_type, .. } => format!("[image:{mime_type}]"),
            ContentBlock::ToolCall { name, .. } => format!("[tool_call:{name}]"),
            ContentBlock::ProviderItem { api, .. } => format!("[provider_item:{api}]"),
        })
        .collect::<Vec<_>>()
        .join("\n")
}
