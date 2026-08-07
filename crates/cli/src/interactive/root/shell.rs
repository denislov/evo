use super::*;

impl InteractiveRoot {
    pub(super) fn render_fullscreen_shell(&mut self, width: usize) -> Vec<String> {
        let editor_lines = self.render_editor_box(width);
        let composer_height = editor_lines.len().clamp(1, MAX_COMPOSER_HEIGHT);
        let layout = self.shell_layout(composer_height);
        let mut frame = Frame::new(self.viewport_width, self.viewport_height);

        let conversation_body = panel_body(layout.conversation);
        self.conversation_viewport_width = conversation_body.width.max(1);
        self.conversation_viewport_height = conversation_body.height.max(1);
        let max_tool_result_lines = MAX_TOOL_RESULT_LINES;
        self.sync_transcript_view();
        let opts =
            self.transcript_render_options(conversation_body.width.max(1), max_tool_result_lines);
        let transcript_viewport = self.local.render_cache.render_viewport(
            &self.transcript,
            &opts,
            conversation_body.height,
            self.transcript.scroll_offset(),
        );
        self.rebuild_mouse_hit_regions(
            layout,
            conversation_body,
            transcript_viewport.total_rows,
            &transcript_viewport.block_rows,
        );
        frame.draw(
            Rect::new(
                layout.conversation.x,
                layout.conversation.y,
                layout.conversation.width,
                1.min(layout.conversation.height),
            ),
            &[self.panel_header(
                &self.conversation_header_title(layout.conversation.width),
                InteractiveRegion::Conversation,
                layout.conversation.width,
            )],
        );
        frame.draw(conversation_body, &transcript_viewport.lines);

        let divider_style = self.semantic_style(CodingAgentThemeForeground::BorderMuted, SYSTEM);
        if let Some(separator) = layout.conversation_context_divider {
            let line = paint_with("│", &divider_style, color_enabled());
            frame.draw(separator, &vec![line; separator.height]);
        }
        if let Some(separator) = layout.context_drawer_divider {
            let line = paint_with("│", &divider_style, color_enabled());
            frame.draw(separator, &vec![line; separator.height]);
        }
        if let Some(context) = layout.context {
            let context_lines = self.render_context_region(context.width, context.height);
            if layout.mode != ShellLayoutMode::Wide {
                frame.fill(context, "");
            }
            frame.draw(context, &context_lines);
        }
        if let Some(tips) = layout.tips {
            frame.draw(tips, &self.render_tips_region(tips.width, tips.height));
        }
        if let Some(divider) = layout.context_tips_divider {
            frame.draw(
                divider,
                &[paint_with(
                    &fit_line("─ Tips ", divider.width),
                    &divider_style,
                    color_enabled(),
                )],
            );
            if let Some(vertical) = layout.conversation_context_divider {
                frame.draw(
                    Rect::new(vertical.x, divider.y, vertical.width, 1),
                    &[paint_with("├", &divider_style, color_enabled())],
                );
            }
        }

        if !layout.composer.is_empty() {
            let composer_lines = tail_lines(&editor_lines, layout.composer.height);
            frame.draw(layout.composer, &composer_lines);
        }
        if !layout.status.is_empty() {
            frame.draw(
                layout.status,
                &[self.render_status_bar(layout.status.width)],
            );
        }

        frame.into_lines()
    }

    pub(super) fn panel_header(
        &self,
        title: &str,
        region: InteractiveRegion,
        width: usize,
    ) -> String {
        let focused = self.local.focus_ring.current() == Some(region);
        let prefix = if focused { "▌ " } else { "  " };
        let fallback = if focused { USER } else { SYSTEM };
        let token = if focused {
            CodingAgentThemeForeground::BorderAccent
        } else {
            CodingAgentThemeForeground::BorderMuted
        };
        let style = self.semantic_style(token, fallback);
        fit_line(
            &paint_with(&format!("{prefix}{title}"), &style, color_enabled()),
            width,
        )
    }

    pub(super) fn semantic_style(
        &self,
        token: CodingAgentThemeForeground,
        fallback: Style,
    ) -> Style {
        self.resolved_theme.as_ref().map_or(fallback, |resolved| {
            Style::fg(crate::interactive::theme::to_color(
                resolved.foreground(token),
            ))
        })
    }

