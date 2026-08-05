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
        let service_options = options.clone();
        let session_service =
            tokio::task::spawn_blocking(move || SessionService::create(&service_options))
                .await
                .map_err(session_initialization_join_error)??;
        let project_root = session_project_root(&options, Some(&session_service));
        let profile_registry = profile_registry_for_options(&options, Some(&session_service))?;
        let runtime_service = runtime_service_for_options(&options);
        let worktree_registry = worktree_registry_for(&options, &project_root)?;
        Self::from_services(
            session_service,
            profile_registry,
            runtime_service,
            options.tool_authorization_mode(),
            project_root,
            worktree_registry,
        )
    }

    pub async fn open(options: CodingAgentSessionOptions) -> Result<Self, CodingAgentPublicError> {
        Self::open_internal(options)
            .await
            .map_err(CodingAgentPublicError::from)
    }

    pub(crate) async fn open_internal(
        options: CodingAgentSessionOptions,
    ) -> Result<Self, CodingSessionError> {
        let service_options = options.clone();
        let session_service =
            tokio::task::spawn_blocking(move || SessionService::open(&service_options))
                .await
                .map_err(session_initialization_join_error)??;
        let project_root = session_project_root(&options, Some(&session_service));
        let profile_registry = profile_registry_for_options(&options, Some(&session_service))?;
        let runtime_service = runtime_service_for_options(&options);
        let worktree_registry = worktree_registry_for(&options, &project_root)?;
        Self::from_services(
            session_service,
            profile_registry,
            runtime_service,
            options.tool_authorization_mode(),
            project_root,
            worktree_registry,
        )
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
        let service_options = options.clone();
        let session_service =
            tokio::task::spawn_blocking(move || SessionService::open_or_create(&service_options))
                .await
                .map_err(session_initialization_join_error)??;
        let project_root = session_project_root(&options, Some(&session_service));
        let profile_registry = profile_registry_for_options(&options, Some(&session_service))?;
        let runtime_service = runtime_service_for_options(&options);
        let worktree_registry = worktree_registry_for(&options, &project_root)?;
        Self::from_services(
            session_service,
            profile_registry,
            runtime_service,
            options.tool_authorization_mode(),
            project_root,
            worktree_registry,
        )
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
        if options.session_id().is_some() || options.session_path().is_some() {
            return Err(CodingSessionError::Input {
                message: "non-persistent coding sessions do not accept a session id or path".into(),
            });
        }
        let project_root = session_project_root(&options, None);
        let worktree_registry = worktree_registry_for(&options, &project_root)?;
        Self::from_transient(
            TransientSessionState::new(option_default_agent_profile_id(&options)),
            profile_registry_for_options(&options, None)?,
            runtime_service_for_options(&options),
            options.tool_authorization_mode(),
            project_root,
            worktree_registry,
        )
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

    fn from_services(
        session_service: SessionService,
        profile_registry: ProfileRegistry,
        runtime_service: RuntimeService,
        tool_authorization_mode: crate::authorization::ToolAuthorizationMode,
        project_root: PathBuf,
        worktree_registry: Arc<workspace_runtime::api::WorktreeRegistry>,
    ) -> Result<Self, CodingSessionError> {
        let mut session_service = session_service;
        let replay_state = replay_derived_owner_state(&mut session_service)?;
        let startup_outbox_records = session_service.take_startup_outbox_records();
        let snapshot_coordinator = SnapshotCoordinator::new();
        let event_service = EventService::with_snapshot_coordinator(snapshot_coordinator.clone());
        let client_service = ClientService::new(snapshot_coordinator.clone());
        let authorization_service = AuthorizationService::new(
            tool_authorization_mode,
            snapshot_coordinator.clone(),
            event_service.clone(),
        );

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
                events: event_service,
                client_projection: crate::runtime::owners::ClientProjectionCoordinator {
                    snapshots: snapshot_coordinator,
                    clients: client_service,
                    pending_submission: None,
                },
                runtime_service,
                profile_registry,
                authorization_service,
                project_root: crate::runtime::owners::ProjectRoot::new(project_root),
            },
        };
        session.refresh_snapshot_projection()?;
        let opened_session_id = match &session.runtime_host.session_coordinator.persistence {
            SessionPersistence::Persistent(session_service) => {
                session_service.session_id().to_owned()
            }
            SessionPersistence::NonPersistent(state) => state.runtime_id.clone(),
        };
        session
            .runtime_host
            .events
            .emit_session_opened(opened_session_id)?;
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
    ) -> Result<Self, CodingSessionError> {
        let snapshot_coordinator = SnapshotCoordinator::new();
        let client_service = ClientService::new(snapshot_coordinator.clone());
        let event_service = EventService::with_snapshot_coordinator(snapshot_coordinator.clone());
        let authorization_service = AuthorizationService::new(
            tool_authorization_mode,
            snapshot_coordinator.clone(),
            event_service.clone(),
        );
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
                events: event_service,
                client_projection: crate::runtime::owners::ClientProjectionCoordinator {
                    snapshots: snapshot_coordinator,
                    clients: client_service,
                    pending_submission: None,
                },
                runtime_service,
                profile_registry,
                authorization_service,
                project_root: crate::runtime::owners::ProjectRoot::new(project_root),
            },
        };
        session.refresh_snapshot_projection()?;
        Ok(session)
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
    workspace_runtime::api::WorktreeRegistry::open_with_capacity(
        root,
        Some(DEFAULT_WORKTREE_CAPACITY),
    )
    .map(Arc::new)
    .map_err(|error| CodingSessionError::Resource {
        message: format!("cannot open managed worktree registry: {error}"),
    })
}
