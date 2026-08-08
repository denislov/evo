use super::*;

impl CodingAgentSession {
    pub async fn create(
        options: CodingAgentSessionOptions,
    ) -> Result<Self, CodingAgentPublicError> {
        Self::create_internal(options)
            .await
            .map_err(CodingAgentPublicError::from)
    }

    pub(crate) async fn create_internal(
        options: CodingAgentSessionOptions,
    ) -> Result<Self, CodingSessionError> {
        let observation_started = observe_session_started("create");
        let service_options = options.clone();
        let session_service =
            tokio::task::spawn_blocking(move || SessionService::create(&service_options))
                .await
                .map_err(session_initialization_join_error)??;
        let project_root = session_project_root(&options, Some(&session_service));
        let profile_registry = profile_registry_for_options(&options, Some(&session_service))?;
        let runtime_service = runtime_service_for_options(&options);
        let worktree_registry = worktree_registry_for(&options, &project_root)?;
        let result = Self::from_services(
            session_service,
            profile_registry,
            runtime_service,
            options.tool_authorization_mode(),
            project_root,
            worktree_registry,
            options.extension_host_options().cloned(),
        )
        .await;
        observe_session_finished("create", observation_started, &result);
        result
    }

    pub async fn open(options: CodingAgentSessionOptions) -> Result<Self, CodingAgentPublicError> {
        Self::open_internal(options)
            .await
            .map_err(CodingAgentPublicError::from)
    }

    pub(crate) async fn open_internal(
        options: CodingAgentSessionOptions,
    ) -> Result<Self, CodingSessionError> {
        let observation_started = observe_session_started("open");
        let service_options = options.clone();
        let session_service =
            tokio::task::spawn_blocking(move || SessionService::open(&service_options))
                .await
                .map_err(session_initialization_join_error)??;
        let project_root = session_project_root(&options, Some(&session_service));
        let profile_registry = profile_registry_for_options(&options, Some(&session_service))?;
        let runtime_service = runtime_service_for_options(&options);
        let worktree_registry = worktree_registry_for(&options, &project_root)?;
        let result = Self::from_services(
            session_service,
            profile_registry,
            runtime_service,
            options.tool_authorization_mode(),
            project_root,
            worktree_registry,
            options.extension_host_options().cloned(),
        )
        .await;
        observe_session_finished("open", observation_started, &result);
        result
    }

    pub async fn open_or_create(
        options: CodingAgentSessionOptions,
    ) -> Result<Self, CodingAgentPublicError> {
        Self::open_or_create_internal(options)
            .await
            .map_err(CodingAgentPublicError::from)
    }

    pub(crate) async fn open_or_create_internal(
        options: CodingAgentSessionOptions,
    ) -> Result<Self, CodingSessionError> {
        let observation_started = observe_session_started("open_or_create");
        let service_options = options.clone();
        let session_service =
            tokio::task::spawn_blocking(move || SessionService::open_or_create(&service_options))
                .await
                .map_err(session_initialization_join_error)??;
        let project_root = session_project_root(&options, Some(&session_service));
        let profile_registry = profile_registry_for_options(&options, Some(&session_service))?;
        let runtime_service = runtime_service_for_options(&options);
        let worktree_registry = worktree_registry_for(&options, &project_root)?;
        let result = Self::from_services(
            session_service,
            profile_registry,
            runtime_service,
            options.tool_authorization_mode(),
            project_root,
            worktree_registry,
            options.extension_host_options().cloned(),
        )
        .await;
        observe_session_finished("open_or_create", observation_started, &result);
        result
    }

    pub async fn non_persistent(
        options: CodingAgentSessionOptions,
    ) -> Result<Self, CodingAgentPublicError> {
        Self::non_persistent_internal(options)
            .await
            .map_err(CodingAgentPublicError::from)
    }

