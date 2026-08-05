use std::sync::{Mutex, MutexGuard};

use crate::error::WorkspaceError;

/// Crate-wide mutex poison policy, mirroring the product-layer policy this
/// capability code was extracted from.
///
/// Product paths use [`lock_resource`] and propagate the resulting resource
/// error. Infallible language/runtime boundaries such as `Debug` and `Drop`
/// may use [`lock_or_recover`]; that exceptional path preserves cleanup while
/// emitting a process-wide diagnostic once.
pub(super) fn lock_resource<'a, T>(
    mutex: &'a Mutex<T>,
    resource: &'static str,
) -> Result<MutexGuard<'a, T>, WorkspaceError> {
    mutex.lock().map_err(|_| WorkspaceError::Resource {
        message: format!("{resource} mutex is poisoned; its invariant may be broken"),
    })
}

pub(super) fn lock_or_recover<'a, T>(mutex: &'a Mutex<T>) -> MutexGuard<'a, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}
