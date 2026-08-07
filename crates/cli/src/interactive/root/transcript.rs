use super::*;

impl InteractiveRoot {
    pub(super) fn transcript_row_snapshot(
        &mut self,
        max_tool_result_lines: usize,
    ) -> TranscriptRowSnapshot {
        self.sync_transcript_view();
        let opts =
            self.transcript_render_options(self.transcript_render_width(), max_tool_result_lines);
        self.local
            .render_cache
            .row_snapshot(&self.transcript, &opts)
    }

    pub(super) fn transcript_row_delta_since(
        &mut self,
        snapshot: TranscriptRowSnapshot,
        changed_indices: &[usize],
        max_tool_result_lines: usize,
        anchor_start_row: Option<usize>,
    ) -> isize {
        self.sync_transcript_view();
        let opts =
            self.transcript_render_options(self.transcript_render_width(), max_tool_result_lines);
        self.local.render_cache.row_delta_since(
            &self.transcript,
            &opts,
            snapshot,
            changed_indices,
            anchor_start_row,
        )
    }

    pub(super) fn transcript_render_width(&self) -> usize {
        self.conversation_viewport_width.max(1)
    }

    pub(super) fn transcript_total_rows(&mut self) -> usize {
        let opts =
            self.transcript_render_options(self.transcript_render_width(), MAX_TOOL_RESULT_LINES);
        self.local
            .render_cache
            .row_snapshot(&self.transcript, &opts)
            .total_rows()
    }

    pub(super) fn toggle_selected_transcript_block(&mut self) -> bool {
        let previous_scroll_offset = self.transcript.scroll_offset();
        let previous_rows = (previous_scroll_offset > 0).then(|| self.transcript_total_rows());
        let changed = self.local.transcript_view.toggle_selected(&self.transcript);
        if !changed {
            return false;
        }
        // Keep the viewport anchored: when scrolled, the previously visible
        // rows stay put and the expanded rows push the content below them
        // downward; when pinned to the tail the new rows simply extend the
        // tail. Scrolling the expanded block to the viewport top (what
        // ensure_selected_transcript_visible does for navigation) would jump
        // the transcript to the top and hide everything before the block.
        if let Some(previous_rows) = previous_rows {
            let current_rows = self.transcript_total_rows();
            self.transcript.preserve_scrolled_view_after_row_change(
                previous_scroll_offset,
                previous_rows,
                current_rows,
            );
        }
        true
    }

    pub(super) fn select_transcript_block(&mut self, block_id: TranscriptBlockId) -> bool {
        self.sync_transcript_view();
        let changed = self
            .local
            .transcript_view
            .select(&self.transcript, block_id);
        if changed {
            self.ensure_selected_transcript_visible();
        }
        changed
    }

    pub(super) fn toggle_selected_transcript_arguments(&mut self) -> bool {
        let previous_scroll_offset = self.transcript.scroll_offset();
        let previous_rows = (previous_scroll_offset > 0).then(|| self.transcript_total_rows());
        let changed = self
            .local
            .transcript_view
            .toggle_selected_arguments(&self.transcript);
        if !changed {
            return false;
        }
        if let Some(previous_rows) = previous_rows {
            let current_rows = self.transcript_total_rows();
            self.transcript.preserve_scrolled_view_after_row_change(
                previous_scroll_offset,
                previous_rows,
                current_rows,
            );
        }
        true
    }

    pub(super) fn ensure_selected_transcript_visible(&mut self) {
        let Some(selected) = self.local.transcript_view.selected() else {
            return;
        };
        let opts =
            self.transcript_render_options(self.transcript_render_width(), MAX_TOOL_RESULT_LINES);
        let Some(rows) = self
            .local
            .render_cache
            .block_rows(&self.transcript, &opts, selected)
        else {
            return;
        };
        self.transcript.ensure_row_range_visible(
            rows.total_rows,
            rows.start,
            rows.end,
            self.conversation_viewport_height.max(1),
        );
    }

    pub(in crate::interactive) fn handle_slash_suggestion_input(
        &mut self,
        event: &InputEvent,
    ) -> bool {
        if self.local.selecting_model
            || self.local.selecting_settings
            || self.local.selecting_session
            || self.delegation_confirmation_menu.is_some()
            || self.profile_menu.is_some()
            || self.pending_profile_task.is_some()
        {
            return false;
        }
        let commands = self.all_slash_commands();
        slash::handle_suggestion_input(
            &self.local.keybindings,
            event,
            &mut self.local.editor,
            &mut self.slash_suggestion_selected,
            &mut self.slash_suggestions_dismissed_for,
            &commands,
        )
    }

    pub(in crate::interactive) fn handle_model_selection_input(
        &mut self,
        event: &InputEvent,
    ) -> bool {
        if !self.local.selecting_model {
            return false;
        }

        match model_selector::handle_input(
            &self.local.keybindings,
            event,
            &mut self.local.editor,
            &mut self.local.model_selection_selected,
            &self.available_models,
        ) {
            model_selector::SelectorInput::Handled => {}
            model_selector::SelectorInput::Cancel => {
                self.local.selecting_model = false;
                self.local.model_selection_selected = 0;
                self.local.editor.set_text("");
                self.transcript.push(TranscriptItem::system(
                    "Model selection canceled".to_string(),
                ));
            }
            model_selector::SelectorInput::Confirm(Some(model_index)) => {
                let model = self.available_models[model_index].clone();
                self.set_selected_model(model);
            }
            model_selector::SelectorInput::Confirm(None) => {}
        }
        true
    }

    pub(in crate::interactive) fn handle_tree_selection_input(
        &mut self,
        event: &InputEvent,
    ) -> bool {
        if !self.local.selecting_tree {
            return false;
        }

        let Some(selector) = self.local.tree_selector.as_mut() else {
            return false;
        };

        match selector.handle_input(&self.local.keybindings, event) {
            TreeSelectorInput::Cancel => {
                self.local.selecting_tree = false;
                self.local.tree_selector = None;
                self.local.selected_tree_entry_id = None;
                self.local.editor.set_text("");
            }
            TreeSelectorInput::Confirm(Some(entry_id)) => {
                self.local.selected_tree_entry_id = Some(entry_id);
                self.local.selecting_tree = false;
                self.local.tree_selector = None;
            }
            TreeSelectorInput::Confirm(None) => {}
            TreeSelectorInput::EditLabel { .. } => {
                // Label edit is handled inside the selector state
            }
            TreeSelectorInput::SaveLabel { entry_id, label } => {
                self.local.pending_tree_label_change = Some((entry_id, label));
            }
            TreeSelectorInput::Handled => {}
        }
        true
    }

    pub(in crate::interactive) fn handle_session_selection_input(
        &mut self,
        event: &InputEvent,
    ) -> bool {
        if !self.local.selecting_session {
            return false;
        }

        match session_selector::handle_input(
            &self.local.keybindings,
            event,
            &mut self.local.editor,
            &mut self.local.session_selection_selected,
            &self.session_choices,
        ) {
            session_selector::SelectorInput::Handled => {}
            session_selector::SelectorInput::Cancel => {
                self.local.selecting_session = false;
                self.local.session_selection_selected = 0;
                self.local.editor.set_text("");
                self.transcript.push(TranscriptItem::system(
                    "Session selection canceled".to_string(),
                ));
            }
            session_selector::SelectorInput::Confirm(Some(session_index)) => {
                let choice = self.session_choices[session_index].clone();
                self.set_selected_session(choice);
            }
            session_selector::SelectorInput::Confirm(None) => {}
        }
        true
    }
}
