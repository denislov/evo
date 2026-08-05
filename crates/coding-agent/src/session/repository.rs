#[cfg(test)]
use std::collections::VecDeque;
use std::fmt;
use std::fs::{self, File, OpenOptions};
#[cfg(test)]
use std::io::BufWriter;
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
#[cfg(test)]
use std::sync::{Arc, Mutex};

use event_journal::api::error::{JournalError, JournalErrorKind};
#[cfg(test)]
use event_journal::api::frame::MAX_JOURNAL_RECORD_BYTES;
use event_journal::api::frame::{decode_json_record, decode_json_value, encode_json_record};
use event_journal::api::read::{read_first_line, visit_lines};
use event_journal::api::storage::{AppendFault, JournalPaths, JournalStore, JournalWriteLease};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

use super::manifest::{
    EVENT_SCHEMA, EVENT_VERSION, LEGACY_SESSION_VERSION, PersistedWorkspaceScope,
    SESSION_EVENT_LOG_FILE, SESSION_MANIFEST_FILE, SESSION_OUTBOX_LOG_FILE, SESSION_SCHEMA,
    SESSION_VERSION, SessionManifest, default_agent_profile_id,
};
use super::replay::{ReplayFold, ReplayIndex, SessionReplay};
use crate::events::outbox::{
    DurableOutboxRecord, DurableOutboxRecordCandidate, OUTBOX_SCHEMA, OUTBOX_VERSION,
};
use crate::kernel::error::CodingSessionError;
use crate::kernel::ids::ProfileId;
#[cfg(test)]
use crate::mutex::MutexExt;
use crate::session::event::{SessionEventData, SessionEventEnvelope};
use crate::workspace::projectless_workspace_id_for_session;

const SESSION_WRITER_LOCK_FILE: &str = ".writer.lock";
static MANIFEST_TEMP_COUNTER: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone)]
pub(crate) struct SessionLogStore {
    root: PathBuf,
    journal: JournalStore,
    #[cfg(test)]
    io_faults: Option<SessionIoFaultPlan>,
}

#[cfg_attr(not(test), allow(dead_code))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SessionIoFault {
    WriteAfterBytes(usize),
    Sync,
}

#[cfg(test)]
#[derive(Debug, Clone, Default)]
pub(crate) struct SessionIoFaultPlan {
    faults: Arc<Mutex<VecDeque<SessionIoFault>>>,
}

#[cfg(test)]
impl SessionIoFaultPlan {
    pub(crate) fn push(&self, fault: SessionIoFault) {
        self.faults
            .lock_or_recover("test session I/O fault plan")
            .push_back(fault);
    }

    fn take(&self) -> Option<SessionIoFault> {
        self.faults
            .lock_or_recover("test session I/O fault plan")
            .pop_front()
    }
}

#[derive(Debug)]
pub(crate) struct SessionWriteLease {
    journal: JournalWriteLease,
}

impl SessionWriteLease {
    pub(crate) fn committed_sequence(&self) -> u64 {
        self.journal.committed_sequence()
    }

    pub(crate) fn tail_recoveries(&self) -> &[String] {
        self.journal.tail_recoveries()
    }
}

impl From<SessionIoFault> for AppendFault {
    fn from(value: SessionIoFault) -> Self {
        match value {
            SessionIoFault::WriteAfterBytes(bytes) => Self::WriteAfterBytes(bytes),
            SessionIoFault::Sync => Self::Sync,
        }
    }
}

#[derive(Debug)]
pub(crate) enum SessionCreateError {
    Create(CodingSessionError),
    CleanupFailed {
        session_id: String,
        session_dir: PathBuf,
        create_error: CodingSessionError,
        cleanup_error: CodingSessionError,
    },
}

impl fmt::Display for SessionCreateError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Create(error) => error.fmt(formatter),
            Self::CleanupFailed {
                session_id,
                session_dir,
                create_error,
                cleanup_error,
            } => write!(
                formatter,
                "session initialization failed for {session_id} at {}: {create_error}; cleanup failed: {cleanup_error}",
                session_dir.display()
            ),
        }
    }
}

impl std::error::Error for SessionCreateError {}

impl From<CodingSessionError> for SessionCreateError {
    fn from(error: CodingSessionError) -> Self {
        Self::Create(error)
    }
}

impl From<SessionCreateError> for CodingSessionError {
    fn from(error: SessionCreateError) -> Self {
        match error {
            SessionCreateError::Create(error) => error,
            cleanup_failed @ SessionCreateError::CleanupFailed { .. } => {
                CodingSessionError::Session {
                    message: cleanup_failed.to_string(),
                }
            }
        }
    }
}

mod store;

pub(crate) use store::CreateSessionOptions;

mod bounded;

pub(crate) use bounded::SessionEventReadBudget;

