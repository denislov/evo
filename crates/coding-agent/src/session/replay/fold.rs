use super::*;

impl ReplayBuilder {
    pub(super) fn observe_session_id(&mut self, event: &SessionEventEnvelope) {
        match self.session_id.as_deref() {
            None => self.session_id = Some(event.session_id.clone()),
            Some(session_id) if session_id != event.session_id => self.warn(format!(
                "event {} belongs to {}, expected {}",
                event.event_id, event.session_id, session_id
            )),
            Some(_) => {}
        }
    }

    pub(super) fn observe_operation_status(&mut self, event: &SessionEventEnvelope) {
        let Some(operation_id) = event.operation_id.as_deref() else {
            return;
        };
        match &event.data {
            SessionEventData::OperationStarted { .. } => {
                self.operation_statuses
                    .entry(operation_id.to_owned())
                    .or_insert(OperationReplayStatus::InDoubt);
            }
            SessionEventData::OperationCommitted { .. } => {
                self.operation_statuses
                    .insert(operation_id.to_owned(), OperationReplayStatus::Committed);
            }
            SessionEventData::OperationFailed { .. } => {
                self.operation_statuses
                    .insert(operation_id.to_owned(), OperationReplayStatus::Failed);
            }
            SessionEventData::OperationAborted { .. } => {
                self.operation_statuses
                    .insert(operation_id.to_owned(), OperationReplayStatus::Aborted);
            }
            SessionEventData::OperationRecovered { .. } => {
                self.operation_statuses
                    .insert(operation_id.to_owned(), OperationReplayStatus::Recovered);
            }
            SessionEventData::OperationRecoveryPending { .. } => {}
            SessionEventData::OperationTerminalRecorded { status, .. } => {
                let status = match status.as_str() {
                    "completed" => OperationReplayStatus::Committed,
                    "failed" => OperationReplayStatus::Failed,
                    "aborted" => OperationReplayStatus::Aborted,
                    _ => return,
                };
                self.operation_statuses
                    .insert(operation_id.to_owned(), status);
            }
            _ => {}
        }
    }

