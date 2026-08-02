use super::*;

/// Complete bounded durable conversation replacement for one session leaf.
#[derive(Debug, Clone, PartialEq)]
pub struct CodingAgentClientTranscript {
    pub(super) session_id: String,
    pub(super) active_leaf_id: Option<String>,
    pub(super) items: VecDeque<CodingAgentSessionTranscriptItem>,
    pub(super) omitted_items: usize,
    pub(super) continuation: Option<CodingAgentTranscriptContinuation>,
    pub(super) retained_bytes: usize,
    pub(super) truncated: bool,
}

impl CodingAgentClientTranscript {
    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    pub fn active_leaf_id(&self) -> Option<&str> {
        self.active_leaf_id.as_deref()
    }

    pub fn items(&self) -> &VecDeque<CodingAgentSessionTranscriptItem> {
        &self.items
    }

    pub const fn omitted_items(&self) -> usize {
        self.omitted_items
    }

    pub fn continuation(&self) -> Option<&CodingAgentTranscriptContinuation> {
        self.continuation.as_ref()
    }

    pub const fn retained_bytes(&self) -> usize {
        self.retained_bytes
    }

    pub const fn truncated(&self) -> bool {
        self.truncated
    }
}

impl CodingAgentClientProjection {
    pub fn new(
        mut snapshot: CodingAgentSnapshot,
    ) -> Result<Self, CodingAgentClientProjectionIssue> {
        validate_snapshot(&snapshot)?;
        sanitize_snapshot(&mut snapshot);
        Ok(Self {
            snapshot,
            lifecycle: CodingAgentClientProjectionLifecycle::Running,
            transcript: None,
            messages: VecDeque::new(),
            tools: VecDeque::new(),
            diagnostics: VecDeque::new(),
            recoveries: VecDeque::new(),
            context_pending: ProductContextPendingState::default(),
            resync_issue: None,
        })
    }

    pub fn from_bootstrap(
        bootstrap: CodingAgentClientBootstrap,
    ) -> Result<Self, CodingAgentClientProjectionIssue> {
        let CodingAgentClientBootstrap {
            snapshot,
            transcript,
            pending_recoveries,
        } = bootstrap;
        let mut projection = Self::new(snapshot)?;
        let transcript = project_transcript(&projection.snapshot, transcript)?;
        let recoveries = project_pending_recoveries(&projection.snapshot, pending_recoveries)?;
        projection.transcript = Some(transcript);
        projection.recoveries = recoveries;
        Ok(projection)
    }

    pub fn snapshot(&self) -> &CodingAgentSnapshot {
        &self.snapshot
    }

    pub const fn lifecycle(&self) -> CodingAgentClientProjectionLifecycle {
        self.lifecycle
    }

    pub fn transcript(&self) -> Option<&CodingAgentClientTranscript> {
        self.transcript.as_ref()
    }

    pub fn messages(&self) -> &VecDeque<CodingAgentClientMessage> {
        &self.messages
    }

    pub fn tools(&self) -> &VecDeque<CodingAgentClientTool> {
        &self.tools
    }

    pub fn diagnostics(&self) -> &VecDeque<CodingAgentClientDiagnostic> {
        &self.diagnostics
    }

    pub fn recoveries(&self) -> &VecDeque<CodingAgentClientRecovery> {
        &self.recoveries
    }

    pub fn resync_issue(&self) -> Option<&CodingAgentClientProjectionIssue> {
        self.resync_issue.as_ref()
    }

    pub fn replace_snapshot(
        &mut self,
        snapshot: CodingAgentSnapshot,
    ) -> Result<CodingAgentClientProjectionChanges, CodingAgentClientProjectionIssue> {
        self.replace_snapshot_with_retention(snapshot, LiveTailRetention::Discard)
    }

