//! Worker-owned runtime session state and the session operations that mutate it.

use std::collections::HashMap;

use coding_agent::api::client::{
    CodingAgentClientId, CodingAgentDraftId, CodingAgentSubmissionDraft,
};
use coding_agent::api::embedding::{
    CodingAgentEmbeddingContext, CodingAgentEmbeddingOptions, CodingAgentSessionOpenTarget,
    CodingAgentThinkingLevel, CodingAgentWorkspaceScope, CodingAgentWorkspaceSelection,
};
use coding_agent::api::event::CodingAgentRecoveryResolution;
use coding_agent::api::operation::{CodingAgentOperation, CodingAgentOperationOutcome};
use coding_agent::api::review::{
    CodingAgentExternalEditorTarget, CodingAgentFileReview, CodingAgentFileReviewRequest,
};
use coding_agent::api::runtime::{
    CodingAgentRecoveryResolutionRequest, CodingAgentRecoveryRetryRequest, CodingAgentSession,
};
use coding_agent::api::view::{CodingAgentSessionTranscriptItem, CodingAgentTranscriptSnapshot};
use tokio::task;

use super::{
    ActivePrompt, DESKTOP_CLIENT_ID, HomeRuntimeContext, NewPromptSession, RuntimeSessionWorkspace,
    admitted_model_thinking, reconnect_event_source,
};
use crate::runtime::protocol::{
    DesktopBridgeError, DesktopRecoveryIdentity, DesktopRuntimeHydratedSnapshot,
    DesktopRuntimeMetadataSnapshot, DesktopRuntimeRecoverySnapshot, DesktopSessionCatalogEntry,
    MAX_CONCURRENT_DESKTOP_SESSIONS, MAX_DESKTOP_SESSION_CATALOG, MAX_SESSION_ID_BYTES,
    bounded_utf8_prefix,
};

pub(in crate::runtime) struct RuntimeState {
    pub(in crate::runtime) home: HomeRuntimeContext,
    pub(in crate::runtime) workspaces: HashMap<String, RuntimeSessionWorkspace>,
    pub(in crate::runtime) focused_session_id: Option<String>,
    #[cfg(test)]
    pub(in crate::runtime) fail_next_prompt_start: bool,
}

impl RuntimeState {
    pub(in crate::runtime) fn metadata_snapshot(
        &self,
        session_id: Option<&str>,
    ) -> Result<DesktopRuntimeMetadataSnapshot, DesktopBridgeError> {
        let session_id = session_id.or(self.focused_session_id.as_deref());
        Ok(
            match session_id.and_then(|session_id| self.workspaces.get(session_id)) {
                Some(workspace) => DesktopRuntimeMetadataSnapshot {
                    project: workspace.context.snapshot().clone(),
                    session: Some(workspace.session.snapshot()?),
                },
                None => DesktopRuntimeMetadataSnapshot {
                    project: self.home.context.snapshot().clone(),
                    session: None,
                },
            },
        )
    }

