use std::sync::Arc;

use coding_agent::api::{
    embedding::CodingAgentThinkingLevel,
    review::{CodingAgentFileReview, CodingAgentFileReviewRequest},
    view::{CodingAgentWorkspaceMigration, CodingAgentWorkspaceMigrationOutcome},
};
use desktop::{
    conversation::{ComposerAdmission, ComposerState},
    preferences::DesktopThinkingLevel,
    projection::{DesktopProjection, DesktopProjectionDelta, DesktopProjectionLifecycle},
    runtime::{
        DesktopRuntimeCommandKind, DesktopRuntimeHydratedSnapshot, DesktopSessionCatalogEntry,
    },
    shell::truncate_label,
};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

use crate::projection::{DesktopProjectionApply, ProjectionEvent};

use super::{
    catalog::ProjectCatalogController,
    change_set::{UiChangeSet, UiRegion},
    commands::{CommandCompletionError, DesktopCommandIntent},
    state::DesktopState,
    workspace::{SessionId, WorkspaceKey},
    workspace_state::{
        DesktopFileReviewState, MAX_SESSION_WORKSPACES, RuntimeWorkspaceDefaults, WorkspaceState,
        admitted_thinking_selection, workspace_selection_from_embedding,
    },
};

/// The only feature-owned behavior required by the application runtime reducer.
///
/// Implementations update derived presentation caches only. Runtime ownership,
/// command admission, projection state, catalog state, and preferences remain
/// application facts and never cross back through the GPUI root.
pub(crate) trait RuntimeWorkspacePresentation: Default {
    fn mark_composer_accepted(&mut self);

    fn reconcile_projection(
        &mut self,
        composer: &mut ComposerState,
        update: RuntimeProjectionPresentation<'_>,
    ) -> bool;
}

pub(crate) struct RuntimeProjectionPresentation<'a> {
    pub(crate) projection: &'a DesktopProjection,
    pub(crate) replaced: bool,
    pub(crate) delta: Option<&'a DesktopProjectionDelta>,
    pub(crate) sequence: u64,
    pub(crate) completes_submitted_prompt: bool,
    pub(crate) active_operation_after: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ProjectionUpdateResult {
    replaced: bool,
    needs_resync: bool,
    changes: UiChangeSet,
}

impl ProjectionUpdateResult {
    pub(crate) const fn new(replaced: bool, needs_resync: bool, changes: UiChangeSet) -> Self {
        Self {
            replaced,
            needs_resync,
            changes,
        }
    }

    pub(crate) const fn replaced(self) -> bool {
        self.replaced
    }

    pub(crate) const fn changes(self) -> UiChangeSet {
        self.changes
    }

    pub(crate) const fn needs_resync(self) -> bool {
        self.needs_resync
    }
}

