//! Conversation row construction: submitted/message/tool/live row resolution
//! and bounded render-cache synchronization.

use std::borrow::Cow;

use super::{
    ConversationBlockKind, ConversationController, ConversationItemKey, ConversationItemKind,
    ConversationRowRenderData, ConversationRowRenderSource, ConversationSource, LiveOverlay,
    message_block_id, sync_list_rows, tool_block_id, upsert_indexed_item,
};
use desktop::projection::{
    DesktopMessageOverlay, DesktopMessageStatus, DesktopToolOverlay, DesktopToolStatus,
};

impl ConversationController {
    // ---- row construction --------------------------------------------------

    fn resolve_submitted_row(
        &mut self,
        source: &ConversationSource<'_>,
        session_id: &str,
        panel_width: u32,
    ) -> Option<ConversationRowRenderData> {
        let submitted = source.submitted?;
        let row_id = submitted.block_id();
        Some(self.render_cache.resolve(
            ConversationRowRenderSource {
                item_key: ConversationItemKey::new(
                    session_id,
                    ConversationItemKind::Submitted,
                    &row_id,
                ),
                source_revision: submitted.command_id,
                title: Cow::Borrowed("You · submitted"),
                text: &submitted.payload,
                detail: "",
                kind: ConversationBlockKind::User,
                done: false,
                is_error: false,
                image_count: 0,
                reasoning_duration_millis: None,
                truncated: false,
                durable: false,
                delegation: None,
                turn: None,
                model: None,
            },
            panel_width,
        ))
    }

    fn resolve_message_row(
        &mut self,
        message: &DesktopMessageOverlay,
        session_id: &str,
        panel_width: u32,
    ) -> ConversationRowRenderData {
        let row_id = message_block_id(message);
        self.render_cache.resolve(
            ConversationRowRenderSource {
                item_key: ConversationItemKey::new(
                    session_id,
                    ConversationItemKind::LiveMessage,
                    &row_id,
                ),
                source_revision: message.updated_sequence,
                title: Cow::Borrowed("Assistant · live"),
                text: &message.text,
                detail: &message.thinking,
                kind: ConversationBlockKind::Assistant,
                done: matches!(message.status, DesktopMessageStatus::Completed),
                is_error: false,
                image_count: 0,
                reasoning_duration_millis: message.reasoning_duration_millis,
                truncated: message.truncated,
                durable: false,
                delegation: None,
                turn: None,
                model: None,
            },
            panel_width,
        )
    }

    fn resolve_tool_row(
        &mut self,
        tool: &DesktopToolOverlay,
        session_id: &str,
        panel_width: u32,
    ) -> ConversationRowRenderData {
        let row_id = tool_block_id(tool);
        self.render_cache.resolve(
            ConversationRowRenderSource {
                item_key: ConversationItemKey::new(
                    session_id,
                    ConversationItemKind::LiveTool,
                    &row_id,
                ),
                source_revision: tool.updated_sequence,
                title: Cow::Owned(format!("Tool · {}", tool.name)),
                text: &tool.detail,
                detail: &tool.arguments,
                kind: ConversationBlockKind::Tool,
                done: !matches!(tool.status, DesktopToolStatus::Running),
                is_error: matches!(tool.status, DesktopToolStatus::Failed),
                image_count: 0,
                reasoning_duration_millis: None,
                truncated: tool.truncated,
                durable: false,
                delegation: None,
                turn: None,
                model: None,
            },
            panel_width,
        )
    }

    fn resolve_live_row(
        &mut self,
        overlay: LiveOverlay<'_>,
        session_id: &str,
        panel_width: u32,
    ) -> ConversationRowRenderData {
        match overlay {
            LiveOverlay::Message(message) => {
                self.resolve_message_row(message, session_id, panel_width)
            }
            LiveOverlay::Tool(tool) => self.resolve_tool_row(tool, session_id, panel_width),
        }
    }

    /// Rebuild every durable and live row from the projection.
    fn rebuild_rows(&mut self, source: &ConversationSource<'_>, panel_width: u32) {
        let session_id = source.session_id().to_owned();
        let expected_count = source.visible_count();
        let mut rows = Vec::with_capacity(expected_count);
        self.render_cache.begin_frame();

        for block in source.conversation().blocks() {
            let promote_detail = block.kind != ConversationBlockKind::Assistant
                && block.text.is_empty()
                && !block.detail.is_empty();
            rows.push(self.render_cache.resolve(
                ConversationRowRenderSource {
                    item_key: ConversationItemKey::new(
                        &session_id,
                        ConversationItemKind::Durable(block.kind),
                        &block.id,
                    ),
                    source_revision: block.source_revision,
                    title: Cow::Borrowed(&block.title),
                    text: if promote_detail {
                        &block.detail
                    } else {
                        &block.text
                    },
                    detail: if promote_detail { "" } else { &block.detail },
                    kind: block.kind,
                    done: block.done,
                    is_error: block.is_error,
                    image_count: block.image_count,
                    reasoning_duration_millis: block.reasoning_duration_millis,
                    truncated: block.truncated,
                    durable: true,
                    delegation: block.delegation.clone(),
                    model: block.model.as_deref(),
                    turn: block.turn.as_ref(),
                },
                panel_width,
            ));
        }

        if let Some(row) = self.resolve_submitted_row(source, &session_id, panel_width) {
            rows.push(row);
        }
        for overlay in source.live_overlays() {
            rows.push(self.resolve_live_row(overlay, &session_id, panel_width));
        }

        self.render_cache.finish_frame();
        debug_assert_eq!(rows.len(), expected_count);
        let previous_rows = self.render_rows.replace(rows.clone());
        sync_list_rows(&self.scroll, &previous_rows, &rows);
        self.render_dirty_sequences.clear();
        self.render_sequence_overflow = false;
    }