    pub(crate) async fn non_persistent_internal(
        options: CodingAgentSessionOptions,
    ) -> Result<Self, CodingSessionError> {
        let observation_started = observe_session_started("non_persistent");
        if options.session_id().is_some() || options.session_path().is_some() {
            return Err(CodingSessionError::Input {
                message: "non-persistent coding sessions do not accept a session id or path".into(),
            });
        }
        let project_root = session_project_root(&options, None);
        let worktree_registry = worktree_registry_for(&options, &project_root)?;
        let result = Self::from_transient(
            TransientSessionState::new(option_default_agent_profile_id(&options)),
            profile_registry_for_options(&options, None)?,
            runtime_service_for_options(&options),
            options.tool_authorization_mode(),
            project_root,
            worktree_registry,
            options.extension_host_options().cloned(),
        );
        observe_session_finished("non_persistent", observation_started, &result);
        result
    }

    pub fn list(
        options: CodingAgentSessionOptions,
    ) -> Result<Vec<CodingAgentSessionSummary>, CodingAgentPublicError> {
        Self::list_internal(options).map_err(CodingAgentPublicError::from)
    }

    pub(crate) fn list_internal(
        options: CodingAgentSessionOptions,
    ) -> Result<Vec<CodingAgentSessionSummary>, CodingSessionError> {
        SessionService::list(&options)
    }

    pub(crate) fn list_overviews_internal(
        options: CodingAgentSessionOptions,
        limit: usize,
    ) -> Result<(Vec<CodingAgentSessionOverview>, bool), CodingSessionError> {
        SessionService::list_overviews(&options, limit)
    }

    pub(crate) fn hydrate(
        options: CodingAgentSessionOptions,
    ) -> Result<CodingAgentSessionHydration, CodingSessionError> {
        crate::operations::session_navigation::hydrate(options)
    }

    pub(crate) fn tree_view(
        options: CodingAgentSessionOptions,
    ) -> Result<CodingAgentSessionTree, CodingSessionError> {
        crate::operations::session_navigation::tree_view(options)
    }

    pub(crate) fn clone_session(
        options: CodingAgentSessionOptions,
    ) -> Result<CodingAgentSessionHydration, CodingSessionError> {
        crate::operations::session_navigation::clone_session(options)
    }

    pub(crate) fn fork_session(
        options: CodingAgentSessionOptions,
        target_leaf_id: Option<&str>,
    ) -> Result<CodingAgentSessionHydration, CodingSessionError> {
        crate::operations::session_navigation::fork_session(options, target_leaf_id)
    }

    pub fn export_session_html(
        options: CodingAgentSessionOptions,
        path: impl AsRef<Path>,
    ) -> Result<PathBuf, CodingAgentPublicError> {
        Self::export_session_html_internal(options, path).map_err(CodingAgentPublicError::from)
    }

    pub(crate) fn export_session_html_internal(
        options: CodingAgentSessionOptions,
        path: impl AsRef<Path>,
    ) -> Result<PathBuf, CodingSessionError> {
        let session_service = SessionService::open(&options)?;
        let mut context = session_service
            .session_export(ExportOptions::html(path.as_ref()))?
            .into_context();
        let outcome =
            crate::operations::export::runner::ExportRunner::new()?.run_typed(&mut context)?;
        outcome.path.ok_or_else(|| CodingSessionError::Session {
            message: "export completed without a written html path".into(),
        })
    }

    pub fn export_session_branch_html(
        options: CodingAgentSessionOptions,
        branch_id: impl AsRef<str>,
        path: impl AsRef<Path>,
    ) -> Result<PathBuf, CodingAgentPublicError> {
        Self::export_session_branch_html_internal(options, branch_id, path)
            .map_err(CodingAgentPublicError::from)
    }

    pub(crate) fn export_session_branch_html_internal(
        options: CodingAgentSessionOptions,
        branch_id: impl AsRef<str>,
        path: impl AsRef<Path>,
    ) -> Result<PathBuf, CodingSessionError> {
        let session_service = SessionService::open(&options)?;
        let mut context = session_service
            .session_export_branch(branch_id.as_ref(), ExportOptions::html(path.as_ref()))?
            .into_context();
        let outcome =
            crate::operations::export::runner::ExportRunner::new()?.run_typed(&mut context)?;
        outcome.path.ok_or_else(|| CodingSessionError::Session {
            message: "branch export completed without a written html path".into(),
        })
    }

