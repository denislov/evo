use super::*;

impl SnapshotCoordinator {
    pub(crate) fn install_projection(
        &self,
        session: CodingAgentSessionView,
        capabilities: CodingAgentCapabilities,
        capability_generation: CapabilityGeneration,
        committed_session_sequence: u64,
    ) -> Result<(), CodingSessionError> {
        let _transition = self.capability_transition_guard()?;
        let mut state = self.lock_state()?;
        let active_operation = state
            .projection
            .as_ref()
            .and_then(|projection| projection.active_operation);
        let revision = state
            .projection
            .as_ref()
            .map_or(1, |projection| projection.revision + 1);
        state.projection = Some(SnapshotProjection {
            revision,
            session,
            capabilities,
            active_operation,
            capability_generation,
        });
        state.capability_generation = capability_generation;
        state.committed_session_sequence = committed_session_sequence;
        Ok(())
    }

    pub(crate) fn snapshot(&self) -> Result<UiSnapshot, CodingSessionError> {
        self.snapshot_for_client(None).map_err(|error| match error {
            ClientRegistryError::Resource { message } => CodingSessionError::Resource { message },
            other => CodingSessionError::Input {
                message: other.to_string(),
            },
        })
    }

    pub(crate) fn set_pending_authorizations(
        &self,
        pending: Vec<crate::authorization::ToolAuthorizationRequest>,
    ) -> Result<(), CodingSessionError> {
        self.lock_state()?.pending_authorizations = pending;
        Ok(())
    }

    pub(crate) fn observe_context_event_in_state(
        state: &mut SnapshotState,
        event: &ProductEvent,
        operation_kind: Option<OperationKind>,
    ) {
        state
            .context_projection
            .apply_product_event(event, operation_kind);
    }

    pub(super) fn snapshot_for_client(
        &self,
        handle: Option<&ClientHandle>,
    ) -> Result<UiSnapshot, ClientRegistryError> {
        let mut state = self.lock_state()?;
        let client_drafts = match handle {
            Some(handle) => {
                let record = Self::record(&mut state, handle)?;
                record
                    .prompt_draft
                    .iter()
                    .chain(record.steer_drafts.iter())
                    .chain(record.follow_up_drafts.iter())
                    .map(|draft| ClientDraft::new(draft.kind, draft.text.clone()))
                    .collect()
            }
            None => Vec::new(),
        };
        let projection = state
            .projection
            .clone()
            .ok_or_else(|| ClientRegistryError::Resource {
                message: "snapshot projection is not installed".into(),
            })?;
        let recent_child_events = state
            .retained_product_events
            .iter()
            .filter(|event| {
                event.parent_operation_id().is_some()
                    || matches!(
                        event.event(),
                        crate::events::CodingAgentProductEventKind::Delegation(_)
                    )
            })
            .cloned()
            .collect::<Vec<_>>();
        Ok(UiSnapshot::new(
            UiSnapshotCursor {
                stream_id: state.event_stream_id.clone(),
                last_event_sequence: ProductEventSequence::new(
                    state.next_event_sequence.saturating_sub(1),
                ),
                last_session_sequence: state.committed_session_sequence,
                capability_generation: projection.capability_generation,
            },
            UI_SNAPSHOT_PROTOCOL_VERSION,
            projection.session,
            projection.capabilities,
            projection.active_operation,
            client_drafts,
            state.pending_authorizations.clone(),
        )
        .with_context(state.context_projection.clone())
        .with_recent_child_events(recent_child_events))
    }
}
