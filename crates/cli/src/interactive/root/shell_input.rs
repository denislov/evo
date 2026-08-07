use super::*;

impl InteractiveRoot {
    pub(in crate::interactive) fn handle_shell_input(&mut self, event: &InputEvent) -> bool {
        if self.local.selecting_model
            || self.local.selecting_session
            || self.local.selecting_settings
        {
            return false;
        }
        if let InputEvent::Mouse(mouse) = event {
            return self.handle_shell_mouse(*mouse);
        }

        if matches_key(event, "escape") && self.close_child_conversation() {
            return true;
        }

        let mode = shell_layout_mode(self.viewport_width);
        if self.local.context_open && mode != ShellLayoutMode::Wide && matches_key(event, "escape")
        {
            self.close_context_overlay();
            return true;
        }
        if matches_key(event, "escape")
            && self.local.focus_ring.current() != Some(InteractiveRegion::Composer)
            && self.local.focus_ring.focus(InteractiveRegion::Composer)
        {
            self.apply_region_focus();
            return true;
        }
        if self.local.keybindings.matches(event, "app.context.toggle") {
            self.toggle_context(mode);
            return true;
        }

        let editor_accepts_tab = self.local.focus_ring.current()
            == Some(InteractiveRegion::Composer)
            && !self.local.editor.text().is_empty();
        if self.local.keybindings.matches(event, "app.focus.next") && !editor_accepts_tab {
            self.local.focus_ring.focus_next();
            self.apply_region_focus();
            return true;
        }
        if self.local.keybindings.matches(event, "app.focus.previous") {
            self.local.focus_ring.focus_previous();
            self.apply_region_focus();
            return true;
        }

        match self.local.focus_ring.current() {
            Some(InteractiveRegion::Conversation) => {
                self.sync_transcript_view();
                if self.local.keybindings.matches(event, "tui.select.up") || matches_key(event, "k")
                {
                    if self.local.transcript_view.select_previous(&self.transcript) {
                        self.ensure_selected_transcript_visible();
                    }
                    return true;
                }
                if self.local.keybindings.matches(event, "tui.select.down")
                    || matches_key(event, "j")
                {
                    if self.local.transcript_view.select_next(&self.transcript) {
                        self.ensure_selected_transcript_visible();
                    }
                    return true;
                }
                if self.local.keybindings.matches(event, "tui.select.confirm") {
                    if !self.open_selected_child_conversation() {
                        self.toggle_selected_transcript_block();
                    }
                    return true;
                }
                if matches_key(event, "space") || matches_key(event, "ctrl+o") {
                    self.toggle_selected_transcript_block();
                    return true;
                }
                if self
                    .local
                    .keybindings
                    .matches(event, "app.transcript.arguments")
                {
                    self.toggle_selected_transcript_arguments();
                    return true;
                }
                if self.local.keybindings.matches(event, "tui.select.pageUp") {
                    self.transcript
                        .scroll_page_up(self.conversation_viewport_height.max(1));
                    return true;
                }
                if self.local.keybindings.matches(event, "tui.select.pageDown") {
                    self.transcript
                        .scroll_page_down(self.conversation_viewport_height.max(1));
                    return true;
                }
                if matches_key(event, "home") {
                    self.local.transcript_view.select_first(&self.transcript);
                    self.ensure_selected_transcript_visible();
                    return true;
                }
                if matches_key(event, "end") {
                    self.local.transcript_view.select_last(&self.transcript);
                    self.ensure_selected_transcript_visible();
                    return true;
                }
            }
            Some(InteractiveRegion::Context) => {
                if self
                    .local
                    .keybindings
                    .matches(event, "app.context.previousTab")
                {
                    self.local.context_tab = self.local.context_tab.previous();
                    self.clamp_context_navigation();
                    return true;
                }
                if self.local.keybindings.matches(event, "app.context.nextTab") {
                    self.local.context_tab = self.local.context_tab.next();
                    self.clamp_context_navigation();
                    return true;
                }
                if self.local.keybindings.matches(event, "tui.select.up") || matches_key(event, "k")
                {
                    if self.local.context_tab == ContextTab::Usage {
                        self.scroll_context(-1);
                    } else {
                        self.move_context_selection(-1);
                    }
                    return true;
                }
                if self.local.keybindings.matches(event, "tui.select.down")
                    || matches_key(event, "j")
                {
                    if self.local.context_tab == ContextTab::Usage {
                        self.scroll_context(1);
                    } else {
                        self.move_context_selection(1);
                    }
                    return true;
                }
                if self.local.keybindings.matches(event, "tui.select.pageUp") {
                    self.scroll_context(-(self.local.context_viewport_height.max(1) as isize));
                    return true;
                }
                if self.local.keybindings.matches(event, "tui.select.pageDown") {
                    self.scroll_context(self.local.context_viewport_height.max(1) as isize);
                    return true;
                }
                if self.local.keybindings.matches(event, "tui.select.confirm") {
                    self.open_selected_context_detail();
                    return true;
                }
            }
            Some(InteractiveRegion::Composer) | None => return false,
        }

        if matches_key(event, "ctrl+c")
            || matches_key(event, "ctrl+o")
            || self.local.keybindings.matches(event, "app.model.next")
            || self.local.keybindings.matches(event, "app.model.previous")
        {
            return false;
        }
        true
    }

