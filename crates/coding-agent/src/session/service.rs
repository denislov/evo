use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use agent_core::api::transcript::{SessionEntry, SessionTreeNode, StoredAgentMessage};
use ai::api::conversation::{ContentBlock, Usage};
use tokio::sync::watch;

use crate::events::CodingAgentProductEventDurability;
use crate::events::CodingAgentSessionWriteFailureStatus;
use crate::events::emission::ProductEventDraft;
use crate::events::outbox::{
    DurableOutboxIntent, DurableOutboxRecord, DurableOutboxRecordCandidate, DurableOutboxRecordKind,
};
use crate::events::session::SessionWriteEvent;
use crate::operations::export::runner::{ExportContext, ExportOptions};
use crate::operations::prompt::context::{
    InternalPromptTurnOutcome, PromptTurnContext, PromptTurnTransaction,
};
use crate::runtime::capability::OperationCapabilitySnapshot;
#[cfg(test)]
use crate::runtime::capability::SessionWriteCapability;
use crate::runtime::facade::CodingAgentSessionOpenTarget;
use crate::runtime::facade::{
    CodingAgentSessionDiagnostic, CodingAgentSessionHydration, CodingAgentSessionOptions,
    CodingAgentSessionOverview, CodingAgentSessionSummary, CodingAgentSessionTranscriptItem,
    CodingAgentSessionTree, CodingAgentSessionUsageSummary, CodingAgentSessionView,
    CodingSessionError, ProfileId, ProfileKind, SelfHealingEditOutcome,
    SelfHealingEditRepairAttempt,
};
use crate::runtime::operation::finalize::FinalizationDecision;
use crate::services::event::EventService;
use crate::session::event::{
    OperationKind, PersistedContentBlock, PersistedDelegationRuntimeSeed,
    PersistedDelegationStatus, PersistedToolAuthorizationResolution, SessionEventData,
    SessionEventEnvelope,
};
use crate::session::id::{Clock, IdGenerator, SystemClock, SystemIdGenerator};
use crate::session::manifest::PersistedWorkspaceScope;
#[cfg(test)]
use crate::session::replay::SessionRecoverySummary;
use crate::session::replay::{
    MessageStatus, ReplayTreeLabel, SessionReplay, ToolCallStatus, TranscriptItem, fold_events,
};
#[cfg(test)]
use crate::session::repository::StoreFailurePoint;
use crate::session::repository::{
    CreateSessionOptions, ManifestPatch, SessionCreateError, SessionHandle, SessionLogStore,
    SessionSummary,
};
use crate::session::transaction::{
    SessionCommitReceipt, SessionTransactionWriter, TurnTransaction,
};
use crate::workspace::{
    CodingAgentWorkspaceMigration, CodingAgentWorkspaceMigrationOutcome, CodingAgentWorkspaceScope,
    infer_legacy_workspace, projectless_workspace_id_for_session, workspace_migration_status,
};

const RECOVERY_RECORD_VERSION: u64 = crate::events::recovery::RECOVERY_RECORD_VERSION;
const MAX_RECOVERY_RETRY_ATTEMPTS: u32 = 3;
const MAX_SESSION_NAME_CHARS: usize = 200;

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

#[derive(Debug)]
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

impl SessionAutoNameWriter {
    pub(crate) fn is_unnamed(&self) -> bool {
        self.writer.manifest_snapshot().name.is_none()
    }

    pub(crate) fn commit_generated_name(
        &self,
        operation_id: &str,
        name: String,
        model_id: String,
        usage: Usage,
    ) -> Result<(), CodingSessionError> {
        let mut ids = SystemIdGenerator;
        let updated_at = SystemClock.now_rfc3339();
        let events = vec![
            SessionEventEnvelope::new(
                self.session_id.clone(),
                ids.next_event_id(),
                updated_at.clone(),
                SessionEventData::OperationStarted {
                    operation: OperationKind::Other {
                        name: "session_naming".into(),
                    },
                    runtime_generation: Default::default(),
                },
            )
            .with_operation_id(operation_id),
            SessionEventEnvelope::new(
                self.session_id.clone(),
                ids.next_event_id(),
                updated_at.clone(),
                SessionEventData::ModelUsageRecorded {
                    purpose: "session_naming".into(),
                    model_id,
                    usage,
                },
            )
            .with_operation_id(operation_id),
            SessionEventEnvelope::new(
                self.session_id.clone(),
                ids.next_event_id(),
                updated_at.clone(),
                SessionEventData::OperationCommitted { new_leaf_id: None },
            )
            .with_operation_id(operation_id),
        ];
        let receipt = self.writer.commit_session_name_if_unset(
            events,
            ManifestPatch::new().updated_at(updated_at).name(Some(name)),
            operation_id.to_owned(),
        )?;
        observe_commit_receipt(&self.committed_session_sequence, receipt);
        let manifest = self.writer.manifest_snapshot();
        self.session_name_updates.send_replace(SessionNameUpdate {
            name: manifest.name,
            updated_at: manifest.updated_at,
        });
        Ok(())
    }

    pub(crate) fn commit_failure_diagnostic(
        &self,
        operation_id: &str,
        message: String,
        model_usage: Option<(String, Usage)>,
    ) -> Result<(), CodingSessionError> {
        let mut ids = SystemIdGenerator;
        let created_at = SystemClock.now_rfc3339();
        let mut events = Vec::with_capacity(4);
        events.push(
            SessionEventEnvelope::new(
                self.session_id.clone(),
                ids.next_event_id(),
                created_at.clone(),
                SessionEventData::OperationStarted {
                    operation: OperationKind::Other {
                        name: "session_naming".into(),
                    },
                    runtime_generation: Default::default(),
                },
            )
            .with_operation_id(operation_id),
        );
        if let Some((model_id, usage)) = model_usage {
            events.push(
                SessionEventEnvelope::new(
                    self.session_id.clone(),
                    ids.next_event_id(),
                    created_at.clone(),
                    SessionEventData::ModelUsageRecorded {
                        purpose: "session_naming".into(),
                        model_id,
                        usage,
                    },
                )
                .with_operation_id(operation_id),
            );
        }
        events.push(
            SessionEventEnvelope::new(
                self.session_id.clone(),
                ids.next_event_id(),
                created_at,
                SessionEventData::DiagnosticEmitted {
                    level: crate::session::event::DiagnosticLevel::Warn,
                    message,
                },
            )
            .with_operation_id(operation_id),
        );
        events.push(
            SessionEventEnvelope::new(
                self.session_id.clone(),
                ids.next_event_id(),
                SystemClock.now_rfc3339(),
                SessionEventData::OperationFailed {
                    error_code: "session_naming".into(),
                    message: "automatic session naming failed".into(),
                },
            )
            .with_operation_id(operation_id),
        );
        let receipt = self.writer.append_checkpoint_events_with_receipt(events)?;
        observe_commit_receipt(&self.committed_session_sequence, receipt);
        let manifest = self.writer.manifest_snapshot();
        self.session_name_updates.send_replace(SessionNameUpdate {
            name: manifest.name,
            updated_at: manifest.updated_at,
        });
        Ok(())
    }
}

impl SessionEventWriter {
    pub(crate) fn append(
        &self,
        operation_id: &str,
        turn_id: &str,
        data: Vec<SessionEventData>,
    ) -> Result<(), CodingSessionError> {
        if data.is_empty() {
            return Ok(());
        }
        let mut ids = SystemIdGenerator;
        let clock = SystemClock;
        let updated_at = clock.now_rfc3339();
        let events = data
            .into_iter()
            .map(|data| {
                SessionEventEnvelope::new(
                    self.session_id.clone(),
                    ids.next_event_id(),
                    updated_at.clone(),
                    data,
                )
                .with_operation_id(operation_id)
                .with_turn_id(turn_id)
            })
            .collect::<Vec<_>>();
        let receipt = self.writer.append_checkpoint_events_with_receipt(events)?;
        observe_commit_receipt(&self.committed_session_sequence, receipt);
        Ok(())
    }
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

impl TransientSessionState {
    pub(crate) fn new(default_agent_profile_id: ProfileId) -> Self {
        let mut ids = SystemIdGenerator;
        Self {
            runtime_id: format!("runtime_{}", ids.next_session_id()),
            transcript: Vec::new(),
            default_agent_profile_id,
        }
    }