    pub(super) fn conversation_header_title(&self, width: usize) -> String {
        let base = if let Some(operation_id) = self.active_child_operation_id.as_deref() {
            let short = short_id(operation_id);
            let delegation = self
                .shared_projection
                .context()
                .delegations
                .iter()
                .find(|delegation| delegation.child_operation_id.as_deref() == Some(operation_id));
            let wide = delegation.map_or_else(
                || format!("Child · {short} · Esc back"),
                |delegation| {
                    format!(
                        "Child · {} · {} · {short} · Esc back",
                        delegation.target_id, delegation.status
                    )
                },
            );
            if visible_width(&wide).saturating_add(2) <= width {
                wide
            } else {
                let compact = format!("Child · {short} · Esc back");
                if visible_width(&compact).saturating_add(2) <= width {
                    compact
                } else {
                    format!("Child · {short} · Esc")
                }
            }
        } else {
            "Conversation".into()
        };

        let (scroll_status, compact_scroll_status) = if self.transcript.has_new_output_below() {
            (
                "↓ new output below · End latest".into(),
                "↓ new · End".into(),
            )
        } else if self.transcript.scroll_offset() > 0 {
            (
                format!("↑ {} rows · End latest", self.transcript.scroll_offset()),
                format!("↑{} · End", self.transcript.scroll_offset()),
            )
        } else {
            return base;
        };
        for status in [scroll_status, compact_scroll_status] {
            let candidate = format!("{base} · {status}");
            if visible_width(&candidate).saturating_add(2) <= width {
                return candidate;
            }
        }
        base
    }

    pub(super) fn render_tips_region(&self, width: usize, height: usize) -> Vec<String> {
        let key = |id: &str| {
            self.local
                .keybindings
                .get_keys(id)
                .into_iter()
                .next()
                .unwrap_or_else(|| "?".into())
        };
        let mut tips: Vec<(u8, usize, String)> = Vec::new();
        let mut order = 0;
        let mut push = |priority: u8, text: String| {
            tips.push((priority, order, text));
            order += 1;
        };

        if self.active_child_operation_id.is_some() {
            push(0, format!("{}  back", key("tui.select.cancel")));
        } else if !self.tool_authorizations.is_empty() {
            push(0, format!("{}  choose", key("tui.select.confirm")));
            push(0, format!("{}  deny", key("tui.select.cancel")));
        } else if self.local.selecting_settings
            || self.local.selecting_model
            || self.local.selecting_session
            || self.local.selecting_tree
            || self.delegation_confirmation_menu.is_some()
            || self.profile_menu.is_some()
        {
            push(0, format!("{}  close", key("tui.select.cancel")));
            push(
                1,
                format!(
                    "{} / {}  select",
                    key("tui.select.up"),
                    key("tui.select.down")
                ),
            );
        }
        match self.local.focus_ring.current() {
            Some(InteractiveRegion::Conversation) => {
                push(
                    1,
                    format!(
                        "{} / {}  select",
                        key("tui.select.up"),
                        key("tui.select.down")
                    ),
                );
                if self
                    .local
                    .transcript_view
                    .selected()
                    .and_then(|block_id| self.transcript.item_for_block(block_id))
                    .is_some_and(TranscriptItem::foldable)
                {
                    push(0, format!("{}  disclose", key("tui.select.confirm")));
                }
                if self
                    .local
                    .transcript_view
                    .selected_has_tool_arguments(&self.transcript)
                {
                    push(1, format!("{}  arguments", key("app.transcript.arguments")));
                }
            }
            Some(InteractiveRegion::Context) => {
                push(
                    1,
                    format!(
                        "{} / {}  tabs",
                        key("app.context.previousTab"),
                        key("app.context.nextTab")
                    ),
                );
                push(
                    1,
                    format!(
                        "{} / {}  {}",
                        key("tui.select.up"),
                        key("tui.select.down"),
                        if self.local.context_tab == ContextTab::Usage {
                            "scroll"
                        } else {
                            "select"
                        }
                    ),
                );
                if self.local.context_tab != ContextTab::Usage
                    && !self.context_items(self.local.context_tab).is_empty()
                {
                    push(0, format!("{}  detail", key("tui.select.confirm")));
                }
                if self
                    .shared_projection
                    .capabilities()
                    .is_some_and(|capabilities| {
                        matches!(capabilities.abort, CapabilityStatus::Available)
                    })
                {
                    push(0, format!("{}  cancel", key("app.interrupt")));
                }
            }
            Some(InteractiveRegion::Composer) => {
                push(0, format!("{}  submit", key("tui.input.submit")));
            }
            None => {}
        }
        push(8, format!("{}  context", key("app.context.toggle")));
        push(
            9,
            format!(
                "{} / {}  focus",
                key("app.focus.next"),
                key("app.focus.previous")
            ),
        );
        tips.sort_by_key(|(priority, insertion, _)| (*priority, *insertion));
        let mut lines = tips
            .into_iter()
            .map(|(priority, _, tip)| {
                let fallback = if priority <= 1 { USER } else { SYSTEM };
                let token = if priority <= 1 {
                    CodingAgentThemeForeground::Accent
                } else {
                    CodingAgentThemeForeground::Muted
                };
                let style = self.semantic_style(token, fallback);
                fit_line(&paint_with(&tip, &style, color_enabled()), width)
            })
            .collect::<Vec<_>>();
        lines.truncate(height);
        lines
    }

