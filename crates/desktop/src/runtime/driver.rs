use std::collections::{HashMap, VecDeque};
use std::sync::mpsc as std_mpsc;
use std::time::Duration;

use coding_agent::api::client::{
    CodingAgentClientConnection, CodingAgentClientId, CodingAgentControlId, CodingAgentDraftId,
    CodingAgentFreshSnapshotRecovery, CodingAgentReconnect, CodingAgentReconnectDelivery,
    CodingAgentReconnectReceiver, CodingAgentRecoveryReason, CodingAgentSnapshot,
    CodingAgentSubmissionDraft,
};
use coding_agent::api::embedding::{
    CodingAgentEmbeddingContext, CodingAgentEmbeddingOptions, CodingAgentSessionOpenTarget,
    CodingAgentThinkingLevel, CodingAgentThinkingLevelSanitization, CodingAgentWorkspaceScope,
    CodingAgentWorkspaceSelection, sanitize_thinking_level,
};
use coding_agent::api::event::{
    CodingAgentProductEvent, CodingAgentProductEventDeliveryClass, CodingAgentProductEventFamily,
    CodingAgentRecoveryResolution,
};
use coding_agent::api::operation::{CodingAgentOperation, CodingAgentOperationOutcome};
use coding_agent::api::review::{
    CodingAgentExternalEditorTarget, CodingAgentFileReview, CodingAgentFileReviewRequest,
};
use coding_agent::api::runtime::{
    CodingAgentRecoveryResolutionRequest, CodingAgentRecoveryRetryRequest, CodingAgentSession,
};
use coding_agent::api::view::{CodingAgentTranscriptSnapshot, ProfileId};
use futures::stream::{FuturesUnordered, StreamExt as _};
use tokio::sync::{mpsc, watch};
use tokio::task;

use crate::file_review::{DesktopExternalEditorConfig, launch_external_editor};

use super::dispatch::dispatch_command_with_updates;
use super::protocol::{
    DESKTOP_UPDATE_QUEUE_CAPACITY, DesktopBridgeError, DesktopRecoveryIdentity,
    DesktopRuntimeCommand, DesktopRuntimeError, DesktopRuntimeErrorSource,
    DesktopRuntimeHydratedSnapshot, DesktopRuntimeMetadataSnapshot, DesktopRuntimeReadySnapshot,
    DesktopRuntimeRecoverySnapshot, DesktopRuntimeUpdate, DesktopSessionCatalogEntry,
    MAX_CONCURRENT_DESKTOP_SESSIONS, MAX_DESKTOP_SESSION_CATALOG, MAX_SESSION_ID_BYTES,
    bounded_utf8_prefix, local_runtime_error, runtime_error,
};

const RUNTIME_SHUTDOWN_DEADLINE: Duration = Duration::from_secs(10);
pub(super) const DESKTOP_CLIENT_ID: &str = "evo-desktop";

pub(super) struct HomeRuntimeContext {
    pub(super) context: CodingAgentEmbeddingContext,
    options: CodingAgentEmbeddingOptions,
}

impl HomeRuntimeContext {
    pub(super) fn load(options: CodingAgentEmbeddingOptions) -> Result<Self, DesktopBridgeError> {
        let context = CodingAgentEmbeddingContext::load(options.clone())?;
        if context.snapshot().workspace.is_none() {
            return Err(DesktopBridgeError::Session {
                message: "desktop runtime requires typed workspace embedding options".into(),
            });
        }
        Ok(Self { context, options })
    }

    fn load_session_context(&self) -> Result<CodingAgentEmbeddingContext, DesktopBridgeError> {
        CodingAgentEmbeddingContext::load(self.options.clone()).map_err(DesktopBridgeError::from)
    }

    pub(super) fn select_model(
        &mut self,
        model_id: String,
        thinking_level: Option<CodingAgentThinkingLevel>,
    ) -> Result<(Option<CodingAgentThinkingLevel>, bool), DesktopBridgeError> {
        let thinking = admitted_model_thinking(&self.context, &model_id, thinking_level)?;
        self.context.select_model(model_id.clone())?;
        self.options = self.options.clone().with_model_id(model_id);
        Ok(thinking)
    }

    pub(super) fn select_profile(&mut self, profile_id: String) -> Result<(), DesktopBridgeError> {
        if !self
            .context
            .snapshot()
            .profiles
            .iter()
            .any(|profile| profile.id.as_str() == profile_id)
        {
            return Err(DesktopBridgeError::Input {
                message: format!("unknown desktop Home profile {profile_id}"),
            });
        }
        let options = self
            .options
            .clone()
            .with_default_agent_profile_id(profile_id);
        let context = CodingAgentEmbeddingContext::load(options.clone())?;
        self.options = options;
        self.context = context;
        Ok(())
    }
}

pub(super) fn admitted_model_thinking(
    context: &CodingAgentEmbeddingContext,
    model_id: &str,
    requested: Option<CodingAgentThinkingLevel>,
) -> Result<(Option<CodingAgentThinkingLevel>, bool), DesktopBridgeError> {
    let model = context
        .snapshot()
        .models
        .iter()
        .find(|model| model.id == model_id)
        .ok_or_else(|| DesktopBridgeError::Input {
            message: format!("unknown desktop model {model_id}"),
        })?;
    Ok(match requested {
        Some(requested) => match sanitize_thinking_level(model, requested) {
            CodingAgentThinkingLevelSanitization::Explicit(level) => (Some(level), false),
            CodingAgentThinkingLevelSanitization::AutoFallback => (None, true),
        },
        None => (None, false),
    })
}

pub(super) struct RuntimeSessionWorkspace {
    pub(super) scope: CodingAgentWorkspaceScope,
    pub(super) context: CodingAgentEmbeddingContext,
    pub(super) session: CodingAgentSession,
}

impl RuntimeSessionWorkspace {
    fn scope_for_context(
        context: &CodingAgentEmbeddingContext,
    ) -> Result<CodingAgentWorkspaceScope, DesktopBridgeError> {
        context
            .snapshot()
            .workspace
            .as_ref()
            .map(|workspace| workspace.scope.clone())
            .ok_or_else(|| DesktopBridgeError::Session {
                message: "desktop session context has no typed workspace scope".into(),
            })
    }

    fn new(
        context: CodingAgentEmbeddingContext,
        session: CodingAgentSession,
    ) -> Result<Self, DesktopBridgeError> {
        let scope = Self::scope_for_context(&context)?;
        Ok(Self {
            scope,
            context,
            session,
        })
    }
}

