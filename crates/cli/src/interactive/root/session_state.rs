use super::*;

impl InteractiveRoot {
    pub(in crate::interactive) fn apply_prompt_context(&mut self, prompt_context: &PromptContext) {
        self.cwd = prompt_context.cwd.clone();
        self.model_id = prompt_context.model_summary.id.clone();
        self.model = Some(prompt_context.model_summary.clone());
        self.thinking_level = prompt_context.thinking_level.unwrap_or_default();
        self.available_models = prompt_context.model_choices.clone();
        self.model_rotation = prompt_context.model_rotation.clone();
        self.session_query = prompt_context.session_query.clone();
        self.session_choices = prompt_context.session_choices.clone();
        self.theme = prompt_context.theme.clone();
        self.settings = prompt_context.settings_snapshot();
        self.local.settings_list = build_settings_list(
            self.settings.clone(),
            &self.theme,
            self.local.keybindings.clone(),
        );
        self.local.render_cache.clear();
        self.auth_snapshot = prompt_context.auth_controller.snapshot();
        self.resource_commands = prompt_context.resource_commands.clone();
        self.profile_catalog = prompt_context.profile_catalog.clone();
        self.set_default_agent_profile_id(prompt_context.default_agent_profile_id.clone());
    }

    pub(in crate::interactive) fn resource_prompt_invocation(
        &self,
        command: &ParsedSlashCommand,
    ) -> Option<PromptInvocation> {
        let skill = if self.settings.runtime.enable_skill_commands {
            self.resource_commands.iter().find(|resource| {
                resource.kind == CodingAgentResourceCommandKind::Skill
                    && resource.command == command.name
            })
        } else {
            None
        };
        skill
            .or_else(|| {
                self.resource_commands.iter().find(|resource| {
                    resource.kind == CodingAgentResourceCommandKind::PromptTemplate
                        && resource.command == command.name
                })
            })
            .map(|resource| resource.prompt_invocation(&command.args))
    }

    pub(in crate::interactive) fn all_slash_commands(&self) -> Vec<slash::BuiltinSlashCommand> {
        let mut commands = slash::builtin_slash_commands();
        for resource in &self.resource_commands {
            if resource.kind == CodingAgentResourceCommandKind::Skill
                && !self.settings.runtime.enable_skill_commands
            {
                continue;
            }
            commands.push(slash::BuiltinSlashCommand {
                name: resource.command.clone(),
                description: resource.description.clone(),
            });
        }
        commands
    }

    pub(in crate::interactive) fn push_user(&mut self, prompt: String) {
        self.transcript.push(TranscriptItem::user(prompt));
    }

    pub(in crate::interactive) fn apply_events(&mut self, events: Vec<UiEvent>) {
        let previous_scroll_offset = self.transcript.scroll_offset();
        let previous_rows = if previous_scroll_offset > 0 {
            Some(self.transcript_row_snapshot(MAX_TOOL_RESULT_LINES))
        } else {
            None
        };
        let mut mutation = TranscriptMutation::default();
        for event in events {
            match event {
                UiEvent::ToolAuthorizationRequired { request } => {
                    if self
                        .tool_authorizations
                        .iter()
                        .all(|pending| pending.authorization_id != request.authorization_id)
                        && self
                            .pending_tool_authorization_decision
                            .as_ref()
                            .is_none_or(|(pending, _)| {
                                pending.authorization_id != request.authorization_id
                            })
                    {
                        self.tool_authorizations.push_back(request);
                    }
                }
                UiEvent::ToolAuthorizationResolved { authorization_id } => {
                    self.tool_authorizations
                        .retain(|request| request.authorization_id != authorization_id);
                    if self
                        .pending_tool_authorization_decision
                        .as_ref()
                        .is_some_and(|(request, _)| request.authorization_id == authorization_id)
                    {
                        self.pending_tool_authorization_decision = None;
                    }
                    self.tool_authorization_selected = self.tool_authorization_selected.min(2);
                }
                UiEvent::DelegationConfirmationRequired { pending } => {
                    self.enqueue_delegation_confirmation(pending);
                }
                UiEvent::DelegationConfirmationResolved {
                    operation_id,
                    tool_call_id,
                } => {
                    self.resolve_delegation_confirmation(&operation_id, &tool_call_id);
                }
                UiEvent::UsageUpdate {
                    input,
                    output,
                    cache_read,
                    cache_write,
                    cost,
                    context_tokens,
                } => {
                    // Accumulate delta values from the stateless bridge.
                    // This ensures hydration-seeded stats are preserved:
                    //   root.stats starts at 0 (fresh) or at the hydrated
                    //   cumulative value, and each UsageUpdate adds to it.
                    self.stats.input = self.stats.input.saturating_add(input);
                    self.stats.output = self.stats.output.saturating_add(output);
                    self.stats.cache_read = self.stats.cache_read.saturating_add(cache_read);
                    self.stats.cache_write = self.stats.cache_write.saturating_add(cache_write);
                    self.stats.cost += cost;
                    self.stats.context_tokens = context_tokens;
                }
                other => mutation.extend(self.transcript.apply_event_with_mutation(other)),
            }
        }
        if let Some(previous_rows) = previous_rows {
            let anchor_start_row = Some(
                previous_rows
                    .total_rows()
                    .saturating_sub(previous_scroll_offset)
                    .saturating_sub(self.conversation_viewport_height.max(1)),
            );
            let row_delta_below_anchor = self.transcript_row_delta_since(
                previous_rows,
                mutation.changed_indices(),
                MAX_TOOL_RESULT_LINES,
                anchor_start_row,
            );
            self.transcript.preserve_scrolled_view_after_hidden_change(
                previous_scroll_offset,
                row_delta_below_anchor,
            );
        }
    }

