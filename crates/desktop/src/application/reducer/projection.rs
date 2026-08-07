//! Product-projection reduction: event projection, completion reconciliation,
//! and pre-projection state reduction.

use coding_agent::api::authorization::ToolAuthorizationDecision;
use coding_agent::api::embedding::CodingAgentThinkingLevel;
use desktop::runtime::{
    DesktopRecoveryAction, DesktopRuntimeCommandKind, DesktopRuntimeError,
    DesktopRuntimeResyncSnapshot, DesktopRuntimeSelectionKind, DesktopRuntimeUpdate,
};
use desktop::ui::shell::truncate_label;

use super::{recovery_action_label, safe_runtime_rejection_notice};
use crate::application::{
    catalog::ProjectCatalogController,
    change_set::{UiChangeSet, UiRegion},
    commands::DesktopCommandIntent,
    runtime_state::{ProjectionUpdateResult, RuntimeWorkspacePresentation},
    state::DesktopState,
    workspace::WorkspaceKey,
    workspace_state::{RuntimeWorkspaceDefaults, WorkspaceState},
};
use crate::projection::ProjectionEvent;

pub(crate) enum ProjectionCompletion {
    None,
    Reload {
        command_id: u64,
        skill_count: usize,
        prompt_count: usize,
        profile_count: usize,
    },
    Selection {
        command_id: u64,
        selection: DesktopRuntimeSelectionKind,
        thinking_level: Option<CodingAgentThinkingLevel>,
        thinking_fallback: bool,
    },
    Recovery {
        command_id: u64,
        action: DesktopRecoveryAction,
        recovery_id: String,
    },
    Resync {
        command_id: u64,
    },
    Session {
        owner: WorkspaceKey,
        command_id: u64,
        intent: DesktopCommandIntent,
    },
}
pub(crate) fn projection_event(update: DesktopRuntimeUpdate) -> Option<ProjectionEvent> {
    match update {
        DesktopRuntimeUpdate::Reloaded { metadata, .. }
        | DesktopRuntimeUpdate::SelectionChanged { metadata, .. } => {
            Some(ProjectionEvent::Metadata(metadata))
        }
        DesktopRuntimeUpdate::RecoveryChanged { recovery, .. } => {
            Some(ProjectionEvent::Recovery(recovery))
        }
        DesktopRuntimeUpdate::Resynced { replacement, .. } => Some(match replacement {
            DesktopRuntimeResyncSnapshot::Metadata(metadata) => ProjectionEvent::Metadata(metadata),
            DesktopRuntimeResyncSnapshot::Hydrated(snapshot) => ProjectionEvent::Hydrated {
                snapshot,
                allow_session_change: false,
                issue: None,
            },
        }),
        DesktopRuntimeUpdate::SessionChanged { snapshot, .. }
        | DesktopRuntimeUpdate::PromptAcceptedWithSession { snapshot, .. } => {
            Some(ProjectionEvent::Hydrated {
                snapshot,
                allow_session_change: true,
                issue: None,
            })
        }
        DesktopRuntimeUpdate::PromptRejectedWithSession {
            snapshot, error, ..
        } => Some(ProjectionEvent::Hydrated {
            snapshot,
            allow_session_change: true,
            issue: Some(error),
        }),
        DesktopRuntimeUpdate::PromptStarted {
            operation_id,
            metadata,
            ..
        } => Some(ProjectionEvent::PromptStarted {
            operation_id,
            metadata,
        }),
        DesktopRuntimeUpdate::PromptFinished { snapshot, .. } => Some(ProjectionEvent::Hydrated {
            snapshot,
            allow_session_change: false,
            issue: None,
        }),
        DesktopRuntimeUpdate::ProductEvent { event, .. } => Some(ProjectionEvent::Product(event)),
        DesktopRuntimeUpdate::ResyncRequired { reason, snapshot } => {
            Some(ProjectionEvent::ProductSnapshot { reason, snapshot })
        }
        DesktopRuntimeUpdate::CommandRejected { code, message, .. } => {
            Some(ProjectionEvent::Issue(DesktopRuntimeError {
                code,
                message,
            }))
        }
        DesktopRuntimeUpdate::RuntimeFailed { error } => {
            Some(ProjectionEvent::RuntimeFailed(error))
        }
        DesktopRuntimeUpdate::Stopped => Some(ProjectionEvent::Stopped),
        DesktopRuntimeUpdate::SessionClosed { .. }
        | DesktopRuntimeUpdate::SessionDeleted { .. }
        | DesktopRuntimeUpdate::SessionsListed { .. }
        | DesktopRuntimeUpdate::SessionRenamed { .. }
        | DesktopRuntimeUpdate::SessionNameObserved { .. }
        | DesktopRuntimeUpdate::PromptAccepted { .. }
        | DesktopRuntimeUpdate::ControlAccepted { .. }
        | DesktopRuntimeUpdate::AuthorizationDecisionAccepted { .. }
        | DesktopRuntimeUpdate::FileReviewed { .. }
        | DesktopRuntimeUpdate::MergeProposalsListed { .. }
        | DesktopRuntimeUpdate::ChildWorktreeMerged { .. }
        | DesktopRuntimeUpdate::ChildWorktreeDiscarded { .. }
        | DesktopRuntimeUpdate::ExternalEditorTargetValidated { .. } => None,
    }
}

