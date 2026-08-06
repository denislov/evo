//! Filesystem change facts: a single-owner semantic event service and the
//! normalized event types that downstream consumers build on.
//!
//! This crate owns no product session or UI types. Raw `notify` events are
//! normalized inside `FsEventService`; consumers only see `FsEvent` values
//! with workspace-relative paths, debounced bursts, paired renames, and
//! gitignored paths filtered out.

mod error;
mod event;
mod git;
mod hunk;
mod receipt;
mod watch;

pub use error::ChangeTrackerError;
pub use event::{FsChangeKind, FsEvent, GitEvent, GitMetaEvent, SemanticEvent};
pub use hunk::{
    ChangeFactSnapshot, ChangeSource, HunkCheckpointFile, HunkCheckpointIdentity,
    HunkCheckpointVersion, HunkId, HunkRange, HunkSnapshot, HunkTracker, HunkTrackerCheckpoint,
    HunkTrackerHandle, HunkTrackerOptions, HunkTrackerSnapshot, HunkTrackingService,
    ReconcileState, RejectPlan, RejectReplacement, TrackedFileSnapshot, TrackingContext,
};
pub use receipt::ChangeReceipt;
pub use watch::{FsEventService, WatchOptions};
