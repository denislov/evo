use super::*;

pub(crate) async fn open_headless_prompt_session(
    options: &PromptRuntimeOptions,
) -> Result<CodingAgentSession, CodingSessionError> {
    let Some(session_options) = options.session.as_ref() else {
        ensure_non_persistent_target(options.session_target.as_ref())?;
        return CodingAgentSession::non_persistent_internal(with_ai_client(
            CodingAgentSessionOptions::new(),
            options.ai_client.as_ref(),
        ))
        .await;
    };
    if !matches!(session_options.mode, SessionMode::Enabled) {
        ensure_non_persistent_target(options.session_target.as_ref())?;
        return CodingAgentSession::non_persistent_internal(with_ai_client(
            session_options_for_run(session_options),
            options.ai_client.as_ref(),
        ))
        .await;
    }

    let session_root = headless_session_root(session_options)?;
    let session_options = with_ai_client(
        session_options_for_run(session_options).with_session_log_root(session_root),
        options.ai_client.as_ref(),
    );
    open_persistent_session(session_options, options.session_target.as_ref()).await
}

pub(crate) fn runtime_session_root(
    options: &SessionRunOptions,
) -> Result<Option<PathBuf>, CodingSessionError> {
    if matches!(options.mode, SessionMode::Enabled) {
        headless_session_root(options).map(Some)
    } else {
        Ok(None)
    }
}

pub(super) fn target_looks_like_rust_native_session_dir(target: &str) -> bool {
    let path = Path::new(target);
    path.is_dir() && path.join("session.json").is_file() && path.join("events.jsonl").is_file()
}

pub(super) fn target_looks_like_legacy_jsonl(target: &str) -> bool {
    let path = Path::new(target);
    path.extension().and_then(|ext| ext.to_str()) == Some("jsonl") || path.is_file()
}

pub(crate) fn hydrate_interactive_session_target(
    session_options: &Option<SessionRunOptions>,
    target: Option<&ResolvedSessionTarget>,
) -> Result<Option<CodingAgentSessionSnapshot>, ApplicationError> {
    let Some(session_options) = enabled_session_options(session_options) else {
        return Ok(None);
    };
    let Some(target) = target else {
        return Ok(None);
    };
    let base_options = interactive_navigation_options(session_options)?;
    let hydration = match target {
        ResolvedSessionTarget::New | ResolvedSessionTarget::ForkTarget(_) => return Ok(None),
        ResolvedSessionTarget::ContinueMostRecent => {
            list_interactive_session_hydrations(&Some(session_options.clone()))?
                .into_iter()
                .next()
        }
        ResolvedSessionTarget::OpenOrCreateId(session_id) => {
            match CodingAgentSession::hydrate(base_options.with_session_id(session_id.clone())) {
                Ok(hydration) => Some(hydration),
                Err(_) => return Ok(None),
            }
        }
        ResolvedSessionTarget::OpenTarget(target) => {
            let is_path = target_looks_like_rust_native_session_dir(target);
            let options = if is_path {
                base_options.with_session_path(target)
            } else {
                base_options.with_session_id(target.clone())
            };
            match CodingAgentSession::hydrate(options) {
                Ok(hydration) => Some(hydration),
                Err(error) if is_path => {
                    return Err(ApplicationError::SessionFailure(error.to_string()));
                }
                Err(_) => return Ok(None),
            }
        }
    };
    Ok(hydration
        .filter(|hydration| hydration_matches_cwd(hydration, &session_options.cwd))
        .map(session_snapshot_from_hydration))
}

pub(crate) fn list_interactive_session_hydrations(
    session_options: &Option<SessionRunOptions>,
) -> Result<Vec<CodingAgentSessionHydration>, ApplicationError> {
    let Some(session_options) = enabled_session_options(session_options) else {
        return Ok(Vec::new());
    };
    let options = interactive_navigation_options(session_options)?;
    Ok(CodingAgentSession::list_internal(options.clone())?
        .into_iter()
        .filter_map(|summary| {
            CodingAgentSession::hydrate(options.clone().with_session_id(summary.session_id)).ok()
        })
        .filter(|hydration| hydration_matches_cwd(hydration, &session_options.cwd))
        .collect())
}