    /// Replace session metadata without disturbing the event-folded live tail.
    ///
    /// A metadata replacement carries no transcript, so the folded message and
    /// tool rows are the only record of an in-flight turn. Discarding them here
    /// blanks the streaming rows with nothing to take their place until the next
    /// full hydration, which reads as rows vanishing mid-turn.
    pub fn replace_metadata_snapshot(
        &mut self,
        snapshot: CodingAgentSnapshot,
    ) -> Result<CodingAgentClientProjectionChanges, CodingAgentClientProjectionIssue> {
        self.replace_snapshot_with_retention(snapshot, LiveTailRetention::Retain)
    }

    fn replace_snapshot_with_retention(
        &mut self,
        mut snapshot: CodingAgentSnapshot,
        retention: LiveTailRetention,
    ) -> Result<CodingAgentClientProjectionChanges, CodingAgentClientProjectionIssue> {
        validate_snapshot(&snapshot)?;
        if snapshot.session.session_id != self.snapshot.session.session_id {
            return Err(CodingAgentClientProjectionIssue::new(
                "snapshot_session_mismatch",
                "A metadata snapshot cannot replace a different session.",
            ));
        }
        if snapshot.cursor.stream_id != self.snapshot.cursor.stream_id {
            return Err(CodingAgentClientProjectionIssue::new(
                "snapshot_stream_mismatch",
                "A metadata snapshot cannot replace a different event stream.",
            ));
        }
        if snapshot.cursor.last_event_sequence < self.snapshot.cursor.last_event_sequence {
            return Err(CodingAgentClientProjectionIssue::new(
                "snapshot_cursor_regression",
                "A replacement snapshot cannot move the event cursor backwards.",
            ));
        }
        sanitize_snapshot(&mut snapshot);
        self.snapshot = snapshot;
        self.lifecycle = CodingAgentClientProjectionLifecycle::Running;
        if retention == LiveTailRetention::Discard {
            self.messages.clear();
            self.tools.clear();
            self.diagnostics.clear();
        }
        // Pending context folds belong to the snapshot's own context, which this
        // replacement supersedes either way.
        self.context_pending = ProductContextPendingState::default();
        self.resync_issue = None;
        Ok(all_projection_areas())
    }

    pub fn replace_bootstrap(
        &mut self,
        bootstrap: CodingAgentClientBootstrap,
    ) -> Result<CodingAgentClientProjectionChanges, CodingAgentClientProjectionIssue> {
        let replacement = Self::from_bootstrap(bootstrap)?;
        *self = replacement;
        Ok(all_projection_areas())
    }

    pub fn replace_transcript(
        &mut self,
        transcript: CodingAgentTranscriptSnapshot,
    ) -> Result<CodingAgentClientProjectionChanges, CodingAgentClientProjectionIssue> {
        let transcript = project_transcript(&self.snapshot, transcript)?;
        self.transcript = Some(transcript);
        let mut changes = CodingAgentClientProjectionChanges::default();
        changes.insert(CodingAgentClientProjectionArea::Conversation);
        Ok(changes)
    }

    pub fn replace_pending_recoveries(
        &mut self,
        pending: Vec<CodingAgentRecoveryPending>,
    ) -> Result<CodingAgentClientProjectionChanges, CodingAgentClientProjectionIssue> {
        let recoveries = project_pending_recoveries(&self.snapshot, pending)?;
        self.recoveries = recoveries;
        let mut changes = CodingAgentClientProjectionChanges::default();
        changes.insert(CodingAgentClientProjectionArea::Recoveries);
        Ok(changes)
    }

