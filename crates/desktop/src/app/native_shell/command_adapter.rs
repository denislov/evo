use super::{
    CenterDrawerKind, CodingAgentRecoveryResolution, ComposerSubmissionKind, Context,
    DesktopCommandIntent, DesktopPickerKind, DesktopRecoveryAction, DesktopRecoveryIdentity,
    DesktopRuntimeSelectionKind, DesktopThinkingLevel, FocusTarget, NativeShell, PanelVisibility,
    ToolAuthorizationDecision, ToolAuthorizationIdentity, UiChangeSet, UiRegion, Window, commands,
    composer_pane, conversation_header, recovery_action_label,
};

impl NativeShell {
    pub(super) fn toggle_sessions(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let viewport = window.viewport_size();
        let dockable = self
            .resolve_layout(
                u32::from(viewport.width),
                u32::from(viewport.height),
                PanelVisibility {
                    sessions: true,
                    context: self.app.preferences.context_panel_visible,
                },
            )
            .sidebar
            .is_some();
        if !dockable {
            if self.ui.active_drawer == Some(CenterDrawerKind::Sessions) {
                self.dismiss_drawer(window, cx, true);
            } else {
                self.activate_drawer(CenterDrawerKind::Sessions, window, cx);
            }
            return;
        }
        self.app.preferences.sessions_panel_visible = !self.app.preferences.sessions_panel_visible;
        let layout = self.layout(window);
        self.ui.focus.reconcile_layout(layout);
        if self.ui.focus.active() == FocusTarget::Composer {
            self.focus_composer_input(window, cx);
        }
        self.schedule_preferences();
        self.refresh_views(UiChangeSet::one(UiRegion::Inspector), cx);
        self.refresh_views(UiChangeSet::one(UiRegion::ConversationHeader), cx);
        cx.notify();
    }

    pub(super) fn toggle_context(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let viewport = window.viewport_size();
        let dockable = self
            .resolve_layout(
                u32::from(viewport.width),
                u32::from(viewport.height),
                PanelVisibility {
                    sessions: self.app.preferences.sessions_panel_visible,
                    context: true,
                },
            )
            .inspector
            .is_some();
        if !dockable {
            if self.ui.active_drawer == Some(CenterDrawerKind::Inspector) {
                self.dismiss_drawer(window, cx, true);
            } else {
                self.activate_drawer(CenterDrawerKind::Inspector, window, cx);
            }
            return;
        }
        self.app.preferences.context_panel_visible = !self.app.preferences.context_panel_visible;
        let layout = self.layout(window);
        self.ui.focus.reconcile_layout(layout);
        if self.ui.focus.active() == FocusTarget::Composer {
            self.focus_composer_input(window, cx);
        }
        self.schedule_preferences();
        self.refresh_views(UiChangeSet::one(UiRegion::ConversationHeader), cx);
        cx.notify();
    }

    pub(super) fn reserve_command(&mut self, intent: DesktopCommandIntent) -> Option<u64> {
        commands::reserve_command(self, intent)
    }