pub(crate) fn capture_projection_completion<Presentation: RuntimeWorkspacePresentation>(
    state: &DesktopState<
        WorkspaceState<Presentation>,
        ProjectCatalogController,
        RuntimeWorkspaceDefaults,
    >,
    owner: &WorkspaceKey,
    update: &DesktopRuntimeUpdate,
) -> ProjectionCompletion {
    match update {
        DesktopRuntimeUpdate::Reloaded {
            command_id,
            metadata,
        } if state
            .commands
            .matches(*command_id, owner, &DesktopCommandIntent::Reload) =>
        {
            ProjectionCompletion::Reload {
                command_id: *command_id,
                skill_count: metadata.project.resources.skill_names.len(),
                prompt_count: metadata.project.resources.prompt_template_names.len(),
                profile_count: metadata.project.profiles.len(),
            }
        }
        DesktopRuntimeUpdate::SelectionChanged {
            command_id,
            selection,
            thinking_level,
            thinking_fallback,
            ..
        } if state.commands.matches(
            *command_id,
            owner,
            &DesktopCommandIntent::Selection(*selection),
        ) =>
        {
            ProjectionCompletion::Selection {
                command_id: *command_id,
                selection: *selection,
                thinking_level: *thinking_level,
                thinking_fallback: *thinking_fallback,
            }
        }
        DesktopRuntimeUpdate::RecoveryChanged {
            command_id,
            action,
            recovery_id,
            ..
        } if state.commands.matches(
            *command_id,
            owner,
            &DesktopCommandIntent::Recovery {
                recovery_id: recovery_id.clone(),
                action: *action,
            },
        ) =>
        {
            ProjectionCompletion::Recovery {
                command_id: *command_id,
                action: *action,
                recovery_id: recovery_id.clone(),
            }
        }
        DesktopRuntimeUpdate::Resynced { command_id, .. }
            if state
                .commands
                .matches(*command_id, owner, &DesktopCommandIntent::Resync) =>
        {
            ProjectionCompletion::Resync {
                command_id: *command_id,
            }
        }
        DesktopRuntimeUpdate::SessionChanged { command_id, .. } => state
            .commands
            .intent(*command_id)
            .cloned()
            .filter(|intent| {
                matches!(
                    intent,
                    DesktopCommandIntent::CreateSession | DesktopCommandIntent::OpenSession { .. }
                )
            })
            .and_then(|intent| {
                state.commands.owner(*command_id).cloned().map(|owner| {
                    ProjectionCompletion::Session {
                        owner,
                        command_id: *command_id,
                        intent,
                    }
                })
            })
            .unwrap_or(ProjectionCompletion::None),
        _ => ProjectionCompletion::None,
    }
}

