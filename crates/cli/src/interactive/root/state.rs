use super::*;

impl InteractiveRoot {
    pub(in crate::interactive) fn new_with_theme_models_and_settings(
        cwd: PathBuf,
        model_id: String,
        session_label: String,
        theme: TuiTheme,
        available_models: Vec<CodingAgentModelCatalogEntry>,
        settings: CodingAgentSettingsSnapshot,
        auth_snapshot: CodingAgentAuthSnapshot,
    ) -> Self {
        let submitted = Arc::new(Mutex::new(None));
        let submitted_for_callback = Arc::clone(&submitted);
        let scroll_command = Arc::new(Mutex::new(None));
        let page_up_command = Arc::clone(&scroll_command);
        let page_down_command = Arc::clone(&scroll_command);
        let keybindings =
            KeybindingsManager::new(keybindings::default_keybindings(), Default::default());
        let mut editor = Editor::new(keybindings.clone());
        editor.set_on_submit(Box::new(move |text| {
            *submitted_for_callback.lock().unwrap() = Some(text.to_string());
        }));
        editor.set_on_scroll_page_up(Box::new(move || {
            *page_up_command.lock().unwrap() = Some(TranscriptScrollCommand::PageUp);
        }));
        editor.set_on_scroll_page_down(Box::new(move || {
            *page_down_command.lock().unwrap() = Some(TranscriptScrollCommand::PageDown);
        }));
        editor.set_focused(true);
        editor.set_autocomplete_provider(Box::new(CombinedAutocompleteProvider::new(
            Vec::new(),
            &cwd,
        )));

        let mut transcript = Transcript::new();
        transcript.push(TranscriptItem::system(welcome_line()));
        let settings_list = build_settings_list(settings.clone(), &theme, keybindings.clone());
        let default_agent_profile_id = ProfileId::from("default");
        let profile_catalog = CodingAgentProfileCatalog::default();
        let mut focus_ring = FocusRing::new([
            InteractiveRegion::Conversation,
            InteractiveRegion::Context,
            InteractiveRegion::Composer,
        ]);
        focus_ring.focus(InteractiveRegion::Composer);
        let modal_overlay = TransientOverlayBridge::default();
        let support_overlay = TransientOverlayBridge::default();

        Self {
            transcript,
            local: InteractiveLocalState {
                transcript_view: TranscriptViewState::default(),
                render_cache: TranscriptRenderCache::new(),
                editor,
                keybindings,
                submitted,
                scroll_command,
                focus_ring,
                context_tab: ContextTab::Ops,
                context_selection: [0; 4],
                context_scroll: [0; 4],
                context_viewport_height: 1,
                context_change_timing: HashMap::new(),
                context_detail: None,
                context_open: false,
                context_restore_focus: InteractiveRegion::Composer,
                mouse_hits: HitMap::new(),
                modal_overlay,
                support_overlay,
                modal_overlay_handle: None,
                support_overlay_handle: None,
                selecting_tree: false,
                tree_selector: None,
                selected_tree_entry_id: None,
                pending_tree_label_change: None,
                selected_model: None,
                selected_thinking_level: None,
                pending_permission_mode: None,
                selecting_model: false,
                model_selection_selected: 0,
                selected_session: None,
                selected_session_hydrate: false,
                selecting_session: false,
                session_selection_selected: 0,
                selecting_settings: false,
                settings_list,
                settings_command: None,
                auth_command: None,
            },
            pending_command: None,
            pending_delegation_confirmation_command: None,
            delegation_confirmation_menu: None,
            pending_delegation_rejection_reason: None,
            tool_authorizations: VecDeque::new(),
            tool_authorization_selected: 0,
            pending_tool_authorization_decision: None,
            profile_menu: None,
            pending_profile_task: None,
            action: InteractiveAction::None,
            status: InteractiveStatus::Idle,
            viewport_width: 80,
            viewport_height: 24,
            terminal_capabilities: TerminalCapabilities {
                images: None,
                true_color: false,
                hyperlinks: false,
            },
            shared_projection: UiProjection::new(),
            child_conversations: HashMap::new(),
            child_conversation_order: VecDeque::new(),
            active_child_operation_id: None,
            main_transcript: None,
            main_tool_authorizations: None,
            conversation_viewport_width: 1,
            conversation_viewport_height: 1,
            cwd,
            model_id,
            session_label,
            model: None,
            thinking_level: CodingAgentThinkingLevel::default(),
            permission_mode: ToolAuthorizationMode::default(),
            available_models,
            model_rotation: Vec::new(),
            session_query: CodingAgentSessionQuery::disabled(),
            session_choices: Vec::new(),
            active_session: None,
            active_leaf_id: None,
            settings,
            auth_snapshot,
            stats: FooterStats::default(),
            tool_output_expanded: false,
            spinner_frame: 0,
            slash_suggestion_selected: 0,
            slash_suggestions_dismissed_for: None,
            last_empty_editor_escape_at: None,
            theme,
            resolved_theme: None,
            resource_commands: Vec::new(),
            profile_catalog,
            default_agent_profile_id,
            clipboard: Arc::new(SystemClipboard),
        }
    }