pub(super) struct NewPromptSession {
    pub(super) session_id: String,
    pub(super) snapshot: DesktopRuntimeHydratedSnapshot,
    pub(super) thinking_level: Option<CodingAgentThinkingLevel>,
}

pub(super) struct RuntimeState {
    pub(super) home: HomeRuntimeContext,
    pub(super) workspaces: HashMap<String, RuntimeSessionWorkspace>,
    pub(super) focused_session_id: Option<String>,
    #[cfg(test)]
    pub(super) fail_next_prompt_start: bool,
}

impl RuntimeState {
    pub(super) fn metadata_snapshot(
        &self,
        session_id: Option<&str>,
    ) -> DesktopRuntimeMetadataSnapshot {
        let session_id = session_id.or(self.focused_session_id.as_deref());
        match session_id.and_then(|session_id| self.workspaces.get(session_id)) {
            Some(workspace) => DesktopRuntimeMetadataSnapshot {
                project: workspace.context.snapshot().clone(),
                session: Some(workspace.session.snapshot()),
            },
            None => DesktopRuntimeMetadataSnapshot {
                project: self.home.context.snapshot().clone(),
                session: None,
            },
        }
    }

    pub(super) fn snapshot(
        &self,
        session_id: &str,
    ) -> Result<DesktopRuntimeHydratedSnapshot, DesktopBridgeError> {
        let workspace =
            self.workspaces
                .get(session_id)
                .ok_or_else(|| DesktopBridgeError::Session {
                    message: format!("desktop runtime has no idle owner for session {session_id}"),
                })?;
        Ok(DesktopRuntimeHydratedSnapshot {
            project: workspace.context.snapshot().clone(),
            session: workspace.session.snapshot(),
            transcript: workspace.session.transcript_snapshot()?,
            pending_recoveries: workspace.session.recovery_pending()?,
        })
    }

    pub(super) fn session_catalog(
        &self,
    ) -> Result<(Vec<DesktopSessionCatalogEntry>, usize), DesktopBridgeError> {
        let catalog = self.home.context.session_query()?.overviews()?;
        let omitted = catalog
            .overviews
            .len()
            .saturating_sub(MAX_DESKTOP_SESSION_CATALOG)
            + usize::from(catalog.truncated);
        let sessions = catalog
            .overviews
            .into_iter()
            .take(MAX_DESKTOP_SESSION_CATALOG)
            .map(|overview| DesktopSessionCatalogEntry {
                session_id: bounded_utf8_prefix(&overview.session_id, MAX_SESSION_ID_BYTES),
                name: overview.name.map(|name| bounded_utf8_prefix(&name, 256)),
                workspace: overview.workspace,
                workspace_migration: overview.workspace_migration,
                created_at: bounded_utf8_prefix(&overview.created_at, 128),
                updated_at: bounded_utf8_prefix(&overview.updated_at, 128),
                active_leaf_id: overview
                    .active_leaf_id
                    .map(|id| bounded_utf8_prefix(&id, 256)),
            })
            .collect();
        Ok((sessions, omitted))
    }

    fn recovery_snapshot(
        &self,
        session_id: &str,
    ) -> Result<DesktopRuntimeRecoverySnapshot, DesktopBridgeError> {
        let workspace =
            self.workspaces
                .get(session_id)
                .ok_or_else(|| DesktopBridgeError::Session {
                    message: format!("desktop runtime has no idle owner for session {session_id}"),
                })?;
        Ok(DesktopRuntimeRecoverySnapshot {
            project: workspace.context.snapshot().clone(),
            session: workspace.session.snapshot(),
            pending_recoveries: workspace.session.recovery_pending()?,
        })
    }

    pub(super) async fn review_changed_file(
        &self,
        session_id: &str,
        request: CodingAgentFileReviewRequest,
    ) -> Result<CodingAgentFileReview, DesktopBridgeError> {
        let workspace =
            self.workspaces
                .get(session_id)
                .ok_or_else(|| DesktopBridgeError::Session {
                    message: "desktop runtime has no idle session owner".into(),
                })?;
        workspace
            .session
            .review_changed_file(request)
            .await
            .map_err(DesktopBridgeError::from)
    }

    pub(super) async fn open_external_editor(
        &self,
        session_id: &str,
        target: CodingAgentExternalEditorTarget,
        editor: DesktopExternalEditorConfig,
    ) -> Result<String, DesktopBridgeError> {
        let workspace =
            self.workspaces
                .get(session_id)
                .ok_or_else(|| DesktopBridgeError::Session {
                    message: "desktop runtime has no idle session owner".into(),
                })?;
        workspace
            .session
            .revalidate_external_editor_target(&target)
            .await?;
        let project_relative_path = target.project_relative_path().to_owned();
        task::spawn_blocking(move || launch_external_editor(&editor, &target))
            .await
            .map_err(|_| DesktopBridgeError::ExternalEditor)??;
        Ok(project_relative_path)
    }

    pub(super) fn retry_recovery(
        &mut self,
        session_id: &str,
        identity: DesktopRecoveryIdentity,
    ) -> Result<(String, DesktopRuntimeRecoverySnapshot), DesktopBridgeError> {
        let session = &mut self
            .workspaces
            .get_mut(session_id)
            .ok_or_else(|| DesktopBridgeError::Session {
                message: "desktop runtime has no idle session owner".into(),
            })?
            .session;
        let result = session.retry_recovery(CodingAgentRecoveryRetryRequest {
            operation_id: identity.operation_id,
            recovery_id: identity.recovery_id,
            expected_record_version: identity.record_version,
            expected_descriptor_revision: identity.descriptor_revision,
            expected_capability_generation: identity.capability_generation,
            expected_attempt_count: identity.attempt_count,
            schedule_with_backoff: false,
        })?;
        let recovery_id = result.recovery_id;
        Ok((recovery_id, self.recovery_snapshot(session_id)?))
    }

    pub(super) fn resolve_recovery(
        &mut self,
        session_id: &str,
        identity: DesktopRecoveryIdentity,
        resolution: CodingAgentRecoveryResolution,
    ) -> Result<(String, DesktopRuntimeRecoverySnapshot), DesktopBridgeError> {
        let action = match resolution {
            CodingAgentRecoveryResolution::Failed => "marked failed",
            CodingAgentRecoveryResolution::Aborted => "aborted",
        };
        let session = &mut self
            .workspaces
            .get_mut(session_id)
            .ok_or_else(|| DesktopBridgeError::Session {
                message: "desktop runtime has no idle session owner".into(),
            })?
            .session;
        let result = session.resolve_recovery(CodingAgentRecoveryResolutionRequest {
            operation_id: identity.operation_id,
            recovery_id: identity.recovery_id,
            expected_record_version: identity.record_version,
            expected_descriptor_revision: identity.descriptor_revision,
            expected_capability_generation: identity.capability_generation,
            expected_attempt_count: identity.attempt_count,
            resolution,
            reason: format!("native desktop operator {action} uncertain operation"),
        })?;
        let recovery_id = result.recovery_id;
        Ok((recovery_id, self.recovery_snapshot(session_id)?))
    }