pub(crate) fn reduce_pre_projection_update<Presentation: RuntimeWorkspacePresentation>(
    state: &mut DesktopState<
        WorkspaceState<Presentation>,
        ProjectCatalogController,
        RuntimeWorkspaceDefaults,
    >,
    target: &WorkspaceKey,
    completion_owner: &WorkspaceKey,
    update: &DesktopRuntimeUpdate,
    changes: &mut UiChangeSet,
) {
    match update {
        DesktopRuntimeUpdate::PromptAccepted { command_id }
        | DesktopRuntimeUpdate::PromptAcceptedWithSession { command_id, .. } => {
            if state.complete_runtime_command(
                *command_id,
                completion_owner,
                &DesktopCommandIntent::Prompt,
            ) && state.accept_composer(target, *command_id)
            {
                changes.insert(UiRegion::Sessions);
            }
        }
        DesktopRuntimeUpdate::PromptRejectedWithSession {
            command_id, error, ..
        } => {
            if state
                .reject_runtime_command(
                    *command_id,
                    completion_owner,
                    DesktopRuntimeCommandKind::SubmitPrompt,
                )
                .is_some()
            {
                state.reject_composer(
                    target,
                    *command_id,
                    safe_runtime_rejection_notice(
                        DesktopRuntimeCommandKind::SubmitPrompt,
                        &error.code,
                    ),
                );
                changes.insert(UiRegion::Sessions);
            }
        }
        DesktopRuntimeUpdate::PromptStarted { command_id, .. } => {
            if state
                .submitted_composer_command(target)
                .is_some_and(|submitted| submitted != *command_id)
            {
                state.set_runtime_notice(
                    target,
                    "Prompt start did not match the submitted command.".into(),
                );
            }
        }
        DesktopRuntimeUpdate::ControlAccepted {
            command_id,
            command: DesktopRuntimeCommandKind::Abort,
            receipt,
        } => {
            let intent = DesktopCommandIntent::Abort {
                operation_id: receipt.operation_id.clone(),
            };
            if state.complete_runtime_command(*command_id, completion_owner, &intent) {
                state.set_runtime_notice(
                    target,
                    format!("Abort accepted for {}.", receipt.operation_id),
                );
            }
        }
        DesktopRuntimeUpdate::ControlAccepted {
            command_id,
            command:
                command @ (DesktopRuntimeCommandKind::Steer | DesktopRuntimeCommandKind::FollowUp),
            receipt,
        } => {
            let intent = match command {
                DesktopRuntimeCommandKind::Steer => DesktopCommandIntent::Steer,
                DesktopRuntimeCommandKind::FollowUp => DesktopCommandIntent::FollowUp,
                _ => unreachable!("match admits only active controls"),
            };
            if state.complete_runtime_command(*command_id, completion_owner, &intent)
                && state.accept_composer(target, *command_id)
            {
                state.set_runtime_notice(
                    target,
                    format!("{command:?} accepted for {}.", receipt.operation_id),
                );
            }
        }
        DesktopRuntimeUpdate::AuthorizationDecisionAccepted {
            command_id,
            authorization_id,
            decision,
        } => {
            let intent = state
                .commands
                .intent(*command_id)
                .cloned()
                .filter(|intent| {
                    matches!(
                        intent,
                        DesktopCommandIntent::Authorization {
                            authorization_id: pending,
                            ..
                        } if pending == authorization_id
                    )
                });
            if let Some(intent) = intent
                && state.complete_runtime_command(*command_id, completion_owner, &intent)
            {
                let decision = match decision {
                    ToolAuthorizationDecision::AllowOnce => "allow once",
                    ToolAuthorizationDecision::AllowForOperation => "allow for operation",
                    ToolAuthorizationDecision::Deny { .. } => "deny",
                };
                state.set_runtime_notice(
                    target,
                    format!("Authorization decision accepted: {decision}."),
                );
            }
        }
        DesktopRuntimeUpdate::CommandRejected {
            command_id,
            command,
            code,
            ..
        } => reduce_command_rejected(
            state,
            target,
            completion_owner,
            *command_id,
            *command,
            code,
            changes,
        ),
        DesktopRuntimeUpdate::PromptFinished {
            command_id,
            operation_id,
            error,
            ..
        } => {
            changes.insert(UiRegion::Sessions);
            let _ = state.complete_runtime_command(
                *command_id,
                completion_owner,
                &DesktopCommandIntent::Prompt,
            );
            state.complete_operation_commands(completion_owner, operation_id);
            if let Some(error) = error {
                state.set_runtime_notice(
                    target,
                    format!(
                        "Prompt finished with runtime error ({}).",
                        truncate_label(&error.code, 28)
                    ),
                );
            }
        }
        DesktopRuntimeUpdate::RuntimeFailed { error } => {
            changes.insert(UiRegion::Sessions);
            let message = format!(
                "desktop runtime failed ({})",
                truncate_label(&error.code, 28)
            );
            if state.catalog.state().is_loading() {
                state.catalog.fail_refresh(message.clone());
            }
            state.commands.cancel_all();
            state.reject_pending_composer(target, message);
        }
        DesktopRuntimeUpdate::Stopped => {
            changes.insert(UiRegion::Sessions);
            let message = "desktop runtime stopped".to_owned();
            if state.catalog.state().is_loading() {
                state.catalog.fail_refresh(message.clone());
            }
            state.commands.cancel_all();
            state.reject_pending_composer(target, message);
        }
        DesktopRuntimeUpdate::Reloaded { .. }
        | DesktopRuntimeUpdate::Resynced { .. }
        | DesktopRuntimeUpdate::SessionChanged { .. }
        | DesktopRuntimeUpdate::SessionClosed { .. }
        | DesktopRuntimeUpdate::SessionDeleted { .. }
        | DesktopRuntimeUpdate::SessionsListed { .. }
        | DesktopRuntimeUpdate::SessionRenamed { .. }
        | DesktopRuntimeUpdate::SessionNameObserved { .. }
        | DesktopRuntimeUpdate::SelectionChanged { .. }
        | DesktopRuntimeUpdate::ProductEvent { .. }
        | DesktopRuntimeUpdate::ResyncRequired { .. }
        | DesktopRuntimeUpdate::RecoveryChanged { .. }
        | DesktopRuntimeUpdate::FileReviewed { .. }
        | DesktopRuntimeUpdate::MergeProposalsListed { .. }
        | DesktopRuntimeUpdate::ChildWorktreeMerged { .. }
        | DesktopRuntimeUpdate::ChildWorktreeDiscarded { .. }
        | DesktopRuntimeUpdate::ExternalEditorTargetValidated { .. }
        | DesktopRuntimeUpdate::ControlAccepted { .. } => {}
    }
}