    pub(super) fn submit_composer(&mut self, cx: &mut Context<Self>) {
        let selected_model_supports_images = {
            let workspace = self.app.workspaces.active();
            workspace
                .project
                .models
                .iter()
                .find(|model| model.id == workspace.project.selected_model_id)
                .is_some_and(|model| model.supports_images)
        };
        if !self.app.workspaces.active().composer_attachments.is_empty()
            && !selected_model_supports_images
        {
            self.app.workspaces.active_mut().set_preference_notice(
                "Selected model does not support image attachments; the draft was retained.".into(),
            );
            self.refresh_views(UiChangeSet::one(UiRegion::Composer), cx);
            self.refresh_views(UiChangeSet::one(UiRegion::Toast), cx);
            cx.notify();
            return;
        }
        let intent = DesktopCommandIntent::Prompt;
        let Some(command_id) = self.reserve_command(intent.clone()) else {
            self.refresh_views(UiChangeSet::one(UiRegion::Toast), cx);
            cx.notify();
            return;
        };
        let has_attachments = !self
            .app
            .workspaces
            .active_mut()
            .composer_attachments
            .is_empty();
        let payload = match self
            .app
            .workspaces
            .active_mut()
            .composer
            .begin_submit_with_attachments(
                command_id,
                ComposerSubmissionKind::Prompt,
                has_attachments,
            ) {
            Ok(payload) => payload.to_owned(),
            Err(error) => {
                self.complete_active_command(command_id, &intent);
                self.app
                    .workspaces
                    .active_mut()
                    .set_preference_notice(error.to_string());
                self.refresh_views(UiChangeSet::one(UiRegion::Composer), cx);
                self.refresh_views(UiChangeSet::one(UiRegion::Toast), cx);
                cx.notify();
                return;
            }
        };
        let thinking_level = self
            .app
            .workspaces
            .active_mut()
            .thinking_selection
            .explicit();
        let target = self.app.workspaces.active_mut().prompt_target();
        let admission = self.connection.runtime_client.as_ref().map_or_else(
            || Err("desktop runtime is stopped".to_owned()),
            |runtime| {
                runtime
                    .try_submit_prompt_with_attachments(
                        command_id,
                        target,
                        &payload,
                        &self.app.workspaces.active_mut().composer_attachments,
                        thinking_level,
                    )
                    .map_err(|error| error.to_string())
            },
        );
        if let Err(message) = admission {
            self.complete_active_command(command_id, &intent);
            let _ = self
                .app
                .workspaces
                .active_mut()
                .composer
                .rejected(command_id, message);
        }
        self.refresh_views(UiChangeSet::one(UiRegion::Composer), cx);
        self.refresh_views(UiChangeSet::one(UiRegion::Inspector), cx);
        self.refresh_views(UiChangeSet::one(UiRegion::ConversationHeader), cx);
        cx.notify();
    }

    /// Send the draft using the normal conversation flow. While an operation is
    /// active this queues the message after it; otherwise it starts a prompt.
    pub(super) fn send_composer(&mut self, cx: &mut Context<Self>) {
        if self
            .app
            .workspaces
            .active_mut()
            .projection
            .as_ref()
            .is_some_and(|projection| projection.snapshot().active_operation.is_some())
        {
            self.submit_active_control(ComposerSubmissionKind::FollowUp, cx);
        } else {
            self.submit_composer(cx);
        }
    }

    /// Insert the draft into the active operation. With no operation to steer,
    /// the same shortcut behaves like a normal send instead of dropping input.
    pub(super) fn insert_composer(&mut self, cx: &mut Context<Self>) {
        if self
            .app
            .workspaces
            .active_mut()
            .projection
            .as_ref()
            .is_some_and(|projection| projection.snapshot().active_operation.is_some())
        {
            self.submit_active_control(ComposerSubmissionKind::Steer, cx);
        } else {
            self.submit_composer(cx);
        }
    }

    pub(super) fn choose_composer_attachments(&mut self, cx: &mut Context<Self>) {
        if let Some(reason) =
            composer_pane::attachment_disabled_reason(self.app.workspaces.active())
        {
            self.app
                .workspaces
                .active_mut()
                .set_preference_notice(reason.to_string());
            self.refresh_views(UiChangeSet::one(UiRegion::Toast), cx);
            cx.notify();
            return;
        }
        let owner = self.app.workspaces.active_key().clone();
        match self
            .connection
            .controller
            .pick_paths(owner, DesktopPickerKind::Attachments)
        {
            Ok(transition) => self.apply_transition(transition, cx),
            Err(error) => {
                self.app
                    .workspaces
                    .active_mut()
                    .set_preference_notice(error.to_string());
                self.refresh_views(UiChangeSet::one(UiRegion::Toast), cx);
            }
        }
    }

    pub(super) fn choose_project_directory(&mut self, cx: &mut Context<Self>) {
        if !self
            .app
            .workspaces
            .active_mut()
            .project_directory_editable()
        {
            return;
        }
        let owner = self.app.workspaces.active_key().clone();
        match self
            .connection
            .controller
            .pick_paths(owner, DesktopPickerKind::ProjectDirectory)
        {
            Ok(transition) => self.apply_transition(transition, cx),
            Err(error) => {
                self.app
                    .workspaces
                    .active_mut()
                    .set_preference_notice(error.to_string());
                self.refresh_views(UiChangeSet::one(UiRegion::Toast), cx);
            }
        }
    }