    pub(in crate::interactive) fn set_status(&mut self, status: InteractiveStatus) {
        if status == InteractiveStatus::Idle {
            self.spinner_frame = 0;
        }
        self.status = status;
    }

    pub(in crate::interactive) fn handle_slash_command(&mut self, command: ParsedSlashCommand) {
        commands::handle_slash_command(self, command);
    }

    pub(in crate::interactive) fn handle_empty_editor_escape(&mut self) {
        let action = self.settings.presentation.double_escape_action;
        if action == CodingAgentDoubleEscapeAction::None {
            self.last_empty_editor_escape_at = None;
            return;
        }

        let now = Instant::now();
        let is_double_escape = self
            .last_empty_editor_escape_at
            .is_some_and(|previous| now.duration_since(previous) < DOUBLE_ESCAPE_WINDOW);
        if !is_double_escape {
            self.last_empty_editor_escape_at = Some(now);
            return;
        }

        self.last_empty_editor_escape_at = None;
        match action {
            CodingAgentDoubleEscapeAction::Fork => self.handle_slash_command(ParsedSlashCommand {
                name: "fork".to_string(),
                args: String::new(),
                original: "/fork".to_string(),
            }),
            CodingAgentDoubleEscapeAction::Tree => self.handle_slash_command(ParsedSlashCommand {
                name: "tree".to_string(),
                args: String::new(),
                original: "/tree".to_string(),
            }),
            CodingAgentDoubleEscapeAction::None => {}
        }
    }

    pub(in crate::interactive) fn clear_empty_editor_escape(&mut self) {
        self.last_empty_editor_escape_at = None;
    }

    pub(super) fn set_selected_model(&mut self, model: CodingAgentModelCatalogEntry) {
        self.set_selected_model_with_thinking(model, None);
    }

    pub(in crate::interactive) fn set_selected_model_with_thinking(
        &mut self,
        model: CodingAgentModelCatalogEntry,
        thinking_level: Option<CodingAgentThinkingLevel>,
    ) {
        self.model_id = model.id.clone();
        self.model = Some(model.clone());
        self.thinking_level = thinking_level.unwrap_or_default();
        self.local.selected_model = Some(model);
        self.local.selected_thinking_level = thinking_level;
        self.local.selecting_model = false;
        self.local.model_selection_selected = 0;
        self.local.editor.set_text("");
        let suffix = thinking_level
            .map(|level| format!(" (thinking: {level})"))
            .unwrap_or_default();
        self.transcript.push(TranscriptItem::system(format!(
            "Model set: {}{}",
            self.model_id, suffix
        )));
    }

    pub(in crate::interactive) fn cycle_model_rotation(&mut self, reverse: bool) {
        if self.model_rotation.is_empty() {
            return;
        }
        let len = self.model_rotation.len();
        let next_index = match self
            .model_rotation
            .iter()
            .position(|model| model.id == self.model_id)
        {
            Some(index) if reverse => (index + len - 1) % len,
            Some(index) => (index + 1) % len,
            None if reverse => len - 1,
            None => 0,
        };
        let model = self.model_rotation[next_index].clone();
        self.set_selected_model(model);
    }