    pub(super) fn apply_event(&mut self, event: &SessionEventEnvelope) {
        match &event.data {
            SessionEventData::SessionCreated { cwd, .. } => {
                self.cwd = cwd.clone();
            }
            SessionEventData::OperationStarted { operation, .. } => {
                if let Some(operation_id) = event.operation_id.as_deref() {
                    self.operation_kinds
                        .insert(operation_id.to_owned(), operation.clone());
                    self.operation_transcript_starts
                        .insert(operation_id.to_owned(), self.transcript.len());
                }
            }
            SessionEventData::SessionCloned { .. }
            | SessionEventData::SessionForked { .. }
            | SessionEventData::SessionCompactionStarted { .. }
            | SessionEventData::TurnStarted {}
            | SessionEventData::SelfHealingEditStarted { .. }
            | SessionEventData::SelfHealingEditRepairAttempted { .. }
            | SessionEventData::SelfHealingEditCompleted { .. }
            | SessionEventData::OperationRecoveryResolved { .. }
            | SessionEventData::MetadataUpdated { .. } => {}
            SessionEventData::SessionTreeLabelUpdated { entry_id, label } => {
                self.tree_labels.insert(
                    entry_id.clone(),
                    ReplayTreeLabel {
                        label: label.clone(),
                        updated_at: event.created_at.clone(),
                    },
                );
            }
            SessionEventData::SessionCompactionCompleted {
                summary,
                first_kept_message_id,
                tokens_before,
            } => {
                self.usage.last_context_tokens = None;
                self.usage.last_context_message_id = None;
                self.apply_compaction_completed(summary, first_kept_message_id, *tokens_before);
            }
            SessionEventData::BranchSummaryCreated {
                summary,
                source_leaf_id,
                target_leaf_id,
            } => {
                self.transcript.push(TranscriptItem::BranchSummary {
                    summary: summary.clone(),
                    source_leaf_id: source_leaf_id.clone(),
                    target_leaf_id: target_leaf_id.clone(),
                });
            }
            SessionEventData::DelegationConfirmationRequested {
                source_operation_id,
                turn_id,
                tool_call_id,
                requesting_profile_id,
                target_kind,
                target_id,
                task,
                reason,
                runtime_seed,
            } => {
                self.add_pending_delegation_confirmation(ReplayPendingDelegationConfirmation {
                    source_operation_id: source_operation_id.clone(),
                    turn_id: turn_id.clone(),
                    tool_call_id: tool_call_id.clone(),
                    requesting_profile_id: requesting_profile_id.clone(),
                    target_kind: *target_kind,
                    target_id: target_id.clone(),
                    task: task.clone(),
                    reason: reason.clone(),
                    requested_at: event.created_at.clone(),
                    runtime_seed: runtime_seed.clone(),
                });
            }
            SessionEventData::DelegationConfirmationApproved {
                source_operation_id,
                tool_call_id,
                ..
            }
            | SessionEventData::DelegationConfirmationRejected {
                source_operation_id,
                tool_call_id,
                ..
            } => {
                self.resolve_pending_delegation_confirmation(source_operation_id, tool_call_id);
            }
            SessionEventData::DelegationFoldedUpdated {
                tool_call_id,
                requesting_profile_id,
                target_kind,
                target_id,
                task,
                status,
                child_operation_id,
                summary,
            } => {
                self.apply_delegation_folded_update(DelegationBlockUpdate {
                    tool_call_id: tool_call_id.clone(),
                    requesting_profile_id: requesting_profile_id.clone(),
                    target_kind: *target_kind,
                    target_id: target_id.clone(),
                    task: task.clone(),
                    status: *status,
                    child_operation_id: child_operation_id.clone(),
                    summary: summary.clone(),
                });
            }
            SessionEventData::ToolAuthorizationRequested { request } => {
                if self
                    .pending_tool_authorizations
                    .insert(request.authorization_id.clone(), request.clone())
                    .is_some()
                {
                    self.warn(format!(
                        "duplicate pending tool authorization: {}",
                        request.authorization_id
                    ));
                }
            }
            SessionEventData::ToolAuthorizationResolved {
                authorization_id, ..
            } => {
                if self
                    .pending_tool_authorizations
                    .remove(authorization_id)
                    .is_none()
                {
                    self.warn(format!(
                        "tool authorization resolution references unknown pending request: {authorization_id}"
                    ));
                }
            }
            SessionEventData::OperationCommitted { new_leaf_id } => {
                if let Some(new_leaf_id) = new_leaf_id {
                    self.record_prompt_leaf(event, new_leaf_id);
                    self.active_leaf_id = Some(new_leaf_id.clone());
                }
            }
            SessionEventData::OperationAborted { reason } => {
                self.warn(format!(
                    "operation {} aborted: {reason}",
                    event.operation_id.as_deref().unwrap_or("<unknown>")
                ));
            }
            SessionEventData::OperationFailed {
                error_code,
                message,
            } => {
                self.diagnostics.push(ReplayDiagnostic {
                    level: DiagnosticLevel::Error,
                    message: format!(
                        "operation {} failed ({error_code}): {message}",
                        event.operation_id.as_deref().unwrap_or("<unknown>")
                    ),
                });
            }
            SessionEventData::OperationRecoveryPending { .. } => {}
            SessionEventData::OperationRecovered { reason, .. } => {
                self.warn(format!(
                    "operation {} recovered: {reason}",
                    event.operation_id.as_deref().unwrap_or("<unknown>")
                ));
            }
            SessionEventData::OperationTerminalRecorded { .. } => {}
            SessionEventData::TurnInputRecorded { content } => {
                self.transcript.push(TranscriptItem::UserInput {
                    turn_id: event.turn_id.clone().unwrap_or_default(),
                    text: content_blocks_text(content),
                    started_at: Some(event.created_at.clone()),
                });
            }
            SessionEventData::MessageStarted { message_id, .. } => {
                self.message_indices
                    .insert(message_id.clone(), self.transcript.len());
                self.transcript.push(TranscriptItem::AssistantMessage {
                    message_id: message_id.clone(),
                    content: Vec::new(),
                    status: MessageStatus::Started,
                    reasoning_duration_millis: None,
                    model_id: None,
                    completed_at: None,
                });
            }
            SessionEventData::MessageReasoningStarted {
                message_id,
                content_index,
            } => {
                if !self.message_indices.contains_key(message_id) {
                    self.warn(format!(
                        "reasoning start references unknown message: {message_id}/{content_index}"
                    ));
                } else {
                    let duplicate = match self
                        .reasoning_started_at
                        .entry((message_id.clone(), *content_index))
                    {
                        std::collections::hash_map::Entry::Vacant(entry) => {
                            entry.insert(event.created_at.clone());
                            false
                        }
                        std::collections::hash_map::Entry::Occupied(_) => true,
                    };
                    if duplicate {
                        self.warn(format!(
                            "duplicate reasoning start: {message_id}/{content_index}"
                        ));
                    }
                }
            }
            SessionEventData::MessageReasoningCompleted {
                message_id,
                content_index,
            } => {
                let key = (message_id.clone(), *content_index);
                let Some(started_at) = self.reasoning_started_at.remove(&key) else {
                    self.warn(format!(
                        "reasoning completion has no matching start: {message_id}/{content_index}"
                    ));
                    return;
                };
                let Some(duration_millis) = elapsed_millis(&started_at, &event.created_at) else {
                    self.warn(format!(
                        "reasoning lifecycle has invalid timestamps: {message_id}/{content_index}"
                    ));
                    return;
                };
                if self
                    .add_reasoning_duration(message_id, duration_millis)
                    .is_err()
                {
                    self.warn(format!(
                        "reasoning completion references unknown message: {message_id}/{content_index}"
                    ));
                }
            }
            SessionEventData::MessageCompleted {
                message_id,
                content,
                finish_reason: _,
                usage,
                model_id,
            } => {
                self.record_assistant_usage(message_id, usage);
                let completed_at = event.created_at.clone();
                if self.complete_message(message_id, content.clone()).is_err() {
                    self.warn(format!(
                        "message completion references unknown message: {message_id}"
                    ));
                } else {
                    if let Some(model_id) = model_id
                        && self
                            .set_message_model(message_id, model_id.to_owned())
                            .is_err()
                    {
                        self.warn(format!(
                            "message model attribution references unknown message: {message_id}"
                        ));
                    }
                    if self
                        .set_message_completed_at(message_id, completed_at)
                        .is_err()
                    {
                        self.warn(format!(
                            "message completion time references unknown message: {message_id}"
                        ));
                    }
                }
            }
            SessionEventData::ModelUsageRecorded {
                purpose: _,
                model_id: _,
                usage,
            } => self.record_usage(None, usage),
            SessionEventData::MessageCancelled { message_id, .. } => {
                self.reasoning_started_at
                    .retain(|(open_message_id, _), _| open_message_id != message_id);
                if self
                    .set_message_status(message_id, MessageStatus::Cancelled)
                    .is_err()
                {
                    self.warn(format!(
                        "message cancellation references unknown message: {message_id}"
                    ));
                }
            }
            SessionEventData::ToolCallStarted {
                tool_call_id,
                name,
                arguments,
            } => {
                self.tool_indices
                    .insert(tool_call_id.clone(), self.transcript.len());
                self.transcript.push(TranscriptItem::ToolCall {
                    tool_call_id: tool_call_id.clone(),
                    name: name.clone(),
                    arguments: arguments.clone(),
                    status: ToolCallStatus::Started,
                    summary: String::new(),
                    started_at: event.created_at.clone(),
                    duration_millis: None,
                });
            }
            SessionEventData::ToolCallUpdated {
                tool_call_id,
                message,
            } => {
                if let Some(tool) = self.tool_mut(tool_call_id) {
                    if !tool.is_empty() {
                        tool.push('\n');
                    }
                    tool.push_str(message);
                } else {
                    self.warn(format!(
                        "tool update references unknown tool call: {tool_call_id}"
                    ));
                }
            }
            SessionEventData::ToolCallCompleted {
                tool_call_id,
                result,
            } => {
                if self
                    .set_tool_status(tool_call_id, ToolCallStatus::Completed, &event.created_at)
                    .is_err()
                {
                    self.warn(format!(
                        "tool completion references unknown tool call: {tool_call_id}"
                    ));
                } else if let Some(summary) = self.tool_mut(tool_call_id) {
                    *summary = tool_result_summary(result);
                }
            }
            SessionEventData::ToolCallFailed {
                tool_call_id,
                message,
            } => {
                if self
                    .set_tool_status(tool_call_id, ToolCallStatus::Failed, &event.created_at)
                    .is_err()
                {
                    self.warn(format!(
                        "tool failure references unknown tool call: {tool_call_id}"
                    ));
                } else if let Some(summary) = self.tool_mut(tool_call_id) {
                    *summary = message.clone();
                }
            }
            SessionEventData::ToolCallCancelled {
                tool_call_id,
                reason,
            } => {
                if self
                    .set_tool_status(tool_call_id, ToolCallStatus::Cancelled, &event.created_at)
                    .is_err()
                {
                    self.warn(format!(
                        "tool cancellation references unknown tool call: {tool_call_id}"
                    ));
                } else if let Some(summary) = self.tool_mut(tool_call_id) {
                    *summary = reason.clone();
                }
            }
            SessionEventData::DiagnosticEmitted { level, message } => {
                self.diagnostics.push(ReplayDiagnostic {
                    level: level.clone(),
                    message: message.clone(),
                });
                self.transcript.push(TranscriptItem::Diagnostic {
                    level: level.clone(),
                    message: message.clone(),
                });
            }
            SessionEventData::ActiveLeafChanged { leaf_id } => {
                self.active_leaf_id = Some(leaf_id.clone());
            }
        }
    }

