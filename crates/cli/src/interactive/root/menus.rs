use super::*;

impl InteractiveRoot {
    pub(in crate::interactive) fn open_delegation_confirmation_menu(
        &mut self,
        pending: Vec<PendingDelegationConfirmation>,
    ) {
        self.delegation_confirmation_menu = Some(DelegationConfirmationMenuState::new(pending));
        self.pending_delegation_rejection_reason = None;
        self.profile_menu = None;
        self.pending_profile_task = None;
        self.local.editor.set_text("");
        self.slash_suggestion_selected = 0;
        self.slash_suggestions_dismissed_for = None;
    }

    pub(super) fn enqueue_delegation_confirmation(
        &mut self,
        pending: PendingDelegationConfirmation,
    ) {
        if let Some(menu) = self.delegation_confirmation_menu.as_mut() {
            menu.upsert(pending);
        } else {
            self.delegation_confirmation_menu =
                Some(DelegationConfirmationMenuState::new(vec![pending]));
        }
        self.pending_delegation_rejection_reason = None;
        self.profile_menu = None;
        self.pending_profile_task = None;
    }

    pub(super) fn resolve_delegation_confirmation(
        &mut self,
        operation_id: &str,
        tool_call_id: &str,
    ) {
        let Some(menu) = self.delegation_confirmation_menu.as_mut() else {
            return;
        };
        menu.remove(operation_id, tool_call_id);
        if menu.is_empty() {
            self.delegation_confirmation_menu = None;
        }
    }

    pub(in crate::interactive) fn has_active_delegation_confirmation_menu(&self) -> bool {
        self.delegation_confirmation_menu.is_some()
    }

    pub(in crate::interactive) fn handle_delegation_confirmation_menu_input(
        &mut self,
        event: &InputEvent,
    ) -> bool {
        let Some(menu) = self.delegation_confirmation_menu.as_mut() else {
            return false;
        };
        let outcome = menu.handle_input(&self.local.keybindings, event);
        match outcome {
            DelegationConfirmationMenuOutcome::None => {}
            DelegationConfirmationMenuOutcome::Close => {
                self.delegation_confirmation_menu = None;
                self.local.editor.set_text("");
            }
            DelegationConfirmationMenuOutcome::Approve {
                operation_id,
                tool_call_id,
            } => {
                self.delegation_confirmation_menu = None;
                self.pending_delegation_confirmation_command =
                    Some(PendingDelegationConfirmationCommand::Approve {
                        selection: PendingDelegationConfirmationSelection {
                            operation_id: Some(operation_id),
                            tool_call_id,
                        },
                    });
                self.action = InteractiveAction::DelegationConfirmation;
            }
            DelegationConfirmationMenuOutcome::Reject {
                operation_id,
                tool_call_id,
            } => {
                self.delegation_confirmation_menu = None;
                self.pending_delegation_confirmation_command =
                    Some(PendingDelegationConfirmationCommand::Reject {
                        selection: PendingDelegationConfirmationSelection {
                            operation_id: Some(operation_id),
                            tool_call_id,
                        },
                        reason: None,
                    });
                self.action = InteractiveAction::DelegationConfirmation;
            }
            DelegationConfirmationMenuOutcome::RejectWithReason {
                operation_id,
                tool_call_id,
            } => {
                self.delegation_confirmation_menu = None;
                self.pending_delegation_rejection_reason = Some(PendingDelegationRejectionReason {
                    selection: PendingDelegationConfirmationSelection {
                        operation_id: Some(operation_id),
                        tool_call_id,
                    },
                });
                self.local.editor.set_text("");
            }
        }
        true
    }

    pub(super) fn render_delegation_confirmation_menu(&mut self, width: usize) -> Vec<String> {
        let Some(menu) = self.delegation_confirmation_menu.as_mut() else {
            return Vec::new();
        };
        menu.render(width)
    }

    pub(in crate::interactive) fn has_pending_tool_authorization(&self) -> bool {
        !self.tool_authorizations.is_empty()
    }