impl CreateSessionOptions {
    pub(crate) fn new(session_id: impl Into<String>, created_at: impl Into<String>) -> Self {
        let session_id = session_id.into();
        Self {
            workspace_scope: PersistedWorkspaceScope::Projectless {
                workspace_id: projectless_workspace_id_for_session(&session_id),
            },
            session_id,
            name: None,
            created_at: created_at.into(),
            default_agent_profile_id: default_agent_profile_id(),
        }
    }

    pub(crate) fn default_agent_profile_id(mut self, profile_id: ProfileId) -> Self {
        self.default_agent_profile_id = profile_id;
        self
    }

    pub(crate) fn name(mut self, name: Option<String>) -> Self {
        self.name = name;
        self
    }

    pub(crate) fn workspace_scope(mut self, workspace_scope: PersistedWorkspaceScope) -> Self {
        self.workspace_scope = workspace_scope;
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SessionHandle {
    session_dir: PathBuf,
    manifest: SessionManifest,
}

impl SessionHandle {
    pub(crate) fn session_dir(&self) -> &Path {
        &self.session_dir
    }

    pub(crate) fn manifest(&self) -> &SessionManifest {
        &self.manifest
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SessionSummary {
    pub(crate) session_id: String,
    pub(crate) name: Option<String>,
    pub(crate) session_dir: PathBuf,
    pub(crate) event_log_name: String,
    pub(crate) created_at: String,
    pub(crate) updated_at: String,
    pub(crate) active_leaf_id: Option<String>,
    pub(crate) workspace_scope: Option<PersistedWorkspaceScope>,
    pub(crate) workspace_migrated_from_legacy: bool,
}

impl SessionSummary {
    pub(crate) fn from_handle(handle: &SessionHandle) -> Self {
        Self {
            session_id: handle.manifest.session_id.clone(),
            name: handle.manifest.name.clone(),
            session_dir: handle.session_dir.clone(),
            event_log_name: handle.manifest.event_log.clone(),
            created_at: handle.manifest.created_at.clone(),
            updated_at: handle.manifest.updated_at.clone(),
            active_leaf_id: handle.manifest.active_leaf_id.clone(),
            workspace_scope: handle.manifest.workspace_scope.clone(),
            workspace_migrated_from_legacy: handle.manifest.workspace_migrated_from_legacy,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SessionCreationWorkspace {
    pub(crate) cwd: Option<String>,
    pub(crate) workspace_scope: Option<PersistedWorkspaceScope>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ManifestPatch {
    updated_at: Option<String>,
    name: Option<Option<String>>,
    active_branch_id: Option<Option<String>>,
    active_leaf_id: Option<Option<String>>,
    default_agent_profile_id: Option<ProfileId>,
    workspace_migration: Option<PersistedWorkspaceScope>,
}

impl ManifestPatch {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn updated_at(mut self, updated_at: impl Into<String>) -> Self {
        self.updated_at = Some(updated_at.into());
        self
    }

    pub(crate) fn name(mut self, name: Option<String>) -> Self {
        self.name = Some(name);
        self
    }

    pub(crate) fn active_leaf_id(mut self, active_leaf_id: Option<String>) -> Self {
        self.active_leaf_id = Some(active_leaf_id);
        self
    }

    fn workspace_migration(mut self, scope: PersistedWorkspaceScope) -> Self {
        self.workspace_migration = Some(scope);
        self
    }

    fn apply(self, manifest: &mut SessionManifest) {
        if let Some(updated_at) = self.updated_at {
            manifest.updated_at = updated_at;
        }
        if let Some(name) = self.name {
            manifest.name = name;
        }
        if let Some(active_branch_id) = self.active_branch_id {
            manifest.active_branch_id = active_branch_id;
        }
        if let Some(active_leaf_id) = self.active_leaf_id {
            manifest.active_leaf_id = active_leaf_id;
        }
        if let Some(default_agent_profile_id) = self.default_agent_profile_id {
            manifest.default_agent_profile_id = default_agent_profile_id;
        }
        if let Some(scope) = self.workspace_migration {
            manifest.version = SESSION_VERSION;
            manifest.workspace_scope = Some(scope);
            manifest.workspace_migrated_from_legacy = true;
        }
    }
}

mod io;

use io::*;

fn journal_error(error: JournalError) -> CodingSessionError {
    if error.kind() == JournalErrorKind::WriteRejected {
        CodingSessionError::SessionWriteRejected {
            message: error.message().to_owned(),
        }
    } else {
        session_error(error.to_string())
    }
}

fn journal_codec_error(error: CodingSessionError) -> JournalError {
    JournalError::codec(error.to_string())
}

fn journal_paths(handle: &SessionHandle) -> Result<JournalPaths, CodingSessionError> {
    Ok(JournalPaths::new(
        event_log_path(&handle.session_dir, &handle.manifest)?,
        outbox_log_path(&handle.session_dir, &handle.manifest)?,
        handle.session_dir.join(SESSION_WRITER_LOCK_FILE),
    ))
}

pub(crate) fn normalize_session_id(value: &str) -> Result<String, CodingSessionError> {
    normalize_session_id_impl(value)
}