    fn record_prompt_leaf(&mut self, event: &SessionEventEnvelope, leaf_id: &str) {
        let Some(operation_id) = event.operation_id.as_deref() else {
            return;
        };
        if self.operation_kinds.get(operation_id)
            != Some(&crate::session::event::OperationKind::Prompt)
        {
            return;
        }
        let transcript_start = self
            .operation_transcript_starts
            .get(operation_id)
            .copied()
            .unwrap_or(self.transcript.len());
        self.leaves.push(ReplayLeaf {
            leaf_id: leaf_id.to_owned(),
            parent_leaf_id: self.active_leaf_id.clone(),
            transcript_start,
            transcript_end: self.transcript.len(),
        });
    }

    fn record_assistant_usage(&mut self, message_id: &str, usage: &Usage) {
        self.record_usage(Some(message_id), usage);
    }

    fn record_usage(&mut self, context_message_id: Option<&str>, usage: &Usage) {
        self.usage.input = self.usage.input.saturating_add(usage.input);
        self.usage.output = self.usage.output.saturating_add(usage.output);
        self.usage.cache_read = self.usage.cache_read.saturating_add(usage.cache_read);
        self.usage.cache_write = self.usage.cache_write.saturating_add(usage.cache_write);
        if usage.cost.known {
            self.usage.cost += usage.cost.input
                + usage.cost.output
                + usage.cost.cache_read
                + usage.cost.cache_write;
        } else {
            self.usage.cost_known = false;
        }

        let context_tokens = calculate_context_tokens(usage);
        if context_tokens > 0
            && let Some(message_id) = context_message_id
        {
            self.usage.last_context_tokens = Some(context_tokens);
            self.usage.last_context_message_id = Some(message_id.to_owned());
        }
    }