    pub(crate) fn finalize_prompt_transaction(
        &mut self,
        context: &PromptTurnContext,
        outcome: &InternalPromptTurnOutcome,
    ) -> FinalizedSessionWrite {
        if outcome.is_success() {
            self.transcript.extend(context.completed_transcript_items());
        }
        SessionService::skip_prompt_transaction(
            context.operation_id().to_owned(),
            "session persistence disabled",
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SessionCopyKind {
    Clone,
    Fork,
}

impl SessionService {
    fn from_handle(
        store: SessionLogStore,
        handle: SessionHandle,
    ) -> Result<Self, CodingSessionError> {
        // The writer lease repairs only a torn final frame before any durable
        // records are decoded, published, or redelivered after restart.
        let transaction_writer = SessionTransactionWriter::new(store.clone(), handle.clone())?;
        let committed_session_sequence = transaction_writer.committed_session_sequence();
        let manifest = transaction_writer.manifest_snapshot();
        let (session_name_updates, _) = watch::channel(SessionNameUpdate {
            name: manifest.name,
            updated_at: manifest.updated_at,
        });
        let startup_outbox_records = store.read_outbox(&handle)?;
        Ok(Self {
            store,
            handle,
            transaction_writer,
            committed_session_sequence: Arc::new(AtomicU64::new(committed_session_sequence)),
            startup_outbox_records,
            startup_recovery_markers: Vec::new(),
            auto_name_eligible_for_active_prompt: false,
            session_name_updates,
        })
    }

    pub(crate) fn create(options: &CodingAgentSessionOptions) -> Result<Self, CodingSessionError> {
        let root = resolve_session_log_root(options)?;
        let store = SessionLogStore::new(root);
        let mut ids = SystemIdGenerator;
        let clock = SystemClock;
        let session_id = match options.session_id() {
            Some(session_id) => normalize_session_id(session_id, "session id")?,
            None => ids.next_session_id(),
        };
        let cwd = option_cwd_string(options);
        let workspace_scope = option_workspace_scope(options, &session_id)?;
        Self::create_with_id(
            store,
            session_id,
            &mut ids,
            &clock,
            workspace_scope,
            cwd,
            option_default_agent_profile_id(options),
            normalize_session_name(options.session_name().map(str::to_owned)),
            None,
        )
    }

    pub(crate) fn open(options: &CodingAgentSessionOptions) -> Result<Self, CodingSessionError> {
        let root = resolve_session_log_root(options)?;
        let store = SessionLogStore::new(root);
        let target = open_target(options)?;
        let handle = store.open_session(&target)?;
        let handle = migrate_workspace_on_open(
            &store,
            handle,
            workspace_global_config_dir(options).as_path(),
        )?;

        let mut service = Self::from_handle(store, handle)?;
        service.apply_startup_recovery()?;
        Ok(service)
    }

    pub(crate) fn open_or_create(
        options: &CodingAgentSessionOptions,
    ) -> Result<Self, CodingSessionError> {
        if options.session_path().is_some() {
            return Err(CodingSessionError::Input {
                message: "open-or-create requires a session id, not a session path".into(),
            });
        }
        let session_id = options
            .session_id()
            .ok_or_else(|| CodingSessionError::Input {
                message: "open-or-create requires a session id".into(),
            })
            .and_then(|session_id| normalize_session_id(session_id, "session id"))?;
        let root = resolve_session_log_root(options)?;
        let store = SessionLogStore::new(root);

        if let Some(handle) = store.try_open_session_id(&session_id)? {
            let handle = migrate_workspace_on_open(
                &store,
                handle,
                workspace_global_config_dir(options).as_path(),
            )?;
            let mut service = Self::from_handle(store, handle)?;
            service.apply_startup_recovery()?;
            return Ok(service);
        }

        let mut ids = SystemIdGenerator;
        let clock = SystemClock;
        let cwd = option_cwd_string(options);
        let workspace_scope = option_workspace_scope(options, &session_id)?;
        Self::create_with_id(
            store,
            session_id,
            &mut ids,
            &clock,
            workspace_scope,
            cwd,
            option_default_agent_profile_id(options),
            normalize_session_name(options.session_name().map(str::to_owned)),
            None,
        )
    }

    pub(crate) fn list(
        options: &CodingAgentSessionOptions,
    ) -> Result<Vec<CodingAgentSessionSummary>, CodingSessionError> {
        let root = resolve_session_log_root(options)?;
        let store = SessionLogStore::new(root);
        let cwd = option_cwd_string(options);
        let mut summaries = Vec::new();
        for summary in store.list_sessions()? {
            if let Some(cwd) = cwd.as_deref() {
                let handle = match store.open_session(&summary.session_dir) {
                    Ok(handle) => handle,
                    Err(_) => continue,
                };
                let replay = match store.replay_session(&handle) {
                    Ok(replay) => replay,
                    Err(_) => continue,
                };
                if replay.cwd.as_deref() != Some(cwd) {
                    continue;
                }
            }
            summaries.push(CodingAgentSessionSummary::from(summary));
        }
        Ok(summaries)
    }

    pub(crate) fn list_overviews(
        options: &CodingAgentSessionOptions,
        limit: usize,
    ) -> Result<(Vec<CodingAgentSessionOverview>, bool), CodingSessionError> {
        let root = resolve_session_log_root(options)?;
        let store = SessionLogStore::new(root);
        let cwd_filter = option_cwd_string(options);
        let workspace_filter = options.workspace_scope().cloned();
        let global_config_dir = workspace_global_config_dir(options);
        let summaries = store.list_sessions()?;
        let mut overviews = Vec::new();
        if cwd_filter.is_none() && workspace_filter.is_none() {
            let truncated = summaries.len() > limit;
            for summary in summaries.into_iter().take(limit) {
                let workspace =
                    match workspace_facts_for_summary(&store, &summary, &global_config_dir) {
                        Ok(workspace) => workspace,
                        Err(_) => continue,
                    };
                overviews.push(CodingAgentSessionOverview {
                    session_id: summary.session_id,
                    name: summary.name,
                    workspace: workspace.scope.overview(),
                    workspace_migration: workspace.migration,
                    created_at: summary.created_at,
                    updated_at: summary.updated_at,
                    active_leaf_id: summary.active_leaf_id,
                });
            }
            return Ok((overviews, truncated));
        }

        let mut matching = 0_usize;
        for summary in summaries {
            let workspace = match workspace_facts_for_summary(&store, &summary, &global_config_dir)
            {
                Ok(workspace) => workspace,
                Err(_) => continue,
            };
            if let Some(expected) = workspace_filter.as_ref() {
                if &workspace.scope != expected {
                    continue;
                }
            } else if let Some(expected) = cwd_filter.as_deref()
                && workspace.compatibility_cwd.as_deref() != Some(expected)
            {
                continue;
            }
            matching = matching.saturating_add(1);
            if overviews.len() < limit {
                overviews.push(CodingAgentSessionOverview {
                    session_id: summary.session_id,
                    name: summary.name,
                    workspace: workspace.scope.overview(),
                    workspace_migration: workspace.migration,
                    created_at: summary.created_at,
                    updated_at: summary.updated_at,
                    active_leaf_id: summary.active_leaf_id,
                });
            }
        }
        Ok((overviews, matching > limit))
    }

    pub(crate) fn migrate_workspace(
        options: &CodingAgentSessionOptions,
    ) -> Result<CodingAgentWorkspaceMigration, CodingSessionError> {
        let root = resolve_session_log_root(options)?;
        let store = SessionLogStore::new(root);
        let target = open_target(options)?;
        let handle = store.open_session(&target)?;
        migrate_workspace_handle(
            &store,
            handle,
            workspace_global_config_dir(options).as_path(),
        )
        .map(|(_, migration)| migration)
    }

    pub(crate) fn open_target(
        options: &CodingAgentSessionOptions,
    ) -> Result<CodingAgentSessionOpenTarget, CodingSessionError> {
        let root = resolve_session_log_root(options)?;
        let store = SessionLogStore::new(root);
        let target = open_target(options)?;
        let handle = store.open_session(&target)?;
        let (handle, _) = migrate_workspace_handle(
            &store,
            handle,
            workspace_global_config_dir(options).as_path(),
        )?;
        let summary = SessionSummary::from_handle(&handle);
        let workspace = workspace_facts_for_summary(
            &store,
            &summary,
            workspace_global_config_dir(options).as_path(),
        )?;
        Ok(CodingAgentSessionOpenTarget {
            session_id: summary.session_id,
            workspace_scope: workspace.scope,
            workspace_migration: workspace.migration,
        })
    }

    pub(crate) fn hydrate(
        options: &CodingAgentSessionOptions,
    ) -> Result<CodingAgentSessionHydration, CodingSessionError> {
        Self::open(options)?.hydrated_view()
    }

    pub(crate) fn tree_view(
        options: &CodingAgentSessionOptions,
    ) -> Result<CodingAgentSessionTree, CodingSessionError> {
        Self::open(options)?.leaf_tree_view()
    }

    fn leaf_tree_view(&self) -> Result<CodingAgentSessionTree, CodingSessionError> {
        let events = self.store.read_events(&self.handle)?;
        let replay = fold_events(&events);
        Ok(build_leaf_tree(
            &events,
            self.current_active_leaf_id(),
            &replay.tree_labels,
        ))
    }

    pub(crate) fn set_tree_label(
        &mut self,
        entry_id: &str,
        label: Option<String>,
        operation_id: &str,
    ) -> Result<SessionTreeLabelUpdate, CodingSessionError> {
        let entry_id = normalize_tree_entry_id(entry_id)?;
        let label = normalize_tree_label(label);
        let source_events = self.store.read_events(&self.handle)?;
        if committed_leaf_cutoff(&source_events, &entry_id).is_none() {
            return Err(CodingSessionError::Session {
                message: format!("tree entry id not found in session: {entry_id}"),
            });
        }

        let session_id = self.session_id().to_owned();
        let mut ids = SystemIdGenerator;
        let updated_at = SystemClock.now_rfc3339();
        let events = vec![
            SessionEventEnvelope::new(
                session_id.clone(),
                ids.next_event_id(),
                updated_at.clone(),
                SessionEventData::OperationStarted {
                    operation: OperationKind::SessionTreeLabel,
                    runtime_generation: Default::default(),
                },
            )
            .with_operation_id(operation_id),
            SessionEventEnvelope::new(
                session_id.clone(),
                ids.next_event_id(),
                updated_at.clone(),
                SessionEventData::SessionTreeLabelUpdated {
                    entry_id: entry_id.clone(),
                    label: label.clone(),
                },
            )
            .with_operation_id(operation_id),
            SessionEventEnvelope::new(
                session_id.clone(),
                ids.next_event_id(),
                updated_at.clone(),
                SessionEventData::OperationCommitted { new_leaf_id: None },
            )
            .with_operation_id(operation_id),
        ];
        self.commit_writer_mutation(
            events,
            ManifestPatch::new().updated_at(updated_at.clone()),
            Some(operation_id.to_owned()),
        )?;
        Ok(SessionTreeLabelUpdate {
            entry_id,
            label,
            updated_at,
        })
    }

    pub(crate) fn clone_current(&self) -> Result<Self, CodingSessionError> {
        self.copy_to_new_session(None, SessionCopyKind::Clone, None)
    }

    pub(crate) fn fork_current(
        &self,
        target_leaf_id: Option<&str>,
    ) -> Result<Self, CodingSessionError> {
        self.copy_to_new_session(target_leaf_id, SessionCopyKind::Fork, None)
    }

    pub(crate) fn fork_current_admitted(
        &self,
        target_leaf_id: Option<&str>,
        operation_id: &str,
    ) -> Result<Self, CodingSessionError> {
        self.copy_to_new_session(target_leaf_id, SessionCopyKind::Fork, Some(operation_id))
    }

    pub(crate) fn cleanup_failed_transition(
        self,
        operation_id: &str,
        error: CodingSessionError,
    ) -> CodingSessionError {
        if let Err(shutdown_error) = self.transaction_writer.shutdown() {
            return CodingSessionError::PartialCommit {
                operation_id: operation_id.to_owned(),
                message: format!(
                    "{error}; failed to close target session writer before cleanup: {shutdown_error}"
                ),
            };
        }
        cleanup_failed_session_copy(&self.store, &self.handle, operation_id, error)
    }

    pub(crate) fn session_id(&self) -> &str {
        &self.handle.manifest().session_id
    }

    pub(crate) fn current_active_leaf_id(&self) -> Option<String> {
        self.transaction_writer.manifest_snapshot().active_leaf_id
    }

    pub(crate) fn set_session_name(
        &mut self,
        name: Option<String>,
        operation_id: &str,
    ) -> Result<SessionNameUpdate, CodingSessionError> {
        let name = normalize_session_name(name);
        let updated_at = SystemClock.now_rfc3339();
        self.commit_writer_mutation(
            Vec::new(),
            ManifestPatch::new()
                .updated_at(updated_at.clone())
                .name(name.clone()),
            Some(operation_id.to_owned()),
        )?;
        let update = SessionNameUpdate { name, updated_at };
        self.session_name_updates.send_replace(update.clone());
        Ok(update)
    }

    pub(crate) fn current_default_agent_profile_id(&self) -> ProfileId {
        self.transaction_writer
            .manifest_snapshot()
            .default_agent_profile_id
    }

    pub(crate) fn set_default_agent_profile_id(
        &mut self,
        profile_id: ProfileId,
    ) -> Result<(), CodingSessionError> {
        self.commit_writer_mutation(
            Vec::new(),
            ManifestPatch::new()
                .updated_at(SystemClock.now_rfc3339())
                .default_agent_profile_id(profile_id),
            None,
        )?;
        Ok(())
    }

    pub(crate) fn branch_summary_for(
        &self,
        source_leaf_id: &str,
        target_leaf_id: &str,
    ) -> Result<Option<String>, CodingSessionError> {
        let source_leaf_id = normalize_leaf_id(source_leaf_id)?;
        let target_leaf_id = normalize_leaf_id(target_leaf_id)?;
        Ok(self
            .replay()?
            .transcript
            .into_iter()
            .rev()
            .find_map(|item| match item {
                TranscriptItem::BranchSummary {
                    summary,
                    source_leaf_id: summary_source_leaf_id,
                    target_leaf_id: summary_target_leaf_id,
                } if summary_source_leaf_id == source_leaf_id
                    && summary_target_leaf_id == target_leaf_id =>
                {
                    Some(summary)
                }
                _ => None,
            }))
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "durable delegation confirmation records retain every typed request fact"
    )]
    pub(crate) fn record_delegation_confirmation_requested(
        &mut self,
        source_operation_id: String,
        turn_id: String,
        tool_call_id: String,
        requesting_profile_id: ProfileId,
        target_kind: ProfileKind,
        target_id: ProfileId,
        task: String,
        reason: String,
        runtime_seed: PersistedDelegationRuntimeSeed,
    ) -> Result<(), CodingSessionError> {
        self.append_durable_session_event(
            Some(source_operation_id.clone()),
            Some(turn_id.clone()),
            SessionEventData::DelegationConfirmationRequested {
                source_operation_id,
                turn_id,
                tool_call_id,
                requesting_profile_id,
                target_kind,
                target_id,
                task,
                reason,
                runtime_seed,
            },
        )
    }

    pub(crate) fn record_delegation_confirmation_approved(
        &mut self,
        source_operation_id: String,
        tool_call_id: String,
        approval_operation_id: String,
    ) -> Result<(), CodingSessionError> {
        self.append_durable_session_event(
            Some(source_operation_id.clone()),
            None,
            SessionEventData::DelegationConfirmationApproved {
                source_operation_id,
                tool_call_id,
                approval_operation_id,
            },
        )
    }

    pub(crate) fn record_delegation_confirmation_rejected(
        &mut self,
        source_operation_id: String,
        tool_call_id: String,
        reason: String,
    ) -> Result<(), CodingSessionError> {
        self.append_durable_session_event(
            Some(source_operation_id.clone()),
            None,
            SessionEventData::DelegationConfirmationRejected {
                source_operation_id,
                tool_call_id,
                reason,
            },
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn record_delegation_folded_update(
        &mut self,
        tool_call_id: String,
        requesting_profile_id: ProfileId,
        target_kind: ProfileKind,
        target_id: ProfileId,
        task: String,
        status: PersistedDelegationStatus,
        child_operation_id: Option<String>,
        summary: Option<String>,
    ) -> Result<(), CodingSessionError> {
        self.append_durable_session_event(
            None,
            None,
            SessionEventData::DelegationFoldedUpdated {
                tool_call_id,
                requesting_profile_id,
                target_kind,
                target_id,
                task,
                status,
                child_operation_id,
                summary,
            },
        )
    }

    pub(crate) fn switch_active_leaf(
        &mut self,
        target_leaf_id: &str,
        operation_id: &str,
    ) -> Result<(), CodingSessionError> {
        let target_leaf_id = normalize_leaf_id(target_leaf_id)?;
        let events = self.store.read_events(&self.handle)?;
        if committed_leaf_cutoff(&events, &target_leaf_id).is_none() {
            return Err(CodingSessionError::Session {
                message: format!("leaf id not found in session: {target_leaf_id}"),
            });
        }

        let session_id = self.session_id().to_owned();
        let mut ids = SystemIdGenerator;
        let clock = SystemClock;
        let updated_at = clock.now_rfc3339();
        let event = SessionEventEnvelope::new(
            session_id.clone(),
            ids.next_event_id(),
            updated_at.clone(),
            SessionEventData::ActiveLeafChanged {
                leaf_id: target_leaf_id.clone(),
            },
        );
        self.commit_writer_mutation(
            vec![event],
            ManifestPatch::new()
                .updated_at(updated_at)
                .active_leaf_id(Some(target_leaf_id)),
            Some(operation_id.to_owned()),
        )?;
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn begin_prompt_transaction(&self) -> PromptTurnTransaction {
        TurnTransaction::begin(
            &self.store,
            self.handle.clone(),
            SystemIdGenerator,
            SystemClock,
            OperationKind::Prompt,
        )
    }

    pub(crate) fn begin_prompt_transaction_with_snapshot(
        &self,
        snapshot: &OperationCapabilitySnapshot,
    ) -> PromptTurnTransaction {
        TurnTransaction::begin_admitted_with_runtime_generation(
            self.transaction_writer(),
            self.session_id().to_owned(),
            SystemIdGenerator,
            SystemClock,
            OperationKind::Prompt,
            snapshot.persisted_runtime_generation_ref(),
            snapshot.operation_id.clone(),
        )
    }

    pub(crate) fn begin_manual_compaction_transaction(
        &self,
        snapshot: &OperationCapabilitySnapshot,
    ) -> PromptTurnTransaction {
        TurnTransaction::begin_admitted_with_runtime_generation(
            self.transaction_writer(),
            self.session_id().to_owned(),
            SystemIdGenerator,
            SystemClock,
            OperationKind::ManualCompaction,
            snapshot.persisted_runtime_generation_ref(),
            snapshot.operation_id.clone(),
        )
    }

    pub(crate) fn begin_branch_summary_transaction(
        &self,
        snapshot: &OperationCapabilitySnapshot,
    ) -> PromptTurnTransaction {
        TurnTransaction::begin_admitted_with_runtime_generation(
            self.transaction_writer(),
            self.session_id().to_owned(),
            SystemIdGenerator,
            SystemClock,
            OperationKind::BranchSummary,
            snapshot.persisted_runtime_generation_ref(),
            snapshot.operation_id.clone(),
        )
    }

    pub(crate) fn begin_self_healing_edit_transaction(
        &self,
        snapshot: &OperationCapabilitySnapshot,
    ) -> PromptTurnTransaction {
        TurnTransaction::begin_admitted_with_runtime_generation(
            self.transaction_writer(),
            self.session_id().to_owned(),
            SystemIdGenerator,
            SystemClock,
            OperationKind::SelfHealingEdit,
            snapshot.persisted_runtime_generation_ref(),
            snapshot.operation_id.clone(),
        )
    }

    pub(crate) fn finalize_prompt_transaction(
        &mut self,
        transaction: Option<PromptTurnTransaction>,
        operation_id: impl Into<String>,
        outcome: &InternalPromptTurnOutcome,
    ) -> Result<FinalizedSessionWrite, CodingSessionError> {
        let operation_id = operation_id.into();
        match outcome {
            InternalPromptTurnOutcome::Success { .. } => {
                self.commit_prompt_transaction(transaction, operation_id)
            }
            InternalPromptTurnOutcome::Aborted { reason, .. } => {
                self.abort_prompt_transaction(transaction, operation_id, reason.clone())
            }
            InternalPromptTurnOutcome::Failed { error, .. } => self.fail_prompt_transaction(
                transaction,
                operation_id,
                error.code(),
                error.to_string(),
            ),
        }
    }

    pub(crate) fn commit_prompt_transaction(
        &mut self,
        transaction: Option<PromptTurnTransaction>,
        operation_id: impl Into<String>,
    ) -> Result<FinalizedSessionWrite, CodingSessionError> {
        let fallback_operation_id = operation_id.into();
        let Some(mut transaction) = transaction else {
            return Ok(Self::skipped_write(
                fallback_operation_id,
                "no active prompt transaction",
            ));
        };

        let operation_id = transaction.operation_id().to_owned();
        let session_id = self.session_id().to_owned();
        let new_leaf_id = Some(Self::next_leaf_id());
        let mut events = vec![EventService::session_write_pending_event(
            operation_id.clone(),
        )];
        let (committed, outbox_intent) = session_write_outbox_intent(&session_id, &operation_id);
        transaction.commit_with_outbox(new_leaf_id.clone(), outbox_intent)?;
        self.observe_committed_sequence(transaction.committed_session_sequence());
        events.push(committed);
        Ok(FinalizedSessionWrite {
            events,
            session_id: Some(session_id),
            leaf_id: new_leaf_id,
            committed_session_sequence: transaction.committed_session_sequence(),
        })
    }

    #[cfg(test)]
    pub(crate) fn commit_prompt_transaction_with_snapshot(
        &mut self,
        transaction: Option<PromptTurnTransaction>,
        operation_id: impl Into<String>,
        snapshot: &OperationCapabilitySnapshot,
    ) -> Result<FinalizedSessionWrite, CodingSessionError> {
        SessionWriteCapability::require(snapshot.session_write.as_ref())?;
        self.commit_prompt_transaction(transaction, operation_id)
    }

    pub(crate) fn fail_prompt_transaction(
        &mut self,
        transaction: Option<PromptTurnTransaction>,
        operation_id: impl Into<String>,
        error_code: impl Into<String>,
        message: impl Into<String>,
    ) -> Result<FinalizedSessionWrite, CodingSessionError> {
        self.fail_non_leaf_transaction(
            transaction,
            operation_id,
            error_code,
            message,
            "no active prompt transaction",
        )
    }

    pub(crate) fn commit_manual_compaction_transaction(
        &mut self,
        transaction: Option<PromptTurnTransaction>,
        operation_id: impl Into<String>,
    ) -> Result<FinalizedSessionWrite, CodingSessionError> {
        self.commit_non_leaf_transaction(
            transaction,
            operation_id,
            "no active manual compaction transaction",
        )
    }

    pub(crate) fn commit_branch_summary_transaction(
        &mut self,
        transaction: Option<PromptTurnTransaction>,
        operation_id: impl Into<String>,
    ) -> Result<FinalizedSessionWrite, CodingSessionError> {
        self.commit_non_leaf_transaction(
            transaction,
            operation_id,
            "no active branch summary transaction",
        )
    }

    pub(crate) fn commit_self_healing_edit_transaction(
        &mut self,
        transaction: Option<PromptTurnTransaction>,
        operation_id: impl Into<String>,
    ) -> Result<FinalizedSessionWrite, CodingSessionError> {
        self.commit_non_leaf_transaction(
            transaction,
            operation_id,
            "no active self-healing edit transaction",
        )
    }

    pub(crate) fn fail_self_healing_edit_transaction(
        &mut self,
        transaction: Option<PromptTurnTransaction>,
        operation_id: impl Into<String>,
        error_code: impl Into<String>,
        message: impl Into<String>,
    ) -> Result<FinalizedSessionWrite, CodingSessionError> {
        self.fail_non_leaf_transaction(
            transaction,
            operation_id,
            error_code,
            message,
            "no active self-healing edit transaction",
        )
    }

    fn commit_non_leaf_transaction(
        &mut self,
        transaction: Option<PromptTurnTransaction>,
        operation_id: impl Into<String>,
        missing_transaction_reason: &'static str,
    ) -> Result<FinalizedSessionWrite, CodingSessionError> {
        let fallback_operation_id = operation_id.into();
        let Some(mut transaction) = transaction else {
            return Ok(Self::skipped_write(
                fallback_operation_id,
                missing_transaction_reason,
            ));
        };

        let operation_id = transaction.operation_id().to_owned();
        let session_id = self.session_id().to_owned();
        let mut events = vec![EventService::session_write_pending_event(
            operation_id.clone(),
        )];
        let (committed, outbox_intent) = session_write_outbox_intent(&session_id, &operation_id);
        transaction.commit_with_outbox(None, outbox_intent)?;
        self.observe_committed_sequence(transaction.committed_session_sequence());
        self.commit_writer_mutation(
            Vec::new(),
            ManifestPatch::new().updated_at(SystemClock.now_rfc3339()),
            Some(operation_id.clone()),
        )?;
        events.push(committed);
        Ok(FinalizedSessionWrite {
            events,
            session_id: Some(session_id),
            leaf_id: self.current_active_leaf_id(),
            committed_session_sequence: transaction.committed_session_sequence(),
        })
    }

    fn fail_non_leaf_transaction(
        &mut self,
        transaction: Option<PromptTurnTransaction>,
        operation_id: impl Into<String>,
        error_code: impl Into<String>,
        message: impl Into<String>,
        missing_transaction_reason: &'static str,
    ) -> Result<FinalizedSessionWrite, CodingSessionError> {
        let fallback_operation_id = operation_id.into();
        let Some(mut transaction) = transaction else {
            return Ok(Self::skipped_write(
                fallback_operation_id,
                missing_transaction_reason,
            ));
        };

        let operation_id = transaction.operation_id().to_owned();
        let session_id = self.session_id().to_owned();
        let mut events = vec![EventService::session_write_pending_event(
            operation_id.clone(),
        )];
        let (committed, outbox_intent) = session_write_outbox_intent(&session_id, &operation_id);
        transaction.fail_with_outbox(error_code, message, outbox_intent)?;
        self.observe_committed_sequence(transaction.committed_session_sequence());
        self.commit_writer_mutation(
            Vec::new(),
            ManifestPatch::new().updated_at(SystemClock.now_rfc3339()),
            Some(operation_id.clone()),
        )?;
        events.push(committed);
        Ok(FinalizedSessionWrite {
            events,
            session_id: Some(session_id),
            leaf_id: self.current_active_leaf_id(),
            committed_session_sequence: transaction.committed_session_sequence(),
        })
    }

    pub(crate) fn record_self_healing_edit_started(
        transaction: &mut PromptTurnTransaction,
        path: String,
        replacements: usize,
    ) -> Result<(), CodingSessionError> {
        transaction.record_self_healing_edit_started(path, replacements)
    }

    pub(crate) fn record_self_healing_edit_repair_attempted(
        transaction: &mut PromptTurnTransaction,
        path: &str,
        repair: &SelfHealingEditRepairAttempt,
    ) -> Result<(), CodingSessionError> {
        transaction.record_self_healing_edit_repair_attempted(path, repair)
    }

    pub(crate) fn record_self_healing_edit_completed(
        transaction: &mut PromptTurnTransaction,
        outcome: &SelfHealingEditOutcome,
    ) -> Result<(), CodingSessionError> {
        transaction.record_self_healing_edit_completed(outcome)
    }

    pub(crate) fn abort_prompt_transaction(
        &mut self,
        transaction: Option<PromptTurnTransaction>,
        operation_id: impl Into<String>,
        reason: impl Into<String>,
    ) -> Result<FinalizedSessionWrite, CodingSessionError> {
        let fallback_operation_id = operation_id.into();
        let Some(mut transaction) = transaction else {
            return Ok(Self::skipped_write(
                fallback_operation_id,
                "no active prompt transaction",
            ));
        };

        let operation_id = transaction.operation_id().to_owned();
        let session_id = self.session_id().to_owned();
        let mut events = vec![EventService::session_write_pending_event(
            operation_id.clone(),
        )];
        let (committed, outbox_intent) = session_write_outbox_intent(&session_id, &operation_id);
        transaction.abort_with_outbox(reason, outbox_intent)?;
        self.observe_committed_sequence(transaction.committed_session_sequence());
        events.push(committed);
        Ok(FinalizedSessionWrite {
            events,
            session_id: Some(session_id),
            leaf_id: None,
            committed_session_sequence: transaction.committed_session_sequence(),
        })
    }

    pub(crate) fn skip_prompt_transaction(
        operation_id: impl Into<String>,
        reason: impl Into<String>,
    ) -> FinalizedSessionWrite {
        Self::skipped_write(operation_id, reason)
    }

    pub(crate) fn failed_prompt_transaction(
        operation_id: impl Into<String>,
        error: &CodingSessionError,
    ) -> FinalizedSessionWrite {
        let operation_id = operation_id.into();
        let status = if matches!(error, CodingSessionError::PartialCommit { .. }) {
            CodingAgentSessionWriteFailureStatus::Uncertain
        } else {
            CodingAgentSessionWriteFailureStatus::Definite
        };
        FinalizedSessionWrite {
            events: vec![
                EventService::session_write_pending_event(operation_id.clone()),
                EventService::session_write_failed_event(operation_id, error.to_string(), status),
            ],
            session_id: None,
            leaf_id: None,
            committed_session_sequence: None,
        }
    }

    #[cfg(test)]
    pub(crate) fn session_dir(&self) -> &Path {
        self.handle.session_dir()
    }

    #[cfg(test)]
    pub(crate) fn fail_store_after_for_tests(
        &self,
        point: StoreFailurePoint,
        successful_calls: usize,
    ) {
        self.store.fail_after(point, successful_calls);
    }

    pub(crate) fn replay(&self) -> Result<SessionReplay, CodingSessionError> {
        self.store.replay_session(&self.handle)
    }

    pub(crate) fn event_writer(&self) -> SessionEventWriter {
        SessionEventWriter {
            session_id: self.handle.manifest().session_id.clone(),
            writer: self.transaction_writer(),
            committed_session_sequence: self.committed_session_sequence.clone(),
        }
    }

    pub(crate) fn arm_auto_name_for_prompt(&mut self, replay: &SessionReplay) {
        let has_conversation = replay.transcript.iter().any(|item| {
            matches!(
                item,
                TranscriptItem::UserInput { .. }
                    | TranscriptItem::AssistantMessage {
                        status: MessageStatus::Completed,
                        ..
                    }
            )
        });
        self.auto_name_eligible_for_active_prompt =
            !has_conversation && self.transaction_writer.manifest_snapshot().name.is_none();
    }

    pub(crate) fn take_auto_name_writer_after_prompt(&mut self) -> Option<SessionAutoNameWriter> {
        if !std::mem::take(&mut self.auto_name_eligible_for_active_prompt)
            || self.transaction_writer.manifest_snapshot().name.is_some()
        {
            return None;
        }
        Some(SessionAutoNameWriter {
            session_id: self.handle.manifest().session_id.clone(),
            writer: self.transaction_writer(),
            committed_session_sequence: self.committed_session_sequence.clone(),
            session_name_updates: self.session_name_updates.clone(),
        })
    }

    pub(crate) fn subscribe_session_name_updates(&self) -> watch::Receiver<SessionNameUpdate> {
        self.session_name_updates.subscribe()
    }

    pub(crate) fn committed_session_sequence(&self) -> u64 {
        self.committed_session_sequence.load(Ordering::Acquire)
    }

    pub(crate) fn recovery_id_for_uncertain_operation(
        &self,
        operation_id: &str,
    ) -> Result<String, CodingSessionError> {
        let outbox = self.store.read_outbox(&self.handle)?;
        if let Some(recovery_id) = outbox.iter().find_map(|record| {
            if record.operation_id.as_deref() != Some(operation_id)
                || record.kind != DurableOutboxRecordKind::Recovery
            {
                return None;
            }
            match &record.draft.event {
                crate::events::CodingAgentProductEventKind::Workflow(
                    crate::events::CodingAgentWorkflowProductEvent::OperationRecoveryPending {
                        recovery_id,
                        ..
                    },
                ) => Some(recovery_id.clone()),
                _ => None,
            }
        }) {
            return Ok(recovery_id);
        }
        if let Some(record) = outbox.into_iter().find(|record| {
            record.operation_id.as_deref() == Some(operation_id)
                && record.kind == DurableOutboxRecordKind::SessionWrite
        }) {
            return Ok(format!("recovery_pending:{}", record.record_id));
        }
        let has_durable_fact = self
            .store
            .read_events(&self.handle)?
            .into_iter()
            .any(|event| event.operation_id.as_deref() == Some(operation_id));
        if has_durable_fact {
            return Ok(format!(
                "recovery_pending:{}/{}",
                self.session_id(),
                operation_id
            ));
        }
        Err(CodingSessionError::PartialCommit {
            operation_id: operation_id.to_owned(),
            message: "partial commit has no durable fact or outbox evidence".into(),
        })
    }

    pub(crate) fn persist_terminal_decision(
        &self,
        decision: &FinalizationDecision,
        draft: ProductEventDraft,
    ) -> Result<(), CodingSessionError> {
        let mut ids = SystemIdGenerator;
        let event = SessionEventEnvelope::new(
            self.session_id(),
            ids.next_event_id(),
            SystemClock.now_rfc3339(),
            SessionEventData::OperationTerminalRecorded {
                status: decision.terminal_status.as_str().into(),
                semantic_event_id: decision.semantic_event_id.clone(),
            },
        )
        .with_operation_id(decision.operation_id.clone());
        let intent = DurableOutboxRecordCandidate::new(
            decision.semantic_event_id.clone(),
            self.session_id().to_owned(),
            Some(decision.operation_id.clone()),
            vec![event.event_id.clone()],
            DurableOutboxRecordKind::OperationTerminal,
            draft.with_durable_session(self.session_id()),
        )
        .map_err(|message| CodingSessionError::Session {
            message: message.into(),
        })?
        .with_operation_kind(decision.operation_kind.as_str());
        let receipt = self
            .transaction_writer
            .commit_session_mutation_with_outbox(
                vec![event],
                vec![intent],
                ManifestPatch::new().updated_at(SystemClock.now_rfc3339()),
                Some(decision.operation_id.clone()),
            )?;
        observe_commit_receipt(&self.committed_session_sequence, receipt);
        Ok(())
    }

    pub(crate) fn take_startup_outbox_records(&mut self) -> Vec<DurableOutboxRecord> {
        std::mem::take(&mut self.startup_outbox_records)
    }

    fn observe_committed_sequence(&self, sequence: Option<u64>) {
        if let Some(sequence) = sequence {
            self.committed_session_sequence
                .fetch_max(sequence, Ordering::AcqRel);
        }
    }

    fn transaction_writer(&self) -> SessionTransactionWriter {
        self.transaction_writer.clone()
    }

    fn commit_writer_mutation(
        &self,
        events: Vec<SessionEventEnvelope>,
        manifest_patch: ManifestPatch,
        operation_id: Option<String>,
    ) -> Result<(), CodingSessionError> {
        let receipt = self.transaction_writer.commit_session_mutation(
            events,
            manifest_patch,
            operation_id,
        )?;
        observe_commit_receipt(&self.committed_session_sequence, receipt);
        Ok(())
    }

    fn commit_writer_mutation_with_outbox(
        &self,
        events: Vec<SessionEventEnvelope>,
        outbox_records: Vec<DurableOutboxRecordCandidate>,
        manifest_patch: ManifestPatch,
        operation_id: Option<String>,
    ) -> Result<(), CodingSessionError> {
        let receipt = self
            .transaction_writer
            .commit_session_mutation_with_outbox(
                events,
                outbox_records,
                manifest_patch,
                operation_id,
            )?;
        observe_commit_receipt(&self.committed_session_sequence, receipt);
        Ok(())
    }

    pub(crate) fn shutdown_transaction_writer(&self) -> Result<(), CodingSessionError> {
        self.transaction_writer.shutdown()
    }

    #[cfg(test)]
    pub(crate) fn recovery_summary(&self) -> Result<SessionRecoverySummary, CodingSessionError> {
        Ok(self.replay()?.recovery_summary())
    }

    pub(crate) fn inspect_recovery_pending(
        &self,
    ) -> Result<Vec<RecoveryPendingInspection>, CodingSessionError> {
        let replay = self.replay()?;
        let outbox = self.store.read_outbox(&self.handle)?;
        let mut pending_operation_ids = replay
            .recovery_summary()
            .in_doubt_operations
            .into_iter()
            .collect::<std::collections::BTreeSet<_>>();
        for record in outbox.iter().filter(|record| {
            record.kind == DurableOutboxRecordKind::SessionWrite
                && matches!(
                    record.draft.durability,
                    CodingAgentProductEventDurability::PersistenceUncertain { .. }
                )
        }) {
            if replay
                .operation_statuses
                .get(record.operation_id.as_deref().unwrap_or_default())
                .is_none_or(|status| {
                    !matches!(
                        status,
                        crate::session::replay::OperationReplayStatus::Recovered
                            | crate::session::replay::OperationReplayStatus::Committed
                            | crate::session::replay::OperationReplayStatus::Failed
                            | crate::session::replay::OperationReplayStatus::Aborted
                    )
                })
                && let Some(operation_id) = &record.operation_id
            {
                pending_operation_ids.insert(operation_id.clone());
            }
        }
        let durable_events = self.store.read_events(&self.handle)?;
        let operation_facts = durable_events
            .iter()
            .filter_map(|event| match &event.data {
                SessionEventData::OperationStarted {
                    operation,
                    runtime_generation,
                } => event.operation_id.clone().map(|id| {
                    (
                        id,
                        (operation.clone(), runtime_generation.capability_generation),
                    )
                }),
                _ => None,
            })
            .collect::<std::collections::HashMap<_, _>>();
        let pending_facts = durable_events
            .into_iter()
            .filter_map(|event| match event.data {
                SessionEventData::OperationRecoveryPending {
                    recovery_id,
                    record_version,
                    descriptor_revision,
                    capability_generation,
                    attempt_count,
                    last_attempt_at,
                    next_attempt_at,
                    ..
                } => event.operation_id.map(|id| {
                    (
                        id,
                        (
                            recovery_id,
                            record_version,
                            descriptor_revision,
                            capability_generation,
                            attempt_count,
                            last_attempt_at,
                            next_attempt_at,
                        ),
                    )
                }),
                _ => None,
            })
            .collect::<std::collections::HashMap<_, _>>();
        pending_operation_ids
            .into_iter()
            .map(|operation_id| {
                let operation_kind = operation_facts
                    .get(&operation_id)
                    .map(|(kind, _)| persisted_operation_kind_name(kind));
                let operation_capability_generation = operation_facts
                    .get(&operation_id)
                    .and_then(|(_, generation)| *generation);
                let (
                    recovery_id,
                    record_version,
                    descriptor_revision,
                    capability_generation,
                    attempt_count,
                    last_attempt_at,
                    next_attempt_at,
                ) = pending_facts.get(&operation_id).cloned().unwrap_or((
                    self.recovery_id_for_uncertain_operation(&operation_id)?,
                    RECOVERY_RECORD_VERSION,
                    crate::runtime::operation::contract::OPERATION_DESCRIPTOR_REVISION,
                    operation_capability_generation,
                    0,
                    None,
                    None,
                ));
                Ok(RecoveryPendingInspection {
                    operation_id,
                    recovery_id,
                    operation_kind,
                    record_version,
                    descriptor_revision,
                    capability_generation,
                    attempt_count,
                    last_attempt_at,
                    next_attempt_at,
                })
            })
            .collect()
    }

    pub(crate) fn resolve_recovery_as(
        &self,
        request: &crate::runtime::facade::CodingAgentRecoveryResolutionRequest,
        authorization_subject: &str,
    ) -> Result<RecoveryResolutionCommit, CodingSessionError> {
        let pending = self
            .inspect_recovery_pending()?
            .into_iter()
            .find(|pending| pending.recovery_id == request.recovery_id)
            .ok_or_else(|| CodingSessionError::Input {
                message: format!(
                    "unknown or already resolved recovery: {}",
                    request.recovery_id
                ),
            })?;
        if pending.operation_id != request.operation_id {
            return Err(CodingSessionError::Input {
                message: "recovery operation identity mismatch".into(),
            });
        }
        if pending.record_version != request.expected_record_version {
            return Err(CodingSessionError::Input {
                message: "recovery record version is stale".into(),
            });
        }
        if pending.descriptor_revision != request.expected_descriptor_revision {
            return Err(CodingSessionError::Input {
                message: "recovery descriptor revision is stale".into(),
            });
        }
        if pending.capability_generation != request.expected_capability_generation {
            return Err(CodingSessionError::Input {
                message: "recovery capability generation is stale".into(),
            });
        }
        if pending.attempt_count != request.expected_attempt_count {
            return Err(CodingSessionError::Input {
                message: "recovery attempt count is stale".into(),
            });
        }
        let reason = request.reason.trim();
        if reason.is_empty() {
            return Err(CodingSessionError::Input {
                message: "recovery resolution reason must not be empty".into(),
            });
        }
        if reason.chars().count() > 1_200 {
            return Err(CodingSessionError::Input {
                message: "recovery resolution reason exceeds 1200 characters".into(),
            });
        }
        let reason = crate::redaction::redact_sensitive_text(reason);
        let operation_kind = self
            .store
            .read_events(&self.handle)?
            .into_iter()
            .find_map(|event| match event.data {
                SessionEventData::OperationStarted { operation, .. }
                    if event.operation_id.as_deref() == Some(request.operation_id.as_str()) =>
                {
                    Some(operation)
                }
                _ => None,
            })
            .ok_or_else(|| CodingSessionError::Session {
                message: "recovery resolution requires the original operation kind".into(),
            })?;
        if matches!(
            operation_kind,
            crate::session::event::OperationKind::Other { .. }
                | crate::session::event::OperationKind::SessionTreeLabel
        ) {
            return Err(CodingSessionError::UnsupportedCapability {
                capability: "recovery resolution requires a durable root operation family".into(),
            });
        }
        let persisted_resolution = match request.resolution {
            crate::events::CodingAgentRecoveryResolution::Failed => {
                crate::session::event::PersistedRecoveryResolution::Failed
            }
            crate::events::CodingAgentRecoveryResolution::Aborted => {
                crate::session::event::PersistedRecoveryResolution::Aborted
            }
        };
        let session_id = self.session_id().to_owned();
        let observed_at = SystemClock.now_rfc3339();
        let semantic_event_id = format!(
            "{}/{}/recovery_resolution/v{}",
            session_id, request.operation_id, pending.record_version
        );
        let mut ids = SystemIdGenerator;
        let audit_event = SessionEventEnvelope::new(
            session_id.clone(),
            ids.next_event_id(),
            observed_at.clone(),
            SessionEventData::OperationRecoveryResolved {
                recovery_id: pending.recovery_id.clone(),
                record_version: pending.record_version,
                descriptor_revision: pending.descriptor_revision,
                capability_generation: pending.capability_generation,
                resolution: persisted_resolution,
                reason: reason.clone(),
                authorization_subject: authorization_subject.to_owned(),
            },
        )
        .with_operation_id(request.operation_id.clone());
        let status_event = SessionEventEnvelope::new(
            session_id.clone(),
            ids.next_event_id(),
            observed_at.clone(),
            match request.resolution {
                crate::events::CodingAgentRecoveryResolution::Failed => {
                    SessionEventData::OperationFailed {
                        error_code: "recovery_resolved".into(),
                        message: reason.clone(),
                    }
                }
                crate::events::CodingAgentRecoveryResolution::Aborted => {
                    SessionEventData::OperationAborted {
                        reason: reason.clone(),
                    }
                }
            },
        )
        .with_operation_id(request.operation_id.clone());
        let terminal_event = SessionEventEnvelope::new(
            session_id.clone(),
            ids.next_event_id(),
            observed_at.clone(),
            SessionEventData::OperationTerminalRecorded {
                status: match request.resolution {
                    crate::events::CodingAgentRecoveryResolution::Failed => "failed",
                    crate::events::CodingAgentRecoveryResolution::Aborted => "aborted",
                }
                .into(),
                semantic_event_id: semantic_event_id.clone(),
            },
        )
        .with_operation_id(request.operation_id.clone());
        let draft = crate::events::recovery::RecoveryResolvedEvent {
            operation_id: request.operation_id.clone(),
            recovery_id: pending.recovery_id.clone(),
            resolution: request.resolution,
            reason,
            session_id: session_id.clone(),
            record_version: pending.record_version,
            descriptor_revision: pending.descriptor_revision,
            capability_generation: pending.capability_generation,
        }
        .into_product_draft();
        let source_event_ids = vec![
            audit_event.event_id.clone(),
            status_event.event_id.clone(),
            terminal_event.event_id.clone(),
        ];
        let outbox = DurableOutboxRecordCandidate::new(
            semantic_event_id,
            session_id,
            Some(request.operation_id.clone()),
            source_event_ids,
            DurableOutboxRecordKind::OperationTerminal,
            draft.clone(),
        )
        .map_err(|message| CodingSessionError::Session {
            message: message.into(),
        })?
        .with_operation_kind(persisted_operation_kind_name(&operation_kind));
        self.commit_writer_mutation_with_outbox(
            vec![audit_event, status_event, terminal_event],
            vec![outbox],
            ManifestPatch::new().updated_at(observed_at),
            Some(request.operation_id.clone()),
        )?;
        Ok(RecoveryResolutionCommit {
            operation_id: request.operation_id.clone(),
            recovery_id: pending.recovery_id,
            resolution: request.resolution,
            operation_kind,
            draft,
        })
    }

    pub(crate) fn retry_recovery(
        &self,
        request: &crate::runtime::facade::CodingAgentRecoveryRetryRequest,
    ) -> Result<RecoveryRetryCommit, CodingSessionError> {
        let pending = self
            .inspect_recovery_pending()?
            .into_iter()
            .find(|pending| pending.recovery_id == request.recovery_id)
            .ok_or_else(|| CodingSessionError::Input {
                message: format!(
                    "unknown or already resolved recovery: {}",
                    request.recovery_id
                ),
            })?;
        if pending.operation_id != request.operation_id {
            return Err(CodingSessionError::Input {
                message: "recovery operation identity mismatch".into(),
            });
        }
        if pending.record_version != request.expected_record_version {
            return Err(CodingSessionError::Input {
                message: "recovery record version is stale".into(),
            });
        }
        if pending.descriptor_revision != request.expected_descriptor_revision {
            return Err(CodingSessionError::Input {
                message: "recovery descriptor revision is stale".into(),
            });
        }
        if pending.capability_generation != request.expected_capability_generation {
            return Err(CodingSessionError::Input {
                message: "recovery capability generation is stale".into(),
            });
        }
        if pending.attempt_count != request.expected_attempt_count {
            return Err(CodingSessionError::Input {
                message: "recovery attempt count is stale".into(),
            });
        }
        if pending.attempt_count >= MAX_RECOVERY_RETRY_ATTEMPTS {
            return Err(CodingSessionError::Input {
                message: format!("recovery retry limit reached: {MAX_RECOVERY_RETRY_ATTEMPTS}"),
            });
        }
        let operation_kind = self
            .store
            .read_events(&self.handle)?
            .into_iter()
            .find_map(|event| match event.data {
                SessionEventData::OperationStarted { operation, .. }
                    if event.operation_id.as_deref() == Some(request.operation_id.as_str()) =>
                {
                    Some(operation)
                }
                _ => None,
            })
            .ok_or_else(|| CodingSessionError::Session {
                message: "recovery retry requires the original operation kind".into(),
            })?;
        if matches!(
            operation_kind,
            crate::session::event::OperationKind::Other { .. }
                | crate::session::event::OperationKind::SessionTreeLabel
        ) {
            return Err(CodingSessionError::UnsupportedCapability {
                capability: "recovery retry requires a durable root operation family".into(),
            });
        }
        let session_id = self.session_id().to_owned();
        let last_attempt_at = SystemClock.now_rfc3339();
        let attempt_count = pending.attempt_count + 1;
        let reason = if request.schedule_with_backoff {
            "recovery retry scheduled deterministic backoff after durable inspection"
        } else {
            "recovery retry inspected durable facts and outbox; operation remains pending"
        };
        let next_attempt_at = request
            .schedule_with_backoff
            .then(|| recovery_next_attempt_at(&last_attempt_at, attempt_count))
            .transpose()?;
        let mut ids = SystemIdGenerator;
        let event = SessionEventEnvelope::new(
            session_id.clone(),
            ids.next_event_id(),
            last_attempt_at.clone(),
            SessionEventData::OperationRecoveryPending {
                reason: reason.into(),
                recovery_id: pending.recovery_id.clone(),
                record_version: pending.record_version,
                descriptor_revision: pending.descriptor_revision,
                capability_generation: pending.capability_generation,
                attempt_count,
                last_attempt_at: Some(last_attempt_at.clone()),
                next_attempt_at: next_attempt_at.clone(),
            },
        )
        .with_operation_id(request.operation_id.clone());
        let draft = crate::events::recovery::RecoveryPendingEvent {
            operation_id: request.operation_id.clone(),
            recovery_id: pending.recovery_id.clone(),
            reason: reason.into(),
            session_id: session_id.clone(),
            record_version: pending.record_version,
            descriptor_revision: pending.descriptor_revision,
            capability_generation: pending.capability_generation,
            attempt_count,
            last_attempt_at: Some(last_attempt_at.clone()),
            next_attempt_at: next_attempt_at.clone(),
        }
        .into_product_draft();
        let outbox = DurableOutboxRecordCandidate::new(
            format!(
                "{}/{}/recovery_pending/retry/{}",
                session_id, request.operation_id, attempt_count
            ),
            session_id,
            Some(request.operation_id.clone()),
            vec![event.event_id.clone()],
            DurableOutboxRecordKind::Recovery,
            draft.clone(),
        )
        .map_err(|message| CodingSessionError::Session {
            message: message.into(),
        })?;
        self.commit_writer_mutation_with_outbox(
            vec![event],
            vec![outbox],
            ManifestPatch::new().updated_at(last_attempt_at.clone()),
            Some(request.operation_id.clone()),
        )?;
        Ok(RecoveryRetryCommit {
            operation_id: request.operation_id.clone(),
            recovery_id: pending.recovery_id,
            operation_kind,
            capability_generation: pending.capability_generation,
            draft,
            attempt_count,
            last_attempt_at,
            next_attempt_at,
        })
    }

    fn apply_startup_recovery(&mut self) -> Result<(), CodingSessionError> {
        let replay = self.replay()?;
        let in_doubt_operations = replay.recovery_summary().in_doubt_operations;
        let pending_tool_authorizations = replay.pending_tool_authorizations;
        if in_doubt_operations.is_empty() && pending_tool_authorizations.is_empty() {
            return Ok(());
        }

        let session_id = self.session_id().to_owned();
        let mut ids = SystemIdGenerator;
        let clock = SystemClock;
        let observed_at = clock.now_rfc3339();
        let reason =
            "startup recovery retained incomplete operation as recovery-pending".to_owned();
        let authorization_reason =
            "startup recovery interrupted unresolved tool authorization".to_owned();
        let durable_events = self.store.read_events(&self.handle)?;
        let operation_facts = durable_events
            .iter()
            .filter_map(|event| match &event.data {
                SessionEventData::OperationStarted {
                    operation,
                    runtime_generation,
                } => event.operation_id.clone().map(|operation_id| {
                    (
                        operation_id,
                        (operation.clone(), runtime_generation.capability_generation),
                    )
                }),
                _ => None,
            })
            .collect::<std::collections::HashMap<_, _>>();
        let existing_pending = durable_events
            .iter()
            .filter_map(|event| match &event.data {
                SessionEventData::OperationRecoveryPending {
                    recovery_id,
                    record_version,
                    descriptor_revision,
                    capability_generation,
                    attempt_count,
                    last_attempt_at,
                    next_attempt_at,
                    ..
                } => event.operation_id.clone().map(|operation_id| {
                    (
                        operation_id,
                        (
                            recovery_id.clone(),
                            *record_version,
                            *descriptor_revision,
                            *capability_generation,
                            *attempt_count,
                            last_attempt_at.clone(),
                            next_attempt_at.clone(),
                        ),
                    )
                }),
                _ => None,
            })
            .collect::<std::collections::HashMap<_, _>>();
        let markers = in_doubt_operations
            .into_iter()
            .filter(|operation_id| !existing_pending.contains_key(operation_id))
            .map(|operation_id| {
                let recovery_id = format!("recovery_pending:{session_id}/{operation_id}");
                let (operation_kind, capability_generation) = operation_facts
                    .get(&operation_id)
                    .cloned()
                    .map(|(kind, generation)| (Some(kind), generation))
                    .unwrap_or((None, None));
                StartupRecoveryMarker {
                    operation_id,
                    recovery_id,
                    reason: reason.clone(),
                    session_id: session_id.clone(),
                    operation_kind,
                    capability_generation,
                    record_version: RECOVERY_RECORD_VERSION,
                    descriptor_revision:
                        crate::runtime::operation::contract::OPERATION_DESCRIPTOR_REVISION,
                    attempt_count: 0,
                    last_attempt_at: None,
                    next_attempt_at: None,
                }
            })
            .collect::<Vec<_>>();
        let mut retry_markers = existing_pending
            .iter()
            .filter_map(|(operation_id, pending)| {
                let next_attempt_at = pending.6.as_deref()?;
                if pending.4 >= MAX_RECOVERY_RETRY_ATTEMPTS
                    || !recovery_retry_is_due(&observed_at, next_attempt_at)
                {
                    return None;
                }
                let (operation_kind, _) = operation_facts.get(operation_id).cloned().unwrap_or((
                    OperationKind::Other {
                        name: "unknown".into(),
                    },
                    None,
                ));
                Some(StartupRecoveryMarker {
                    operation_id: operation_id.clone(),
                    recovery_id: pending.0.clone(),
                    reason: "automatic recovery retry inspected durable facts and outbox"
                        .to_owned(),
                    session_id: session_id.clone(),
                    operation_kind: Some(operation_kind),
                    capability_generation: pending.3,
                    record_version: pending.1,
                    descriptor_revision: pending.2,
                    attempt_count: pending.4 + 1,
                    last_attempt_at: Some(observed_at.clone()),
                    next_attempt_at: None,
                })
            })
            .collect::<Vec<_>>();
        let mut all_markers = markers;
        all_markers.append(&mut retry_markers);
        let recovery_events = all_markers
            .iter()
            .map(|marker| {
                SessionEventEnvelope::new(
                    session_id.clone(),
                    ids.next_event_id(),
                    observed_at.clone(),
                    SessionEventData::OperationRecoveryPending {
                        reason: marker.reason.clone(),
                        recovery_id: marker.recovery_id.clone(),
                        record_version: marker.record_version,
                        descriptor_revision: marker.descriptor_revision,
                        capability_generation: marker.capability_generation,
                        attempt_count: marker.attempt_count,
                        last_attempt_at: marker.last_attempt_at.clone(),
                        next_attempt_at: marker.next_attempt_at.clone(),
                    },
                )
                .with_operation_id(marker.operation_id.clone())
            })
            .collect::<Vec<_>>();
        let recovery_outbox = all_markers
            .iter()
            .zip(&recovery_events)
            .map(|(marker, event)| {
                DurableOutboxRecordCandidate::new(
                    if marker.attempt_count == 0 {
                        format!(
                            "{}/{}/recovery_pending",
                            marker.session_id, marker.operation_id
                        )
                    } else {
                        format!(
                            "{}/{}/recovery_pending/retry/{}",
                            marker.session_id, marker.operation_id, marker.attempt_count
                        )
                    },
                    marker.session_id.clone(),
                    Some(marker.operation_id.clone()),
                    vec![event.event_id.clone()],
                    DurableOutboxRecordKind::Recovery,
                    crate::events::recovery::RecoveryPendingEvent {
                        operation_id: marker.operation_id.clone(),
                        recovery_id: marker.recovery_id.clone(),
                        reason: marker.reason.clone(),
                        session_id: marker.session_id.clone(),
                        record_version: marker.record_version,
                        descriptor_revision: marker.descriptor_revision,
                        capability_generation: marker.capability_generation,
                        attempt_count: marker.attempt_count,
                        last_attempt_at: marker.last_attempt_at.clone(),
                        next_attempt_at: marker.next_attempt_at.clone(),
                    }
                    .into_product_draft(),
                )
                .map_err(|message| CodingSessionError::Session {
                    message: message.into(),
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        let mut events = recovery_events;
        events.extend(pending_tool_authorizations.into_iter().map(|request| {
            SessionEventEnvelope::new(
                session_id.clone(),
                ids.next_event_id(),
                observed_at.clone(),
                SessionEventData::ToolAuthorizationResolved {
                    authorization_id: request.authorization_id,
                    resolution: PersistedToolAuthorizationResolution::Interrupted {
                        reason: authorization_reason.clone(),
                    },
                },
            )
            .with_operation_id(request.operation_id)
            .with_turn_id(request.turn_id)
        }));

        if events.is_empty() {
            return Ok(());
        }

        self.commit_writer_mutation_with_outbox(
            events,
            recovery_outbox,
            ManifestPatch::new().updated_at(observed_at),
            None,
        )?;
        self.startup_recovery_markers.extend(all_markers);
        Ok(())
    }

    pub(crate) fn take_startup_recovery_markers(&mut self) -> Vec<StartupRecoveryMarker> {
        std::mem::take(&mut self.startup_recovery_markers)
    }

    pub(crate) fn view(&self) -> CodingAgentSessionView {
        CodingAgentSessionView {
            session_id: self.session_id().to_owned(),
            default_agent_profile_id: self.current_default_agent_profile_id(),
        }
    }

    pub(crate) fn hydrated_view(&self) -> Result<CodingAgentSessionHydration, CodingSessionError> {
        let replay = self.replay()?;
        let mut diagnostics = replay
            .diagnostics
            .into_iter()
            .map(|diagnostic| CodingAgentSessionDiagnostic {
                message: diagnostic.message,
            })
            .collect::<Vec<_>>();
        diagnostics.extend(
            self.transaction_writer
                .startup_storage_recoveries()
                .iter()
                .cloned()
                .map(|message| CodingAgentSessionDiagnostic { message }),
        );
        Ok(CodingAgentSessionHydration {
            summary: self.summary(),
            cwd: replay.cwd.clone(),
            transcript: replay
                .transcript
                .into_iter()
                .map(coding_transcript_item_from_replay)
                .collect(),
            diagnostics,
            usage: CodingAgentSessionUsageSummary {
                input: replay.usage.input,
                output: replay.usage.output,
                cache_read: replay.usage.cache_read,
                cache_write: replay.usage.cache_write,
                cost: replay.usage.cost,
                cost_known: replay.usage.cost_known,
                last_context_tokens: replay.usage.last_context_tokens,
            },
        })
    }

    pub(crate) fn export_context(
        &self,
        options: ExportOptions,
    ) -> Result<ExportContext, CodingSessionError> {
        Ok(ExportContext::new(options, self.summary(), self.replay()?))
    }

    fn summary(&self) -> CodingAgentSessionSummary {
        let manifest = self.transaction_writer.manifest_snapshot();
        CodingAgentSessionSummary {
            session_id: manifest.session_id,
            name: manifest.name,
            session_dir: self.handle.session_dir().to_path_buf(),
            created_at: manifest.created_at,
            updated_at: manifest.updated_at,
            active_leaf_id: manifest.active_leaf_id,
        }
    }

    fn copy_to_new_session(
        &self,
        target_leaf_id: Option<&str>,
        kind: SessionCopyKind,
        admitted_operation_id: Option<&str>,
    ) -> Result<Self, CodingSessionError> {
        let writer_manifest = self.transaction_writer.manifest_snapshot();
        let target_leaf_id = resolve_copy_target_leaf(&writer_manifest, target_leaf_id)?;
        let source_events = self.store.read_events(&self.handle)?;
        let cutoff = committed_leaf_cutoff(&source_events, &target_leaf_id).ok_or_else(|| {
            CodingSessionError::Session {
                message: format!("leaf id not found in source session: {target_leaf_id}"),
            }
        })?;

        let mut ids = SystemIdGenerator;
        let clock = SystemClock;
        let operation_id = admitted_operation_id
            .map(str::to_owned)
            .unwrap_or_else(|| ids.next_session_copy_id());
        let replay = self.replay()?;
        let workspace_scope = writer_manifest
            .workspace_scope
            .as_ref()
            .ok_or_else(|| CodingSessionError::Session {
                message: "legacy session workspace is unavailable for copy".into(),
            })?
            .to_product()
            .map_err(workspace_persistence_error)?;
        let target_session_id = ids.next_session_id();
        let target = Self::create_with_id(
            self.store.clone(),
            target_session_id,
            &mut ids,
            &clock,
            workspace_scope,
            replay.cwd,
            self.current_default_agent_profile_id(),
            writer_manifest.name,
            Some(&operation_id),
        )?;

        let copy_result = (|| {
            let provenance = SessionEventEnvelope::new(
                target.session_id().to_owned(),
                ids.next_event_id(),
                clock.now_rfc3339(),
                kind.provenance_event(self.session_id().to_owned(), target_leaf_id.clone()),
            );

            let branch_summary_operations = branch_summary_operation_ids_for_target(
                &source_events[cutoff + 1..],
                &target_leaf_id,
            );
            let copied_leaf_ids = committed_leaf_ids(&source_events[..=cutoff]);
            let tree_label_operations = tree_label_operation_ids_for_entries(
                &source_events[cutoff + 1..],
                &copied_leaf_ids,
            );
            let mut target_events = vec![provenance];
            target_events.extend(
                source_events[..=cutoff]
                    .iter()
                    .chain(source_events[cutoff + 1..].iter().filter(|event| {
                        should_copy_branch_summary_operation(
                            event,
                            &target_leaf_id,
                            &branch_summary_operations,
                        ) || should_copy_tree_label_operation(event, &tree_label_operations)
                    }))
                    .filter(|event| should_copy_source_event(event))
                    .map(|event| rewrite_event_for_session(event, target.session_id(), &mut ids))
                    .collect::<Vec<_>>(),
            );
            target.commit_writer_mutation(
                target_events,
                ManifestPatch::new()
                    .updated_at(clock.now_rfc3339())
                    .active_leaf_id(Some(target_leaf_id)),
                None,
            )?;
            Ok(())
        })();
        if let Err(error) = copy_result {
            if let Err(shutdown_error) = target.transaction_writer.shutdown() {
                return Err(CodingSessionError::PartialCommit {
                    operation_id,
                    message: format!(
                        "{error}; failed to close target session writer before cleanup: {shutdown_error}"
                    ),
                });
            }
            return Err(cleanup_failed_session_copy(
                &target.store,
                &target.handle,
                &operation_id,
                error,
            ));
        }

        Ok(target)
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "session creation atomically carries identity, presentation, persistence, and copy-recovery facts"
    )]
    fn create_with_id(
        store: SessionLogStore,
        session_id: String,
        ids: &mut impl IdGenerator,
        clock: &impl Clock,
        workspace_scope: CodingAgentWorkspaceScope,
        cwd: Option<String>,
        default_agent_profile_id: ProfileId,
        name: Option<String>,
        copy_operation_id: Option<&str>,
    ) -> Result<Self, CodingSessionError> {
        let created_at = clock.now_rfc3339();
        let persisted_workspace_scope = PersistedWorkspaceScope::from_product(&workspace_scope)
            .map_err(workspace_persistence_error)?;
        let handle = match store.create_session(
            CreateSessionOptions::new(session_id, created_at.clone())
                .name(name)
                .default_agent_profile_id(default_agent_profile_id)
                .workspace_scope(persisted_workspace_scope.clone()),
        ) {
            Ok(handle) => handle,
            Err(SessionCreateError::CleanupFailed {
                session_id,
                session_dir,
                create_error,
                cleanup_error,
            }) => match copy_operation_id {
                Some(operation_id) => {
                    return Err(CodingSessionError::PartialCommit {
                        operation_id: operation_id.to_owned(),
                        message: format!(
                            "session copy failed while creating {session_id} at {}: {create_error}; cleanup failed: {cleanup_error}",
                            session_dir.display()
                        ),
                    });
                }
                None => {
                    return Err(SessionCreateError::CleanupFailed {
                        session_id,
                        session_dir,
                        create_error,
                        cleanup_error,
                    }
                    .into());
                }
            },
            Err(error) => return Err(error.into()),
        };
        let created = SessionEventEnvelope::new(
            handle.manifest().session_id.clone(),
            ids.next_event_id(),
            created_at,
            SessionEventData::SessionCreated {
                cwd,
                workspace_scope: Some(persisted_workspace_scope),
            },
        );
        let service = Self::from_handle(store, handle)?;
        let receipt = match service
            .transaction_writer
            .initialize_session_with_receipt(created)
        {
            Ok(receipt) => receipt,
            Err(error) => {
                if let Err(shutdown_error) = service.transaction_writer.shutdown() {
                    return Err(match copy_operation_id {
                        Some(operation_id) => CodingSessionError::PartialCommit {
                            operation_id: operation_id.to_owned(),
                            message: format!(
                                "{error}; failed to close new session writer before cleanup: {shutdown_error}"
                            ),
                        },
                        None => shutdown_error,
                    });
                }
                return Err(match copy_operation_id {
                    Some(operation_id) => cleanup_failed_session_copy(
                        &service.store,
                        &service.handle,
                        operation_id,
                        error,
                    ),
                    None => error,
                });
            }
        };
        observe_commit_receipt(&service.committed_session_sequence, receipt);

        Ok(service)
    }

    fn append_durable_session_event(
        &mut self,
        operation_id: Option<String>,
        turn_id: Option<String>,
        data: SessionEventData,
    ) -> Result<(), CodingSessionError> {
        let session_id = self.session_id().to_owned();
        let mut ids = SystemIdGenerator;
        let clock = SystemClock;
        let updated_at = clock.now_rfc3339();
        let mut event = SessionEventEnvelope::new(
            session_id.clone(),
            ids.next_event_id(),
            updated_at.clone(),
            data,
        );
        event.operation_id = operation_id.clone();
        event.turn_id = turn_id;
        self.commit_writer_mutation(
            vec![event],
            ManifestPatch::new().updated_at(updated_at),
            operation_id.clone(),
        )?;
        Ok(())
    }

    fn skipped_write(
        operation_id: impl Into<String>,
        reason: impl Into<String>,
    ) -> FinalizedSessionWrite {
        FinalizedSessionWrite {
            events: vec![EventService::session_write_skipped_event(
                operation_id,
                reason,
            )],
            session_id: None,
            leaf_id: None,
            committed_session_sequence: None,
        }
    }

    fn next_leaf_id() -> String {
        let mut ids = SystemIdGenerator;
        ids.next_leaf_id()
    }
}

fn persisted_operation_kind_name(kind: &OperationKind) -> String {
    match kind {
        OperationKind::Prompt => "prompt".into(),
        OperationKind::ManualCompaction => "compact".into(),
        OperationKind::BranchSummary => "branch_summary".into(),
        OperationKind::Export => "export".into(),
        OperationKind::SelfHealingEdit => "self_healing_edit".into(),
        OperationKind::SessionTreeLabel => "session_tree_label".into(),
        OperationKind::Other { name } => name.clone(),
    }
}

fn recovery_next_attempt_at(
    last_attempt_at: &str,
    attempt_count: u32,
) -> Result<String, CodingSessionError> {
    let seconds = 1_i64 << attempt_count.saturating_sub(1).min(2);
    let timestamp = time::OffsetDateTime::parse(
        last_attempt_at,
        &time::format_description::well_known::Rfc3339,
    )
    .map_err(|error| CodingSessionError::Session {
        message: format!("recovery retry timestamp is invalid: {error}"),
    })?
    .checked_add(time::Duration::seconds(seconds))
    .ok_or_else(|| CodingSessionError::Session {
        message: "recovery retry timestamp overflow".into(),
    })?;
    timestamp
        .format(&time::format_description::well_known::Rfc3339)
        .map(|value| value.replace("+00:00", "Z"))
        .map_err(|error| CodingSessionError::Session {
            message: format!("recovery retry timestamp formatting failed: {error}"),
        })
}

fn recovery_retry_is_due(now: &str, next_attempt_at: &str) -> bool {
    let format = &time::format_description::well_known::Rfc3339;
    match (
        time::OffsetDateTime::parse(now, format),
        time::OffsetDateTime::parse(next_attempt_at, format),
    ) {
        (Ok(now), Ok(next)) => next <= now,
        _ => false,
    }
}

fn session_write_outbox_intent(
    session_id: &str,
    operation_id: &str,
) -> (SessionWriteEvent, DurableOutboxIntent) {
    let committed =
        EventService::session_write_committed_event(operation_id.to_owned(), session_id.to_owned());
    let intent = DurableOutboxIntent::new(
        format!("{session_id}/{operation_id}/session_write_committed"),
        DurableOutboxRecordKind::SessionWrite,
        committed.clone().into_product_draft(),
    );
    (committed, intent)
}

fn observe_commit_receipt(cursor: &AtomicU64, receipt: SessionCommitReceipt) {
    if let Some(sequence) = receipt.committed_session_sequence {
        cursor.fetch_max(sequence, Ordering::AcqRel);
    }
}

fn cleanup_failed_session_copy(
    store: &SessionLogStore,
    handle: &SessionHandle,
    operation_id: &str,
    copy_error: CodingSessionError,
) -> CodingSessionError {
    match store.remove_session(handle) {
        Ok(()) => copy_error,
        Err(cleanup_error) => CodingSessionError::PartialCommit {
            operation_id: operation_id.to_owned(),
            message: format!(
                "session copy failed after creating {}: {copy_error}; cleanup failed: {cleanup_error}",
                handle.manifest().session_id
            ),
        },
    }
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

fn resolve_copy_target_leaf(
    manifest: &crate::session::manifest::SessionManifest,
    target_leaf_id: Option<&str>,
) -> Result<String, CodingSessionError> {
    if let Some(target_leaf_id) = target_leaf_id {
        let target_leaf_id = target_leaf_id.trim();
        if target_leaf_id.is_empty() {
            return Err(CodingSessionError::Input {
                message: "target leaf id must not be empty".into(),
            });
        }
        return Ok(target_leaf_id.to_owned());
    }

    manifest
        .active_leaf_id
        .clone()
        .ok_or_else(|| CodingSessionError::Session {
            message: "session has no committed active leaf".into(),
        })
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

fn branch_summary_operation_ids_for_target(
    events: &[SessionEventEnvelope],
    target_leaf_id: &str,
) -> HashSet<String> {
    events
        .iter()
        .filter_map(|event| match &event.data {
            SessionEventData::BranchSummaryCreated {
                target_leaf_id: summary_target_leaf_id,
                ..
            } if summary_target_leaf_id == target_leaf_id => event.operation_id.clone(),
            _ => None,
        })
        .collect()
}

fn committed_leaf_ids(events: &[SessionEventEnvelope]) -> HashSet<String> {
    events
        .iter()
        .filter_map(|event| match &event.data {
            SessionEventData::OperationCommitted {
                new_leaf_id: Some(leaf_id),
            } => Some(leaf_id.clone()),
            _ => None,
        })
        .collect()
}

fn tree_label_operation_ids_for_entries(
    events: &[SessionEventEnvelope],
    entry_ids: &HashSet<String>,
) -> HashSet<String> {
    events
        .iter()
        .filter_map(|event| match &event.data {
            SessionEventData::SessionTreeLabelUpdated { entry_id, .. }
                if entry_ids.contains(entry_id) =>
            {
                event.operation_id.clone()
            }
            _ => None,
        })
        .collect()
}

fn should_copy_tree_label_operation(
    event: &SessionEventEnvelope,
    operation_ids: &HashSet<String>,
) -> bool {
    event
        .operation_id
        .as_ref()
        .is_some_and(|operation_id| operation_ids.contains(operation_id))
}

fn should_copy_branch_summary_operation(
    event: &SessionEventEnvelope,
    target_leaf_id: &str,
    operation_ids: &HashSet<String>,
) -> bool {
    if event
        .operation_id
        .as_ref()
        .is_some_and(|operation_id| operation_ids.contains(operation_id))
    {
        return true;
    }

    matches!(
        &event.data,
        SessionEventData::BranchSummaryCreated {
            target_leaf_id: summary_target_leaf_id,
            ..
        } if event.operation_id.is_none() && summary_target_leaf_id == target_leaf_id
    )
}

fn should_copy_source_event(event: &SessionEventEnvelope) -> bool {
    !matches!(
        event.data,
        SessionEventData::SessionCreated { .. }
            | SessionEventData::SessionCloned { .. }
            | SessionEventData::SessionForked { .. }
    )
}

fn rewrite_event_for_session(
    event: &SessionEventEnvelope,
    target_session_id: &str,
    ids: &mut impl IdGenerator,
) -> SessionEventEnvelope {
    let mut copied = event.clone();
    copied.session_id = target_session_id.to_owned();
    copied.event_id = ids.next_event_id();
    copied.parent_event_id = None;
    copied
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct LeafTreeEntry {
    leaf_id: String,
    parent_leaf_id: Option<String>,
    timestamp: String,
    text: String,
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

fn build_leaf_tree(
    events: &[SessionEventEnvelope],
    active_leaf_id: Option<String>,
    tree_labels: &HashMap<String, ReplayTreeLabel>,
) -> CodingAgentSessionTree {
    let mut operation_kinds = HashMap::new();
    let mut operation_inputs = HashMap::new();
    let mut leaves = Vec::new();
    let mut current_parent_leaf_id: Option<String> = None;

    for event in events {
        if let SessionEventData::ActiveLeafChanged { leaf_id } = &event.data {
            current_parent_leaf_id = Some(leaf_id.clone());
            continue;
        }
        let Some(operation_id) = event.operation_id.as_deref() else {
            continue;
        };
        match &event.data {
            SessionEventData::OperationStarted { operation, .. } => {
                operation_kinds.insert(operation_id.to_owned(), operation.clone());
            }
            SessionEventData::TurnInputRecorded { content } => {
                operation_inputs
                    .entry(operation_id.to_owned())
                    .or_insert_with(|| text_from_persisted_content(content));
            }
            SessionEventData::OperationCommitted {
                new_leaf_id: Some(leaf_id),
            } if operation_kinds.get(operation_id) == Some(&OperationKind::Prompt) => {
                leaves.push(LeafTreeEntry {
                    leaf_id: leaf_id.clone(),
                    parent_leaf_id: current_parent_leaf_id.clone(),
                    timestamp: event.created_at.clone(),
                    text: operation_inputs
                        .get(operation_id)
                        .filter(|text| !text.trim().is_empty())
                        .cloned()
                        .unwrap_or_else(|| leaf_id.clone()),
                });
                current_parent_leaf_id = Some(leaf_id.clone());
            }
            _ => {}
        }
    }

    CodingAgentSessionTree {
        tree: leaf_tree(leaves, tree_labels),
        active_leaf_id,
    }
}

fn text_from_persisted_content(content: &[PersistedContentBlock]) -> String {
    content
        .iter()
        .filter_map(|block| match block {
            PersistedContentBlock::Text { text } => Some(text.trim()),
            PersistedContentBlock::Thinking { thinking, .. } => Some(thinking.trim()),
            PersistedContentBlock::Image { .. } => None,
        })
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

fn leaf_tree(
    leaves: Vec<LeafTreeEntry>,
    tree_labels: &HashMap<String, ReplayTreeLabel>,
) -> Vec<SessionTreeNode> {
    let known_leaf_ids = leaves
        .iter()
        .map(|leaf| leaf.leaf_id.clone())
        .collect::<std::collections::HashSet<_>>();
    let mut children_by_parent: HashMap<Option<String>, Vec<LeafTreeEntry>> = HashMap::new();
    for mut leaf in leaves {
        if leaf
            .parent_leaf_id
            .as_ref()
            .is_some_and(|parent| !known_leaf_ids.contains(parent))
        {
            leaf.parent_leaf_id = None;
        }
        children_by_parent
            .entry(leaf.parent_leaf_id.clone())
            .or_default()
            .push(leaf);
    }
    build_leaf_children(None, &mut children_by_parent, tree_labels)
}

fn build_leaf_children(
    parent_leaf_id: Option<&str>,
    children_by_parent: &mut HashMap<Option<String>, Vec<LeafTreeEntry>>,
    tree_labels: &HashMap<String, ReplayTreeLabel>,
) -> Vec<SessionTreeNode> {
    let key = parent_leaf_id.map(str::to_owned);
    let leaves = children_by_parent.remove(&key).unwrap_or_default();
    leaves
        .into_iter()
        .map(|leaf| {
            let leaf_id = leaf.leaf_id.clone();
            let label = tree_labels.get(&leaf_id);
            let mut node = SessionTreeNode {
                entry: SessionEntry::message(
                    leaf.leaf_id,
                    leaf.parent_leaf_id,
                    leaf.timestamp,
                    StoredAgentMessage::User {
                        content: vec![ContentBlock::Text {
                            text: leaf.text,
                            text_signature: None,
                        }],
                        timestamp: 0,
                    },
                ),
                children: Vec::new(),
                label: label.and_then(|label| label.label.clone()),
                label_timestamp: label
                    .filter(|label| label.label.is_some())
                    .map(|label| label.updated_at.clone()),
            };
            node.children = build_leaf_children(Some(&leaf_id), children_by_parent, tree_labels);
            node
        })
        .collect()
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

pub(crate) fn coding_transcript_item_from_replay(
    item: TranscriptItem,
) -> CodingAgentSessionTranscriptItem {
    match item {
        TranscriptItem::UserInput { text, .. } => CodingAgentSessionTranscriptItem::User { text },
        TranscriptItem::AssistantMessage {
            message_id,
            content,
            status,
            reasoning_duration_millis,
        } => CodingAgentSessionTranscriptItem::Assistant {
            id: message_id,
            text: persisted_content_blocks_text(&content),
            thinking: persisted_content_blocks_thinking(&content),
            images: persisted_content_blocks_images(&content),
            done: !matches!(status, MessageStatus::Started),
            reasoning_duration_millis,
        },
        TranscriptItem::ToolCall {
            tool_call_id,
            name,
            arguments,
            status,
            summary,
            duration_millis,
            ..
        } => CodingAgentSessionTranscriptItem::Tool {
            call_id: tool_call_id,
            name,
            args: arguments,
            result: if summary.is_empty() {
                None
            } else {
                Some(summary)
            },
            is_error: matches!(status, ToolCallStatus::Failed),
            duration_millis,
        },
        TranscriptItem::DelegationBlock {
            tool_call_id,
            requesting_profile_id,
            target_kind,
            target_id,
            task,
            status,
            child_operation_id,
            summary,
        } => CodingAgentSessionTranscriptItem::Delegation {
            tool_call_id,
            requesting_profile_id,
            target_kind,
            target_id,
            task,
            status: delegation_status_label(status).into(),
            child_operation_id,
            summary,
        },
        TranscriptItem::CompactionSummary { summary, .. } => {
            CodingAgentSessionTranscriptItem::CompactionSummary { summary }
        }
        TranscriptItem::BranchSummary { summary, .. } => {
            CodingAgentSessionTranscriptItem::BranchSummary { summary }
        }
        TranscriptItem::Diagnostic { message, .. } => {
            CodingAgentSessionTranscriptItem::Diagnostic { message }
        }
    }
}

fn delegation_status_label(status: PersistedDelegationStatus) -> &'static str {
    match status {
        PersistedDelegationStatus::Requested => "requested",
        PersistedDelegationStatus::Running => "running",
        PersistedDelegationStatus::Completed => "completed",
        PersistedDelegationStatus::Failed => "failed",
        PersistedDelegationStatus::Rejected => "rejected",
        PersistedDelegationStatus::Cancelled => "cancelled",
        PersistedDelegationStatus::ConfirmationRequired => "confirmation_required",
    }
}

fn persisted_content_blocks_text(
    content: &[crate::session::event::PersistedContentBlock],
) -> String {
    content
        .iter()
        .filter_map(|block| match block {
            crate::session::event::PersistedContentBlock::Text { text } => Some(text.clone()),
            crate::session::event::PersistedContentBlock::Thinking { .. }
            | crate::session::event::PersistedContentBlock::Image { .. } => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn persisted_content_blocks_thinking(
    content: &[crate::session::event::PersistedContentBlock],
) -> String {
    content
        .iter()
        .filter_map(|block| match block {
            crate::session::event::PersistedContentBlock::Thinking { thinking, .. } => {
                Some(thinking.clone())
            }
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn persisted_content_blocks_images(
    content: &[crate::session::event::PersistedContentBlock],
) -> Vec<crate::events::CodingAgentImageContent> {
    content
        .iter()
        .filter_map(|block| match block {
            crate::session::event::PersistedContentBlock::Image { mime_type, data } => {
                Some(crate::events::CodingAgentImageContent {
                    mime_type: mime_type.clone(),
                    data: data.clone(),
                })
            }
            _ => None,
        })
        .collect()
}

impl From<SessionSummary> for CodingAgentSessionSummary {
    fn from(summary: SessionSummary) -> Self {
        Self {
            session_id: summary.session_id,
            name: summary.name,
            session_dir: summary.session_dir,
            created_at: summary.created_at,
            updated_at: summary.updated_at,
            active_leaf_id: summary.active_leaf_id,
        }
    }
}

fn resolve_session_log_root(
    options: &CodingAgentSessionOptions,
) -> Result<PathBuf, CodingSessionError> {
    if let Some(root) = options.session_log_root() {
        return Ok(root.to_path_buf());
    }
    crate::app::session::default_sessions_root().map_err(|error| CodingSessionError::Session {
        message: error.to_string(),
    })
}

fn open_target(options: &CodingAgentSessionOptions) -> Result<PathBuf, CodingSessionError> {
    if let Some(path) = options.session_path() {
        return Ok(path.to_path_buf());
    }
    let session_id = options
        .session_id()
        .ok_or_else(|| CodingSessionError::Input {
            message: "opening a coding session requires a session id or session path".into(),
        })?;
    Ok(PathBuf::from(normalize_session_id(
        session_id,
        "session id",
    )?))
}

fn normalize_session_id(value: &str, label: &str) -> Result<String, CodingSessionError> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(CodingSessionError::Input {
            message: format!("{label} must not be empty"),
        });
    }
    Ok(trimmed.to_owned())
}

fn option_cwd_string(options: &CodingAgentSessionOptions) -> Option<String> {
    options.cwd().map(normalized_path_string)
}

fn option_workspace_scope(
    options: &CodingAgentSessionOptions,
    session_id: &str,
) -> Result<CodingAgentWorkspaceScope, CodingSessionError> {
    let scope = match options.workspace_scope() {
        Some(CodingAgentWorkspaceScope::Legacy { .. }) => {
            return Err(CodingSessionError::Input {
                message: "new sessions cannot use a legacy workspace scope".into(),
            });
        }
        Some(scope) => scope.clone(),
        None => match options.cwd() {
            Some(cwd) => CodingAgentWorkspaceScope::Project {
                cwd: cwd.canonicalize().unwrap_or_else(|_| cwd.to_path_buf()),
            },
            None => CodingAgentWorkspaceScope::Projectless {
                workspace_id: projectless_workspace_id_for_session(session_id),
            },
        },
    };
    PersistedWorkspaceScope::from_product(&scope).map_err(workspace_persistence_error)?;
    Ok(scope)
}

fn workspace_global_config_dir(options: &CodingAgentSessionOptions) -> PathBuf {
    options
        .workspace_global_config_dir()
        .map(Path::to_path_buf)
        .unwrap_or_else(crate::app::embedding::global_config_directory)
}

fn migrate_workspace_on_open(
    store: &SessionLogStore,
    handle: SessionHandle,
    global_config_dir: &Path,
) -> Result<SessionHandle, CodingSessionError> {
    migrate_workspace_handle(store, handle, global_config_dir).map(|(handle, _)| handle)
}

fn migrate_workspace_handle(
    store: &SessionLogStore,
    handle: SessionHandle,
    global_config_dir: &Path,
) -> Result<(SessionHandle, CodingAgentWorkspaceMigration), CodingSessionError> {
    if handle.manifest().workspace_scope.is_some() {
        let scope = handle
            .manifest()
            .workspace_scope
            .as_ref()
            .expect("workspace scope checked above")
            .to_product()
            .map_err(workspace_persistence_error)?;
        let outcome = if handle.manifest().workspace_migrated_from_legacy {
            CodingAgentWorkspaceMigrationOutcome::Migrated
        } else {
            CodingAgentWorkspaceMigrationOutcome::NotRequired
        };
        let migration = workspace_migration_status(&scope, outcome, global_config_dir);
        return Ok((handle, migration));
    }
    let creation = store.session_creation_workspace_for_handle(&handle)?;
    let inference = match creation.workspace_scope {
        Some(scope) => {
            let scope = scope.to_product().map_err(workspace_persistence_error)?;
            crate::workspace::LegacyWorkspaceInference {
                migration: workspace_migration_status(
                    &scope,
                    CodingAgentWorkspaceMigrationOutcome::Pending,
                    global_config_dir,
                ),
                scope,
            }
        }
        None => infer_legacy_workspace(creation.cwd.as_deref(), global_config_dir),
    };
    if matches!(inference.scope, CodingAgentWorkspaceScope::Legacy { .. }) {
        return Ok((handle, inference.migration));
    }
    let persisted = PersistedWorkspaceScope::from_product(&inference.scope)
        .map_err(workspace_persistence_error)?;
    let handle = store.migrate_manifest_workspace(&handle, persisted)?;
    let migration = workspace_migration_status(
        &inference.scope,
        CodingAgentWorkspaceMigrationOutcome::Migrated,
        global_config_dir,
    );
    Ok((handle, migration))
}

struct SessionWorkspaceFacts {
    scope: CodingAgentWorkspaceScope,
    migration: CodingAgentWorkspaceMigration,
    compatibility_cwd: Option<String>,
}

fn workspace_facts_for_summary(
    store: &SessionLogStore,
    summary: &SessionSummary,
    global_config_dir: &Path,
) -> Result<SessionWorkspaceFacts, CodingSessionError> {
    if let Some(persisted) = summary.workspace_scope.as_ref() {
        let scope = persisted
            .to_product()
            .map_err(workspace_persistence_error)?;
        let outcome = if summary.workspace_migrated_from_legacy {
            CodingAgentWorkspaceMigrationOutcome::Migrated
        } else {
            CodingAgentWorkspaceMigrationOutcome::NotRequired
        };
        return Ok(SessionWorkspaceFacts {
            compatibility_cwd: compatibility_cwd(&scope),
            migration: workspace_migration_status(&scope, outcome, global_config_dir),
            scope,
        });
    }

    let creation = store.session_creation_workspace(summary)?;
    if let Some(persisted) = creation.workspace_scope {
        let scope = persisted
            .to_product()
            .map_err(workspace_persistence_error)?;
        return Ok(SessionWorkspaceFacts {
            compatibility_cwd: compatibility_cwd(&scope),
            migration: workspace_migration_status(
                &scope,
                CodingAgentWorkspaceMigrationOutcome::Pending,
                global_config_dir,
            ),
            scope,
        });
    }
    let inferred = infer_legacy_workspace(creation.cwd.as_deref(), global_config_dir);
    Ok(SessionWorkspaceFacts {
        compatibility_cwd: compatibility_cwd(&inferred.scope),
        migration: inferred.migration,
        scope: inferred.scope,
    })
}

fn compatibility_cwd(scope: &CodingAgentWorkspaceScope) -> Option<String> {
    match scope {
        CodingAgentWorkspaceScope::Project { cwd } => Some(normalized_path_string(cwd)),
        CodingAgentWorkspaceScope::Projectless { .. }
        | CodingAgentWorkspaceScope::Legacy { .. } => None,
    }
}

fn workspace_persistence_error(
    error: crate::workspace::CodingAgentWorkspaceResolutionError,
) -> CodingSessionError {
    CodingSessionError::Session {
        message: format!("invalid durable workspace identity: {error}"),
    }
}

fn option_default_agent_profile_id(options: &CodingAgentSessionOptions) -> ProfileId {
    options
        .default_agent_profile_id()
        .cloned()
        .unwrap_or_else(|| ProfileId::from("default"))
}

fn normalized_path_string(path: &Path) -> String {
    path.canonicalize()
        .unwrap_or_else(|_| path.to_path_buf())
        .to_string_lossy()
        .into_owned()
}
