use super::*;

impl EventService {
    pub(crate) fn with_snapshot_coordinator(
        snapshot_coordinator: Arc<SnapshotCoordinator>,
    ) -> Self {
        Self::with_event_capacities_and_coordinator(
            EVENT_CHANNEL_CAPACITY,
            EVENT_RETAINED_CAPACITY,
            snapshot_coordinator,
        )
    }

    pub(super) fn with_event_capacities_and_coordinator(
        channel_capacity: usize,
        retained_capacity: usize,
        snapshot_coordinator: Arc<SnapshotCoordinator>,
    ) -> Self {
        let channel_capacity = channel_capacity.max(1);
        let (product_sender, _) = broadcast::channel(channel_capacity);
        Self {
            product_sender,
            snapshot_coordinator,
            deferred_terminal_drafts: Arc::new(Mutex::new(HashMap::new())),
            retained_capacity,
        }
    }

    pub(crate) fn recovery_boundary_after_for_client(
        &self,
        handle: &ClientHandle,
        cursor: ProductEventSequence,
    ) -> Result<ProductEventRecovery, ClientRegistryError> {
        let state = self
            .snapshot_coordinator
            .state
            .lock_resource("runtime snapshot state")?;
        SnapshotCoordinator::validate_client(&state, handle)?;
        Ok(self.recovery_boundary_from_state(&state, cursor))
    }

    pub(super) fn recovery_boundary_from_state(
        &self,
        state: &crate::application::snapshot::SnapshotState,
        cursor: ProductEventSequence,
    ) -> ProductEventRecovery {
        let receiver = ProductEventReceiver {
            inner: self.product_sender.subscribe(),
            lifecycle_receiver: self.snapshot_coordinator.subscribe_lifecycle(),
            snapshot_coordinator: self.snapshot_coordinator.clone(),
        };
        let oldest_available = state
            .retained_product_events
            .front()
            .map(ProductEvent::sequence_internal);
        if let Some(oldest) = oldest_available
            && cursor < oldest
            && cursor != ProductEventSequence::default()
        {
            return ProductEventRecovery::RetainedGap {
                requested_after: cursor,
                oldest_available: oldest,
            };
        }
        let replayed_through =
            ProductEventSequence::new(state.next_event_sequence.saturating_sub(1));
        let replay = state
            .retained_product_events
            .iter()
            .filter(|event| event.sequence_internal() > cursor)
            .cloned()
            .collect();
        ProductEventRecovery::Ready(ProductEventRecoveryBoundary {
            replayed_through,
            replay,
            receiver,
            lifecycle_receiver: self.snapshot_coordinator.subscribe_lifecycle(),
            lifecycle_epoch: state.lifecycle_epoch,
            capability_generation: state.capability_generation.get(),
        })
    }

    pub(super) fn retain_product_event(
        &self,
        state: &mut crate::application::snapshot::SnapshotState,
        event: ProductEvent,
    ) {
        if self.retained_capacity == 0 {
            state.dropped_before = Some(event.sequence_internal().next());
            return;
        }
        let dropped = state.retained_product_events.len() == self.retained_capacity;
        if state.retained_product_events.len() == self.retained_capacity {
            state.retained_product_events.pop_front();
        }
        state.retained_product_events.push_back(event);
        if dropped {
            state.dropped_before = state
                .retained_product_events
                .front()
                .map(ProductEvent::sequence_internal);
        }
    }

    pub(super) fn publish_without_root_terminal(
        &self,
        draft: ProductEventDraft,
    ) -> Result<ProductEvent, CodingSessionError> {
        self.publish(draft, ProductEventEmissionContext::default(), |_, _| None)
    }

    pub(super) fn publish_self_healing_edit_event(
        &self,
        event: SelfHealingEditEvent,
    ) -> Result<ProductEvent, CodingSessionError> {
        let evidence = event.root_terminal_evidence();
        self.publish(
            event.into_product_draft(),
            ProductEventEmissionContext::default(),
            move |operation_kind, terminal_status| {
                terminal_status.and_then(|status| {
                    operation_kind.and_then(|kind| {
                        evidence.and_then(|evidence| {
                            crate::application::operation::contract::product_terminal_operation(
                                kind, evidence, status,
                            )
                        })
                    })
                })
            },
        )
    }