    pub(in crate::interactive) fn handle_tool_authorization_input(
        &mut self,
        event: &InputEvent,
    ) -> bool {
        if self.tool_authorizations.is_empty() || matches_key(event, "ctrl+c") {
            return false;
        }
        if matches_key(event, "escape") {
            self.resolve_current_tool_authorization(ToolAuthorizationDecision::Deny {
                reason: None,
            });
            return true;
        }
        if self.local.keybindings.matches(event, "tui.select.up") {
            self.tool_authorization_selected = (self.tool_authorization_selected + 2) % 3;
            return true;
        }
        if self.local.keybindings.matches(event, "tui.select.down") {
            self.tool_authorization_selected = (self.tool_authorization_selected + 1) % 3;
            return true;
        }
        let InputEvent::Key(key_event) = event else {
            return true;
        };
        if key_event.kind == KeyEventKind::Release {
            return true;
        }
        if matches!(key_event.key, Key::Tab) {
            if key_event.modifiers.contains(KeyModifiers::SHIFT) {
                self.tool_authorization_selected = (self.tool_authorization_selected + 2) % 3;
            } else {
                self.tool_authorization_selected = (self.tool_authorization_selected + 1) % 3;
            }
            return true;
        }
        if self.local.keybindings.matches(event, "tui.select.confirm") {
            let decision = match self.tool_authorization_selected {
                0 => ToolAuthorizationDecision::AllowOnce,
                1 => ToolAuthorizationDecision::AllowForOperation,
                _ => ToolAuthorizationDecision::Deny { reason: None },
            };
            self.resolve_current_tool_authorization(decision);
        }
        true
    }

    pub(super) fn resolve_current_tool_authorization(
        &mut self,
        decision: ToolAuthorizationDecision,
    ) {
        let Some(request) = self.tool_authorizations.pop_front() else {
            return;
        };
        self.tool_authorization_selected = 0;
        self.pending_tool_authorization_decision = Some((request, decision));
        self.action = InteractiveAction::ToolAuthorization;
    }

    pub(super) fn render_tool_authorization(&self, width: usize) -> Vec<String> {
        let Some(request) = self.tool_authorizations.front() else {
            return Vec::new();
        };
        let color = color_enabled();
        let content_width = width.saturating_sub(3).max(1);
        let mut inner = vec![fit_line(
            &paint_with(
                &format!("Tool authorization (1/{})", self.tool_authorizations.len()),
                &WARNING,
                color,
            ),
            content_width,
        )];
        inner.push(fit_line(
            &format!(
                "  tool: {}  risk: {}  operation: {}",
                request.tool_name,
                tool_authorization_risk_label(request.risk),
                request.operation_id
            ),
            content_width,
        ));
        inner.push(fit_line(
            &format!("  {}", request.preview.summary),
            content_width,
        ));
        if let Some(path) = request.preview.path.as_deref() {
            inner.push(fit_line(&format!("  path: {path}"), content_width));
        }
        if let Some(cwd) = request.preview.cwd.as_deref() {
            inner.push(fit_line(&format!("  cwd: {cwd}"), content_width));
        }
        if let Some(command) = request.preview.command.as_deref() {
            for (index, command_line) in command.lines().take(3).enumerate() {
                let label = if index == 0 { "command" } else { "       " };
                inner.push(fit_line(
                    &format!("  {label}: {command_line}"),
                    content_width,
                ));
            }
        }
        if let Some(content) = request.preview.content_preview.as_deref() {
            inner.push(fit_line("  preview:", content_width));
            for content_line in content.lines().take(6) {
                inner.push(fit_line(&format!("    {content_line}"), content_width));
            }
        }
        for (index, label) in ["Allow once", "Allow for operation", "Deny"]
            .into_iter()
            .enumerate()
        {
            let marker = if index == self.tool_authorization_selected {
                "->"
            } else {
                "  "
            };
            let line = format!("{marker} {label}");
            if index == self.tool_authorization_selected {
                inner.push(fit_line(&paint_with(&line, &USER, color), content_width));
            } else {
                inner.push(fit_line(&line, content_width));
            }
        }
        inner.push(fit_line(
            &paint_with(
                "Up/Down or Tab choose · Enter confirm · Esc deny · Ctrl+C abort operation",
                &SYSTEM,
                color,
            ),
            content_width,
        ));

        if width < 5 {
            return inner
                .into_iter()
                .map(|line| fit_line(&line, width))
                .collect();
        }

        // Visible warning-colored border so the authorization dialog reads as
        // a modal surface instead of plain transcript text.
        framed_modal_lines(inner, width, &WARNING, color)
    }

