use super::super::*;

pub(super) fn resolve_session_log_root(
    options: &CodingAgentSessionOptions,
) -> Result<PathBuf, CodingSessionError> {
    if let Some(root) = options.session_log_root() {
        return Ok(root.to_path_buf());
    }
    crate::app::session::default_sessions_root().map_err(|error| CodingSessionError::Session {
        message: error.to_string(),
    })
}

pub(super) fn open_target(
    options: &CodingAgentSessionOptions,
) -> Result<PathBuf, CodingSessionError> {
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

pub(super) fn normalize_session_id(value: &str, label: &str) -> Result<String, CodingSessionError> {
    // Reuse the repository's strict charset check: session ids are joined
    // onto the log root as path components, so anything but
    // `[a-zA-Z0-9_-]` (e.g. `../x` or an absolute path) must be rejected
    // before any directory is created or opened.
    crate::session::repository::normalize_session_id(value).map_err(|_| CodingSessionError::Input {
        message: format!("{label} contains unsupported characters"),
    })
}

pub(super) fn option_cwd_string(options: &CodingAgentSessionOptions) -> Option<String> {
    options.cwd().map(normalized_path_string)
}

pub(super) fn option_workspace_scope(
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

pub(super) fn workspace_global_config_dir(options: &CodingAgentSessionOptions) -> PathBuf {
    options
        .workspace_global_config_dir()
        .map(Path::to_path_buf)
        .unwrap_or_else(crate::app::embedding::global_config_directory)
}

pub(super) fn migrate_workspace_on_open(
    store: &SessionLogStore,
    handle: SessionHandle,
    global_config_dir: &Path,
) -> Result<SessionHandle, CodingSessionError> {
    migrate_workspace_handle(store, handle, global_config_dir).map(|(handle, _)| handle)
}

pub(super) fn migrate_workspace_handle(
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

pub(super) struct SessionWorkspaceFacts {
    pub(super) scope: CodingAgentWorkspaceScope,
    pub(super) migration: CodingAgentWorkspaceMigration,
    pub(super) compatibility_cwd: Option<String>,
}

pub(super) fn workspace_facts_for_summary(
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

pub(super) fn compatibility_cwd(scope: &CodingAgentWorkspaceScope) -> Option<String> {
    match scope {
        CodingAgentWorkspaceScope::Project { cwd } => Some(normalized_path_string(cwd)),
        CodingAgentWorkspaceScope::Projectless { .. }
        | CodingAgentWorkspaceScope::Legacy { .. } => None,
    }
}

pub(super) fn workspace_persistence_error(
    error: crate::workspace::CodingAgentWorkspaceResolutionError,
) -> CodingSessionError {
    CodingSessionError::Session {
        message: format!("invalid durable workspace identity: {error}"),
    }
}

pub(super) fn option_default_agent_profile_id(options: &CodingAgentSessionOptions) -> ProfileId {
    options
        .default_agent_profile_id()
        .cloned()
        .unwrap_or_else(|| ProfileId::from("default"))
}

pub(super) fn normalized_path_string(path: &Path) -> String {
    path.canonicalize()
        .unwrap_or_else(|_| path.to_path_buf())
        .to_string_lossy()
        .into_owned()
}
