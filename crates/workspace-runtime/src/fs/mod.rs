//! Bounded filesystem capability: authorization-bound targets, mutation
//! fences, and directory walk support over `cap-std` handles.

mod cap_walk;
mod capability;
mod edit_file;
mod mutation;

pub use cap_walk::{
    CapWalkEntry, CapWalkEntryKind, CapWalkRoot, MAX_WALK_DEPTH, MAX_WALK_ENTRIES, walk_target,
};
pub(crate) use capability::FilesystemCapability;
pub use capability::{
    FilesystemBindingDescriptor, FilesystemPathPreview, FilesystemReviewTargetError,
    FilesystemTarget,
};
pub use edit_file::OpenedEditFile;
pub use mutation::{FileMutation, MutationGuard};