    pub(super) async fn select_session_profile(
        &mut self,
        session_id: &str,
        profile_id: String,
    ) -> Result<DesktopRuntimeMetadataSnapshot, DesktopBridgeError> {
        let workspace =
            self.workspaces
                .get_mut(session_id)
                .ok_or_else(|| DesktopBridgeError::Busy {
                    operation: "desktop_profile_selection".into(),
                })?;
        if !workspace
            .context
            .snapshot()
            .profiles
            .iter()
            .any(|profile| profile.id.as_str() == profile_id)
        {
            return Err(DesktopBridgeError::Input {
                message: format!("unknown desktop session profile {profile_id}"),
            });
        }
        let profile_id =
            ProfileId::new(profile_id).map_err(|message| DesktopBridgeError::Input {
                message: format!("invalid desktop session profile: {message}"),
            })?;
        let outcome = workspace
            .session
            .run(CodingAgentOperation::SetDefaultAgentProfile { profile_id })
            .await?;
        if !matches!(
            outcome,
            CodingAgentOperationOutcome::DefaultAgentProfileChanged
        ) {
            return Err(DesktopBridgeError::Session {
                message: "desktop profile selection returned an unexpected outcome".into(),
            });
        }
        Ok(self.metadata_snapshot(Some(session_id)))
    }

    pub(super) async fn rename_session(
        &mut self,
        session_id: &str,
        name: Option<String>,
    ) -> Result<(Option<String>, String), DesktopBridgeError> {
        let session = &mut self
            .workspaces
            .get_mut(session_id)
            .ok_or_else(|| DesktopBridgeError::Busy {
                operation: "desktop_session_rename".into(),
            })?
            .session;
        let outcome = session
            .run(CodingAgentOperation::SetSessionName { name })
            .await?;
        let CodingAgentOperationOutcome::SessionNameChanged { name, updated_at } = outcome else {
            return Err(DesktopBridgeError::Session {
                message: "desktop session rename returned an unexpected outcome".into(),
            });
        };
        Ok((name, updated_at))
    }

    pub(super) async fn create_session(
        &mut self,
        open_session_count: usize,
    ) -> Result<String, DesktopBridgeError> {
        self.ensure_capacity(open_session_count)?;
        let context = self.home.load_session_context()?;
        self.create_session_in_context(context)
            .await
            .map(|(session_id, _)| session_id)
    }

    pub(super) async fn create_session_for_workspace(
        &mut self,
        workspace: CodingAgentWorkspaceSelection,
        model_id: String,
        profile_id: String,
        thinking_level: Option<CodingAgentThinkingLevel>,
        open_session_count: usize,
    ) -> Result<NewPromptSession, DesktopBridgeError> {
        self.ensure_capacity(open_session_count)?;
        let mut options = CodingAgentEmbeddingOptions::for_workspace(workspace)
            .map_err(|error| DesktopBridgeError::Input {
                message: format!("desktop workspace could not be resolved: {error}"),
            })?
            .with_model_id(model_id)
            .with_default_agent_profile_id(profile_id);
        if let Some(session_root) = self.home.context.snapshot().settings.session_dir.as_ref() {
            options = options.with_session_dir(session_root);
        }
        let context = CodingAgentEmbeddingContext::load(options)?;
        let selected_model_id = context.snapshot().selected_model_id.clone();
        let (thinking_level, _) =
            admitted_model_thinking(&context, &selected_model_id, thinking_level)?;
        let (session_id, snapshot) = self.create_session_in_context(context).await?;
        Ok(NewPromptSession {
            session_id,
            snapshot,
            thinking_level,
        })
    }

    async fn create_session_in_context(
        &mut self,
        context: CodingAgentEmbeddingContext,
    ) -> Result<(String, DesktopRuntimeHydratedSnapshot), DesktopBridgeError> {
        let scope = RuntimeSessionWorkspace::scope_for_context(&context)?;
        let project = context.snapshot().clone();
        let session = context.create_session().await?;
        let session_id = session.view().session_id.clone();
        let snapshot = DesktopRuntimeHydratedSnapshot {
            project,
            session: session.snapshot(),
            transcript: CodingAgentTranscriptSnapshot {
                session_id: session_id.clone(),
                active_leaf_id: None,
                items: Vec::new(),
            },
            pending_recoveries: Vec::new(),
        };
        let workspace = RuntimeSessionWorkspace {
            scope,
            context,
            session,
        };
        self.workspaces.insert(session_id.clone(), workspace);
        self.focused_session_id = Some(session_id.clone());
        Ok((session_id, snapshot))
    }

    pub(super) async fn open_session(
        &mut self,
        session_id: String,
        open_session_count: usize,
    ) -> Result<String, DesktopBridgeError> {
        if self.workspaces.contains_key(&session_id) {
            self.focused_session_id = Some(session_id.clone());
            return Ok(session_id);
        }
        self.ensure_capacity(open_session_count)?;
        let target = self
            .home
            .context
            .session_query()?
            .open_target(&session_id)?;
        let context = self.context_for_open_target(&target)?;
        let session = context.open_session(target.session_id.clone()).await?;
        let workspace = RuntimeSessionWorkspace::new(context, session)?;
        self.workspaces.insert(target.session_id.clone(), workspace);
        self.focused_session_id = Some(target.session_id.clone());
        Ok(target.session_id)
    }

