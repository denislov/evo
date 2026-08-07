//! Root runtime-update reducer: workspace targeting, command completion, and
//! resync reservation.

use coding_agent::api::review::CodingAgentFileReviewRequest;
use desktop::runtime::{DesktopRecoveryAction, DesktopRuntimeCommandKind, DesktopRuntimeUpdate};
use desktop::ui::shell::truncate_label;

use super::{
    DesktopController, Transition, capture_projection_completion, projection_event,
    reconcile_projection_completion, reduce_pre_projection_update, runtime_update_command_id,
    runtime_update_hydrated_snapshot, runtime_update_kind, runtime_update_observed_workspace_key,
};
use crate::application::{
    catalog::ProjectCatalogController,
    change_set::{UiChangeSet, UiRegion},
    commands::DesktopCommandIntent,
    effect::ExternalEditorLaunchTarget,
    runtime_state::RuntimeWorkspacePresentation,
    state::DesktopState,
    workspace::WorkspaceKey,
    workspace_state::{RuntimeWorkspaceDefaults, WorkspaceState},
};

pub(crate) fn reduce_runtime_update<Presentation: RuntimeWorkspacePresentation>(
    controller: &mut DesktopController,
    state: &mut DesktopState<
        WorkspaceState<Presentation>,
        ProjectCatalogController,
        RuntimeWorkspaceDefaults,
    >,
    update: DesktopRuntimeUpdate,
) -> Transition {
    let _kind = runtime_update_kind(&update);
    let initial_foreground = state.workspaces.active_key().clone();
    let inherit_home_thinking = match &update {
        DesktopRuntimeUpdate::PromptAcceptedWithSession { .. }
        | DesktopRuntimeUpdate::PromptRejectedWithSession { .. } => true,
        DesktopRuntimeUpdate::SessionChanged { command_id, .. } => state.commands.matches(
            *command_id,
            &initial_foreground,
            &DesktopCommandIntent::CreateSession,
        ),
        _ => false,
    };

    if inherit_home_thinking
        && let DesktopRuntimeUpdate::SessionChanged {
            command_id,
            snapshot,
        } = &update
    {
        let _ = state.commands.transfer_command(
            *command_id,
            WorkspaceKey::session(&snapshot.session.session.session_id),
        );
    }

    let creates_session_from_prompt = match &update {
        DesktopRuntimeUpdate::PromptAcceptedWithSession { command_id, .. }
        | DesktopRuntimeUpdate::PromptRejectedWithSession { command_id, .. } => {
            state.commands.matches(
                *command_id,
                &initial_foreground,
                &DesktopCommandIntent::Prompt,
            )
        }
        _ => false,
    };
    if creates_session_from_prompt && let Some(snapshot) = runtime_update_hydrated_snapshot(&update)
    {
        let _ = state.install_hydrated_workspace(snapshot, inherit_home_thinking, true);
    }

    if let Some(command_id) = runtime_update_command_id(&update)
        && let Some(pending_owner) = state.commands.owner(command_id).cloned()
        && let Some(observed_owner) = runtime_update_observed_workspace_key(&update)
        && pending_owner != observed_owner
    {
        state.require_command_owner_resync(&pending_owner, &observed_owner);
        let resync_owner = if state.workspaces.contains(&pending_owner) {
            pending_owner
        } else {
            state.workspaces.active_key().clone()
        };
        let foreground = state.workspaces.active_key().clone();
        let mut transition =
            workspace_update_transition(&foreground, &resync_owner, runtime_base_changes(&update));
        transition.merge(reserve_resync_effect(controller, state, &resync_owner));
        return transition;
    }

    if let DesktopRuntimeUpdate::SessionChanged { snapshot, .. } = &update {
        let target = WorkspaceKey::session(&snapshot.session.session.session_id);
        if state.workspaces.active_key() != &target
            && !state.install_hydrated_workspace(snapshot, inherit_home_thinking, true)
        {
            if let DesktopRuntimeUpdate::SessionChanged { command_id, .. } = &update
                && let Some(intent) = state
                    .commands
                    .intent(*command_id)
                    .cloned()
                    .filter(|intent| {
                        matches!(
                            intent,
                            DesktopCommandIntent::CreateSession
                                | DesktopCommandIntent::OpenSession { .. }
                        )
                    })
            {
                let owner = state.commands.owner(*command_id).cloned().unwrap_or(target);
                if state.complete_runtime_command(*command_id, &owner, &intent) {
                    let foreground = state.workspaces.active_key().clone();
                    state.set_runtime_notice(
                        &foreground,
                        "Session response failed projection validation; resync is required.".into(),
                    );
                    let mut changes = runtime_base_changes(&update);
                    changes.insert(UiRegion::Sessions);
                    return Transition::from_changes(changes);
                }
            }
            return Transition::default();
        }
    }

    let target = runtime_update_observed_workspace_key(&update)
        .or_else(|| {
            runtime_update_command_id(&update).and_then(|id| state.commands.owner(id).cloned())
        })
        .filter(|owner| state.workspaces.contains(owner))
        .unwrap_or_else(|| state.workspaces.active_key().clone());
    let completion_owner = runtime_update_observed_workspace_key(&update)
        .or_else(|| {
            runtime_update_command_id(&update).and_then(|id| state.commands.owner(id).cloned())
        })
        .unwrap_or_else(|| target.clone());
    let mut changes = runtime_base_changes(&update);

    match update {
        DesktopRuntimeUpdate::SessionClosed {
            command_id,
            session_id,
        } => {
            let owner = WorkspaceKey::session(&session_id);
            let intent = DesktopCommandIntent::CloseSession {
                session_id: session_id.clone(),
            };
            if state.complete_runtime_command(command_id, &owner, &intent) {
                let cancelled = state.remove_closed_workspace(&session_id);
                state.catalog.remove_session(&session_id);
                let foreground = state.workspaces.active_key().clone();
                state.set_runtime_notice(
                    &foreground,
                    if cancelled == 0 {
                        "Session closed.".into()
                    } else {
                        format!("Session closed; {cancelled} pending command(s) cancelled.")
                    },
                );
                changes.insert(UiRegion::Sessions);
                changes.insert(UiRegion::Inspector);
            }
            return Transition::from_changes(changes);
        }
        DesktopRuntimeUpdate::SessionDeleted {
            command_id,
            session_id,
        } => {
            let owner = WorkspaceKey::session(&session_id);
            let intent = DesktopCommandIntent::DeleteSession {
                session_id: session_id.clone(),
            };
            if state.complete_runtime_command(command_id, &owner, &intent) {
                let cancelled = state.remove_closed_workspace(&session_id);
                state.catalog.remove_session(&session_id);
                let foreground = state.workspaces.active_key().clone();
                state.set_runtime_notice(
                    &foreground,
                    if cancelled == 0 {
                        "Session deleted.".into()
                    } else {
                        format!("Session deleted; {cancelled} pending command(s) cancelled.")
                    },
                );
                changes.insert(UiRegion::Sessions);
                changes.insert(UiRegion::Inspector);
            }
            return Transition::from_changes(changes);
        }
        DesktopRuntimeUpdate::FileReviewed { command_id, review } => {
            let request = CodingAgentFileReviewRequest::new(review.change.clone(), review.revision);
            let owner = state
                .commands
                .owner(command_id)
                .cloned()
                .unwrap_or(target.clone());
            if state.complete_runtime_command(
                command_id,
                &owner,
                &DesktopCommandIntent::FileReview { request },
            ) {
                state.set_file_review_ready(&owner, review);
                state.set_runtime_notice(&owner, "Changed-file review loaded.".into());
                changes.insert(UiRegion::Inspector);
            }
            let foreground = state.workspaces.active_key().clone();
            return workspace_update_transition(&foreground, &owner, changes);
        }
        DesktopRuntimeUpdate::MergeProposalsListed {
            command_id,
            proposals,
        } => {
            let owner = state
                .commands
                .owner(command_id)
                .cloned()
                .unwrap_or(target.clone());
            if state.complete_runtime_command(
                command_id,
                &owner,
                &DesktopCommandIntent::ListMergeProposals,
            ) {
                let count = proposals.len();
                state.set_merge_proposals(&owner, proposals);
                state.set_runtime_notice(&owner, format!("Loaded {count} merge proposal(s)."));
                changes.insert(UiRegion::Inspector);
            }
            let foreground = state.workspaces.active_key().clone();
            return workspace_update_transition(&foreground, &owner, changes);
        }
        DesktopRuntimeUpdate::ChildWorktreeMerged {
            command_id,
            worktree_id,
            applied,
        } => {
            let owner = state
                .commands
                .owner(command_id)
                .cloned()
                .unwrap_or(target.clone());
            let intent = DesktopCommandIntent::MergeProposal {
                worktree_id: worktree_id.clone(),
            };
            if state.complete_runtime_command(command_id, &owner, &intent) {
                let proposals = state
                    .workspaces
                    .get(&owner)
                    .map(|workspace| {
                        workspace
                            .merge_proposals
                            .iter()
                            .filter(|proposal| proposal.worktree_id != worktree_id)
                            .cloned()
                            .collect()
                    })
                    .unwrap_or_default();
                state.set_merge_proposals(&owner, proposals);
                state.set_runtime_notice(
                    &owner,
                    format!("Merged {worktree_id}; applied {applied} change(s)."),
                );
                changes.insert(UiRegion::Inspector);
            }
            let foreground = state.workspaces.active_key().clone();
            return workspace_update_transition(&foreground, &owner, changes);
        }
        DesktopRuntimeUpdate::ChildWorktreeDiscarded {
            command_id,
            worktree_id,
        } => {
            let owner = state
                .commands
                .owner(command_id)
                .cloned()
                .unwrap_or(target.clone());
            let intent = DesktopCommandIntent::DiscardProposal {
                worktree_id: worktree_id.clone(),
            };
            if state.complete_runtime_command(command_id, &owner, &intent) {
                let proposals = state
                    .workspaces
                    .get(&owner)
                    .map(|workspace| {
                        workspace
                            .merge_proposals
                            .iter()
                            .filter(|proposal| proposal.worktree_id != worktree_id)
                            .cloned()
                            .collect()
                    })
                    .unwrap_or_default();
                state.set_merge_proposals(&owner, proposals);
                state.set_runtime_notice(&owner, format!("Discarded {worktree_id}."));
                changes.insert(UiRegion::Inspector);
            }
            let foreground = state.workspaces.active_key().clone();
            return workspace_update_transition(&foreground, &owner, changes);
        }
        DesktopRuntimeUpdate::ExternalEditorTargetValidated {
            command_id,
            target: validated_target,
        } => {
            let project_relative_path = validated_target.project_relative_path().to_owned();
            let owner = state
                .commands
                .owner(command_id)
                .cloned()
                .unwrap_or(target.clone());
            let intent = DesktopCommandIntent::ExternalEditor {
                project_relative_path: project_relative_path.clone(),
            };
            if !state.commands.matches(command_id, &owner, &intent) {
                return Transition::default();
            }
            let Some(preference) = state.preferences.external_editor.clone() else {
                let _ = state.commands.complete(command_id, &owner, &intent);
                state.set_runtime_notice(
                    &owner,
                    "The external editor is no longer configured.".into(),
                );
                changes.insert(UiRegion::Inspector);
                let foreground = state.workspaces.active_key().clone();
                return workspace_update_transition(&foreground, &owner, changes);
            };
            let effect = match controller.launch_external_editor(
                owner.clone(),
                command_id,
                preference,
                ExternalEditorLaunchTarget::new(
                    validated_target.path().to_path_buf(),
                    project_relative_path.clone(),
                ),
            ) {
                Ok(effect) => effect,
                Err(error) => {
                    let _ = state.commands.complete(command_id, &owner, &intent);
                    state.set_runtime_notice(&owner, error.to_string());
                    changes.insert(UiRegion::Inspector);
                    let foreground = state.workspaces.active_key().clone();
                    return workspace_update_transition(&foreground, &owner, changes);
                }
            };
            state.set_runtime_notice(
                &owner,
                format!(
                    "Launching {} in the configured editor…",
                    truncate_label(&project_relative_path, 48)
                ),
            );
            changes.insert(UiRegion::Inspector);
            let foreground = state.workspaces.active_key().clone();
            let mut transition = workspace_update_transition(&foreground, &owner, changes);
            transition.merge(effect);
            return transition;
        }
        DesktopRuntimeUpdate::SessionsListed {
            command_id,
            sessions,
            omitted,
        } => {
            if let Some(owner) = state.commands.owner(command_id).cloned()
                && state.complete_runtime_command(
                    command_id,
                    &owner,
                    &DesktopCommandIntent::ListSessions,
                )
            {
                state.catalog.replace_catalog(sessions, omitted);
                changes.insert(UiRegion::Sessions);
            }
            return Transition::from_changes(changes);
        }
        DesktopRuntimeUpdate::SessionRenamed {
            command_id,
            session_id,
            name,
            updated_at,
        } => {
            let owner = WorkspaceKey::session(&session_id);
            let intent = DesktopCommandIntent::RenameSession {
                session_id: session_id.clone(),
            };
            if state.complete_runtime_command(command_id, &owner, &intent) {
                state.catalog.rename_session(&session_id, name, updated_at);
                let foreground = state.workspaces.active_key().clone();
                state.set_runtime_notice(&foreground, "Session name updated.".into());
                changes.insert(UiRegion::Sessions);
            }
            return Transition::from_changes(changes);
        }
        DesktopRuntimeUpdate::SessionNameObserved {
            session_id,
            name,
            updated_at,
        } => {
            if state.catalog.rename_session(&session_id, name, updated_at) {
                changes.insert(UiRegion::Sessions);
            }
            return Transition::from_changes(changes);
        }
        update => {
            let completion = capture_projection_completion(state, &target, &update);
            reduce_pre_projection_update(state, &target, &completion_owner, &update, &mut changes);
            let completed_prompt_command = match &update {
                DesktopRuntimeUpdate::PromptFinished { command_id, .. } => Some(*command_id),
                _ => None,
            };
            let event = projection_event(update);
            let projection = state.apply_projection_event(
                &target,
                event,
                creates_session_from_prompt,
                completed_prompt_command,
            );
            changes.merge(projection.changes());
            reconcile_projection_completion(state, &target, completion, projection, &mut changes);
            if projection.needs_resync() {
                let mut transition = reserve_resync_effect(controller, state, &target);
                let foreground = state.workspaces.active_key().clone();
                transition.merge(workspace_update_transition(&foreground, &target, changes));
                return transition;
            }
        }
    }

    let foreground = state.workspaces.active_key().clone();
    workspace_update_transition(&foreground, &target, changes)
}