impl<Presentation: RuntimeWorkspacePresentation>
    DesktopState<WorkspaceState<Presentation>, ProjectCatalogController, RuntimeWorkspaceDefaults>
{
    pub(crate) fn complete_runtime_command(
        &mut self,
        command_id: u64,
        observed_owner: &WorkspaceKey,
        intent: &DesktopCommandIntent,
    ) -> bool {
        let pending_owner = self.commands.owner(command_id).cloned();
        match self.commands.complete(command_id, observed_owner, intent) {
            Ok(_) => true,
            Err(CommandCompletionError::OwnerMismatch) => {
                if let Some(pending_owner) = pending_owner {
                    self.require_command_owner_resync(&pending_owner, observed_owner);
                }
                false
            }
            Err(
                CommandCompletionError::UnknownCommand | CommandCompletionError::IntentMismatch,
            ) => false,
        }
    }

    pub(crate) fn reject_runtime_command(
        &mut self,
        command_id: u64,
        observed_owner: &WorkspaceKey,
        command: DesktopRuntimeCommandKind,
    ) -> Option<DesktopCommandIntent> {
        let pending_owner = self.commands.owner(command_id).cloned();
        match self.commands.reject(command_id, observed_owner, command) {
            Ok(pending) => Some(pending.into_intent()),
            Err(CommandCompletionError::OwnerMismatch) => {
                if let Some(pending_owner) = pending_owner {
                    self.require_command_owner_resync(&pending_owner, observed_owner);
                }
                None
            }
            Err(
                CommandCompletionError::UnknownCommand | CommandCompletionError::IntentMismatch,
            ) => None,
        }
    }

    pub(crate) fn complete_matching_runtime_command(
        &mut self,
        owner: &WorkspaceKey,
        predicate: impl Fn(&DesktopCommandIntent) -> bool,
    ) -> Option<DesktopCommandIntent> {
        let (command_id, intent) = self.commands.find(owner, predicate)?;
        self.complete_runtime_command(command_id, owner, &intent)
            .then_some(intent)
    }

    pub(crate) fn complete_operation_commands(&mut self, owner: &WorkspaceKey, operation_id: &str) {
        self.complete_matching_runtime_command(owner, |intent| {
            matches!(
                intent,
                DesktopCommandIntent::Abort {
                    operation_id: pending,
                } if pending == operation_id
            )
        });
        self.complete_matching_runtime_command(owner, |intent| {
            matches!(
                intent,
                DesktopCommandIntent::Authorization {
                    operation_id: pending,
                    ..
                } if pending == operation_id
            )
        });
    }

    pub(crate) fn require_command_owner_resync(
        &mut self,
        pending_owner: &WorkspaceKey,
        observed_owner: &WorkspaceKey,
    ) {
        let mut marked = false;
        for owner in [pending_owner, observed_owner] {
            let Some(workspace) = self.workspaces.get_mut(owner) else {
                continue;
            };
            marked = true;
            if let Some(projection) = workspace.projection.as_mut() {
                projection.require_command_resync(
                    "command_owner_mismatch",
                    "runtime command completion targeted a different workspace",
                );
            }
            workspace.set_preference_notice(
                "Runtime response targeted another session; resync is required.".into(),
            );
        }
        if !marked {
            let workspace = self.workspaces.active_mut();
            if let Some(projection) = workspace.projection.as_mut() {
                projection.require_command_resync(
                    "command_owner_mismatch",
                    "runtime command completion targeted a different workspace",
                );
            }
            workspace.set_preference_notice(
                "Runtime response targeted another session; resync is required.".into(),
            );
        }
    }

    pub(crate) fn install_hydrated_workspace(
        &mut self,
        snapshot: &DesktopRuntimeHydratedSnapshot,
        inherit_home_thinking: bool,
        activate: bool,
    ) -> bool {
        let target_session_id = SessionId::from_dto(&snapshot.session.session.session_id);
        let target_key = WorkspaceKey::Session(target_session_id.clone());
        if self.workspaces.active_key() == &target_key {
            return true;
        }
        if self.workspaces.contains(&target_key) {
            return !activate || self.workspaces.activate(&target_key);
        }
        if self.workspaces.session_count() >= MAX_SESSION_WORKSPACES {
            self.workspaces.active_mut().set_preference_notice(format!(
                "Up to {MAX_SESSION_WORKSPACES} sessions can stay open; close one first."
            ));
            return false;
        }
        let projection = match DesktopProjection::new(snapshot.clone()) {
            Ok(projection) => projection,
            Err(issue) => {
                self.workspaces.active_mut().set_preference_notice(format!(
                    "Session response failed projection validation ({}).",
                    truncate_label(&issue.code, 28)
                ));
                return false;
            }
        };
        let promoting_home = activate && self.workspaces.active_key() == &WorkspaceKey::Home;
        let thinking_selection = if inherit_home_thinking && promoting_home {
            self.workspaces.active().thinking_selection
        } else {
            self.preferences
                .thinking_level_for_session(target_session_id.as_str())
        };
        if promoting_home {
            let defaults = self.workspace_defaults.clone();
            {
                let workspace = self.workspaces.active_mut();
                workspace.project = snapshot.project.clone();
                workspace.projection = Some(projection);
                workspace.thinking_selection = thinking_selection;
            }
            self.reconcile_thinking_selection(&WorkspaceKey::Home);
            let admitted_selection = self.workspaces.active().thinking_selection;
            if self
                .preferences
                .set_thinking_level_for_session(target_session_id.as_str(), admitted_selection)
            {
                self.mark_runtime_preferences_dirty();
            }
            let fresh_home = new_workspace(
                defaults.home_project,
                None,
                None,
                thinking_selection,
                defaults.projectless_selection,
            );
            self.commands.transfer_owner(
                &WorkspaceKey::Home,
                &WorkspaceKey::Session(target_session_id.clone()),
            );
            let promoted = self.workspaces.promote_home(target_session_id, fresh_home);
            debug_assert!(
                promoted.is_ok(),
                "new session must promote the active Home entry"
            );
            return true;
        }
        let target = new_workspace(
            snapshot.project.clone(),
            Some(projection),
            None,
            thinking_selection,
            workspace_selection_from_embedding(&snapshot.project),
        );
        self.workspaces
            .insert_session(target_session_id.clone(), target);
        !activate
            || self
                .workspaces
                .activate(&WorkspaceKey::Session(target_session_id))
    }

    pub(crate) fn remove_closed_workspace(&mut self, session_id: &str) -> usize {
        let owner = WorkspaceKey::session(session_id);
        let cancelled = self.commands.cancel_owner(&owner).len();
        self.workspaces
            .remove_session(&SessionId::from_dto(session_id));
        cancelled
    }

    pub(crate) fn insert_session_into_catalog(&mut self, owner: &WorkspaceKey) -> bool {
        let Some(workspace_state) = self.workspaces.get(owner) else {
            return false;
        };
        let Some(projection) = workspace_state.projection.as_ref() else {
            return false;
        };
        let session_id = projection.snapshot().session.session_id.clone();
        let (workspace, workspace_migration) =
            workspace_state.project.workspace.as_ref().map_or_else(
                || {
                    let fallback = DesktopSessionCatalogEntry::default();
                    (fallback.workspace, fallback.workspace_migration)
                },
                |workspace| {
                    (
                        workspace.overview.clone(),
                        CodingAgentWorkspaceMigration {
                            outcome: CodingAgentWorkspaceMigrationOutcome::NotRequired,
                            diagnostic: None,
                        },
                    )
                },
            );
        let observed_at = OffsetDateTime::now_utc()
            .format(&Rfc3339)
            .unwrap_or_default();
        self.catalog
            .insert_created_session(DesktopSessionCatalogEntry {
                session_id,
                name: None,
                workspace,
                workspace_migration,
                created_at: observed_at.clone(),
                updated_at: observed_at,
                active_leaf_id: None,
            });
        true
    }

    pub(crate) fn set_runtime_notice(&mut self, owner: &WorkspaceKey, notice: String) {
        if let Some(workspace) = self.workspaces.get_mut(owner) {
            workspace.set_preference_notice(notice);
        }
    }

    pub(crate) fn accept_composer(&mut self, owner: &WorkspaceKey, command_id: u64) -> bool {
        let Some(workspace) = self.workspaces.get_mut(owner) else {
            return false;
        };
        if workspace.composer.accepted(command_id).is_err() {
            return false;
        }
        workspace.composer_attachments.clear();
        workspace.composer_needs_sync = true;
        workspace.presentation.mark_composer_accepted();
        true
    }

    pub(crate) fn reject_composer(
        &mut self,
        owner: &WorkspaceKey,
        command_id: u64,
        notice: String,
    ) -> bool {
        self.workspaces
            .get_mut(owner)
            .is_some_and(|workspace| workspace.composer.rejected(command_id, notice).is_ok())
    }

    pub(crate) fn submitted_composer_command(&self, owner: &WorkspaceKey) -> Option<u64> {
        self.workspaces
            .get(owner)
            .and_then(|workspace| workspace.composer.submitted())
            .map(|submitted| submitted.command_id)
    }

    pub(crate) fn reject_pending_composer(&mut self, owner: &WorkspaceKey, message: String) {
        let command_id =
            self.workspaces
                .get(owner)
                .and_then(|workspace| match workspace.composer.admission() {
                    ComposerAdmission::Pending { command_id, .. } => Some(*command_id),
                    ComposerAdmission::Idle => None,
                });
        if let Some(command_id) = command_id
            && self.reject_composer(owner, command_id, message)
            && let Some(workspace) = self.workspaces.get_mut(owner)
        {
            workspace.composer_needs_sync = true;
        }
    }

    pub(crate) fn set_file_review_ready(
        &mut self,
        owner: &WorkspaceKey,
        review: CodingAgentFileReview,
    ) {
        if let Some(workspace) = self.workspaces.get_mut(owner) {
            workspace.file_review = Arc::new(DesktopFileReviewState::Ready(
                desktop::ui::inspector::review::DesktopFileReviewDocument::from_product(review),
            ));
        }
    }

    pub(crate) fn set_file_review_failed(
        &mut self,
        owner: &WorkspaceKey,
        request: CodingAgentFileReviewRequest,
        code: String,
    ) {
        if let Some(workspace) = self.workspaces.get_mut(owner) {
            workspace.file_review = Arc::new(DesktopFileReviewState::Failed { request, code });
        }
    }

    pub(crate) fn apply_model_thinking_selection(
        &mut self,
        owner: &WorkspaceKey,
        thinking_level: Option<CodingAgentThinkingLevel>,
        thinking_fallback: bool,
    ) {
        let selection = DesktopThinkingLevel::from_explicit(thinking_level);
        let session_id = self.workspaces.get_mut(owner).and_then(|workspace| {
            workspace.thinking_selection = selection;
            workspace.thinking_hint = thinking_fallback
                .then(|| Arc::from("Thinking reset to Auto for the selected model."));
            workspace
                .projection
                .as_ref()
                .map(|projection| projection.snapshot().session.session_id.clone())
        });
        if let Some(session_id) = session_id.as_deref()
            && self
                .preferences
                .set_thinking_level_for_session(session_id, selection)
        {
            self.mark_runtime_preferences_dirty();
        }
    }

    pub(crate) fn selected_model_label(&self, owner: &WorkspaceKey) -> String {
        self.workspaces
            .get(owner)
            .map(|workspace| workspace.project.selected_model_id.clone())
            .unwrap_or_default()
    }

    pub(crate) fn selected_profile_label(&self, owner: &WorkspaceKey) -> String {
        self.workspaces
            .get(owner)
            .map(|workspace| {
                workspace
                    .projection
                    .as_ref()
                    .map(|projection| {
                        projection
                            .snapshot()
                            .session
                            .default_agent_profile_id
                            .as_str()
                            .to_owned()
                    })
                    .unwrap_or_else(|| {
                        workspace
                            .project
                            .default_agent_profile_id
                            .as_str()
                            .to_owned()
                    })
            })
            .unwrap_or_default()
    }

    pub(crate) fn apply_projection_event(
        &mut self,
        owner: &WorkspaceKey,
        event: Option<ProjectionEvent>,
        creates_session_from_prompt: bool,
        completed_prompt_command: Option<u64>,
    ) -> ProjectionUpdateResult {
        let Some(event) = event else {
            return ProjectionUpdateResult::new(false, false, UiChangeSet::default());
        };
        let composer_state_before = self.composer_state(owner);
        let projection_was_none = self
            .workspaces
            .get(owner)
            .is_none_or(|workspace| workspace.projection.is_none());
        if projection_was_none {
            if let ProjectionEvent::Hydrated { snapshot, .. } = &event {
                if let Some(workspace) = self.workspaces.get_mut(owner) {
                    workspace.project = snapshot.project.clone();
                    match DesktopProjection::new(snapshot.clone()) {
                        Ok(projection) => workspace.projection = Some(projection),
                        Err(issue) => workspace.set_preference_notice(format!(
                            "Session response failed projection validation ({}).",
                            truncate_label(&issue.code, 28)
                        )),
                    }
                }
                self.reconcile_thinking_selection(owner);
            } else if let Some(metadata) = match &event {
                ProjectionEvent::Metadata(metadata)
                | ProjectionEvent::PromptStarted { metadata, .. } => Some(metadata),
                _ => None,
            } {
                if let Some(workspace) = self.workspaces.get_mut(owner) {
                    workspace.project = metadata.project.clone();
                }
                if self.workspaces.active_key() == owner {
                    self.workspace_defaults.home_project = metadata.project.clone();
                }
                self.reconcile_thinking_selection(owner);
            }
        }

        if creates_session_from_prompt
            && self
                .workspaces
                .get(owner)
                .is_some_and(|workspace| workspace.projection.is_some())
            && self.workspaces.active_key() == owner
        {
            let _ = self.insert_session_into_catalog(owner);
        }

        let Some(workspace) = self.workspaces.get(owner) else {
            return ProjectionUpdateResult::new(false, false, UiChangeSet::default());
        };
        if workspace.projection.is_none() {
            return ProjectionUpdateResult::new(true, false, UiChangeSet::default());
        }

        let completes_submitted_prompt = completed_prompt_command
            .is_some_and(|command_id| self.submitted_composer_command(owner) == Some(command_id));
        let (had_active_operation, outcome, project_after, active_operation_after, sequence_after) = {
            let workspace = self
                .workspaces
                .get_mut(owner)
                .expect("runtime reducer target must exist");
            let projection = workspace
                .projection
                .as_mut()
                .expect("projection availability was checked");
            let had_active_operation = projection.snapshot().active_operation.is_some();
            let outcome = projection.apply(event);
            let project_after = projection.project().clone();
            let active_operation_after = projection.snapshot().active_operation.is_some();
            let sequence_after = projection.cursor().last_event_sequence;
            (
                had_active_operation,
                outcome,
                project_after,
                active_operation_after,
                sequence_after,
            )
        };
        self.workspaces
            .get_mut(owner)
            .expect("runtime reducer target must exist")
            .project = project_after;
        self.reconcile_thinking_selection(owner);

        let delta = outcome.delta();
        let file_changes_dirty = delta.is_some_and(|delta| {
            delta
                .context
                .contains(desktop::projection::ContextDirtyFlags::CHANGES)
        });
        let mut changes = UiChangeSet::for_projection(outcome.is_replaced(), delta);
        if had_active_operation != active_operation_after {
            changes.insert(UiRegion::Sessions);
        }
        if let Some(workspace) = self.workspaces.get_mut(owner) {
            let WorkspaceState {
                projection,
                composer,
                composer_needs_sync,
                presentation,
                ..
            } = workspace;
            let projection = projection
                .as_ref()
                .expect("projection availability was checked");
            *composer_needs_sync |= presentation.reconcile_projection(
                composer,
                RuntimeProjectionPresentation {
                    projection,
                    replaced: outcome.is_replaced(),
                    delta,
                    sequence: sequence_after,
                    completes_submitted_prompt,
                    active_operation_after,
                },
            );
        }

        let authorization = self.commands.authorization(owner).map(
            |(command_id, authorization_id, operation_id)| {
                (
                    command_id,
                    authorization_id.to_owned(),
                    operation_id.to_owned(),
                )
            },
        );
        if let Some((command_id, authorization_id, operation_id)) = authorization
            && !self
                .workspaces
                .get(owner)
                .and_then(|workspace| workspace.projection.as_ref())
                .is_some_and(|projection| {
                    projection
                        .snapshot()
                        .pending_authorizations
                        .iter()
                        .any(|request| request.authorization_id == authorization_id)
                })
        {
            let intent = DesktopCommandIntent::Authorization {
                authorization_id,
                operation_id,
            };
            let _ = self.complete_runtime_command(command_id, owner, &intent);
        }
        if outcome.is_replaced() || file_changes_dirty {
            self.reconcile_file_review(owner);
        }
        if composer_state_before != self.composer_state(owner) {
            changes.insert(UiRegion::Composer);
            changes.insert(UiRegion::Inspector);
            changes.insert(UiRegion::Toast);
            changes.insert(UiRegion::ConversationHeader);
            changes.insert(UiRegion::Modal);
        }
        ProjectionUpdateResult::new(
            outcome.is_replaced(),
            matches!(outcome, DesktopProjectionApply::NeedsResync),
            changes,
        )
    }

    pub(crate) fn reserve_resync_command(&mut self, owner: &WorkspaceKey) -> Option<u64> {
        if !self
            .workspaces
            .get(owner)
            .and_then(|workspace| workspace.projection.as_ref())
            .is_some_and(|projection| {
                projection.lifecycle() == DesktopProjectionLifecycle::NeedsResync
            })
            || self.commands.contains(owner, &DesktopCommandIntent::Resync)
        {
            return None;
        }
        match self
            .commands
            .reserve(owner.clone(), DesktopCommandIntent::Resync)
        {
            Ok(command_id) => Some(command_id),
            Err(error) => {
                self.set_runtime_notice(owner, error.to_string());
                None
            }
        }
    }

    pub(crate) fn abandon_resync_command(
        &mut self,
        owner: &WorkspaceKey,
        command_id: u64,
        message: String,
    ) {
        let _ = self
            .commands
            .complete(command_id, owner, &DesktopCommandIntent::Resync);
        self.set_runtime_notice(owner, message);
    }

    pub(crate) fn active_runtime_is_running(&self) -> bool {
        !self
            .workspaces
            .active()
            .projection
            .as_ref()
            .is_some_and(|projection| projection.lifecycle() == DesktopProjectionLifecycle::Stopped)
    }

    fn reconcile_thinking_selection(&mut self, owner: &WorkspaceKey) {
        let Some(workspace) = self.workspaces.get_mut(owner) else {
            return;
        };
        let (selection, fallback) =
            admitted_thinking_selection(&workspace.project, workspace.thinking_selection);
        if !fallback {
            return;
        }
        workspace.thinking_selection = selection;
        workspace.thinking_hint = Some(Arc::from("Thinking reset to Auto for the selected model."));
        let session_id = workspace
            .projection
            .as_ref()
            .map(|projection| projection.snapshot().session.session_id.clone());
        if let Some(session_id) = session_id.as_deref()
            && self
                .preferences
                .set_thinking_level_for_session(session_id, selection)
        {
            self.mark_runtime_preferences_dirty();
        }
    }

    fn reconcile_file_review(&mut self, owner: &WorkspaceKey) {
        let Some(workspace) = self.workspaces.get(owner) else {
            return;
        };
        let request = match workspace.file_review.as_ref() {
            DesktopFileReviewState::Empty => return,
            DesktopFileReviewState::Loading(request)
            | DesktopFileReviewState::Failed { request, .. } => request.clone(),
            DesktopFileReviewState::Ready(document) => document.request.clone(),
        };
        let remains_current = workspace.projection.as_ref().is_some_and(|projection| {
            projection.snapshot().context.changes.iter().any(|change| {
                change.operation_id == request.change.operation_id
                    && change.tool_call_id == request.change.tool_call_id
                    && change.path == request.change.path
                    && change.updated_sequence == request.revision.value()
            })
        });
        if remains_current {
            return;
        }
        self.complete_matching_runtime_command(owner, |intent| {
            matches!(
                intent,
                DesktopCommandIntent::FileReview {
                    request: pending,
                } if pending == &request
            )
        });
        if let Some(workspace) = self.workspaces.get_mut(owner) {
            workspace.file_review = Arc::new(DesktopFileReviewState::Empty);
        }
    }

    fn composer_state(&self, owner: &WorkspaceKey) -> (bool, bool, bool, bool) {
        let Some(workspace) = self.workspaces.get(owner) else {
            return (false, false, false, false);
        };
        (
            matches!(
                workspace.composer.admission(),
                desktop::conversation::ComposerAdmission::Pending { .. }
            ),
            workspace
                .projection
                .as_ref()
                .is_some_and(|projection| projection.snapshot().active_operation.is_some()),
            workspace.composer.submitted().is_some(),
            workspace.composer.rejection().is_some(),
        )
    }
}

fn new_workspace<Presentation: RuntimeWorkspacePresentation>(
    project: coding_agent::api::embedding::CodingAgentEmbeddingSnapshot,
    projection: Option<DesktopProjection>,
    preference_notice: Option<String>,
    thinking_selection: DesktopThinkingLevel,
    draft_workspace_selection: coding_agent::api::embedding::CodingAgentWorkspaceSelection,
) -> WorkspaceState<Presentation> {
    let (thinking_selection, thinking_fallback) =
        admitted_thinking_selection(&project, thinking_selection);
    WorkspaceState::new(
        project,
        projection,
        draft_workspace_selection,
        preference_notice,
        thinking_selection,
        thinking_fallback,
        Presentation::default(),
    )
}
