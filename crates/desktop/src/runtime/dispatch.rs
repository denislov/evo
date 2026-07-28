use coding_agent::api::client::CodingAgentControlId;
use coding_agent::api::event::CodingAgentRecoveryResolution;

use super::driver::{ActivePrompt, RuntimeState};
use super::protocol::{
    DesktopBridgeError, DesktopRecoveryAction, DesktopRuntimeCommand,
    DesktopRuntimeMetadataSnapshot, DesktopRuntimeResyncSnapshot, DesktopRuntimeSelectionKind,
    DesktopRuntimeUpdate, runtime_error,
};

pub(super) async fn dispatch_idle_command(
    state: &mut RuntimeState,
    active: &mut Option<ActivePrompt>,
    command: DesktopRuntimeCommand,
) -> DesktopRuntimeUpdate {
    let command_id = command.command_id();
    let kind = command.kind();
    let result = match command {
        DesktopRuntimeCommand::Reload { .. } => {
            let reload = state
                .context
                .reload_local_resources()
                .map(|_| ())
                .map_err(DesktopBridgeError::from);
            reload.map(|()| state.metadata_snapshot()).map(|metadata| {
                DesktopRuntimeUpdate::Reloaded {
                    command_id,
                    metadata,
                }
            })
        }
        DesktopRuntimeCommand::Resync { .. } => {
            state
                .snapshot()
                .map(|snapshot| DesktopRuntimeUpdate::Resynced {
                    command_id,
                    replacement: DesktopRuntimeResyncSnapshot::Hydrated(snapshot),
                })
        }
        DesktopRuntimeCommand::CreateSession { .. } => state
            .replace_with_new_session()
            .await
            .and_then(|()| state.snapshot())
            .map(|snapshot| DesktopRuntimeUpdate::SessionChanged {
                command_id,
                snapshot,
            }),
        DesktopRuntimeCommand::OpenSession { session_id, .. } => state
            .replace_with_open_session(session_id)
            .await
            .and_then(|()| state.snapshot())
            .map(|snapshot| DesktopRuntimeUpdate::SessionChanged {
                command_id,
                snapshot,
            }),
        DesktopRuntimeCommand::ListSessions { .. } => {
            state.session_catalog().map(|(sessions, omitted)| {
                DesktopRuntimeUpdate::SessionsListed {
                    command_id,
                    sessions,
                    omitted,
                }
            })
        }
        DesktopRuntimeCommand::SelectModel { model_id, .. } => state
            .context
            .select_model(model_id)
            .map(|_| ())
            .map_err(DesktopBridgeError::from)
            .map(|()| state.metadata_snapshot())
            .map(|metadata| DesktopRuntimeUpdate::SelectionChanged {
                command_id,
                selection: DesktopRuntimeSelectionKind::Model,
                metadata,
            }),
        DesktopRuntimeCommand::SelectSessionProfile { profile_id, .. } => state
            .select_session_profile(profile_id)
            .await
            .map(|metadata| DesktopRuntimeUpdate::SelectionChanged {
                command_id,
                selection: DesktopRuntimeSelectionKind::SessionProfile,
                metadata,
            }),
        DesktopRuntimeCommand::RetryRecovery { identity, .. } => state
            .retry_recovery(identity)
            .map(
                |(recovery_id, recovery)| DesktopRuntimeUpdate::RecoveryChanged {
                    command_id,
                    action: DesktopRecoveryAction::Retry,
                    recovery_id,
                    recovery,
                },
            ),
        DesktopRuntimeCommand::ResolveRecovery {
            identity,
            resolution,
            ..
        } => {
            let action = match resolution {
                CodingAgentRecoveryResolution::Failed => DesktopRecoveryAction::MarkFailed,
                CodingAgentRecoveryResolution::Aborted => DesktopRecoveryAction::Abort,
            };
            state
                .resolve_recovery(identity, resolution)
                .map(
                    |(recovery_id, recovery)| DesktopRuntimeUpdate::RecoveryChanged {
                        command_id,
                        action,
                        recovery_id,
                        recovery,
                    },
                )
        }
        DesktopRuntimeCommand::ReviewChangedFile { request, .. } => state
            .review_changed_file(request)
            .await
            .map(|review| DesktopRuntimeUpdate::FileReviewed { command_id, review }),
        DesktopRuntimeCommand::OpenExternalEditor { target, editor, .. } => state
            .open_external_editor(target, editor)
            .await
            .map(
                |project_relative_path| DesktopRuntimeUpdate::ExternalEditorOpened {
                    command_id,
                    project_relative_path,
                },
            ),
        DesktopRuntimeCommand::SubmitPrompt {
            prompt,
            thinking_level,
            ..
        } => start_prompt(state, active, command_id, prompt, thinking_level).await,
        DesktopRuntimeCommand::Abort { .. }
        | DesktopRuntimeCommand::Steer { .. }
        | DesktopRuntimeCommand::FollowUp { .. }
        | DesktopRuntimeCommand::DecideToolAuthorization { .. } => Err(DesktopBridgeError::Busy {
            operation: "no_active_prompt".into(),
        }),
    };
    result.unwrap_or_else(|error| {
        let error = runtime_error(&error);
        DesktopRuntimeUpdate::CommandRejected {
            command_id,
            command: kind,
            code: error.code,
            message: error.message,
        }
    })
}

