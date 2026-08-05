use super::*;

impl OperationState {
    pub(crate) fn with_snapshot_coordinator(
        snapshot_coordinator: Arc<SnapshotCoordinator>,
    ) -> Self {
        Self {
            shared: Arc::new(Mutex::new(OperationStateInner {
                session_write: None,
                non_session_roots: Vec::new(),
                runtime_write: None,
                children: Vec::new(),
                non_session_root_limit: DEFAULT_RUNTIME_ROOT_LIMIT,
                next_generation: 1,
            })),
            snapshot_coordinator,
        }
    }

    pub(crate) fn activity(&self) -> Result<OperationActivity, CodingSessionError> {
        Ok(self.shared.lock_resource("operation state")?.activity())
    }

    pub(super) fn ensure_active_target(
        &self,
        expected_kind: OperationKind,
        expected_operation_id: &str,
    ) -> Result<(), OperationIdentityRejection> {
        let shared = self.shared.lock_resource("operation state")?;
        let Some(active) = shared.root_identities().find(|active| {
            !active.owner_released
                && active.kind == expected_kind
                && active.operation_id == expected_operation_id
        }) else {
            if let Some(active) = shared
                .root_identities()
                .find(|active| !active.owner_released && active.kind == expected_kind)
            {
                return Err(OperationIdentityRejection::TargetMismatch {
                    kind: expected_kind,
                    expected_operation_id: expected_operation_id.to_owned(),
                    active_operation_id: active.operation_id.clone(),
                });
            }
            if let Some(active) = shared
                .root_identities()
                .find(|active| !active.owner_released)
            {
                return Err(OperationIdentityRejection::KindMismatch {
                    expected_kind,
                    active_kind: active.kind,
                    expected_operation_id: expected_operation_id.to_owned(),
                });
            }
            return Err(OperationIdentityRejection::NoActiveOperation {
                expected_kind,
                expected_operation_id: expected_operation_id.to_owned(),
            });
        };
        debug_assert_eq!(active.kind, expected_kind);
        debug_assert_eq!(active.operation_id, expected_operation_id);
        Ok(())
    }

    pub(crate) fn begin_root_with_capability_generation(
        &self,
        class: OperationClass,
        kind: OperationKind,
        operation_id: String,
        capability_generation: CapabilityGeneration,
    ) -> Result<OperationGuard, CodingSessionError> {
        self.begin_root_inner(class, kind, operation_id, Some(capability_generation))
    }

    pub(super) fn begin_root_inner(
        &self,
        class: OperationClass,
        kind: OperationKind,
        operation_id: String,
        capability_generation: Option<CapabilityGeneration>,
    ) -> Result<OperationGuard, CodingSessionError> {
        let mut shared = self.shared.lock_resource("operation state")?;
        let current_generation = self.snapshot_coordinator.current_capability_generation()?;
        if capability_generation.is_some_and(|generation| generation < current_generation) {
            return Err(CodingSessionError::UnsupportedCapability {
                capability: format!(
                    "operation {operation_id} was admitted with a stale capability generation"
                ),
            });
        }
        if let Some(active_kind) = shared.operation_kind_for_id(&operation_id) {
            return Err(CodingSessionError::Busy {
                operation: active_kind.as_str().into(),
            });
        }
        let activity = shared.activity();
        let blocker = match class {
            OperationClass::SessionWriteRoot => activity.session_write_blocker(),
            OperationClass::NonSessionRoot => activity.non_session_root_blocker(),
            OperationClass::RuntimeWrite => activity.runtime_write_blocker(),
            OperationClass::Query | OperationClass::ReadOnly | OperationClass::Child => {
                return Err(CodingSessionError::UnsupportedCapability {
                    capability: format!("{class:?} does not occupy a root operation slot"),
                });
            }
        };
        if let Some(active) = blocker {
            return Err(CodingSessionError::Busy {
                operation: active.as_str().into(),
            });
        }
        let previous_primary = activity.primary();
        let generation = shared.next_generation;
        shared.next_generation = shared.next_generation.saturating_add(1);
        let cancellation = CancellationToken::new();
        let identity = ActiveOperationIdentity {
            kind,
            operation_id: operation_id.clone(),
            generation,
            capability_generation,
            cancellation: cancellation.clone(),
            cancellation_open: true,
            owner_released: false,
        };
        match class {
            OperationClass::SessionWriteRoot => shared.session_write = Some(identity),
            OperationClass::NonSessionRoot => shared.non_session_roots.push(identity),
            OperationClass::RuntimeWrite => shared.runtime_write = Some(identity),
            OperationClass::Query | OperationClass::ReadOnly | OperationClass::Child => {
                unreachable!("root class validated above")
            }
        }
        let current_primary = shared.activity().primary();
        drop(shared);
        if previous_primary != current_primary {
            self.snapshot_coordinator
                .set_active_operation(current_primary)?;
        }
        if let Some(capability_generation) = capability_generation {
            self.snapshot_coordinator.register_operation_event_context(
                operation_id.clone(),
                kind,
                capability_generation,
                None,
                operation_id.clone(),
            )?;
        }
        Ok(OperationGuard {
            shared: Arc::clone(&self.shared),
            snapshot_coordinator: Arc::clone(&self.snapshot_coordinator),
            class,
            kind,
            operation_id,
            generation,
            cancellation: Some(cancellation),
        })
    }