    async fn from_services(
        session_service: SessionService,
        profile_registry: ProfileRegistry,
        runtime_service: RuntimeService,
        tool_authorization_mode: crate::authorization::ToolAuthorizationMode,
        project_root: PathBuf,
        worktree_registry: Arc<workspace_runtime::api::WorktreeRegistry>,
        extension_host_options: Option<extension_host::api::ExtensionHostOptions>,
    ) -> Result<Self, CodingSessionError> {
        let mut session_service = session_service;
        let startup_rewind = session_service.startup_rewind_checkpoint()?;
        let replay_state = replay_derived_owner_state(&mut session_service)?;
        let startup_outbox_records = session_service.take_startup_outbox_records();
        let snapshot_coordinator = SnapshotCoordinator::new();
        let event_service = EventService::with_snapshot_coordinator(snapshot_coordinator.clone());
        let client_service = ClientService::new(snapshot_coordinator.clone());
        let extension_host = extension_host_service(extension_host_options.as_ref(), &project_root);
        let session_id = session_service.session_id().to_owned();
        let workspace_root = project_root.to_string_lossy().into_owned();
        let authorization_service = AuthorizationService::new(
            tool_authorization_mode,
            snapshot_coordinator.clone(),
            event_service.clone(),
            extension_host.sink(),
            session_id,
            workspace_root,
        );
        let background_tasks =
            crate::services::background::BackgroundTaskService::new(event_service.clone());
        let review_workspace = review_workspace_for(
            &project_root,
            review_workspace_kind(&session_service, &project_root, &worktree_registry)?,
        )?;

        let session = Self {
            runtime_host: crate::runtime::owners::RuntimeHost {
                operation_supervisor: crate::runtime::owners::OperationSupervisor {
                    control: OperationControl::with_snapshot_coordinator(
                        snapshot_coordinator.clone(),
                    )
                    .with_worktree_registry(worktree_registry),
                    capabilities: CapabilitySnapshotService::with_snapshot_coordinator(
                        snapshot_coordinator.clone(),
                    ),
                },
                session_coordinator: crate::application::session_coordinator::SessionCoordinator {
                    persistence: SessionPersistence::Persistent(session_service),
                    pending_delegation_confirmations: replay_state.pending_delegation_confirmations,
                    startup_recovery_markers: Mutex::new(replay_state.startup_recovery_markers),
                },
                events: event_service.clone(),
                client_projection: crate::runtime::owners::ClientProjectionCoordinator {
                    snapshots: snapshot_coordinator.clone(),
                    clients: client_service,
                    pending_submission: None,
                },
                runtime_service: runtime_service.with_background_tasks(background_tasks.clone()),
                background_tasks,
                extension_host: extension_host.clone(),
                profile_registry,
                authorization_service,
                review_service: crate::services::review::ReviewService::new(
                    review_workspace,
                    snapshot_coordinator.clone(),
                    event_service,
                ),
                project_root: crate::runtime::owners::ProjectRoot::new(project_root),
            },
        };
        if let Some(checkpoint) = startup_rewind {
            session
                .runtime_host
                .review_service
                .restore_checkpoint(&crate::services::review::ReviewCheckpoint {
                    tracker: checkpoint.tracker,
                    workspace: checkpoint.workspace,
                })
                .await?;
        }
        session.refresh_snapshot_projection()?;
        // ARC-730：hook 修改自动归因与 review tracker 共享 handle 槽
        // （tracker 懒启动时填充、rewind 停用时清空，观察点不长期持有）。
        session
            .runtime_host
            .review_service
            .bind_hook_tracker_slot(extension_host.hook_tracker_slot());
        let opened_session_id = match &session.runtime_host.session_coordinator.persistence {
            SessionPersistence::Persistent(session_service) => {
                session_service.session_id().to_owned()
            }
            SessionPersistence::NonPersistent(state) => state.runtime_id.clone(),
        };
        session
            .runtime_host
            .events
            .emit_session_opened(opened_session_id.clone())?;
        session.runtime_host.extension_host.submit_event(
            extension_host::api::ExtensionEventKind::SessionStart,
            &opened_session_id,
            &session
                .runtime_host
                .project_root
                .as_path()
                .to_string_lossy(),
            extension_host::api::ExtensionEventPayload::SessionStart {
                source: "open".into(),
                model_id: None,
                agent_type: None,
            },
        );
        // 首次启用展示：来源 + 能力（folder trust 未决定 → 等待产品
        // 放行；确认路径由 Phase 9 CLI/Desktop 完成，此处经诊断展示）。
        for request in session.runtime_host.extension_host.first_enables() {
            let capabilities = request
                .capabilities
                .iter()
                .map(|claim| format!("{}:{:?}", claim.name, claim.risk))
                .collect::<Vec<_>>()
                .join(", ");
            session.runtime_host.events.emit_diagnostic(
                Option::<String>::None,
                format!(
                    "extension '{}' ({}) awaits first-enable approval: source={}, dir={}, \
                     capabilities=[{capabilities}]",
                    request.extension_id,
                    request.name,
                    request.source,
                    request.source_dir.to_string_lossy(),
                ),
            )?;
        }
        for record in startup_outbox_records {
            session
                .runtime_host
                .events
                .emit_durable_outbox_record(&record)?;
        }
        Ok(session)
    }

