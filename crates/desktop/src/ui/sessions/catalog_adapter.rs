#[cfg(test)]
use coding_agent::api::view::CodingAgentWorkspaceMigration;
#[cfg(test)]
use desktop::runtime::DesktopSessionCatalogEntry;
use desktop::ui::shell::truncate_label;
use gpui::Context;

use super::{MAX_SESSION_WORKSPACES, NativeShell};
#[cfg(test)]
use crate::application::catalog::{ProjectCatalogController, ProjectCatalogState};
use crate::application::change_set::{UiChangeSet, UiRegion};
use crate::application::commands::DesktopCommandIntent;
use crate::application::workspace::WorkspaceKey;
#[cfg(test)]
use coding_agent::api::view::CodingAgentWorkspaceOverview;
#[cfg(test)]
use desktop::runtime::MAX_DESKTOP_SESSION_CATALOG;

impl NativeShell {
    pub(super) fn create_session(&mut self, cx: &mut Context<Self>) {
        if self.open_workspace_count() >= MAX_SESSION_WORKSPACES {
            self.app
                .workspaces
                .active_mut()
                .set_preference_notice(format!(
                    "Up to {MAX_SESSION_WORKSPACES} sessions can stay open; close one first."
                ));
            self.refresh_views(UiChangeSet::one(UiRegion::Sessions), cx);
            return;
        }
        if self
            .app
            .workspaces
            .active_mut()
            .projection
            .as_ref()
            .is_some_and(|projection| projection.snapshot().active_operation.is_some())
            || self
                .app
                .workspaces
                .active_mut()
                .composer
                .submitted()
                .is_some()
            || self.app.commands.contains_anywhere(|intent| {
                matches!(
                    intent,
                    DesktopCommandIntent::CreateSession | DesktopCommandIntent::OpenSession { .. }
                )
            })
        {
            return;
        }
        let intent = DesktopCommandIntent::CreateSession;
        let Some(command_id) = self.reserve_command(intent.clone()) else {
            self.refresh_views(UiChangeSet::one(UiRegion::Toast), cx);
            cx.notify();
            return;
        };
        let admission = self.connection.runtime_client.as_ref().map_or_else(
            || Err("desktop runtime is stopped".to_owned()),
            |runtime| {
                runtime
                    .try_create_session(command_id)
                    .map_err(|error| error.to_string())
            },
        );
        match admission {
            Ok(()) => self
                .app
                .workspaces
                .active_mut()
                .set_preference_notice("Creating a new session…".into()),
            Err(message) => {
                self.complete_active_command(command_id, &intent);
                self.app
                    .workspaces
                    .active_mut()
                    .set_preference_notice(message);
            }
        }
        self.refresh_views(UiChangeSet::one(UiRegion::Sessions), cx);
        cx.notify();
    }

    pub(in crate::app) fn request_session_catalog(&mut self, cx: &mut Context<Self>) {
        if self.active_command_contains(&DesktopCommandIntent::ListSessions) {
            return;
        }
        let intent = DesktopCommandIntent::ListSessions;
        let Some(command_id) = self.reserve_command(intent.clone()) else {
            self.refresh_views(UiChangeSet::one(UiRegion::Toast), cx);
            cx.notify();
            return;
        };
        self.app.catalog.begin_refresh();
        let admission = self.connection.runtime_client.as_ref().map_or_else(
            || Err("desktop runtime is stopped".to_owned()),
            |runtime| {
                runtime
                    .try_list_sessions(command_id)
                    .map_err(|error| error.to_string())
            },
        );
        if let Err(message) = admission {
            self.complete_active_command(command_id, &intent);
            self.app.catalog.fail_refresh(message.clone());
            self.app
                .workspaces
                .active_mut()
                .set_preference_notice(message);
            self.refresh_views(UiChangeSet::one(UiRegion::Toast), cx);
        }
        self.refresh_views(UiChangeSet::one(UiRegion::Sessions), cx);
        cx.notify();
    }

