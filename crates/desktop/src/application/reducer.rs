#![allow(
    dead_code,
    reason = "DSK-730 root event branches are consumed incrementally by DSK-731 and DSK-733"
)]

use coding_agent::api::{
    authorization::ToolAuthorizationDecision,
    embedding::CodingAgentThinkingLevel,
    review::{CodingAgentFileReview, CodingAgentFileReviewRequest},
};
use desktop::runtime::{
    DesktopRecoveryAction, DesktopRuntimeCommandKind, DesktopRuntimeError,
    DesktopRuntimeHydratedSnapshot, DesktopRuntimeResyncSnapshot, DesktopRuntimeSelectionKind,
    DesktopRuntimeUpdate,
};
use desktop::shell::truncate_label;
use thiserror::Error;

use super::{
    change_set::{UiChangeSet, UiRegion},
    commands::DesktopCommandIntent,
    effect::{DesktopEffect, DesktopTimer, EffectIdentity, EffectRequestId, PlatformResult},
    state::DesktopState,
    workspace::WorkspaceKey,
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
    ExternalEditorOpened,
    PromptFinished,
    CommandRejected,
    RuntimeFailed,
    Stopped,
}

impl RuntimeUpdateKind {
    pub(crate) const ALL: [Self; 23] = [
        Self::Reloaded,
        Self::Resynced,
        Self::SessionChanged,
        Self::SessionClosed,
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
        Self::ExternalEditorOpened,
        Self::PromptFinished,
        Self::CommandRejected,
        Self::RuntimeFailed,
        Self::Stopped,
    ];

    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Reloaded => "reloaded",
            Self::Resynced => "resynced",
            Self::SessionChanged => "session_changed",
            Self::SessionClosed => "session_closed",
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
            Self::ExternalEditorOpened => "external_editor_opened",
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
        DesktopRuntimeUpdate::ExternalEditorOpened { .. } => {
            RuntimeUpdateKind::ExternalEditorOpened
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
        | DesktopRuntimeUpdate::ExternalEditorOpened { .. }
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
        | DesktopRuntimeUpdate::ExternalEditorOpened { command_id, .. }
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
        | DesktopRuntimeUpdate::SessionClosed { session_id, .. } => Some(session_id.clone()),
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
        | DesktopRuntimeUpdate::ExternalEditorOpened { .. }
        | DesktopRuntimeUpdate::CommandRejected { .. }
        | DesktopRuntimeUpdate::RuntimeFailed { .. }
        | DesktopRuntimeUpdate::Stopped => None,
    };
    session_id.map(WorkspaceKey::session)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ProjectionUpdateResult {
    replaced: bool,
    changes: UiChangeSet,
}

impl ProjectionUpdateResult {
    pub(crate) const fn new(replaced: bool, changes: UiChangeSet) -> Self {
        Self { replaced, changes }
    }

    pub(crate) const fn replaced(self) -> bool {
        self.replaced
    }

