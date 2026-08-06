use std::sync::Arc;

use super::client::service::ClientService;
use crate::application::capability::CapabilitySnapshotService;
use crate::application::operation::control::OperationControl;
use crate::application::operation::submission::PendingSubmissionLease;
use crate::application::session_coordinator::SessionCoordinator;
use crate::application::snapshot::SnapshotCoordinator;
use crate::profiles::ProfileRegistry;
use crate::services::authorization::AuthorizationService;
use crate::services::background::BackgroundTaskService;
use crate::services::event::EventService;
use crate::services::ports::ExtensionHostService;
use crate::services::review::ReviewService;
use crate::services::runtime::RuntimeService;
use crate::session::service::SessionPersistence;
use std::path::PathBuf;

pub(crate) struct ProjectRoot(PathBuf);

impl ProjectRoot {
    pub(crate) fn new(path: PathBuf) -> Self {
        Self(path)
    }

    pub(crate) fn as_path(&self) -> &std::path::Path {
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
pub(crate) struct RuntimeHost {
    pub(crate) operation_supervisor: OperationSupervisor,
    pub(crate) session_coordinator: SessionCoordinator,
    pub(crate) events: EventService,
    pub(crate) client_projection: ClientProjectionCoordinator,
    pub(crate) runtime_service: RuntimeService,
    pub(crate) background_tasks: BackgroundTaskService,
    pub(crate) extension_host: ExtensionHostService,
    pub(crate) profile_registry: ProfileRegistry,
    pub(crate) authorization_service: AuthorizationService,
    pub(crate) review_service: ReviewService,
    pub(crate) project_root: ProjectRoot,
}

impl RuntimeHost {
    /// 当前会话身份（session id + workspace root），extension 事件信封使用。
    pub(crate) fn session_identity(&self) -> (String, String) {
        let session_id = match &self.session_coordinator.persistence {
            SessionPersistence::Persistent(service) => service.session_id().to_owned(),
            SessionPersistence::NonPersistent(state) => state.runtime_id.clone(),
        };
        let workspace_root = self.project_root.as_path().to_string_lossy().into_owned();
        (session_id, workspace_root)
    }
}

/// Admission, immutable execution, capacity, cancellation, and capability owner.
#[derive(Debug)]
pub(crate) struct OperationSupervisor {
    pub(crate) control: OperationControl,
    pub(crate) capabilities: CapabilitySnapshotService,
}

/// Client registry, snapshot projection, controls, and reconnect overlay owner.
#[derive(Debug)]
pub(crate) struct ClientProjectionCoordinator {
    pub(crate) snapshots: Arc<SnapshotCoordinator>,
    pub(crate) clients: ClientService,
    pub(crate) pending_submission: Option<PendingSubmissionLease>,
}