    pub fn apply(&mut self, event: &CodingAgentProductEvent) -> CodingAgentClientProjectionApply {
        if self.lifecycle != CodingAgentClientProjectionLifecycle::Running {
            return CodingAgentClientProjectionApply::NeedsResync(
                self.resync_issue.clone().unwrap_or_else(|| {
                    CodingAgentClientProjectionIssue::new(
                        "product_projection_not_running",
                        "The product projection requires a fresh snapshot.",
                    )
                }),
            );
        }
        if event.stream_id() != self.snapshot.cursor.stream_id {
            return self.require_resync(CodingAgentClientProjectionIssue::new(
                "product_event_stream_mismatch",
                "The product event belongs to a different stream.",
            ));
        }
        if event.sequence() <= self.snapshot.cursor.last_event_sequence {
            return CodingAgentClientProjectionApply::IgnoredDuplicate;
        }
        let Some(expected_sequence) = self.snapshot.cursor.last_event_sequence.checked_add(1)
        else {
            return self.require_resync(CodingAgentClientProjectionIssue::new(
                "product_event_cursor_exhausted",
                "The product event cursor is exhausted.",
            ));
        };
        if event.sequence() != expected_sequence {
            return self.require_resync(CodingAgentClientProjectionIssue::new(
                "product_event_cursor_gap",
                "The product event stream contains a gap.",
            ));
        }
        if event
            .session_id()
            .is_some_and(|session_id| session_id != self.snapshot.session.session_id)
        {
            return self.require_resync(CodingAgentClientProjectionIssue::new(
                "product_event_session_mismatch",
                "The product event belongs to a different session.",
            ));
        }
        if !operation_association_matches(&self.snapshot, event) {
            return self.require_resync(CodingAgentClientProjectionIssue::new(
                "product_event_operation_mismatch",
                "The product event does not belong to the submitted operation.",
            ));
        }
        if let Err(issue) = validate_authorization_event(&self.snapshot, event) {
            return self.require_resync(issue);
        }
        let next_generation = match validate_capability_generation(&self.snapshot, event) {
            Ok(generation) => generation,
            Err(issue) => return self.require_resync(issue),
        };

        let mut changes = CodingAgentClientProjectionChanges::default();
        changes.insert(CodingAgentClientProjectionArea::Cursor);
        self.snapshot.cursor.last_event_sequence = event.sequence();
        self.snapshot.cursor.capability_generation = next_generation;
        self.apply_profile(event, &mut changes);
        for change in fold_product_context(
            &mut self.snapshot.context,
            &mut self.context_pending,
            event,
            None,
        ) {
            changes.insert(match change {
                ProductContextFoldChange::Operations => CodingAgentClientProjectionArea::Operations,
                ProductContextFoldChange::Changes => CodingAgentClientProjectionArea::Changes,
                ProductContextFoldChange::Delegations => {
                    CodingAgentClientProjectionArea::Delegations
                }
                ProductContextFoldChange::Usage => CodingAgentClientProjectionArea::Usage,
            });
        }
        self.apply_message(event, &mut changes);
        self.apply_tool(event, &mut changes);
        self.apply_authorization(event, &mut changes);
        self.apply_diagnostic(event, &mut changes);
        self.apply_recovery(event, &mut changes);
        self.apply_runtime(event, &mut changes);
        CodingAgentClientProjectionApply::Applied(changes)
    }

    fn require_resync(
        &mut self,
        issue: CodingAgentClientProjectionIssue,
    ) -> CodingAgentClientProjectionApply {
        self.lifecycle = CodingAgentClientProjectionLifecycle::NeedsResync;
        self.resync_issue = Some(issue.clone());
        CodingAgentClientProjectionApply::NeedsResync(issue)
    }

    fn apply_profile(
        &mut self,
        event: &CodingAgentProductEvent,
        changes: &mut CodingAgentClientProjectionChanges,
    ) {
        if matches!(
            event.event(),
            CodingAgentProductEventKind::Capability(
                CodingAgentCapabilityProductEvent::Changed { .. }
            )
        ) {
            changes.insert(CodingAgentClientProjectionArea::Capabilities);
        }
    }

