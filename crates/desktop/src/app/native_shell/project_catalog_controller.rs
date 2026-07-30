use std::collections::{HashMap, HashSet};

use coding_agent::api::view::{
    CodingAgentWorkspaceMigration, CodingAgentWorkspaceMigrationOutcome,
    CodingAgentWorkspaceOverview,
};
use desktop::runtime::{DesktopSessionCatalogEntry, MAX_DESKTOP_SESSION_CATALOG};
use desktop::shell::truncate_label;
use gpui::Context;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

use super::{MAX_SESSION_WORKSPACES, NativeShell};
use crate::application::commands::DesktopCommandIntent;
use crate::application::workspace::WorkspaceKey;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ProjectCatalogState {
    NotLoaded,
    Loading,
    Ready,
    Error { message: String },
    Stale { error: Option<String> },
}

impl ProjectCatalogState {
    pub(super) const fn is_loading(&self) -> bool {
        matches!(self, Self::Loading)
    }

    pub(super) fn error_message(&self) -> Option<&str> {
        match self {
            Self::Error { message } => Some(message),
            Self::Stale {
                error: Some(message),
            } => Some(message),
            Self::NotLoaded | Self::Loading | Self::Ready | Self::Stale { error: None } => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ProjectCatalogGroup {
    pub(super) workspace: CodingAgentWorkspaceOverview,
    pub(super) sessions: Vec<DesktopSessionCatalogEntry>,
    pub(super) collapsed: bool,
}

pub(super) struct ProjectCatalogController {
    catalog: Vec<DesktopSessionCatalogEntry>,
    omitted: usize,
    state: ProjectCatalogState,
    has_loaded_snapshot: bool,
    collapsed_group_ids: HashSet<String>,
}

impl Default for ProjectCatalogController {
    fn default() -> Self {
        Self {
            catalog: Vec::new(),
            omitted: 0,
            state: ProjectCatalogState::NotLoaded,
            has_loaded_snapshot: false,
            collapsed_group_ids: HashSet::new(),
        }
    }
}

impl ProjectCatalogController {
    #[cfg(test)]
    pub(super) fn catalog(&self) -> &[DesktopSessionCatalogEntry] {
        &self.catalog
    }

    pub(super) fn omitted(&self) -> usize {
        self.omitted
    }

    pub(super) fn state(&self) -> &ProjectCatalogState {
        &self.state
    }

    pub(super) fn begin_refresh(&mut self) {
        self.state = ProjectCatalogState::Loading;
    }

    pub(super) fn fail_refresh(&mut self, message: impl Into<String>) {
        let message = message.into();
        self.state = if self.has_loaded_snapshot || !self.catalog.is_empty() {
            ProjectCatalogState::Stale {
                error: Some(message),
            }
        } else {
            ProjectCatalogState::Error { message }
        };
    }

    pub(super) fn replace_catalog(
        &mut self,
        mut catalog: Vec<DesktopSessionCatalogEntry>,
        omitted: usize,
    ) {
        let locally_omitted = catalog.len().saturating_sub(MAX_DESKTOP_SESSION_CATALOG);
        catalog.truncate(MAX_DESKTOP_SESSION_CATALOG);
        self.catalog = catalog;
        self.omitted = omitted.saturating_add(locally_omitted);
        self.has_loaded_snapshot = true;
        self.state = ProjectCatalogState::Ready;
    }

    pub(super) fn project_groups(&self) -> Vec<ProjectCatalogGroup> {
        self.filtered_project_groups("")
    }

    pub(super) fn filtered_project_groups(&self, query: &str) -> Vec<ProjectCatalogGroup> {
        let query = query.trim().to_lowercase();
        let mut group_indexes = HashMap::<String, usize>::new();
        let mut groups = Vec::<ProjectCatalogGroup>::new();
        for session in &self.catalog {
            let group_id = session.workspace.group_id.clone();
            let group_index = match group_indexes.get(&group_id).copied() {
                Some(index) => index,
                None => {
                    let index = groups.len();
                    group_indexes.insert(group_id.clone(), index);
                    groups.push(ProjectCatalogGroup {
                        workspace: session.workspace.clone(),
                        sessions: Vec::new(),
                        collapsed: self.collapsed_group_ids.contains(&group_id),
                    });
                    index
                }
            };
            groups[group_index].sessions.push(session.clone());
        }
        if query.is_empty() {
            return groups;
        }
        groups
            .into_iter()
            .filter_map(|mut group| {
                if !workspace_matches_query(&group.workspace, &query) {
                    group
                        .sessions
                        .retain(|session| session_matches_query(session, &query));
                }
                if group.sessions.is_empty() {
                    None
                } else {
                    // Search reveals matching descendants without mutating
                    // the user's independent disclosure state.
                    group.collapsed = false;
                    Some(group)
                }
            })
            .collect()
    }

    pub(super) fn set_group_collapsed(&mut self, group_id: &str, collapsed: bool) -> bool {
        if !self
            .catalog
            .iter()
            .any(|session| session.workspace.group_id == group_id)
        {
            return false;
        }
        if collapsed {
            self.collapsed_group_ids.insert(group_id.to_owned())
        } else {
            self.collapsed_group_ids.remove(group_id)
        }
    }

    pub(super) fn rename_session(
        &mut self,
        session_id: &str,
        name: Option<String>,
        updated_at: String,
    ) -> bool {
        let Some(index) = self
            .catalog
            .iter()
            .position(|session| session.session_id == session_id)
        else {
            return false;
        };
        let mut session = self.catalog.remove(index);
        session.name = name;
        session.updated_at = updated_at;
        self.catalog.insert(0, session);
        self.record_local_mutation();
        true
    }

    pub(super) fn insert_created_session(&mut self, entry: DesktopSessionCatalogEntry) {
        let replaced = self
            .catalog
            .iter()
            .position(|session| session.session_id == entry.session_id)
            .map(|index| self.catalog.remove(index))
            .is_some();
        self.catalog.insert(0, entry);
        if self.catalog.len() > MAX_DESKTOP_SESSION_CATALOG {
            self.catalog.truncate(MAX_DESKTOP_SESSION_CATALOG);
            if !replaced {
                self.omitted = self.omitted.saturating_add(1);
            }
        }
        self.record_local_mutation();
    }

    pub(super) fn remove_session(&mut self, session_id: &str) -> bool {
        let before = self.catalog.len();
        self.catalog
            .retain(|session| session.session_id != session_id);
        let removed = self.catalog.len() != before;
        if removed {
            self.record_local_mutation();
        }
        removed
    }

    pub(super) fn next_session_id(&self, active_session_id: Option<&str>) -> Option<String> {
        let current = active_session_id.and_then(|active_session_id| {
            self.catalog
                .iter()
                .position(|session| session.session_id == active_session_id)
        });
        let next = current.map_or(0, |index| (index + 1) % self.catalog.len());
        self.catalog
            .get(next)
            .map(|session| session.session_id.clone())
    }

    fn record_local_mutation(&mut self) {
        match &self.state {
            ProjectCatalogState::NotLoaded => {
                self.state = ProjectCatalogState::Stale { error: None };
            }
            ProjectCatalogState::Error { message } => {
                self.state = ProjectCatalogState::Stale {
                    error: Some(message.clone()),
                };
            }
            ProjectCatalogState::Loading
            | ProjectCatalogState::Ready
            | ProjectCatalogState::Stale { .. } => {}
        }
    }
}

pub(super) fn workspace_matches_query(
    workspace: &CodingAgentWorkspaceOverview,
    normalized_query: &str,
) -> bool {
    workspace.group_id.to_lowercase().contains(normalized_query)
        || workspace
            .display_name
            .to_lowercase()
            .contains(normalized_query)
        || workspace.display_path.as_ref().is_some_and(|path| {
            path.to_string_lossy()
                .to_lowercase()
                .contains(normalized_query)
        })
}

pub(super) fn session_matches_query(
    session: &DesktopSessionCatalogEntry,
    normalized_query: &str,
) -> bool {
    session.session_id.to_lowercase().contains(normalized_query)
        || session
            .name
            .as_deref()
            .is_some_and(|name| name.to_lowercase().contains(normalized_query))
        || session.updated_at.to_lowercase().contains(normalized_query)
}

impl NativeShell {
    pub(super) fn create_session(&mut self, cx: &mut Context<Self>) {
        if self.open_workspace_count() >= MAX_SESSION_WORKSPACES {
            self.workspace_store
                .active_mut()
                .set_preference_notice(format!(
                    "Up to {MAX_SESSION_WORKSPACES} sessions can stay open; close one first."
                ));
            self.notify_sessions_pane(cx);
            return;
        }
        if self
            .workspace_store
            .active_mut()
            .projection
            .as_ref()
            .is_some_and(|projection| projection.snapshot().active_operation.is_some())
            || self
                .workspace_store
                .active_mut()
                .composer
                .submitted()
                .is_some()
            || self.command_tracker.contains_anywhere(|intent| {
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
            self.notify_toast_host(cx);
            cx.notify();
            return;
        };
        let admission = self.runtime_client.as_ref().map_or_else(
            || Err("desktop runtime is stopped".to_owned()),
            |runtime| {
                runtime
                    .try_create_session(command_id)
                    .map_err(|error| error.to_string())
            },
        );
        match admission {
            Ok(()) => self
                .workspace_store
                .active_mut()
                .set_preference_notice("Creating a new session…".into()),
            Err(message) => {
                self.complete_active_command(command_id, &intent);
                self.workspace_store
                    .active_mut()
                    .set_preference_notice(message);
            }
        }
        self.notify_sessions_pane(cx);
        cx.notify();
    }

    pub(in crate::app) fn request_session_catalog(&mut self, cx: &mut Context<Self>) {
        if self.active_command_contains(&DesktopCommandIntent::ListSessions) {
            return;
        }
        let intent = DesktopCommandIntent::ListSessions;
        let Some(command_id) = self.reserve_command(intent.clone()) else {
            self.notify_toast_host(cx);
            cx.notify();
            return;
        };
        self.project_catalog.begin_refresh();
        let admission = self.runtime_client.as_ref().map_or_else(
            || Err("desktop runtime is stopped".to_owned()),
            |runtime| {
                runtime
                    .try_list_sessions(command_id)
                    .map_err(|error| error.to_string())
            },
        );
        if let Err(message) = admission {
            self.complete_active_command(command_id, &intent);
            self.project_catalog.fail_refresh(message.clone());
            self.workspace_store
                .active_mut()
                .set_preference_notice(message);
            self.notify_toast_host(cx);
        }
        self.notify_sessions_pane(cx);
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
        let command_id = match self.command_tracker.reserve(owner.clone(), intent.clone()) {
            Ok(command_id) => command_id,
            Err(error) => {
                self.workspace_store
                    .active_mut()
                    .set_preference_notice(error.to_string());
                self.notify_toast_host(cx);
                return;
            }
        };
        let admission = self.runtime_client.as_ref().map_or_else(
            || Err("desktop runtime is stopped".to_owned()),
            |runtime| {
                runtime
                    .try_rename_session(command_id, &session_id, Some(&name))
                    .map_err(|error| error.to_string())
            },
        );
        if let Err(message) = admission {
            self.complete_command(command_id, &owner, &intent);
            self.workspace_store
                .active_mut()
                .set_preference_notice(message);
            self.notify_toast_host(cx);
        }
        self.notify_sessions_pane(cx);
        cx.notify();
    }

    pub(super) fn insert_active_session_into_catalog(&mut self) -> bool {
        let Some(projection) = self.workspace_store.active_mut().projection.as_ref() else {
            return false;
        };
        let session_id = projection.snapshot().session.session_id.clone();
        let (workspace, workspace_migration) = self
            .workspace_store
            .active_mut()
            .project
            .workspace
            .as_ref()
            .map_or_else(
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
        self.project_catalog
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

    pub(super) fn open_session(&mut self, session_id: String, cx: &mut Context<Self>) {
        let already_open = self
            .workspace_store
            .contains(&WorkspaceKey::session(session_id.clone()));
        if !already_open && self.open_workspace_count() >= MAX_SESSION_WORKSPACES {
            self.workspace_store
                .active_mut()
                .set_preference_notice(format!(
                    "Up to {MAX_SESSION_WORKSPACES} sessions can stay open; close one first."
                ));
            self.notify_sessions_pane(cx);
            return;
        }
        if self
            .workspace_store
            .active_mut()
            .projection
            .as_ref()
            .is_some_and(|projection| projection.snapshot().active_operation.is_some())
            || self
                .workspace_store
                .active_mut()
                .composer
                .submitted()
                .is_some()
            || self.command_tracker.contains_anywhere(|intent| {
                matches!(
                    intent,
                    DesktopCommandIntent::CreateSession | DesktopCommandIntent::OpenSession { .. }
                )
            })
        {
            self.workspace_store.active_mut().set_preference_notice(
                "Session switching is available only while the runtime is idle.".into(),
            );
            cx.notify();
            return;
        }
        if self
            .workspace_store
            .active_mut()
            .projection
            .as_ref()
            .is_some_and(|projection| session_id == projection.snapshot().session.session_id)
        {
            self.workspace_store
                .active_mut()
                .set_preference_notice("The requested session is already active.".into());
            cx.notify();
            return;
        }
        let intent = DesktopCommandIntent::OpenSession {
            session_id: session_id.clone(),
        };
        let owner = WorkspaceKey::session(session_id.clone());
        let command_id = match self.command_tracker.reserve(owner.clone(), intent.clone()) {
            Ok(command_id) => command_id,
            Err(error) => {
                self.workspace_store
                    .active_mut()
                    .set_preference_notice(error.to_string());
                cx.notify();
                return;
            }
        };
        let admission = self.runtime_client.as_ref().map_or_else(
            || Err("desktop runtime is stopped".to_owned()),
            |runtime| {
                runtime
                    .try_open_session(command_id, &session_id)
                    .map_err(|error| error.to_string())
            },
        );
        match admission {
            Ok(()) => {
                self.workspace_store
                    .active_mut()
                    .set_preference_notice(format!(
                        "Opening session {}…",
                        truncate_label(&session_id, 32)
                    ));
            }
            Err(message) => {
                self.complete_command(command_id, &owner, &intent);
                self.workspace_store
                    .active_mut()
                    .set_preference_notice(message);
            }
        }
        self.notify_sessions_pane(cx);
        cx.notify();
    }

    pub(super) fn switch_next_session(&mut self, cx: &mut Context<Self>) {
        let active = self
            .workspace_store
            .active_key()
            .session_id()
            .map(|session_id| session_id.as_str());
        let Some(session_id) = self.project_catalog.next_session_id(active) else {
            self.workspace_store.active_mut().set_preference_notice(
                "Refresh the session catalog before switching sessions.".into(),
            );
            self.notify_toast_host(cx);
            cx.notify();
            return;
        };
        if active.is_some_and(|active| session_id == active) {
            self.workspace_store
                .active_mut()
                .set_preference_notice("No other project session is available.".into());
            self.notify_toast_host(cx);
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
    fn groups_use_product_identity_and_preserve_global_and_group_recent_order() {
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
        let projectless = workspace(
            "projectless:one",
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
                entry("no-project", None, "02", projectless),
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
                "projectless:one",
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
