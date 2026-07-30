use std::collections::{HashMap, HashSet};

use coding_agent::api::view::CodingAgentWorkspaceOverview;
use desktop::runtime::{DesktopSessionCatalogEntry, MAX_DESKTOP_SESSION_CATALOG};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ProjectCatalogState {
    NotLoaded,
    Loading,
    Ready,
    Error { message: String },
    Stale { error: Option<String> },
}

impl ProjectCatalogState {
    pub(crate) const fn is_loading(&self) -> bool {
        matches!(self, Self::Loading)
    }

    pub(crate) fn error_message(&self) -> Option<&str> {
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
pub(crate) struct ProjectCatalogGroup {
    pub(crate) workspace: CodingAgentWorkspaceOverview,
    pub(crate) sessions: Vec<DesktopSessionCatalogEntry>,
    pub(crate) collapsed: bool,
}

pub(crate) struct ProjectCatalogController {
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
    pub(crate) fn catalog(&self) -> &[DesktopSessionCatalogEntry] {
        &self.catalog
    }

    pub(crate) fn omitted(&self) -> usize {
        self.omitted
    }

    pub(crate) fn state(&self) -> &ProjectCatalogState {
        &self.state
    }

    pub(crate) fn begin_refresh(&mut self) {
        self.state = ProjectCatalogState::Loading;
    }

    pub(crate) fn fail_refresh(&mut self, message: impl Into<String>) {
        let message = message.into();
        self.state = if self.has_loaded_snapshot || !self.catalog.is_empty() {
            ProjectCatalogState::Stale {
                error: Some(message),
            }
        } else {
            ProjectCatalogState::Error { message }
        };
    }

    pub(crate) fn replace_catalog(
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

    pub(crate) fn project_groups(&self) -> Vec<ProjectCatalogGroup> {
        self.filtered_project_groups("")
    }

    pub(crate) fn filtered_project_groups(&self, query: &str) -> Vec<ProjectCatalogGroup> {
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
                    group.collapsed = false;
                    Some(group)
                }
            })
            .collect()
    }

    pub(crate) fn set_group_collapsed(&mut self, group_id: &str, collapsed: bool) -> bool {
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

    pub(crate) fn rename_session(
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

    pub(crate) fn insert_created_session(&mut self, entry: DesktopSessionCatalogEntry) {
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

    pub(crate) fn remove_session(&mut self, session_id: &str) -> bool {
        let before = self.catalog.len();
        self.catalog
            .retain(|session| session.session_id != session_id);
        let removed = self.catalog.len() != before;
        if removed {
            self.record_local_mutation();
        }
        removed
    }

    pub(crate) fn next_session_id(&self, active_session_id: Option<&str>) -> Option<String> {
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

pub(crate) fn workspace_matches_query(
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

pub(crate) fn session_matches_query(
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