    pub(super) fn handle_shell_mouse(&mut self, event: MouseEvent) -> bool {
        let point = Point::new(event.column, event.row);
        let target = self.local.mouse_hits.hit(point).copied();
        match event.kind {
            MouseEventKind::ScrollUp => {
                if target.is_some_and(InteractiveHitTarget::is_conversation) {
                    self.transcript.scroll_page_up(MOUSE_SCROLL_ROWS);
                } else if matches!(
                    target,
                    Some(
                        InteractiveHitTarget::Context
                            | InteractiveHitTarget::ContextTab(_)
                            | InteractiveHitTarget::ContextRow(_)
                    )
                ) {
                    self.scroll_context(-(MOUSE_SCROLL_ROWS as isize));
                }
            }
            MouseEventKind::ScrollDown => {
                if target.is_some_and(InteractiveHitTarget::is_conversation) {
                    self.transcript.scroll_page_down(MOUSE_SCROLL_ROWS);
                } else if matches!(
                    target,
                    Some(
                        InteractiveHitTarget::Context
                            | InteractiveHitTarget::ContextTab(_)
                            | InteractiveHitTarget::ContextRow(_)
                    )
                ) {
                    self.scroll_context(MOUSE_SCROLL_ROWS as isize);
                }
            }
            MouseEventKind::Down(MouseButton::Left) => match target {
                Some(InteractiveHitTarget::TranscriptDisclosure(block_id)) => {
                    self.focus_shell_region(InteractiveRegion::Conversation);
                    self.select_transcript_block(block_id);
                    self.toggle_selected_transcript_block();
                }
                Some(InteractiveHitTarget::TranscriptBlock(block_id)) => {
                    self.focus_shell_region(InteractiveRegion::Conversation);
                    self.select_transcript_block(block_id);
                }
                Some(InteractiveHitTarget::Conversation) => {
                    self.focus_shell_region(InteractiveRegion::Conversation);
                }
                Some(InteractiveHitTarget::Context) => {
                    self.focus_shell_region(InteractiveRegion::Context);
                }
                Some(InteractiveHitTarget::ContextTab(tab)) => {
                    self.focus_shell_region(InteractiveRegion::Context);
                    self.local.context_tab = tab;
                    self.clamp_context_navigation();
                }
                Some(InteractiveHitTarget::ContextRow(index)) => {
                    self.focus_shell_region(InteractiveRegion::Context);
                    self.local.context_selection[self.local.context_tab.index()] = index;
                    self.ensure_context_selection_visible();
                }
                Some(InteractiveHitTarget::Composer) => {
                    self.focus_shell_region(InteractiveRegion::Composer);
                }
                None => {}
            },
            _ => {}
        }
        true
    }

    pub(super) fn focus_shell_region(&mut self, region: InteractiveRegion) {
        if self.local.focus_ring.focus(region) {
            self.apply_region_focus();
        }
    }