    pub(super) fn clear_project_directory(&mut self, cx: &mut Context<Self>) -> bool {
        if !self
            .app
            .workspaces
            .active_mut()
            .project_directory_editable()
        {
            return false;
        }
        let projectless_selection = self.app.workspace_defaults.projectless_selection.clone();
        self.app.workspaces.active_mut().draft_workspace_selection = projectless_selection;
        self.refresh_views(UiChangeSet::one(UiRegion::Composer), cx);
        cx.notify();
        true
    }

    pub(super) fn remove_composer_attachment(&mut self, index: usize, cx: &mut Context<Self>) {
        if index < self.app.workspaces.active_mut().composer_attachments.len() {
            self.app
                .workspaces
                .active_mut()
                .composer_attachments
                .remove(index);
            self.refresh_views(UiChangeSet::one(UiRegion::Composer), cx);
            cx.notify();
        }
    }

    pub(super) fn submit_active_control(
        &mut self,
        kind: ComposerSubmissionKind,
        cx: &mut Context<Self>,
    ) {
        if !self
            .app
            .workspaces
            .active_mut()
            .composer_attachments
            .is_empty()
        {
            self.app.workspaces.active_mut().set_preference_notice(
                "Attachments cannot be added to a running operation; the draft was retained."
                    .into(),
            );
            self.refresh_views(UiChangeSet::one(UiRegion::Composer), cx);
            self.refresh_views(UiChangeSet::one(UiRegion::Toast), cx);
            cx.notify();
            return;
        }
        if kind == ComposerSubmissionKind::Prompt {
            self.app.workspaces.active_mut().set_preference_notice(
                "Prompt submissions must use the idle composer action.".into(),
            );
            self.refresh_views(UiChangeSet::one(UiRegion::Toast), cx);
            cx.notify();
            return;
        }
        let intent = match kind {
            ComposerSubmissionKind::Steer => DesktopCommandIntent::Steer,
            ComposerSubmissionKind::FollowUp => DesktopCommandIntent::FollowUp,
            ComposerSubmissionKind::Prompt => {
                unreachable!("prompt submission was rejected before command reservation")
            }
        };
        let Some(command_id) = self.reserve_command(intent.clone()) else {
            self.refresh_views(UiChangeSet::one(UiRegion::Toast), cx);
            cx.notify();
            return;
        };
        let payload = match self
            .app
            .workspaces
            .active_mut()
            .composer
            .begin_submit(command_id, kind)
        {
            Ok(payload) => payload.to_owned(),
            Err(error) => {
                self.complete_active_command(command_id, &intent);
                self.app
                    .workspaces
                    .active_mut()
                    .set_preference_notice(error.to_string());
                self.refresh_views(UiChangeSet::one(UiRegion::Composer), cx);
                self.refresh_views(UiChangeSet::one(UiRegion::Toast), cx);
                cx.notify();
                return;
            }
        };
        let session_id = self
            .app
            .workspaces
            .active_mut()
            .projection
            .as_ref()
            .map(|projection| projection.snapshot().session.session_id.clone());
        let admission = self.connection.runtime_client.as_ref().map_or_else(
            || Err("desktop runtime is stopped".to_owned()),
            |runtime| {
                let Some(session_id) = session_id.as_deref() else {
                    return Err("desktop session is unavailable".to_owned());
                };
                let result = match kind {
                    ComposerSubmissionKind::Steer => {
                        runtime.try_steer_for_session(command_id, session_id, &payload)
                    }
                    ComposerSubmissionKind::FollowUp => {
                        runtime.try_follow_up_for_session(command_id, session_id, &payload)
                    }
                    ComposerSubmissionKind::Prompt => {
                        unreachable!("prompt submission was rejected before runtime admission")
                    }
                };
                result.map_err(|error| error.to_string())
            },
        );
        if let Err(message) = admission {
            self.complete_active_command(command_id, &intent);
            let _ = self
                .app
                .workspaces
                .active_mut()
                .composer
                .rejected(command_id, message);
        }
        self.refresh_views(UiChangeSet::one(UiRegion::Composer), cx);
        self.refresh_views(UiChangeSet::one(UiRegion::Inspector), cx);
        self.refresh_views(UiChangeSet::one(UiRegion::ConversationHeader), cx);
        cx.notify();
    }