pub(crate) fn reserve_resync_effect<Presentation: RuntimeWorkspacePresentation>(
    controller: &mut DesktopController,
    state: &mut DesktopState<
        WorkspaceState<Presentation>,
        ProjectCatalogController,
        RuntimeWorkspaceDefaults,
    >,
    owner: &WorkspaceKey,
) -> Transition {
    let Some(command_id) = state.reserve_resync_command(owner) else {
        return Transition::default();
    };
    match controller.request_resync(owner.clone(), command_id) {
        Ok(transition) => transition,
        Err(error) => {
            state.abandon_resync_command(owner, command_id, error.to_string());
            Transition::changed(UiRegion::Toast)
        }
    }
}
pub(crate) fn runtime_base_changes(update: &DesktopRuntimeUpdate) -> UiChangeSet {
    if matches!(update, DesktopRuntimeUpdate::ProductEvent { .. }) {
        return UiChangeSet::default();
    }
    let mut changes = UiChangeSet::one(UiRegion::Root);
    changes.insert(UiRegion::ConversationHeader);
    changes.insert(UiRegion::Composer);
    changes.insert(UiRegion::Modal);
    changes.insert(UiRegion::Toast);
    changes
}

pub(crate) fn workspace_update_transition(
    foreground: &WorkspaceKey,
    target: &WorkspaceKey,
    changes: UiChangeSet,
) -> Transition {
    if foreground == target {
        Transition::from_changes(changes)
    } else {
        Transition::default()
    }
}

pub(crate) fn safe_runtime_rejection_notice(
    command: DesktopRuntimeCommandKind,
    code: &str,
) -> String {
    format!("{command:?} rejected ({})", truncate_label(code, 28))
}

pub(crate) const fn recovery_action_label(action: DesktopRecoveryAction) -> &'static str {
    match action {
        DesktopRecoveryAction::Retry => "retry",
        DesktopRecoveryAction::MarkFailed => "mark-failed",
        DesktopRecoveryAction::Abort => "abort",
    }
}
