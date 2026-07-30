use desktop::preferences::DesktopPreferences;

use super::{
    commands::CommandTracker, workspace::WorkspaceStore, workspace_state::RuntimeWorkspaceDefaults,
};

pub(crate) struct DesktopState<Workspace, Catalog, WorkspaceDefaults = ()> {
    pub(crate) workspaces: WorkspaceStore<Workspace>,
    pub(crate) commands: CommandTracker,
    pub(crate) catalog: Catalog,
    pub(crate) preferences: DesktopPreferences,
    pub(crate) workspace_defaults: WorkspaceDefaults,
    runtime_preferences_dirty: bool,
}

#[cfg(test)]
impl<Workspace, Catalog> DesktopState<Workspace, Catalog, ()> {
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
            workspace_defaults: (),
            runtime_preferences_dirty: false,
        }
    }
}

impl<Workspace, Catalog> DesktopState<Workspace, Catalog, RuntimeWorkspaceDefaults> {
    pub(crate) const fn new_with_workspace_defaults(
        workspaces: WorkspaceStore<Workspace>,
        commands: CommandTracker,
        catalog: Catalog,
        preferences: DesktopPreferences,
        defaults: RuntimeWorkspaceDefaults,
    ) -> Self {
        Self {
            workspaces,
            commands,
            catalog,
            preferences,
            workspace_defaults: defaults,
            runtime_preferences_dirty: false,
        }
    }
}

impl<Workspace, Catalog, WorkspaceDefaults> DesktopState<Workspace, Catalog, WorkspaceDefaults> {
    pub(crate) fn mark_runtime_preferences_dirty(&mut self) {
        self.runtime_preferences_dirty = true;
    }

    pub(crate) fn take_runtime_preferences_dirty(&mut self) -> bool {
        std::mem::take(&mut self.runtime_preferences_dirty)
    }
}
