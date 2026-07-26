use std::sync::Arc;

use super::capability::CapabilitySnapshotService;
use super::client::service::ClientService;
use super::control::OperationControl;
use super::finalization::OperationFinalizer;
use super::session_coordinator::SessionCoordinator;
use super::snapshot::SnapshotCoordinator;
use super::submission::PendingSubmissionLease;
use crate::profiles::ProfileRegistry;
use crate::services::authorization::AuthorizationService;
use crate::services::capability::CapabilityService;
use crate::services::event::EventService;
use crate::services::runtime::RuntimeService;
use std::path::PathBuf;

pub(super) struct ProjectRoot(PathBuf);

impl ProjectRoot {
    pub(super) fn new(path: PathBuf) -> Self {
        Self(path)
    }

    pub(super) fn as_path(&self) -> &std::path::Path {
        &self.0
    }
}

impl std::fmt::Debug for ProjectRoot {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("<project-root>")
    }
}

/// Composition and lifetime owner for the product runtime.
///
/// Workflows receive only the narrow collaborator they need; this value must not
/// be passed into operations as a mutable service container.
#[derive(Debug)]
pub(super) struct RuntimeHost {
    pub(super) operation_supervisor: OperationSupervisor,
    pub(super) session_coordinator: SessionCoordinator,
    pub(super) event_hub: EventHub,
    pub(super) client_projection: ClientProjectionCoordinator,
    pub(super) runtime_service: RuntimeService,
    pub(super) capability_service: CapabilityService,
    pub(super) profile_registry: ProfileRegistry,
    pub(super) authorization_service: AuthorizationService,
    pub(super) project_root: ProjectRoot,
}

/// Admission, immutable execution, capacity, cancellation, and capability owner.
#[derive(Debug)]
pub(super) struct OperationSupervisor {
    pub(super) control: OperationControl,
    pub(super) capabilities: CapabilitySnapshotService,
    pub(super) finalizer: OperationFinalizer,
}

/// Bounded product-event sequencing and fan-out owner.
#[derive(Debug)]
pub(super) struct EventHub {
    pub(super) service: EventService,
}

/// Client registry, snapshot projection, controls, and reconnect overlay owner.
#[derive(Debug)]
pub(super) struct ClientProjectionCoordinator {
    pub(super) coordinator: Arc<SnapshotCoordinator>,
    pub(super) clients: ClientService,
    pub(super) pending_submission: Option<PendingSubmissionLease>,
}