    pub(crate) fn begin_child_with_capability_generation(
        &self,
        kind: OperationKind,
        operation_id: String,
        parent_operation_id: String,
        capability_generation: CapabilityGeneration,
    ) -> Result<ChildOperationGuard, CodingSessionError> {
        self.begin_child_inner(
            kind,
            operation_id,
            parent_operation_id,
            Some(capability_generation),
        )
    }

    pub(super) fn begin_child_inner(
        &self,
        kind: OperationKind,
        operation_id: String,
        parent_operation_id: String,
        capability_generation: Option<CapabilityGeneration>,
    ) -> Result<ChildOperationGuard, CodingSessionError> {
        let mut shared = self.shared.lock_resource("operation state")?;
        let current_generation = self.snapshot_coordinator.current_capability_generation()?;
        if capability_generation.is_some_and(|generation| generation < current_generation) {
            return Err(CodingSessionError::UnsupportedCapability {
                capability: format!(
                    "child operation {operation_id} was admitted with a stale capability generation"
                ),
            });
        }
        if let Some(active_kind) = shared.operation_kind_for_id(&operation_id) {
            return Err(CodingSessionError::Busy {
                operation: active_kind.as_str().into(),
            });
        }
        if !shared.parent_is_active(&parent_operation_id) {
            return Err(CodingSessionError::UnsupportedCapability {
                capability: format!(
                    "child operation {operation_id} requires active parent {parent_operation_id}"
                ),
            });
        }
        let root_operation_id = shared
            .root_operation_id_for(&parent_operation_id)
            .expect("active parent must resolve to a root operation");
        let generation = shared.next_generation;
        shared.next_generation = shared.next_generation.saturating_add(1);
        let cancellation = CancellationToken::new();
        shared.children.push(ActiveChildOperation {
            kind,
            operation_id: operation_id.clone(),
            parent_operation_id: parent_operation_id.clone(),
            generation,
            capability_generation,
            cancellation: cancellation.clone(),
            cancellation_open: true,
            owner_released: false,
        });
        if let Some(capability_generation) = capability_generation {
            self.snapshot_coordinator.register_operation_event_context(
                operation_id.clone(),
                kind,
                capability_generation,
                Some(parent_operation_id.clone()),
                root_operation_id.clone(),
            )?;
        }
        Ok(ChildOperationGuard {
            shared: Arc::clone(&self.shared),
            snapshot_coordinator: Arc::clone(&self.snapshot_coordinator),
            kind,
            operation_id,
            parent_operation_id,
            root_operation_id,
            generation,
            cancellation,
        })
    }

    pub(crate) fn cancel_capability_generations_before(
        &self,
        generation: CapabilityGeneration,
    ) -> Result<Vec<String>, CodingSessionError> {
        Ok(self
            .shared
            .lock_resource("operation state")?
            .cancel_capability_generations_before(generation))
    }
}

#[derive(Debug, Clone)]
pub(crate) struct OperationControl {
    pub(super) state: OperationState,
    pub(super) prompt_control: PromptControlState,
    pub(super) worktree_registry: Option<Arc<workspace_runtime::api::WorktreeRegistry>>,
}

impl OperationControl {
    pub(crate) fn worktree_registry(
        &self,
    ) -> Option<&Arc<workspace_runtime::api::WorktreeRegistry>> {
        self.worktree_registry.as_ref()
    }
}
