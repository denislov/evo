use super::*;

impl OperationControl {
    pub(crate) fn with_snapshot_coordinator(
        snapshot_coordinator: Arc<SnapshotCoordinator>,
    ) -> Self {
        Self {
            state: OperationState::with_snapshot_coordinator(snapshot_coordinator),
            prompt_control: PromptControlState::new(),
        }
    }

    pub(crate) fn activity(&self) -> Result<OperationActivity, CodingSessionError> {
        self.state.activity()
    }

    pub(crate) fn begin_root_with_capability_generation(
        &self,
        class: OperationClass,
        kind: OperationKind,
        operation_id: String,
        capability_generation: CapabilityGeneration,
    ) -> Result<OperationGuard, CodingSessionError> {
        self.state.begin_root_with_capability_generation(
            class,
            kind,
            operation_id,
            capability_generation,
        )
    }

    pub(crate) fn begin_child_with_capability_generation(
        &self,
        kind: OperationKind,
        operation_id: String,
        parent_operation_id: String,
        capability_generation: CapabilityGeneration,
    ) -> Result<ChildOperationGuard, CodingSessionError> {
        self.state.begin_child_with_capability_generation(
            kind,
            operation_id,
            parent_operation_id,
            capability_generation,
        )
    }

    pub(crate) fn cancel_capability_generations_before(
        &self,
        generation: CapabilityGeneration,
    ) -> Result<Vec<String>, CodingSessionError> {
        self.state.cancel_capability_generations_before(generation)
    }

    pub(crate) fn cancel_open_operations_for_shutdown(
        &self,
    ) -> Result<Vec<String>, CodingSessionError> {
        Ok(self
            .state
            .shared
            .lock_resource("operation state")?
            .cancel_operations_for_shutdown())
    }

    pub(crate) fn current_prompt_control_registration(
        &self,
    ) -> Result<Option<PromptControlRegistration>, CodingSessionError> {
        self.prompt_control.current()
    }

    pub(crate) fn prompt_control_registration_for(
        &mut self,
        operation_id: &str,
    ) -> Result<PromptControlRegistration, CodingSessionError> {
        self.state
            .ensure_active_target(OperationKind::Prompt, operation_id)
            .map_err(OperationIdentityRejection::into_error)?;
        match self.prompt_control.current()? {
            Some(registration) => Ok(registration),
            None => self.prompt_control.create(),
        }
    }

    pub(crate) fn prompt_control_cleanup(&self) -> PromptControlCleanup {
        self.prompt_control.cleanup()
    }

    pub(crate) fn take_prompt_control_receiver(
        &mut self,
    ) -> Result<Option<PromptControlReceiver>, CodingSessionError> {
        self.prompt_control.take_receiver()
    }

    pub(crate) fn clear_prompt_control_receiver(&mut self) -> Result<(), CodingSessionError> {
        self.prompt_control.clear()
    }
}

#[derive(Debug)]
#[must_use = "dropping OperationGuard clears the active operation"]
pub(crate) struct OperationGuard {
    pub(super) shared: Arc<Mutex<OperationStateInner>>,
    pub(super) snapshot_coordinator: Arc<SnapshotCoordinator>,
    pub(super) class: OperationClass,
    pub(super) kind: OperationKind,
    pub(super) operation_id: String,
    pub(super) generation: u64,
    pub(super) cancellation: Option<CancellationToken>,
}

impl OperationGuard {
    pub(crate) fn cancellation_token(&self) -> Option<CancellationToken> {
        self.cancellation.clone()
    }

    pub(crate) fn cancellation_handle(&self) -> OperationCancellationHandle {
        OperationCancellationHandle {
            shared: Arc::clone(&self.shared),
            operation_id: self.operation_id.clone(),
        }
    }

    pub(crate) fn bind_capability_generation(
        &mut self,
        generation: CapabilityGeneration,
    ) -> Result<(), CodingSessionError> {
        let mut shared = self.shared.lock_resource("operation state")?;
        let matches = |active: &ActiveOperationIdentity| {
            active.kind == self.kind
                && active.operation_id == self.operation_id
                && active.generation == self.generation
        };
        let active = match self.class {
            OperationClass::SessionWriteRoot => shared
                .session_write
                .as_mut()
                .filter(|active| matches(active)),
            OperationClass::NonSessionRoot => shared
                .non_session_roots
                .iter_mut()
                .find(|active| matches(active)),
            OperationClass::RuntimeWrite => shared
                .runtime_write
                .as_mut()
                .filter(|active| matches(active)),
            OperationClass::Query | OperationClass::ReadOnly | OperationClass::Child => None,
        };
        active
            .expect("operation guard must retain its active identity")
            .capability_generation = Some(generation);
        drop(shared);
        self.snapshot_coordinator.register_operation_event_context(
            self.operation_id.clone(),
            self.kind,
            generation,
            None,
            self.operation_id.clone(),
        )?;
        Ok(())
    }
}

