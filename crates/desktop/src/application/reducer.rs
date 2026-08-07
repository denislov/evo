//! Typed desktop application reducer: pure state reduction over runtime
//! updates, platform results, timers, and UI intents.

mod controller;
mod projection;
mod runtime;

#[cfg(test)]
mod tests;

#[allow(unused_imports)]
pub(crate) use self::{
    controller::{
        DesktopController, PlatformUpdatePort, effect_supersedes, foreground_changes,
        foreground_notice_transition, foreground_transition, reduce_paths_picked,
        reduce_platform_result, reduce_timer_result,
    },
    projection::{
        ProjectionCompletion, capture_projection_completion, projection_event,
        reconcile_projection_completion, reduce_command_rejected, reduce_pre_projection_update,
    },
    runtime::{
        recovery_action_label, reduce_runtime_update, reserve_resync_effect, runtime_base_changes,
        safe_runtime_rejection_notice, workspace_update_transition,
    },
};

use crate::application::{
    change_set::{UiChangeSet, UiRegion},
    effect::DesktopEffect,
    workspace::WorkspaceKey,
};
use desktop::runtime::{DesktopRuntimeResyncSnapshot, DesktopRuntimeUpdate};
use thiserror::Error;

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
    Platform(crate::application::effect::PlatformResult),
    Timer(crate::application::effect::DesktopTimer),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub(crate) enum EffectIdentityError {
    #[error("desktop effect request id space is exhausted")]
    Exhausted,
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
