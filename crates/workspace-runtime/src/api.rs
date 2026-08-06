pub use crate::access::WorkspaceAccessHandle;
pub use crate::contract::{
    WorkspaceHandle, WorkspaceId, WorkspaceIdentityError, WorkspaceKind, WorkspaceLease,
    WorkspaceLeaseError, WorkspaceLifecycle,
};
pub use crate::error::WorkspaceError;
pub use crate::fs::{
    CapWalkEntry, CapWalkEntryKind, CapWalkRoot, FileMutation, FilesystemBindingDescriptor,
    FilesystemPathPreview, FilesystemReviewTargetError, FilesystemTarget, MAX_WALK_DEPTH,
    MAX_WALK_ENTRIES, MutationGuard, OpenedEditFile, walk_target,
};
pub use crate::process::{
    EnvPolicy, OutputBudget, OutputGap, ProcessOutcome, ProcessOutput, ProcessSpec,
    ProcessUpdateCallback, ProgramKind, TaskHandle, TaskId, TaskOutputChunk, TaskOwner,
    TaskRegistry, TaskReport, TaskSnapshot, TaskSpawnError, TaskState, path_exists,
    resolve_shell_path, run,
};
pub use crate::rewind::{
    WorkspaceFileSnapshot, WorkspaceRestoreEntry, WorkspaceRestoreError, WorkspaceRestorePlan,
    WorkspaceRestoreReport, WorkspaceSnapshot, WorkspaceSnapshotError, capture_workspace_snapshot,
    restore_workspace_snapshot,
};
pub use crate::sandbox::{
    CapabilityDimension, ExecPolicy, NetworkPolicy, SandboxCapability, SandboxProfile,
    SandboxUnsupported,
};
pub use crate::worktree::merge::{
    ChangeEntry, ChangeKind, ChangeSet, MergeError, MergeProposal, MergeReport, apply_merge,
    apply_merge_cancellable, build_changeset, build_changeset_cancellable,
};
pub use crate::worktree::registry::{
    GcOptions, GcRemovedWorktree, GcReport, RecoveryReport, RegistryError,
    StartupMaintenanceReport, WorktreeRecord, WorktreeRegistry,
};
pub use crate::worktree::{
    ManagedWorktree, WorkingTreeMode, WorktreeBuilder, WorktreeCreationMode, WorktreeError,
    WorktreeReport,
};