    pub(super) fn toggle_context(&mut self, mode: ShellLayoutMode) {
        if mode == ShellLayoutMode::Wide {
            self.local.focus_ring.focus(InteractiveRegion::Context);
            self.apply_region_focus();
            return;
        }
        if self.local.context_open {
            self.close_context_overlay();
        } else {
            self.local.context_restore_focus = self
                .local
                .focus_ring
                .current()
                .unwrap_or(InteractiveRegion::Composer);
            self.local.context_open = true;
            self.refresh_shell_focus();
        }
    }

    pub(super) fn close_context_overlay(&mut self) {
        self.local.context_open = false;
        self.refresh_shell_focus();
        self.local
            .focus_ring
            .focus(self.local.context_restore_focus);
        self.apply_region_focus();
    }

    pub(super) fn refresh_shell_focus(&mut self) {
        if self.active_child_operation_id.is_some() {
            self.local
                .focus_ring
                .set_items([InteractiveRegion::Conversation]);
            self.local.focus_ring.focus(InteractiveRegion::Conversation);
            self.apply_region_focus();
            return;
        }
        match shell_layout_mode(self.viewport_width) {
            ShellLayoutMode::Wide => {
                self.local.context_open = false;
                self.local.focus_ring.set_items([
                    InteractiveRegion::Conversation,
                    InteractiveRegion::Context,
                    InteractiveRegion::Composer,
                ]);
            }
            ShellLayoutMode::Medium | ShellLayoutMode::Narrow if self.local.context_open => {
                self.local
                    .focus_ring
                    .set_items([InteractiveRegion::Context]);
                self.local.focus_ring.focus(InteractiveRegion::Context);
            }
            ShellLayoutMode::Medium | ShellLayoutMode::Narrow => {
                self.local
                    .focus_ring
                    .set_items([InteractiveRegion::Conversation, InteractiveRegion::Composer]);
            }
        }
        self.apply_region_focus();
    }

    pub(super) fn apply_region_focus(&mut self) {
        self.local
            .editor
            .set_focused(self.local.focus_ring.current() == Some(InteractiveRegion::Composer));
    }

    pub(super) fn shell_layout(&self, composer_height: usize) -> ShellLayout {
        let width = self.viewport_width.max(1);
        let height = self.viewport_height.max(1);
        let mode = shell_layout_mode(width);
        let status_height = usize::from(height >= 2);
        let context_page = mode == ShellLayoutMode::Narrow && self.local.context_open;
        let maximum_composer = height.saturating_sub(status_height + 1).max(1);
        let composer_height = if context_page {
            0
        } else {
            composer_height.clamp(1, maximum_composer)
        };
        let rows = Layout::vertical(
            Rect::new(0, 0, width, height),
            &[
                Constraint::Fill(1),
                Constraint::Length(composer_height),
                Constraint::Length(status_height),
            ],
        );
        let work = rows[0];
        let composer = rows[1];
        let status = rows[2];

        match mode {
            ShellLayoutMode::Wide => {
                let context_width = (width / 3).clamp(26, 38).min(width.saturating_sub(2));
                let columns = Layout::horizontal(
                    work,
                    &[
                        Constraint::Fill(1),
                        Constraint::Length(1),
                        Constraint::Length(context_width),
                    ],
                );
                let side_rows = if work.height >= TIPS_MIN_HEIGHT {
                    Layout::vertical(
                        columns[2],
                        &[
                            Constraint::Fill(1),
                            Constraint::Length(1),
                            Constraint::Length(4),
                        ],
                    )
                } else {
                    Layout::vertical(columns[2], &[Constraint::Fill(1)])
                };
                ShellLayout {
                    mode,
                    conversation: columns[0],
                    conversation_context_divider: Some(columns[1]),
                    context_drawer_divider: None,
                    context: Some(side_rows[0]),
                    context_tips_divider: (side_rows.len() == 3).then(|| side_rows[1]),
                    tips: (side_rows.len() == 3).then(|| side_rows[2]),
                    composer,
                    status,
                    work,
                }
            }
            ShellLayoutMode::Medium => {
                let (context_drawer_divider, context) = if self.local.context_open {
                    let overlay_width = (width * 2 / 5).clamp(26, 38).min(width);
                    let drawer = Rect::new(
                        width.saturating_sub(overlay_width),
                        work.y,
                        overlay_width,
                        work.height,
                    );
                    (
                        Some(Rect::new(
                            drawer.x,
                            drawer.y,
                            1.min(drawer.width),
                            drawer.height,
                        )),
                        Some(Rect::new(
                            drawer.x.saturating_add(1),
                            drawer.y,
                            drawer.width.saturating_sub(1),
                            drawer.height,
                        )),
                    )
                } else {
                    (None, None)
                };
                ShellLayout {
                    mode,
                    conversation: work,
                    conversation_context_divider: None,
                    context_drawer_divider,
                    context,
                    context_tips_divider: None,
                    tips: None,
                    composer,
                    status,
                    work,
                }
            }
            ShellLayoutMode::Narrow => ShellLayout {
                mode,
                conversation: work,
                conversation_context_divider: None,
                context_drawer_divider: None,
                context: self.local.context_open.then_some(work),
                context_tips_divider: None,
                tips: None,
                composer,
                status,
                work,
            },
        }
    }