pub(super) fn enabled_session_options(
    session_options: &Option<SessionRunOptions>,
) -> Option<&SessionRunOptions> {
    session_options
        .as_ref()
        .filter(|options| matches!(options.mode, SessionMode::Enabled))
}

pub(super) fn interactive_navigation_options(
    session_options: &SessionRunOptions,
) -> Result<CodingAgentSessionOptions, CodingSessionError> {
    Ok(session_options_for_run(session_options)
        .with_session_log_root(headless_session_root(session_options)?))
}

pub(super) fn session_options_for_run(options: &SessionRunOptions) -> CodingAgentSessionOptions {
    match options.workspace.as_ref() {
        Some(workspace) => {
            CodingAgentSessionOptions::new().with_resolved_workspace(workspace.clone())
        }
        None => CodingAgentSessionOptions::new().with_cwd(options.cwd.clone()),
    }
}

pub(super) fn hydration_matches_cwd(hydration: &CodingAgentSessionHydration, cwd: &Path) -> bool {
    let expected = normalized_path_string(cwd);
    hydration.cwd.as_deref() == Some(expected.as_str())
}

pub(super) fn normalized_path_string(path: &Path) -> String {
    path.canonicalize()
        .unwrap_or_else(|_| path.to_path_buf())
        .to_string_lossy()
        .into_owned()
}

pub(super) fn ensure_non_persistent_target(
    target: Option<&ResolvedSessionTarget>,
) -> Result<(), CodingSessionError> {
    match target {
        None | Some(ResolvedSessionTarget::New) => Ok(()),
        Some(_) => Err(CodingSessionError::UnsupportedCapability {
            capability: "persistent session target in non-persistent headless mode".into(),
        }),
    }
}

async fn open_persistent_session(
    options: CodingAgentSessionOptions,
    target: Option<&ResolvedSessionTarget>,
) -> Result<CodingAgentSession, CodingSessionError> {
    match target.unwrap_or(&ResolvedSessionTarget::New) {
        ResolvedSessionTarget::New => CodingAgentSession::create_internal(options).await,
        ResolvedSessionTarget::OpenTarget(session_id) => {
            CodingAgentSession::open_internal(options.with_session_id(session_id.clone())).await
        }
        ResolvedSessionTarget::OpenOrCreateId(session_id) => {
            CodingAgentSession::open_or_create_internal(options.with_session_id(session_id.clone()))
                .await
        }
        ResolvedSessionTarget::ContinueMostRecent => {
            let session_id = CodingAgentSession::list_internal(options.clone())?
                .into_iter()
                .next()
                .map(|summary| summary.session_id)
                .ok_or_else(|| CodingSessionError::Session {
                    message: "no previous session to continue".into(),
                })?;
            CodingAgentSession::open_internal(options.with_session_id(session_id)).await
        }
        ResolvedSessionTarget::ForkTarget(source) => {
            let forked = CodingAgentSession::fork_session(
                options.clone().with_session_id(source.clone()),
                None,
            )?;
            CodingAgentSession::open_internal(options.with_session_id(forked.summary.session_id))
                .await
        }
    }
}

pub(super) fn headless_session_root(
    options: &SessionRunOptions,
) -> Result<PathBuf, CodingSessionError> {
    match options.session_dir.as_ref() {
        Some(root) => Ok(root.clone()),
        None => resolve_session_dir(&options.cwd, None, None).map_err(|error| {
            CodingSessionError::Session {
                message: error.to_string(),
            }
        }),
    }
}

pub(super) fn with_ai_client(
    options: CodingAgentSessionOptions,
    ai_client: Option<&AiClient>,
) -> CodingAgentSessionOptions {
    match ai_client {
        Some(ai_client) => options.with_ai_client(ai_client.clone()),
        None => options,
    }
}
