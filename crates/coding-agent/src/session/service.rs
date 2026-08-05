use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use agent_core::api::transcript::{SessionEntry, SessionTreeNode, StoredAgentMessage};
use ai_protocol::api::conversation::{ContentBlock, Usage};
use tokio::sync::watch;

use crate::application::capability::OperationCapabilitySnapshot;
use crate::application::operation::finalize::FinalizationDecision;
use crate::events::CodingAgentProductEventDurability;
use crate::events::emission::ProductEventDraft;
use crate::events::outbox::{
    DurableOutboxIntent, DurableOutboxRecord, DurableOutboxRecordCandidate, DurableOutboxRecordKind,
};
use crate::events::session::SessionWriteEvent;
use crate::events::{CodingAgentSessionWriteFailureReason, CodingAgentSessionWriteFailureStatus};
use crate::kernel::error::{CodingSessionError, SessionWriteFailureReason};
use crate::operations::export::runner::{ExportContext, ExportOptions};
use crate::operations::prompt::context::{
    InternalPromptTurnOutcome, PromptTurnContext, PromptTurnTransaction,
};
use crate::operations::self_healing_edit::runner::{
    SelfHealingEditOutcome, SelfHealingEditRepairAttempt,
};
use crate::platform::time::{Clock, IdGenerator, SystemClock, SystemIdGenerator};
use crate::profiles::{ProfileId, ProfileKind};
use crate::services::event::EventService;
use crate::session::event::{
    OperationKind, PersistedContentBlock, PersistedDelegationRuntimeSeed,
    PersistedDelegationStatus, PersistedToolAuthorizationResolution, SessionEventData,
    SessionEventEnvelope,
};
use crate::session::manifest::PersistedWorkspaceScope;
use crate::session::replay::{
    MessageStatus, ReplayTreeLabel, SessionReplay, TranscriptItem, fold_events,
};
use crate::session::repository::{
    CreateSessionOptions, ManifestPatch, SessionCreateError, SessionEventReadBudget, SessionHandle,
    SessionLogStore, SessionSummary,
};
use crate::session::transaction::{
    SessionCommitReceipt, SessionTransactionWriter, TurnTransaction,
};
use crate::session::view::{
    CodingAgentRecoveryResolutionRequest, CodingAgentRecoveryRetryRequest,
    CodingAgentSessionDiagnostic, CodingAgentSessionHydration, CodingAgentSessionOpenTarget,
    CodingAgentSessionOptions, CodingAgentSessionOverview, CodingAgentSessionSummary,
    CodingAgentSessionTree, CodingAgentSessionUsageSummary, CodingAgentSessionView,
    CodingAgentTranscriptContinuation,
};
use crate::workspace::{
    CodingAgentWorkspaceMigration, CodingAgentWorkspaceMigrationOutcome, CodingAgentWorkspaceScope,
    infer_legacy_workspace, projectless_workspace_id_for_session, workspace_migration_status,
};

const RECOVERY_RECORD_VERSION: u64 = crate::events::recovery::RECOVERY_RECORD_VERSION;
const MAX_RECOVERY_RETRY_ATTEMPTS: u32 = 3;
const MAX_SESSION_NAME_CHARS: usize = 200;
const MAX_HYDRATION_EVENT_ITEMS: usize = 10_000;
const MAX_HYDRATION_EVENT_BYTES: usize = 32 * 1024 * 1024;

pub(crate) fn session_cwd(session_service: &SessionService) -> Option<PathBuf> {
    session_service
        .replay()
        .ok()
        .and_then(|replay| replay.cwd.map(PathBuf::from))
}