    pub(in crate::interactive) fn with_resolved_theme(
        mut self,
        resolved_theme: CodingAgentThemeSnapshot,
    ) -> Self {
        self.resolved_theme = Some(resolved_theme);
        self
    }

    pub(in crate::interactive) fn take_action(&mut self) -> InteractiveAction {
        std::mem::replace(&mut self.action, InteractiveAction::None)
    }

    pub(in crate::interactive) fn transient_overlay_components(
        &self,
    ) -> (
        crate::interactive::transient_overlay::TransientOverlay,
        crate::interactive::transient_overlay::TransientOverlay,
    ) {
        (
            self.local.support_overlay.component(),
            self.local.modal_overlay.component(),
        )
    }

    pub(in crate::interactive) fn install_transient_overlay_handles(
        &mut self,
        support: OverlayHandle,
        modal: OverlayHandle,
    ) {
        self.local.support_overlay_handle = Some(support);
        self.local.modal_overlay_handle = Some(modal);
    }

    pub(in crate::interactive) fn transient_overlay_handles(
        &self,
    ) -> Option<(OverlayHandle, OverlayHandle)> {
        Some((
            self.local.support_overlay_handle?,
            self.local.modal_overlay_handle?,
        ))
    }

    pub(in crate::interactive) fn prepare_transient_overlays(
        &mut self,
        terminal_width: usize,
    ) -> TransientOverlayProjection {
        let modal_role = if self.local.context_detail.is_some() {
            match shell_layout_mode(terminal_width) {
                ShellLayoutMode::Wide => TransientOverlayRole::ContextRailDetail,
                ShellLayoutMode::Medium => TransientOverlayRole::ContextDrawerDetail,
                ShellLayoutMode::Narrow => TransientOverlayRole::ContextPageDetail,
            }
        } else {
            TransientOverlayRole::ModalDialog
        };
        let modal_width = modal_overlay_width(modal_role, terminal_width);
        let modal_lines = self.render_modal_surface(modal_width.max(1));
        let support_width = terminal_width.saturating_sub(4).clamp(1, 72);
        let mut support_lines = self.render_transient_prompts(support_width);
        let support_role = if support_lines.is_empty() {
            support_lines = if modal_lines.is_empty() {
                self.render_completion_surface(support_width)
            } else {
                Vec::new()
            };
            TransientOverlayRole::ComposerAssistance
        } else {
            TransientOverlayRole::SupportPrompt
        };
        let modal_visible = !modal_lines.is_empty();
        let support_visible = !support_lines.is_empty();
        self.local.modal_overlay.set_lines(modal_lines);
        self.local.support_overlay.set_lines(support_lines);

        let composer_height = self
            .render_editor_box(terminal_width.max(1))
            .len()
            .clamp(1, MAX_COMPOSER_HEIGHT);
        TransientOverlayProjection {
            modal_visible,
            support_visible,
            bottom_margin: composer_height.saturating_add(1),
            support_role,
            modal_role,
        }
    }

    pub(in crate::interactive) fn drain_modal_overlay_input(&mut self) {
        for event in self.local.modal_overlay.take_pending_input() {
            input::handle_root_input(self, &event);
        }
    }

    pub(in crate::interactive) fn take_pending_tool_authorization_decision(
        &mut self,
    ) -> Option<(ToolAuthorizationRequest, ToolAuthorizationDecision)> {
        self.pending_tool_authorization_decision.take()
    }

    pub(in crate::interactive) fn restore_tool_authorization(
        &mut self,
        request: ToolAuthorizationRequest,
    ) {
        if self
            .tool_authorizations
            .iter()
            .all(|pending| pending.authorization_id != request.authorization_id)
        {
            self.tool_authorizations.push_front(request);
        }
        self.tool_authorization_selected = 0;
    }