    fn apply_message(
        &mut self,
        event: &CodingAgentProductEvent,
        changes: &mut CodingAgentClientProjectionChanges,
    ) {
        let CodingAgentProductEventKind::Message(message) = event.event() else {
            return;
        };
        let (operation_id, turn_id, message_id) = match message {
            CodingAgentMessageProductEvent::Started {
                operation_id,
                turn_id,
                message_id,
            }
            | CodingAgentMessageProductEvent::Delta {
                operation_id,
                turn_id,
                message_id,
                ..
            }
            | CodingAgentMessageProductEvent::ThinkingDelta {
                operation_id,
                turn_id,
                message_id,
                ..
            }
            | CodingAgentMessageProductEvent::Completed {
                operation_id,
                turn_id,
                message_id,
                ..
            } => (operation_id, turn_id, message_id),
        };
        let index = self
            .messages
            .iter()
            .position(|current| {
                current.operation_id == *operation_id
                    && current.turn_id == *turn_id
                    && current.message_id == *message_id
            })
            .unwrap_or_else(|| {
                self.messages.push_back(CodingAgentClientMessage {
                    operation_id: bounded_text(operation_id, MAX_ID_BYTES),
                    turn_id: bounded_text(turn_id, MAX_ID_BYTES),
                    message_id: message_id
                        .as_deref()
                        .map(|value| bounded_text(value, MAX_ID_BYTES)),
                    text: String::new(),
                    thinking: String::new(),
                    reasoning_duration_millis: None,
                    status: CodingAgentClientMessageStatus::Streaming,
                    started_sequence: event.sequence(),
                    updated_sequence: event.sequence(),
                    truncated: false,
                });
                self.messages.len() - 1
            });
        let current = &mut self.messages[index];
        current.updated_sequence = event.sequence();
        match message {
            CodingAgentMessageProductEvent::Started { .. } => {}
            CodingAgentMessageProductEvent::Delta { text, .. } => {
                current.truncated |= append_bounded(&mut current.text, text, MAX_MESSAGE_BYTES);
            }
            CodingAgentMessageProductEvent::ThinkingDelta { text, .. } => {
                current.truncated |=
                    append_bounded(&mut current.thinking, text, MAX_THINKING_BYTES);
            }
            CodingAgentMessageProductEvent::Completed {
                final_text,
                reasoning_duration_millis,
                ..
            } => {
                let (text, truncated) = bounded_prefix(final_text, MAX_MESSAGE_BYTES);
                current.text = text;
                current.truncated |= truncated;
                current.status = CodingAgentClientMessageStatus::Completed;
                current.reasoning_duration_millis = *reasoning_duration_millis;
            }
        }
        trim_messages(&mut self.messages);
        changes.insert(CodingAgentClientProjectionArea::Conversation);
    }

    fn apply_tool(
        &mut self,
        event: &CodingAgentProductEvent,
        changes: &mut CodingAgentClientProjectionChanges,
    ) {
        let CodingAgentProductEventKind::Tool(tool) = event.event() else {
            return;
        };
        let (operation_id, turn_id, tool_call_id, name) = match tool {
            CodingAgentToolProductEvent::Started {
                operation_id,
                turn_id,
                tool_call_id,
                name,
                ..
            }
            | CodingAgentToolProductEvent::Updated {
                operation_id,
                turn_id,
                tool_call_id,
                name,
                ..
            }
            | CodingAgentToolProductEvent::Completed {
                operation_id,
                turn_id,
                tool_call_id,
                name,
                ..
            }
            | CodingAgentToolProductEvent::Failed {
                operation_id,
                turn_id,
                tool_call_id,
                name,
                ..
            } => (operation_id, turn_id, tool_call_id, name),
            CodingAgentToolProductEvent::AuthorizationRequired { .. }
            | CodingAgentToolProductEvent::AuthorizationApproved { .. }
            | CodingAgentToolProductEvent::AuthorizationDenied { .. }
            | CodingAgentToolProductEvent::AuthorizationCancelled { .. } => return,
        };
        let index = self
            .tools
            .iter()
            .position(|current| current.tool_call_id == *tool_call_id)
            .unwrap_or_else(|| {
                self.tools.push_back(CodingAgentClientTool {
                    operation_id: bounded_text(operation_id, MAX_ID_BYTES),
                    turn_id: bounded_text(turn_id, MAX_ID_BYTES),
                    tool_call_id: bounded_text(tool_call_id, MAX_ID_BYTES),
                    name: bounded_text(name, MAX_ID_BYTES),
                    arguments: String::new(),
                    detail: String::new(),
                    status: CodingAgentClientToolStatus::Running,
                    started_sequence: event.sequence(),
                    updated_sequence: event.sequence(),
                    truncated: false,
                });
                self.tools.len() - 1
            });
        let current = &mut self.tools[index];
        current.updated_sequence = event.sequence();
        match tool {
            CodingAgentToolProductEvent::Started { arguments_json, .. } => {
                let (arguments, truncated) = bounded_prefix(arguments_json, MAX_TOOL_BYTES);
                current.arguments = arguments;
                current.truncated |= truncated;
            }
            CodingAgentToolProductEvent::Updated { message, .. } => {
                let (detail, truncated) = bounded_prefix(message, MAX_TOOL_BYTES);
                current.detail = detail;
                current.truncated |= truncated;
            }
            CodingAgentToolProductEvent::Completed { summary, .. } => {
                let (detail, truncated) = bounded_prefix(summary, MAX_TOOL_BYTES);
                current.detail = detail;
                current.truncated |= truncated;
                current.status = CodingAgentClientToolStatus::Completed;
            }
            CodingAgentToolProductEvent::Failed { message, .. } => {
                let (detail, truncated) = bounded_prefix(message, MAX_TOOL_BYTES);
                current.detail = detail;
                current.truncated |= truncated;
                current.status = CodingAgentClientToolStatus::Failed;
            }
            CodingAgentToolProductEvent::AuthorizationRequired { .. }
            | CodingAgentToolProductEvent::AuthorizationApproved { .. }
            | CodingAgentToolProductEvent::AuthorizationDenied { .. }
            | CodingAgentToolProductEvent::AuthorizationCancelled { .. } => {}
        }
        trim_tools(&mut self.tools);
        changes.insert(CodingAgentClientProjectionArea::Tools);
    }

