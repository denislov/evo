use tokio_util::sync::CancellationToken;

use super::OperationExecution;
use super::control::{ChildOperationGuard, OperationCancellationHandle, OperationGuard};
use crate::runtime::capability::OperationCapabilitySnapshot;

#[derive(Debug)]
#[must_use = "dropping OperationPermit releases any guarded operation"]
pub(crate) struct OperationPermit {
    guard: Option<OperationGuard>,
    child_guard: Option<ChildOperationGuard>,
    execution: OperationExecution,
    cancellation: Option<CancellationToken>,
    cancellation_handle: Option<OperationCancellationHandle>,
}

impl OperationPermit {
    pub(crate) fn guarded(mut guard: OperationGuard, execution: OperationExecution) -> Self {
        guard.bind_capability_generation(execution.capability_generation);
        let cancellation = guard.cancellation_token();
        let cancellation_handle = Some(guard.cancellation_handle());

        Self {
            guard: Some(guard),
            child_guard: None,
            execution,
            cancellation,
            cancellation_handle,
        }
    }

    pub(crate) fn unguarded(execution: OperationExecution) -> Self {
        Self {
            guard: None,
            child_guard: None,
            execution,
            cancellation: None,
            cancellation_handle: None,
        }
    }

    pub(crate) fn child(execution: OperationExecution, mut guard: ChildOperationGuard) -> Self {
        guard.bind_capability_generation(execution.capability_generation);
        let cancellation = Some(guard.cancellation_token());
        let cancellation_handle = Some(guard.cancellation_handle());

        Self {
            guard: None,
            child_guard: Some(guard),
            execution,
            cancellation,
            cancellation_handle,
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

    /// Release admission ownership early while retaining immutable execution
    /// metadata for finalization. Session forking is the sole caller: the old
    /// session guard must be gone before the writer switches to the new session.
    pub(crate) fn release(&mut self) {
        self.guard.take();
        self.child_guard.take();
    }
}

impl Drop for OperationPermit {
    fn drop(&mut self) {
        let _ = self.guard.is_some();
    }
}