    pub(in crate::interactive) fn take_selected_model(
        &mut self,
    ) -> Option<CodingAgentModelCatalogEntry> {
        self.local.selected_model.take()
    }

    pub(in crate::interactive) fn take_selected_thinking_level(
        &mut self,
    ) -> Option<CodingAgentThinkingLevel> {
        self.local.selected_thinking_level.take()
    }

    /// Record a permission-mode change for the status bar immediately and mark
    /// it pending so the runtime session can be switched once connected.
    pub(in crate::interactive) fn set_permission_mode(&mut self, mode: ToolAuthorizationMode) {
        self.permission_mode = mode;
        self.local.pending_permission_mode = Some(mode);
    }

    pub(in crate::interactive) fn take_pending_permission_mode(
        &mut self,
    ) -> Option<ToolAuthorizationMode> {
        self.local.pending_permission_mode.take()
    }

    pub(in crate::interactive) fn set_default_agent_profile_id(&mut self, profile_id: ProfileId) {
        self.profile_catalog.sync_default_agent_profile(&profile_id);
        self.default_agent_profile_id = profile_id;
        self.slash_suggestion_selected = 0;
        self.slash_suggestions_dismissed_for = None;
    }

    pub(in crate::interactive) fn display_default_agent_profile_id(&self) -> &ProfileId {
        self.shared_projection
            .session()
            .map(|session| &session.default_agent_profile_id)
            .unwrap_or(&self.default_agent_profile_id)
    }

    pub(in crate::interactive) fn install_shared_snapshot(
        &mut self,
        snapshot: CodingAgentSnapshot,
    ) {
        // Preserve operation timings: a fresh snapshot arrives for every new
        // connection handoff, and rebuilding the projection would reset all
        // op elapsed times to zero.
        self.shared_projection
            .replace_snapshot_retaining_timings(snapshot);
        Self::update_context_local_state(&mut self.local, self.shared_projection.context());
        self.clamp_context_navigation();
    }

    pub(in crate::interactive) fn apply_shared_product_event(&mut self, event: &ProductEvent) {
        self.shared_projection.apply_product_event(event);
        Self::update_context_local_state(&mut self.local, self.shared_projection.context());
        self.clamp_context_navigation();
    }

    pub(in crate::interactive) fn drain_shared_ui_events(&mut self) -> Vec<UiEvent> {
        self.shared_projection.drain()
    }

    pub(in crate::interactive) fn apply_shared_child_ui_events(&mut self) {
        for (operation_id, events) in self.shared_projection.drain_children() {
            self.ensure_child_conversation(&operation_id);
            if self.active_child_operation_id.as_deref() == Some(operation_id.as_str()) {
                apply_child_events(&mut self.transcript, &mut self.tool_authorizations, events);
            } else {
                self.child_conversations
                    .entry(operation_id)
                    .or_insert_with(ChildConversationState::new)
                    .apply_events(events);
            }
        }
    }

    pub(super) fn ensure_child_conversation(&mut self, operation_id: &str) {
        if self.child_conversations.contains_key(operation_id) {
            return;
        }
        while self.child_conversation_order.len() >= MAX_CHILD_CONVERSATIONS {
            let evict = self.child_conversation_order.iter().position(|candidate| {
                self.active_child_operation_id.as_deref() != Some(candidate.as_str())
            });
            let Some(index) = evict else {
                break;
            };
            if let Some(evicted) = self.child_conversation_order.remove(index) {
                self.child_conversations.remove(&evicted);
            }
        }
        self.child_conversation_order
            .push_back(operation_id.to_owned());
        self.child_conversations
            .insert(operation_id.to_owned(), ChildConversationState::new());
    }

    pub(in crate::interactive) fn apply_root_events(&mut self, events: Vec<UiEvent>) {
        let Some(mut main_transcript) = self.main_transcript.take() else {
            self.apply_events(events);
            return;
        };
        let mut main_authorizations = self.main_tool_authorizations.take().unwrap_or_default();
        std::mem::swap(&mut self.transcript, &mut main_transcript);
        std::mem::swap(&mut self.tool_authorizations, &mut main_authorizations);
        self.apply_events(events);
        std::mem::swap(&mut self.tool_authorizations, &mut main_authorizations);
        std::mem::swap(&mut self.transcript, &mut main_transcript);
        self.main_transcript = Some(main_transcript);
        self.main_tool_authorizations = Some(main_authorizations);
    }