async fn start_prompt(
    state: &mut RuntimeState,
    active: &mut Option<ActivePrompt>,
    command_id: u64,
    prompt: String,
    thinking_level: Option<coding_agent::api::embedding::CodingAgentThinkingLevel>,
) -> Result<DesktopRuntimeUpdate, DesktopBridgeError> {
    let created_snapshot = if state.session.is_none() {
        state.replace_with_new_session().await?;
        match state.snapshot() {
            Ok(snapshot) => Some(snapshot),
            Err(error) => {
                return Ok(DesktopRuntimeUpdate::PromptRejectedWithSession {
                    command_id,
                    metadata: state.metadata_snapshot(),
                    snapshot: None,
                    error: runtime_error(&error),
                });
            }
        }
    } else {
        None
    };
    match state.start_prompt(command_id, prompt, thinking_level) {
        Ok(started) => {
            *active = Some(started);
            Ok(match created_snapshot {
                Some(snapshot) => DesktopRuntimeUpdate::PromptAcceptedWithSession {
                    command_id,
                    snapshot,
                },
                None => DesktopRuntimeUpdate::PromptAccepted { command_id },
            })
        }
        Err(error) => match created_snapshot {
            Some(snapshot) => Ok(DesktopRuntimeUpdate::PromptRejectedWithSession {
                command_id,
                metadata: state.metadata_snapshot(),
                snapshot: Some(Box::new(state.snapshot().unwrap_or(snapshot))),
                error: runtime_error(&error),
            }),
            None => Err(error),
        },
    }
}

pub(super) fn dispatch_active_command(
    active: &ActivePrompt,
    command: DesktopRuntimeCommand,
) -> DesktopRuntimeUpdate {
    let command_id = command.command_id();
    let kind = command.kind();
    if matches!(command, DesktopRuntimeCommand::Resync { .. }) {
        return match active.connection.state() {
            Ok(session) => DesktopRuntimeUpdate::Resynced {
                command_id,
                replacement: DesktopRuntimeResyncSnapshot::Metadata(
                    DesktopRuntimeMetadataSnapshot {
                        project: active.project.clone(),
                        session: Some(session),
                    },
                ),
            },
            Err(error) => {
                let error = runtime_error(&error);
                DesktopRuntimeUpdate::CommandRejected {
                    command_id,
                    command: kind,
                    code: error.code,
                    message: error.message,
                }
            }
        };
    }
    let Some(operation_id) = active.operation_id.as_deref() else {
        return DesktopRuntimeUpdate::CommandRejected {
            command_id,
            command: kind,
            code: "operation_starting".into(),
            message: "desktop prompt has not received its product operation identity yet".into(),
        };
    };
    if let DesktopRuntimeCommand::DecideToolAuthorization {
        identity, decision, ..
    } = command
    {
        return match active
            .connection
            .decide_tool_authorization(&identity, decision.clone())
        {
            Ok(()) => DesktopRuntimeUpdate::AuthorizationDecisionAccepted {
                command_id,
                authorization_id: identity.authorization_id,
                decision,
            },
            Err(error) => {
                let error = runtime_error(&error);
                DesktopRuntimeUpdate::CommandRejected {
                    command_id,
                    command: kind,
                    code: error.code,
                    message: error.message,
                }
            }
        };
    }
    let control = active.connection.prompt_control(operation_id);
    let control_id = CodingAgentControlId(format!("desktop-control-{command_id}"));
    let result = match command {
        DesktopRuntimeCommand::Abort { .. } => {
            control.abort(control_id, "desktop user requested abort")
        }
        DesktopRuntimeCommand::Steer { text, .. } => control.steer(control_id, text),
        DesktopRuntimeCommand::FollowUp { text, .. } => control.follow_up(control_id, text),
        DesktopRuntimeCommand::Reload { .. }
        | DesktopRuntimeCommand::Resync { .. }
        | DesktopRuntimeCommand::CreateSession { .. }
        | DesktopRuntimeCommand::OpenSession { .. }
        | DesktopRuntimeCommand::ListSessions { .. }
        | DesktopRuntimeCommand::SelectModel { .. }
        | DesktopRuntimeCommand::SelectSessionProfile { .. }
        | DesktopRuntimeCommand::SubmitPrompt { .. }
        | DesktopRuntimeCommand::DecideToolAuthorization { .. }
        | DesktopRuntimeCommand::RetryRecovery { .. }
        | DesktopRuntimeCommand::ResolveRecovery { .. }
        | DesktopRuntimeCommand::ReviewChangedFile { .. }
        | DesktopRuntimeCommand::OpenExternalEditor { .. } => {
            return DesktopRuntimeUpdate::CommandRejected {
                command_id,
                command: kind,
                code: "busy".into(),
                message: format!(
                    "desktop runtime is executing prompt operation {}",
                    operation_id
                ),
            };
        }
    };
    match result {
        Ok(receipt) => DesktopRuntimeUpdate::ControlAccepted {
            command_id,
            command: kind,
            receipt,
        },
        Err(rejection) => DesktopRuntimeUpdate::CommandRejected {
            command_id,
            command: kind,
            code: "control_rejected".into(),
            message: format!(
                "control {:?} for operation {} was rejected: {:?}",
                rejection.kind, rejection.operation_id, rejection.reason
            ),
        },
    }
}