pub(crate) fn reduce_command_rejected<Presentation: RuntimeWorkspacePresentation>(
    state: &mut DesktopState<
        WorkspaceState<Presentation>,
        ProjectCatalogController,
        RuntimeWorkspaceDefaults,
    >,
    target: &WorkspaceKey,
    completion_owner: &WorkspaceKey,
    command_id: u64,
    command: DesktopRuntimeCommandKind,
    code: &str,
    changes: &mut UiChangeSet,
) {
    if command == DesktopRuntimeCommandKind::RenameSession {
        return;
    }
    let rejected = state.reject_runtime_command(command_id, completion_owner, command);
    let Some(intent) = rejected else {
        return;
    };
    match command {
        DesktopRuntimeCommandKind::SubmitPrompt => {
            state.reject_composer(
                target,
                command_id,
                safe_runtime_rejection_notice(command, code),
            );
            changes.insert(UiRegion::Sessions);
        }
        DesktopRuntimeCommandKind::Abort => {
            state.set_runtime_notice(target, safe_runtime_rejection_notice(command, code));
        }
        DesktopRuntimeCommandKind::Reload => state.set_runtime_notice(
            target,
            format!(
                "Reload failed ({}); previous context retained.",
                truncate_label(code, 28)
            ),
        ),
        DesktopRuntimeCommandKind::SelectModel
        | DesktopRuntimeCommandKind::SelectSessionProfile => state.set_runtime_notice(
            target,
            format!(
                "{command:?} failed ({}); previous selection retained.",
                truncate_label(code, 28)
            ),
        ),
        DesktopRuntimeCommandKind::Steer | DesktopRuntimeCommandKind::FollowUp => {
            let notice = safe_runtime_rejection_notice(command, code);
            if state.reject_composer(target, command_id, notice.clone()) {
                state.set_runtime_notice(target, notice);
            }
        }
        DesktopRuntimeCommandKind::DecideToolAuthorization => {
            state.set_runtime_notice(target, safe_runtime_rejection_notice(command, code));
        }
        DesktopRuntimeCommandKind::RetryRecovery | DesktopRuntimeCommandKind::ResolveRecovery => {
            state.set_runtime_notice(target, safe_runtime_rejection_notice(command, code));
            changes.insert(UiRegion::Inspector);
        }
        DesktopRuntimeCommandKind::Resync
        | DesktopRuntimeCommandKind::CreateSession
        | DesktopRuntimeCommandKind::OpenSession
        | DesktopRuntimeCommandKind::CloseSession
        | DesktopRuntimeCommandKind::DeleteSession => {
            state.set_runtime_notice(target, safe_runtime_rejection_notice(command, code));
            changes.insert(UiRegion::Sessions);
        }
        DesktopRuntimeCommandKind::ListSessions => {
            let notice = safe_runtime_rejection_notice(command, code);
            state.catalog.fail_refresh(notice.clone());
            state.set_runtime_notice(target, notice);
            changes.insert(UiRegion::Sessions);
        }
        DesktopRuntimeCommandKind::OpenChange => {
            if let DesktopCommandIntent::FileReview { request } = intent {
                state.set_file_review_failed(target, request, code.to_owned());
                state.set_runtime_notice(
                    target,
                    format!("File review unavailable ({}).", truncate_label(code, 32)),
                );
                changes.insert(UiRegion::Inspector);
            }
        }
        DesktopRuntimeCommandKind::ListMergeProposals
        | DesktopRuntimeCommandKind::MergeChildWorktree
        | DesktopRuntimeCommandKind::DiscardChildWorktree => {
            state.set_runtime_notice(
                target,
                format!(
                    "Merge proposal operation failed ({}).",
                    truncate_label(code, 32)
                ),
            );
            changes.insert(UiRegion::Inspector);
        }
        DesktopRuntimeCommandKind::OpenExternalEditor => {
            state.set_runtime_notice(
                target,
                format!(
                    "External editor unavailable ({}).",
                    truncate_label(code, 32)
                ),
            );
            changes.insert(UiRegion::Inspector);
        }
        DesktopRuntimeCommandKind::RenameSession => {}
    }
}