    pub(super) fn publish_prompt_event(
        &self,
        event: PromptEvent,
    ) -> Result<ProductEvent, CodingSessionError> {
        let evidence_source = event.clone();
        self.publish(
            event.into_product_draft(),
            ProductEventEmissionContext::default(),
            move |operation_kind, terminal_status| {
                terminal_status.and_then(|status| {
                    operation_kind.and_then(|kind| {
                        evidence_source
                            .root_terminal_evidence(kind)
                            .and_then(|evidence| {
                                crate::application::operation::contract::product_terminal_operation(
                                    kind, evidence, status,
                                )
                            })
                    })
                })
            },
        )
    }

    pub(super) fn publish_agent_invocation_event(
        &self,
        event: AgentInvocationEvent,
    ) -> Result<ProductEvent, CodingSessionError> {
        let evidence = event.root_terminal_evidence();
        self.publish(
            event.into_product_draft(),
            ProductEventEmissionContext::default(),
            move |operation_kind, terminal_status| {
                terminal_status.and_then(|status| {
                    operation_kind.and_then(|kind| {
                        evidence.and_then(|evidence| {
                            crate::application::operation::contract::product_terminal_operation(
                                kind, evidence, status,
                            )
                        })
                    })
                })
            },
        )
    }

    pub(super) fn publish_team_event(
        &self,
        event: TeamEvent,
    ) -> Result<ProductEvent, CodingSessionError> {
        let evidence = event.root_terminal_evidence();
        self.publish(
            event.into_product_draft(),
            ProductEventEmissionContext::default(),
            move |operation_kind, terminal_status| {
                terminal_status.and_then(|status| {
                    operation_kind.and_then(|kind| {
                        evidence.and_then(|evidence| {
                            crate::application::operation::contract::product_terminal_operation(
                                kind, evidence, status,
                            )
                        })
                    })
                })
            },
        )
    }

    pub(crate) fn publish_prompt_stream_event(
        &self,
        event: PromptStreamEvent,
    ) -> Result<ProductEvent, CodingSessionError> {
        self.publish_without_root_terminal(event.into_product_draft())
    }

    pub(super) fn publish(
        &self,
        draft: ProductEventDraft,
        explicit: ProductEventEmissionContext,
        resolve_terminal: impl FnOnce(
            Option<OperationKind>,
            Option<crate::events::CodingAgentProductEventTerminalStatus>,
        )
            -> Option<crate::events::CodingAgentProductEventTerminalOperation>,
    ) -> Result<ProductEvent, CodingSessionError> {
        let mut state = self
            .snapshot_coordinator
            .state
            .lock_resource("runtime snapshot state")?;
        let operation_context = draft
            .operation_id
            .as_ref()
            .and_then(|operation_id| state.operation_event_contexts.get(operation_id))
            .cloned();
        let capability_generation = explicit.capability_generation.or_else(|| {
            operation_context
                .as_ref()
                .map(|context| context.capability_generation)
        });
        let operation_kind = explicit
            .operation_kind
            .or_else(|| operation_context.as_ref().map(|context| context.kind));
        let terminal_operation = resolve_terminal(operation_kind, draft.terminal_status);
        let sequence = ProductEventSequence::new(state.next_event_sequence);
        state.next_event_sequence += 1;
        let is_runtime_shutdown = matches!(
            &draft.event,
            crate::events::CodingAgentProductEventKind::Runtime(
                crate::events::CodingAgentRuntimeProductEvent::ShutDown
            )
        );
        let product_event = ProductEvent::new(
            state.event_stream_id.clone(),
            sequence,
            draft.event,
            draft.operation_id,
            operation_context
                .as_ref()
                .and_then(|context| context.parent_operation_id.clone()),
            operation_context
                .as_ref()
                .map(|context| context.root_operation_id.clone())
                .or(explicit.root_operation_id),
            draft.session_id,
            capability_generation,
            draft.terminal_status,
            terminal_operation,
            draft.durability,
        );
        if is_runtime_shutdown
            && state.runtime_lifecycle
                == crate::application::snapshot::RuntimeLifecycle::ShuttingDown
        {
            state.shutdown_drain_boundary = Some(sequence);
        }
        SnapshotCoordinator::observe_root_terminal_in_state(&mut state, &product_event);
        SnapshotCoordinator::observe_context_event_in_state(
            &mut state,
            &product_event,
            operation_kind,
        );
        self.retain_product_event(&mut state, product_event.clone());
        drop(state);
        let _ = self.product_sender.send(product_event.clone());
        Ok(product_event)
    }

    pub(crate) fn subscribe_product_events(&self) -> ProductEventReceiver {
        ProductEventReceiver {
            inner: self.product_sender.subscribe(),
            lifecycle_receiver: self.snapshot_coordinator.subscribe_lifecycle(),
            snapshot_coordinator: self.snapshot_coordinator.clone(),
        }
    }
}
