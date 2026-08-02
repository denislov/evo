use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, MutexGuard, PoisonError};

use crate::kernel::error::CodingSessionError;

static POISON_RECOVERY_DIAGNOSTIC_EMITTED: AtomicBool = AtomicBool::new(false);
static INFALLIBLE_RESOURCE_DIAGNOSTIC_EMITTED: AtomicBool = AtomicBool::new(false);

pub(crate) fn report_infallible_resource_error<T>(
    context: &'static str,
    result: Result<T, CodingSessionError>,
) {
    if let Err(error) = result
        && !INFALLIBLE_RESOURCE_DIAGNOSTIC_EMITTED.swap(true, Ordering::Relaxed)
    {
        eprintln!(
            "coding-agent diagnostic: {context} could not report a resource failure: {error}"
        );
    }
}

pub(crate) fn recover_poisoned<T>(resource: &'static str, poisoned: PoisonError<T>) -> T {
    if !POISON_RECOVERY_DIAGNOSTIC_EMITTED.swap(true, Ordering::Relaxed) {
        eprintln!(
            "coding-agent diagnostic: recovering poisoned {resource} mutex at an infallible boundary"
        );
    }
    poisoned.into_inner()
}

/// Crate-wide mutex poison policy.
///
/// Product paths must use [`MutexExt::lock_resource`] and propagate the
/// resulting resource error. Infallible language/runtime boundaries such as
/// `Debug` and `Drop` may use [`MutexExt::lock_or_recover`]; that exceptional
/// path preserves cleanup while emitting a process-wide diagnostic once.
pub(crate) trait MutexExt<T> {
    fn lock_resource(
        &self,
        resource: &'static str,
    ) -> Result<MutexGuard<'_, T>, CodingSessionError>;

    fn lock_or_recover(&self, resource: &'static str) -> MutexGuard<'_, T>;
}

impl<T> MutexExt<T> for Mutex<T> {
    fn lock_resource(
        &self,
        resource: &'static str,
    ) -> Result<MutexGuard<'_, T>, CodingSessionError> {
        self.lock().map_err(|_| CodingSessionError::Resource {
            message: format!("{resource} mutex is poisoned; its invariant may be broken"),
        })
    }

    fn lock_or_recover(&self, resource: &'static str) -> MutexGuard<'_, T> {
        self.lock()
            .unwrap_or_else(|poisoned| recover_poisoned(resource, poisoned))
    }
}

#[cfg(test)]
mod tests {
    use std::panic::{AssertUnwindSafe, catch_unwind};

    use super::*;

    #[test]
    fn poisoned_mutex_maps_to_a_resource_error() {
        let mutex = Mutex::new(());
        let _ = catch_unwind(AssertUnwindSafe(|| {
            let _guard = mutex.lock_or_recover("test state");
            panic!("poison test state");
        }));

        let error = mutex.lock_resource("test state").unwrap_err();
        assert!(matches!(error, CodingSessionError::Resource { .. }));
        assert!(error.to_string().contains("test state mutex is poisoned"));
    }

    #[test]
    fn infallible_boundary_recovers_the_inner_value() {
        let mutex = Mutex::new(41_u8);
        let _ = catch_unwind(AssertUnwindSafe(|| {
            let mut guard = mutex.lock_or_recover("test cleanup state");
            *guard = 42;
            panic!("poison test cleanup state");
        }));

        assert_eq!(*mutex.lock_or_recover("test cleanup state"), 42);
    }
}