    /// Rebuild only the live tail, keeping the durable rows and their cache
    /// entries intact. Falls back to a full rebuild when the durable prefix no
    /// longer matches the projection.
    fn rebuild_live_rows(&mut self, source: &ConversationSource<'_>, panel_width: u32) {
        let durable_count = source.durable_count();
        let durable_rows_invalid = {
            let rows = self.render_rows.borrow();
            rows.len() < durable_count || rows[..durable_count].iter().any(|row| !row.durable)
        };
        if durable_rows_invalid {
            self.rebuild_rows(source, panel_width);
            self.render_full_dirty = false;
            return;
        }

        let session_id = source.session_id().to_owned();
        self.render_cache.begin_frame();

        let mut live_rows =
            Vec::with_capacity(source.visible_count().saturating_sub(durable_count));
        if let Some(row) = self.resolve_submitted_row(source, &session_id, panel_width) {
            live_rows.push(row);
        }
        for overlay in source.live_overlays() {
            live_rows.push(self.resolve_live_row(overlay, &session_id, panel_width));
        }
        self.render_cache.finish_incremental();

        let previous_rows = self.render_rows.borrow().clone();
        let mut render_rows = self.render_rows.borrow_mut();
        render_rows.truncate(durable_count);
        render_rows.extend(live_rows);
        let next_rows = render_rows.clone();
        drop(render_rows);
        sync_list_rows(&self.scroll, &previous_rows, &next_rows);
        self.render_dirty_sequences.clear();
        self.render_sequence_overflow = false;
    }

    /// Refresh only the rows whose event sequence is dirty.
    ///
    /// Returns `Err(())` when the bounded sequence window overflowed or the
    /// resulting tail no longer matches the projection, so the caller falls back
    /// to a bounded live rebuild instead of scanning the whole history.
    fn update_rows_by_sequence(
        &mut self,
        source: &ConversationSource<'_>,
        panel_width: u32,
    ) -> Result<(), ()> {
        if self.render_sequence_overflow || self.render_dirty_sequences.is_empty() {
            return Err(());
        }

        let durable_count = source.durable_count();
        let submitted_count = source.submitted_count();
        let session_id = source.session_id().to_owned();
        let sequences = std::mem::take(&mut self.render_dirty_sequences);
        self.render_cache.begin_frame();

        for sequence in sequences {
            // At most one live row carries a given sequence: every product event
            // updates exactly one message or one tool.
            let Some((position, overlay)) = source
                .live_overlays()
                .enumerate()
                .find(|(_, overlay)| overlay.updated_sequence() == sequence)
            else {
                continue;
            };
            let row = self.resolve_live_row(overlay, &session_id, panel_width);
            let desired_index = durable_count + submitted_count + position;
            self.upsert_render_row(durable_count, desired_index, row);
        }
        self.render_cache.finish_incremental();
        self.live_rows_match(source).then_some(()).ok_or(())
    }

    fn upsert_render_row(
        &mut self,
        durable_count: usize,
        desired_index: usize,
        row: ConversationRowRenderData,
    ) {
        let existing_index = self.render_rows.borrow()[durable_count..]
            .iter()
            .position(|candidate| candidate.item_key == row.item_key)
            .map(|index| durable_count + index);
        let row_index = upsert_indexed_item(
            &mut self.render_rows.borrow_mut(),
            existing_index,
            desired_index,
            row,
        );
        if let Some(previous_index) = existing_index {
            let start = previous_index.min(row_index);
            let end = previous_index.max(row_index) + 1;
            self.scroll.remeasure_items(start..end);
        } else {
            self.scroll.splice(row_index..row_index, 1);
        }
    }

    /// Durable/live identity reconciliation: every live row must still line up
    /// with the projection overlay it was built from.
    fn live_rows_match(&self, source: &ConversationSource<'_>) -> bool {
        let durable_count = source.durable_count();
        let render_rows = self.render_rows.borrow();
        if render_rows.len() != source.visible_count() {
            return false;
        }
        let mut index = durable_count;
        if let Some(submitted) = source.submitted {
            let Some(row) = render_rows.get(index) else {
                return false;
            };
            if row.item_key.row_id() != submitted.block_id()
                || row.source_revision != submitted.command_id
            {
                return false;
            }
            index += 1;
        }
        for overlay in source.live_overlays() {
            let Some(row) = render_rows.get(index) else {
                return false;
            };
            if row.item_key.row_id() != overlay.row_id()
                || row.source_revision != overlay.updated_sequence()
            {
                return false;
            }
            index += 1;
        }
        index == render_rows.len()
    }

    // ---- frame preparation -------------------------------------------------

    /// Bring the native list's rows in line with the projection for one frame.
    pub(crate) fn prepare_rows(&mut self, source: &ConversationSource<'_>, layout_width: u32) {
        let visible_conversation_count = source.visible_count();
        let _span = tracing::trace_span!(
            "desktop.render.prepare_rows",
            layout_width,
            visible_rows = visible_conversation_count
        )
        .entered();
        let full_render_update =
            self.render_full_dirty || self.render_width_bucket != Some(layout_width);
        if full_render_update {
            self.rebuild_rows(source, layout_width);
            self.render_width_bucket = Some(layout_width);
            self.render_full_dirty = false;
            self.render_live_dirty = false;
        } else if self.render_live_dirty {
            match self.update_rows_by_sequence(source, layout_width) {
                Ok(_) => {
                    self.render_sequence_overflow = false;
                }
                Err(()) => {
                    self.rebuild_live_rows(source, layout_width);
                }
            }
            self.render_live_dirty = false;
        }
        debug_assert_eq!(self.render_rows.borrow().len(), visible_conversation_count);
    }
}
