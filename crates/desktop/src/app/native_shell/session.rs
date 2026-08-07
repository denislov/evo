//! NativeShell session commands, workspace navigation, and preference flows.

use coding_agent::api::embedding::CodingAgentWorkspaceSelection;
use desktop::ui::shell::FocusTarget;
use gpui::{Context, Window};
use std::path::PathBuf;

use crate::application::{
    change_set::{UiChangeSet, UiRegion},
    commands::DesktopCommandIntent,
    workspace::WorkspaceKey,
};

use super::NativeShell;
use crate::ui::shell::{CenterNavigationTarget, CenterSurface};
impl NativeShell {
    pub(in crate::app) fn active_command_contains(&self, intent: &DesktopCommandIntent) -> bool {
        self.app
            .commands
            .contains(self.app.workspaces.active_key(), intent)
    }

    pub(in crate::app) fn active_command_contains_where(
        &self,
        predicate: impl Fn(&DesktopCommandIntent) -> bool,
    ) -> bool {
        self.app
            .commands
            .contains_where(self.app.workspaces.active_key(), predicate)
    }

    pub(in crate::app) fn complete_active_command(
        &mut self,
        command_id: u64,
        intent: &DesktopCommandIntent,
    ) -> bool {
        let owner = self.app.workspaces.active_key().clone();
        self.app
            .complete_runtime_command(command_id, &owner, intent)
    }

    pub(in crate::app) fn navigate_center(
        &mut self,
        target: CenterNavigationTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match target {
            CenterNavigationTarget::NewConversation => self.show_home_workspace(window, cx),
            CenterNavigationTarget::Skills => {
                self.ui.center_surface = CenterSurface::Skills;
                self.dismiss_drawer(window, cx, false);
                self.focus_target(FocusTarget::CenterBody, window, cx);
                self.refresh_views(UiChangeSet::one(UiRegion::Sessions), cx);
                cx.notify();
            }
            CenterNavigationTarget::Session(session_id) => {
                self.ui.center_surface = CenterSurface::Primary;
                self.dismiss_drawer(window, cx, false);
                self.focus_target(FocusTarget::CenterBody, window, cx);
                if self
                    .app
                    .workspaces
                    .active_mut()
                    .projection
                    .as_ref()
                    .is_some_and(|projection| {
                        projection.snapshot().session.session_id == session_id
                    })
                {
                    self.refresh_views(UiChangeSet::one(UiRegion::Sessions), cx);
                    cx.notify();
                } else {
                    self.open_session(session_id, cx);
                }
            }
        }
    }

    pub(in crate::app) fn show_home_workspace(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.ui.center_surface = CenterSurface::Primary;
        let activated = self.app.workspaces.activate(&WorkspaceKey::Home);
        debug_assert!(activated, "Home must remain a stable workspace entry");

        self.dismiss_drawer(window, cx, true);
        self.record_focus(FocusTarget::Composer, window, cx);
        self.refresh_views(UiChangeSet::one(UiRegion::Sessions), cx);
        self.refresh_views(UiChangeSet::one(UiRegion::Composer), cx);
        self.refresh_views(UiChangeSet::one(UiRegion::Conversation), cx);
        self.refresh_views(UiChangeSet::one(UiRegion::ConversationHeader), cx);
        self.refresh_views(UiChangeSet::one(UiRegion::Inspector), cx);
        self.refresh_views(UiChangeSet::one(UiRegion::Modal), cx);
        cx.notify();
    }

    pub(in crate::app) fn show_project_home_workspace(
        &mut self,
        path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let home = WorkspaceKey::Home;
        let Some(workspace) = self.app.workspaces.get_mut(&home) else {
            return;
        };
        if !workspace.project_directory_editable() {
            workspace.set_preference_notice(
                "The new conversation is still being prepared; try again when it is idle.".into(),
            );
            self.refresh_views(UiChangeSet::one(UiRegion::Toast), cx);
            cx.notify();
            return;
        }
        workspace.draft_workspace_selection = CodingAgentWorkspaceSelection::project(path);
        self.show_home_workspace(window, cx);
    }

    pub(in crate::app) fn open_workspace_count(&self) -> usize {
        self.app.workspaces.session_count()
    }

    pub(in crate::app) fn reserve_session_command(
        &mut self,
        session_id: &str,
        intent: DesktopCommandIntent,
    ) -> Result<u64, String> {
        let key = WorkspaceKey::session(session_id);
        if !self.app.workspaces.contains(&key) {
            return Err("Cannot close an unavailable session.".to_owned());
        }
        self.app
            .commands
            .reserve(key, intent)
            .map_err(|error| error.to_string())
    }

    pub(in crate::app) fn close_session(&mut self, session_id: &str, cx: &mut Context<Self>) {
        let intent = DesktopCommandIntent::CloseSession {
            session_id: session_id.to_owned(),
        };
        let command_id = match self.reserve_session_command(session_id, intent.clone()) {
            Ok(command_id) => command_id,
            Err(error) => {
                self.app
                    .workspaces
                    .active_mut()
                    .set_preference_notice(error);
                self.refresh_views(UiChangeSet::one(UiRegion::Sessions), cx);
                return;
            }
        };
        let admission = self
            .connection
            .runtime_client
            .as_ref()
            .ok_or_else(|| "desktop runtime is unavailable".to_owned())
            .and_then(|runtime| {
                runtime
                    .try_close_session(command_id, session_id)
                    .map_err(|error| error.to_string())
            });
        if let Err(error) = admission {
            let owner = WorkspaceKey::session(session_id);
            let _ = self
                .app
                .complete_runtime_command(command_id, &owner, &intent);
            self.app
                .workspaces
                .active_mut()
                .set_preference_notice(error);
        }
        self.refresh_views(UiChangeSet::one(UiRegion::Sessions), cx);
    }

    pub(in crate::app) fn delete_session(&mut self, session_id: &str, cx: &mut Context<Self>) {
        let intent = DesktopCommandIntent::DeleteSession {
            session_id: session_id.to_owned(),
        };
        let owner = WorkspaceKey::session(session_id);
        let command_id = match self.app.commands.reserve(owner.clone(), intent.clone()) {
            Ok(command_id) => command_id,
            Err(error) => {
                self.app
                    .workspaces
                    .active_mut()
                    .set_preference_notice(error.to_string());
                self.refresh_views(UiChangeSet::one(UiRegion::Sessions), cx);
                return;
            }
        };
        let admission = self
            .connection
            .runtime_client
            .as_ref()
            .ok_or_else(|| "desktop runtime is unavailable".to_owned())
            .and_then(|runtime| {
                runtime
                    .try_delete_session(command_id, session_id)
                    .map_err(|error| error.to_string())
            });
        if let Err(error) = admission {
            let _ = self
                .app
                .complete_runtime_command(command_id, &owner, &intent);
            self.app
                .workspaces
                .active_mut()
                .set_preference_notice(error);
        }
        self.refresh_views(UiChangeSet::one(UiRegion::Sessions), cx);
    }

    #[cfg(feature = "desktop-devtools")]
    pub(in crate::app) fn reconcile_thinking_selection_for(&mut self, owner: &WorkspaceKey) {
        let Some(workspace) = self.app.workspaces.get_mut(owner) else {
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
        if let Some(session_id) = session_id.as_deref() {
            self.remember_thinking_selection(session_id, selection);
        }
    }
}