    fn apply_authorization(
        &mut self,
        event: &CodingAgentProductEvent,
        changes: &mut CodingAgentClientProjectionChanges,
    ) {
        let CodingAgentProductEventKind::Tool(tool) = event.event() else {
            return;
        };
        match tool {
            CodingAgentToolProductEvent::AuthorizationRequired { request } => {
                let mut request = request.clone();
                sanitize_authorization(&mut request);
                self.snapshot
                    .pending_authorizations
                    .retain(|current| current.authorization_id != request.authorization_id);
                self.snapshot.pending_authorizations.push(request);
                self.snapshot
                    .pending_authorizations
                    .truncate(MAX_PENDING_AUTHORIZATIONS);
            }
            CodingAgentToolProductEvent::AuthorizationApproved {
                authorization_id, ..
            }
            | CodingAgentToolProductEvent::AuthorizationDenied {
                authorization_id, ..
            }
            | CodingAgentToolProductEvent::AuthorizationCancelled {
                authorization_id, ..
            } => self
                .snapshot
                .pending_authorizations
                .retain(|current| current.authorization_id != *authorization_id),
            _ => return,
        }
        changes.insert(CodingAgentClientProjectionArea::Authorizations);
    }

    fn apply_diagnostic(
        &mut self,
        event: &CodingAgentProductEvent,
        changes: &mut CodingAgentClientProjectionChanges,
    ) {
        let CodingAgentProductEventKind::Diagnostic(
            CodingAgentDiagnosticProductEvent::Diagnostic { diagnostic },
        ) = event.event()
        else {
            return;
        };
        let (summary, truncated) = bounded_prefix(&diagnostic.summary, MAX_DIAGNOSTIC_BYTES);
        self.diagnostics.push_back(CodingAgentClientDiagnostic {
            operation_id: diagnostic
                .operation_id
                .as_deref()
                .map(|value| bounded_text(value, MAX_ID_BYTES)),
            code: bounded_text(&diagnostic.code, MAX_ID_BYTES),
            summary,
            sequence: event.sequence(),
            truncated,
        });
        while self.diagnostics.len() > MAX_DIAGNOSTICS {
            self.diagnostics.pop_front();
        }
        changes.insert(CodingAgentClientProjectionArea::Diagnostics);
    }

