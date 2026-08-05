//! Normalization of git metadata directory changes.

use std::path::Path;

use notify::event::EventKind;

use crate::event::GitMetaEvent;

/// Files whose modification signals that HEAD or a ref moved.
const REF_LIKE_FILES: [&str; 5] = [
    "HEAD",
    "ORIG_HEAD",
    "FETCH_HEAD",
    "MERGED_HEAD",
    "packed-refs",
];
/// Files whose modification signals index activity.
const INDEX_LIKE_FILES: [&str; 3] = ["index", "index.json", "shallow"];
/// Marker files that indicate an in-progress operation while present.
const OPERATION_MARKERS: [&str; 6] = [
    "MERGE_HEAD",
    "CHERRY_PICK_HEAD",
    "REVERT_HEAD",
    "BISECT_HEAD",
    "AUTO_MERGE",
    "sequencer",
];

/// Classify one raw change under the git metadata directory.
///
/// `relative` is the path relative to the gitdir. Lock files map to
/// operation lifecycle; HEAD/refs changes map to `HeadMoved`; index changes
/// map to `IndexChanged`. Returns `None` for uninteresting changes.
pub(super) fn classify(relative: &Path, kind: &EventKind) -> Option<GitMetaEvent> {
    let name = relative.file_name()?.to_string_lossy();
    if relative
        .extension()
        .is_some_and(|extension| extension == "lock")
    {
        return match kind {
            EventKind::Create(_) => Some(GitMetaEvent::OperationStarted),
            EventKind::Remove(_) => Some(GitMetaEvent::OperationCompleted),
            _ => None,
        };
    }
    match kind {
        EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_) => {
            let is_remove = matches!(kind, EventKind::Remove(_));
            if OPERATION_MARKERS.contains(&name.as_ref()) {
                return Some(if is_remove {
                    GitMetaEvent::OperationCompleted
                } else {
                    GitMetaEvent::OperationStarted
                });
            }
            if REF_LIKE_FILES.contains(&name.as_ref()) {
                return Some(GitMetaEvent::HeadMoved);
            }
            if INDEX_LIKE_FILES.contains(&name.as_ref()) {
                return Some(GitMetaEvent::IndexChanged);
            }
            if relative
                .parent()
                .is_some_and(|parent| parent.file_name().is_some_and(|name| name == "refs"))
            {
                return Some(GitMetaEvent::HeadMoved);
            }
            None
        }
        _ => None,
    }
}
