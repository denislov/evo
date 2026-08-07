use super::*;

impl InteractiveRoot {
    pub(in crate::interactive) fn editor_border_style(&self) -> Style {
        if !self.local.editor.focused() {
            self.resolved_theme
                .as_ref()
                .map_or(self.theme.editor.border, |resolved| {
                    Style::fg(crate::interactive::theme::to_color(
                        resolved.foreground(CodingAgentThemeForeground::BorderMuted),
                    ))
                })
        } else if self.local.selecting_model
            || self.local.selecting_settings
            || self.local.selecting_session
            || self.delegation_confirmation_menu.is_some()
            || !self.tool_authorizations.is_empty()
            || self.pending_delegation_rejection_reason.is_some()
            || self.profile_menu.is_some()
            || self.pending_profile_task.is_some()
        {
            self.theme.editor.menu_border
        } else if let Some(resolved) = &self.resolved_theme {
            // Editor border reflects the active thinking level, mirroring TS
            // `getThinkingBorderColor`. Bash-mode border (TS
            // `getBashModeBorderColor`) is not yet wired: Rust has no
            // bash-mode input state.
            Style::fg(crate::interactive::theme::to_color(
                resolved.foreground(Self::thinking_border_token(self.thinking_level)),
            ))
        } else {
            self.theme.editor.active_border
        }
    }

    /// Map a thinking level to its border color token, mirroring TS
    /// `getThinkingBorderColor`.
    pub(super) fn thinking_border_token(
        level: CodingAgentThinkingLevel,
    ) -> CodingAgentThemeForeground {
        match level {
            CodingAgentThinkingLevel::Off => CodingAgentThemeForeground::ThinkingOff,
            CodingAgentThinkingLevel::Minimal => CodingAgentThemeForeground::ThinkingMinimal,
            CodingAgentThinkingLevel::Low => CodingAgentThemeForeground::ThinkingLow,
            CodingAgentThinkingLevel::Medium => CodingAgentThemeForeground::ThinkingMedium,
            CodingAgentThinkingLevel::High => CodingAgentThemeForeground::ThinkingHigh,
            CodingAgentThinkingLevel::XHigh => CodingAgentThemeForeground::ThinkingXhigh,
        }
    }

    pub(super) fn render_slash_suggestions(&mut self, width: usize) -> Vec<String> {
        if (shell_layout_mode(self.viewport_width) == ShellLayoutMode::Narrow
            && self.local.context_open)
            || self.local.selecting_model
            || self.local.selecting_settings
            || self.local.selecting_session
            || self.delegation_confirmation_menu.is_some()
            || self.profile_menu.is_some()
            || self.pending_profile_task.is_some()
        {
            return Vec::new();
        }

        let commands = self.all_slash_commands();
        slash::render_suggestions(
            self.local.editor.text(),
            self.local.editor.cursor(),
            self.slash_suggestions_dismissed_for.as_deref(),
            &mut self.slash_suggestion_selected,
            width,
            &commands,
            &self.theme.select_list,
        )
    }

    pub(super) fn render_settings_menu(&mut self, width: usize) -> Vec<String> {
        if !self.local.selecting_settings {
            return Vec::new();
        }
        let mut lines = vec![fit_line("Settings", width)];
        lines.extend(self.local.settings_list.render(width));
        lines
    }

