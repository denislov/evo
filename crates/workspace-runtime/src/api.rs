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
    EnvPolicy, OutputBudget, ProcessOutcome, ProcessOutput, ProcessSpec, ProcessUpdateCallback,
    ProgramKind, path_exists, resolve_shell_path, run,
};
pub use crate::worktree::registry::{
    GcOptions, GcRemovedWorktree, GcReport, RecoveryReport, RegistryError, WorktreeRecord,
    WorktreeRegistry,
};
pub use crate::worktree::{
    ManagedWorktree, WorkingTreeMode, WorktreeBuilder, WorktreeCreationMode, WorktreeError,
    WorktreeReport,
};
