use super::{
    CodingAgentWorkspaceSelection, DesktopCommandIntent, Instant, NativeShell, PathBuf,
    PlatformUpdatePort, SessionWorkspace, WorkspaceKey, truncate_label,
    validate_prompt_attachments,
};

impl PlatformUpdatePort for NativeShell {
    fn active_workspace_key(&self) -> WorkspaceKey {
        self.app.workspaces.active_key().clone()
    }

    fn workspace_exists(&self, owner: &WorkspaceKey) -> bool {
        self.app.workspaces.contains(owner)
    }

    fn project_directory_editable(&self, owner: &WorkspaceKey) -> bool {
        self.app
            .workspaces
            .get(owner)
            .is_some_and(SessionWorkspace::project_directory_editable)
    }

    fn set_project_directory(&mut self, owner: &WorkspaceKey, path: PathBuf) -> bool {
        let Some(workspace) = self.app.workspaces.get_mut(owner) else {
            return false;
        };
        if !workspace.project_directory_editable() {
            return false;
        }
        workspace.draft_workspace_selection = CodingAgentWorkspaceSelection::project(path);
        true
    }

    fn add_composer_attachments(
        &mut self,
        owner: &WorkspaceKey,
        paths: Vec<PathBuf>,
    ) -> Result<bool, String> {
        let Some(workspace) = self.app.workspaces.get_mut(owner) else {
            return Ok(false);
        };
        let mut candidate = workspace.composer_attachments.clone();
        for path in paths {
            if !candidate.contains(&path) {
                candidate.push(path);
            }
        }
        validate_prompt_attachments(&candidate).map_err(|error| error.to_string())?;
        if candidate == workspace.composer_attachments {
            return Ok(false);
        }
        workspace.composer_attachments = candidate;
        Ok(true)
    }

    fn set_notice(&mut self, owner: &WorkspaceKey, notice: String) {
        if let Some(workspace) = self.app.workspaces.get_mut(owner) {
            workspace.set_preference_notice(notice);
        }
    }

    fn show_conversation_announcement(&mut self, owner: &WorkspaceKey, message: String) {
        self.ui.announce_conversation(owner.clone(), message);
    }

    fn clear_conversation_announcement(&mut self, owner: &WorkspaceKey) -> bool {
        self.ui.clear_conversation_announcement(owner)
    }

    fn fire_conversation_height_refresh(&mut self, owner: &WorkspaceKey) -> bool {
        self.app.workspaces.get_mut(owner).is_some_and(|workspace| {
            workspace
                .presentation
                .conversation_controller
                .fire_current_height_refresh()
        })
    }

    fn commit_conversation_width(&mut self, owner: &WorkspaceKey) -> bool {
        self.app.workspaces.get_mut(owner).is_some_and(|workspace| {
            workspace
                .presentation
                .conversation_controller
                .commit_current_pending_width()
        })
    }

    fn refresh_inspector_telemetry(&mut self, owner: &WorkspaceKey) -> bool {
        if self.app.workspaces.active_key() != owner
            || self.ui.inspector_telemetry_refresh_deadline.is_none()
        {
            return false;
        }
        self.ui.inspector_telemetry_refresh_deadline = None;
        self.ui.inspector_telemetry_last_refresh = Some(Instant::now());
        true
    }

    fn complete_resync_admission(
        &mut self,
        owner: &WorkspaceKey,
        command_id: u64,
        failure: Option<String>,
    ) {
        let Some(message) = failure else {
            return;
        };
        let intent = DesktopCommandIntent::Resync;
        let _ = self.app.commands.complete(command_id, owner, &intent);
        if let Some(workspace) = self.app.workspaces.get_mut(owner) {
            workspace.set_preference_notice(message);
        }
    }

    fn complete_external_editor_launch(
        &mut self,
        owner: &WorkspaceKey,
        command_id: u64,
        project_relative_path: &str,
        failure: Option<String>,
    ) {
        let intent = DesktopCommandIntent::ExternalEditor {
            project_relative_path: project_relative_path.to_owned(),
        };
        if self
            .app
            .commands
            .complete(command_id, owner, &intent)
            .is_err()
        {
            return;
        }
        if let Some(workspace) = self.app.workspaces.get_mut(owner) {
            workspace.set_preference_notice(failure.unwrap_or_else(|| {
                format!(
                    "Opened {} in the configured editor.",
                    truncate_label(project_relative_path, 48)
                )
            }));
        }
    }
}