    pub(super) fn abort_active_operation(&mut self, cx: &mut Context<Self>) {
        if self.active_command_contains_where(|intent| {
            matches!(intent, DesktopCommandIntent::Abort { .. })
        }) {
            return;
        }
        let Some(operation_id) = self
            .app
            .workspaces
            .active_mut()
            .projection
            .as_ref()
            .and_then(|projection| projection.snapshot().active_operation.clone())
        else {
            self.app
                .workspaces
                .active_mut()
                .set_preference_notice("No active operation is available to abort.".into());
            self.refresh_views(UiChangeSet::one(UiRegion::Toast), cx);
            cx.notify();
            return;
        };
        let session_id = self
            .app
            .workspaces
            .active_mut()
            .projection
            .as_ref()
            .expect("an active operation always belongs to a session projection")
            .snapshot()
            .session
            .session_id
            .clone();
        let intent = DesktopCommandIntent::Abort { operation_id };
        let Some(command_id) = self.reserve_command(intent.clone()) else {
            self.refresh_views(UiChangeSet::one(UiRegion::Toast), cx);
            cx.notify();
            return;
        };
        let admission = self.connection.runtime_client.as_ref().map_or_else(
            || Err("desktop runtime is stopped".to_owned()),
            |runtime| {
                runtime
                    .try_abort_for_session(command_id, &session_id)
                    .map_err(|error| error.to_string())
            },
        );
        match admission {
            Ok(()) => {
                self.app
                    .workspaces
                    .active_mut()
                    .set_preference_notice("Abort requested…".into());
            }
            Err(message) => {
                self.complete_active_command(command_id, &intent);
                self.app
                    .workspaces
                    .active_mut()
                    .set_preference_notice(message);
            }
        }
        self.refresh_views(UiChangeSet::one(UiRegion::Toast), cx);
        self.refresh_views(UiChangeSet::one(UiRegion::ConversationHeader), cx);
        cx.notify();
    }

