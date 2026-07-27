use super::*;
use desktop::runtime::DesktopRuntimeUpdate;

pub(super) enum DirectCommandUpdate {
    Continue(DesktopRuntimeUpdate),
    Consumed {
        sessions_dirty: bool,
        inspector_dirty: bool,
    },
}

pub(super) fn reconcile_direct_update(
    shell: &mut NativeShell,
    update: DesktopRuntimeUpdate,
    cx: &mut Context<NativeShell>,
) -> DirectCommandUpdate {
    match update {
        DesktopRuntimeUpdate::FileReviewed { command_id, review } => {
            let request = CodingAgentFileReviewRequest::new(review.change.clone(), review.revision);
            let inspector_dirty = shell.command_ledger.complete(
                command_id,
                &DesktopCommandIntent::FileReview {
                    request: request.clone(),
                },
            );
            if inspector_dirty {
                shell.file_review =
                    DesktopFileReviewState::Ready(DesktopFileReviewDocument::from_product(review));
                shell.preference_notice = Some("Changed-file review loaded.".into());
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
            let inspector_dirty = shell.command_ledger.complete(
                command_id,
                &DesktopCommandIntent::ExternalEditor {
                    project_relative_path: project_relative_path.clone(),
                },
            );
            if inspector_dirty {
                shell.preference_notice = Some(format!(
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
                .command_ledger
                .complete(command_id, &DesktopCommandIntent::ListSessions);
            if sessions_dirty {
                shell.session_catalog = sessions;
                shell.omitted_sessions = omitted;
                shell.preference_notice = Some(if omitted == 0 {
                    format!("Loaded {} session(s).", shell.session_catalog.len())
                } else {
                    format!(
                        "Loaded {} session(s); {omitted} older session(s) omitted.",
                        shell.session_catalog.len()
                    )
                });
                shell.schedule_session_catalog_refresh(cx);
            }
            DirectCommandUpdate::Consumed {
                sessions_dirty,
                inspector_dirty: false,
            }
        }
        update => DirectCommandUpdate::Continue(update),
    }
}

pub(super) struct ProjectionCommandCompletions {
    reload: Option<(u64, usize, usize, usize)>,
    selection: Option<(u64, DesktopRuntimeSelectionKind)>,
    recovery: Option<(u64, DesktopRecoveryAction, String)>,
    resync: Option<u64>,
    session: Option<(u64, DesktopCommandIntent)>,
}

impl ProjectionCommandCompletions {
    pub(super) fn capture(shell: &NativeShell, update: &DesktopRuntimeUpdate) -> Self {
        let reload = match update {
            DesktopRuntimeUpdate::Reloaded {
                command_id,
                metadata,
            } if shell
                .command_ledger
                .matches(*command_id, &DesktopCommandIntent::Reload) =>
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
                ..
            } if shell
                .command_ledger
                .matches(*command_id, &DesktopCommandIntent::Selection(*selection)) =>
            {
                Some((*command_id, *selection))
            }
            _ => None,
        };
        let recovery = match update {
            DesktopRuntimeUpdate::RecoveryChanged {
                command_id,
                action,
                recovery_id,
                ..
            } if shell.command_ledger.matches(
                *command_id,
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
                if shell
                    .command_ledger
                    .matches(*command_id, &DesktopCommandIntent::Resync) =>
            {
                Some(*command_id)
            }
            _ => None,
        };
        let session = match update {
            DesktopRuntimeUpdate::SessionChanged { command_id, .. } => shell
                .command_ledger
                .intent(*command_id)
                .filter(|intent| {
                    matches!(
                        intent,
                        DesktopCommandIntent::CreateSession
                            | DesktopCommandIntent::OpenSession { .. }
                    )
                })
                .cloned()
                .map(|intent| (*command_id, intent)),
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
        cx: &mut Context<NativeShell>,
    ) -> bool {
        let mut sessions_dirty = false;
        if let Some(command_id) = self.resync
            && shell
                .command_ledger
                .complete(command_id, &DesktopCommandIntent::Resync)
        {
            shell.preference_notice = Some(if projection_replaced {
                "Runtime state resynchronized.".into()
            } else {
                "Resync response failed projection validation.".into()
            });
            if projection_replaced {
                shell.request_session_catalog(cx);
            }
        }
        if let Some((command_id, intent)) = self.session
            && shell.command_ledger.complete(command_id, &intent)
        {
            sessions_dirty = true;
            shell.preference_notice = Some(if projection_replaced {
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
            if projection_replaced {
                shell.request_session_catalog(cx);
            }
        }
        if let Some((command_id, skill_count, prompt_count, profile_count)) = self.reload
            && shell
                .command_ledger
                .complete(command_id, &DesktopCommandIntent::Reload)
        {
            shell.preference_notice = Some(if projection_replaced {
                format!(
                    "Reloaded {skill_count} skills, {prompt_count} prompts, and \
                     {profile_count} profiles."
                )
            } else {
                "Reload response failed projection validation; resync is required.".into()
            });
        }
        if let Some((command_id, selection)) = self.selection
            && shell
                .command_ledger
                .complete(command_id, &DesktopCommandIntent::Selection(selection))
        {
            shell.preference_notice = Some(if projection_replaced {
                match selection {
                    DesktopRuntimeSelectionKind::Model => format!(
                        "Future prompts will use model {}.",
                        truncate_label(&shell.projection.project().selected_model_id, 28)
                    ),
                    DesktopRuntimeSelectionKind::SessionProfile => format!(
                        "Session profile changed to {}.",
                        truncate_label(
                            shell
                                .projection
                                .snapshot()
                                .session
                                .default_agent_profile_id
                                .as_str(),
                            28
                        )
                    ),
                }
            } else {
                "Selection response failed projection validation; resync is required.".into()
            });
        }
        if let Some((command_id, action, recovery_id)) = self.recovery
            && shell.command_ledger.complete(
                command_id,
                &DesktopCommandIntent::Recovery {
                    recovery_id: recovery_id.clone(),
                    action,
                },
            )
        {
            shell.preference_notice = Some(if projection_replaced {
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
    match shell.command_ledger.reserve(intent) {
        Ok(command_id) => Some(command_id),
        Err(error) => {
            shell.preference_notice = Some(error.to_string());
            None
        }
    }
}