    pub(super) fn rename_session(
        &mut self,
        session_id: String,
        name: String,
        cx: &mut Context<Self>,
    ) {
        let intent = DesktopCommandIntent::RenameSession {
            session_id: session_id.clone(),
        };
        let owner = WorkspaceKey::session(session_id.clone());
        let command_id = match self.app.commands.reserve(owner.clone(), intent.clone()) {
            Ok(command_id) => command_id,
            Err(error) => {
                self.app
                    .workspaces
                    .active_mut()
                    .set_preference_notice(error.to_string());
                self.refresh_views(UiChangeSet::one(UiRegion::Toast), cx);
                return;
            }
        };
        let admission = self.connection.runtime_client.as_ref().map_or_else(
            || Err("desktop runtime is stopped".to_owned()),
            |runtime| {
                runtime
                    .try_rename_session(command_id, &session_id, Some(&name))
                    .map_err(|error| error.to_string())
            },
        );
        if let Err(message) = admission {
            self.app
                .complete_runtime_command(command_id, &owner, &intent);
            self.app
                .workspaces
                .active_mut()
                .set_preference_notice(message);
            self.refresh_views(UiChangeSet::one(UiRegion::Toast), cx);
        }
        self.refresh_views(UiChangeSet::one(UiRegion::Sessions), cx);
        cx.notify();
    }

    pub(super) fn open_session(&mut self, session_id: String, cx: &mut Context<Self>) {
        let already_open = self
            .app
            .workspaces
            .contains(&WorkspaceKey::session(session_id.clone()));
        if !already_open && self.open_workspace_count() >= MAX_SESSION_WORKSPACES {
            self.app
                .workspaces
                .active_mut()
                .set_preference_notice(format!(
                    "Up to {MAX_SESSION_WORKSPACES} sessions can stay open; close one first."
                ));
            self.refresh_views(UiChangeSet::one(UiRegion::Sessions), cx);
            return;
        }
        if self
            .app
            .workspaces
            .active_mut()
            .projection
            .as_ref()
            .is_some_and(|projection| projection.snapshot().active_operation.is_some())
            || self
                .app
                .workspaces
                .active_mut()
                .composer
                .submitted()
                .is_some()
            || self.app.commands.contains_anywhere(|intent| {
                matches!(
                    intent,
                    DesktopCommandIntent::CreateSession | DesktopCommandIntent::OpenSession { .. }
                )
            })
        {
            self.app.workspaces.active_mut().set_preference_notice(
                "Session switching is available only while the runtime is idle.".into(),
            );
            cx.notify();
            return;
        }
        if self
            .app
            .workspaces
            .active_mut()
            .projection
            .as_ref()
            .is_some_and(|projection| session_id == projection.snapshot().session.session_id)
        {
            self.app
                .workspaces
                .active_mut()
                .set_preference_notice("The requested session is already active.".into());
            cx.notify();
            return;
        }
        let intent = DesktopCommandIntent::OpenSession {
            session_id: session_id.clone(),
        };
        let owner = WorkspaceKey::session(session_id.clone());
        let command_id = match self.app.commands.reserve(owner.clone(), intent.clone()) {
            Ok(command_id) => command_id,
            Err(error) => {
                self.app
                    .workspaces
                    .active_mut()
                    .set_preference_notice(error.to_string());
                cx.notify();
                return;
            }
        };
        let admission = self.connection.runtime_client.as_ref().map_or_else(
            || Err("desktop runtime is stopped".to_owned()),
            |runtime| {
                runtime
                    .try_open_session(command_id, &session_id)
                    .map_err(|error| error.to_string())
            },
        );
        match admission {
            Ok(()) => {
                self.app
                    .workspaces
                    .active_mut()
                    .set_preference_notice(format!(
                        "Opening session {}…",
                        truncate_label(&session_id, 32)
                    ));
            }
            Err(message) => {
                self.app
                    .complete_runtime_command(command_id, &owner, &intent);
                self.app
                    .workspaces
                    .active_mut()
                    .set_preference_notice(message);
            }
        }
        self.refresh_views(UiChangeSet::one(UiRegion::Sessions), cx);
        cx.notify();
    }

