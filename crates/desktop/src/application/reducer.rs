use std::{collections::HashMap, path::PathBuf, time::Duration};

use coding_agent::api::{
    authorization::ToolAuthorizationDecision, embedding::CodingAgentThinkingLevel,
    review::CodingAgentFileReviewRequest,
};
use desktop::preferences::{DesktopPreferences, ExternalEditorPreference};
use desktop::runtime::{
    DesktopRecoveryAction, DesktopRuntimeCommandKind, DesktopRuntimeError,
    DesktopRuntimeResyncSnapshot, DesktopRuntimeSelectionKind, DesktopRuntimeUpdate,
};
use desktop::ui::shell::truncate_label;
use thiserror::Error;

use super::{
    catalog::ProjectCatalogController,
    change_set::{UiChangeSet, UiRegion},
    commands::DesktopCommandIntent,
    effect::{
        ClipboardFeedback, DesktopEffect, DesktopPickerKind, DesktopTimer, DesktopTimerKind,
        EffectIdentity, EffectRequestId, ExternalEditorLaunchTarget, PlatformOutcome,
        PlatformResult,
    },
    runtime_state::{ProjectionUpdateResult, RuntimeWorkspacePresentation},
    state::DesktopState,
    workspace::WorkspaceKey,
    workspace_state::{RuntimeWorkspaceDefaults, WorkspaceState},
};
use crate::projection::ProjectionEvent;

/// Exhaustive protocol coverage used by the root runtime reducer.
///
/// Keeping this list next to the reducer makes protocol growth fail closed:
/// adding a [`DesktopRuntimeUpdate`] variant requires extending the exhaustive
/// match below, while the table test protects accidental omissions/reordering
/// in review output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub(crate) enum RuntimeUpdateKind {
    Reloaded,
    Resynced,
    SessionChanged,
    SessionClosed,
    SessionDeleted,
    SessionsListed,
    SessionRenamed,
    SessionNameObserved,
    SelectionChanged,
    PromptAccepted,
    PromptAcceptedWithSession,
    PromptRejectedWithSession,
    PromptStarted,
    ProductEvent,
    ResyncRequired,
    ControlAccepted,
    AuthorizationDecisionAccepted,
    RecoveryChanged,
    FileReviewed,
    MergeProposalsListed,
    ChildWorktreeMerged,
    ChildWorktreeDiscarded,
    ExternalEditorTargetValidated,
    PromptFinished,
    CommandRejected,
    RuntimeFailed,
    Stopped,
}

impl RuntimeUpdateKind {
    #[cfg(test)]
    pub(crate) const ALL: [Self; 27] = [
        Self::Reloaded,
        Self::Resynced,
        Self::SessionChanged,
        Self::SessionClosed,
        Self::SessionDeleted,
        Self::SessionsListed,
        Self::SessionRenamed,
        Self::SessionNameObserved,
        Self::SelectionChanged,
        Self::PromptAccepted,
        Self::PromptAcceptedWithSession,
        Self::PromptRejectedWithSession,
        Self::PromptStarted,
        Self::ProductEvent,
        Self::ResyncRequired,
        Self::ControlAccepted,
        Self::AuthorizationDecisionAccepted,
        Self::RecoveryChanged,
        Self::FileReviewed,
        Self::MergeProposalsListed,
        Self::ChildWorktreeMerged,
        Self::ChildWorktreeDiscarded,
        Self::ExternalEditorTargetValidated,
        Self::PromptFinished,
        Self::CommandRejected,
        Self::RuntimeFailed,
        Self::Stopped,
    ];

    #[cfg(test)]
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Reloaded => "reloaded",
            Self::Resynced => "resynced",
            Self::SessionChanged => "session_changed",
            Self::SessionClosed => "session_closed",
            Self::SessionDeleted => "session_deleted",
            Self::SessionsListed => "sessions_listed",
            Self::SessionRenamed => "session_renamed",
            Self::SessionNameObserved => "session_name_observed",
            Self::SelectionChanged => "selection_changed",
            Self::PromptAccepted => "prompt_accepted",
            Self::PromptAcceptedWithSession => "prompt_accepted_with_session",
            Self::PromptRejectedWithSession => "prompt_rejected_with_session",
            Self::PromptStarted => "prompt_started",
            Self::ProductEvent => "product_event",
            Self::ResyncRequired => "resync_required",
            Self::ControlAccepted => "control_accepted",
            Self::AuthorizationDecisionAccepted => "authorization_decision_accepted",
            Self::RecoveryChanged => "recovery_changed",
            Self::FileReviewed => "file_reviewed",
            Self::MergeProposalsListed => "merge_proposals_listed",
            Self::ChildWorktreeMerged => "child_worktree_merged",
            Self::ChildWorktreeDiscarded => "child_worktree_discarded",
            Self::ExternalEditorTargetValidated => "external_editor_target_validated",
            Self::PromptFinished => "prompt_finished",
            Self::CommandRejected => "command_rejected",
            Self::RuntimeFailed => "runtime_failed",
            Self::Stopped => "stopped",
        }
    }
}