    fn add_pending_delegation_confirmation(
        &mut self,
        pending: ReplayPendingDelegationConfirmation,
    ) {
        if self
            .pending_delegation_confirmations
            .iter()
            .any(|existing| {
                existing.source_operation_id == pending.source_operation_id
                    && existing.tool_call_id == pending.tool_call_id
            })
        {
            self.warn(format!(
                "duplicate pending delegation confirmation: operation_id={}, tool_call_id={}",
                pending.source_operation_id, pending.tool_call_id
            ));
            return;
        }
        self.pending_delegation_confirmations.push(pending);
    }

    fn resolve_pending_delegation_confirmation(
        &mut self,
        source_operation_id: &str,
        tool_call_id: &str,
    ) {
        let Some(index) = self
            .pending_delegation_confirmations
            .iter()
            .position(|pending| {
                pending.source_operation_id == source_operation_id
                    && pending.tool_call_id == tool_call_id
            })
        else {
            self.warn(format!(
                "delegation confirmation resolution references unknown pending request: operation_id={source_operation_id}, tool_call_id={tool_call_id}"
            ));
            return;
        };
        self.pending_delegation_confirmations.remove(index);
    }

    fn apply_delegation_folded_update(&mut self, update: DelegationBlockUpdate) {
        let item = TranscriptItem::DelegationBlock {
            tool_call_id: update.tool_call_id.clone(),
            requesting_profile_id: update.requesting_profile_id,
            target_kind: update.target_kind,
            target_id: update.target_id,
            task: update.task,
            status: update.status,
            child_operation_id: update.child_operation_id,
            summary: update.summary,
        };
        if let Some(index) = self.delegation_indices.get(&update.tool_call_id).copied() {
            self.transcript[index] = item;
            return;
        }
        if let Some(index) = self.tool_indices.remove(&update.tool_call_id) {
            self.transcript[index] = item;
            self.delegation_indices.insert(update.tool_call_id, index);
            return;
        }
        let index = self.transcript.len();
        self.transcript.push(item);
        self.delegation_indices.insert(update.tool_call_id, index);
    }

    fn tool_mut(&mut self, tool_call_id: &str) -> Option<&mut String> {
        let index = *self.tool_indices.get(tool_call_id)?;
        match self.transcript.get_mut(index)? {
            TranscriptItem::ToolCall { summary, .. } => Some(summary),
            _ => None,
        }
    }

    fn complete_message(
        &mut self,
        message_id: &str,
        content: Vec<PersistedContentBlock>,
    ) -> Result<(), ()> {
        let index = *self.message_indices.get(message_id).ok_or(())?;
        match self.transcript.get_mut(index).ok_or(())? {
            TranscriptItem::AssistantMessage {
                content: current,
                status,
                ..
            } => {
                *current = content;
                *status = MessageStatus::Completed;
                Ok(())
            }
            _ => Err(()),
        }
    }