    pub(super) fn apply_settings_value(&mut self, id: &str, value: &str) {
        let command = match id {
            "theme" => {
                self.settings.presentation.theme = Some(value.to_string());
                self.apply_builtin_theme(value);
                CodingAgentSettingsCommand::set_theme(value)
            }
            "auto_compaction" => {
                let enabled = value == "on";
                self.settings.runtime.auto_compaction = enabled;
                CodingAgentSettingsCommand::SetAutoCompaction(enabled)
            }
            "steering_mode" => {
                let mode = if value == "all" {
                    CodingAgentQueueMode::All
                } else {
                    CodingAgentQueueMode::OneAtATime
                };
                self.settings.runtime.steering_mode = mode;
                CodingAgentSettingsCommand::SetSteeringMode(mode)
            }
            "follow_up_mode" => {
                let mode = if value == "all" {
                    CodingAgentQueueMode::All
                } else {
                    CodingAgentQueueMode::OneAtATime
                };
                self.settings.runtime.follow_up_mode = mode;
                CodingAgentSettingsCommand::SetFollowUpMode(mode)
            }
            "show_progress" => {
                let visible = value == "on";
                self.settings.presentation.show_progress = visible;
                CodingAgentSettingsCommand::SetProgressVisibility(visible)
            }
            "auto_resize_images" => {
                let enabled = value == "on";
                self.settings.runtime.auto_resize_images = enabled;
                CodingAgentSettingsCommand::SetImageAutoResize(enabled)
            }
            "block_images" => {
                let enabled = value == "on";
                self.settings.runtime.block_images = enabled;
                CodingAgentSettingsCommand::SetImageBlocking(enabled)
            }
            "enable_skill_commands" => {
                let enabled = value == "on";
                self.settings.runtime.enable_skill_commands = enabled;
                CodingAgentSettingsCommand::SetSkillCommands(enabled)
            }
            "hide_thinking_block" => {
                let hidden = value == "on";
                self.settings.presentation.hide_thinking_block = hidden;
                CodingAgentSettingsCommand::SetThinkingVisibility(!hidden)
            }
            "quiet_startup" => {
                let quiet = value == "on";
                self.settings.presentation.quiet_startup = quiet;
                CodingAgentSettingsCommand::SetQuietStartup(quiet)
            }
            "clear_on_shrink" => {
                let enabled = value == "on";
                self.settings.presentation.clear_on_shrink = enabled;
                CodingAgentSettingsCommand::SetClearOnShrink(enabled)
            }
            "double_escape_action" => {
                let action = match value {
                    "fork" => CodingAgentDoubleEscapeAction::Fork,
                    "none" => CodingAgentDoubleEscapeAction::None,
                    _ => CodingAgentDoubleEscapeAction::Tree,
                };
                self.settings.presentation.double_escape_action = action;
                CodingAgentSettingsCommand::SetDoubleEscapeAction(action)
            }
            "default_thinking_level" => {
                let Ok(level) = value.parse::<CodingAgentThinkingLevel>() else {
                    return;
                };
                self.settings.runtime.default_thinking_level = Some(level);
                self.thinking_level = level;
                self.local.selected_thinking_level = Some(level);
                CodingAgentSettingsCommand::SetDefaultThinkingLevel(level)
            }
            "http_idle_timeout" => {
                let Some((_, timeout_ms)) = HTTP_IDLE_TIMEOUT_CHOICES
                    .iter()
                    .find(|(label, _)| *label == value)
                else {
                    return;
                };
                self.settings.runtime.http_idle_timeout_ms = *timeout_ms;
                CodingAgentSettingsCommand::SetHttpIdleTimeoutMs(*timeout_ms)
            }
            _ => return,
        };
        self.local.settings_command = Some(command);
    }

    /// Apply a built-in theme by name ("dark"/"light").
    pub(super) fn apply_builtin_theme(&mut self, name: &str) {
        let snapshot = match name {
            "light" => CodingAgentThemeSnapshot::light(),
            _ => CodingAgentThemeSnapshot::dark(),
        };
        self.apply_theme_snapshot(snapshot);
    }

    /// Install a fully resolved product theme projection.
    pub(in crate::interactive) fn apply_theme_snapshot(
        &mut self,
        snapshot: CodingAgentThemeSnapshot,
    ) {
        self.theme = crate::interactive::theme::tui_theme_from_snapshot(&snapshot);
        self.resolved_theme = Some(snapshot);
        self.local.render_cache.clear();
    }

    /// Build a `MarkdownTheme` for the active resolved theme, wiring the
    /// syntax-highlight callback (TS `getMarkdownTheme` + `highlightCode`).
    /// Falls back to the palette theme's markdown styles when no resolved
    /// theme is set.
    pub(super) fn markdown_theme(&self) -> MarkdownTheme {
        let mut md = match &self.resolved_theme {
            Some(resolved) => markdown_theme_from_resolved(resolved),
            None => self.theme.markdown.clone(),
        };
        if let Some(resolved) = &self.resolved_theme {
            let resolved = resolved.clone();
            md.highlight_code = Some(std::sync::Arc::new(
                move |code: &str, lang: Option<&str>| {
                    crate::interactive::syntax::highlight_code(code, lang, &resolved)
                },
            ));
        }
        md
    }