    pub(in crate::runtime) fn snapshot(
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
            session: workspace.session.snapshot()?,
            transcript: workspace.session.transcript_snapshot()?,
            pending_recoveries: workspace.session.recovery_pending()?,
        })
    }

    pub(in crate::runtime) fn session_catalog(
        &self,
    ) -> Result<(Vec<DesktopSessionCatalogEntry>, usize), DesktopBridgeError> {
        let catalog = self.home.context.session_directory_query()?.overviews()?;
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
            session: workspace.session.snapshot()?,
            pending_recoveries: workspace.session.recovery_pending()?,
        })
    }

    pub(in crate::runtime) async fn open_change(
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
            .open_change(request)
            .await
            .map_err(DesktopBridgeError::from)
    }

    pub(in crate::runtime) async fn validate_external_editor_target(
        &self,
        session_id: &str,
        target: CodingAgentExternalEditorTarget,
    ) -> Result<CodingAgentExternalEditorTarget, DesktopBridgeError> {
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
        Ok(target)
    }

    pub(in crate::runtime) async fn retry_recovery(
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
        let result = session
            .retry_recovery(CodingAgentRecoveryRetryRequest {
                operation_id: identity.operation_id,
                recovery_id: identity.recovery_id,
                expected_record_version: identity.record_version,
                expected_descriptor_revision: identity.descriptor_revision,
                expected_capability_generation: identity.capability_generation,
                expected_attempt_count: identity.attempt_count,
                schedule_with_backoff: false,
            })
            .await?;
        let recovery_id = result.recovery_id;
        Ok((recovery_id, self.recovery_snapshot(session_id)?))
    }

    pub(in crate::runtime) async fn resolve_recovery(
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
        let result = session
            .resolve_recovery(CodingAgentRecoveryResolutionRequest {
                operation_id: identity.operation_id,
                recovery_id: identity.recovery_id,
                expected_record_version: identity.record_version,
                expected_descriptor_revision: identity.descriptor_revision,
                expected_capability_generation: identity.capability_generation,
                expected_attempt_count: identity.attempt_count,
                resolution,
                reason: format!("native desktop operator {action} uncertain operation"),
            })
            .await?;
        let recovery_id = result.recovery_id;
        Ok((recovery_id, self.recovery_snapshot(session_id)?))
    }

    pub(in crate::runtime) async fn rename_session(
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

    pub(in crate::runtime) async fn list_merge_proposals(
        &mut self,
        session_id: &str,
    ) -> Result<Vec<coding_agent::api::event::CodingAgentMergeProposal>, DesktopBridgeError> {
        let session = &mut self
            .workspaces
            .get_mut(session_id)
            .ok_or_else(|| DesktopBridgeError::Busy {
                operation: "desktop_list_merge_proposals".into(),
            })?
            .session;
        let outcome = session
            .run(CodingAgentOperation::ListMergeProposals)
            .await?;
        let CodingAgentOperationOutcome::MergeProposals(proposals) = outcome else {
            return Err(DesktopBridgeError::Session {
                message: "listing merge proposals returned an unexpected outcome".into(),
            });
        };
        Ok(proposals)
    }

    pub(in crate::runtime) async fn merge_child_worktree(
        &mut self,
        session_id: &str,
        worktree_id: String,
    ) -> Result<(String, usize), DesktopBridgeError> {
        let session = &mut self
            .workspaces
            .get_mut(session_id)
            .ok_or_else(|| DesktopBridgeError::Busy {
                operation: "desktop_merge_child_worktree".into(),
            })?
            .session;
        let outcome = session
            .run(CodingAgentOperation::MergeChildWorktree { worktree_id })
            .await?;
        let CodingAgentOperationOutcome::MergeApplied {
            worktree_id,
            applied,
            ..
        } = outcome
        else {
            return Err(DesktopBridgeError::Session {
                message: "merging a child worktree returned an unexpected outcome".into(),
            });
        };
        Ok((worktree_id, applied))
    }

    pub(in crate::runtime) async fn discard_child_worktree(
        &mut self,
        session_id: &str,
        worktree_id: String,
    ) -> Result<String, DesktopBridgeError> {
        let session = &mut self
            .workspaces
            .get_mut(session_id)
            .ok_or_else(|| DesktopBridgeError::Busy {
                operation: "desktop_discard_child_worktree".into(),
            })?
            .session;
        session
            .discard_child_proposal(worktree_id)
            .await
            .map_err(DesktopBridgeError::from)
    }

    pub(in crate::runtime) async fn create_session(
        &mut self,
        open_session_count: usize,
    ) -> Result<String, DesktopBridgeError> {
        self.ensure_capacity(open_session_count)?;
        let (session_id, context) = self.home.load_session_context()?;
        self.create_session_in_context(session_id, context)
            .await
            .map(|(session_id, _)| session_id)
    }

    pub(in crate::runtime) async fn create_session_for_workspace(
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
        let (session_id, options) =
            options
                .into_new_session()
                .map_err(|error| DesktopBridgeError::Input {
                    message: format!("desktop session workspace could not be resolved: {error}"),
                })?;
        let context = CodingAgentEmbeddingContext::load(options)?;
        let selected_model_id = context.snapshot().selected_model_id.clone();
        let (thinking_level, _) =
            admitted_model_thinking(&context, &selected_model_id, thinking_level)?;
        let (session_id, snapshot) = self.create_session_in_context(session_id, context).await?;
        Ok(NewPromptSession {
            session_id,
            snapshot,
            thinking_level,
        })
    }

    async fn create_session_in_context(
        &mut self,
        session_id: String,
        context: CodingAgentEmbeddingContext,
    ) -> Result<(String, DesktopRuntimeHydratedSnapshot), DesktopBridgeError> {
        let scope = RuntimeSessionWorkspace::scope_for_context(&context)?;
        let project = context.snapshot().clone();
        let session = context.create_session_with_id(session_id.clone()).await?;
        let snapshot = DesktopRuntimeHydratedSnapshot {
            project,
            session: session.snapshot()?,
            transcript: CodingAgentTranscriptSnapshot::new(session_id.clone(), None, Vec::new()),
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

    pub(in crate::runtime) async fn open_session(
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

    pub(in crate::runtime) fn start_prompt(
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
        let session_name_updates = match session.subscribe_session_name_updates() {
            Some(updates) if updates.current().name.is_none() => {
                let transcript = session.transcript_snapshot()?;
                (!transcript.items.iter().any(|item| {
                    matches!(
                        item,
                        CodingAgentSessionTranscriptItem::User { .. }
                            | CodingAgentSessionTranscriptItem::Assistant { .. }
                    )
                }))
                .then_some(updates)
            }
            Some(_) | None => None,
        };
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
            session_name_updates,
            task,
        })
    }

    pub(in crate::runtime) fn insert_idle_workspace(
        &mut self,
        session_id: String,
        scope: CodingAgentWorkspaceScope,
        context: CodingAgentEmbeddingContext,
        session: CodingAgentSession,
    ) {
        self.workspaces.insert(
            session_id,
            RuntimeSessionWorkspace {
                scope,
                context,
                session,
            },
        );
    }

    pub(in crate::runtime) async fn close_idle_session(
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

    pub(in crate::runtime) async fn delete_session(
        &mut self,
        session_id: &str,
    ) -> Result<(), DesktopBridgeError> {
        if let Some(mut workspace) = self.workspaces.remove(session_id) {
            workspace.session.shutdown().await?;
        }
        self.home
            .context
            .session_directory_query()?
            .delete_session(session_id)?;
        if self.focused_session_id.as_deref() == Some(session_id) {
            self.focused_session_id = self.workspaces.keys().min().cloned();
        }
        Ok(())
    }

    pub(in crate::runtime) async fn shutdown_idle_sessions(
        &mut self,
    ) -> Result<(), DesktopBridgeError> {
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
