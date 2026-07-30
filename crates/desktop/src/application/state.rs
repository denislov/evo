use desktop::preferences::DesktopPreferences;

use super::{commands::CommandTracker, workspace::WorkspaceStore};

pub(crate) struct DesktopState<Workspace, Catalog> {
    pub(crate) workspaces: WorkspaceStore<Workspace>,
    pub(crate) commands: CommandTracker,
    pub(crate) catalog: Catalog,
    pub(crate) preferences: DesktopPreferences,
}

impl<Workspace, Catalog> DesktopState<Workspace, Catalog> {
    pub(crate) const fn new(
        workspaces: WorkspaceStore<Workspace>,
        commands: CommandTracker,
        catalog: Catalog,
        preferences: DesktopPreferences,
    ) -> Self {
        Self {
            workspaces,
            commands,
            catalog,
            preferences,
        }
    }
}
