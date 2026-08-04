use super::*;

impl SessionService {
    pub(super) fn from_handle(
        store: SessionLogStore,
        handle: SessionHandle,
    ) -> Result<Self, CodingSessionError> {
        // The writer lease repairs only a torn final frame before any durable
        // records are decoded, published, or redelivered after restart.
        let transaction_writer = SessionTransactionWriter::new(store.clone(), handle.clone())?;
        let committed_session_sequence = transaction_writer.committed_session_sequence();
        let manifest = transaction_writer.manifest_snapshot()?;
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

    /// Opens just enough repository state for a bounded transcript bootstrap.
    ///
    /// Unlike [`Self::open`], this path deliberately avoids constructing the
    /// transaction writer, reading the outbox, and running full-log startup
    /// recovery. The short-lived lease preserves the existing torn-tail repair
    /// guarantee before the reverse reader inspects the durable suffix.
    pub(super) fn open_hydration_handle(
        options: &CodingAgentSessionOptions,
    ) -> Result<(SessionLogStore, SessionHandle), CodingSessionError> {
        let root = resolve_session_log_root(options)?;
        let store = SessionLogStore::new(root);
        let target = open_target(options)?;
        let handle = store.open_session(&target)?;
        let handle = migrate_workspace_on_open(
            &store,
            handle,
            workspace_global_config_dir(options).as_path(),
        )?;
        store.repair_tails_for_bounded_read(&handle)?;
        Ok((store, handle))
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

    pub(crate) fn delete(options: &CodingAgentSessionOptions) -> Result<(), CodingSessionError> {
        let root = resolve_session_log_root(options)?;
        let store = SessionLogStore::new(root);
        let target = open_target(options)?;
        let handle = store.open_session(&target)?;
        store.remove_session(&handle)
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

    pub(crate) fn take_startup_outbox_records(&mut self) -> Vec<DurableOutboxRecord> {
        std::mem::take(&mut self.startup_outbox_records)
    }

    pub(super) fn transaction_writer(&self) -> SessionTransactionWriter {
        self.transaction_writer.clone()
    }

    pub(super) async fn commit_writer_mutation(
        &self,
        events: Vec<SessionEventEnvelope>,
        manifest_patch: ManifestPatch,
        operation_id: Option<String>,
    ) -> Result<(), CodingSessionError> {
        let receipt = self
            .transaction_writer
            .commit_session_mutation(events, manifest_patch, operation_id)
            .await?;
        observe_commit_receipt(&self.committed_session_sequence, receipt);
        Ok(())
    }

    pub(super) async fn commit_writer_mutation_with_outbox(
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
            )
            .await?;
        observe_commit_receipt(&self.committed_session_sequence, receipt);
        Ok(())
    }

    pub(super) fn commit_writer_mutation_blocking(
        &self,
        events: Vec<SessionEventEnvelope>,
        manifest_patch: ManifestPatch,
        operation_id: Option<String>,
    ) -> Result<(), CodingSessionError> {
        let receipt = self.transaction_writer.commit_session_mutation_blocking(
            events,
            manifest_patch,
            operation_id,
        )?;
        observe_commit_receipt(&self.committed_session_sequence, receipt);
        Ok(())
    }

    pub(super) fn commit_writer_mutation_with_outbox_blocking(
        &self,
        events: Vec<SessionEventEnvelope>,
        outbox_records: Vec<DurableOutboxRecordCandidate>,
        manifest_patch: ManifestPatch,
        operation_id: Option<String>,
    ) -> Result<(), CodingSessionError> {
        let receipt = self
            .transaction_writer
            .commit_session_mutation_with_outbox_blocking(
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

    pub(super) fn copy_to_new_session(
        &self,
        target_leaf_id: Option<&str>,
        kind: SessionCopyKind,
        admitted_operation_id: Option<&str>,
    ) -> Result<Self, CodingSessionError> {
        let writer_manifest = self.transaction_writer.manifest_snapshot()?;
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
            target.commit_writer_mutation_blocking(
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
    pub(super) fn create_with_id(
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
            .initialize_session_with_receipt_blocking(created)
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

    pub(super) fn next_leaf_id() -> String {
        let mut ids = SystemIdGenerator;
        ids.next_leaf_id()
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

mod workspace;

use workspace::*;