    /// The currently active model for display (context window, reasoning,
    /// provider). Distinct from `selected_model`, which is consumed by
    /// `take_selected_model` to apply a pending change to the agent.
    pub(super) fn current_model(&self) -> Option<&CodingAgentModelCatalogEntry> {
        self.model.as_ref()
    }

    pub(super) fn render_status_bar(&self, width: usize) -> String {
        let active_kind = self
            .shared_projection
            .context()
            .operations
            .iter()
            .find(|operation| operation_status_is_running(operation.status))
            .map(|operation| operation.kind.as_str());
        let (state, state_token, state_fallback) = match self.status {
            InteractiveStatus::Idle => (
                "● idle".to_string(),
                CodingAgentThemeForeground::Success,
                STATUS_IDLE,
            ),
            InteractiveStatus::Running => active_kind.map_or_else(
                || {
                    (
                        running_status_text(self.spinner_frame),
                        CodingAgentThemeForeground::Accent,
                        STATUS_RUNNING,
                    )
                },
                |kind| {
                    (
                        format!("{} {kind}", running_status_text(self.spinner_frame)),
                        CodingAgentThemeForeground::Accent,
                        STATUS_RUNNING,
                    )
                },
            ),
        };
        let state = paint_with(
            &state,
            &self.semantic_style(state_token, state_fallback),
            color_enabled(),
        );
        let mut segments = vec![state];
        let permission_token = match self.permission_mode {
            ToolAuthorizationMode::Plan => CodingAgentThemeForeground::Warning,
            ToolAuthorizationMode::Ask => CodingAgentThemeForeground::Accent,
            ToolAuthorizationMode::Yolo => CodingAgentThemeForeground::Error,
        };
        segments.push(paint_with(
            &format!("{}", self.permission_mode).to_uppercase(),
            &self.semantic_style(permission_token, SYSTEM),
            color_enabled(),
        ));
        let context_usage = self
            .shared_projection
            .context()
            .usage
            .latest_turn
            .as_ref()
            .and_then(|turn| turn.context_tokens)
            .zip(self.shared_projection.context().usage.context_window)
            .map(|(tokens, window)| {
                if window == 0 {
                    return paint_with(
                        &format!("ctx unavailable ({})", format_tokens(tokens)),
                        &self.semantic_style(CodingAgentThemeForeground::Muted, SYSTEM),
                        color_enabled(),
                    );
                }
                let bar_width = if width >= WIDE_LAYOUT_MIN_WIDTH {
                    7
                } else if width >= MEDIUM_LAYOUT_MIN_WIDTH {
                    4
                } else {
                    0
                };
                let text = if bar_width == 0 {
                    format!("ctx {}%", context_percentage(tokens, window))
                } else {
                    format!(
                        "ctx {}",
                        context_gauge(tokens, window, bar_width, !color_enabled())
                    )
                };
                let percent = context_percentage(tokens, window);
                let token = if percent > 90 {
                    CodingAgentThemeForeground::Error
                } else if percent > 70 {
                    CodingAgentThemeForeground::Warning
                } else {
                    CodingAgentThemeForeground::Accent
                };
                paint_with(&text, &self.semantic_style(token, SYSTEM), color_enabled())
            });
        let cost = self.shared_projection.context().usage.cost.map(|cost| {
            paint_with(
                &format!("${cost:.4}"),
                &self.semantic_style(CodingAgentThemeForeground::Muted, SYSTEM),
                color_enabled(),
            )
        });
        if context_usage.is_some() || cost.is_some() {
            segments.push(
                [context_usage, cost]
                    .into_iter()
                    .flatten()
                    .collect::<Vec<_>>()
                    .join(" "),
            );
        }
        segments.push(paint_with(
            self.display_default_agent_profile_id().as_str(),
            &self.semantic_style(CodingAgentThemeForeground::Text, Style::default()),
            color_enabled(),
        ));
        segments.push(paint_with(
            &format!(
                "{} · {}",
                self.current_model()
                    .map(|model| model.id.as_str())
                    .unwrap_or("no-model"),
                self.thinking_level
            ),
            &self.semantic_style(CodingAgentThemeForeground::Text, Style::default()),
            color_enabled(),
        ));
        segments.push(paint_with(
            &abbreviate_cwd(&self.cwd),
            &self.semantic_style(CodingAgentThemeForeground::Muted, SYSTEM),
            color_enabled(),
        ));

        let mut rendered = format!(" {}", segments[0]);
        for segment in segments.into_iter().skip(1) {
            let candidate = format!("{rendered}   {segment}");
            if visible_width(&candidate) > width {
                break;
            }
            rendered = candidate;
        }
        fit_line(&rendered, width)
    }