    fn context_for_open_target(
        &self,
        target: &CodingAgentSessionOpenTarget,
    ) -> Result<CodingAgentEmbeddingContext, DesktopBridgeError> {
        let selection = match &target.workspace_scope {
            CodingAgentWorkspaceScope::Project { cwd } => {
                CodingAgentWorkspaceSelection::project(cwd)
            }
            CodingAgentWorkspaceScope::Projectless { workspace_id } => {
                CodingAgentWorkspaceSelection::projectless(workspace_id)
            }
            CodingAgentWorkspaceScope::Legacy { .. } => {
                return Err(DesktopBridgeError::WorkspaceUnavailable {
                    message: target
                        .workspace_migration
                        .diagnostic
                        .clone()
                        .unwrap_or_else(|| "Legacy session workspace is unavailable.".into()),
                });
            }
        };
        let unavailable = target
            .workspace_migration
            .diagnostic
            .clone()
            .unwrap_or_else(|| "Session workspace is unavailable.".into());
        let mut options = CodingAgentEmbeddingOptions::for_workspace(selection).map_err(|_| {
            DesktopBridgeError::WorkspaceUnavailable {
                message: unavailable,
            }
        })?;
        if let Some(session_root) = self.home.context.snapshot().settings.session_dir.as_ref() {
            options = options.with_session_dir(session_root);
        }
        CodingAgentEmbeddingContext::load(options).map_err(DesktopBridgeError::from)
    }

    pub(super) fn start_prompt(
        &mut self,
        session_id: &str,
        command_id: u64,
        prompt: String,
        attachments: Vec<std::path::PathBuf>,
        thinking_level: Option<CodingAgentThinkingLevel>,
    ) -> Result<ActivePrompt, DesktopBridgeError> {
        let workspace =
            self.workspaces
                .get(session_id)
                .ok_or_else(|| DesktopBridgeError::Busy {
                    operation: "desktop_prompt".into(),
                })?;
        let selected_model_id = workspace.context.snapshot().selected_model_id.clone();
        let (thinking_level, _) =
            admitted_model_thinking(&workspace.context, &selected_model_id, thinking_level)?;
        let prepared = workspace
            .context
            .prepare_prompt_with_attachments(&prompt, &attachments)?;
        let display_text = prepared.display_text().to_owned();
        let operation = workspace
            .context
            .prepared_prompt_operation(prepared, thinking_level);
        #[cfg(test)]
        if std::mem::take(&mut self.fail_next_prompt_start) {
            return Err(DesktopBridgeError::Session {
                message: "injected desktop prompt start failure".into(),
            });
        }
        let RuntimeSessionWorkspace {
            scope,
            context,
            mut session,
        } = self
            .workspaces
            .remove(session_id)
            .expect("validated idle workspace must remain present");
        let connection = match session.connect(CodingAgentClientId::new(DESKTOP_CLIENT_ID)) {
            Ok(connection) => connection,
            Err(error) => {
                self.workspaces.insert(
                    session_id.to_owned(),
                    RuntimeSessionWorkspace {
                        scope,
                        context,
                        session,
                    },
                );
                return Err(error.into());
            }
        };
        let draft_id = CodingAgentDraftId(format!("desktop-prompt-{command_id}"));
        let submission = match connection.prepare_client_submission(
            &mut session,
            Some(CodingAgentSubmissionDraft::new(draft_id, display_text)),
            operation,
        ) {
            Ok(submission) => submission,
            Err(error) => {
                let _ = connection.detach();
                self.workspaces.insert(
                    session_id.to_owned(),
                    RuntimeSessionWorkspace {
                        scope,
                        context,
                        session,
                    },
                );
                return Err(error.into());
            }
        };
        let requested_after = match connection.state() {
            Ok(snapshot) => snapshot.cursor.last_event_sequence,
            Err(error) => {
                let cleanup = submission.discard(&mut session);
                let _ = connection.detach();
                self.workspaces.insert(
                    session_id.to_owned(),
                    RuntimeSessionWorkspace {
                        scope,
                        context,
                        session,
                    },
                );
                if let Err(cleanup) = cleanup {
                    return Err(cleanup.into());
                }
                return Err(error.into());
            }
        };
        let (events, pending_recovery) = match reconnect_event_source(&connection, requested_after)
        {
            Ok(reconnect) => reconnect,
            Err(error) => {
                let cleanup = submission.discard(&mut session);
                let _ = connection.detach();
                self.workspaces.insert(
                    session_id.to_owned(),
                    RuntimeSessionWorkspace {
                        scope,
                        context,
                        session,
                    },
                );
                if let Err(cleanup) = cleanup {
                    return Err(cleanup.into());
                }
                return Err(error);
            }
        };
        let task = task::spawn(async move {
            let result = submission
                .run(&mut session)
                .await
                .map_err(DesktopBridgeError::from);
            (session, result)
        });
        Ok(ActivePrompt {
            session_id: session_id.to_owned(),
            command_id,
            operation_id: None,
            scope,
            context,
            connection,
            events,
            pending_recovery,
            last_forwarded_sequence: requested_after,
            task,
        })
    }

    pub(super) fn insert_idle_workspace(
        &mut self,
        scope: CodingAgentWorkspaceScope,
        context: CodingAgentEmbeddingContext,
        session: CodingAgentSession,
    ) {
        self.workspaces.insert(
            session.view().session_id.clone(),
            RuntimeSessionWorkspace {
                scope,
                context,
                session,
            },
        );
    }

    pub(super) async fn close_idle_session(
        &mut self,
        session_id: &str,
    ) -> Result<(), DesktopBridgeError> {
        let mut workspace = self.workspaces.remove(session_id).ok_or_else(|| {
            DesktopBridgeError::SessionTarget {
                message: format!("session {session_id} is not open"),
            }
        })?;
        workspace.session.shutdown().await?;
        Ok(())
    }

    async fn shutdown_idle_sessions(&mut self) -> Result<(), DesktopBridgeError> {
        let mut session_ids = self.workspaces.keys().cloned().collect::<Vec<_>>();
        session_ids.sort();
        for session_id in session_ids {
            self.close_idle_session(&session_id).await?;
        }
        Ok(())
    }

    fn ensure_capacity(&self, open_session_count: usize) -> Result<(), DesktopBridgeError> {
        if open_session_count >= MAX_CONCURRENT_DESKTOP_SESSIONS {
            return Err(DesktopBridgeError::SessionLimit {
                limit: MAX_CONCURRENT_DESKTOP_SESSIONS,
            });
        }
        Ok(())
    }
}

pub(super) type PromptTaskOutput = (
    CodingAgentSession,
    Result<CodingAgentOperationOutcome, DesktopBridgeError>,
);

pub(super) struct ActivePrompt {
    pub(super) session_id: String,
    pub(super) command_id: u64,
    pub(super) operation_id: Option<String>,
    pub(super) scope: CodingAgentWorkspaceScope,
    pub(super) context: CodingAgentEmbeddingContext,
    pub(super) connection: CodingAgentClientConnection,
    pub(super) events: DesktopProductEventSource,
    pub(super) pending_recovery: Option<CodingAgentFreshSnapshotRecovery>,
    pub(super) last_forwarded_sequence: u64,
    pub(super) task: task::JoinHandle<PromptTaskOutput>,
}