    pub(super) fn switch_next_session(&mut self, cx: &mut Context<Self>) {
        let active = self
            .app
            .workspaces
            .active_key()
            .session_id()
            .map(|session_id| session_id.as_str());
        let Some(session_id) = self.app.catalog.next_session_id(active) else {
            self.app.workspaces.active_mut().set_preference_notice(
                "Refresh the session catalog before switching sessions.".into(),
            );
            self.refresh_views(UiChangeSet::one(UiRegion::Toast), cx);
            cx.notify();
            return;
        };
        if active.is_some_and(|active| session_id == active) {
            self.app
                .workspaces
                .active_mut()
                .set_preference_notice("No other project session is available.".into());
            self.refresh_views(UiChangeSet::one(UiRegion::Toast), cx);
            cx.notify();
            return;
        }
        self.open_session(session_id, cx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    use coding_agent::api::view::{CodingAgentWorkspaceKind, CodingAgentWorkspaceMigrationOutcome};

    fn workspace(
        group_id: &str,
        kind: CodingAgentWorkspaceKind,
        display_name: &str,
        display_path: Option<&str>,
    ) -> CodingAgentWorkspaceOverview {
        CodingAgentWorkspaceOverview {
            group_id: group_id.into(),
            kind,
            display_name: display_name.into(),
            display_path: display_path.map(PathBuf::from),
        }
    }

    fn entry(
        session_id: &str,
        name: Option<&str>,
        updated_at: &str,
        workspace: CodingAgentWorkspaceOverview,
    ) -> DesktopSessionCatalogEntry {
        DesktopSessionCatalogEntry {
            session_id: session_id.into(),
            name: name.map(str::to_owned),
            workspace,
            workspace_migration: CodingAgentWorkspaceMigration {
                outcome: CodingAgentWorkspaceMigrationOutcome::NotRequired,
                diagnostic: None,
            },
            updated_at: updated_at.into(),
            ..Default::default()
        }
    }

    #[test]
    fn catalog_state_distinguishes_initial_loading_error_ready_and_stale() {
        let mut controller = ProjectCatalogController::default();
        assert_eq!(controller.state(), &ProjectCatalogState::NotLoaded);
        assert!(controller.project_groups().is_empty());

        controller.begin_refresh();
        assert_eq!(controller.state(), &ProjectCatalogState::Loading);
        controller.fail_refresh("offline");
        assert_eq!(
            controller.state(),
            &ProjectCatalogState::Error {
                message: "offline".into()
            }
        );

        controller.begin_refresh();
        controller.replace_catalog(Vec::new(), 7);
        assert_eq!(controller.state(), &ProjectCatalogState::Ready);
        assert!(controller.project_groups().is_empty());
        assert_eq!(controller.omitted(), 7);

        controller.begin_refresh();
        controller.fail_refresh("timed out");
        assert_eq!(
            controller.state(),
            &ProjectCatalogState::Stale {
                error: Some("timed out".into())
            }
        );
        assert_eq!(controller.state().error_message(), Some("timed out"));

        let mut local_only = ProjectCatalogController::default();
        local_only.insert_created_session(entry(
            "local",
            None,
            "2026-07-30T12:00:00Z",
            workspace(
                "projectless:local",
                CodingAgentWorkspaceKind::Projectless,
                "Projectless",
                None,
            ),
        ));
        assert_eq!(
            local_only.state(),
            &ProjectCatalogState::Stale { error: None }
        );
        local_only.begin_refresh();
        local_only.fail_refresh("offline again");
        assert_eq!(
            local_only.state(),
            &ProjectCatalogState::Stale {
                error: Some("offline again".into())
            }
        );
    }

    #[test]
    fn groups_preserve_project_identity_and_collect_projectless_conversations() {
        let same_name_a = workspace(
            "project:a",
            CodingAgentWorkspaceKind::Project,
            "repo",
            Some("/workspace/a/repo"),
        );
        let same_name_b = workspace(
            "project:b",
            CodingAgentWorkspaceKind::Project,
            "repo",
            Some("/workspace/b/repo"),
        );
        let projectless_one = workspace(
            "projectless:one",
            CodingAgentWorkspaceKind::Projectless,
            "Projectless",
            None,
        );
        let projectless_two = workspace(
            "projectless:two",
            CodingAgentWorkspaceKind::Projectless,
            "Projectless",
            None,
        );
        let legacy = workspace(
            "legacy:unscoped",
            CodingAgentWorkspaceKind::Legacy,
            "Legacy session",
            None,
        );
        let mut controller = ProjectCatalogController::default();
        controller.replace_catalog(
            vec![
                entry("a-new", Some("A new"), "05", same_name_a.clone()),
                entry("b-only", Some("B"), "04", same_name_b),
                entry("a-old", Some("A old"), "03", same_name_a),
                entry("no-project-new", None, "02", projectless_one),
                entry("no-project-old", None, "015", projectless_two),
                entry("legacy", None, "01", legacy),
            ],
            0,
        );

        let groups = controller.project_groups();
        assert_eq!(
            groups
                .iter()
                .map(|group| group.workspace.group_id.as_str())
                .collect::<Vec<_>>(),
            [
                "project:a",
                "project:b",
                crate::application::catalog::PROJECTLESS_CONVERSATIONS_GROUP_ID,
                "legacy:unscoped"
            ]
        );
        assert_eq!(
            groups[0].workspace.display_name,
            groups[1].workspace.display_name
        );
        assert_ne!(
            groups[0].workspace.display_path,
            groups[1].workspace.display_path
        );
        assert_eq!(
            groups[0]
                .sessions
                .iter()
                .map(|session| session.session_id.as_str())
                .collect::<Vec<_>>(),
            ["a-new", "a-old"]
        );
        assert_eq!(
            groups[2].workspace.kind,
            CodingAgentWorkspaceKind::Projectless
        );
        assert_eq!(
            groups[2]
                .sessions
                .iter()
                .map(|session| session.session_id.as_str())
                .collect::<Vec<_>>(),
            ["no-project-new", "no-project-old"]
        );
        assert_eq!(groups[3].workspace.kind, CodingAgentWorkspaceKind::Legacy);
    }

    #[test]
    fn search_matches_workspace_or_session_without_mutating_collapse_state() {
        let project = workspace(
            "project:alpha",
            CodingAgentWorkspaceKind::Project,
            "shared-name",
            Some("/teams/alpha/repo"),
        );
        let projectless = workspace(
            "projectless:one",
            CodingAgentWorkspaceKind::Projectless,
            "Projectless",
            None,
        );
        let mut controller = ProjectCatalogController::default();
        controller.replace_catalog(
            vec![
                entry("alpha-one", Some("Release plan"), "03", project.clone()),
                entry("alpha-two", Some("Bug triage"), "02", project),
                entry("scratch", Some("Draft"), "01", projectless),
            ],
            0,
        );
        assert!(controller.set_group_collapsed("project:alpha", true));
        assert!(!controller.set_group_collapsed("missing", true));

        let by_session = controller.filtered_project_groups("release");
        assert_eq!(by_session.len(), 1);
        assert_eq!(by_session[0].sessions.len(), 1);
        assert_eq!(by_session[0].sessions[0].session_id, "alpha-one");
        assert!(!by_session[0].collapsed);

        let by_path = controller.filtered_project_groups("teams/alpha");
        assert_eq!(by_path.len(), 1);
        assert_eq!(by_path[0].sessions.len(), 2);
        assert!(!by_path[0].collapsed);
        assert!(
            controller
                .filtered_project_groups("does-not-exist")
                .is_empty()
        );

        assert!(controller.project_groups()[0].collapsed);
        let refreshed = controller.catalog().to_vec();
        controller.replace_catalog(Vec::new(), 0);
        assert!(controller.project_groups().is_empty());
        controller.replace_catalog(refreshed, 0);
        assert!(controller.project_groups()[0].collapsed);
    }

    #[test]
    fn catalog_local_mutations_keep_recent_order_and_bounds() {
        let mut controller = ProjectCatalogController::default();
        let project_a = workspace(
            "project:a",
            CodingAgentWorkspaceKind::Project,
            "A",
            Some("/a"),
        );
        let project_b = workspace(
            "project:b",
            CodingAgentWorkspaceKind::Project,
            "B",
            Some("/b"),
        );
        controller.replace_catalog(
            vec![
                entry("session-a", None, "02", project_a.clone()),
                entry("session-b", None, "01", project_b.clone()),
            ],
            3,
        );
        assert_eq!(controller.omitted(), 3);
        assert_eq!(
            controller.next_session_id(Some("session-a")).as_deref(),
            Some("session-b")
        );
        assert_eq!(
            controller.next_session_id(Some("missing")).as_deref(),
            Some("session-a")
        );
        controller.insert_created_session(entry("session-c", None, "03", project_a));
        assert_eq!(controller.catalog()[0].session_id, "session-c");
        assert!(controller.rename_session(
            "session-b",
            Some("Renamed recently".into()),
            "04".into(),
        ));
        assert_eq!(controller.catalog()[0].session_id, "session-b");
        assert_eq!(
            controller.catalog()[0].name.as_deref(),
            Some("Renamed recently")
        );
        assert_eq!(
            controller.project_groups()[0].workspace.group_id,
            "project:b"
        );
        assert!(controller.remove_session("session-b"));
        assert!(!controller.remove_session("session-b"));

        let mut bounded = ProjectCatalogController::default();
        for index in 0..=MAX_DESKTOP_SESSION_CATALOG {
            bounded.insert_created_session(entry(
                &format!("session-{index}"),
                None,
                &format!("{index:03}"),
                project_b.clone(),
            ));
        }
        assert_eq!(bounded.catalog().len(), MAX_DESKTOP_SESSION_CATALOG);
        assert_eq!(bounded.catalog()[0].session_id, "session-128");
        assert_eq!(bounded.omitted(), 1);
    }
}
