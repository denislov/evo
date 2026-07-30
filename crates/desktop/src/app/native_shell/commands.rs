use std::sync::Arc;

use coding_agent::api::{
    embedding::CodingAgentThinkingLevel, review::CodingAgentFileReviewRequest,
};
use desktop::{
    file_review::DesktopFileReviewDocument,
    runtime::{DesktopRecoveryAction, DesktopRuntimeSelectionKind, DesktopRuntimeUpdate},
    shell::truncate_label,
};
use gpui::Context;

use super::{DesktopFileReviewState, DesktopThinkingLevel, NativeShell, recovery_action_label};
use crate::application::commands::DesktopCommandIntent;
use crate::application::workspace::{SessionId, WorkspaceKey};

pub(super) enum DirectCommandUpdate {
    Continue(Box<DesktopRuntimeUpdate>),
    Consumed {
        sessions_dirty: bool,
        inspector_dirty: bool,
    },
}

pub(super) fn reconcile_direct_update(
    shell: &mut NativeShell,
    update: DesktopRuntimeUpdate,
    _cx: &mut Context<NativeShell>,
) -> DirectCommandUpdate {
    match update {
        DesktopRuntimeUpdate::SessionClosed {
            command_id,
            session_id,
        } => {
            let intent = DesktopCommandIntent::CloseSession {
                session_id: session_id.clone(),
            };
            let owner = WorkspaceKey::Session(SessionId::from_dto(session_id.clone()));
            let sessions_dirty = shell.complete_command(command_id, &owner, &intent);
            if sessions_dirty {
                let cancelled = shell.remove_closed_workspace(&session_id);
                shell.project_catalog.remove_session(&session_id);
                shell
                    .update_workspace_mut()
                    .set_preference_notice(if cancelled == 0 {
                        "Session closed.".into()
                    } else {
                        format!("Session closed; {cancelled} pending command(s) cancelled.")
                    });
            }
            DirectCommandUpdate::Consumed {
                sessions_dirty,
                inspector_dirty: sessions_dirty,
            }
        }
        DesktopRuntimeUpdate::FileReviewed { command_id, review } => {
            let request = CodingAgentFileReviewRequest::new(review.change.clone(), review.revision);
            let owner = shell
                .command_tracker
                .owner(command_id)
                .cloned()
                .unwrap_or_else(|| shell.update_workspace_key());
            let inspector_dirty = shell.complete_command(
                command_id,
                &owner,
                &DesktopCommandIntent::FileReview {
                    request: request.clone(),
                },
            );
            if inspector_dirty {
                shell.update_workspace_mut().file_review = Arc::new(DesktopFileReviewState::Ready(
                    DesktopFileReviewDocument::from_product(review),
                ));
                shell
                    .update_workspace_mut()
                    .set_preference_notice("Changed-file review loaded.".into());
            }
            DirectCommandUpdate::Consumed {
                sessions_dirty: false,
                inspector_dirty,
            }
        }
        DesktopRuntimeUpdate::ExternalEditorOpened {
            command_id,
            project_relative_path,
        } => {
            let owner = shell
                .command_tracker
                .owner(command_id)
                .cloned()
                .unwrap_or_else(|| shell.update_workspace_key());
            let inspector_dirty = shell.complete_command(
                command_id,
                &owner,
                &DesktopCommandIntent::ExternalEditor {
                    project_relative_path: project_relative_path.clone(),
                },
            );
            if inspector_dirty {
                shell.update_workspace_mut().set_preference_notice(format!(
                    "Opened {} in the configured editor.",
                    truncate_label(&project_relative_path, 48)
                ));
            }
            DirectCommandUpdate::Consumed {
                sessions_dirty: false,
                inspector_dirty,
            }
        }
        DesktopRuntimeUpdate::SessionsListed {
            command_id,
            sessions,
            omitted,
        } => {
            let sessions_dirty = shell
                .command_tracker
                .owner(command_id)
                .cloned()
                .is_some_and(|owner| {
                    shell.complete_command(command_id, &owner, &DesktopCommandIntent::ListSessions)
                });
            if sessions_dirty {
                shell.project_catalog.replace_catalog(sessions, omitted);
            }
            DirectCommandUpdate::Consumed {
                sessions_dirty,
                inspector_dirty: false,
            }
        }
        DesktopRuntimeUpdate::SessionRenamed {
            command_id,
            session_id,
            name,
            updated_at,
        } => {
            let intent = DesktopCommandIntent::RenameSession {
                session_id: session_id.clone(),
            };
            let owner = WorkspaceKey::Session(SessionId::from_dto(session_id.clone()));
            let sessions_dirty = shell.complete_command(command_id, &owner, &intent);
            if sessions_dirty {
                shell
                    .project_catalog
                    .rename_session(&session_id, name, updated_at);
                shell
                    .update_workspace_mut()
                    .set_preference_notice("Session name updated.".into());
            }
            DirectCommandUpdate::Consumed {
                sessions_dirty,
                inspector_dirty: false,
            }
        }
        DesktopRuntimeUpdate::SessionNameObserved {
            session_id,
            name,
            updated_at,
        } => DirectCommandUpdate::Consumed {
            sessions_dirty: shell
                .project_catalog
                .rename_session(&session_id, name, updated_at),
            inspector_dirty: false,
        },
        update => DirectCommandUpdate::Continue(Box::new(update)),
    }
}