    fn from_transient(
        state: TransientSessionState,
        profile_registry: ProfileRegistry,
        runtime_service: RuntimeService,
        tool_authorization_mode: crate::authorization::ToolAuthorizationMode,
        project_root: PathBuf,
        worktree_registry: Arc<workspace_runtime::api::WorktreeRegistry>,
        extension_host_options: Option<extension_host::api::ExtensionHostOptions>,
    ) -> Result<Self, CodingSessionError> {
        let snapshot_coordinator = SnapshotCoordinator::new();
        let client_service = ClientService::new(snapshot_coordinator.clone());
        let event_service = EventService::with_snapshot_coordinator(snapshot_coordinator.clone());
        let extension_host = extension_host_service(extension_host_options.as_ref(), &project_root);
        let workspace_root = project_root.to_string_lossy().into_owned();
        let authorization_service = AuthorizationService::new(
            tool_authorization_mode,
            snapshot_coordinator.clone(),
            event_service.clone(),
            extension_host.sink(),
            state.runtime_id.clone(),
            workspace_root,
        );
        let background_tasks =
            crate::services::background::BackgroundTaskService::new(event_service.clone());
        let review_workspace =
            review_workspace_for(&project_root, workspace_runtime::api::WorkspaceKind::Source)?;
        let session = Self {
            runtime_host: crate::runtime::owners::RuntimeHost {
                operation_supervisor: crate::runtime::owners::OperationSupervisor {
                    control: OperationControl::with_snapshot_coordinator(
                        snapshot_coordinator.clone(),
                    )
                    .with_worktree_registry(worktree_registry),
                    capabilities: CapabilitySnapshotService::with_snapshot_coordinator(
                        snapshot_coordinator.clone(),
                    ),
                },
                session_coordinator: crate::application::session_coordinator::SessionCoordinator {
                    persistence: SessionPersistence::NonPersistent(state),
                    pending_delegation_confirmations: PendingDelegationConfirmationQueue::default(),
                    startup_recovery_markers: Mutex::new(Vec::new()),
                },
                events: event_service.clone(),
                client_projection: crate::runtime::owners::ClientProjectionCoordinator {
                    snapshots: snapshot_coordinator.clone(),
                    clients: client_service,
                    pending_submission: None,
                },
                runtime_service: runtime_service.with_background_tasks(background_tasks.clone()),
                background_tasks,
                extension_host: extension_host.clone(),
                profile_registry,
                authorization_service,
                review_service: crate::services::review::ReviewService::new(
                    review_workspace,
                    snapshot_coordinator.clone(),
                    event_service,
                ),
                project_root: crate::runtime::owners::ProjectRoot::new(project_root),
            },
        };
        session.refresh_snapshot_projection()?;
        let session_id = session.runtime_host.session_identity().0;
        // ARC-730：hook 修改自动归因与 review tracker 共享 handle 槽
        // （tracker 懒启动时填充、rewind 停用时清空，观察点不长期持有）。
        session
            .runtime_host
            .review_service
            .bind_hook_tracker_slot(extension_host.hook_tracker_slot());
        session.runtime_host.extension_host.submit_event(
            extension_host::api::ExtensionEventKind::SessionStart,
            &session_id,
            &session
                .runtime_host
                .project_root
                .as_path()
                .to_string_lossy(),
            extension_host::api::ExtensionEventPayload::SessionStart {
                source: "new".into(),
                model_id: None,
                agent_type: None,
            },
        );
        Ok(session)
    }
}