    pub(super) fn render_transient_prompts(&self, width: usize) -> Vec<String> {
        let mut lines = self.render_pending_delegation_rejection_reason(width);
        lines.extend(self.render_pending_profile_task(width));
        lines
    }

    pub(super) fn render_modal_surface(&mut self, width: usize) -> Vec<String> {
        let content_width = width.saturating_sub(3).max(1);
        if self.local.selecting_tree {
            if let Some(ref selector) = self.local.tree_selector {
                return self.framed_modal(selector.render(content_width), width);
            }
            return Vec::new();
        }
        if !self.tool_authorizations.is_empty() {
            return self.render_tool_authorization(width);
        }
        if self.delegation_confirmation_menu.is_some() {
            let lines = self.render_delegation_confirmation_menu(content_width);
            return self.framed_modal(lines, width);
        }
        if self.profile_menu.is_some() {
            let lines = self.render_profile_menu(content_width);
            return self.framed_modal(lines, width);
        }
        if self.local.selecting_model {
            let lines = self.render_model_selector(content_width);
            return self.framed_modal(lines, width);
        }
        if self.local.selecting_session {
            let lines = self.render_session_selector(content_width);
            return self.framed_modal(lines, width);
        }
        if self.local.selecting_settings {
            let lines = self.render_settings_menu(content_width);
            return self.framed_modal(lines, width);
        }
        if self.local.context_detail.is_some() {
            let lines = self.render_context_detail(content_width);
            return self.framed_modal(lines, width);
        }
        Vec::new()
    }

    pub(super) fn framed_modal(&self, lines: Vec<String>, width: usize) -> Vec<String> {
        let border_style = self.semantic_style(CodingAgentThemeForeground::Border, SYSTEM);
        framed_modal_lines(lines, width, &border_style, color_enabled())
    }

    pub(super) fn render_context_detail(&mut self, width: usize) -> Vec<String> {
        let Some(detail) = self.local.context_detail.as_mut() else {
            return Vec::new();
        };
        let viewport = self.viewport_height.saturating_sub(8).clamp(3, 20);
        detail.scroll = detail
            .scroll
            .min(detail.lines.len().saturating_sub(viewport));
        let mut lines = vec![fit_line(&detail.title, width)];
        lines.extend(
            detail
                .lines
                .iter()
                .skip(detail.scroll)
                .take(viewport)
                .map(|line| fit_line(line, width)),
        );
        lines.push(fit_line("Up/Down scroll · Enter/Esc close", width));
        lines
    }

    pub(super) fn render_completion_surface(&mut self, width: usize) -> Vec<String> {
        let slash = self.render_slash_suggestions(width);
        if slash.is_empty() {
            self.local.editor.render_assistance(width)
        } else {
            slash
        }
    }
}
