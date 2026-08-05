use std::fmt;
use std::path::{Path, PathBuf};

use crate::contract::{WorkspaceHandle, WorkspaceKind};
use crate::error::WorkspaceError;
use crate::fs::{
    FilesystemBindingDescriptor, FilesystemCapability, FilesystemPathPreview,
    FilesystemReviewTargetError, FilesystemTarget,
};
use crate::process::ShellCapability;

/// Opaque operation-facing handle to one workspace's filesystem and shell authority.
///
/// Product crates carry this handle but cannot access the underlying directory or
/// process capability objects. Managed-child creation can replace the handle without
/// changing tool adapters or leaking platform handles across the crate boundary.
#[derive(Clone)]
pub struct WorkspaceAccessHandle {
    identity: WorkspaceHandle,
    filesystem: FilesystemCapability,
    shell: ShellCapability,
}

impl WorkspaceAccessHandle {
    pub fn open(
        identity: WorkspaceHandle,
        shell_path: Option<String>,
        command_prefix: Option<String>,
    ) -> Result<Self, WorkspaceError> {
        let cwd = identity.root().to_path_buf();
        let filesystem = FilesystemCapability::new(cwd.clone())?;
        Ok(Self {
            identity,
            filesystem,
            shell: ShellCapability::with_configuration(cwd, shell_path, command_prefix),
        })
    }

    pub fn open_source(root: impl Into<PathBuf>) -> Result<Self, WorkspaceError> {
        let root = std::path::absolute(root.into()).map_err(|error| WorkspaceError::Resource {
            message: format!("cannot make workspace root absolute: {error}"),
        })?;
        let identity = WorkspaceHandle::new(WorkspaceKind::Source, root).map_err(|error| {
            WorkspaceError::Resource {
                message: format!("cannot construct workspace identity: {error}"),
            }
        })?;
        Self::open(identity, None, None)
    }

    pub fn identity(&self) -> &WorkspaceHandle {
        &self.identity
    }

    pub fn cwd(&self) -> &Path {
        self.identity.root()
    }

    pub fn shell_path(&self) -> Option<&str> {
        self.shell.shell_path.as_deref()
    }

    pub fn command_prefix(&self) -> Option<&str> {
        self.shell.command_prefix.as_deref()
    }

    pub fn preview_path(&self, path: &str) -> Result<FilesystemPathPreview, WorkspaceError> {
        self.filesystem.preview_path(path)
    }

    pub async fn bind_tool_target(
        &self,
        operation_id: &str,
        tool_call_id: &str,
        tool_name: &str,
        path: &str,
    ) -> Result<FilesystemBindingDescriptor, WorkspaceError> {
        self.filesystem
            .bind_tool_target(operation_id, tool_call_id, tool_name, path)
            .await
    }

    pub fn take_bound_tool_target(
        &self,
        operation_id: &str,
        tool_call_id: &str,
        tool_name: &str,
        path: &str,
    ) -> Result<FilesystemTarget, WorkspaceError> {
        self.filesystem
            .take_bound_tool_target(operation_id, tool_call_id, tool_name, path)
    }

    pub fn discard_bound_tool_target(&self, operation_id: &str, tool_call_id: &str) {
        self.filesystem
            .discard_bound_tool_target(operation_id, tool_call_id);
    }

    pub fn discard_operation_bindings(&self, operation_id: &str) {
        self.filesystem.discard_operation_bindings(operation_id);
    }

    pub async fn prepare_target_for_tool(
        &self,
        tool_name: &str,
        path: &str,
    ) -> Result<FilesystemTarget, WorkspaceError> {
        self.filesystem
            .prepare_target_for_tool(tool_name, path)
            .await
    }

    pub async fn prepare_workspace_review_target(
        &self,
        path: &str,
    ) -> Result<FilesystemTarget, FilesystemReviewTargetError> {
        self.filesystem.prepare_workspace_review_target(path).await
    }
}

impl fmt::Debug for WorkspaceAccessHandle {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WorkspaceAccessHandle")
            .field("identity", &self.identity)
            .field("shell_path", &self.shell.shell_path)
            .field(
                "command_prefix",
                &self.shell.command_prefix.as_ref().map(|_| "<configured>"),
            )
            .finish()
    }
}

impl PartialEq for WorkspaceAccessHandle {
    fn eq(&self, other: &Self) -> bool {
        self.identity == other.identity && self.shell == other.shell
    }
}

impl Eq for WorkspaceAccessHandle {}
