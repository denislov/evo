use std::collections::{HashMap, HashSet};

use crate::interactive::transcript::{
    Transcript, TranscriptBlockId, TranscriptDisplayState, TranscriptItem, TranscriptRenderKey,
};

use super::{
    TranscriptRenderOptions, render_block, render_profile_hash, render_row_profile_hash,
    transcript_image_id,
};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct TranscriptBlockCacheKey {
    transcript_id: u64,
    item_id: u64,
    item_revision: u64,
    profile_hash: u64,
    display_state: TranscriptDisplayState,
    tool_argument_state: TranscriptDisplayState,
    selected: bool,
    selection_gutter: bool,
}

#[derive(Debug, Clone)]
struct TranscriptBlockCacheEntry {
    lines: Vec<String>,
    line_count: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct TranscriptRowMetadataKey {
    transcript_id: u64,
    profile_hash: u64,
}

#[derive(Debug, Clone)]
struct TranscriptRowMetadataEntry {
    item_id: u64,
    contribution_line_count: usize,
    end_row: usize,
    has_visible_rows: bool,
    separator_before: bool,
}

#[derive(Debug, Clone)]
struct TranscriptRowMetadata {
    content_revision: u64,
    total_rows: usize,
    has_visible_rows: bool,
    entries: Vec<TranscriptRowMetadataEntry>,
}

impl TranscriptRowMetadata {
    fn new(content_revision: u64) -> Self {
        Self {
            content_revision,
            total_rows: 0,
            has_visible_rows: false,
            entries: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::interactive) struct TranscriptRowSnapshot {
    key: TranscriptRowMetadataKey,
    content_revision: u64,
    total_rows: usize,
}

impl TranscriptRowSnapshot {
    pub(in crate::interactive) fn total_rows(self) -> usize {
        self.total_rows
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::interactive) struct TranscriptBlockRows {
    pub total_rows: usize,
    pub start: usize,
    pub end: usize,
}

#[derive(Debug, Default)]
pub(in crate::interactive) struct TranscriptViewport {
    pub lines: Vec<String>,
    pub total_rows: usize,
    pub block_rows: Vec<(TranscriptBlockId, TranscriptBlockRows)>,
}

#[cfg(test)]
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(in crate::interactive) struct TranscriptRenderCacheStats {
    pub block_hits: usize,
    pub block_misses: usize,
    pub row_metadata_hits: usize,
    pub row_metadata_misses: usize,
    pub row_delta_hits: usize,
    pub row_delta_fallbacks: usize,
}

#[derive(Debug, Default)]
pub(in crate::interactive) struct TranscriptRenderCache {
    blocks: HashMap<TranscriptBlockCacheKey, TranscriptBlockCacheEntry>,
    row_metadata: HashMap<TranscriptRowMetadataKey, TranscriptRowMetadata>,
    #[cfg(test)]
    stats: TranscriptRenderCacheStats,
}

impl TranscriptRenderCache {
    pub(in crate::interactive) fn new() -> Self {
        Self::default()
    }

    pub(in crate::interactive) fn clear(&mut self) {
        self.blocks.clear();
        self.row_metadata.clear();
        #[cfg(test)]
        self.reset_stats();
    }

    pub(in crate::interactive) fn render_viewport(
        &mut self,
        transcript: &Transcript,
        opts: &TranscriptRenderOptions<'_>,
        height: usize,
        scroll_offset: usize,
    ) -> TranscriptViewport {
        let profile_hash = render_profile_hash(opts);
        let row_profile_hash = render_row_profile_hash(opts, profile_hash);
        let key = self.row_metadata_key(transcript, row_profile_hash);
        let needs_rebuild = self
            .row_metadata
            .get(&key)
            .is_none_or(|metadata| metadata.content_revision != transcript.content_revision());
        if needs_rebuild {
            self.rebuild_row_metadata(transcript, opts, profile_hash, row_profile_hash);
        }
        let Some(metadata) = self.row_metadata.get(&key) else {
            return TranscriptViewport::default();
        };
        let total_rows = metadata.total_rows;
        if height == 0 || total_rows == 0 {
            return TranscriptViewport {
                total_rows,
                ..Default::default()
            };
        }
        let max_offset = total_rows.saturating_sub(height);
        let offset = scroll_offset.min(max_offset);
        let viewport_end = total_rows.saturating_sub(offset);
        let viewport_start = viewport_end.saturating_sub(height);
        let first_index = metadata
            .entries
            .partition_point(|entry| entry.end_row <= viewport_start);
        let visible_entries = metadata.entries[first_index..]
            .iter()
            .take_while(|entry| {
                entry.end_row.saturating_sub(entry.contribution_line_count) < viewport_end
            })
            .cloned()
            .collect::<Vec<_>>();

        let mut lines = Vec::with_capacity(viewport_end.saturating_sub(viewport_start));
        let mut block_rows = Vec::with_capacity(visible_entries.len());
        for (offset, entry) in visible_entries.into_iter().enumerate() {
            let index = first_index + offset;
            let Some((render_key, item)) = transcript.render_entry_at(index) else {
                continue;
            };
            let contribution_start = entry.end_row - entry.contribution_line_count;
            let block_start = contribution_start + usize::from(entry.separator_before);
            let block_end = entry.end_row;
            let (display_state, tool_argument_state, selected, selection_gutter) =
                block_view(render_key, item, opts);
            let block_key = block_cache_key(
                render_key,
                profile_hash,
                display_state,
                tool_argument_state,
                selected,
                selection_gutter,
            );
            let block = self.render_block(
                &block_key,
                item,
                opts,
                display_state,
                tool_argument_state,
                selected,
                selection_gutter,
            );
            let local_start = viewport_start.saturating_sub(contribution_start);
            let local_end = viewport_end.min(entry.end_row) - contribution_start;
            if entry.separator_before && local_start == 0 && local_end > 0 {
                lines.push(String::new());
            }
            let block_local_start = local_start.saturating_sub(usize::from(entry.separator_before));
            let block_local_end = local_end
                .saturating_sub(usize::from(entry.separator_before))
                .min(block.lines.len());
            if block_local_start < block_local_end {
                lines.extend_from_slice(&block.lines[block_local_start..block_local_end]);
            }
            block_rows.push((
                render_key.block_id(),
                TranscriptBlockRows {
                    total_rows,
                    start: block_start,
                    end: block_end,
                },
            ));
        }
        TranscriptViewport {
            lines,
            total_rows,
            block_rows,
        }
    }

    pub(in crate::interactive) fn row_snapshot(
        &mut self,
        transcript: &Transcript,
        opts: &TranscriptRenderOptions<'_>,
    ) -> TranscriptRowSnapshot {
        let profile_hash = render_profile_hash(opts);
        let row_profile_hash = render_row_profile_hash(opts, profile_hash);
        let key = self.row_metadata_key(transcript, row_profile_hash);
        if let Some(metadata) = self
            .row_metadata
            .get(&key)
            .filter(|metadata| metadata.content_revision == transcript.content_revision())
        {
            #[cfg(test)]
            {
                self.stats.row_metadata_hits += 1;
            }
            return TranscriptRowSnapshot {
                key,
                content_revision: metadata.content_revision,
                total_rows: metadata.total_rows,
            };
        }

        #[cfg(test)]
        {
            self.stats.row_metadata_misses += 1;
        }
        let metadata = self.rebuild_row_metadata(transcript, opts, profile_hash, row_profile_hash);
        TranscriptRowSnapshot {
            key,
            content_revision: metadata.content_revision,
            total_rows: metadata.total_rows,
        }
    }

    pub(in crate::interactive) fn row_delta_since(
        &mut self,
        transcript: &Transcript,
        opts: &TranscriptRenderOptions<'_>,
        before: TranscriptRowSnapshot,
        changed_indices: &[usize],
        anchor_start_row: Option<usize>,
    ) -> isize {
        let profile_hash = render_profile_hash(opts);
        let row_profile_hash = render_row_profile_hash(opts, profile_hash);
        let key = self.row_metadata_key(transcript, row_profile_hash);
        if key != before.key {
            return self.row_delta_fallback(
                transcript,
                opts,
                profile_hash,
                row_profile_hash,
                before.total_rows,
            );
        }
        if before.content_revision == transcript.content_revision() {
            return 0;
        }
        if self
            .row_metadata
            .get(&key)
            .is_none_or(|metadata| metadata.content_revision != before.content_revision)
        {
            return self.row_delta_fallback(
                transcript,
                opts,
                profile_hash,
                row_profile_hash,
                before.total_rows,
            );
        }

        let mut indices = changed_indices.to_vec();
        indices.sort_unstable();
        indices.dedup();
        if indices.is_empty() {
            return self.row_delta_fallback(
                transcript,
                opts,
                profile_hash,
                row_profile_hash,
                before.total_rows,
            );
        }

        let old_positions = self.row_metadata.get(&key).map(|metadata| {
            let mut row = 0usize;
            metadata
                .entries
                .iter()
                .map(|entry| {
                    let position = (row, row.saturating_add(entry.contribution_line_count));
                    row = position.1;
                    position
                })
                .collect::<Vec<_>>()
        });
        let mut signed_anchor_delta = 0isize;
        for index in indices {
            let Some((render_key, item)) = transcript.render_entry_at(index) else {
                return self.row_delta_fallback(
                    transcript,
                    opts,
                    profile_hash,
                    row_profile_hash,
                    before.total_rows,
                );
            };
            let old_entry = self
                .row_metadata
                .get(&key)
                .and_then(|metadata| metadata.entries.get(index))
                .cloned();
            let separator_before = match old_entry.as_ref() {
                Some(entry) => {
                    if entry.item_id != render_key.item_id {
                        return self.row_delta_fallback(
                            transcript,
                            opts,
                            profile_hash,
                            row_profile_hash,
                            before.total_rows,
                        );
                    }
                    entry.separator_before
                }
                None => {
                    let metadata = self
                        .row_metadata
                        .get(&key)
                        .expect("row metadata exists after earlier guard");
                    if index != metadata.entries.len() {
                        return self.row_delta_fallback(
                            transcript,
                            opts,
                            profile_hash,
                            row_profile_hash,
                            before.total_rows,
                        );
                    }
                    metadata.has_visible_rows
                }
            };

            let (display_state, tool_argument_state, selected, selection_gutter) =
                block_view(render_key, item, opts);
            let block_key = block_cache_key(
                render_key,
                profile_hash,
                display_state,
                tool_argument_state,
                selected,
                selection_gutter,
            );
            let block = self.render_block(
                &block_key,
                item,
                opts,
                display_state,
                tool_argument_state,
                selected,
                selection_gutter,
            );
            let new_entry =
                row_metadata_entry(render_key, item, block.line_count, separator_before, 0);
            let metadata = self
                .row_metadata
                .get_mut(&key)
                .expect("row metadata exists after earlier guard");

            if let Some(old_entry) = old_entry {
                if old_entry.has_visible_rows != new_entry.has_visible_rows {
                    return self.row_delta_fallback(
                        transcript,
                        opts,
                        profile_hash,
                        row_profile_hash,
                        before.total_rows,
                    );
                }
                let delta = new_entry.contribution_line_count as isize
                    - old_entry.contribution_line_count as isize;
                let affects_anchor = anchor_start_row.is_none_or(|anchor| {
                    old_positions
                        .as_ref()
                        .and_then(|positions| positions.get(index))
                        .is_none_or(|(_, end)| *end > anchor)
                });
                if affects_anchor {
                    signed_anchor_delta += delta;
                }
                metadata.total_rows = add_signed_usize(metadata.total_rows, delta);
                let mut new_entry = new_entry;
                new_entry.end_row = add_signed_usize(old_entry.end_row, delta);
                metadata.entries[index] = new_entry;
                for entry in &mut metadata.entries[index + 1..] {
                    entry.end_row = add_signed_usize(entry.end_row, delta);
                }
            } else {
                let mut new_entry = new_entry;
                new_entry.end_row = metadata
                    .total_rows
                    .saturating_add(new_entry.contribution_line_count);
                let delta = usize_to_isize(new_entry.contribution_line_count);
                let old_total_rows = before.total_rows;
                if anchor_start_row.is_none_or(|anchor| old_total_rows >= anchor) {
                    signed_anchor_delta += delta;
                }
                metadata.total_rows = metadata
                    .total_rows
                    .saturating_add(new_entry.contribution_line_count);
                metadata.has_visible_rows |= new_entry.has_visible_rows;
                metadata.entries.push(new_entry);
            }
        }

        if let Some(metadata) = self.row_metadata.get_mut(&key) {
            metadata.content_revision = transcript.content_revision();
        }
        #[cfg(test)]
        {
            self.stats.row_delta_hits += 1;
        }
        signed_anchor_delta
    }

    pub(in crate::interactive) fn block_rows(
        &mut self,
        transcript: &Transcript,
        opts: &TranscriptRenderOptions<'_>,
        block_id: TranscriptBlockId,
    ) -> Option<TranscriptBlockRows> {
        let profile_hash = render_profile_hash(opts);
        let row_profile_hash = render_row_profile_hash(opts, profile_hash);
        let key = self.row_metadata_key(transcript, row_profile_hash);
        let needs_rebuild = self
            .row_metadata
            .get(&key)
            .is_none_or(|metadata| metadata.content_revision != transcript.content_revision());
        if needs_rebuild {
            self.rebuild_row_metadata(transcript, opts, profile_hash, row_profile_hash);
        }
        let metadata = self.row_metadata.get(&key)?;
        let mut row = 0usize;
        for ((render_key, _), entry) in transcript.render_entries().zip(&metadata.entries) {
            let block_start = row + usize::from(entry.separator_before);
            let block_end = row + entry.contribution_line_count;
            if render_key.block_id() == block_id {
                return Some(TranscriptBlockRows {
                    total_rows: metadata.total_rows,
                    start: block_start,
                    end: block_end,
                });
            }
            row = block_end;
        }
        None
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "render cache input dimensions are explicit parts of the cache contract"
    )]
    fn render_block(
        &mut self,
        key: &TranscriptBlockCacheKey,
        item: &TranscriptItem,
        opts: &TranscriptRenderOptions<'_>,
        display_state: TranscriptDisplayState,
        tool_argument_state: TranscriptDisplayState,
        selected: bool,
        selection_gutter: bool,
    ) -> TranscriptBlockCacheEntry {
        if let Some(entry) = self.blocks.get(key) {
            #[cfg(test)]
            {
                self.stats.block_hits += 1;
            }
            return entry.clone();
        }
        #[cfg(test)]
        {
            self.stats.block_misses += 1;
        }

        let block = render_block(
            item,
            opts.width,
            opts.max_tool_result_lines,
            opts.color,
            &opts.markdown_theme,
            opts.hide_thinking_block,
            opts.hidden_thinking_label,
            opts.styles,
            display_state,
            tool_argument_state,
            transcript_image_id(key.transcript_id, key.item_id),
            selected,
            selection_gutter,
            opts.show_images,
            opts.image_width_cells,
            opts.terminal_capabilities,
        );
        let entry = TranscriptBlockCacheEntry {
            line_count: block.len(),
            lines: block,
        };
        self.blocks.insert(key.clone(), entry.clone());
        entry
    }

    fn retain_used_blocks(&mut self, used_keys: &HashSet<TranscriptBlockCacheKey>) {
        self.blocks.retain(|key, _| used_keys.contains(key));
    }

    fn rebuild_row_metadata(
        &mut self,
        transcript: &Transcript,
        opts: &TranscriptRenderOptions<'_>,
        profile_hash: u64,
        row_profile_hash: u64,
    ) -> TranscriptRowMetadata {
        let mut metadata = TranscriptRowMetadata::new(transcript.content_revision());
        let mut used_keys = HashSet::new();

        for (render_key, item) in transcript.render_entries() {
            let (display_state, tool_argument_state, selected, selection_gutter) =
                block_view(render_key, item, opts);
            let block_key = block_cache_key(
                render_key,
                profile_hash,
                display_state,
                tool_argument_state,
                selected,
                selection_gutter,
            );
            used_keys.insert(block_key.clone());
            let block = self.render_block(
                &block_key,
                item,
                opts,
                display_state,
                tool_argument_state,
                selected,
                selection_gutter,
            );
            let entry = row_metadata_entry(
                render_key,
                item,
                block.line_count,
                metadata.has_visible_rows,
                metadata.total_rows,
            );
            metadata.total_rows += entry.contribution_line_count;
            metadata.has_visible_rows |= entry.has_visible_rows;
            metadata.entries.push(entry);
        }

        self.retain_used_blocks(&used_keys);
        self.record_row_metadata(transcript, row_profile_hash, metadata.clone());
        metadata
    }

    fn row_delta_fallback(
        &mut self,
        transcript: &Transcript,
        opts: &TranscriptRenderOptions<'_>,
        profile_hash: u64,
        row_profile_hash: u64,
        previous_total_rows: usize,
    ) -> isize {
        #[cfg(test)]
        {
            self.stats.row_delta_fallbacks += 1;
        }
        let current_total_rows = self
            .rebuild_row_metadata(transcript, opts, profile_hash, row_profile_hash)
            .total_rows;
        row_count_delta(current_total_rows, previous_total_rows)
    }

    fn record_row_metadata(
        &mut self,
        transcript: &Transcript,
        profile_hash: u64,
        metadata: TranscriptRowMetadata,
    ) {
        let key = self.row_metadata_key(transcript, profile_hash);
        self.row_metadata.insert(key, metadata);
    }

    fn row_metadata_key(
        &self,
        transcript: &Transcript,
        profile_hash: u64,
    ) -> TranscriptRowMetadataKey {
        TranscriptRowMetadataKey {
            transcript_id: transcript.render_cache_id(),
            profile_hash,
        }
    }

    #[cfg(test)]
    pub(in crate::interactive) fn reset_stats(&mut self) {
        self.stats = TranscriptRenderCacheStats::default();
    }
}

fn block_cache_key(
    render_key: TranscriptRenderKey,
    profile_hash: u64,
    display_state: TranscriptDisplayState,
    tool_argument_state: TranscriptDisplayState,
    selected: bool,
    selection_gutter: bool,
) -> TranscriptBlockCacheKey {
    TranscriptBlockCacheKey {
        transcript_id: render_key.transcript_id,
        item_id: render_key.item_id,
        item_revision: render_key.item_revision,
        profile_hash,
        display_state,
        tool_argument_state,
        selected,
        selection_gutter,
    }
}

fn block_view(
    render_key: TranscriptRenderKey,
    item: &TranscriptItem,
    opts: &TranscriptRenderOptions<'_>,
) -> (TranscriptDisplayState, TranscriptDisplayState, bool, bool) {
    let block_id = render_key.block_id();
    let display_state = opts.view.as_ref().map_or_else(
        || legacy_display_state(item),
        |view| view.display_state(block_id, item),
    );
    let tool_argument_state = opts
        .view
        .as_ref()
        .map_or(TranscriptDisplayState::Collapsed, |view| {
            view.tool_argument_state(block_id, item)
        });
    let selection_gutter = opts.selection_gutter;
    let selected = item.selectable() && opts.selected_block == Some(block_id);
    (
        display_state,
        tool_argument_state,
        selected,
        selection_gutter,
    )
}

pub(super) fn legacy_display_state(item: &TranscriptItem) -> TranscriptDisplayState {
    match item {
        TranscriptItem::Tool { name, .. } if matches!(name.as_str(), "edit" | "write") => {
            TranscriptDisplayState::Expanded
        }
        TranscriptItem::Tool { .. } => TranscriptDisplayState::Preview,
        _ => TranscriptDisplayState::Expanded,
    }
}

fn row_metadata_entry(
    render_key: TranscriptRenderKey,
    item: &TranscriptItem,
    block_line_count: usize,
    has_visible_rows_before: bool,
    contribution_start: usize,
) -> TranscriptRowMetadataEntry {
    let is_visible_block = !matches!(item, TranscriptItem::System { .. });
    let has_visible_rows = is_visible_block && block_line_count > 0;
    let separator_before = has_visible_rows && has_visible_rows_before;
    TranscriptRowMetadataEntry {
        item_id: render_key.item_id,
        contribution_line_count: block_line_count + usize::from(separator_before),
        end_row: contribution_start
            .saturating_add(block_line_count)
            .saturating_add(usize::from(separator_before)),
        has_visible_rows,
        separator_before,
    }
}

fn add_signed_usize(value: usize, delta: isize) -> usize {
    if delta >= 0 {
        value.saturating_add(delta as usize)
    } else {
        value.saturating_sub((-delta) as usize)
    }
}

fn usize_to_isize(value: usize) -> isize {
    isize::try_from(value).unwrap_or(isize::MAX)
}

fn row_count_delta(current: usize, previous: usize) -> isize {
    if current >= previous {
        usize_to_isize(current - previous)
    } else {
        -usize_to_isize(previous - current)
    }
}