    pub(super) fn open_selected_child_conversation(&mut self) -> bool {
        if self.active_child_operation_id.is_some() {
            return false;
        }
        let operation_id = self
            .local
            .transcript_view
            .selected()
            .and_then(|block_id| self.transcript.item_for_block(block_id))
            .and_then(|item| match item {
                TranscriptItem::Tool { name, args, .. } if name == "delegation" => args
                    .get("childOperationId")
                    .and_then(|value| value.as_str())
                    .map(ToOwned::to_owned),
                _ => None,
            });
        let Some(operation_id) = operation_id else {
            return false;
        };
        let Some(conversation) = self.child_conversations.get_mut(&operation_id) else {
            return false;
        };
        let child_transcript = std::mem::replace(&mut conversation.transcript, Transcript::new());
        let main_transcript = std::mem::replace(&mut self.transcript, child_transcript);
        let child_authorizations = std::mem::take(&mut conversation.authorizations);
        let main_authorizations =
            std::mem::replace(&mut self.tool_authorizations, child_authorizations);
        self.main_transcript = Some(main_transcript);
        self.main_tool_authorizations = Some(main_authorizations);
        self.active_child_operation_id = Some(operation_id);
        self.refresh_shell_focus();
        self.local.transcript_view = TranscriptViewState::default();
        self.local.render_cache.clear();
        true
    }

    pub(super) fn close_child_conversation(&mut self) -> bool {
        let Some(operation_id) = self.active_child_operation_id.take() else {
            return false;
        };
        let Some(main_transcript) = self.main_transcript.take() else {
            return false;
        };
        let main_authorizations = self.main_tool_authorizations.take().unwrap_or_default();
        let child_transcript = std::mem::replace(&mut self.transcript, main_transcript);
        let child_authorizations =
            std::mem::replace(&mut self.tool_authorizations, main_authorizations);
        let conversation = self
            .child_conversations
            .entry(operation_id)
            .or_insert_with(ChildConversationState::new);
        conversation.transcript = child_transcript;
        conversation.authorizations = child_authorizations;
        self.refresh_shell_focus();
        self.local.transcript_view = TranscriptViewState::default();
        self.local.render_cache.clear();
        true
    }

    pub(super) fn update_context_local_state(
        local: &mut InteractiveLocalState,
        projection: &CodingAgentContextSnapshot,
    ) {
        let now = Instant::now();
        for change in &projection.changes {
            let timing = local
                .context_change_timing
                .entry(change.path.clone())
                .or_insert((change.updated_sequence, now));
            if timing.0 != change.updated_sequence {
                *timing = (change.updated_sequence, now);
            }
        }
        local
            .context_change_timing
            .retain(|path, _| projection.changes.iter().any(|change| change.path == *path));
    }

    pub(super) fn clamp_context_navigation(&mut self) {
        for tab in ContextTab::ALL {
            let index = tab.index();
            let count = if tab == ContextTab::Usage {
                self.context_usage_lines(self.viewport_width).len()
            } else {
                self.context_items(tab).len()
            };
            self.local.context_selection[index] =
                self.local.context_selection[index].min(count.saturating_sub(1));
            self.local.context_scroll[index] = self.local.context_scroll[index]
                .min(count.saturating_sub(self.local.context_viewport_height.max(1)));
        }
    }

    pub(super) fn move_context_selection(&mut self, direction: isize) {
        let items = self.context_items(self.local.context_tab);
        if items.is_empty() {
            return;
        }
        let index = self.local.context_tab.index();
        self.local.context_selection[index] = if direction < 0 {
            self.local.context_selection[index].saturating_sub(1)
        } else {
            self.local.context_selection[index]
                .saturating_add(1)
                .min(items.len() - 1)
        };
        self.ensure_context_selection_visible();
    }

    pub(super) fn ensure_context_selection_visible(&mut self) {
        let index = self.local.context_tab.index();
        let selected = self.local.context_selection[index];
        let viewport = self.local.context_viewport_height.max(1);
        if selected < self.local.context_scroll[index] {
            self.local.context_scroll[index] = selected;
        } else if selected >= self.local.context_scroll[index].saturating_add(viewport) {
            self.local.context_scroll[index] = selected.saturating_add(1).saturating_sub(viewport);
        }
    }

