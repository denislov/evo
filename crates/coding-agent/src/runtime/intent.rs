use super::capability::OperationCapabilitySnapshot;
use super::control::{
    ChildOperationGuard, OperationCancellationHandle, OperationControl, OperationGuard,
    OperationKind,
};
use super::operation::{OperationClass, OperationExecution};
use super::scheduler::OperationScheduler;
use crate::runtime::facade::CodingSessionError;
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum QueryIntent {
    Capabilities,
    SessionView,
    AgentProfiles,
    TeamProfiles,
    ProfileDiagnostics,
    PendingDelegationConfirmations,
    ChangedFileReview,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct QueryIntentMetadata {
    pub(crate) intent: QueryIntent,
    pub(crate) class: OperationClass,
}

#[derive(Debug)]
#[must_use = "dropping OperationPermit releases any guarded operation"]
pub(crate) struct OperationPermit {
    guard: Option<OperationGuard>,
    _child_guard: Option<ChildOperationGuard>,
    execution: OperationExecution,
    cancellation: Option<CancellationToken>,
    cancellation_handle: Option<OperationCancellationHandle>,
    #[cfg(test)]
    kind: OperationKind,
    #[cfg(test)]
    class: OperationClass,
}

impl QueryIntent {
    pub(crate) fn metadata(self) -> QueryIntentMetadata {
        QueryIntentMetadata {
            intent: self,
            class: OperationClass::Query,
        }
    }
}

impl OperationPermit {
    pub(crate) fn guarded(
        kind: OperationKind,
        class: OperationClass,
        mut guard: OperationGuard,
        execution: OperationExecution,
    ) -> Self {
        guard.bind_capability_generation(execution.capability_generation);
        let cancellation = guard.cancellation_token();
        let cancellation_handle = Some(guard.cancellation_handle());
        #[cfg(not(test))]
        let _ = (kind, class);

        Self {
            guard: Some(guard),
            _child_guard: None,
            execution,
            cancellation,
            cancellation_handle,
            #[cfg(test)]
            kind,
            #[cfg(test)]
            class,
        }
    }

    pub(crate) fn unguarded(
        kind: OperationKind,
        class: OperationClass,
        execution: OperationExecution,
    ) -> Self {
        #[cfg(not(test))]
        let _ = (kind, class);

        Self {
            guard: None,
            _child_guard: None,
            execution,
            cancellation: None,
            cancellation_handle: None,
            #[cfg(test)]
            kind,
            #[cfg(test)]
            class,
        }
    }

    pub(crate) fn child(
        kind: OperationKind,
        execution: OperationExecution,
        mut guard: ChildOperationGuard,
    ) -> Self {
        guard.bind_capability_generation(execution.capability_generation);
        let cancellation = Some(guard.cancellation_token());
        let cancellation_handle = Some(guard.cancellation_handle());
        #[cfg(not(test))]
        let _ = kind;

        Self {
            guard: None,
            _child_guard: Some(guard),
            execution,
            cancellation,
            cancellation_handle,
            #[cfg(test)]
            kind,
            #[cfg(test)]
            class: OperationClass::Child,
        }
    }

    pub(crate) fn capability_snapshot(&self) -> &OperationCapabilitySnapshot {
        &self.execution.capability_snapshot
    }

    pub(crate) fn execution(&self) -> &OperationExecution {
        &self.execution
    }

    pub(crate) fn cancellation_token(&self) -> Option<CancellationToken> {
        self.cancellation.clone()
    }

    pub(crate) fn cancellation_handle(&self) -> Option<OperationCancellationHandle> {
        self.cancellation_handle.clone()
    }
}

impl Drop for OperationPermit {
    fn drop(&mut self) {
        let _ = self.guard.is_some();
    }
}

pub(crate) struct IntentRouter;

impl IntentRouter {
    pub(crate) fn admit_query(
        control: &OperationControl,
        intent: QueryIntent,
    ) -> QueryIntentMetadata {
        let metadata = OperationScheduler::admit_query(control, intent);
        debug_assert_eq!(metadata.class, OperationClass::Query);
        metadata
    }

    pub(crate) fn unsupported_dispatch(admission: &OperationExecution) -> CodingSessionError {
        CodingSessionError::UnsupportedCapability {
            capability: format!(
                "{} operation requires {} dispatcher",
                admission.kind.as_str(),
                admission.descriptor.dispatch_mode.dispatcher_label(),
            ),
        }
    }
}