pub(crate) const fn runtime_update_kind(update: &DesktopRuntimeUpdate) -> RuntimeUpdateKind {
    match update {
        DesktopRuntimeUpdate::Reloaded { .. } => RuntimeUpdateKind::Reloaded,
        DesktopRuntimeUpdate::Resynced { .. } => RuntimeUpdateKind::Resynced,
        DesktopRuntimeUpdate::SessionChanged { .. } => RuntimeUpdateKind::SessionChanged,
        DesktopRuntimeUpdate::SessionClosed { .. } => RuntimeUpdateKind::SessionClosed,
        DesktopRuntimeUpdate::SessionDeleted { .. } => RuntimeUpdateKind::SessionDeleted,
        DesktopRuntimeUpdate::SessionsListed { .. } => RuntimeUpdateKind::SessionsListed,
        DesktopRuntimeUpdate::SessionRenamed { .. } => RuntimeUpdateKind::SessionRenamed,
        DesktopRuntimeUpdate::SessionNameObserved { .. } => RuntimeUpdateKind::SessionNameObserved,
        DesktopRuntimeUpdate::SelectionChanged { .. } => RuntimeUpdateKind::SelectionChanged,
        DesktopRuntimeUpdate::PromptAccepted { .. } => RuntimeUpdateKind::PromptAccepted,
        DesktopRuntimeUpdate::PromptAcceptedWithSession { .. } => {
            RuntimeUpdateKind::PromptAcceptedWithSession
        }
        DesktopRuntimeUpdate::PromptRejectedWithSession { .. } => {
            RuntimeUpdateKind::PromptRejectedWithSession
        }
        DesktopRuntimeUpdate::PromptStarted { .. } => RuntimeUpdateKind::PromptStarted,
        DesktopRuntimeUpdate::ProductEvent { .. } => RuntimeUpdateKind::ProductEvent,
        DesktopRuntimeUpdate::ResyncRequired { .. } => RuntimeUpdateKind::ResyncRequired,
        DesktopRuntimeUpdate::ControlAccepted { .. } => RuntimeUpdateKind::ControlAccepted,
        DesktopRuntimeUpdate::AuthorizationDecisionAccepted { .. } => {
            RuntimeUpdateKind::AuthorizationDecisionAccepted
        }
        DesktopRuntimeUpdate::RecoveryChanged { .. } => RuntimeUpdateKind::RecoveryChanged,
        DesktopRuntimeUpdate::FileReviewed { .. } => RuntimeUpdateKind::FileReviewed,
        DesktopRuntimeUpdate::MergeProposalsListed { .. } => {
            RuntimeUpdateKind::MergeProposalsListed
        }
        DesktopRuntimeUpdate::ChildWorktreeMerged { .. } => RuntimeUpdateKind::ChildWorktreeMerged,
        DesktopRuntimeUpdate::ChildWorktreeDiscarded { .. } => {
            RuntimeUpdateKind::ChildWorktreeDiscarded
        }
        DesktopRuntimeUpdate::ExternalEditorTargetValidated { .. } => {
            RuntimeUpdateKind::ExternalEditorTargetValidated
        }
        DesktopRuntimeUpdate::PromptFinished { .. } => RuntimeUpdateKind::PromptFinished,
        DesktopRuntimeUpdate::CommandRejected { .. } => RuntimeUpdateKind::CommandRejected,
        DesktopRuntimeUpdate::RuntimeFailed { .. } => RuntimeUpdateKind::RuntimeFailed,
        DesktopRuntimeUpdate::Stopped => RuntimeUpdateKind::Stopped,
    }
}

pub(crate) fn runtime_update_hydrated_snapshot(
    update: &DesktopRuntimeUpdate,
) -> Option<&desktop::runtime::DesktopRuntimeHydratedSnapshot> {
    match update {
        DesktopRuntimeUpdate::SessionChanged { snapshot, .. }
        | DesktopRuntimeUpdate::PromptAcceptedWithSession { snapshot, .. }
        | DesktopRuntimeUpdate::PromptRejectedWithSession { snapshot, .. }
        | DesktopRuntimeUpdate::PromptFinished { snapshot, .. } => Some(snapshot),
        DesktopRuntimeUpdate::Resynced {
            replacement: DesktopRuntimeResyncSnapshot::Hydrated(snapshot),
            ..
        } => Some(snapshot),
        DesktopRuntimeUpdate::Reloaded { .. }
        | DesktopRuntimeUpdate::Resynced {
            replacement: DesktopRuntimeResyncSnapshot::Metadata(_),
            ..
        }
        | DesktopRuntimeUpdate::SessionClosed { .. }
        | DesktopRuntimeUpdate::SessionDeleted { .. }
        | DesktopRuntimeUpdate::SessionsListed { .. }
        | DesktopRuntimeUpdate::SessionRenamed { .. }
        | DesktopRuntimeUpdate::SessionNameObserved { .. }
        | DesktopRuntimeUpdate::SelectionChanged { .. }
        | DesktopRuntimeUpdate::PromptAccepted { .. }
        | DesktopRuntimeUpdate::PromptStarted { .. }
        | DesktopRuntimeUpdate::ProductEvent { .. }
        | DesktopRuntimeUpdate::ResyncRequired { .. }
        | DesktopRuntimeUpdate::ControlAccepted { .. }
        | DesktopRuntimeUpdate::AuthorizationDecisionAccepted { .. }
        | DesktopRuntimeUpdate::RecoveryChanged { .. }
        | DesktopRuntimeUpdate::FileReviewed { .. }
        | DesktopRuntimeUpdate::MergeProposalsListed { .. }
        | DesktopRuntimeUpdate::ChildWorktreeMerged { .. }
        | DesktopRuntimeUpdate::ChildWorktreeDiscarded { .. }
        | DesktopRuntimeUpdate::ExternalEditorTargetValidated { .. }
        | DesktopRuntimeUpdate::CommandRejected { .. }
        | DesktopRuntimeUpdate::RuntimeFailed { .. }
        | DesktopRuntimeUpdate::Stopped => None,
    }
}