    pub(super) fn rebuild_mouse_hit_regions(
        &mut self,
        layout: ShellLayout,
        conversation_body: Rect,
        transcript_total_rows: usize,
        block_rows: &[(TranscriptBlockId, TranscriptBlockRows)],
    ) {
        self.local.mouse_hits.clear();
        self.local.mouse_hits.push(HitRegion::new(
            layout.conversation,
            InteractiveHitTarget::Conversation,
        ));

        let (viewport_start, viewport_end) = transcript_viewport_bounds(
            transcript_total_rows,
            conversation_body.height,
            self.transcript.scroll_offset(),
        );
        for &(block_id, rows) in block_rows {
            let visible_start = rows.start.max(viewport_start);
            let visible_end = rows.end.min(viewport_end);
            if visible_start >= visible_end {
                continue;
            }
            let block_rect = Rect::new(
                conversation_body.x,
                conversation_body
                    .y
                    .saturating_add(visible_start.saturating_sub(viewport_start)),
                conversation_body.width,
                visible_end.saturating_sub(visible_start),
            );
            self.local.mouse_hits.push(HitRegion::new(
                block_rect,
                InteractiveHitTarget::TranscriptBlock(block_id),
            ));

            if rows.start >= viewport_start
                && rows.start < viewport_end
                && self
                    .transcript
                    .item_for_block(block_id)
                    .is_some_and(TranscriptItem::foldable)
            {
                self.local.mouse_hits.push(HitRegion::new(
                    Rect::new(
                        conversation_body.x,
                        conversation_body
                            .y
                            .saturating_add(rows.start.saturating_sub(viewport_start)),
                        conversation_body.width,
                        1,
                    ),
                    InteractiveHitTarget::TranscriptDisclosure(block_id),
                ));
            }
        }

        if let Some(context) = layout.context {
            self.local
                .mouse_hits
                .push(HitRegion::new(context, InteractiveHitTarget::Context));
            let mut tab_x = context.x.saturating_add(10);
            for (tab, label) in visible_context_tabs(context.width, self.local.context_tab) {
                let tab_width = visible_width(label)
                    .saturating_add(usize::from(tab == self.local.context_tab) * 2);
                if tab_x < context.right() {
                    self.local.mouse_hits.push(HitRegion::new(
                        Rect::new(
                            tab_x,
                            context.y,
                            tab_width.min(context.right().saturating_sub(tab_x)),
                            1,
                        ),
                        InteractiveHitTarget::ContextTab(tab),
                    ));
                }
                tab_x = tab_x.saturating_add(tab_width + 1);
            }
            if self.local.context_tab != ContextTab::Usage {
                let item_count = self.context_items(self.local.context_tab).len();
                let scroll = self.local.context_scroll[self.local.context_tab.index()];
                for (visible_index, item_index) in (scroll..item_count)
                    .take(context.height.saturating_sub(1))
                    .enumerate()
                {
                    self.local.mouse_hits.push(HitRegion::new(
                        Rect::new(
                            context.x,
                            context.y.saturating_add(1 + visible_index),
                            context.width,
                            1,
                        ),
                        InteractiveHitTarget::ContextRow(item_index),
                    ));
                }
            }
        }
        self.local.mouse_hits.push(HitRegion::new(
            layout.composer,
            InteractiveHitTarget::Composer,
        ));
    }
}