enum ActivePromptSignal {
    Event(Box<Result<CodingAgentReconnectDelivery, DesktopBridgeError>>),
    Finished(Box<Result<PromptTaskOutput, task::JoinError>>),
}

enum RuntimeSignal {
    Command(Option<DesktopRuntimeCommand>),
    Active {
        session_id: String,
        signal: ActivePromptSignal,
    },
    Shutdown,
}

pub(super) async fn run_runtime(
    options: CodingAgentEmbeddingOptions,
    mut commands: mpsc::Receiver<DesktopRuntimeCommand>,
    mut shutdown: watch::Receiver<bool>,
    priority_updates: mpsc::Sender<DesktopRuntimeUpdate>,
    data_updates: mpsc::Sender<DesktopRuntimeUpdate>,
    ready: std_mpsc::SyncSender<Result<DesktopRuntimeReadySnapshot, DesktopRuntimeError>>,
) {
    let home = match HomeRuntimeContext::load(options) {
        Ok(home) => home,
        Err(error) => {
            let _ = ready.send(Err(runtime_error(&error)));
            return;
        }
    };
    let mut state = RuntimeState {
        home,
        workspaces: HashMap::new(),
        focused_session_id: None,
        #[cfg(test)]
        fail_next_prompt_start: false,
    };
    let initial = DesktopRuntimeReadySnapshot {
        project: state.home.context.snapshot().clone(),
    };
    if ready.send(Ok(initial)).is_err() {
        let _ = state.shutdown_idle_sessions().await;
        return;
    }

    let mut active = HashMap::<String, ActivePrompt>::new();
    loop {
        let mut active_ids = active.keys().cloned().collect::<Vec<_>>();
        active_ids.sort();
        for session_id in active_ids {
            let active_prompt = active
                .get_mut(&session_id)
                .expect("collected active session must remain present");
            if let Some(recovery) = active_prompt.pending_recovery.take() {
                active_prompt.last_forwarded_sequence = recovery.fresh_cursor.last_event_sequence;
                if priority_updates
                    .send(recovery_update(recovery))
                    .await
                    .is_err()
                {
                    shutdown_all_active_prompts(&mut active, &priority_updates).await;
                    return;
                }
            }
        }

        let signal = if active.is_empty() {
            tokio::select! {
                biased;
                changed = shutdown.changed() => {
                    let _ = changed;
                    RuntimeSignal::Shutdown
                }
                command = commands.recv() => RuntimeSignal::Command(command),
            }
        } else {
            let next_active = next_active_signal(&mut active);
            tokio::pin!(next_active);
            tokio::select! {
                biased;
                changed = shutdown.changed() => {
                    let _ = changed;
                    RuntimeSignal::Shutdown
                }
                command = commands.recv() => RuntimeSignal::Command(command),
                active_signal = &mut next_active => {
                    let (session_id, signal) = active_signal
                        .expect("non-empty active prompt set must produce a signal");
                    RuntimeSignal::Active { session_id, signal }
                }
            }
        };
        match signal {
            RuntimeSignal::Shutdown | RuntimeSignal::Command(None) => break,
            RuntimeSignal::Command(Some(command)) => {
                let update = dispatch_command_with_updates(
                    &mut state,
                    &mut active,
                    Some(&priority_updates),
                    Some(&data_updates),
                    command,
                )
                .await;
                if priority_updates.send(update).await.is_err() {
                    break;
                }
            }
            RuntimeSignal::Active { session_id, signal } => match signal {
                ActivePromptSignal::Event(event) => match *event {
                    Ok(CodingAgentReconnectDelivery::Event(event)) => {
                        let active_prompt = active
                            .get_mut(&session_id)
                            .expect("signaled active prompt must remain present");
                        let sequence = event.sequence();
                        let candidate_operation_id = event.operation_id().map(str::to_owned);
                        if !ensure_operation_started(
                            active_prompt,
                            candidate_operation_id.as_deref(),
                            &priority_updates,
                        )
                        .await
                        {
                            shutdown_active_prompt(active.remove(&session_id), &priority_updates)
                                .await;
                            continue;
                        }
                        if !publish_product_event(
                            event,
                            active_prompt,
                            &priority_updates,
                            &data_updates,
                        )
                        .await
                        {
                            shutdown_active_prompt(active.remove(&session_id), &priority_updates)
                                .await;
                            continue;
                        }
                        if !acknowledge_product_event(active_prompt, sequence, &priority_updates)
                            .await
                        {
                            shutdown_active_prompt(active.remove(&session_id), &priority_updates)
                                .await;
                            continue;
                        }
                        active_prompt.last_forwarded_sequence = sequence;
                    }
                    Ok(CodingAgentReconnectDelivery::FreshSnapshotRequired(recovery)) => {
                        let active_prompt = active
                            .get_mut(&session_id)
                            .expect("signaled active prompt must remain present");
                        active_prompt.last_forwarded_sequence =
                            recovery.fresh_cursor.last_event_sequence;
                        if priority_updates
                            .send(recovery_update(recovery))
                            .await
                            .is_err()
                        {
                            shutdown_active_prompt(active.remove(&session_id), &priority_updates)
                                .await;
                            continue;
                        }
                    }
                    Err(error) => {
                        let active_prompt = active
                            .get_mut(&session_id)
                            .expect("signaled active prompt must remain present");
                        if !recover_product_event_source(active_prompt, error, &priority_updates)
                            .await
                        {
                            shutdown_active_prompt(active.remove(&session_id), &priority_updates)
                                .await;
                        }
                    }
                },
                ActivePromptSignal::Finished(result) => {
                    let result = *result;
                    let mut completed = active
                        .remove(&session_id)
                        .expect("signaled active prompt must remain present");
                    if !drain_product_events(&mut completed, &priority_updates, &data_updates).await
                    {
                        shutdown_active_prompt(Some(completed), &priority_updates).await;
                        continue;
                    }
                    let _ = completed.connection.detach();
                    match result {
                        Ok((session, operation_result)) => {
                            let operation_started =
                                ensure_operation_started(&mut completed, None, &priority_updates)
                                    .await;
                            let operation_id = completed.operation_id.take();
                            let command_id = completed.command_id;
                            state.insert_idle_workspace(
                                completed.scope,
                                completed.context,
                                session,
                            );
                            if !operation_started {
                                continue;
                            }
                            let Some(operation_id) = operation_id else {
                                let _ = priority_updates
                                    .send(DesktopRuntimeUpdate::RuntimeFailed {
                                        error: DesktopRuntimeError {
                                            code: "operation_association_missing".into(),
                                            message: "completed desktop prompt has no product operation id"
                                                .into(),
                                        },
                                    })
                                    .await;
                                continue;
                            };
                            let snapshot = match state.snapshot(&session_id) {
                                Ok(snapshot) => snapshot,
                                Err(error) => {
                                    let _ = priority_updates
                                        .send(DesktopRuntimeUpdate::RuntimeFailed {
                                            error: runtime_error(&error),
                                        })
                                        .await;
                                    continue;
                                }
                            };
                            let error = operation_result.err().map(|error| runtime_error(&error));
                            if priority_updates
                                .send(DesktopRuntimeUpdate::PromptFinished {
                                    command_id,
                                    operation_id,
                                    snapshot,
                                    error,
                                })
                                .await
                                .is_err()
                            {
                                continue;
                            }
                        }
                        Err(_) => {
                            let _ = priority_updates
                                .send(DesktopRuntimeUpdate::RuntimeFailed {
                                    error: local_runtime_error(
                                        "runtime_task_panicked",
                                        "A desktop runtime task stopped unexpectedly.",
                                    ),
                                })
                                .await;
                            continue;
                        }
                    }
                }
            },
        }
    }

    shutdown_all_active_prompts(&mut active, &priority_updates).await;
    let _ = state.shutdown_idle_sessions().await;
    let _ = priority_updates.send(DesktopRuntimeUpdate::Stopped).await;
}