fn observe_session_started(mode: &'static str) -> std::time::Instant {
    tracing::info!(
        target: "evo::lifecycle",
        domain = "session",
        phase = "started",
        mode,
    );
    std::time::Instant::now()
}

fn observe_session_finished(
    mode: &'static str,
    started: std::time::Instant,
    result: &Result<CodingAgentSession, CodingSessionError>,
) {
    let session_id = result
        .as_ref()
        .ok()
        .map(|session| session.runtime_host.session_identity().0);
    tracing::info!(
        target: "evo::lifecycle",
        domain = "session",
        phase = if result.is_ok() { "ready" } else { "failed" },
        mode,
        session_id = session_id.as_deref().unwrap_or("unavailable"),
        duration_ms = started.elapsed().as_millis() as u64,
    );
}

/// 装配 extension host 服务：显式提供 host options 时启动真实 host
/// （user hooks 生效），否则保持 Noop（无 host，行为不变）。
///
/// project hooks 目录默认取 `project_root/.evo/extensions`，与 product
/// folder trust（[`ExtensionHostOptions::trust_store`]）共用判定；首次
/// 启用（NotDecided）的扩展经 [`ExtensionHostService::first_enables`]
/// 展示来源与能力。
///
/// ARC-730：产品未显式注入 hook 生命周期观察点时，装配
/// [`HookEditAttribution`]（hook 修改自动归因 `HookEdit`）；review
/// tracker handle 经 [`ExtensionHostService::bind_hook_tracker`] 在
/// session 装配完成后注入。
fn extension_host_service(
    options: Option<&extension_host::api::ExtensionHostOptions>,
    project_root: &std::path::Path,
) -> crate::services::ports::ExtensionHostService {
    let Some(options) = options else {
        return crate::services::ports::ExtensionHostService::new(std::sync::Arc::new(
            crate::services::ports::NoopExtensionHostPort,
        ));
    };
    let mut options = options.clone();
    if options.project_dirs.is_empty() {
        options
            .project_dirs
            .push(project_root.join(".evo").join("extensions"));
    }
    // ARC-730：未显式注入观察点时装配自动归因（hook 修改 → HookEdit）。
    let hook_tracker = Arc::new(std::sync::Mutex::new(None));
    if options.hook_lifecycle.is_none() {
        options.hook_lifecycle = Some(Arc::new(
            crate::services::hook_attribution::HookEditAttribution::new(
                hook_tracker.clone(),
                project_root,
                options.diagnostics.clone(),
            ),
        ));
    }
    match crate::services::ports::LiveExtensionHostPort::start(options) {
        Ok(port) => crate::services::ports::ExtensionHostService::with_hook_tracker_slot(
            Arc::new(port),
            hook_tracker,
        ),
        Err(_) => crate::services::ports::ExtensionHostService::with_hook_tracker_slot(
            std::sync::Arc::new(crate::services::ports::NoopExtensionHostPort),
            hook_tracker,
        ),
    }
}

fn review_workspace_for(
    project_root: &Path,
    kind: workspace_runtime::api::WorkspaceKind,
) -> Result<workspace_runtime::api::WorkspaceAccessHandle, CodingSessionError> {
    let identity =
        workspace_runtime::api::WorkspaceHandle::new(kind, project_root).map_err(|error| {
            CodingSessionError::Resource {
                message: format!("cannot construct managed review workspace: {error}"),
            }
        })?;
    workspace_runtime::api::WorkspaceAccessHandle::open(identity, None, None).map_err(|error| {
        CodingSessionError::Resource {
            message: format!("cannot open managed review workspace: {error}"),
        }
    })
}