    pub(crate) const fn changes(self) -> UiChangeSet {
        self.changes
    }
}

/// Narrow state port for the runtime reducer.
///
/// All decisions stay in this module. The GPUI adapter implements only typed,
/// explicit-workspace mutations; projection internals remain behind one method
/// until DSK-732 narrows `DesktopProjection` itself.
pub(crate) trait RuntimeUpdatePort {
    fn active_workspace_key(&self) -> WorkspaceKey;
    fn workspace_exists(&self, owner: &WorkspaceKey) -> bool;
    fn command_owner(&self, command_id: u64) -> Option<WorkspaceKey>;
    fn command_intent(&self, command_id: u64) -> Option<DesktopCommandIntent>;
    fn command_matches(
        &self,
        command_id: u64,
        owner: &WorkspaceKey,
        intent: &DesktopCommandIntent,
    ) -> bool;
    fn transfer_command(&mut self, command_id: u64, owner: WorkspaceKey) -> bool;
    fn require_command_owner_resync(
        &mut self,
        pending_owner: &WorkspaceKey,
        observed_owner: &WorkspaceKey,
    );
    fn complete_command(
        &mut self,
        command_id: u64,
        owner: &WorkspaceKey,
        intent: &DesktopCommandIntent,
    ) -> bool;
    fn reject_command(
        &mut self,
        command_id: u64,
        owner: &WorkspaceKey,
        command: DesktopRuntimeCommandKind,
    ) -> Option<DesktopCommandIntent>;
    fn complete_operation_commands(&mut self, owner: &WorkspaceKey, operation_id: &str);
    fn install_hydrated_workspace(
        &mut self,
        snapshot: &DesktopRuntimeHydratedSnapshot,
        inherit_home_thinking: bool,
        activate: bool,
    ) -> bool;
    fn remove_closed_workspace(&mut self, session_id: &str) -> usize;
    fn remove_catalog_session(&mut self, session_id: &str);
    fn replace_catalog(
        &mut self,
        sessions: Vec<desktop::runtime::DesktopSessionCatalogEntry>,
        omitted: usize,
    );
    fn rename_catalog_session(
        &mut self,
        session_id: &str,
        name: Option<String>,
        updated_at: String,
    ) -> bool;
    fn insert_session_into_catalog(&mut self, owner: &WorkspaceKey) -> bool;
    fn catalog_is_loading(&self) -> bool;
    fn fail_catalog(&mut self, message: String);
    fn cancel_all_commands(&mut self);
    fn set_notice(&mut self, owner: &WorkspaceKey, notice: String);
    fn accept_composer(&mut self, owner: &WorkspaceKey, command_id: u64) -> bool;
    fn reject_composer(&mut self, owner: &WorkspaceKey, command_id: u64, notice: String) -> bool;
    fn submitted_composer_command(&self, owner: &WorkspaceKey) -> Option<u64>;
    fn reject_pending_composer(&mut self, owner: &WorkspaceKey, message: String);
    fn set_file_review_ready(&mut self, owner: &WorkspaceKey, review: CodingAgentFileReview);
    fn set_file_review_failed(
        &mut self,
        owner: &WorkspaceKey,
        request: CodingAgentFileReviewRequest,
        code: String,
    );
    fn apply_model_thinking_selection(
        &mut self,
        owner: &WorkspaceKey,
        thinking_level: Option<CodingAgentThinkingLevel>,
        thinking_fallback: bool,
    );
    fn selected_model_label(&self, owner: &WorkspaceKey) -> String;
    fn selected_profile_label(&self, owner: &WorkspaceKey) -> String;
    fn apply_projection_event(
        &mut self,
        owner: &WorkspaceKey,
        event: Option<ProjectionEvent>,
        creates_session_from_prompt: bool,
        completed_prompt_command: Option<u64>,
    ) -> ProjectionUpdateResult;
    fn request_resync_if_needed(&mut self, owner: &WorkspaceKey);
    fn active_runtime_is_running(&self) -> bool;
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

fn reduce_runtime_update(
    port: &mut impl RuntimeUpdatePort,
    update: DesktopRuntimeUpdate,
) -> Transition {
    let _kind = runtime_update_kind(&update);
    let initial_foreground = port.active_workspace_key();
    let inherit_home_thinking = match &update {
        DesktopRuntimeUpdate::PromptAcceptedWithSession { .. }
        | DesktopRuntimeUpdate::PromptRejectedWithSession { .. } => true,
        DesktopRuntimeUpdate::SessionChanged { command_id, .. } => port.command_matches(
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
        let _ = port.transfer_command(
            *command_id,
            WorkspaceKey::session(&snapshot.session.session.session_id),
        );
    }

    let creates_session_from_prompt = match &update {
        DesktopRuntimeUpdate::PromptAcceptedWithSession { command_id, .. }
        | DesktopRuntimeUpdate::PromptRejectedWithSession { command_id, .. } => port
            .command_matches(
                *command_id,
                &initial_foreground,
                &DesktopCommandIntent::Prompt,
            ),
        _ => false,
    };
    if creates_session_from_prompt && let Some(snapshot) = runtime_update_hydrated_snapshot(&update)
    {
        let _ = port.install_hydrated_workspace(snapshot, inherit_home_thinking, true);
    }

    if let Some(command_id) = runtime_update_command_id(&update)
        && let Some(pending_owner) = port.command_owner(command_id)
        && let Some(observed_owner) = runtime_update_observed_workspace_key(&update)
        && pending_owner != observed_owner
    {
        port.require_command_owner_resync(&pending_owner, &observed_owner);
        let resync_owner = if port.workspace_exists(&pending_owner) {
            pending_owner
        } else {
            port.active_workspace_key()
        };
        port.request_resync_if_needed(&resync_owner);
        let foreground = port.active_workspace_key();
        return workspace_update_transition(
            &foreground,
            &resync_owner,
            runtime_base_changes(&update),
        );
    }

    if let DesktopRuntimeUpdate::SessionChanged { snapshot, .. } = &update {
        let target = WorkspaceKey::session(&snapshot.session.session.session_id);
        if port.active_workspace_key() != target
            && !port.install_hydrated_workspace(snapshot, inherit_home_thinking, true)
        {
            if let DesktopRuntimeUpdate::SessionChanged { command_id, .. } = &update
                && let Some(intent) = port.command_intent(*command_id).filter(|intent| {
                    matches!(
                        intent,
                        DesktopCommandIntent::CreateSession
                            | DesktopCommandIntent::OpenSession { .. }
                    )
                })
            {
                let owner = port.command_owner(*command_id).unwrap_or(target);
                if port.complete_command(*command_id, &owner, &intent) {
                    let foreground = port.active_workspace_key();
                    port.set_notice(
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
        .or_else(|| runtime_update_command_id(&update).and_then(|id| port.command_owner(id)))
        .filter(|owner| port.workspace_exists(owner))
        .unwrap_or_else(|| port.active_workspace_key());
    let completion_owner = runtime_update_observed_workspace_key(&update)
        .or_else(|| runtime_update_command_id(&update).and_then(|id| port.command_owner(id)))
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
            if port.complete_command(command_id, &owner, &intent) {
                let cancelled = port.remove_closed_workspace(&session_id);
                port.remove_catalog_session(&session_id);
                port.set_notice(
                    &port.active_workspace_key(),
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
        DesktopRuntimeUpdate::FileReviewed { command_id, review } => {
            let request = CodingAgentFileReviewRequest::new(review.change.clone(), review.revision);
            let owner = port.command_owner(command_id).unwrap_or(target.clone());
            if port.complete_command(
                command_id,
                &owner,
                &DesktopCommandIntent::FileReview { request },
            ) {
                port.set_file_review_ready(&owner, review);
                port.set_notice(&owner, "Changed-file review loaded.".into());
                changes.insert(UiRegion::Inspector);
            }
            let foreground = port.active_workspace_key();
            return workspace_update_transition(&foreground, &owner, changes);
        }
        DesktopRuntimeUpdate::ExternalEditorOpened {
            command_id,
            project_relative_path,
        } => {
            let owner = port.command_owner(command_id).unwrap_or(target.clone());
            if port.complete_command(
                command_id,
                &owner,
                &DesktopCommandIntent::ExternalEditor {
                    project_relative_path: project_relative_path.clone(),
                },
            ) {
                port.set_notice(
                    &owner,
                    format!(
                        "Opened {} in the configured editor.",
                        truncate_label(&project_relative_path, 48)
                    ),
                );
                changes.insert(UiRegion::Inspector);
            }
            let foreground = port.active_workspace_key();
            return workspace_update_transition(&foreground, &owner, changes);
        }
        DesktopRuntimeUpdate::SessionsListed {
            command_id,
            sessions,
            omitted,
        } => {
            if let Some(owner) = port.command_owner(command_id)
                && port.complete_command(command_id, &owner, &DesktopCommandIntent::ListSessions)
            {
                port.replace_catalog(sessions, omitted);
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
            if port.complete_command(command_id, &owner, &intent) {
                port.rename_catalog_session(&session_id, name, updated_at);
                port.set_notice(&port.active_workspace_key(), "Session name updated.".into());
                changes.insert(UiRegion::Sessions);
            }
            return Transition::from_changes(changes);
        }
        DesktopRuntimeUpdate::SessionNameObserved {
            session_id,
            name,
            updated_at,
        } => {
            if port.rename_catalog_session(&session_id, name, updated_at) {
                changes.insert(UiRegion::Sessions);
            }
            return Transition::from_changes(changes);
        }
        update => {
            let completion = capture_projection_completion(port, &target, &update);
            reduce_pre_projection_update(port, &target, &completion_owner, &update, &mut changes);
            let completed_prompt_command = match &update {
                DesktopRuntimeUpdate::PromptFinished { command_id, .. } => Some(*command_id),
                _ => None,
            };
            let event = projection_event(update);
            let projection = port.apply_projection_event(
                &target,
                event,
                creates_session_from_prompt,
                completed_prompt_command,
            );
            changes.merge(projection.changes());
            reconcile_projection_completion(port, &target, completion, projection, &mut changes);
            port.request_resync_if_needed(&target);
        }
    }

    let foreground = port.active_workspace_key();
    workspace_update_transition(&foreground, &target, changes)
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
        | DesktopRuntimeUpdate::SessionsListed { .. }
        | DesktopRuntimeUpdate::SessionRenamed { .. }
        | DesktopRuntimeUpdate::SessionNameObserved { .. }
        | DesktopRuntimeUpdate::PromptAccepted { .. }
        | DesktopRuntimeUpdate::ControlAccepted { .. }
        | DesktopRuntimeUpdate::AuthorizationDecisionAccepted { .. }
        | DesktopRuntimeUpdate::FileReviewed { .. }
        | DesktopRuntimeUpdate::ExternalEditorOpened { .. } => None,
    }
}

fn capture_projection_completion(
    port: &impl RuntimeUpdatePort,
    owner: &WorkspaceKey,
    update: &DesktopRuntimeUpdate,
) -> ProjectionCompletion {
    match update {
        DesktopRuntimeUpdate::Reloaded {
            command_id,
            metadata,
        } if port.command_matches(*command_id, owner, &DesktopCommandIntent::Reload) => {
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
        } if port.command_matches(
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
        } if port.command_matches(
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
            if port.command_matches(*command_id, owner, &DesktopCommandIntent::Resync) =>
        {
            ProjectionCompletion::Resync {
                command_id: *command_id,
            }
        }
        DesktopRuntimeUpdate::SessionChanged { command_id, .. } => port
            .command_intent(*command_id)
            .filter(|intent| {
                matches!(
                    intent,
                    DesktopCommandIntent::CreateSession | DesktopCommandIntent::OpenSession { .. }
                )
            })
            .and_then(|intent| {
                port.command_owner(*command_id)
                    .map(|owner| ProjectionCompletion::Session {
                        owner,
                        command_id: *command_id,
                        intent,
                    })
            })
            .unwrap_or(ProjectionCompletion::None),
        _ => ProjectionCompletion::None,
    }
}

fn reduce_pre_projection_update(
    port: &mut impl RuntimeUpdatePort,
    target: &WorkspaceKey,
    completion_owner: &WorkspaceKey,
    update: &DesktopRuntimeUpdate,
    changes: &mut UiChangeSet,
) {
    match update {
        DesktopRuntimeUpdate::PromptAccepted { command_id }
        | DesktopRuntimeUpdate::PromptAcceptedWithSession { command_id, .. } => {
            if port.complete_command(*command_id, completion_owner, &DesktopCommandIntent::Prompt)
                && port.accept_composer(target, *command_id)
            {
                changes.insert(UiRegion::Sessions);
            }
        }
        DesktopRuntimeUpdate::PromptRejectedWithSession {
            command_id, error, ..
        } => {
            if port
                .reject_command(
                    *command_id,
                    completion_owner,
                    DesktopRuntimeCommandKind::SubmitPrompt,
                )
                .is_some()
            {
                port.reject_composer(
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
            if port
                .submitted_composer_command(target)
                .is_some_and(|submitted| submitted != *command_id)
            {
                port.set_notice(
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
            if port.complete_command(*command_id, completion_owner, &intent) {
                port.set_notice(
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
            if port.complete_command(*command_id, completion_owner, &intent)
                && port.accept_composer(target, *command_id)
            {
                port.set_notice(
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
            let intent = port.command_intent(*command_id).filter(|intent| {
                matches!(
                    intent,
                    DesktopCommandIntent::Authorization {
                        authorization_id: pending,
                        ..
                    } if pending == authorization_id
                )
            });
            if let Some(intent) = intent
                && port.complete_command(*command_id, completion_owner, &intent)
            {
                let decision = match decision {
                    ToolAuthorizationDecision::AllowOnce => "allow once",
                    ToolAuthorizationDecision::AllowForOperation => "allow for operation",
                    ToolAuthorizationDecision::Deny { .. } => "deny",
                };
                port.set_notice(
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
            port,
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
            let _ =
                port.complete_command(*command_id, completion_owner, &DesktopCommandIntent::Prompt);
            port.complete_operation_commands(completion_owner, operation_id);
            if let Some(error) = error {
                port.set_notice(
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
            if port.catalog_is_loading() {
                port.fail_catalog(message.clone());
            }
            port.cancel_all_commands();
            port.reject_pending_composer(target, message);
        }
        DesktopRuntimeUpdate::Stopped => {
            changes.insert(UiRegion::Sessions);
            let message = "desktop runtime stopped".to_owned();
            if port.catalog_is_loading() {
                port.fail_catalog(message.clone());
            }
            port.cancel_all_commands();
            port.reject_pending_composer(target, message);
        }
        DesktopRuntimeUpdate::Reloaded { .. }
        | DesktopRuntimeUpdate::Resynced { .. }
        | DesktopRuntimeUpdate::SessionChanged { .. }
        | DesktopRuntimeUpdate::SessionClosed { .. }
        | DesktopRuntimeUpdate::SessionsListed { .. }
        | DesktopRuntimeUpdate::SessionRenamed { .. }
        | DesktopRuntimeUpdate::SessionNameObserved { .. }
        | DesktopRuntimeUpdate::SelectionChanged { .. }
        | DesktopRuntimeUpdate::ProductEvent { .. }
        | DesktopRuntimeUpdate::ResyncRequired { .. }
        | DesktopRuntimeUpdate::RecoveryChanged { .. }
        | DesktopRuntimeUpdate::FileReviewed { .. }
        | DesktopRuntimeUpdate::ExternalEditorOpened { .. }
        | DesktopRuntimeUpdate::ControlAccepted { .. } => {}
    }
}

fn reduce_command_rejected(
    port: &mut impl RuntimeUpdatePort,
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
    let rejected = port.reject_command(command_id, completion_owner, command);
    let Some(intent) = rejected else {
        return;
    };
    match command {
        DesktopRuntimeCommandKind::SubmitPrompt => {
            port.reject_composer(
                target,
                command_id,
                safe_runtime_rejection_notice(command, code),
            );
            changes.insert(UiRegion::Sessions);
        }
        DesktopRuntimeCommandKind::Abort => {
            port.set_notice(target, safe_runtime_rejection_notice(command, code));
        }
        DesktopRuntimeCommandKind::Reload => port.set_notice(
            target,
            format!(
                "Reload failed ({}); previous context retained.",
                truncate_label(code, 28)
            ),
        ),
        DesktopRuntimeCommandKind::SelectModel
        | DesktopRuntimeCommandKind::SelectSessionProfile => port.set_notice(
            target,
            format!(
                "{command:?} failed ({}); previous selection retained.",
                truncate_label(code, 28)
            ),
        ),
        DesktopRuntimeCommandKind::Steer | DesktopRuntimeCommandKind::FollowUp => {
            let notice = safe_runtime_rejection_notice(command, code);
            if port.reject_composer(target, command_id, notice.clone()) {
                port.set_notice(target, notice);
            }
        }
        DesktopRuntimeCommandKind::DecideToolAuthorization => {
            port.set_notice(target, safe_runtime_rejection_notice(command, code));
        }
        DesktopRuntimeCommandKind::RetryRecovery | DesktopRuntimeCommandKind::ResolveRecovery => {
            port.set_notice(target, safe_runtime_rejection_notice(command, code));
            changes.insert(UiRegion::Inspector);
        }
        DesktopRuntimeCommandKind::Resync
        | DesktopRuntimeCommandKind::CreateSession
        | DesktopRuntimeCommandKind::OpenSession
        | DesktopRuntimeCommandKind::CloseSession => {
            port.set_notice(target, safe_runtime_rejection_notice(command, code));
            changes.insert(UiRegion::Sessions);
        }
        DesktopRuntimeCommandKind::ListSessions => {
            let notice = safe_runtime_rejection_notice(command, code);
            port.fail_catalog(notice.clone());
            port.set_notice(target, notice);
            changes.insert(UiRegion::Sessions);
        }
        DesktopRuntimeCommandKind::ReviewChangedFile => {
            if let DesktopCommandIntent::FileReview { request } = intent {
                port.set_file_review_failed(target, request, code.to_owned());
                port.set_notice(
                    target,
                    format!("File review unavailable ({}).", truncate_label(code, 32)),
                );
                changes.insert(UiRegion::Inspector);
            }
        }
        DesktopRuntimeCommandKind::OpenExternalEditor => {
            port.set_notice(
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

fn reconcile_projection_completion(
    port: &mut impl RuntimeUpdatePort,
    target: &WorkspaceKey,
    completion: ProjectionCompletion,
    projection: ProjectionUpdateResult,
    changes: &mut UiChangeSet,
) {
    let replaced = projection.replaced();
    match completion {
        ProjectionCompletion::None => {}
        ProjectionCompletion::Resync { command_id } => {
            if port.complete_command(command_id, target, &DesktopCommandIntent::Resync) {
                port.set_notice(
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
            let owner = port.command_owner(command_id).unwrap_or(owner);
            if port.complete_command(command_id, &owner, &intent) {
                let created = matches!(intent, DesktopCommandIntent::CreateSession);
                port.set_notice(
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
                if replaced && created && port.insert_session_into_catalog(target) {
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
            if port.complete_command(command_id, target, &DesktopCommandIntent::Reload) {
                port.set_notice(
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
            if port.complete_command(
                command_id,
                target,
                &DesktopCommandIntent::Selection(selection),
            ) {
                if replaced && selection == DesktopRuntimeSelectionKind::Model {
                    port.apply_model_thinking_selection(target, thinking_level, thinking_fallback);
                }
                let notice = if replaced {
                    match selection {
                        DesktopRuntimeSelectionKind::Model => format!(
                            "Future prompts will use model {}.",
                            truncate_label(&port.selected_model_label(target), 28)
                        ),
                        DesktopRuntimeSelectionKind::SessionProfile => format!(
                            "Session profile changed to {}.",
                            truncate_label(&port.selected_profile_label(target), 28)
                        ),
                    }
                } else {
                    "Selection response failed projection validation; resync is required.".into()
                };
                port.set_notice(target, notice);
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
            if port.complete_command(command_id, target, &intent) {
                port.set_notice(
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
pub(crate) enum UiIntent {
    SetProjectCollapsed { group_id: String, collapsed: bool },
}

#[derive(Debug, Clone)]
pub(crate) enum DesktopEvent {
    Ui(UiIntent),
    Runtime(Box<DesktopRuntimeUpdate>),
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

    pub(crate) fn effects(&self) -> &[DesktopEffect] {
        &self.effects
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
}

impl DesktopController {
    pub(crate) const fn new() -> Self {
        Self {
            next_effect_request_id: 0,
        }
    }

    pub(crate) fn reduce_runtime(
        port: &mut impl RuntimeUpdatePort,
        update: DesktopRuntimeUpdate,
    ) -> Transition {
        reduce_runtime_update(port, update)
    }

    /// Route an event through one mutable application-state authority while
    /// feature branches are migrated from the GPUI adapter in later tasks.
    pub(crate) fn reduce<Workspace, Catalog>(
        &mut self,
        state: &mut DesktopState<Workspace, Catalog>,
        event: DesktopEvent,
        delegate: impl FnOnce(&mut DesktopState<Workspace, Catalog>, DesktopEvent) -> Transition,
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
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use desktop::preferences::DesktopPreferences;

    use super::{DesktopController, DesktopEvent, RuntimeUpdateKind, Transition, UiIntent};
    use crate::application::{
        change_set::UiRegion,
        commands::CommandTracker,
        effect::{
            DesktopEffect, DesktopPickerKind, DesktopTimer, DesktopTimerKind, EffectIdentity,
            PlatformOutcome, PlatformResult,
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

    #[test]
    fn runtime_update_coverage_table_registers_all_twenty_three_protocol_variants() {
        let labels = RuntimeUpdateKind::ALL.map(RuntimeUpdateKind::label);
        assert_eq!(
            labels,
            [
                "reloaded",
                "resynced",
                "session_changed",
                "session_closed",
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
                "external_editor_opened",
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
            DesktopEvent::Ui(UiIntent::SetProjectCollapsed {
                group_id: "project:alpha".into(),
                collapsed: true,
            }),
            |state, event| {
                let DesktopEvent::Ui(UiIntent::SetProjectCollapsed { group_id, .. }) = event else {
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
            outcome: PlatformOutcome::Failed,
        };
        assert!(!same_kind.matches_platform_result(&wrong_owner));

        let editor = DesktopEffect::OpenExternalEditor {
            identity: identity.clone(),
        };
        let editor_result = PlatformResult::ExternalEditorOpened {
            identity: identity.clone(),
            outcome: PlatformOutcome::Completed(()),
        };
        assert!(editor.matches_platform_result(&editor_result));

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
}