pub(crate) fn default_cwd() -> PathBuf {
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StartupRecoveryMarker {
    pub(crate) operation_id: String,
    pub(crate) recovery_id: String,
    pub(crate) reason: String,
    pub(crate) session_id: String,
    pub(crate) operation_kind: Option<crate::session::event::OperationKind>,
    pub(crate) capability_generation: Option<u64>,
    pub(crate) record_version: u64,
    pub(crate) descriptor_revision: u16,
    pub(crate) attempt_count: u32,
    pub(crate) last_attempt_at: Option<String>,
    pub(crate) next_attempt_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RecoveryPendingInspection {
    pub(crate) operation_id: String,
    pub(crate) recovery_id: String,
    pub(crate) operation_kind: Option<String>,
    pub(crate) record_version: u64,
    pub(crate) descriptor_revision: u16,
    pub(crate) capability_generation: Option<u64>,
    pub(crate) attempt_count: u32,
    pub(crate) last_attempt_at: Option<String>,
    pub(crate) next_attempt_at: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct RecoveryResolutionCommit {
    pub(crate) operation_id: String,
    pub(crate) recovery_id: String,
    pub(crate) resolution: crate::events::CodingAgentRecoveryResolution,
    pub(crate) operation_kind: crate::session::event::OperationKind,
    pub(crate) draft: ProductEventDraft,
}

#[derive(Debug, Clone)]
pub(crate) struct RecoveryRetryCommit {
    pub(crate) operation_id: String,
    pub(crate) recovery_id: String,
    pub(crate) operation_kind: crate::session::event::OperationKind,
    pub(crate) capability_generation: Option<u64>,
    pub(crate) draft: ProductEventDraft,
    pub(crate) attempt_count: u32,
    pub(crate) last_attempt_at: String,
    pub(crate) next_attempt_at: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct SessionService {
    store: SessionLogStore,
    handle: SessionHandle,
    transaction_writer: SessionTransactionWriter,
    committed_session_sequence: Arc<AtomicU64>,
    startup_outbox_records: Vec<DurableOutboxRecord>,
    startup_recovery_markers: Vec<StartupRecoveryMarker>,
    auto_name_eligible_for_active_prompt: bool,
    session_name_updates: watch::Sender<SessionNameUpdate>,
}

#[derive(Debug, Clone)]
pub(crate) struct SessionEventWriter {
    session_id: String,
    writer: SessionTransactionWriter,
    committed_session_sequence: Arc<AtomicU64>,
}

#[derive(Debug, Clone)]
pub(crate) struct SessionAutoNameWriter {
    session_id: String,
    writer: SessionTransactionWriter,
    committed_session_sequence: Arc<AtomicU64>,
    session_name_updates: watch::Sender<SessionNameUpdate>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct FinalizedSessionWrite {
    pub(crate) events: Vec<SessionWriteEvent>,
    pub(crate) session_id: Option<String>,
    pub(crate) leaf_id: Option<String>,
    pub(crate) committed_session_sequence: Option<u64>,
}

#[derive(Debug)]
#[allow(
    clippy::large_enum_variant,
    reason = "session persistence owns exactly one persistent or transient state implementation"
)]
pub(crate) enum SessionPersistence {
    Persistent(SessionService),
    NonPersistent(TransientSessionState),
}

#[derive(Debug)]
pub(crate) struct TransientSessionState {
    pub(crate) runtime_id: String,
    pub(crate) transcript: Vec<TranscriptItem>,
    pub(crate) default_agent_profile_id: ProfileId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SessionTreeLabelUpdate {
    pub(crate) entry_id: String,
    pub(crate) label: Option<String>,
    pub(crate) updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SessionNameUpdate {
    pub(crate) name: Option<String>,
    pub(crate) updated_at: String,
}

/// Explicit full-history export boundary. UI hydration must never obtain this
/// value because constructing it intentionally replays the complete log.
pub(crate) struct SessionExport {
    options: ExportOptions,
    summary: CodingAgentSessionSummary,
    replay: SessionReplay,
}

impl SessionExport {
    pub(crate) fn into_context(self) -> ExportContext {
        ExportContext::new(self.options, self.summary, self.replay)
    }
}

fn normalize_leaf_id(value: &str) -> Result<String, CodingSessionError> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(CodingSessionError::Input {
            message: "target leaf id must not be empty".into(),
        });
    }
    Ok(trimmed.to_owned())
}

fn committed_leaf_cutoff(events: &[SessionEventEnvelope], target_leaf_id: &str) -> Option<usize> {
    events.iter().position(|event| {
        matches!(
            &event.data,
            SessionEventData::OperationCommitted {
                new_leaf_id: Some(new_leaf_id),
            } if new_leaf_id == target_leaf_id
        )
    })
}

fn normalize_tree_entry_id(value: &str) -> Result<String, CodingSessionError> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(CodingSessionError::Input {
            message: "tree entry id must not be empty".into(),
        });
    }
    Ok(trimmed.to_owned())
}

fn normalize_tree_label(label: Option<String>) -> Option<String> {
    label.and_then(|label| {
        let label = label.trim();
        (!label.is_empty()).then(|| label.to_owned())
    })
}

fn normalize_session_name(name: Option<String>) -> Option<String> {
    name.and_then(|name| {
        let name = name.trim();
        if name.is_empty() {
            None
        } else {
            Some(name.chars().take(MAX_SESSION_NAME_CHARS).collect())
        }
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SessionCopyKind {
    Clone,
    Fork,
}

impl SessionCopyKind {
    fn provenance_event(
        self,
        source_session_id: String,
        source_leaf_id: String,
    ) -> SessionEventData {
        match self {
            Self::Clone => SessionEventData::SessionCloned {
                source_session_id,
                source_leaf_id,
            },
            Self::Fork => SessionEventData::SessionForked {
                source_session_id,
                source_leaf_id,
            },
        }
    }
}

mod commands;
mod finalize;
mod persistence;
mod queries;
mod recovery;

pub(crate) use queries::coding_transcript_from_replay;

fn observe_commit_receipt(cursor: &AtomicU64, receipt: SessionCommitReceipt) {
    if let Some(sequence) = receipt.committed_session_sequence {
        cursor.fetch_max(sequence, Ordering::AcqRel);
    }
}
