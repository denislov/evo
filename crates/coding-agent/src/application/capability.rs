use std::path::PathBuf;
use std::sync::Arc;

use crate::application::snapshot::SnapshotCoordinator;
use crate::kernel::capability::{
    ActorId, CapabilityGeneration, CommandCapabilitySet, ModelCapability, SessionReadCapability,
    SessionWriteCapability, ToolCapabilitySet, UiCapability,
};
use crate::kernel::error::CodingSessionError;
use crate::kernel::operation::OperationKind;
use crate::profiles::ProfileId;
use crate::session::event::PersistedRuntimeGenerationRef;
use tool_contract::api::definition::ToolId;
use workspace_runtime::api::{WorkspaceAccessHandle, WorkspaceHandle, WorkspaceKind};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OperationCapabilitySnapshot {
    pub(crate) generation: CapabilityGeneration,
    pub(crate) operation_id: String,
    pub(crate) actor: ActorId,
    pub(crate) model: Option<ModelCapability>,
    pub(crate) tools: ToolCapabilitySet,
    pub(crate) commands: CommandCapabilitySet,
    pub(crate) workspace: Option<WorkspaceAccessHandle>,
    pub(crate) session_read: Option<SessionReadCapability>,
    pub(crate) session_write: Option<SessionWriteCapability>,
    pub(crate) ui: Option<UiCapability>,
}

impl OperationCapabilitySnapshot {
    pub(crate) fn persisted_runtime_generation_ref(&self) -> PersistedRuntimeGenerationRef {
        PersistedRuntimeGenerationRef {
            profile_id: self
                .model
                .as_ref()
                .and_then(|model| model.profile_id.clone()),
            capability_generation: Some(self.generation.get()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CapabilitySnapshotInput {
    pub(crate) operation_id: String,
    pub(crate) operation_kind: OperationKind,
    pub(crate) session_access: crate::kernel::capability::SessionCapabilityAccess,
    pub(crate) actor: ActorId,
    pub(crate) uses_model: bool,
    pub(crate) model_profile_id: Option<ProfileId>,
    pub(crate) persistent_session: bool,
    pub(crate) cwd: Option<PathBuf>,
    pub(crate) workspace_handle: Option<WorkspaceHandle>,
    pub(crate) shell_path: Option<String>,
    pub(crate) shell_command_prefix: Option<String>,
    pub(crate) runtime_tools: Vec<ToolId>,
    pub(crate) profile_tools: Vec<ToolId>,
}

#[derive(Debug, Clone)]
pub(crate) struct CapabilitySnapshotService {
    snapshot_coordinator: Arc<SnapshotCoordinator>,
}

impl CapabilitySnapshotService {
    pub(crate) fn new() -> Self {
        Self::with_snapshot_coordinator(SnapshotCoordinator::new())
    }

    pub(crate) fn with_snapshot_coordinator(
        snapshot_coordinator: Arc<SnapshotCoordinator>,
    ) -> Self {
        Self {
            snapshot_coordinator,
        }
    }

    pub(crate) fn current_generation(&self) -> Result<CapabilityGeneration, CodingSessionError> {
        self.snapshot_coordinator.current_capability_generation()
    }

    pub(crate) fn snapshot(
        &self,
        input: CapabilitySnapshotInput,
    ) -> Result<OperationCapabilitySnapshot, CodingSessionError> {
        use crate::kernel::capability::SessionCapabilityAccess;

        let writes_session = matches!(input.session_access, SessionCapabilityAccess::Write);
        let reads_session = !matches!(input.session_access, SessionCapabilityAccess::None);
        let model = input.uses_model.then_some(ModelCapability {
            profile_id: input.model_profile_id,
        });
        let allowed_tools = if input.profile_tools.is_empty() {
            Vec::new()
        } else {
            input
                .runtime_tools
                .into_iter()
                .filter(|id| input.profile_tools.iter().any(|allowed| allowed == id))
                .collect::<Vec<_>>()
        };
        let needs_workspace = matches!(
            input.operation_kind,
            OperationKind::MergeChildWorktree | OperationKind::DiscardChildWorktree
        ) || allowed_tools
            .iter()
            .any(|id| tool_uses_filesystem(id) || id.as_str() == "bash");
        let identity = match (input.workspace_handle, input.cwd) {
            (Some(handle), _) => Some(handle),
            (None, Some(cwd)) => {
                let root =
                    std::path::absolute(&cwd).map_err(|error| CodingSessionError::Resource {
                        message: format!(
                            "cannot make operation workspace root absolute ({}): {error}",
                            cwd.display()
                        ),
                    })?;
                Some(
                    WorkspaceHandle::new(WorkspaceKind::Source, root).map_err(|error| {
                        CodingSessionError::Resource {
                            message: format!(
                                "cannot construct operation workspace handle: {error}"
                            ),
                        }
                    })?,
                )
            }
            (None, None) => None,
        };
        let workspace = if needs_workspace {
            identity
                .map(|handle| {
                    WorkspaceAccessHandle::open(
                        handle,
                        input.shell_path,
                        input.shell_command_prefix,
                    )
                })
                .transpose()?
        } else {
            None
        };
        Ok(OperationCapabilitySnapshot {
            generation: self.current_generation()?,
            operation_id: input.operation_id,
            actor: input.actor,
            model,
            tools: ToolCapabilitySet::from_ids(allowed_tools),
            commands: CommandCapabilitySet::default(),
            workspace,
            session_read: reads_session.then_some(SessionReadCapability {
                persistent: input.persistent_session,
            }),
            session_write: writes_session.then_some(SessionWriteCapability {
                persistent: input.persistent_session,
            }),
            ui: None,
        })
    }
}

impl Default for CapabilitySnapshotService {
    fn default() -> Self {
        Self::new()
    }
}

pub(crate) fn tool_uses_filesystem(id: &ToolId) -> bool {
    matches!(
        id.as_str(),
        "read" | "write" | "edit" | "hashline_edit" | "apply_patch" | "grep" | "find" | "ls"
    )
}