pub(super) struct ProjectionCommandCompletions {
    reload: Option<(u64, usize, usize, usize)>,
    selection: Option<(
        u64,
        DesktopRuntimeSelectionKind,
        Option<CodingAgentThinkingLevel>,
        bool,
    )>,
    recovery: Option<(u64, DesktopRecoveryAction, String)>,
    resync: Option<u64>,
    session: Option<(WorkspaceKey, u64, DesktopCommandIntent)>,
}

impl ProjectionCommandCompletions {
    pub(super) fn capture(shell: &NativeShell, update: &DesktopRuntimeUpdate) -> Self {
        let owner = shell.update_workspace_key();
        let reload = match update {
            DesktopRuntimeUpdate::Reloaded {
                command_id,
                metadata,
            } if shell.command_tracker.matches(
                *command_id,
                &owner,
                &DesktopCommandIntent::Reload,
            ) =>
            {
                Some((
                    *command_id,
                    metadata.project.resources.skill_names.len(),
                    metadata.project.resources.prompt_template_names.len(),
                    metadata.project.profiles.len(),
                ))
            }
            _ => None,
        };
        let selection = match update {
            DesktopRuntimeUpdate::SelectionChanged {
                command_id,
                selection,
                thinking_level,
                thinking_fallback,
                ..
            } if shell.command_tracker.matches(
                *command_id,
                &owner,
                &DesktopCommandIntent::Selection(*selection),
            ) =>
            {
                Some((*command_id, *selection, *thinking_level, *thinking_fallback))
            }
            _ => None,
        };
        let recovery = match update {
            DesktopRuntimeUpdate::RecoveryChanged {
                command_id,
                action,
                recovery_id,
                ..
            } if shell.command_tracker.matches(
                *command_id,
                &owner,
                &DesktopCommandIntent::Recovery {
                    recovery_id: recovery_id.clone(),
                    action: *action,
                },
            ) =>
            {
                Some((*command_id, *action, recovery_id.clone()))
            }
            _ => None,
        };
        let resync = match update {
            DesktopRuntimeUpdate::Resynced { command_id, .. }
                if shell.command_tracker.matches(
                    *command_id,
                    &owner,
                    &DesktopCommandIntent::Resync,
                ) =>
            {
                Some(*command_id)
            }
            _ => None,
        };
        let session = match update {
            DesktopRuntimeUpdate::SessionChanged { command_id, .. } => shell
                .command_tracker
                .intent(*command_id)
                .filter(|intent| {
                    matches!(
                        intent,
                        DesktopCommandIntent::CreateSession
                            | DesktopCommandIntent::OpenSession { .. }
                    )
                })
                .cloned()
                .and_then(|intent| {
                    shell
                        .command_tracker
                        .owner(*command_id)
                        .cloned()
                        .map(|owner| (owner, *command_id, intent))
                }),
            _ => None,
        };
        Self {
            reload,
            selection,
            recovery,
            resync,
            session,
        }
    }