async fn next_active_signal(
    active: &mut HashMap<String, ActivePrompt>,
) -> Option<(String, ActivePromptSignal)> {
    let pending = active
        .iter_mut()
        .map(|(session_id, prompt)| {
            let session_id = session_id.clone();
            async move {
                let signal = tokio::select! {
                    biased;
                    result = &mut prompt.task => {
                        ActivePromptSignal::Finished(Box::new(result))
                    }
                    event = recv_product_event(&mut prompt.events) => {
                        ActivePromptSignal::Event(Box::new(event))
                    }
                };
                (session_id, signal)
            }
        })
        .collect::<FuturesUnordered<_>>();
    pending.into_future().await.0
}

async fn shutdown_all_active_prompts(
    active: &mut HashMap<String, ActivePrompt>,
    priority_updates: &mpsc::Sender<DesktopRuntimeUpdate>,
) {
    let mut session_ids = active.keys().cloned().collect::<Vec<_>>();
    session_ids.sort();
    for session_id in session_ids {
        shutdown_active_prompt(active.remove(&session_id), priority_updates).await;
    }
}

async fn recv_product_event(
    receiver: &mut DesktopProductEventSource,
) -> Result<CodingAgentReconnectDelivery, DesktopBridgeError> {
    receiver.recv().await
}

pub(super) struct DesktopProductEventSource {
    pub(super) replay: VecDeque<CodingAgentProductEvent>,
    pub(super) receiver: DesktopProductEventReceiver,
}

pub(super) enum DesktopProductEventReceiver {
    Product(CodingAgentReconnectReceiver),
    #[cfg(test)]
    Injected(mpsc::Receiver<Result<CodingAgentReconnectDelivery, DesktopBridgeError>>),
}

impl DesktopProductEventReceiver {
    async fn recv(&mut self) -> Result<CodingAgentReconnectDelivery, DesktopBridgeError> {
        match self {
            Self::Product(receiver) => receiver.recv().await.map_err(DesktopBridgeError::from),
            #[cfg(test)]
            Self::Injected(receiver) => receiver
                .recv()
                .await
                .unwrap_or_else(|| Err(DesktopBridgeError::cancelled_for_tests())),
        }
    }

    fn try_recv(&mut self) -> Result<Option<CodingAgentReconnectDelivery>, DesktopBridgeError> {
        match self {
            Self::Product(receiver) => receiver.try_recv().map_err(DesktopBridgeError::from),
            #[cfg(test)]
            Self::Injected(receiver) => match receiver.try_recv() {
                Ok(delivery) => delivery.map(Some),
                Err(mpsc::error::TryRecvError::Empty) => Ok(None),
                Err(mpsc::error::TryRecvError::Disconnected) => {
                    Err(DesktopBridgeError::cancelled_for_tests())
                }
            },
        }
    }
}

impl DesktopProductEventSource {
    pub(super) async fn recv(
        &mut self,
    ) -> Result<CodingAgentReconnectDelivery, DesktopBridgeError> {
        if let Some(event) = self.replay.pop_front() {
            return Ok(CodingAgentReconnectDelivery::Event(event));
        }
        self.receiver.recv().await
    }

    fn try_recv(&mut self) -> Result<Option<CodingAgentReconnectDelivery>, DesktopBridgeError> {
        if let Some(event) = self.replay.pop_front() {
            return Ok(Some(CodingAgentReconnectDelivery::Event(event)));
        }
        self.receiver.try_recv()
    }
}

pub(super) enum DesktopReconnectAttempt<R> {
    Replayed {
        events: Vec<CodingAgentProductEvent>,
        receiver: R,
    },
    FreshSnapshotRequired(CodingAgentFreshSnapshotRecovery),
}

pub(super) fn establish_reconnect<R>(
    requested_after: u64,
    mut reconnect: impl FnMut(u64) -> Result<DesktopReconnectAttempt<R>, DesktopBridgeError>,
) -> Result<
    (
        Vec<CodingAgentProductEvent>,
        R,
        Option<CodingAgentFreshSnapshotRecovery>,
    ),
    DesktopBridgeError,
> {
    match reconnect(requested_after)? {
        DesktopReconnectAttempt::Replayed { events, receiver } => Ok((events, receiver, None)),
        DesktopReconnectAttempt::FreshSnapshotRequired(recovery) => {
            let fresh_sequence = recovery.fresh_cursor.last_event_sequence;
            match reconnect(fresh_sequence)? {
                DesktopReconnectAttempt::Replayed { events, receiver } => {
                    Ok((events, receiver, Some(recovery)))
                }
                DesktopReconnectAttempt::FreshSnapshotRequired(second) => {
                    Err(DesktopBridgeError::Input {
                        message: format!(
                            "desktop ProductEvent reconnect exhausted after fresh cursor {} \
                             (oldest retained sequence {})",
                            second.requested_sequence, second.oldest_available_sequence
                        ),
                    })
                }
            }
        }
    }
}