    pub(super) fn scroll_context(&mut self, rows: isize) {
        let index = self.local.context_tab.index();
        let count = if self.local.context_tab == ContextTab::Usage {
            self.context_usage_lines(self.viewport_width).len()
        } else {
            self.context_items(self.local.context_tab).len()
        };
        let maximum = count.saturating_sub(self.local.context_viewport_height.max(1));
        self.local.context_scroll[index] = if rows < 0 {
            self.local.context_scroll[index].saturating_sub(rows.unsigned_abs())
        } else {
            self.local.context_scroll[index]
                .saturating_add(rows as usize)
                .min(maximum)
        };
    }

    pub(super) fn open_selected_context_detail(&mut self) -> bool {
        let items = self.context_items(self.local.context_tab);
        let Some(item) = items.get(self.local.context_selection[self.local.context_tab.index()])
        else {
            return false;
        };
        self.local.context_detail = Some(ContextDetail {
            title: item.detail_title.clone(),
            lines: item.detail_lines.clone(),
            scroll: 0,
        });
        true
    }

    pub(in crate::interactive) fn has_context_detail(&self) -> bool {
        self.local.context_detail.is_some()
    }

    pub(in crate::interactive) fn handle_context_detail_input(
        &mut self,
        event: &InputEvent,
    ) -> bool {
        let Some(detail) = self.local.context_detail.as_mut() else {
            return false;
        };
        if matches_key(event, "escape")
            || matches_key(event, "enter")
            || matches_key(event, "ctrl+c")
        {
            self.local.context_detail = None;
            return true;
        }
        if matches_key(event, "pageup") || matches_key(event, "up") {
            detail.scroll = detail
                .scroll
                .saturating_sub(if matches_key(event, "pageup") { 8 } else { 1 });
            return true;
        }
        if matches_key(event, "pagedown") || matches_key(event, "down") {
            detail.scroll = detail
                .scroll
                .saturating_add(if matches_key(event, "pagedown") { 8 } else { 1 });
            return true;
        }
        true
    }

    pub(in crate::interactive) fn take_selected_session(&mut self) -> Option<SessionChoice> {
        self.local.selected_session.take()
    }

    pub(in crate::interactive) fn take_selected_session_hydrate(&mut self) -> bool {
        std::mem::take(&mut self.local.selected_session_hydrate)
    }

    pub(in crate::interactive) fn take_selected_tree_entry_id(&mut self) -> Option<String> {
        self.local.selected_tree_entry_id.take()
    }

    pub(in crate::interactive) fn take_pending_tree_label_change(
        &mut self,
    ) -> Option<(String, Option<String>)> {
        self.local.pending_tree_label_change.take()
    }

    pub(in crate::interactive) fn apply_tree_label_update(
        &mut self,
        entry_id: &str,
        label: Option<String>,
        updated_at: String,
    ) {
        if let Some(selector) = self.local.tree_selector.as_mut() {
            let timestamp = label.as_ref().map(|_| updated_at);
            selector.update_node_label(entry_id, label, timestamp);
        }
    }

    pub(in crate::interactive) fn take_settings_command(
        &mut self,
    ) -> Option<CodingAgentSettingsCommand> {
        self.local.settings_command.take()
    }

    pub(in crate::interactive) fn take_auth_command(&mut self) -> Option<CodingAgentAuthCommand> {
        self.local.auth_command.take()
    }

    pub(in crate::interactive) fn take_submitted(&mut self) -> Option<String> {
        self.local.submitted.lock().unwrap().take()
    }

    pub(in crate::interactive) fn queue_command(&mut self, command: PendingInteractiveCommand) {
        self.action = command.action();
        self.pending_command = Some(command);
    }

    pub(in crate::interactive) fn take_pending_command(
        &mut self,
    ) -> Option<PendingInteractiveCommand> {
        self.pending_command.take()
    }

    pub(in crate::interactive) fn take_pending_delegation_confirmation_command(
        &mut self,
    ) -> Option<PendingDelegationConfirmationCommand> {
        self.pending_delegation_confirmation_command.take()
    }

    pub(in crate::interactive) fn take_scroll_command(
        &mut self,
    ) -> Option<TranscriptScrollCommand> {
        self.local.scroll_command.lock().unwrap().take()
    }
}