    pub(super) fn reconcile(
        self,
        shell: &mut NativeShell,
        projection_replaced: bool,
        _cx: &mut Context<NativeShell>,
    ) -> bool {
        let mut sessions_dirty = false;
        let update_owner = shell.update_workspace_key();
        if let Some(command_id) = self.resync
            && shell.complete_command(command_id, &update_owner, &DesktopCommandIntent::Resync)
        {
            shell
                .update_workspace_mut()
                .set_preference_notice(if projection_replaced {
                    "Runtime state resynchronized.".into()
                } else {
                    "Resync response failed projection validation.".into()
                });
        }
        if let Some((owner, command_id, intent)) = self.session {
            let owner = shell
                .command_tracker
                .owner(command_id)
                .cloned()
                .unwrap_or(owner);
            if shell.complete_command(command_id, &owner, &intent) {
                let created = matches!(&intent, DesktopCommandIntent::CreateSession);
                sessions_dirty = true;
                shell
                    .update_workspace_mut()
                    .set_preference_notice(if projection_replaced {
                        match intent {
                            DesktopCommandIntent::CreateSession => "Created a new session.".into(),
                            DesktopCommandIntent::OpenSession { .. } => {
                                "Opened the requested session.".into()
                            }
                            _ => unreachable!("session completion was filtered by typed intent"),
                        }
                    } else {
                        "Session response failed projection validation; resync is required.".into()
                    });
                if projection_replaced && created {
                    sessions_dirty |= shell.insert_active_session_into_catalog();
                }
            }
        }
        if let Some((command_id, skill_count, prompt_count, profile_count)) = self.reload
            && shell.complete_command(command_id, &update_owner, &DesktopCommandIntent::Reload)
        {
            shell
                .update_workspace_mut()
                .set_preference_notice(if projection_replaced {
                    format!(
                        "Reloaded {skill_count} skills, {prompt_count} prompts, and \
                     {profile_count} profiles."
                    )
                } else {
                    "Reload response failed projection validation; resync is required.".into()
                });
        }
        if let Some((command_id, selection, thinking_level, thinking_fallback)) = self.selection
            && shell.complete_command(
                command_id,
                &update_owner,
                &DesktopCommandIntent::Selection(selection),
            )
        {
            if projection_replaced && selection == DesktopRuntimeSelectionKind::Model {
                let thinking_selection = DesktopThinkingLevel::from_explicit(thinking_level);
                shell.update_workspace_mut().thinking_selection = thinking_selection;
                shell.update_workspace_mut().thinking_hint = thinking_fallback
                    .then(|| Arc::from("Thinking reset to Auto for the selected model."));
                let session_id = shell
                    .update_workspace_mut()
                    .projection
                    .as_ref()
                    .map(|projection| projection.snapshot().session.session_id.clone());
                if let Some(session_id) = session_id.as_deref() {
                    shell.remember_thinking_selection(session_id, thinking_selection);
                }
            }
            let notice = if projection_replaced {
                match selection {
                    DesktopRuntimeSelectionKind::Model => format!(
                        "Future prompts will use model {}.",
                        truncate_label(&shell.update_workspace_mut().project.selected_model_id, 28)
                    ),
                    DesktopRuntimeSelectionKind::SessionProfile => format!(
                        "Session profile changed to {}.",
                        truncate_label(
                            {
                                let workspace = shell.update_workspace();
                                workspace
                                    .projection
                                    .as_ref()
                                    .map(|projection| {
                                        projection
                                            .snapshot()
                                            .session
                                            .default_agent_profile_id
                                            .as_str()
                                    })
                                    .unwrap_or(workspace.project.default_agent_profile_id.as_str())
                            },
                            28
                        )
                    ),
                }
            } else {
                "Selection response failed projection validation; resync is required.".into()
            };
            shell.update_workspace_mut().set_preference_notice(notice);
        }
        if let Some((command_id, action, recovery_id)) = self.recovery
            && shell.complete_command(
                command_id,
                &update_owner,
                &DesktopCommandIntent::Recovery {
                    recovery_id: recovery_id.clone(),
                    action,
                },
            )
        {
            shell
                .update_workspace_mut()
                .set_preference_notice(if projection_replaced {
                    format!(
                        "Recovery {} accepted for {}.",
                        recovery_action_label(action),
                        truncate_label(&recovery_id, 28)
                    )
                } else {
                    "Recovery changed, but its snapshot failed projection validation; resync is \
                 required."
                        .into()
                });
        }
        sessions_dirty
    }
}

pub(super) fn reserve_command(
    shell: &mut NativeShell,
    intent: DesktopCommandIntent,
) -> Option<u64> {
    let owner = shell.update_workspace_key();
    match shell.command_tracker.reserve(owner, intent) {
        Ok(command_id) => Some(command_id),
        Err(error) => {
            shell
                .update_workspace_mut()
                .set_preference_notice(error.to_string());
            None
        }
    }
}