pub(super) fn reconnect_event_source(
    connection: &CodingAgentClientConnection,
    requested_after: u64,
) -> Result<
    (
        DesktopProductEventSource,
        Option<CodingAgentFreshSnapshotRecovery>,
    ),
    DesktopBridgeError,
> {
    let (events, receiver, recovery) = establish_reconnect(requested_after, |sequence| {
        connection
            .reconnect(sequence)
            .map(|reconnect| match reconnect {
                CodingAgentReconnect::Replayed {
                    events, receiver, ..
                } => DesktopReconnectAttempt::Replayed { events, receiver },
                CodingAgentReconnect::FreshSnapshotRequired(recovery) => {
                    DesktopReconnectAttempt::FreshSnapshotRequired(recovery)
                }
            })
            .map_err(DesktopBridgeError::from)
    })?;
    Ok((
        DesktopProductEventSource {
            replay: events.into(),
            receiver: DesktopProductEventReceiver::Product(receiver),
        },
        recovery,
    ))
}

async fn recover_product_event_source(
    active: &mut ActivePrompt,
    receiver_error: DesktopBridgeError,
    priority_updates: &mpsc::Sender<DesktopRuntimeUpdate>,
) -> bool {
    match reconnect_event_source(&active.connection, active.last_forwarded_sequence) {
        Ok((events, recovery)) => {
            active.events = events;
            if let Some(recovery) = recovery {
                active.last_forwarded_sequence = recovery.fresh_cursor.last_event_sequence;
                priority_updates
                    .send(recovery_update(recovery))
                    .await
                    .is_ok()
            } else {
                true
            }
        }
        Err(reconnect_error) => priority_updates
            .send(DesktopRuntimeUpdate::RuntimeFailed {
                error: DesktopRuntimeError {
                    code: "product_event_reconnect_failed".into(),
                    message: format!(
                        "ProductEvent receiver failed ({}); reconnect from sequence {} failed: {}",
                        receiver_error, active.last_forwarded_sequence, reconnect_error
                    ),
                },
            })
            .await
            .is_ok(),
    }
}

pub(super) fn recovery_update(recovery: CodingAgentFreshSnapshotRecovery) -> DesktopRuntimeUpdate {
    let reason = match recovery.reason {
        CodingAgentRecoveryReason::RetainedHistoryGap => DesktopRuntimeError {
            code: "product_event_retained_history_gap".into(),
            message: format!(
                "ProductEvent replay after sequence {} is unavailable; oldest retained sequence is {}",
                recovery.requested_sequence, recovery.oldest_available_sequence
            ),
        },
        CodingAgentRecoveryReason::LiveReceiverLag => DesktopRuntimeError {
            code: "product_event_live_receiver_lag".into(),
            message: format!(
                "ProductEvent receiver lagged after sequence {}; recovered at fresh sequence {}",
                recovery.requested_sequence, recovery.fresh_cursor.last_event_sequence
            ),
        },
    };
    DesktopRuntimeUpdate::ResyncRequired {
        reason,
        snapshot: *recovery.snapshot,
    }
}

async fn publish_product_event(
    event: CodingAgentProductEvent,
    active: &ActivePrompt,
    priority_updates: &mpsc::Sender<DesktopRuntimeUpdate>,
    data_updates: &mpsc::Sender<DesktopRuntimeUpdate>,
) -> bool {
    if event.family() == CodingAgentProductEventFamily::Capability {
        let snapshot = match active.connection.state() {
            Ok(snapshot) => snapshot,
            Err(error) => {
                return priority_updates
                    .send(DesktopRuntimeUpdate::RuntimeFailed {
                        error: runtime_error(&error),
                    })
                    .await
                    .is_ok();
            }
        };
        return priority_updates
            .send(DesktopRuntimeUpdate::ResyncRequired {
                reason: DesktopRuntimeError {
                    code: "capability_generation_changed".into(),
                    message: format!(
                        "capability generation changed at ProductEvent sequence {}; replacing the desktop projection atomically",
                        event.sequence()
                    ),
                },
                snapshot,
            })
            .await
            .is_ok();
    }
    if is_priority_event(&event) {
        return priority_updates
            .send(DesktopRuntimeUpdate::ProductEvent {
                session_id: active.session_id.clone(),
                event,
            })
            .await
            .is_ok();
    }
    publish_data_update(
        DesktopRuntimeUpdate::ProductEvent {
            session_id: active.session_id.clone(),
            event,
        },
        || active.connection.state(),
        priority_updates,
        data_updates,
    )
    .await
}

async fn acknowledge_product_event(
    active: &ActivePrompt,
    sequence: u64,
    priority_updates: &mpsc::Sender<DesktopRuntimeUpdate>,
) -> bool {
    match active.connection.acknowledge(sequence) {
        Ok(_) => true,
        Err(error) => priority_updates
            .send(DesktopRuntimeUpdate::RuntimeFailed {
                error: runtime_error(&error),
            })
            .await
            .is_ok(),
    }
}

pub(super) async fn publish_data_update<E>(
    update: DesktopRuntimeUpdate,
    snapshot: impl FnOnce() -> Result<CodingAgentSnapshot, E>,
    priority_updates: &mpsc::Sender<DesktopRuntimeUpdate>,
    data_updates: &mpsc::Sender<DesktopRuntimeUpdate>,
) -> bool
where
    E: DesktopRuntimeErrorSource,
{
    match data_updates.try_send(update) {
        Ok(()) => true,
        Err(mpsc::error::TrySendError::Closed(_)) => false,
        Err(mpsc::error::TrySendError::Full(_)) => {
            let snapshot = match snapshot() {
                Ok(snapshot) => snapshot,
                Err(error) => {
                    return priority_updates
                        .send(DesktopRuntimeUpdate::RuntimeFailed {
                            error: runtime_error(&error),
                        })
                        .await
                        .is_ok();
                }
            };
            priority_updates
                .send(DesktopRuntimeUpdate::ResyncRequired {
                    reason: DesktopRuntimeError {
                        code: "desktop_data_queue_full".into(),
                        message: format!(
                            "desktop message update queue reached its {}-event bound",
                            DESKTOP_UPDATE_QUEUE_CAPACITY
                        ),
                    },
                    snapshot,
                })
                .await
                .is_ok()
        }
    }
}