impl Drop for OperationGuard {
    fn drop(&mut self) {
        // Drop cannot surface the resource error; recover only to release the
        // operation identity and report poison once.
        let mut shared = self.shared.lock_or_recover("operation state");
        let previous_primary = shared.activity().primary();
        let matches = |active: &ActiveOperationIdentity| {
            active.kind == self.kind
                && active.operation_id == self.operation_id
                && active.generation == self.generation
        };
        let released = match self.class {
            OperationClass::SessionWriteRoot => {
                if let Some(active) = shared
                    .session_write
                    .as_mut()
                    .filter(|active| matches(active))
                {
                    active.owner_released = true;
                    true
                } else {
                    false
                }
            }
            OperationClass::NonSessionRoot => {
                if let Some(active) = shared
                    .non_session_roots
                    .iter_mut()
                    .find(|active| matches(active))
                {
                    active.owner_released = true;
                    true
                } else {
                    false
                }
            }
            OperationClass::RuntimeWrite => {
                if let Some(active) = shared
                    .runtime_write
                    .as_mut()
                    .filter(|active| matches(active))
                {
                    active.owner_released = true;
                    true
                } else {
                    false
                }
            }
            OperationClass::Query | OperationClass::ReadOnly | OperationClass::Child => false,
        };
        if released {
            shared.cancel_descendants(&self.operation_id);
            let removed = shared.remove_released_roots_without_descendants();
            let current_primary = shared.activity().primary();
            drop(shared);
            for (operation_id, generation) in removed {
                self.snapshot_coordinator
                    .clear_operation_event_context_if(&operation_id, generation);
            }
            self.snapshot_coordinator
                .clear_operation_cancellation_if(&self.operation_id);
            if previous_primary != current_primary {
                self.snapshot_coordinator
                    .set_active_operation_from_drop(current_primary);
            }
        } else {
            drop(shared);
        }
    }
}

#[derive(Debug)]
#[must_use = "dropping ChildOperationGuard releases the child operation"]
pub(crate) struct ChildOperationGuard {
    pub(super) shared: Arc<Mutex<OperationStateInner>>,
    pub(super) snapshot_coordinator: Arc<SnapshotCoordinator>,
    pub(super) kind: OperationKind,
    pub(super) operation_id: String,
    pub(super) parent_operation_id: String,
    pub(super) root_operation_id: String,
    pub(super) generation: u64,
    pub(super) cancellation: CancellationToken,
}

impl ChildOperationGuard {
    pub(crate) fn parent_operation_id(&self) -> &str {
        &self.parent_operation_id
    }

    pub(crate) fn root_operation_id(&self) -> &str {
        &self.root_operation_id
    }

    pub(crate) fn cancellation_token(&self) -> CancellationToken {
        self.cancellation.clone()
    }

    pub(crate) fn cancellation_handle(&self) -> OperationCancellationHandle {
        OperationCancellationHandle {
            shared: Arc::clone(&self.shared),
            operation_id: self.operation_id.clone(),
        }
    }

    pub(crate) fn bind_capability_generation(
        &mut self,
        generation: CapabilityGeneration,
    ) -> Result<(), CodingSessionError> {
        let mut shared = self.shared.lock_resource("operation state")?;
        shared
            .children
            .iter_mut()
            .find(|active| {
                active.kind == self.kind
                    && active.operation_id == self.operation_id
                    && active.generation == self.generation
            })
            .expect("child guard must retain its active identity")
            .capability_generation = Some(generation);
        drop(shared);
        self.snapshot_coordinator.register_operation_event_context(
            self.operation_id.clone(),
            self.kind,
            generation,
            Some(self.parent_operation_id.clone()),
            self.root_operation_id.clone(),
        )?;
        Ok(())
    }
}

impl Drop for ChildOperationGuard {
    fn drop(&mut self) {
        // Drop cannot surface the resource error; recover only to release the
        // child identity and report poison once.
        let mut shared = self.shared.lock_or_recover("operation state");
        let previous_primary = shared.activity().primary();
        let matches = |active: &ActiveChildOperation| {
            active.kind == self.kind
                && active.operation_id == self.operation_id
                && active.generation == self.generation
        };
        let Some(child) = shared.children.iter_mut().find(|child| matches(child)) else {
            return;
        };
        child.owner_released = true;
        shared.cancel_descendants(&self.operation_id);
        let mut removed = shared.remove_released_children_without_descendants();
        removed.extend(shared.remove_released_roots_without_descendants());
        let current_primary = shared.activity().primary();
        drop(shared);
        for (operation_id, generation) in removed {
            self.snapshot_coordinator
                .clear_operation_event_context_if(&operation_id, generation);
        }
        if previous_primary != current_primary {
            self.snapshot_coordinator
                .set_active_operation_from_drop(current_primary);
        }
    }
}
