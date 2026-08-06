use std::collections::{BTreeMap, VecDeque};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::diff::HunkIdentity;
use super::observation::{normalize_relative, read_observed, revision};
use super::state::{FileState, FileVersion};
use super::{
    ActorState, ChangeFactSnapshot, ChangeSource, ChangeTrackerError, HunkId, HunkRange,
    HunkTrackerSnapshot, ReconcileState, TrackedFileSnapshot, TrackingContext,
};

const CHECKPOINT_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HunkTrackerCheckpoint {
    pub version: u32,
    pub files: Vec<HunkCheckpointFile>,
    pub facts: Vec<ChangeFactSnapshot>,
    pub reconcile: ReconcileState,
    pub next_hunk: u64,
    pub next_fact: u64,
    pub history_bytes: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HunkCheckpointFile {
    pub path: PathBuf,
    pub snapshot: Option<TrackedFileSnapshot>,
    pub baseline: Option<HunkCheckpointVersion>,
    pub current: Option<HunkCheckpointVersion>,
    pub identities: Vec<HunkCheckpointIdentity>,
    pub agent_touched: bool,
    pub target_fingerprint: Option<String>,
    pub mutation_kind: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HunkCheckpointVersion {
    pub exists: bool,
    pub revision: String,
    pub content: Option<Vec<u8>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HunkCheckpointIdentity {
    pub id: HunkId,
    pub fingerprint: String,
    pub range: HunkRange,
    pub source: ChangeSource,
    pub context: Option<TrackingContext>,
    pub before_revision: Option<String>,
    pub after_revision: String,
}

impl HunkTrackerCheckpoint {
    pub fn snapshot(&self) -> HunkTrackerSnapshot {
        HunkTrackerSnapshot {
            files: self
                .files
                .iter()
                .filter_map(|file| file.snapshot.clone())
                .collect(),
            facts: self.facts.clone(),
            reconcile: self.reconcile,
            pending_receipts: 0,
            pending_events: 0,
        }
    }

    pub fn file(&self, path: &Path) -> Option<&HunkCheckpointFile> {
        self.files.iter().find(|file| file.path == path)
    }
}

impl From<FileVersion> for HunkCheckpointVersion {
    fn from(version: FileVersion) -> Self {
        Self {
            exists: version.exists,
            revision: version.revision,
            content: version.content,
        }
    }
}

impl From<HunkCheckpointVersion> for FileVersion {
    fn from(version: HunkCheckpointVersion) -> Self {
        Self {
            exists: version.exists,
            revision: version.revision,
            content: version.content,
        }
    }
}

impl From<HunkIdentity> for HunkCheckpointIdentity {
    fn from(identity: HunkIdentity) -> Self {
        Self {
            id: identity.id,
            fingerprint: identity.fingerprint,
            range: identity.range,
            source: identity.source,
            context: identity.context,
            before_revision: identity.before_revision,
            after_revision: identity.after_revision,
        }
    }
}

impl From<HunkCheckpointIdentity> for HunkIdentity {
    fn from(identity: HunkCheckpointIdentity) -> Self {
        Self {
            id: identity.id,
            fingerprint: identity.fingerprint,
            range: identity.range,
            source: identity.source,
            context: identity.context,
            before_revision: identity.before_revision,
            after_revision: identity.after_revision,
        }
    }
}

impl ActorState {
    pub(super) fn checkpoint(&mut self) -> Result<HunkTrackerCheckpoint, ChangeTrackerError> {
        self.flush_all_events()?;
        if !matches!(self.reconcile, ReconcileState::Ready) {
            return Err(ChangeTrackerError::InvalidFact {
                message: "cannot checkpoint tracker state while reconcile is required".into(),
            });
        }
        Ok(HunkTrackerCheckpoint {
            version: CHECKPOINT_VERSION,
            files: self
                .files
                .iter()
                .map(|(path, state)| HunkCheckpointFile {
                    path: path.clone(),
                    snapshot: state.snapshot.clone(),
                    baseline: state.baseline.clone().map(Into::into),
                    current: state.current.clone().map(Into::into),
                    identities: state
                        .identities
                        .clone()
                        .into_iter()
                        .map(Into::into)
                        .collect(),
                    agent_touched: state.agent_touched,
                    target_fingerprint: state.target_fingerprint.clone(),
                    mutation_kind: state.mutation_kind.clone(),
                })
                .collect(),
            facts: self.facts.iter().cloned().collect(),
            reconcile: self.reconcile,
            next_hunk: self.next_hunk,
            next_fact: self.next_fact,
            history_bytes: self.history_bytes,
        })
    }

    pub(super) fn restore_checkpoint(
        &mut self,
        checkpoint: HunkTrackerCheckpoint,
    ) -> Result<(), ChangeTrackerError> {
        let restored = Self::from_checkpoint(self.root.clone(), self.options.clone(), checkpoint)?;
        *self = restored;
        Ok(())
    }

    fn from_checkpoint(
        root: PathBuf,
        options: super::HunkTrackerOptions,
        checkpoint: HunkTrackerCheckpoint,
    ) -> Result<Self, ChangeTrackerError> {
        validate_checkpoint_header(&checkpoint, &options)?;
        let mut files = BTreeMap::new();
        for file in checkpoint.files {
            let path = normalize_relative(&file.path)?;
            if path != file.path || files.contains_key(&path) {
                return Err(invalid_checkpoint(format!(
                    "checkpoint contains a duplicate or non-normal path: {}",
                    file.path.display()
                )));
            }
            validate_checkpoint_file(&root, &path, &file, &options)?;
            files.insert(
                path,
                FileState {
                    snapshot: file.snapshot,
                    baseline: file.baseline.map(Into::into),
                    current: file.current.map(Into::into),
                    identities: file.identities.into_iter().map(Into::into).collect(),
                    agent_touched: file.agent_touched,
                    target_fingerprint: file.target_fingerprint,
                    mutation_kind: file.mutation_kind,
                },
            );
        }
        Ok(Self {
            root,
            options,
            files,
            pending_receipts: VecDeque::new(),
            pending_events: VecDeque::new(),
            facts: checkpoint.facts.into(),
            next_hunk: checkpoint.next_hunk,
            next_fact: checkpoint.next_fact,
            history_bytes: checkpoint.history_bytes,
            reconcile: checkpoint.reconcile,
        })
    }
}

fn validate_checkpoint_header(
    checkpoint: &HunkTrackerCheckpoint,
    options: &super::HunkTrackerOptions,
) -> Result<(), ChangeTrackerError> {
    if checkpoint.version != CHECKPOINT_VERSION {
        return Err(invalid_checkpoint(format!(
            "unsupported hunk checkpoint version: {}",
            checkpoint.version
        )));
    }
    if checkpoint.files.len() > options.max_files
        || checkpoint.facts.len() > options.max_change_facts
        || checkpoint.history_bytes > options.max_history_bytes
        || checkpoint.next_hunk == 0
        || checkpoint.next_fact == 0
        || !matches!(checkpoint.reconcile, ReconcileState::Ready)
    {
        return Err(invalid_checkpoint("hunk checkpoint exceeds tracker bounds"));
    }
    let mut previous = 0;
    for fact in &checkpoint.facts {
        if fact.recorded_sequence <= previous || fact.recorded_sequence >= checkpoint.next_fact {
            return Err(invalid_checkpoint(
                "hunk checkpoint fact sequence is invalid",
            ));
        }
        previous = fact.recorded_sequence;
    }
    Ok(())
}

fn validate_checkpoint_file(
    root: &Path,
    path: &Path,
    file: &HunkCheckpointFile,
    options: &super::HunkTrackerOptions,
) -> Result<(), ChangeTrackerError> {
    if file.identities.len() > options.max_hunks_per_file {
        return Err(invalid_checkpoint(format!(
            "checkpoint hunk budget exceeded for {}",
            path.display()
        )));
    }
    if let Some(snapshot) = &file.snapshot
        && (snapshot.path != path
            || snapshot.hunks.len() > options.max_hunks_per_file
            || file.current.as_ref().is_none_or(|current| {
                current.exists != snapshot.after_exists
                    || current.revision != snapshot.after_revision
            }))
    {
        return Err(invalid_checkpoint(format!(
            "checkpoint snapshot does not match file state: {}",
            path.display()
        )));
    }
    if let Some(version) = &file.baseline {
        validate_version(version, path, "baseline")?;
    }
    if let Some(version) = &file.current {
        validate_version(version, path, "current")?;
        let observed = read_observed(root, path, options.max_content_bytes)?;
        if observed.exists != version.exists || observed.revision != version.revision {
            return Err(invalid_checkpoint(format!(
                "workspace does not match checkpoint current state: {}",
                path.display()
            )));
        }
    }
    for identity in &file.identities {
        HunkId::parse(identity.id.as_str().to_owned())?;
        if identity.fingerprint.is_empty() || identity.after_revision.is_empty() {
            return Err(invalid_checkpoint(format!(
                "checkpoint hunk identity is incomplete: {}",
                path.display()
            )));
        }
    }
    Ok(())
}

fn validate_version(
    version: &HunkCheckpointVersion,
    path: &Path,
    label: &str,
) -> Result<(), ChangeTrackerError> {
    if version.revision.is_empty()
        || (!version.exists && version.content.as_deref() != Some(&[]))
        || version
            .content
            .as_deref()
            .is_some_and(|content| revision(content) != version.revision)
    {
        return Err(invalid_checkpoint(format!(
            "checkpoint {label} version is invalid: {}",
            path.display()
        )));
    }
    Ok(())
}

fn invalid_checkpoint(message: impl Into<String>) -> ChangeTrackerError {
    ChangeTrackerError::InvalidFact {
        message: message.into(),
    }
}