async fn ensure_operation_started(
    active: &mut ActivePrompt,
    candidate_operation_id: Option<&str>,
    priority_updates: &mpsc::Sender<DesktopRuntimeUpdate>,
) -> bool {
    if active.operation_id.is_some() {
        return true;
    }
    let snapshot = match active.connection.state() {
        Ok(snapshot) => snapshot,
        Err(error) => {
            let _ = priority_updates
                .send(DesktopRuntimeUpdate::RuntimeFailed {
                    error: runtime_error(&error),
                })
                .await;
            return false;
        }
    };
    let operation_id = snapshot
        .submitted_operation
        .as_ref()
        .map(|operation| operation.operation_id.clone())
        .or_else(|| candidate_operation_id.map(str::to_owned));
    let Some(operation_id) = operation_id else {
        return true;
    };
    active.operation_id = Some(operation_id.clone());
    priority_updates
        .send(DesktopRuntimeUpdate::PromptStarted {
            command_id: active.command_id,
            operation_id,
            metadata: DesktopRuntimeMetadataSnapshot {
                project: active.context.snapshot().clone(),
                session: Some(snapshot),
            },
        })
        .await
        .is_ok()
}

async fn drain_product_events(
    active: &mut ActivePrompt,
    priority_updates: &mpsc::Sender<DesktopRuntimeUpdate>,
    data_updates: &mpsc::Sender<DesktopRuntimeUpdate>,
) -> bool {
    loop {
        let received = active.events.try_recv();
        match received {
            Ok(Some(CodingAgentReconnectDelivery::Event(event))) => {
                let sequence = event.sequence();
                let candidate_operation_id = event.operation_id().map(str::to_owned);
                if !ensure_operation_started(
                    active,
                    candidate_operation_id.as_deref(),
                    priority_updates,
                )
                .await
                {
                    return false;
                }
                if !publish_product_event(event, active, priority_updates, data_updates).await {
                    return false;
                }
                if !acknowledge_product_event(active, sequence, priority_updates).await {
                    return false;
                }
                active.last_forwarded_sequence = sequence;
            }
            Ok(Some(CodingAgentReconnectDelivery::FreshSnapshotRequired(recovery))) => {
                active.last_forwarded_sequence = recovery.fresh_cursor.last_event_sequence;
                if priority_updates
                    .send(recovery_update(recovery))
                    .await
                    .is_err()
                {
                    return false;
                }
            }
            Ok(None) => return true,
            Err(error) => {
                return recover_product_event_source(active, error, priority_updates).await;
            }
        }
    }
}

fn is_priority_event(event: &CodingAgentProductEvent) -> bool {
    !matches!(
        (event.delivery_class(), event.family()),
        (
            CodingAgentProductEventDeliveryClass::Data,
            CodingAgentProductEventFamily::Message
        )
    )
}

pub(super) async fn close_active_prompt(
    mut active: ActivePrompt,
    priority_updates: &mpsc::Sender<DesktopRuntimeUpdate>,
    data_updates: &mpsc::Sender<DesktopRuntimeUpdate>,
) -> Result<(), DesktopBridgeError> {
    let operation_id = active.operation_id.clone().or_else(|| {
        active
            .connection
            .state()
            .ok()
            .and_then(|snapshot| snapshot.submitted_operation)
            .map(|operation| operation.operation_id)
    });
    if let Some(operation_id) = operation_id.as_deref() {
        let control = active.connection.prompt_control(operation_id);
        let _ = control.abort(
            CodingAgentControlId("desktop-session-close".into()),
            "desktop session close",
        );
    }
    match tokio::time::timeout(RUNTIME_SHUTDOWN_DEADLINE, &mut active.task).await {
        Ok(Ok((mut session, _))) => {
            if !drain_product_events(&mut active, priority_updates, data_updates).await {
                let _ = active.connection.detach();
                let _ = session.shutdown().await;
                return Err(DesktopBridgeError::Session {
                    message: "desktop session close could not drain terminal ProductEvents".into(),
                });
            }
            let _ = active.connection.detach();
            session.shutdown().await?;
            Ok(())
        }
        Ok(Err(_)) => {
            let _ = active.connection.detach();
            Err(DesktopBridgeError::Session {
                message: "desktop session prompt task stopped unexpectedly".into(),
            })
        }
        Err(_) => {
            active.task.abort();
            let _ = active.task.await;
            let _ = active.connection.detach();
            Err(DesktopBridgeError::Session {
                message: format!(
                    "prompt operation {} did not stop within {} seconds",
                    operation_id.as_deref().unwrap_or("<starting>"),
                    RUNTIME_SHUTDOWN_DEADLINE.as_secs_f64()
                ),
            })
        }
    }
}

pub(super) async fn shutdown_active_prompt(
    active: Option<ActivePrompt>,
    priority_updates: &mpsc::Sender<DesktopRuntimeUpdate>,
) {
    shutdown_active_prompt_with_deadline(active, priority_updates, RUNTIME_SHUTDOWN_DEADLINE).await;
}

pub(super) async fn shutdown_active_prompt_with_deadline(
    active: Option<ActivePrompt>,
    priority_updates: &mpsc::Sender<DesktopRuntimeUpdate>,
    shutdown_deadline: Duration,
) {
    let Some(mut active) = active else {
        return;
    };
    let operation_id = active.operation_id.clone().or_else(|| {
        active
            .connection
            .state()
            .ok()
            .and_then(|snapshot| snapshot.submitted_operation)
            .map(|operation| operation.operation_id)
    });
    if let Some(operation_id) = operation_id.as_deref() {
        let control = active.connection.prompt_control(operation_id);
        let _ = control.abort(
            CodingAgentControlId("desktop-runtime-shutdown".into()),
            "desktop runtime shutdown",
        );
    }
    match tokio::time::timeout(shutdown_deadline, &mut active.task).await {
        Ok(Ok((mut session, _))) => {
            let _ = session.shutdown().await;
        }
        Ok(Err(_)) => {
            let _ = priority_updates
                .send(DesktopRuntimeUpdate::RuntimeFailed {
                    error: local_runtime_error(
                        "runtime_task_panicked",
                        "A desktop runtime task stopped unexpectedly.",
                    ),
                })
                .await;
        }
        Err(_) => {
            active.task.abort();
            let _ = active.task.await;
            let _ = priority_updates
                .send(DesktopRuntimeUpdate::RuntimeFailed {
                    error: DesktopRuntimeError {
                        code: "shutdown_deadline_exceeded".into(),
                        message: format!(
                            "prompt operation {} did not stop within {} seconds",
                            operation_id.as_deref().unwrap_or("<starting>"),
                            shutdown_deadline.as_secs_f64()
                        ),
                    },
                })
                .await;
        }
    }
    let _ = active.connection.detach();
}
