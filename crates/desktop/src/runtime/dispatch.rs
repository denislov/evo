use std::collections::HashMap;

use coding_agent::api::client::CodingAgentControlId;
use coding_agent::api::event::CodingAgentRecoveryResolution;

use super::driver::{ActivePrompt, RuntimeState, close_active_prompt, shutdown_active_prompt};
use super::protocol::{
    DesktopBridgeError, DesktopRecoveryAction, DesktopRuntimeCommand,
    DesktopRuntimeMetadataSnapshot, DesktopRuntimeResyncSnapshot, DesktopRuntimeSelectionKind,
    DesktopRuntimeUpdate, runtime_error,
};

#[cfg(test)]
pub(super) async fn dispatch_command(
    state: &mut RuntimeState,
    active: &mut HashMap<String, ActivePrompt>,
    command: DesktopRuntimeCommand,
) -> DesktopRuntimeUpdate {
    dispatch_command_with_updates(state, active, None, None, command).await
}

pub(super) async fn dispatch_command_with_updates(
    state: &mut RuntimeState,
    active: &mut HashMap<String, ActivePrompt>,
    priority_updates: Option<&tokio::sync::mpsc::Sender<DesktopRuntimeUpdate>>,
    data_updates: Option<&tokio::sync::mpsc::Sender<DesktopRuntimeUpdate>>,
    command: DesktopRuntimeCommand,
) -> DesktopRuntimeUpdate {
    let command_id = command.command_id();
    let kind = command.kind();
    let result =
        dispatch_command_inner(state, active, priority_updates, data_updates, command).await;
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

async fn dispatch_command_inner(
    state: &mut RuntimeState,
    active: &mut HashMap<String, ActivePrompt>,
    priority_updates: Option<&tokio::sync::mpsc::Sender<DesktopRuntimeUpdate>>,
    data_updates: Option<&tokio::sync::mpsc::Sender<DesktopRuntimeUpdate>>,
    command: DesktopRuntimeCommand,
) -> Result<DesktopRuntimeUpdate, DesktopBridgeError> {
    let command_id = command.command_id();
    match command {
        DesktopRuntimeCommand::Reload { .. } => {
            state.context.reload_local_resources()?;
            Ok(DesktopRuntimeUpdate::Reloaded {
                command_id,
                metadata: state.metadata_snapshot(None),
            })
        }
        DesktopRuntimeCommand::ListSessions { .. } => {
            let (sessions, omitted) = state.session_catalog()?;
            Ok(DesktopRuntimeUpdate::SessionsListed {
                command_id,
                sessions,
                omitted,
            })
        }
        DesktopRuntimeCommand::RenameSession {
            session_id, name, ..
        } => {
            let session_id = resolve_idle_target(state, active, Some(&session_id))?;
            let (name, updated_at) = state.rename_session(&session_id, name).await?;
            Ok(DesktopRuntimeUpdate::SessionRenamed {
                command_id,
                session_id,
                name,
                updated_at,
            })
        }
        DesktopRuntimeCommand::SelectModel { model_id, .. } => {
            state.context.select_model(model_id)?;
            Ok(DesktopRuntimeUpdate::SelectionChanged {
                command_id,
                selection: DesktopRuntimeSelectionKind::Model,
                metadata: state.metadata_snapshot(None),
            })
        }
        DesktopRuntimeCommand::CreateSession { .. } => {
            let session_id = state
                .create_session(open_session_count(state, active))
                .await?;
            let snapshot = state.snapshot(&session_id)?;
            Ok(DesktopRuntimeUpdate::SessionChanged {
                command_id,
                snapshot,
            })
        }
        DesktopRuntimeCommand::OpenSession { session_id, .. } => {
            if active.contains_key(&session_id) {
                return Err(DesktopBridgeError::Busy {
                    operation: format!("session {session_id} already has an active prompt"),
                });
            }
            let session_id = state
                .open_session(session_id, open_session_count(state, active))
                .await?;
            let snapshot = state.snapshot(&session_id)?;
            Ok(DesktopRuntimeUpdate::SessionChanged {
                command_id,
                snapshot,
            })
        }
        DesktopRuntimeCommand::CloseSession { session_id, .. } => {
            if let Some(prompt) = active.remove(&session_id) {
                match (priority_updates, data_updates) {
                    (Some(priority_updates), Some(data_updates)) => {
                        close_active_prompt(prompt, priority_updates, data_updates).await?;
                    }
                    _ => {
                        let (sender, receiver) = tokio::sync::mpsc::channel(1);
                        drop(receiver);
                        shutdown_active_prompt(Some(prompt), &sender).await;
                    }
                }
            } else {
                state.close_idle_session(&session_id).await?;
            }
            Ok(DesktopRuntimeUpdate::SessionClosed {
                command_id,
                session_id,
            })
        }
        DesktopRuntimeCommand::Resync { session_id, .. } => {
            let session_id = resolve_target(state, active, session_id.as_deref())?;
            if let Some(prompt) = active.get(&session_id) {
                let session = prompt.connection.state()?;
                Ok(DesktopRuntimeUpdate::Resynced {
                    command_id,
                    replacement: DesktopRuntimeResyncSnapshot::Metadata(
                        DesktopRuntimeMetadataSnapshot {
                            project: prompt.project.clone(),
                            session: Some(session),
                        },
                    ),
                })
            } else {
                Ok(DesktopRuntimeUpdate::Resynced {
                    command_id,
                    replacement: DesktopRuntimeResyncSnapshot::Hydrated(
                        state.snapshot(&session_id)?,
                    ),
                })
            }
        }
        DesktopRuntimeCommand::SubmitPrompt {
            session_id,
            prompt,
            attachments,
            thinking_level,
            ..
        } => {
            let created = if state.sessions.is_empty() && active.is_empty() && session_id.is_none()
            {
                Some(state.create_session(0).await?)
            } else {
                None
            };
            let session_id =
                resolve_target(state, active, session_id.as_deref().or(created.as_deref()))?;
            if active.contains_key(&session_id) {
                return Err(DesktopBridgeError::Busy {
                    operation: format!("session {session_id} already has an active prompt"),
                });
            }
            let created_snapshot = created
                .as_ref()
                .map(|session_id| state.snapshot(session_id))
                .transpose()?;
            match state.start_prompt(&session_id, command_id, prompt, attachments, thinking_level) {
                Ok(prompt) => {
                    active.insert(session_id, prompt);
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
                        metadata: state.metadata_snapshot(Some(&session_id)),
                        snapshot: Some(Box::new(state.snapshot(&session_id).unwrap_or(snapshot))),
                        error: runtime_error(&error),
                    }),
                    None => Err(error),
                },
            }
        }
        DesktopRuntimeCommand::SelectSessionProfile {
            session_id,
            profile_id,
            ..
        } => {
            let session_id = resolve_idle_target(state, active, session_id.as_deref())?;
            let metadata = state
                .select_session_profile(&session_id, profile_id)
                .await?;
            Ok(DesktopRuntimeUpdate::SelectionChanged {
                command_id,
                selection: DesktopRuntimeSelectionKind::SessionProfile,
                metadata,
            })
        }
        DesktopRuntimeCommand::RetryRecovery {
            session_id,
            identity,
            ..
        } => {
            let session_id = resolve_idle_target(state, active, session_id.as_deref())?;
            let (recovery_id, recovery) = state.retry_recovery(&session_id, identity)?;
            Ok(DesktopRuntimeUpdate::RecoveryChanged {
                command_id,
                action: DesktopRecoveryAction::Retry,
                recovery_id,
                recovery,
            })
        }
        DesktopRuntimeCommand::ResolveRecovery {
            session_id,
            identity,
            resolution,
            ..
        } => {
            let session_id = resolve_idle_target(state, active, session_id.as_deref())?;
            let action = match resolution {
                CodingAgentRecoveryResolution::Failed => DesktopRecoveryAction::MarkFailed,
                CodingAgentRecoveryResolution::Aborted => DesktopRecoveryAction::Abort,
            };
            let (recovery_id, recovery) =
                state.resolve_recovery(&session_id, identity, resolution)?;
            Ok(DesktopRuntimeUpdate::RecoveryChanged {
                command_id,
                action,
                recovery_id,
                recovery,
            })
        }
        DesktopRuntimeCommand::ReviewChangedFile {
            session_id,
            request,
            ..
        } => {
            let session_id = resolve_idle_target(state, active, session_id.as_deref())?;
            let review = state.review_changed_file(&session_id, request).await?;
            Ok(DesktopRuntimeUpdate::FileReviewed { command_id, review })
        }
        DesktopRuntimeCommand::OpenExternalEditor {
            session_id,
            target,
            editor,
            ..
        } => {
            let session_id = resolve_idle_target(state, active, session_id.as_deref())?;
            let project_relative_path = state
                .open_external_editor(&session_id, target, editor)
                .await?;
            Ok(DesktopRuntimeUpdate::ExternalEditorOpened {
                command_id,
                project_relative_path,
            })
        }
        command @ (DesktopRuntimeCommand::Abort { .. }
        | DesktopRuntimeCommand::Steer { .. }
        | DesktopRuntimeCommand::FollowUp { .. }
        | DesktopRuntimeCommand::DecideToolAuthorization { .. }) => {
            let requested = command.target_session_id();
            let session_id = resolve_active_target(state, active, requested)?;
            Ok(dispatch_active_command(
                active
                    .get(&session_id)
                    .expect("resolved active session must remain present"),
                command,
            ))
        }
    }
}

