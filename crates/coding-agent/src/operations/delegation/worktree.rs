//! Child workspace isolation for delegation and team invocation.
//!
//! ARC-330: every delegation/team child with write authority gets its own
//! managed worktree instead of cloning the parent's workspace capability.
//! Read-only children share the parent read path (their tool set already
//! forbids writes), projectless children carry no workspace at all, and a
//! missing registry fails closed: isolation must never silently degrade.

use std::path::Path;
use std::sync::Arc;

use tokio_util::sync::CancellationToken;
use tool_contract::api::definition::ToolId;
use workspace_runtime::api::{WorkspaceHandle, WorkspaceKind, WorktreeRecord, WorktreeRegistry};

use crate::application::capability::OperationCapabilitySnapshot;
use crate::application::operation::control::OperationControl;
use crate::kernel::error::CodingSessionError;
use crate::profiles::AgentProfile;

/// Whether a tool can mutate the workspace (including arbitrary shell writes).
fn tool_writes_workspace(id: &ToolId) -> bool {
    matches!(
        id.as_str(),
        "write" | "edit" | "hashline_edit" | "apply_patch" | "bash"
    )
}

/// How a child operation's filesystem authority is provisioned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ChildWorkspacePolicy {
    /// Child carries write/bash tools and must run in its own managed worktree.
    Managed,
    /// Child carries only read-only tools and shares the parent read path.
    ReadOnlyShared,
    /// Parent has no workspace authority; the child has none either.
    Projectless,
}

impl ChildWorkspacePolicy {
    pub(crate) fn decide(parent: &OperationCapabilitySnapshot, profile: &AgentProfile) -> Self {
        if parent.workspace.is_none() {
            return Self::Projectless;
        }
        if profile.tools.iter().any(tool_writes_workspace) {
            Self::Managed
        } else {
            Self::ReadOnlyShared
        }
    }
}

/// The workspace authority bound into a child capability snapshot.
#[derive(Debug, Clone)]
pub(crate) enum ChildWorkspaceBinding {
    /// No workspace authority.
    None,
    /// The child's own managed worktree.
    Managed(WorkspaceHandle),
    /// Shared parent read path; the child tool set enforces read-only.
    ReadOnlyShared,
}

impl ChildWorkspaceBinding {
    pub(crate) fn as_managed(&self) -> Option<&WorkspaceHandle> {
        match self {
            Self::Managed(handle) => Some(handle),
            Self::None | Self::ReadOnlyShared => None,
        }
    }
}

/// Durable ownership of one child managed worktree.
///
/// The worktree is materialized before the child runs and reclaimed when the
/// child reaches a terminal state. The guard's `Drop` retries reclamation so
/// a cancelled or panicking child cannot leak an isolated worktree silently.
#[derive(Clone, Debug)]
pub(crate) struct ChildWorktreeLease {
    registry: Arc<WorktreeRegistry>,
    record: WorktreeRecord,
    released: bool,
}

impl ChildWorktreeLease {
    pub(crate) fn root(&self) -> &Path {
        self.record.dest.as_path()
    }

    /// The handle identity bound to this worktree (id == directory == record).
    pub(crate) fn handle(&self) -> Result<WorkspaceHandle, CodingSessionError> {
        let id =
            workspace_runtime::api::WorkspaceId::parse(self.record.id.clone()).map_err(|_| {
                CodingSessionError::Resource {
                    message: format!(
                        "child worktree record has an invalid identity: {}",
                        self.record.id
                    ),
                }
            })?;
        WorkspaceHandle::with_explicit_id(id, WorkspaceKind::ManagedChild, self.record.dest.clone())
            .map_err(|error| CodingSessionError::Resource {
                message: format!(
                    "cannot construct child worktree handle {}: {error}",
                    self.record.dest.display()
                ),
            })
    }

    /// Reclaim the worktree after the child reaches a terminal state.
    pub(crate) fn release(&mut self) -> Result<(), CodingSessionError> {
        if self.released {
            return Ok(());
        }
        self.released = true;
        self.registry
            .discard(&self.record.id)
            .map_err(|error| CodingSessionError::Resource {
                message: format!("cannot release child worktree {}: {error}", self.record.id),
            })
    }
}

impl Drop for ChildWorktreeLease {
    fn drop(&mut self) {
        if !self.released {
            let _ = self.registry.discard(&self.record.id);
        }
    }
}

/// Acquire a managed child worktree for `owner_operation`.
///
/// Fails closed when no registry is configured: child isolation is not
/// optional. Materialization runs on the blocking pool.
pub(crate) async fn acquire_child_worktree(
    control: &OperationControl,
    source: &WorkspaceHandle,
    owner_operation: &str,
    parent_session: Option<&str>,
    cancellation: &CancellationToken,
) -> Result<ChildWorktreeLease, CodingSessionError> {
    let registry = control.worktree_registry().cloned().ok_or_else(|| {
        CodingSessionError::UnsupportedCapability {
            capability: "child workspace isolation requires a managed worktree registry".into(),
        }
    })?;
    let source = source.clone();
    let owner_operation = owner_operation.to_owned();
    let parent_session = parent_session.map(str::to_owned);
    let cancellation = cancellation.clone();
    let registry_for_builder = registry.clone();
    let record = tokio::task::spawn_blocking(move || {
        registry_for_builder.create_managed(
            &source,
            &owner_operation,
            parent_session.as_deref(),
            workspace_runtime::api::WorkingTreeMode::PreserveWorkingTree,
            &cancellation,
        )
    })
    .await
    .map_err(|error| CodingSessionError::Session {
        message: format!("worktree creation worker failed: {error}"),
    })?
    .map_err(|error| CodingSessionError::Resource {
        message: format!("cannot acquire child worktree: {error}"),
    })?;
    Ok(ChildWorktreeLease {
        registry,
        record,
        released: false,
    })
}

/// Resolve a child workspace binding for the given policy, acquiring a
/// managed worktree when the policy demands isolation.
pub(crate) async fn bind_child_workspace(
    control: &OperationControl,
    parent: &OperationCapabilitySnapshot,
    owner_operation: &str,
    parent_session: Option<&str>,
    cancellation: &CancellationToken,
    policy: ChildWorkspacePolicy,
) -> Result<Option<ChildWorktreeLease>, CodingSessionError> {
    match policy {
        ChildWorkspacePolicy::Projectless => Ok(None),
        ChildWorkspacePolicy::ReadOnlyShared => Ok(None),
        ChildWorkspacePolicy::Managed => {
            let source = parent
                .workspace
                .as_ref()
                .map(|workspace| workspace.identity().clone())
                .ok_or_else(|| CodingSessionError::UnsupportedCapability {
                    capability: "managed child worktree requires a parent workspace".into(),
                })?;
            let lease = acquire_child_worktree(
                control,
                &source,
                owner_operation,
                parent_session,
                cancellation,
            )
            .await?;
            Ok(Some(lease))
        }
    }
}