    pub(in crate::interactive) fn has_active_profile_menu(&self) -> bool {
        self.profile_menu.is_some()
    }

    pub(in crate::interactive) fn has_pending_delegation_rejection_reason(&self) -> bool {
        self.pending_delegation_rejection_reason.is_some()
    }

    pub(in crate::interactive) fn handle_pending_delegation_rejection_reason_input(
        &mut self,
        event: &InputEvent,
    ) -> bool {
        let Some(pending_reason) = self.pending_delegation_rejection_reason.clone() else {
            return false;
        };
        if matches_key(event, "escape") || matches_key(event, "ctrl+c") {
            self.pending_delegation_rejection_reason = None;
            self.local.editor.set_text("");
            self.transcript
                .push(TranscriptItem::system("Delegation rejection canceled"));
            return true;
        }

        let before_text = self.local.editor.text().to_string();
        self.local.editor.handle_input(event);
        if self.local.editor.text() != before_text {
            self.slash_suggestion_selected = 0;
            self.slash_suggestions_dismissed_for = None;
        }
        if let Some(command) = self.take_scroll_command() {
            let page_rows = self.viewport_height.saturating_sub(2).max(1);
            match command {
                TranscriptScrollCommand::PageUp => self.transcript.scroll_page_up(page_rows),
                TranscriptScrollCommand::PageDown => self.transcript.scroll_page_down(page_rows),
            }
        }
        let Some(text) = self.take_submitted() else {
            return true;
        };
        let reason = text.trim().to_string();
        self.pending_delegation_confirmation_command =
            Some(PendingDelegationConfirmationCommand::Reject {
                selection: pending_reason.selection,
                reason: (!reason.is_empty()).then_some(reason),
            });
        self.pending_delegation_rejection_reason = None;
        self.local.editor.set_text("");
        self.action = InteractiveAction::DelegationConfirmation;
        true
    }

    pub(in crate::interactive) fn has_pending_profile_task(&self) -> bool {
        self.pending_profile_task.is_some()
    }

    pub(in crate::interactive) fn open_agent_menu(&mut self) {
        self.delegation_confirmation_menu = None;
        self.pending_delegation_rejection_reason = None;
        self.profile_menu = Some(ProfileMenuState::agent());
        self.pending_profile_task = None;
        self.local.editor.set_text("");
        self.slash_suggestion_selected = 0;
        self.slash_suggestions_dismissed_for = None;
    }

    pub(in crate::interactive) fn open_team_menu(&mut self) {
        self.delegation_confirmation_menu = None;
        self.pending_delegation_rejection_reason = None;
        self.profile_menu = Some(ProfileMenuState::team());
        self.pending_profile_task = None;
        self.local.editor.set_text("");
        self.slash_suggestion_selected = 0;
        self.slash_suggestions_dismissed_for = None;
    }

    pub(in crate::interactive) fn handle_profile_menu_input(&mut self, event: &InputEvent) -> bool {
        let default_agent_profile_id = self.display_default_agent_profile_id().clone();
        let Some(menu) = self.profile_menu.as_mut() else {
            return false;
        };
        let outcome = menu.handle_input(
            &self.local.keybindings,
            event,
            &self.profile_catalog,
            &default_agent_profile_id,
        );
        match outcome {
            ProfileMenuOutcome::None => {}
            ProfileMenuOutcome::Close => {
                self.profile_menu = None;
                self.local.editor.set_text("");
            }
            ProfileMenuOutcome::SetDefaultAgent(profile_id) => {
                self.profile_menu = None;
                self.set_default_agent_profile_id(profile_id.clone());
                self.queue_command(PendingInteractiveCommand::UseAgentProfile(
                    profile_id.clone(),
                ));
                self.transcript.push(TranscriptItem::system(format!(
                    "Default agent profile: {profile_id}"
                )));
            }
            ProfileMenuOutcome::BeginAgentTask(profile_id) => {
                self.profile_menu = None;
                self.pending_profile_task = Some(PendingProfileTask::Agent { profile_id });
                self.local.editor.set_text("");
            }
            ProfileMenuOutcome::BeginTeamTask(team_id) => {
                self.profile_menu = None;
                self.pending_profile_task = Some(PendingProfileTask::Team { team_id });
                self.local.editor.set_text("");
            }
        }
        true
    }