fn open_session_count(state: &RuntimeState, active: &HashMap<String, ActivePrompt>) -> usize {
    state.sessions.len() + active.len()
}

fn resolve_target(
    state: &RuntimeState,
    active: &HashMap<String, ActivePrompt>,
    requested: Option<&str>,
) -> Result<String, DesktopBridgeError> {
    if let Some(session_id) = requested {
        if state.sessions.contains_key(session_id) || active.contains_key(session_id) {
            return Ok(session_id.to_owned());
        }
        return Err(DesktopBridgeError::SessionTarget {
            message: format!("session {session_id} is not open"),
        });
    }
    if let Some(session_id) = state.focused_session_id.as_deref()
        && (state.sessions.contains_key(session_id) || active.contains_key(session_id))
    {
        return Ok(session_id.to_owned());
    }
    let mut session_ids = state.sessions.keys().chain(active.keys()).take(2).cloned();
    let Some(session_id) = session_ids.next() else {
        return Err(DesktopBridgeError::Session {
            message: "desktop runtime has no idle session owner".into(),
        });
    };
    if session_ids.next().is_some() {
        return Err(DesktopBridgeError::SessionTarget {
            message: "more than one desktop session is open; a target session id is required"
                .into(),
        });
    }
    Ok(session_id)
}