    fn add_reasoning_duration(&mut self, message_id: &str, duration_millis: u64) -> Result<(), ()> {
        let index = *self.message_indices.get(message_id).ok_or(())?;
        match self.transcript.get_mut(index).ok_or(())? {
            TranscriptItem::AssistantMessage {
                reasoning_duration_millis,
                ..
            } => {
                *reasoning_duration_millis = Some(
                    reasoning_duration_millis
                        .unwrap_or_default()
                        .saturating_add(duration_millis),
                );
                Ok(())
            }
            _ => Err(()),
        }
    }

    fn set_message_status(&mut self, message_id: &str, status: MessageStatus) -> Result<(), ()> {
        let index = *self.message_indices.get(message_id).ok_or(())?;
        match self.transcript.get_mut(index).ok_or(())? {
            TranscriptItem::AssistantMessage {
                status: current, ..
            } => {
                *current = status;
                Ok(())
            }
            _ => Err(()),
        }
    }

    fn set_message_model(&mut self, message_id: &str, model_id: String) -> Result<(), ()> {
        let index = *self.message_indices.get(message_id).ok_or(())?;
        match self.transcript.get_mut(index).ok_or(())? {
            TranscriptItem::AssistantMessage {
                model_id: current, ..
            } => {
                *current = Some(model_id);
                Ok(())
            }
            _ => Err(()),
        }
    }

    fn set_message_completed_at(
        &mut self,
        message_id: &str,
        completed_at: String,
    ) -> Result<(), ()> {
        let index = *self.message_indices.get(message_id).ok_or(())?;
        match self.transcript.get_mut(index).ok_or(())? {
            TranscriptItem::AssistantMessage {
                completed_at: current,
                ..
            } => {
                *current = Some(completed_at);
                Ok(())
            }
            _ => Err(()),
        }
    }

    fn set_tool_status(
        &mut self,
        tool_call_id: &str,
        status: ToolCallStatus,
        terminal_at: &str,
    ) -> Result<(), ()> {
        let index = *self.tool_indices.get(tool_call_id).ok_or(())?;
        match self.transcript.get_mut(index).ok_or(())? {
            TranscriptItem::ToolCall {
                status: current,
                started_at,
                duration_millis,
                ..
            } => {
                *current = status;
                *duration_millis = elapsed_millis(started_at, terminal_at);
                Ok(())
            }
            _ => Err(()),
        }
    }

    pub(super) fn warn(&mut self, message: impl Into<String>) {
        self.diagnostics.push(ReplayDiagnostic {
            level: DiagnosticLevel::Warn,
            message: message.into(),
        });
    }

    fn apply_compaction_completed(
        &mut self,
        summary: &str,
        first_kept_message_id: &str,
        tokens_before: u32,
    ) {
        let Some(first_kept_index) = self
            .transcript
            .iter()
            .position(|item| transcript_item_id(item).as_deref() == Some(first_kept_message_id))
        else {
            self.warn(format!(
                "session compaction references unknown first kept message: {first_kept_message_id}"
            ));
            return;
        };

        let kept = self.transcript.split_off(first_kept_index);
        self.transcript.clear();
        self.transcript.push(TranscriptItem::CompactionSummary {
            summary: summary.to_owned(),
            first_kept_message_id: first_kept_message_id.to_owned(),
            tokens_before,
        });
        self.transcript.extend(kept);
        self.rebuild_indices();
    }

    fn rebuild_indices(&mut self) {
        self.message_indices.clear();
        self.tool_indices.clear();
        self.delegation_indices.clear();
        for (index, item) in self.transcript.iter().enumerate() {
            match item {
                TranscriptItem::AssistantMessage { message_id, .. } => {
                    self.message_indices.insert(message_id.clone(), index);
                }
                TranscriptItem::ToolCall { tool_call_id, .. } => {
                    self.tool_indices.insert(tool_call_id.clone(), index);
                }
                TranscriptItem::DelegationBlock { tool_call_id, .. } => {
                    self.delegation_indices.insert(tool_call_id.clone(), index);
                }
                TranscriptItem::UserInput { .. }
                | TranscriptItem::CompactionSummary { .. }
                | TranscriptItem::BranchSummary { .. }
                | TranscriptItem::Diagnostic { .. } => {}
            }
        }
        self.reasoning_started_at
            .retain(|(message_id, _), _| self.message_indices.contains_key(message_id));
    }
}