    pub(super) fn reload_local_resources(&mut self, cx: &mut Context<Self>) {
        let intent = DesktopCommandIntent::Reload;
        if self.active_command_contains(&intent) {
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
        {
            self.app.workspaces.active_mut().set_preference_notice(
                "Reload is available only while the runtime is idle.".into(),
            );
            self.refresh_views(UiChangeSet::one(UiRegion::Toast), cx);
            cx.notify();
            return;
        }
        let Some(command_id) = self.reserve_command(intent.clone()) else {
            self.refresh_views(UiChangeSet::one(UiRegion::Toast), cx);
            cx.notify();
            return;
        };
        let target = self.app.workspaces.active_mut().runtime_owner_target();
        let admission = self.connection.runtime_client.as_ref().map_or_else(
            || Err("desktop runtime is stopped".to_owned()),
            |runtime| {
                runtime
                    .try_reload(command_id, target)
                    .map_err(|error| error.to_string())
            },
        );
        match admission {
            Ok(()) => {
                self.app
                    .workspaces
                    .active_mut()
                    .set_preference_notice("Reloading local resources…".into());
            }
            Err(message) => {
                self.complete_active_command(command_id, &intent);
                self.app
                    .workspaces
                    .active_mut()
                    .set_preference_notice(message);
            }
        }
        self.refresh_views(UiChangeSet::one(UiRegion::Toast), cx);
        self.refresh_views(UiChangeSet::one(UiRegion::ConversationHeader), cx);
        cx.notify();
    }

    pub(super) fn submit_recovery_action(
        &mut self,
        identity: DesktopRecoveryIdentity,
        action: DesktopRecoveryAction,
        cx: &mut Context<Self>,
    ) {
        if self.active_command_contains_where(|intent| {
            matches!(intent, DesktopCommandIntent::Recovery { .. })
        }) {
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
        {
            self.app.workspaces.active_mut().set_preference_notice(
                "Recovery actions are available only while the runtime is idle.".into(),
            );
            self.refresh_views(UiChangeSet::one(UiRegion::Toast), cx);
            cx.notify();
            return;
        }
        let intent = DesktopCommandIntent::Recovery {
            recovery_id: identity.recovery_id.clone(),
            action,
        };
        let Some(command_id) = self.reserve_command(intent.clone()) else {
            cx.notify();
            return;
        };
        let admission = self.connection.runtime_client.as_ref().map_or_else(
            || Err("desktop runtime is stopped".to_owned()),
            |runtime| {
                let result = match action {
                    DesktopRecoveryAction::Retry => {
                        runtime.try_retry_recovery(command_id, &identity)
                    }
                    DesktopRecoveryAction::MarkFailed => runtime.try_resolve_recovery(
                        command_id,
                        &identity,
                        CodingAgentRecoveryResolution::Failed,
                    ),
                    DesktopRecoveryAction::Abort => runtime.try_resolve_recovery(
                        command_id,
                        &identity,
                        CodingAgentRecoveryResolution::Aborted,
                    ),
                };
                result.map_err(|error| error.to_string())
            },
        );
        match admission {
            Ok(()) => {
                self.app
                    .workspaces
                    .active_mut()
                    .set_preference_notice(format!(
                        "Submitting recovery {}…",
                        recovery_action_label(action)
                    ));
            }
            Err(message) => {
                self.complete_active_command(command_id, &intent);
                self.app
                    .workspaces
                    .active_mut()
                    .set_preference_notice(message);
            }
        }
        self.refresh_views(UiChangeSet::one(UiRegion::Inspector), cx);
        cx.notify();
    }

    pub(super) fn submit_selection(
        &mut self,
        selection: DesktopRuntimeSelectionKind,
        id: String,
        cx: &mut Context<Self>,
    ) {
        let selected_profile_id = {
            let workspace = self.app.workspaces.active();
            workspace
                .projection
                .as_ref()
                .map(|projection| {
                    projection
                        .snapshot()
                        .session
                        .default_agent_profile_id
                        .as_str()
                })
                .unwrap_or(workspace.project.default_agent_profile_id.as_str())
                .to_owned()
        };
        let already_selected = match selection {
            DesktopRuntimeSelectionKind::Model => {
                id == self.app.workspaces.active_mut().project.selected_model_id
            }
            DesktopRuntimeSelectionKind::SessionProfile => id == selected_profile_id,
        };
        if already_selected {
            return;
        }
        if self.active_command_contains_where(|intent| {
            matches!(intent, DesktopCommandIntent::Selection(_))
        }) {
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
        {
            self.app.workspaces.active_mut().set_preference_notice(
                "Model and profile selection is available only while idle.".into(),
            );
            self.refresh_views(UiChangeSet::one(UiRegion::Toast), cx);
            cx.notify();
            return;
        }
        let intent = DesktopCommandIntent::Selection(selection);
        let Some(command_id) = self.reserve_command(intent.clone()) else {
            self.refresh_views(UiChangeSet::one(UiRegion::Toast), cx);
            cx.notify();
            return;
        };
        let target = self.app.workspaces.active_mut().runtime_owner_target();
        let admission = self.connection.runtime_client.as_ref().map_or_else(
            || Err("desktop runtime is stopped".to_owned()),
            |runtime| {
                let result = match selection {
                    DesktopRuntimeSelectionKind::Model => runtime.try_select_model(
                        command_id,
                        target,
                        &id,
                        self.app
                            .workspaces
                            .active_mut()
                            .thinking_selection
                            .explicit(),
                    ),
                    DesktopRuntimeSelectionKind::SessionProfile => {
                        runtime.try_select_session_profile(command_id, target, &id)
                    }
                };
                result.map_err(|error| error.to_string())
            },
        );
        match admission {
            Ok(()) => {
                self.app
                    .workspaces
                    .active_mut()
                    .set_preference_notice("Applying selection…".into());
            }
            Err(message) => {
                self.complete_active_command(command_id, &intent);
                self.app
                    .workspaces
                    .active_mut()
                    .set_preference_notice(message);
            }
        }
        self.refresh_views(UiChangeSet::one(UiRegion::Toast), cx);
        self.refresh_views(UiChangeSet::one(UiRegion::ConversationHeader), cx);
        cx.notify();
    }

    pub(super) fn select_thinking_level(
        &mut self,
        selection: DesktopThinkingLevel,
        cx: &mut Context<Self>,
    ) {
        let options = {
            let workspace = self.app.workspaces.active();
            conversation_header::thinking_menu(
                workspace
                    .project
                    .models
                    .iter()
                    .find(|model| model.id == workspace.project.selected_model_id),
            )
        };
        if !options.iter().any(|option| option.selection == selection) {
            return;
        }
        if self.app.workspaces.active_mut().thinking_selection == selection {
            return;
        }
        self.app.workspaces.active_mut().thinking_selection = selection;
        self.app.workspaces.active_mut().thinking_hint = None;
        let session_id = self
            .app
            .workspaces
            .active_mut()
            .projection
            .as_ref()
            .map(|projection| projection.snapshot().session.session_id.clone());
        if let Some(session_id) = session_id.as_deref() {
            self.remember_thinking_selection(session_id, selection);
        }
        let label = self.app.workspaces.active_mut().thinking_selection.label(
            self.app
                .workspaces
                .active_mut()
                .project
                .settings
                .default_thinking_level
                .as_deref(),
        );
        self.app
            .workspaces
            .active_mut()
            .set_preference_notice(format!(
                "{} will use thinking {label}.",
                if session_id.is_some() {
                    "This session"
                } else {
                    "The next session"
                }
            ));
        self.refresh_views(UiChangeSet::one(UiRegion::Toast), cx);
        self.refresh_views(UiChangeSet::one(UiRegion::ConversationHeader), cx);
        self.refresh_views(UiChangeSet::one(UiRegion::Inspector), cx);
        cx.notify();
    }

    pub(super) fn cycle_thinking_selection(&mut self, cx: &mut Context<Self>) {
        let options = {
            let workspace = self.app.workspaces.active();
            conversation_header::thinking_menu(
                workspace
                    .project
                    .models
                    .iter()
                    .find(|model| model.id == workspace.project.selected_model_id),
            )
        };
        let Some(next) = options
            .iter()
            .position(|option| {
                option.selection == self.app.workspaces.active_mut().thinking_selection
            })
            .map(|index| options[(index + 1) % options.len()].selection)
            .or_else(|| options.first().map(|option| option.selection))
        else {
            return;
        };
        self.select_thinking_level(next, cx);
    }

    pub(super) fn decide_tool_authorization(
        &mut self,
        identity: ToolAuthorizationIdentity,
        decision: ToolAuthorizationDecision,
        cx: &mut Context<Self>,
    ) {
        if self.active_command_contains_where(|intent| {
            matches!(intent, DesktopCommandIntent::Authorization { .. })
        }) {
            return;
        }
        let intent = DesktopCommandIntent::Authorization {
            authorization_id: identity.authorization_id.clone(),
            operation_id: identity.operation_id.clone(),
        };
        let Some(command_id) = self.reserve_command(intent.clone()) else {
            self.refresh_views(UiChangeSet::one(UiRegion::Toast), cx);
            cx.notify();
            return;
        };
        let admission = self.connection.runtime_client.as_ref().map_or_else(
            || Err("desktop runtime is stopped".to_owned()),
            |runtime| {
                runtime
                    .try_decide_tool_authorization(command_id, &identity, decision)
                    .map_err(|error| error.to_string())
            },
        );
        match admission {
            Ok(()) => {
                self.app
                    .workspaces
                    .active_mut()
                    .set_preference_notice("Authorization decision pending…".into());
            }
            Err(message) => {
                self.complete_active_command(command_id, &intent);
                self.app
                    .workspaces
                    .active_mut()
                    .set_preference_notice(message);
            }
        }
        self.refresh_views(UiChangeSet::one(UiRegion::Toast), cx);
        self.refresh_views(UiChangeSet::one(UiRegion::Modal), cx);
        cx.notify();
    }
}