fn resolve_idle_target(
    state: &RuntimeState,
    active: &HashMap<String, ActivePrompt>,
    requested: Option<&str>,
) -> Result<String, DesktopBridgeError> {
    let session_id = resolve_target(state, active, requested)?;
    if active.contains_key(&session_id) {
        return Err(DesktopBridgeError::Busy {
            operation: format!("session {session_id} has an active prompt"),
        });
    }
    Ok(session_id)
}

fn resolve_active_target(
    state: &RuntimeState,
    active: &HashMap<String, ActivePrompt>,
    requested: Option<&str>,
) -> Result<String, DesktopBridgeError> {
    let session_id = resolve_target(state, active, requested)?;
    if !active.contains_key(&session_id) {
        return Err(DesktopBridgeError::Busy {
            operation: "no_active_prompt".into(),
        });
    }
    Ok(session_id)
}

pub(super) fn dispatch_active_command(
    active: &ActivePrompt,
    command: DesktopRuntimeCommand,
) -> DesktopRuntimeUpdate {
    let command_id = command.command_id();
    let kind = command.kind();
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
        _ => {
            return DesktopRuntimeUpdate::CommandRejected {
                command_id,
                command: kind,
                code: "busy".into(),
                message: format!(
                    "desktop session {} is executing prompt operation {}",
                    active.session_id, operation_id
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