pub(crate) const fn runtime_update_command_id(update: &DesktopRuntimeUpdate) -> Option<u64> {
    match update {
        DesktopRuntimeUpdate::Reloaded { command_id, .. }
        | DesktopRuntimeUpdate::Resynced { command_id, .. }
        | DesktopRuntimeUpdate::SessionChanged { command_id, .. }
        | DesktopRuntimeUpdate::SessionClosed { command_id, .. }
        | DesktopRuntimeUpdate::SessionDeleted { command_id, .. }
        | DesktopRuntimeUpdate::SessionsListed { command_id, .. }
        | DesktopRuntimeUpdate::SessionRenamed { command_id, .. }
        | DesktopRuntimeUpdate::SelectionChanged { command_id, .. }
        | DesktopRuntimeUpdate::PromptAccepted { command_id }
        | DesktopRuntimeUpdate::PromptAcceptedWithSession { command_id, .. }
        | DesktopRuntimeUpdate::PromptRejectedWithSession { command_id, .. }
        | DesktopRuntimeUpdate::PromptStarted { command_id, .. }
        | DesktopRuntimeUpdate::ControlAccepted { command_id, .. }
        | DesktopRuntimeUpdate::AuthorizationDecisionAccepted { command_id, .. }
        | DesktopRuntimeUpdate::RecoveryChanged { command_id, .. }
        | DesktopRuntimeUpdate::FileReviewed { command_id, .. }
        | DesktopRuntimeUpdate::MergeProposalsListed { command_id, .. }
        | DesktopRuntimeUpdate::ChildWorktreeMerged { command_id, .. }
        | DesktopRuntimeUpdate::ChildWorktreeDiscarded { command_id, .. }
        | DesktopRuntimeUpdate::ExternalEditorTargetValidated { command_id, .. }
        | DesktopRuntimeUpdate::PromptFinished { command_id, .. }
        | DesktopRuntimeUpdate::CommandRejected { command_id, .. } => Some(*command_id),
        DesktopRuntimeUpdate::SessionNameObserved { .. }
        | DesktopRuntimeUpdate::ProductEvent { .. }
        | DesktopRuntimeUpdate::ResyncRequired { .. }
        | DesktopRuntimeUpdate::RuntimeFailed { .. }
        | DesktopRuntimeUpdate::Stopped => None,
    }
}

pub(crate) fn runtime_update_observed_workspace_key(
    update: &DesktopRuntimeUpdate,
) -> Option<WorkspaceKey> {
    let session_id = match update {
        DesktopRuntimeUpdate::ProductEvent { session_id, .. }
        | DesktopRuntimeUpdate::SessionRenamed { session_id, .. }
        | DesktopRuntimeUpdate::SessionClosed { session_id, .. }
        | DesktopRuntimeUpdate::SessionDeleted { session_id, .. } => Some(session_id.clone()),
        DesktopRuntimeUpdate::SessionChanged { snapshot, .. }
        | DesktopRuntimeUpdate::PromptAcceptedWithSession { snapshot, .. }
        | DesktopRuntimeUpdate::PromptRejectedWithSession { snapshot, .. }
        | DesktopRuntimeUpdate::PromptFinished { snapshot, .. } => {
            Some(snapshot.session.session.session_id.clone())
        }
        DesktopRuntimeUpdate::Resynced {
            replacement: DesktopRuntimeResyncSnapshot::Hydrated(snapshot),
            ..
        } => Some(snapshot.session.session.session_id.clone()),
        DesktopRuntimeUpdate::Reloaded { metadata, .. }
        | DesktopRuntimeUpdate::SelectionChanged { metadata, .. }
        | DesktopRuntimeUpdate::PromptStarted { metadata, .. } => metadata
            .session
            .as_ref()
            .map(|snapshot| snapshot.session.session_id.clone()),
        DesktopRuntimeUpdate::Resynced {
            replacement: DesktopRuntimeResyncSnapshot::Metadata(metadata),
            ..
        } => metadata
            .session
            .as_ref()
            .map(|snapshot| snapshot.session.session_id.clone()),
        DesktopRuntimeUpdate::RecoveryChanged { recovery, .. } => {
            Some(recovery.session.session.session_id.clone())
        }
        DesktopRuntimeUpdate::ResyncRequired { snapshot, .. } => {
            Some(snapshot.session.session_id.clone())
        }
        DesktopRuntimeUpdate::SessionsListed { .. }
        | DesktopRuntimeUpdate::SessionNameObserved { .. }
        | DesktopRuntimeUpdate::PromptAccepted { .. }
        | DesktopRuntimeUpdate::ControlAccepted { .. }
        | DesktopRuntimeUpdate::AuthorizationDecisionAccepted { .. }
        | DesktopRuntimeUpdate::FileReviewed { .. }
        | DesktopRuntimeUpdate::MergeProposalsListed { .. }
        | DesktopRuntimeUpdate::ChildWorktreeMerged { .. }
        | DesktopRuntimeUpdate::ChildWorktreeDiscarded { .. }
        | DesktopRuntimeUpdate::ExternalEditorTargetValidated { .. }
        | DesktopRuntimeUpdate::CommandRejected { .. }
        | DesktopRuntimeUpdate::RuntimeFailed { .. }
        | DesktopRuntimeUpdate::Stopped => None,
    };
    session_id.map(WorkspaceKey::session)
}

pub(crate) trait PlatformUpdatePort {
    fn active_workspace_key(&self) -> WorkspaceKey;
    fn workspace_exists(&self, owner: &WorkspaceKey) -> bool;
    fn project_directory_editable(&self, owner: &WorkspaceKey) -> bool;
    fn set_project_directory(&mut self, owner: &WorkspaceKey, path: PathBuf) -> bool;
    fn add_composer_attachments(
        &mut self,
        owner: &WorkspaceKey,
        paths: Vec<PathBuf>,
    ) -> Result<bool, String>;
    fn set_notice(&mut self, owner: &WorkspaceKey, notice: String);
    fn show_conversation_announcement(&mut self, owner: &WorkspaceKey, message: String);
    fn clear_conversation_announcement(&mut self, owner: &WorkspaceKey) -> bool;
    fn commit_conversation_width(&mut self, owner: &WorkspaceKey) -> bool;
    fn refresh_inspector_telemetry(&mut self, owner: &WorkspaceKey) -> bool;
    fn complete_resync_admission(
        &mut self,
        owner: &WorkspaceKey,
        command_id: u64,
        failure: Option<String>,
    );
    fn complete_external_editor_launch(
        &mut self,
        owner: &WorkspaceKey,
        command_id: u64,
        project_relative_path: &str,
        failure: Option<String>,
    );
}

