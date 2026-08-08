use super::*;

impl OperationDescriptor {
    pub(crate) fn admission_class(self) -> OperationClass {
        match (
            self.lineage,
            self.session_access,
            self.runtime_access,
            self.capacity,
        ) {
            (
                OperationLineage::Root,
                _,
                OperationRuntimeAccess::Write,
                OperationCapacity::RuntimeExclusive,
            ) => OperationClass::RuntimeWrite,
            (
                OperationLineage::Root,
                OperationSessionAccess::Write,
                _,
                OperationCapacity::SessionWriter,
            ) => OperationClass::SessionWriteRoot,
            (
                OperationLineage::Root,
                OperationSessionAccess::None,
                _,
                OperationCapacity::BoundedRuntime,
            ) => OperationClass::NonSessionRoot,
            (
                OperationLineage::Root,
                OperationSessionAccess::Read,
                _,
                OperationCapacity::Shared,
            ) => OperationClass::ReadOnly,
            (OperationLineage::Child, _, _, _) => OperationClass::Child,
            _ => unreachable!("validated descriptor must derive one admission class"),
        }
    }

    pub(crate) fn validate(self) -> Result<(), &'static str> {
        self.validate_terminal_policy()?;
        match (
            self.lineage,
            self.session_access,
            self.runtime_access,
            self.capacity,
        ) {
            (
                OperationLineage::Root,
                _,
                OperationRuntimeAccess::Write,
                OperationCapacity::RuntimeExclusive,
            )
            | (
                OperationLineage::Root,
                OperationSessionAccess::Write,
                _,
                OperationCapacity::SessionWriter,
            )
            | (
                OperationLineage::Root,
                OperationSessionAccess::None,
                _,
                OperationCapacity::BoundedRuntime,
            )
            | (
                OperationLineage::Root,
                OperationSessionAccess::Read,
                _,
                OperationCapacity::Shared,
            ) => {}
            (OperationLineage::Child, OperationSessionAccess::None, _, _) => {}
            _ => return Err("operation access and capacity claims do not derive a valid class"),
        }
        if self.durability.session_if_persistent
            && self.session_access != OperationSessionAccess::Write
        {
            return Err("session durability requires session write access");
        }
        if self.durability.runtime_generation
            && self.runtime_access != OperationRuntimeAccess::Write
        {
            return Err("runtime generation durability requires runtime write access");
        }
        match (self.dispatch_mode, self.cancellation) {
            (OperationDispatchMode::Async, OperationCancellation::Cancellable)
            | (
                OperationDispatchMode::SyncReadOnly | OperationDispatchMode::SyncMutable,
                OperationCancellation::Atomic,
            ) => {}
            _ => return Err("dispatch mode and cancellation claim conflict"),
        }
        if self.child_policy == OperationChildPolicy::Structured
            && self.cancellation != OperationCancellation::Cancellable
        {
            return Err("structured children require cancellable ownership");
        }
        Ok(())
    }

    pub(crate) fn validate_terminal_policy(self) -> Result<(), &'static str> {
        match (
            self.terminal_policy,
            self.permitted_root_evidence.is_empty(),
        ) {
            (OperationTerminalPolicy::ProductEvent, false)
            | (OperationTerminalPolicy::OutcomeAcknowledgement, true) => Ok(()),
            (OperationTerminalPolicy::ProductEvent, true) => {
                Err("ProductEvent terminal policy requires root terminal evidence")
            }
            (OperationTerminalPolicy::OutcomeAcknowledgement, false) => {
                Err("outcome acknowledgement policy forbids root terminal evidence")
            }
        }
    }

    pub(super) fn for_child(mut self) -> Option<Self> {
        if self.child_policy != OperationChildPolicy::Structured
            || self.dispatch_mode != OperationDispatchMode::Async
            || self.cancellation != OperationCancellation::Cancellable
        {
            return None;
        }
        self.lineage = OperationLineage::Child;
        self.session_access = OperationSessionAccess::None;
        self.runtime_access = OperationRuntimeAccess::Read;
        self.capacity = OperationCapacity::BoundedRuntime;
        self.durability = OperationDurability::NONE;
        debug_assert_eq!(self.validate(), Ok(()));
        Some(self)
    }
}