    pub(in crate::interactive) fn set_selected_session(&mut self, choice: SessionChoice) {
        self.session_label = choice.display_name().to_string();
        self.local.selected_session = Some(choice.clone());
        self.local.selected_session_hydrate = true;
        self.set_active_session_choice(choice.clone());
        self.local.selecting_session = false;
        self.local.session_selection_selected = 0;
        self.local.editor.set_text("");
        self.transcript.push(TranscriptItem::system(format!(
            "Session selected: {}",
            choice.display_name()
        )));
    }

    pub(in crate::interactive) fn apply_hydrated_session(
        &mut self,
        hydrated: HydratedSession,
        notice: Option<String>,
    ) {
        self.session_label = hydrated.choice.display_name().to_string();
        let mut choice = hydrated.choice.clone();
        choice.active_leaf_id = hydrated.leaf_id.clone();
        self.set_active_session_choice(choice);
        // Restore cumulative token/cost stats so the footer reflects the
        // entire session immediately after resume, without waiting for the
        // next turn to emit a UsageUpdate event.
        self.stats = FooterStats {
            input: hydrated.cumulative_usage.input,
            output: hydrated.cumulative_usage.output,
            cache_read: hydrated.cumulative_usage.cache_read,
            cache_write: hydrated.cumulative_usage.cache_write,
            cost: hydrated.cumulative_usage.cost,
            context_tokens: hydrated.cumulative_usage.last_context_tokens,
        };

        let mut transcript = Transcript::new();
        if let Some(first) = self.transcript.items().first().cloned() {
            transcript.push(first);
        }
        for item in hydrated.transcript_items {
            transcript.push(item);
        }
        if let Some(notice) = notice {
            transcript.push(TranscriptItem::system(notice));
        }
        self.transcript = transcript;
        self.local.render_cache.clear();
    }

    pub(in crate::interactive) fn set_active_session_choice(&mut self, choice: SessionChoice) {
        self.active_leaf_id = choice.active_leaf_id.clone();
        self.active_session = Some(choice);
    }

    pub(in crate::interactive) fn clear_active_session(&mut self) {
        self.active_session = None;
        self.active_leaf_id = None;
    }

    pub(in crate::interactive) fn render_state(&self) -> InteractiveRenderState {
        InteractiveRenderState {
            editor_text: self.local.editor.text().to_string(),
            editor_cursor: self.local.editor.cursor(),
            transcript_revision: self.transcript.revision(),
            transcript_view_revision: self.local.transcript_view.revision(),
            selected_transcript_block: self.local.transcript_view.selected(),
            transcript_scroll_offset: self.transcript.scroll_offset(),
            transcript_has_new_output_below: self.transcript.has_new_output_below(),
            focused_region: self.local.focus_ring.current(),
            context_tab: self.local.context_tab,
            context_projection: Some(self.shared_projection.context().clone()),
            capabilities: self.shared_projection.capabilities().cloned(),
            context_selection: self.local.context_selection,
            context_scroll: self.local.context_scroll,
            context_detail: self.local.context_detail.clone(),
            context_open: self.local.context_open,
            status: self.status,
            stats: self.stats,
            tool_output_expanded: self.tool_output_expanded,
            spinner_frame: self.spinner_frame,
            permission_mode: self.permission_mode,
            slash_suggestion_selected: self.slash_suggestion_selected,
            slash_suggestions_dismissed_for: self.slash_suggestions_dismissed_for.clone(),
            selecting_settings: self.local.selecting_settings,
            selecting_tree: self.local.selecting_tree,
            tree_selector_state: self
                .local
                .tree_selector
                .as_ref()
                .map(|selector| selector.render_state()),
            settings: self.settings.clone(),
            auth_snapshot: self.auth_snapshot.clone(),
            theme_name: self.theme.name.clone(),
            settings_selected_item_id: self
                .local
                .settings_list
                .selected_item()
                .map(|item| item.id.clone()),
            selecting_model: self.local.selecting_model,
            model_selection_selected: self.local.model_selection_selected,
            selecting_session: self.local.selecting_session,
            session_selection_selected: self.local.session_selection_selected,
            delegation_confirmation_menu_state: self
                .delegation_confirmation_menu
                .as_ref()
                .map(|menu| menu.render_state()),
            pending_delegation_rejection_reason: self.pending_delegation_rejection_reason.clone(),
            tool_authorization_ids: self
                .tool_authorizations
                .iter()
                .map(|request| request.authorization_id.clone())
                .collect(),
            tool_authorization_selected: self.tool_authorization_selected,
            profile_menu_state: self.profile_menu.as_ref().map(|menu| menu.render_state()),
            pending_profile_task: self.pending_profile_task.clone(),
        }
    }
}