#[derive(Debug, Clone)]
enum ProjectionCompletion {
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

fn reduce_runtime_update<Presentation: RuntimeWorkspacePresentation>(
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

fn reserve_resync_effect<Presentation: RuntimeWorkspacePresentation>(
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

fn capture_projection_completion<Presentation: RuntimeWorkspacePresentation>(
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

fn reduce_pre_projection_update<Presentation: RuntimeWorkspacePresentation>(
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

fn reduce_command_rejected<Presentation: RuntimeWorkspacePresentation>(
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

fn reconcile_projection_completion<Presentation: RuntimeWorkspacePresentation>(
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

fn runtime_base_changes(update: &DesktopRuntimeUpdate) -> UiChangeSet {
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

fn workspace_update_transition(
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

const fn recovery_action_label(action: DesktopRecoveryAction) -> &'static str {
    match action {
        DesktopRecoveryAction::Retry => "retry",
        DesktopRecoveryAction::MarkFailed => "mark-failed",
        DesktopRecoveryAction::Abort => "abort",
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CatalogIntent {
    SetProjectCollapsed { group_id: String, collapsed: bool },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PreferencePanel {
    Sessions,
    Context,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PreferencesIntent {
    SetPanelWidth { panel: PreferencePanel, width: u32 },
}

#[derive(Debug, Clone)]
pub(crate) enum DesktopEvent {
    Ui(CatalogIntent),
    Preferences(PreferencesIntent),
    Platform(PlatformResult),
    Timer(DesktopTimer),
}

#[derive(Debug, Default, Clone, PartialEq)]
pub(crate) struct Transition {
    changes: UiChangeSet,
    effects: Vec<DesktopEffect>,
}

impl Transition {
    pub(crate) const fn changed(region: UiRegion) -> Self {
        Self {
            changes: UiChangeSet::one(region),
            effects: Vec::new(),
        }
    }

    pub(crate) const fn from_changes(changes: UiChangeSet) -> Self {
        Self {
            changes,
            effects: Vec::new(),
        }
    }

    pub(crate) fn with_effect(mut self, effect: DesktopEffect) -> Self {
        self.effects.push(effect);
        self
    }

    pub(crate) const fn changes(&self) -> UiChangeSet {
        self.changes
    }

    #[cfg(test)]
    pub(crate) fn effects(&self) -> &[DesktopEffect] {
        &self.effects
    }

    pub(crate) fn into_parts(self) -> (UiChangeSet, Vec<DesktopEffect>) {
        (self.changes, self.effects)
    }

    pub(crate) fn merge(&mut self, other: Self) {
        self.changes.merge(other.changes);
        self.effects.extend(other.effects);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub(crate) enum EffectIdentityError {
    #[error("desktop effect request id space is exhausted")]
    Exhausted,
}

pub(crate) struct DesktopController {
    next_effect_request_id: u64,
    pending_effects: HashMap<EffectRequestId, DesktopEffect>,
}

impl DesktopController {
    pub(crate) fn new() -> Self {
        Self {
            next_effect_request_id: 0,
            pending_effects: HashMap::new(),
        }
    }

    pub(crate) fn reduce_runtime<Presentation: RuntimeWorkspacePresentation>(
        &mut self,
        state: &mut DesktopState<
            WorkspaceState<Presentation>,
            ProjectCatalogController,
            RuntimeWorkspaceDefaults,
        >,
        update: DesktopRuntimeUpdate,
    ) -> Transition {
        reduce_runtime_update(self, state, update)
    }

    /// Route an event through one mutable application-state authority while
    /// feature branches are migrated from the GPUI adapter in later tasks.
    pub(crate) fn reduce<Workspace, Catalog, WorkspaceDefaults>(
        &mut self,
        state: &mut DesktopState<Workspace, Catalog, WorkspaceDefaults>,
        event: DesktopEvent,
        delegate: impl FnOnce(
            &mut DesktopState<Workspace, Catalog, WorkspaceDefaults>,
            DesktopEvent,
        ) -> Transition,
    ) -> Transition {
        delegate(state, event)
    }

    pub(crate) fn reserve_effect_identity(
        &mut self,
        owner: WorkspaceKey,
    ) -> Result<EffectIdentity, EffectIdentityError> {
        let request_id = self.next_effect_request_id;
        self.next_effect_request_id = self
            .next_effect_request_id
            .checked_add(1)
            .ok_or(EffectIdentityError::Exhausted)?;
        Ok(EffectIdentity::new(EffectRequestId::new(request_id), owner))
    }

    pub(crate) fn pick_paths(
        &mut self,
        owner: WorkspaceKey,
        picker: DesktopPickerKind,
    ) -> Result<Transition, EffectIdentityError> {
        let identity = self.reserve_effect_identity(owner)?;
        Ok(self.register_effect(DesktopEffect::PickPaths { identity, picker }))
    }

    pub(crate) fn write_clipboard(
        &mut self,
        owner: WorkspaceKey,
        text: Option<String>,
        feedback: ClipboardFeedback,
    ) -> Result<Transition, EffectIdentityError> {
        let identity = self.reserve_effect_identity(owner)?;
        Ok(self.register_effect(DesktopEffect::WriteClipboard {
            identity,
            text,
            feedback,
        }))
    }

    pub(crate) fn write_preferences(
        &mut self,
        owner: WorkspaceKey,
        preferences: DesktopPreferences,
    ) -> Result<Transition, EffectIdentityError> {
        let identity = self.reserve_effect_identity(owner)?;
        Ok(self.register_effect(DesktopEffect::WritePreferences {
            identity,
            preferences,
        }))
    }

    pub(crate) fn request_resync(
        &mut self,
        owner: WorkspaceKey,
        command_id: u64,
    ) -> Result<Transition, EffectIdentityError> {
        let identity = self.reserve_effect_identity(owner)?;
        Ok(self.register_effect(DesktopEffect::RequestResync {
            identity,
            command_id,
        }))
    }

    pub(crate) fn launch_external_editor(
        &mut self,
        owner: WorkspaceKey,
        command_id: u64,
        preference: ExternalEditorPreference,
        target: ExternalEditorLaunchTarget,
    ) -> Result<Transition, EffectIdentityError> {
        let identity = self.reserve_effect_identity(owner)?;
        Ok(self.register_effect(DesktopEffect::LaunchExternalEditor {
            identity,
            command_id,
            preference,
            target,
        }))
    }

    pub(crate) fn schedule_timer(
        &mut self,
        owner: WorkspaceKey,
        kind: DesktopTimerKind,
        delay: Duration,
    ) -> Result<Transition, EffectIdentityError> {
        let identity = self.reserve_effect_identity(owner)?;
        Ok(self.register_effect(DesktopEffect::ScheduleTimer {
            timer: DesktopTimer::new(identity, kind),
            delay,
        }))
    }

    pub(crate) fn reduce_platform(
        &mut self,
        port: &mut impl PlatformUpdatePort,
        result: PlatformResult,
    ) -> Transition {
        let request_id = result.identity().request_id();
        let Some(effect) = self.pending_effects.get(&request_id) else {
            return Transition::default();
        };
        if !effect.matches_platform_result(&result) {
            return Transition::default();
        }
        let effect = self
            .pending_effects
            .remove(&request_id)
            .expect("a matching pending effect must still exist");
        reduce_platform_result(self, port, effect, result)
    }

    pub(crate) fn reduce_async(
        &mut self,
        port: &mut impl PlatformUpdatePort,
        event: DesktopEvent,
    ) -> Transition {
        match event {
            DesktopEvent::Platform(result) => self.reduce_platform(port, result),
            DesktopEvent::Timer(timer) => self.reduce_timer(port, timer),
            DesktopEvent::Ui(_) | DesktopEvent::Preferences(_) => {
                debug_assert!(false, "UI intents use their typed feature reducer");
                Transition::default()
            }
        }
    }

    pub(crate) fn reduce_timer(
        &mut self,
        port: &mut impl PlatformUpdatePort,
        timer: DesktopTimer,
    ) -> Transition {
        let request_id = timer.identity().request_id();
        let Some(DesktopEffect::ScheduleTimer {
            timer: expected, ..
        }) = self.pending_effects.get(&request_id)
        else {
            return Transition::default();
        };
        if expected != &timer {
            return Transition::default();
        }
        self.pending_effects.remove(&request_id);
        reduce_timer_result(port, timer)
    }

    fn register_effect(&mut self, effect: DesktopEffect) -> Transition {
        self.pending_effects
            .retain(|_, pending| !effect_supersedes(&effect, pending));
        self.pending_effects
            .insert(effect.identity().request_id(), effect.clone());
        Transition::default().with_effect(effect)
    }
}

impl Default for DesktopController {
    fn default() -> Self {
        Self::new()
    }
}

fn effect_supersedes(next: &DesktopEffect, pending: &DesktopEffect) -> bool {
    match (next, pending) {
        (
            DesktopEffect::PickPaths {
                identity: next_identity,
                picker: next_picker,
            },
            DesktopEffect::PickPaths {
                identity: pending_identity,
                picker: pending_picker,
            },
        ) => next_identity.owner() == pending_identity.owner() && next_picker == pending_picker,
        (DesktopEffect::WritePreferences { .. }, DesktopEffect::WritePreferences { .. }) => true,
        (
            DesktopEffect::RequestResync {
                identity: next_identity,
                ..
            },
            DesktopEffect::RequestResync {
                identity: pending_identity,
                ..
            },
        ) => next_identity.owner() == pending_identity.owner(),
        (
            DesktopEffect::LaunchExternalEditor {
                identity: next_identity,
                ..
            },
            DesktopEffect::LaunchExternalEditor {
                identity: pending_identity,
                ..
            },
        ) => next_identity.owner() == pending_identity.owner(),
        (
            DesktopEffect::ScheduleTimer {
                timer: next_timer, ..
            },
            DesktopEffect::ScheduleTimer {
                timer: pending_timer,
                ..
            },
        ) => next_timer.kind() == pending_timer.kind(),
        _ => false,
    }
}

fn reduce_platform_result(
    controller: &mut DesktopController,
    port: &mut impl PlatformUpdatePort,
    effect: DesktopEffect,
    result: PlatformResult,
) -> Transition {
    let owner = effect.identity().owner().clone();
    if !port.workspace_exists(&owner) {
        return Transition::default();
    }
    match (effect, result) {
        (DesktopEffect::PickPaths { picker, .. }, PlatformResult::PathsPicked { outcome, .. }) => {
            reduce_paths_picked(port, &owner, picker, outcome)
        }
        (
            DesktopEffect::WriteClipboard { feedback, .. },
            PlatformResult::ClipboardWritten { outcome, .. },
        ) => match outcome {
            PlatformOutcome::Completed(()) => match feedback {
                ClipboardFeedback::ConversationAnnouncement(message) => {
                    port.show_conversation_announcement(&owner, message);
                    let mut transition = foreground_transition(port, &owner, UiRegion::Root);
                    if let Ok(timer) = controller.schedule_timer(
                        owner,
                        DesktopTimerKind::ConversationAnnouncement,
                        Duration::from_secs(2),
                    ) {
                        transition.merge(timer);
                    }
                    transition
                }
                ClipboardFeedback::Notice(message) => {
                    port.set_notice(&owner, message);
                    foreground_notice_transition(port, &owner)
                }
            },
            PlatformOutcome::Cancelled => Transition::default(),
            PlatformOutcome::Failed(message) => {
                port.set_notice(&owner, message);
                foreground_notice_transition(port, &owner)
            }
        },
        (
            DesktopEffect::WritePreferences { .. },
            PlatformResult::PreferencesWritten { outcome, .. },
        ) => match outcome {
            PlatformOutcome::Completed(()) | PlatformOutcome::Cancelled => Transition::default(),
            PlatformOutcome::Failed(message) => {
                port.set_notice(&owner, message);
                foreground_notice_transition(port, &owner)
            }
        },
        (
            DesktopEffect::RequestResync { command_id, .. },
            PlatformResult::ResyncRequested { outcome, .. },
        ) => {
            let failure = match outcome {
                PlatformOutcome::Completed(()) => None,
                PlatformOutcome::Cancelled => Some("desktop resync request was cancelled".into()),
                PlatformOutcome::Failed(message) => Some(message),
            };
            let failed = failure.is_some();
            port.complete_resync_admission(&owner, command_id, failure);
            if failed {
                foreground_notice_transition(port, &owner)
            } else {
                Transition::default()
            }
        }
        (
            DesktopEffect::LaunchExternalEditor {
                command_id, target, ..
            },
            PlatformResult::ExternalEditorLaunched { outcome, .. },
        ) => {
            let project_relative_path = target.project_relative_path().to_owned();
            let failure = match outcome {
                PlatformOutcome::Completed(()) => None,
                PlatformOutcome::Cancelled => Some("external editor launch was cancelled".into()),
                PlatformOutcome::Failed(message) => Some(message),
            };
            port.complete_external_editor_launch(
                &owner,
                command_id,
                &project_relative_path,
                failure,
            );
            foreground_changes(
                port,
                &owner,
                &[UiRegion::Inspector, UiRegion::Root, UiRegion::Toast],
            )
        }
        _ => unreachable!("platform result was matched to its exact pending effect"),
    }
}

fn reduce_paths_picked(
    port: &mut impl PlatformUpdatePort,
    owner: &WorkspaceKey,
    picker: DesktopPickerKind,
    outcome: PlatformOutcome<Vec<PathBuf>>,
) -> Transition {
    let paths = match outcome {
        PlatformOutcome::Completed(paths) => paths,
        PlatformOutcome::Cancelled => return Transition::default(),
        PlatformOutcome::Failed(message) => {
            port.set_notice(owner, message);
            return foreground_notice_transition(port, owner);
        }
    };
    match picker {
        DesktopPickerKind::ProjectDirectory => {
            if !port.project_directory_editable(owner) {
                return Transition::default();
            }
            let mut paths = paths.into_iter();
            let Some(path) = paths.next() else {
                port.set_notice(
                    owner,
                    "The directory picker returned no project directory.".into(),
                );
                return foreground_notice_transition(port, owner);
            };
            if paths.next().is_some() {
                port.set_notice(
                    owner,
                    "The directory picker returned more than one project directory.".into(),
                );
                return foreground_notice_transition(port, owner);
            }
            if port.set_project_directory(owner, path) {
                foreground_changes(
                    port,
                    owner,
                    &[
                        UiRegion::Root,
                        UiRegion::ConversationHeader,
                        UiRegion::Composer,
                    ],
                )
            } else {
                Transition::default()
            }
        }
        DesktopPickerKind::Attachments => match port.add_composer_attachments(owner, paths) {
            Ok(true) => foreground_changes(port, owner, &[UiRegion::Root, UiRegion::Composer]),
            Ok(false) => Transition::default(),
            Err(message) => {
                port.set_notice(owner, message);
                foreground_notice_transition(port, owner)
            }
        },
    }
}

fn reduce_timer_result(port: &mut impl PlatformUpdatePort, timer: DesktopTimer) -> Transition {
    let owner = timer.identity().owner();
    let (changed, region) = match timer.kind() {
        DesktopTimerKind::ConversationAnnouncement => {
            (port.clear_conversation_announcement(owner), UiRegion::Root)
        }
        DesktopTimerKind::ConversationWidthCommit => {
            (port.commit_conversation_width(owner), UiRegion::Root)
        }
        DesktopTimerKind::InspectorTelemetryRefresh => {
            (port.refresh_inspector_telemetry(owner), UiRegion::Inspector)
        }
    };
    if changed {
        foreground_transition(port, owner, region)
    } else {
        Transition::default()
    }
}

fn foreground_notice_transition(
    port: &impl PlatformUpdatePort,
    owner: &WorkspaceKey,
) -> Transition {
    foreground_changes(port, owner, &[UiRegion::Root, UiRegion::Toast])
}

fn foreground_transition(
    port: &impl PlatformUpdatePort,
    owner: &WorkspaceKey,
    region: UiRegion,
) -> Transition {
    foreground_changes(port, owner, &[region])
}

fn foreground_changes(
    port: &impl PlatformUpdatePort,
    owner: &WorkspaceKey,
    regions: &[UiRegion],
) -> Transition {
    if &port.active_workspace_key() != owner {
        return Transition::default();
    }
    let mut changes = UiChangeSet::default();
    for region in regions {
        changes.insert(*region);
    }
    Transition::from_changes(changes)
}

#[cfg(test)]
mod tests {
    use std::{collections::HashMap, path::PathBuf, time::Duration};

    use desktop::preferences::{DesktopPreferences, ExternalEditorPreference};

    use super::{
        CatalogIntent, DesktopController, DesktopEvent, PlatformUpdatePort, RuntimeUpdateKind,
        Transition, safe_runtime_rejection_notice,
    };
    use crate::application::{
        change_set::UiRegion,
        commands::CommandTracker,
        effect::{
            ClipboardFeedback, DesktopEffect, DesktopPickerKind, DesktopTimer, DesktopTimerKind,
            EffectIdentity, ExternalEditorLaunchTarget, PlatformOutcome, PlatformResult,
        },
        state::DesktopState,
        workspace::{WorkspaceKey, WorkspaceStore},
    };

    fn state() -> DesktopState<&'static str, Vec<String>> {
        DesktopState::new(
            WorkspaceStore::new("home"),
            CommandTracker::default(),
            Vec::new(),
            DesktopPreferences::default(),
        )
    }

    struct TestPlatformPort {
        active: WorkspaceKey,
        projects: HashMap<WorkspaceKey, PathBuf>,
        attachments: HashMap<WorkspaceKey, Vec<PathBuf>>,
        notices: HashMap<WorkspaceKey, String>,
        announcement: Option<(WorkspaceKey, String)>,
        timer_fires: HashMap<DesktopTimerKind, usize>,
    }

    impl TestPlatformPort {
        fn new(active: WorkspaceKey) -> Self {
            Self {
                active,
                projects: HashMap::new(),
                attachments: HashMap::new(),
                notices: HashMap::new(),
                announcement: None,
                timer_fires: HashMap::new(),
            }
        }

        fn record_timer(&mut self, kind: DesktopTimerKind) -> bool {
            *self.timer_fires.entry(kind).or_default() += 1;
            true
        }
    }

    impl PlatformUpdatePort for TestPlatformPort {
        fn active_workspace_key(&self) -> WorkspaceKey {
            self.active.clone()
        }

        fn workspace_exists(&self, _owner: &WorkspaceKey) -> bool {
            true
        }

        fn project_directory_editable(&self, _owner: &WorkspaceKey) -> bool {
            true
        }

        fn set_project_directory(&mut self, owner: &WorkspaceKey, path: PathBuf) -> bool {
            self.projects.insert(owner.clone(), path);
            true
        }

        fn add_composer_attachments(
            &mut self,
            owner: &WorkspaceKey,
            paths: Vec<PathBuf>,
        ) -> Result<bool, String> {
            self.attachments.insert(owner.clone(), paths);
            Ok(true)
        }

        fn set_notice(&mut self, owner: &WorkspaceKey, notice: String) {
            self.notices.insert(owner.clone(), notice);
        }

        fn show_conversation_announcement(&mut self, owner: &WorkspaceKey, message: String) {
            self.announcement = Some((owner.clone(), message));
        }

        fn clear_conversation_announcement(&mut self, owner: &WorkspaceKey) -> bool {
            if self
                .announcement
                .as_ref()
                .is_some_and(|(current, _)| current == owner)
            {
                self.announcement = None;
                true
            } else {
                false
            }
        }

        fn commit_conversation_width(&mut self, _owner: &WorkspaceKey) -> bool {
            self.record_timer(DesktopTimerKind::ConversationWidthCommit)
        }

        fn refresh_inspector_telemetry(&mut self, _owner: &WorkspaceKey) -> bool {
            self.record_timer(DesktopTimerKind::InspectorTelemetryRefresh)
        }

        fn complete_resync_admission(
            &mut self,
            owner: &WorkspaceKey,
            _command_id: u64,
            failure: Option<String>,
        ) {
            if let Some(message) = failure {
                self.set_notice(owner, message);
            }
        }

        fn complete_external_editor_launch(
            &mut self,
            owner: &WorkspaceKey,
            _command_id: u64,
            project_relative_path: &str,
            failure: Option<String>,
        ) {
            self.set_notice(
                owner,
                failure.unwrap_or_else(|| format!("opened {project_relative_path}")),
            );
        }
    }

    fn emitted_identity(transition: &Transition) -> EffectIdentity {
        transition
            .effects()
            .first()
            .expect("request emits one effect")
            .identity()
            .clone()
    }

    #[test]
    fn runtime_update_coverage_table_registers_all_twenty_seven_protocol_variants() {
        let labels = RuntimeUpdateKind::ALL.map(RuntimeUpdateKind::label);
        assert_eq!(
            labels,
            [
                "reloaded",
                "resynced",
                "session_changed",
                "session_closed",
                "session_deleted",
                "sessions_listed",
                "session_renamed",
                "session_name_observed",
                "selection_changed",
                "prompt_accepted",
                "prompt_accepted_with_session",
                "prompt_rejected_with_session",
                "prompt_started",
                "product_event",
                "resync_required",
                "control_accepted",
                "authorization_decision_accepted",
                "recovery_changed",
                "file_reviewed",
                "merge_proposals_listed",
                "child_worktree_merged",
                "child_worktree_discarded",
                "external_editor_target_validated",
                "prompt_finished",
                "command_rejected",
                "runtime_failed",
                "stopped",
            ]
        );
    }

    #[test]
    fn delegated_reduce_mutates_the_single_state_and_returns_typed_changes() {
        let mut controller = DesktopController::new();
        let mut state = state();
        let transition = controller.reduce(
            &mut state,
            DesktopEvent::Ui(CatalogIntent::SetProjectCollapsed {
                group_id: "project:alpha".into(),
                collapsed: true,
            }),
            |state, event| {
                let DesktopEvent::Ui(CatalogIntent::SetProjectCollapsed { group_id, .. }) = event
                else {
                    panic!("test event must remain typed");
                };
                state.catalog.push(group_id);
                Transition::changed(UiRegion::Sessions)
            },
        );

        assert_eq!(state.catalog, ["project:alpha"]);
        assert!(transition.changes().contains(UiRegion::Sessions));
    }

    #[test]
    fn platform_results_require_kind_request_id_and_owner_identity() {
        let mut controller = DesktopController::new();
        let owner = WorkspaceKey::Home;
        let identity = controller.reserve_effect_identity(owner.clone()).unwrap();
        let same_kind = DesktopEffect::PickPaths {
            identity: identity.clone(),
            picker: DesktopPickerKind::Attachments,
        };
        let matching = PlatformResult::PathsPicked {
            identity: identity.clone(),
            picker: DesktopPickerKind::Attachments,
            outcome: PlatformOutcome::Completed(vec![PathBuf::from("image.png")]),
        };
        assert!(same_kind.matches_platform_result(&matching));

        let wrong_kind = DesktopEffect::PickPaths {
            identity: identity.clone(),
            picker: DesktopPickerKind::ProjectDirectory,
        };
        assert!(!wrong_kind.matches_platform_result(&matching));

        let wrong_request = PlatformResult::PathsPicked {
            identity: controller.reserve_effect_identity(owner).unwrap(),
            picker: DesktopPickerKind::Attachments,
            outcome: PlatformOutcome::Cancelled,
        };
        assert!(!same_kind.matches_platform_result(&wrong_request));

        let different_owner =
            EffectIdentity::new(identity.request_id(), WorkspaceKey::session("session-b"));
        let wrong_owner = PlatformResult::PathsPicked {
            identity: different_owner,
            picker: DesktopPickerKind::Attachments,
            outcome: PlatformOutcome::Failed("picker failed".into()),
        };
        assert!(!same_kind.matches_platform_result(&wrong_owner));

        let clipboard = DesktopEffect::WriteClipboard {
            identity: identity.clone(),
            text: Some("copy".into()),
            feedback: ClipboardFeedback::Notice("copied".into()),
        };
        let clipboard_result = PlatformResult::ClipboardWritten {
            identity: identity.clone(),
            outcome: PlatformOutcome::Completed(()),
        };
        assert!(clipboard.matches_platform_result(&clipboard_result));

        let timer = DesktopTimer::new(
            identity.clone(),
            DesktopTimerKind::InspectorTelemetryRefresh,
        );
        let timer_effect = DesktopEffect::ScheduleTimer {
            timer,
            delay: std::time::Duration::from_millis(250),
        };
        assert_eq!(timer_effect.identity(), &identity);
        assert_eq!(timer_effect.identity().owner(), &WorkspaceKey::Home);
    }

    #[test]
    fn external_editor_launch_failure_returns_through_typed_platform_result() {
        let mut controller = DesktopController::new();
        let owner = WorkspaceKey::Home;
        let transition = controller
            .launch_external_editor(
                owner.clone(),
                41,
                ExternalEditorPreference {
                    program: "missing-editor".into(),
                    args: Vec::new(),
                },
                ExternalEditorLaunchTarget::new(
                    PathBuf::from("/project/src/lib.rs"),
                    "src/lib.rs".into(),
                ),
            )
            .unwrap();
        let identity = emitted_identity(&transition);
        let mut port = TestPlatformPort::new(owner.clone());

        let completion = controller.reduce_platform(
            &mut port,
            PlatformResult::ExternalEditorLaunched {
                identity,
                outcome: PlatformOutcome::Failed(
                    "external editor executable is unavailable".into(),
                ),
            },
        );

        assert_eq!(
            port.notices.get(&owner).map(String::as_str),
            Some("external editor executable is unavailable")
        );
        assert!(completion.changes().contains(UiRegion::Inspector));
        assert!(completion.changes().contains(UiRegion::Toast));
    }

    #[test]
    fn newer_picker_request_rejects_the_stale_result() {
        let mut controller = DesktopController::new();
        let owner = WorkspaceKey::Home;
        let first = controller
            .pick_paths(owner.clone(), DesktopPickerKind::Attachments)
            .unwrap();
        let first_identity = emitted_identity(&first);
        let second = controller
            .pick_paths(owner.clone(), DesktopPickerKind::Attachments)
            .unwrap();
        let second_identity = emitted_identity(&second);
        let mut port = TestPlatformPort::new(owner.clone());

        let stale = controller.reduce_async(
            &mut port,
            DesktopEvent::Platform(PlatformResult::PathsPicked {
                identity: first_identity,
                picker: DesktopPickerKind::Attachments,
                outcome: PlatformOutcome::Completed(vec![PathBuf::from("stale.png")]),
            }),
        );
        assert!(stale.changes().is_empty());
        assert!(!port.attachments.contains_key(&owner));

        let current = controller.reduce_async(
            &mut port,
            DesktopEvent::Platform(PlatformResult::PathsPicked {
                identity: second_identity,
                picker: DesktopPickerKind::Attachments,
                outcome: PlatformOutcome::Completed(vec![PathBuf::from("current.png")]),
            }),
        );
        assert!(current.changes().contains(UiRegion::Composer));
        assert_eq!(port.attachments[&owner], [PathBuf::from("current.png")]);
    }

    #[test]
    fn picker_result_mutates_its_owner_without_refreshing_a_switched_workspace() {
        let mut controller = DesktopController::new();
        let owner = WorkspaceKey::Home;
        let requested = controller
            .pick_paths(owner.clone(), DesktopPickerKind::ProjectDirectory)
            .unwrap();
        let identity = emitted_identity(&requested);
        let mut port = TestPlatformPort::new(WorkspaceKey::session("session-b"));

        let transition = controller.reduce_async(
            &mut port,
            DesktopEvent::Platform(PlatformResult::PathsPicked {
                identity,
                picker: DesktopPickerKind::ProjectDirectory,
                outcome: PlatformOutcome::Completed(vec![PathBuf::from("/owner/home")]),
            }),
        );

        assert!(transition.changes().is_empty());
        assert_eq!(port.projects[&owner], PathBuf::from("/owner/home"));
    }

    #[test]
    fn preference_writer_failure_returns_to_the_request_owner_as_a_typed_notice() {
        let mut controller = DesktopController::new();
        let owner = WorkspaceKey::Home;
        let requested = controller
            .write_preferences(owner.clone(), DesktopPreferences::default())
            .unwrap();
        let identity = emitted_identity(&requested);
        let mut port = TestPlatformPort::new(owner.clone());

        let transition = controller.reduce_async(
            &mut port,
            DesktopEvent::Platform(PlatformResult::PreferencesWritten {
                identity,
                outcome: PlatformOutcome::Failed("preference disk failed".into()),
            }),
        );

        assert!(transition.changes().contains(UiRegion::Toast));
        assert_eq!(port.notices[&owner], "preference disk failed");
    }

    #[test]
    fn superseded_timer_identity_cannot_fire_current_state() {
        let mut controller = DesktopController::new();
        let owner = WorkspaceKey::Home;
        let first = controller
            .schedule_timer(
                owner.clone(),
                DesktopTimerKind::ConversationWidthCommit,
                Duration::from_millis(10),
            )
            .unwrap();
        let first_timer = match first.effects().first() {
            Some(DesktopEffect::ScheduleTimer { timer, .. }) => timer.clone(),
            _ => panic!("timer request emits one typed timer"),
        };
        let second = controller
            .schedule_timer(
                owner.clone(),
                DesktopTimerKind::ConversationWidthCommit,
                Duration::from_millis(10),
            )
            .unwrap();
        let second_timer = match second.effects().first() {
            Some(DesktopEffect::ScheduleTimer { timer, .. }) => timer.clone(),
            _ => panic!("timer request emits one typed timer"),
        };
        let mut port = TestPlatformPort::new(owner);

        let stale = controller.reduce_async(&mut port, DesktopEvent::Timer(first_timer));
        assert!(stale.changes().is_empty());
        assert!(port.timer_fires.is_empty());

        let current = controller.reduce_async(&mut port, DesktopEvent::Timer(second_timer));
        assert!(current.changes().contains(UiRegion::Root));
        assert_eq!(
            port.timer_fires[&DesktopTimerKind::ConversationWidthCommit],
            1
        );
    }

    #[test]
    fn runtime_rejection_notice_never_includes_an_untrusted_body() {
        const SECRET: &str = "desktop-secret-canary";
        let notice = safe_runtime_rejection_notice(
            desktop::runtime::DesktopRuntimeCommandKind::DecideToolAuthorization,
            "authorization_not_pending",
        );
        assert!(!notice.contains(SECRET));
    }
}