pub(crate) fn reconcile_projection_completion<Presentation: RuntimeWorkspacePresentation>(
    state: &mut DesktopState<
        WorkspaceState<Presentation>,
        ProjectCatalogController,
        RuntimeWorkspaceDefaults,
    >,
    target: &WorkspaceKey,
    completion: ProjectionCompletion,
    projection: ProjectionUpdateResult,
    changes: &mut UiChangeSet,
) {
    let replaced = projection.replaced();
    match completion {
        ProjectionCompletion::None => {}
        ProjectionCompletion::Resync { command_id } => {
            if state.complete_runtime_command(command_id, target, &DesktopCommandIntent::Resync) {
                state.set_runtime_notice(
                    target,
                    if replaced {
                        "Runtime state resynchronized.".into()
                    } else {
                        "Resync response failed projection validation.".into()
                    },
                );
            }
        }
        ProjectionCompletion::Session {
            owner,
            command_id,
            intent,
        } => {
            let owner = state.commands.owner(command_id).cloned().unwrap_or(owner);
            if state.complete_runtime_command(command_id, &owner, &intent) {
                let created = matches!(intent, DesktopCommandIntent::CreateSession);
                state.set_runtime_notice(
                    target,
                    if replaced {
                        match intent {
                            DesktopCommandIntent::CreateSession => "Created a new session.".into(),
                            DesktopCommandIntent::OpenSession { .. } => {
                                "Opened the requested session.".into()
                            }
                            _ => unreachable!("session completion is typed"),
                        }
                    } else {
                        "Session response failed projection validation; resync is required.".into()
                    },
                );
                changes.insert(UiRegion::Sessions);
                if replaced && created && state.insert_session_into_catalog(target) {
                    changes.insert(UiRegion::Sessions);
                }
            }
        }
        ProjectionCompletion::Reload {
            command_id,
            skill_count,
            prompt_count,
            profile_count,
        } => {
            if state.complete_runtime_command(command_id, target, &DesktopCommandIntent::Reload) {
                state.set_runtime_notice(
                    target,
                    if replaced {
                        format!(
                            "Reloaded {skill_count} skills, {prompt_count} prompts, and \
                             {profile_count} profiles."
                        )
                    } else {
                        "Reload response failed projection validation; resync is required.".into()
                    },
                );
            }
        }
        ProjectionCompletion::Selection {
            command_id,
            selection,
            thinking_level,
            thinking_fallback,
        } => {
            if state.complete_runtime_command(
                command_id,
                target,
                &DesktopCommandIntent::Selection(selection),
            ) {
                if replaced && selection == DesktopRuntimeSelectionKind::Model {
                    state.apply_model_thinking_selection(target, thinking_level, thinking_fallback);
                }
                let notice = if replaced {
                    match selection {
                        DesktopRuntimeSelectionKind::Model => format!(
                            "Future prompts will use model {}.",
                            truncate_label(&state.selected_model_label(target), 28)
                        ),
                        DesktopRuntimeSelectionKind::SessionProfile => format!(
                            "Session profile changed to {}.",
                            truncate_label(&state.selected_profile_label(target), 28)
                        ),
                    }
                } else {
                    "Selection response failed projection validation; resync is required.".into()
                };
                state.set_runtime_notice(target, notice);
            }
        }
        ProjectionCompletion::Recovery {
            command_id,
            action,
            recovery_id,
        } => {
            let intent = DesktopCommandIntent::Recovery {
                recovery_id: recovery_id.clone(),
                action,
            };
            if state.complete_runtime_command(command_id, target, &intent) {
                state.set_runtime_notice(
                    target,
                    if replaced {
                        format!(
                            "Recovery {} accepted for {}.",
                            recovery_action_label(action),
                            truncate_label(&recovery_id, 28)
                        )
                    } else {
                        "Recovery changed, but its snapshot failed projection validation; resync \
                         is required."
                            .into()
                    },
                );
            }
        }
    }
}
