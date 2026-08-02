use std::path::PathBuf;
use std::sync::Arc;

use crate::application::snapshot::SnapshotCoordinator;
use crate::kernel::capability::{
    ActorId, CapabilityGeneration, CommandCapabilitySet, ModelCapability, SessionReadCapability,
    SessionWriteCapability, ToolCapabilitySet, UiCapability,
};
use crate::kernel::error::CodingSessionError;
use crate::kernel::operation::OperationKind;
use crate::platform::fs::capability::FilesystemCapability;
use crate::platform::process::ShellCapability;
use crate::profiles::ProfileId;
use crate::session::event::PersistedRuntimeGenerationRef;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OperationCapabilitySnapshot {
    pub(crate) generation: CapabilityGeneration,
    pub(crate) operation_id: String,
    pub(crate) actor: ActorId,
    pub(crate) model: Option<ModelCapability>,
    pub(crate) tools: ToolCapabilitySet,
    pub(crate) commands: CommandCapabilitySet,
    pub(crate) filesystem: Option<FilesystemCapability>,
    pub(crate) shell: Option<ShellCapability>,
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
    pub(crate) shell_path: Option<String>,
    pub(crate) shell_command_prefix: Option<String>,
    pub(crate) runtime_tools: Vec<String>,
    pub(crate) profile_tools: Vec<String>,
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
                .filter(|name| input.profile_tools.iter().any(|allowed| allowed == name))
                .collect::<Vec<_>>()
        };
        let cwd = input.cwd;
        let filesystem = cwd
            .as_ref()
            .filter(|_| allowed_tools.iter().any(|name| tool_uses_filesystem(name)))
            .map(|cwd| FilesystemCapability::new(cwd.clone()))
            .transpose()?;
        let shell = cwd
            .as_ref()
            .filter(|_| allowed_tools.iter().any(|name| name == "bash"))
            .map(|cwd| {
                ShellCapability::with_configuration(
                    cwd.clone(),
                    input.shell_path,
                    input.shell_command_prefix,
                )
            });
        Ok(OperationCapabilitySnapshot {
            generation: self.current_generation()?,
            operation_id: input.operation_id,
            actor: input.actor,
            model,
            tools: ToolCapabilitySet::from_names(allowed_tools),
            commands: CommandCapabilitySet::default(),
            filesystem,
            shell,
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

pub(crate) fn tool_uses_filesystem(name: &str) -> bool {
    matches!(name, "read" | "write" | "edit" | "grep" | "find" | "ls")
}
