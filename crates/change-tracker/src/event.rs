//! Semantic filesystem events and normalized git metadata events.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

/// Classification of one workspace-relative change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FsChangeKind {
    /// A file or directory appeared.
    Created,
    /// Contents or metadata of a file or directory changed.
    Modified,
    /// A file or directory disappeared.
    Removed,
    /// A path moved; `from` and `to` are both workspace-relative.
    Renamed,
}

/// A normalized, workspace-relative change. Consumers never see raw
/// `notify` types; the path is relative to the watched workspace root and
/// the sequence number is monotonic per service.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SemanticEvent {
    pub sequence: u64,
    pub root: PathBuf,
    /// Workspace-relative path. For `Renamed`, this is the destination; the
    /// source is in `from`.
    pub path: PathBuf,
    /// Workspace-relative source path, present only for `Renamed`.
    pub from: Option<PathBuf>,
    pub kind: FsChangeKind,
    pub at: SystemTime,
}

impl SemanticEvent {
    pub(crate) fn new(
        sequence: u64,
        root: &Path,
        path: PathBuf,
        from: Option<PathBuf>,
        kind: FsChangeKind,
    ) -> Self {
        Self {
            sequence,
            root: root.to_path_buf(),
            path,
            from,
            kind,
            at: SystemTime::now(),
        }
    }
}

/// Normalized state change inside the git metadata directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GitMetaEvent {
    /// HEAD or a ref moved (commit, checkout, merge, reset).
    HeadMoved,
    /// The index was updated (staging or unstaging).
    IndexChanged,
    /// A git write operation started (a `.lock` or operation marker appeared).
    OperationStarted,
    /// A git write operation finished (the lock or marker disappeared).
    OperationCompleted,
}

/// One item on the change stream: either a workspace change or git metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FsEvent {
    Workspace(SemanticEvent),
    Git(GitMetaEvent),
    /// The raw event channel overflowed. `lost` is the number of events that
    /// were dropped and never normalized; consumers must reconcile.
    WatchGap {
        lost: u64,
    },
}