fn review_workspace_kind(
    session_service: &SessionService,
    project_root: &Path,
    worktree_registry: &workspace_runtime::api::WorktreeRegistry,
) -> Result<workspace_runtime::api::WorkspaceKind, CodingSessionError> {
    if matches!(
        session_service.persisted_workspace_scope(),
        Some(crate::session::manifest::PersistedWorkspaceScope::Projectless { .. })
    ) {
        return Ok(workspace_runtime::api::WorkspaceKind::Projectless);
    }
    let canonical_root =
        project_root
            .canonicalize()
            .map_err(|error| CodingSessionError::Resource {
                message: format!(
                    "cannot resolve review workspace root {}: {error}",
                    project_root.display()
                ),
            })?;
    let records = worktree_registry
        .load_all()
        .map_err(|error| CodingSessionError::Resource {
            message: format!("cannot inspect managed worktree registry: {error}"),
        })?;
    if records.iter().any(|record| record.dest == canonical_root) {
        Ok(workspace_runtime::api::WorkspaceKind::ManagedChild)
    } else {
        Ok(workspace_runtime::api::WorkspaceKind::Source)
    }
}

fn session_initialization_join_error(error: tokio::task::JoinError) -> CodingSessionError {
    CodingSessionError::Session {
        message: format!("session initialization worker failed: {error}"),
    }
}

fn session_project_root(
    options: &CodingAgentSessionOptions,
    session_service: Option<&SessionService>,
) -> PathBuf {
    session_service
        .and_then(session_cwd)
        .or_else(|| options.cwd().map(Path::to_path_buf))
        .unwrap_or_else(default_cwd)
}

fn profile_registry_for_options(
    options: &CodingAgentSessionOptions,
    session_service: Option<&SessionService>,
) -> Result<ProfileRegistry, CodingSessionError> {
    let cwd = options
        .cwd()
        .map(Path::to_path_buf)
        .or_else(|| session_service.and_then(session_cwd))
        .unwrap_or_else(default_cwd);
    let paths = crate::config::resolve_paths(&cwd);
    ProfileRegistry::load(
        ProfileRegistryOptions::new()
            .with_user_root(paths.global_dir)
            .with_project_root(paths.project_dir),
    )
}

fn option_default_agent_profile_id(options: &CodingAgentSessionOptions) -> ProfileId {
    options
        .default_agent_profile_id()
        .cloned()
        .unwrap_or_else(|| ProfileId::from("default"))
}

fn runtime_service_for_options(options: &CodingAgentSessionOptions) -> RuntimeService {
    options
        .ai_client()
        .cloned()
        .map(RuntimeService::with_ai_client)
        .unwrap_or_else(RuntimeService::new)
}

/// The managed-worktree registry root for a session.
///
/// An explicit option wins; otherwise the user-global config directory's
/// `worktrees` directory is used (honoring `EVO_DIR`).
fn worktree_registry_dir_for(options: &CodingAgentSessionOptions, cwd: &Path) -> PathBuf {
    options
        .worktree_registry_dir()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| {
            crate::config::resolve_paths(cwd)
                .global_dir
                .join("worktrees")
        })
}

/// Default live-worktree capacity: the concurrency budget for parallel child
/// agents when the product does not configure a different bound.
const DEFAULT_WORKTREE_CAPACITY: usize = 4;

fn worktree_registry_for(
    options: &CodingAgentSessionOptions,
    cwd: &Path,
) -> Result<Arc<workspace_runtime::api::WorktreeRegistry>, CodingSessionError> {
    let root = worktree_registry_dir_for(options, cwd);
    let registry = workspace_runtime::api::WorktreeRegistry::open_with_capacity(
        root,
        Some(DEFAULT_WORKTREE_CAPACITY),
    )
    .map_err(|error| CodingSessionError::Resource {
        message: format!("cannot open managed worktree registry: {error}"),
    })?;
    registry
        .startup_maintenance()
        .map_err(|error| CodingSessionError::Resource {
            message: format!("cannot recover managed worktree registry: {error}"),
        })?;
    Ok(Arc::new(registry))
}

#[cfg(test)]
#[path = "hooks_tests.rs"]
mod hooks_tests;

#[cfg(test)]
#[path = "hooks_e2e_tests.rs"]
mod hooks_e2e_tests;
