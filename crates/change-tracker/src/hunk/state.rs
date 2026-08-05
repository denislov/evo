use super::TrackedFileSnapshot;
use super::diff::HunkIdentity;
use super::observation::ObservedFile;

#[derive(Clone)]
pub(super) struct FileVersion {
    pub(super) exists: bool,
    pub(super) revision: String,
    pub(super) content: Option<Vec<u8>>,
}

impl FileVersion {
    pub(super) fn missing(revision: String) -> Self {
        Self {
            exists: false,
            revision,
            content: Some(Vec::new()),
        }
    }

    pub(super) fn existing(revision: String, content: Option<Vec<u8>>) -> Self {
        Self {
            exists: true,
            revision,
            content,
        }
    }

    pub(super) fn from_observed(observed: ObservedFile) -> Self {
        Self {
            exists: observed.exists,
            revision: observed.revision,
            content: observed.content,
        }
    }

    pub(super) fn same_identity(&self, other: &Self) -> bool {
        self.exists == other.exists && self.revision == other.revision
    }
}

#[derive(Clone, Default)]
pub(super) struct FileState {
    pub(super) snapshot: Option<TrackedFileSnapshot>,
    pub(super) baseline: Option<FileVersion>,
    pub(super) current: Option<FileVersion>,
    pub(super) identities: Vec<HunkIdentity>,
    pub(super) agent_touched: bool,
    pub(super) target_fingerprint: Option<String>,
    pub(super) mutation_kind: String,
}