    pub(in crate::interactive) fn handle_pending_profile_task_input(
        &mut self,
        event: &InputEvent,
    ) -> bool {
        let Some(pending_task) = self.pending_profile_task.clone() else {
            return false;
        };
        if matches_key(event, "escape") || matches_key(event, "ctrl+c") {
            self.pending_profile_task = None;
            self.local.editor.set_text("");
            self.transcript
                .push(TranscriptItem::system("Profile task canceled"));
            return true;
        }

        let before_text = self.local.editor.text().to_string();
        self.local.editor.handle_input(event);
        if self.local.editor.text() != before_text {
            self.slash_suggestion_selected = 0;
            self.slash_suggestions_dismissed_for = None;
        }
        if let Some(command) = self.take_scroll_command() {
            let page_rows = self.viewport_height.saturating_sub(2).max(1);
            match command {
                TranscriptScrollCommand::PageUp => self.transcript.scroll_page_up(page_rows),
                TranscriptScrollCommand::PageDown => self.transcript.scroll_page_down(page_rows),
            }
        }
        let Some(text) = self.take_submitted() else {
            return true;
        };
        let task = text.trim().to_string();
        if task.is_empty() {
            self.transcript
                .push(TranscriptItem::system("Profile task requires text"));
            return true;
        }
        self.local.editor.add_to_history(&task);
        match pending_task {
            PendingProfileTask::Agent { profile_id } => {
                self.queue_command(PendingInteractiveCommand::AgentInvocation(
                    PendingAgentInvocationRequest { profile_id, task },
                ));
            }
            PendingProfileTask::Team { team_id } => {
                self.queue_command(PendingInteractiveCommand::AgentTeam(
                    PendingAgentTeamRequest { team_id, task },
                ));
            }
        }
        self.pending_profile_task = None;
        true
    }

    pub(super) fn render_profile_menu(&mut self, width: usize) -> Vec<String> {
        let default_agent_profile_id = self.display_default_agent_profile_id().clone();
        let Some(menu) = self.profile_menu.as_mut() else {
            return Vec::new();
        };
        menu.render(&self.profile_catalog, &default_agent_profile_id, width)
    }

    pub(super) fn render_pending_delegation_rejection_reason(&self, width: usize) -> Vec<String> {
        let Some(pending_reason) = &self.pending_delegation_rejection_reason else {
            return Vec::new();
        };
        let operation_id = pending_reason
            .selection
            .operation_id
            .as_deref()
            .unwrap_or("unknown-operation");
        let text = format!(
            "Delegation rejection reason for {operation_id} {}: enter reason, then press Enter",
            pending_reason.selection.tool_call_id
        );
        vec![fit_line(
            &paint_with(&text, &SYSTEM, color_enabled()),
            width,
        )]
    }

    pub(super) fn render_pending_profile_task(&self, width: usize) -> Vec<String> {
        let Some(pending_task) = &self.pending_profile_task else {
            return Vec::new();
        };
        let text = match pending_task {
            PendingProfileTask::Agent { profile_id } => {
                format!("Agent {profile_id}: enter task, then press Enter")
            }
            PendingProfileTask::Team { team_id } => {
                format!("Team {team_id}: enter task, then press Enter")
            }
        };
        vec![fit_line(
            &paint_with(&text, &SYSTEM, color_enabled()),
            width,
        )]
    }
}