    /// Build the [`TranscriptRenderOptions`] used by transcript block
    /// rendering. Resolves styles from the active [`ResolvedTheme`] when
    /// available, falling back to the built-in palette otherwise.
    pub(super) fn transcript_render_options(
        &self,
        width: usize,
        max_tool_result_lines: usize,
    ) -> TranscriptRenderOptions<'static> {
        TranscriptRenderOptions {
            width,
            max_tool_result_lines,
            color: color_enabled(),
            markdown_theme: self.markdown_theme(),
            hide_thinking_block: self.settings.presentation.hide_thinking_block,
            hidden_thinking_label: "Thinking...",
            styles: TranscriptStyles::from_theme(self.resolved_theme.as_ref()),
            view: Some(self.local.transcript_view.snapshot()),
            selected_block: (self.local.focus_ring.current()
                == Some(InteractiveRegion::Conversation))
            .then(|| self.local.transcript_view.selected())
            .flatten(),
            selection_gutter: true,
            show_images: self.settings.presentation.show_images,
            image_width_cells: self.settings.presentation.image_width_cells,
            terminal_capabilities: self.terminal_capabilities,
        }
    }

    pub(in crate::interactive) fn handle_settings_input(&mut self, event: &InputEvent) -> bool {
        if !self.local.selecting_settings {
            return false;
        }

        let before = self
            .local
            .settings_list
            .selected_item()
            .map(|item| (item.id.clone(), item.current_value.clone()));
        self.local.settings_list.handle_input(event);
        let after = self
            .local
            .settings_list
            .selected_item()
            .map(|item| (item.id.clone(), item.current_value.clone()));

        if let (Some((before_id, before_value)), Some((after_id, after_value))) = (before, after)
            && before_id == after_id
            && before_value != after_value
        {
            self.apply_settings_value(&after_id, &after_value);
        }
        true
    }

    pub(in crate::interactive) fn queue_auth_command(&mut self, command: CodingAgentAuthCommand) {
        self.local.auth_command = Some(command);
    }

    pub(super) fn render_model_selector(&mut self, width: usize) -> Vec<String> {
        if !self.local.selecting_model {
            return Vec::new();
        }
        model_selector::render(
            &self.available_models,
            self.local.editor.text(),
            &mut self.local.model_selection_selected,
            width,
        )
    }

    pub(super) fn render_session_selector(&mut self, width: usize) -> Vec<String> {
        if !self.local.selecting_session {
            return Vec::new();
        }
        session_selector::render(
            &self.session_choices,
            self.local.editor.text(),
            &mut self.local.session_selection_selected,
            width,
        )
    }

    pub(super) fn render_editor_box(&mut self, width: usize) -> Vec<String> {
        let editor_width = width.saturating_sub(2);
        let editor_lines = self.local.editor.render_input(editor_width);
        let border = editor_border_line(width, &self.editor_border_style(), color_enabled());
        let mut lines = Vec::with_capacity(editor_lines.len() + 2);
        lines.push(border.clone());
        for (index, line) in editor_lines.into_iter().enumerate() {
            let prompt = if index == 0 { "> " } else { "  " };
            lines.push(fit_line(&format!("{prompt}{line}"), width));
        }
        lines.push(border);
        lines
    }

    pub(in crate::interactive) fn set_terminal_capabilities(
        &mut self,
        capabilities: TerminalCapabilities,
    ) {
        if self.terminal_capabilities != capabilities {
            self.terminal_capabilities = capabilities;
            self.local.render_cache.clear();
        }
    }

    pub(super) fn sync_transcript_view(&mut self) {
        self.local.transcript_view.sync(&self.transcript);
    }

    pub(in crate::interactive) fn toggle_all_transcript_blocks(&mut self) -> bool {
        self.sync_transcript_view();
        let previous_scroll_offset = self.transcript.scroll_offset();
        let previous_rows = (previous_scroll_offset > 0).then(|| self.transcript_total_rows());
        let changed = self.local.transcript_view.toggle_all(&self.transcript);
        if changed && let Some(previous_rows) = previous_rows {
            let current_rows = self.transcript_total_rows();
            self.transcript.preserve_scrolled_view_after_row_change(
                previous_scroll_offset,
                previous_rows,
                current_rows,
            );
        }
        changed
    }

    pub(in crate::interactive) fn uses_per_block_transcript_view(&self) -> bool {
        true
    }
}