    fn apply_recovery(
        &mut self,
        event: &CodingAgentProductEvent,
        changes: &mut CodingAgentClientProjectionChanges,
    ) {
        let CodingAgentProductEventKind::Workflow(workflow) = event.event() else {
            return;
        };
        let recovery = match workflow {
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
            } => CodingAgentClientRecovery {
                operation_id: bounded_text(operation_id, MAX_ID_BYTES),
                recovery_id: bounded_text(recovery_id, MAX_ID_BYTES),
                operation_kind: self
                    .snapshot
                    .context
                    .operations
                    .iter()
                    .find(|operation| operation.operation_id == *operation_id)
                    .map(|operation| operation.kind.clone()),
                status: CodingAgentClientRecoveryStatus::Pending,
                reason: bounded_text(reason, MAX_DIAGNOSTIC_BYTES),
                record_version: Some(*record_version),
                descriptor_revision: Some(*descriptor_revision),
                capability_generation: *capability_generation,
                attempt_count: *attempt_count,
                last_attempt_at: last_attempt_at
                    .as_deref()
                    .map(|value| bounded_text(value, MAX_ID_BYTES)),
                next_attempt_at: next_attempt_at
                    .as_deref()
                    .map(|value| bounded_text(value, MAX_ID_BYTES)),
                updated_sequence: event.sequence(),
            },
            CodingAgentWorkflowProductEvent::OperationRecoveryResolved {
                operation_id,
                recovery_id,
                reason,
                record_version,
                descriptor_revision,
                capability_generation,
                ..
            } => CodingAgentClientRecovery {
                operation_id: bounded_text(operation_id, MAX_ID_BYTES),
                recovery_id: bounded_text(recovery_id, MAX_ID_BYTES),
                operation_kind: self
                    .snapshot
                    .context
                    .operations
                    .iter()
                    .find(|operation| operation.operation_id == *operation_id)
                    .map(|operation| operation.kind.clone()),
                status: CodingAgentClientRecoveryStatus::Resolved,
                reason: bounded_text(reason, MAX_DIAGNOSTIC_BYTES),
                record_version: Some(*record_version),
                descriptor_revision: Some(*descriptor_revision),
                capability_generation: *capability_generation,
                attempt_count: 0,
                last_attempt_at: None,
                next_attempt_at: None,
                updated_sequence: event.sequence(),
            },
            CodingAgentWorkflowProductEvent::OperationRecovered {
                operation_id,
                recovery_id,
                reason,
            } => CodingAgentClientRecovery {
                operation_id: bounded_text(operation_id, MAX_ID_BYTES),
                recovery_id: bounded_text(recovery_id, MAX_ID_BYTES),
                operation_kind: self
                    .snapshot
                    .context
                    .operations
                    .iter()
                    .find(|operation| operation.operation_id == *operation_id)
                    .map(|operation| operation.kind.clone()),
                status: CodingAgentClientRecoveryStatus::Recovered,
                reason: bounded_text(reason, MAX_DIAGNOSTIC_BYTES),
                record_version: None,
                descriptor_revision: None,
                capability_generation: None,
                attempt_count: 0,
                last_attempt_at: None,
                next_attempt_at: None,
                updated_sequence: event.sequence(),
            },
            _ => return,
        };
        self.recoveries
            .retain(|current| current.recovery_id != recovery.recovery_id);
        self.recoveries.push_front(recovery);
        self.recoveries.truncate(MAX_RECOVERIES);
        changes.insert(CodingAgentClientProjectionArea::Recoveries);
    }

    fn apply_runtime(
        &mut self,
        event: &CodingAgentProductEvent,
        changes: &mut CodingAgentClientProjectionChanges,
    ) {
        if matches!(
            event.event(),
            CodingAgentProductEventKind::Runtime(CodingAgentRuntimeProductEvent::ShutDown)
        ) {
            self.lifecycle = CodingAgentClientProjectionLifecycle::Stopped;
            changes.insert(CodingAgentClientProjectionArea::Lifecycle);
        }
    }
}
