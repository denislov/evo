use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::path::Path;
use std::sync::Arc;

use tui::api::component::{Component, Image, Loader, Markdown};
use tui::api::render::{
    Color, ERROR, SYSTEM, Style, TOOL_ERROR, TOOL_NAME, USER, paint_with, truncate_to_width,
    visible_width, wrap_text_with_ansi,
};
use tui::api::terminal::TerminalCapabilities;
use tui::api::theme::MarkdownTheme;

use crate::interactive::transcript::{
    Transcript, TranscriptBlockId, TranscriptDisplayState, TranscriptItem, TranscriptRenderKey,
    TranscriptViewSnapshot,
};
use coding_agent::api::settings::{
    CodingAgentResolvedColor, CodingAgentThemeBackground, CodingAgentThemeForeground,
    CodingAgentThemeSnapshot,
};

/// Resolved visual styles for transcript blocks, derived from a
/// [`CodingAgentThemeSnapshot`] (when available) or falling back to the built-in
/// palette constants otherwise. Mirrors the TS `theme.fg`/`theme.bg`
/// calls used by the interactive transcript components.
#[derive(Debug, Clone, Copy)]
pub(super) struct TranscriptStyles {
    pub user_text: Style,
    pub user_bg: Style,
    pub thinking: Style,
    pub system: Style,
    pub error: Style,
    pub tool_title: Style,
    pub tool_output: Style,
    pub tool_pending_bg: Style,
    pub tool_success_bg: Style,
    pub tool_error_bg: Style,
    pub tool_error_text: Style,
    pub tool_diff_added: Style,
    pub tool_diff_removed: Style,
    pub tool_diff_context: Style,
    pub bash_mode: Style,
    pub warning: Style,
    pub accent: Style,
}

impl TranscriptStyles {
    /// Resolve styles from an optional [`CodingAgentThemeSnapshot`]. When `None`
    /// (e.g. in unit tests without a loaded theme), falls back to the
    /// built-in tui palette constants so the transcript still renders
    /// with sensible defaults.
    pub(super) fn from_theme(resolved: Option<&CodingAgentThemeSnapshot>) -> Self {
        match resolved {
            Some(theme) => Self::from_resolved(theme),
            None => Self::fallback(),
        }
    }

    fn from_resolved(theme: &CodingAgentThemeSnapshot) -> Self {
        let fg = |token: CodingAgentThemeForeground| Style::fg(to_color(theme.foreground(token)));
        let bg = |token: CodingAgentThemeBackground| Style {
            fg: Color::Default,
            bg: to_color(theme.background(token)),
            bold: false,
            dim: false,
            italic: false,
            underline: false,
            strikethrough: false,
            reverse: false,
        };
        Self {
            user_text: fg(CodingAgentThemeForeground::UserMessageText),
            user_bg: bg(CodingAgentThemeBackground::UserMessage),
            thinking: fg(CodingAgentThemeForeground::ThinkingText).italic(),
            system: Style::fg(Color::Default).dim(),
            error: fg(CodingAgentThemeForeground::Error).bold(),
            tool_title: fg(CodingAgentThemeForeground::ToolTitle).bold(),
            tool_output: fg(CodingAgentThemeForeground::ToolOutput),
            tool_pending_bg: bg(CodingAgentThemeBackground::ToolPending),
            tool_success_bg: bg(CodingAgentThemeBackground::ToolSuccess),
            tool_error_bg: bg(CodingAgentThemeBackground::ToolError),
            tool_error_text: fg(CodingAgentThemeForeground::Error),
            tool_diff_added: fg(CodingAgentThemeForeground::ToolDiffAdded),
            tool_diff_removed: fg(CodingAgentThemeForeground::ToolDiffRemoved),
            tool_diff_context: fg(CodingAgentThemeForeground::ToolDiffContext),
            bash_mode: fg(CodingAgentThemeForeground::BashMode).bold(),
            warning: fg(CodingAgentThemeForeground::Warning),
            accent: fg(CodingAgentThemeForeground::Accent),
        }
    }

    fn fallback() -> Self {
        Self {
            user_text: USER,
            user_bg: Style::default(),
            thinking: Style::fg(Color::Yellow).italic(),
            system: SYSTEM,
            error: ERROR,
            tool_title: TOOL_NAME.bold(),
            tool_output: Style::default(),
            tool_pending_bg: Style::default(),
            tool_success_bg: Style::default(),
            tool_error_bg: Style::default(),
            tool_error_text: TOOL_ERROR,
            tool_diff_added: Style::fg(Color::Green),
            tool_diff_removed: Style::fg(Color::Red),
            tool_diff_context: Style::fg(Color::Default).dim(),
            bash_mode: Style::fg(Color::Green).bold(),
            warning: Style::fg(Color::Yellow),
            accent: Style::fg(Color::Cyan),
        }
    }
}

fn to_color(color: CodingAgentResolvedColor) -> Color {
    match color {
        CodingAgentResolvedColor::Default => Color::Default,
        CodingAgentResolvedColor::Rgb(red, green, blue) => Color::Rgb(red, green, blue),
        CodingAgentResolvedColor::Ansi256(value) => Color::Ansi256(value),
    }
}

/// Build a [`MarkdownTheme`] from a [`CodingAgentThemeSnapshot`], mirroring TS
/// `getMarkdownTheme()` (theme.ts). Each `md*` token maps to its resolved
/// color; `bold`/`italic`/`underline`/`strikethrough` are attribute-only
/// (fg=Default) to match TS `theme.bold`/`theme.italic`/... which inherit
/// the surrounding foreground rather than imposing a fixed color. No `.dim()`
/// is layered on — dark.json's `gray`/`dimGray` vars already carry the
/// intended lightness, and stacking `dim` would diverge from TS.
///
/// `highlight_code` is left `None`; the caller (root `markdown_theme()`)
/// mounts the syntax-highlight callback separately.
pub(super) fn markdown_theme_from_resolved(theme: &CodingAgentThemeSnapshot) -> MarkdownTheme {
    let fg = |token: CodingAgentThemeForeground| Style::fg(to_color(theme.foreground(token)));
    MarkdownTheme {
        heading: fg(CodingAgentThemeForeground::MdHeading).bold(),
        link: fg(CodingAgentThemeForeground::MdLink),
        link_url: fg(CodingAgentThemeForeground::MdLinkUrl),
        code: fg(CodingAgentThemeForeground::MdCode),
        code_block: fg(CodingAgentThemeForeground::MdCodeBlock),
        code_block_border: fg(CodingAgentThemeForeground::MdCodeBlockBorder),
        quote: fg(CodingAgentThemeForeground::MdQuote),
        quote_border: fg(CodingAgentThemeForeground::MdQuoteBorder),
        hr: fg(CodingAgentThemeForeground::MdHr),
        list_bullet: fg(CodingAgentThemeForeground::MdListBullet),
        bold: Style::fg(Color::Default).bold(),
        italic: Style::fg(Color::Default).italic(),
        underline: Style::fg(Color::Default).underline(),
        strikethrough: Style::fg(Color::Default).strikethrough(),
        highlight_code: None,
    }
}

/// All inputs to transcript block rendering, bundling width, color,
/// markdown theme, thinking visibility, and resolved [`TranscriptStyles`].
/// Mirrors the props threaded through TS `UserMessageComponent` /
/// `AssistantMessageComponent` / `ToolExecutionComponent`.
#[derive(Clone)]
pub(super) struct TranscriptRenderOptions<'a> {
    pub width: usize,
    pub max_tool_result_lines: usize,
    pub color: bool,
    pub markdown_theme: MarkdownTheme,
    pub hide_thinking_block: bool,
    pub hidden_thinking_label: &'a str,
    pub styles: TranscriptStyles,
    pub view: Option<Arc<TranscriptViewSnapshot>>,
    pub selected_block: Option<TranscriptBlockId>,
    pub selection_gutter: bool,
    pub show_images: bool,
    pub image_width_cells: u32,
    pub terminal_capabilities: TerminalCapabilities,
}

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
pub(super) struct TranscriptRowSnapshot {
    key: TranscriptRowMetadataKey,
    content_revision: u64,
    total_rows: usize,
}

impl TranscriptRowSnapshot {
    pub(super) fn total_rows(self) -> usize {
        self.total_rows
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct TranscriptBlockRows {
    pub total_rows: usize,
    pub start: usize,
    pub end: usize,
}

#[derive(Debug, Default)]
pub(super) struct TranscriptViewport {
    pub lines: Vec<String>,
    pub total_rows: usize,
    pub block_rows: Vec<(TranscriptBlockId, TranscriptBlockRows)>,
}

#[cfg(test)]
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(super) struct TranscriptRenderCacheStats {
    pub block_hits: usize,
    pub block_misses: usize,
    pub row_metadata_hits: usize,
    pub row_metadata_misses: usize,
    pub row_delta_hits: usize,
    pub row_delta_fallbacks: usize,
}

#[derive(Debug, Default)]
pub(super) struct TranscriptRenderCache {
    blocks: HashMap<TranscriptBlockCacheKey, TranscriptBlockCacheEntry>,
    row_metadata: HashMap<TranscriptRowMetadataKey, TranscriptRowMetadata>,
    #[cfg(test)]
    stats: TranscriptRenderCacheStats,
}

impl TranscriptRenderCache {
    pub(super) fn new() -> Self {
        Self::default()
    }

    pub(super) fn clear(&mut self) {
        self.blocks.clear();
        self.row_metadata.clear();
        #[cfg(test)]
        self.reset_stats();
    }

    pub(super) fn render_viewport(
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

    pub(super) fn row_snapshot(
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

    pub(super) fn row_delta_since(
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

    pub(super) fn block_rows(
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
    pub(super) fn reset_stats(&mut self) {
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

fn legacy_display_state(item: &TranscriptItem) -> TranscriptDisplayState {
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

#[cfg(test)]
pub(super) fn render_transcript_lines(
    transcript: &Transcript,
    opts: &TranscriptRenderOptions<'_>,
) -> Vec<String> {
    let TranscriptRenderOptions {
        width,
        max_tool_result_lines,
        color,
        markdown_theme,
        hide_thinking_block,
        hidden_thinking_label,
        styles,
        view,
        selected_block,
        selection_gutter,
        show_images,
        image_width_cells,
        terminal_capabilities,
    } = opts.clone();

    let mut lines = Vec::new();
    // Spacing policy: insert one blank line before every visible block except
    // the very first one. "Visible" excludes leading System welcome lines,
    // which keep their existing dim treatment. This replaces the old
    // ad-hoc "rule between finished tool and assistant" separator.
    let mut emitted_visible_block = false;

    for (render_key, item) in transcript.render_entries() {
        let block_id = render_key.block_id();
        let display_state = view.as_ref().map_or_else(
            || legacy_display_state(item),
            |view| view.display_state(block_id, item),
        );
        let tool_argument_state = view
            .as_ref()
            .map_or(TranscriptDisplayState::Collapsed, |view| {
                view.tool_argument_state(block_id, item)
            });
        let item_selection_gutter = selection_gutter;
        let block = render_block(
            item,
            width,
            max_tool_result_lines,
            color,
            &markdown_theme,
            hide_thinking_block,
            hidden_thinking_label,
            styles,
            display_state,
            tool_argument_state,
            transcript_image_id(render_key.transcript_id, render_key.item_id),
            item.selectable() && selected_block == Some(block_id),
            item_selection_gutter,
            show_images,
            image_width_cells,
            terminal_capabilities,
        );
        if block.is_empty() {
            continue;
        }
        let is_visible_block = !matches!(item, TranscriptItem::System { .. });
        if is_visible_block && emitted_visible_block {
            lines.push(String::new());
        }
        lines.extend(block);
        if is_visible_block {
            emitted_visible_block = true;
        }
    }

    lines
}

fn render_profile_hash(opts: &TranscriptRenderOptions<'_>) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    opts.width.hash(&mut hasher);
    opts.max_tool_result_lines.hash(&mut hasher);
    opts.color.hash(&mut hasher);
    opts.hide_thinking_block.hash(&mut hasher);
    opts.hidden_thinking_label.hash(&mut hasher);
    format!("{:?}", opts.markdown_theme).hash(&mut hasher);
    format!("{:?}", opts.styles).hash(&mut hasher);
    opts.show_images.hash(&mut hasher);
    opts.image_width_cells.hash(&mut hasher);
    format!("{:?}", opts.terminal_capabilities).hash(&mut hasher);
    hasher.finish()
}

fn render_row_profile_hash(opts: &TranscriptRenderOptions<'_>, profile_hash: u64) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    profile_hash.hash(&mut hasher);
    opts.view
        .as_ref()
        .map(|view| view.revision())
        .hash(&mut hasher);
    opts.selected_block.hash(&mut hasher);
    opts.selection_gutter.hash(&mut hasher);
    hasher.finish()
}

/// Render a single transcript item into zero or more lines. Each visible
/// item is a self-contained "block"; the caller inserts spacing between
/// blocks.
#[allow(clippy::too_many_arguments)]
fn render_block(
    item: &TranscriptItem,
    width: usize,
    max_tool_result_lines: usize,
    color: bool,
    markdown_theme: &MarkdownTheme,
    hide_thinking_block: bool,
    hidden_thinking_label: &str,
    styles: TranscriptStyles,
    display_state: TranscriptDisplayState,
    tool_argument_state: TranscriptDisplayState,
    image_id: u32,
    selected: bool,
    selection_gutter: bool,
    show_images: bool,
    image_width_cells: u32,
    terminal_capabilities: TerminalCapabilities,
) -> Vec<String> {
    let content_width = if selection_gutter {
        width.saturating_sub(2).max(1)
    } else {
        width
    };
    let lines = match item {
        TranscriptItem::User { text } => {
            render_user_message(text, content_width, color, markdown_theme, &styles)
        }
        TranscriptItem::System { text } => text
            .split('\n')
            .map(|line| fit_line(&paint_with(line, &styles.system, color), content_width))
            .collect(),
        TranscriptItem::Assistant {
            markdown, thinking, ..
        } => render_assistant_message(
            markdown,
            thinking,
            content_width,
            color,
            markdown_theme,
            hide_thinking_block,
            hidden_thinking_label,
            &styles,
            display_state,
        ),
        TranscriptItem::Tool {
            name,
            args,
            result,
            is_error,
            ..
        } => render_tool_block(
            name,
            args,
            result.as_deref(),
            *is_error,
            content_width,
            max_tool_result_lines,
            color,
            &styles,
            display_state,
            tool_argument_state,
            selection_gutter,
        ),
        TranscriptItem::Image { mime_type, data } => render_image_block(
            mime_type,
            data,
            content_width,
            show_images,
            image_width_cells,
            terminal_capabilities,
            image_id,
            &styles,
            color,
        ),
        TranscriptItem::Error { text } => render_error_message(text, content_width, color, &styles),
    };
    if selection_gutter {
        apply_selection_gutter(lines, width, selected)
    } else {
        lines
    }
}

fn transcript_image_id(transcript_id: u64, item_id: u64) -> u32 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    transcript_id.hash(&mut hasher);
    item_id.hash(&mut hasher);
    let id = hasher.finish() as u32;
    id.max(1)
}

#[allow(clippy::too_many_arguments)]
fn render_image_block(
    mime_type: &str,
    data: &str,
    width: usize,
    show_images: bool,
    image_width_cells: u32,
    terminal_capabilities: TerminalCapabilities,
    image_id: u32,
    styles: &TranscriptStyles,
    color: bool,
) -> Vec<String> {
    if !show_images {
        return vec![fit_line(
            &paint_with(&format!("[Image: {mime_type}]"), &styles.system, color),
            width,
        )];
    }
    let max_width = image_width_cells.max(1).min(width.max(1) as u32);
    let mut image = Image::new(data, mime_type)
        .capabilities(terminal_capabilities)
        .max_width_cells(max_width)
        .image_id(image_id);
    image.render(width)
}

fn apply_selection_gutter(lines: Vec<String>, width: usize, selected: bool) -> Vec<String> {
    lines
        .into_iter()
        .enumerate()
        .map(|(index, line)| {
            let marker = if selected && index == 0 { "▌ " } else { "  " };
            fit_line(&format!("{marker}{line}"), width)
        })
        .collect()
}

/// Render a user message as a backgrounded box (TS `UserMessageComponent`):
/// one padding row top/bottom, content padded left/right by one column,
/// painted with `userMessageBg` / `userMessageText`.
fn render_user_message(
    text: &str,
    width: usize,
    color: bool,
    markdown_theme: &MarkdownTheme,
    styles: &TranscriptStyles,
) -> Vec<String> {
    if width == 0 {
        return Vec::new();
    }
    // Inner content width after left/right padding (min 1).
    let padding_x = 1usize.min(width.saturating_sub(1) / 2);
    let content_width = width.saturating_sub(padding_x * 2).max(1);
    let left_pad = " ".repeat(padding_x);

    let mut content_lines = Vec::new();
    let mut md = Markdown::new(text).with_theme(markdown_theme.clone());
    for line in md.render(content_width) {
        content_lines.push(format!(
            "{left_pad}{}",
            paint_with(&line, &styles.user_text, color)
        ));
    }
    if content_lines.is_empty() {
        content_lines.push(left_pad.clone());
    }

    let mut lines = Vec::new();
    // Top padding row (background-filled blank line).
    lines.push(paint_bg_line("", width, &styles.user_bg, color));
    for line in content_lines {
        lines.push(paint_bg_line(&line, width, &styles.user_bg, color));
    }
    lines.push(paint_bg_line("", width, &styles.user_bg, color));
    lines
}

/// Render an assistant message (TS `AssistantMessageComponent`): no
/// background, optional thinking block, then markdown body at the common
/// transcript content origin. Thinking and body are separated by one blank line only when the
/// body has visible content.
#[allow(clippy::too_many_arguments)]
fn render_assistant_message(
    markdown: &str,
    thinking: &str,
    width: usize,
    color: bool,
    markdown_theme: &MarkdownTheme,
    hide_thinking_block: bool,
    hidden_thinking_label: &str,
    styles: &TranscriptStyles,
    display_state: TranscriptDisplayState,
) -> Vec<String> {
    let mut lines = Vec::new();
    let has_thinking = !thinking.trim().is_empty();
    let has_body = !markdown.trim().is_empty();

    if has_thinking {
        if hide_thinking_block {
            // Hidden thinking still surfaces a static label (TS behavior),
            // so users know reasoning happened without dumping its content.
            lines.push(fit_line(
                &paint_with(hidden_thinking_label, &styles.thinking, color),
                width,
            ));
        } else {
            let thinking_lines = thinking.lines().collect::<Vec<_>>();
            let (shown, omitted, label) = match display_state {
                TranscriptDisplayState::Collapsed => (
                    Vec::new(),
                    thinking_lines.len(),
                    format!("thinking · {} lines hidden", thinking_lines.len()),
                ),
                TranscriptDisplayState::Preview => {
                    let start = thinking_lines.len().saturating_sub(3);
                    (
                        thinking_lines[start..].to_vec(),
                        start,
                        "thinking · preview".to_string(),
                    )
                }
                TranscriptDisplayState::Expanded => {
                    (thinking_lines.clone(), 0, "thinking · expanded".to_string())
                }
            };
            lines.push(fit_line(&paint_with(&label, &styles.system, color), width));
            if omitted > 0 && display_state == TranscriptDisplayState::Preview {
                lines.push(fit_line(
                    &paint_with(
                        &format!("  ... {omitted} earlier thinking lines"),
                        &styles.system,
                        color,
                    ),
                    width,
                ));
            }
            let think_width = width.saturating_sub(2).max(1);
            for line in shown {
                let painted = paint_with(line, &styles.thinking, color);
                for wrapped in wrap_text_with_ansi(&painted, think_width) {
                    lines.push(fit_line(&format!("  {wrapped}"), width));
                }
            }
        }
        if has_body {
            lines.push(String::new());
        }
    }

    if has_body {
        let mut md = Markdown::new(markdown).with_theme(markdown_theme.clone());
        for line in md.render(width) {
            lines.push(fit_line(&line, width));
        }
    }

    lines
}

/// Render an error item with an `Error:` label (TS assistant-message error
/// fallback style).
///
/// Long errors wrap to the available transcript width (mirrors TS error
/// rendering) instead of being truncated to a single line. The `Error:` label
/// prefixes only the first rendered line; continuation lines wrap at column 0.
/// `fit_line` is kept as a final safety clamp so ANSI-bearing wrapped lines
/// can never overflow the width.
fn render_error_message(
    text: &str,
    width: usize,
    color: bool,
    styles: &TranscriptStyles,
) -> Vec<String> {
    let label = paint_with("Error:", &styles.error, color);
    // The first rendered line shares its row with the `Error: ` label (label
    // plus one separating space), so the first source line wraps to the
    // reduced width; later lines use the full width.
    let first_width = width.saturating_sub(visible_width("Error: ")).max(1);

    let mut out: Vec<String> = Vec::new();
    for source_line in text.split('\n') {
        let wrap_width = if out.is_empty() { first_width } else { width };
        for wrapped_line in wrap_text_with_ansi(source_line, wrap_width) {
            let body = paint_with(&wrapped_line, &styles.error, color);
            if out.is_empty() {
                out.push(fit_line(&format!("{label} {body}"), width));
            } else {
                out.push(fit_line(&body, width));
            }
        }
    }
    out
}

/// Paint a line with a background style, padding it to the full render
/// width so the background fills the row (mirrors the generic TUI `Box` background
/// handling). When color is disabled this collapses to a plain padded line,
/// so layout (spacing/indent) is preserved on colorless terminals.
///
/// `text` may already carry foreground ANSI codes (e.g. the user-message
/// text color). Those nested resets would normally drop the background for
/// the rest of the row, so when a background is applied we rewrite inner
/// `\x1b[0m` (full reset) to `\x1b[39m` (foreground-only reset, mirroring
/// TS `theme.bg` which closes with `\x1b[49m`). This keeps the background
/// span unbroken across the trailing padding.
fn paint_bg_line(text: &str, width: usize, bg: &Style, color: bool) -> String {
    let padded = pad_to_width(text, width);
    if !color || bg.bg == Color::Default {
        // No background to apply: keep the padded line verbatim (foreground
        // codes, if any, stay as-is).
        return padded;
    }
    // Rewrite inner full-resets so the background survives the content's
    // own foreground reset.
    let content = padded.replace("\x1b[0m", "\x1b[39m");
    let bg_style = Style {
        fg: Color::Default,
        bg: bg.bg,
        bold: false,
        dim: false,
        italic: false,
        underline: false,
        strikethrough: false,
        reverse: false,
    };
    paint_with(&content, &bg_style, color)
}

/// Pad `text` with trailing spaces to `width`, truncating if it overflows.
fn pad_to_width(text: &str, width: usize) -> String {
    let mut line = if visible_width(text) <= width {
        text.to_string()
    } else {
        truncate_to_width(text, width)
    };
    let line_width = visible_width(&line);
    if line_width < width {
        line.push_str(&" ".repeat(width - line_width));
    }
    line
}

#[allow(
    clippy::too_many_arguments,
    reason = "tool rendering keeps independent presentation controls explicit"
)]
fn render_tool_block(
    name: &str,
    args: &serde_json::Value,
    result: Option<&str>,
    is_error: bool,
    width: usize,
    max_tool_result_lines: usize,
    color: bool,
    styles: &TranscriptStyles,
    display_state: TranscriptDisplayState,
    tool_argument_state: TranscriptDisplayState,
    per_block_view: bool,
) -> Vec<String> {
    let status = match (result, is_error) {
        (None, _) => ToolStatus::Running,
        (Some(_), true) => ToolStatus::Error,
        (Some(_), false) => ToolStatus::Done,
    };
    let bg = match status {
        ToolStatus::Running => &styles.tool_pending_bg,
        ToolStatus::Error => &styles.tool_error_bg,
        ToolStatus::Done => &styles.tool_success_bg,
    };

    let header = render_tool_header(name, args, status, color, styles);
    if display_state == TranscriptDisplayState::Collapsed {
        if name == "edit" {
            return vec![fit_line(&header, width)];
        }
        return vec![paint_bg_line(&header, width, bg, color)];
    }
    let result_limit = match display_state {
        TranscriptDisplayState::Collapsed => 0,
        TranscriptDisplayState::Preview => max_tool_result_lines,
        TranscriptDisplayState::Expanded => usize::MAX,
    };

    // `edit` self-renders its diff (TS renderShell: "self") so the diff's
    // added/removed/context colors aren't swallowed by a flat tool bg.
    if name == "edit" {
        return render_edit_block(
            args,
            result,
            is_error,
            width,
            color,
            styles,
            result_limit,
            per_block_view,
        );
    }

    let mut lines = vec![paint_bg_line(&header, width, bg, color)];
    if per_block_view && tool_argument_state != TranscriptDisplayState::Collapsed {
        for line in render_tool_arguments(args, tool_argument_state, width, color, styles) {
            lines.push(paint_bg_line(&line, width, bg, color));
        }
    }
    if name == "delegation"
        && let Some(task) = string_arg(args, &["task"])
    {
        let task = paint_with(&format!("task: {task}"), &styles.tool_output, color);
        lines.push(paint_bg_line(&format!("  {task}"), width, bg, color));
    }
    let Some(result) = result else {
        // Bash shows a running hint while pending; other tools just stop.
        if name == "bash" {
            let hint = paint_with("Running...", &styles.system, color);
            lines.push(paint_bg_line(&format!("  {hint}"), width, bg, color));
        }
        return lines;
    };

    let body = render_tool_result_body(
        name,
        result,
        is_error,
        result_limit,
        color,
        styles,
        per_block_view,
    );
    for line in body {
        lines.push(paint_bg_line(&line, width, bg, color));
    }
    lines
}

#[derive(Clone, Copy)]
enum ToolStatus {
    Running,
    Done,
    Error,
}

impl ToolStatus {
    fn label(self) -> &'static str {
        match self {
            ToolStatus::Running => "running",
            ToolStatus::Done => "completed",
            ToolStatus::Error => "failed",
        }
    }
    fn style(self, styles: &TranscriptStyles) -> Style {
        match self {
            ToolStatus::Running => styles.warning,
            ToolStatus::Done => styles.tool_diff_added,
            ToolStatus::Error => styles.tool_error_text,
        }
    }
}

/// Render a tool's header line. Built-in tools get friendly, TS-parity
/// headers (`read <path>:range`, `$ <command>`, `edit <path>`); others fall
/// back to the generic `tool <name> <target> <status>` shape.
fn render_tool_header(
    name: &str,
    args: &serde_json::Value,
    status: ToolStatus,
    color: bool,
    styles: &TranscriptStyles,
) -> String {
    let status_text = paint_with(status.label(), &status.style(styles), color);
    match name {
        "read" => {
            let path = tool_target(name, args);
            let range = read_line_range(args, color, styles);
            format!(
                "{} {}{} {}",
                paint_with("read", &styles.tool_title, color),
                path,
                range,
                status_text,
            )
        }
        "bash" => {
            let command = tool_target(name, args);
            format!(
                "{} {}",
                paint_with(&format!("$ {command}"), &styles.bash_mode, color),
                status_text,
            )
        }
        "grep" => format!("{} {}", grep_header(args, color, styles), status_text),
        "find" => format!("{} {}", find_header(args, color, styles), status_text),
        "ls" => {
            let path = string_arg(args, &["path"]).unwrap_or_else(|| ".".to_string());
            format!(
                "{} {} {}",
                paint_with("ls", &styles.tool_title, color),
                path,
                status_text,
            )
        }
        "write" | "edit" => {
            let path = tool_target(name, args);
            format!(
                "{} {} {}",
                paint_with(name, &styles.tool_title, color),
                path,
                status_text,
            )
        }
        "delegation" => {
            let target_kind = string_arg(args, &["targetKind", "target_kind"])
                .unwrap_or_else(|| "agent".to_string());
            let target_id =
                string_arg(args, &["targetId", "target_id"]).unwrap_or_else(|| "-".to_string());
            let live_status =
                string_arg(args, &["status"]).unwrap_or_else(|| status.label().to_string());
            format!(
                "{} {} {} {}",
                paint_with("delegate", &styles.tool_title, color),
                target_kind,
                target_id,
                paint_with(&live_status, &status.style(styles), color),
            )
        }
        _ => format!(
            "{} {} {} {}",
            paint_with("tool", &styles.tool_title, color),
            paint_with(name, &styles.tool_title, color),
            tool_target(name, args),
            status_text,
        ),
    }
}

/// `:<start>-<end>` range suffix for `read`, mirroring TS
/// `formatReadLineRange`, in the warning color.
fn read_line_range(args: &serde_json::Value, color: bool, styles: &TranscriptStyles) -> String {
    let offset = args.get("offset").and_then(|v| v.as_u64());
    let limit = args.get("limit").and_then(|v| v.as_u64());
    if offset.is_none() && limit.is_none() {
        return String::new();
    }
    let start = offset.unwrap_or(1);
    let end = limit.map(|l| start + l - 1);
    let range = match end {
        Some(e) => format!(":{start}-{e}"),
        None => format!(":{start}"),
    };
    paint_with(&range, &styles.warning, color)
}

/// `grep /<pattern>/ in <path> (<glob>) limit <n>` header, mirroring TS
/// `formatGrepCall`. The pattern is accented; path/glob/limit use toolOutput.
fn grep_header(args: &serde_json::Value, color: bool, styles: &TranscriptStyles) -> String {
    let pattern = string_arg(args, &["pattern"]).unwrap_or_default();
    let path = string_arg(args, &["path"]).unwrap_or_else(|| ".".to_string());
    let glob = string_arg(args, &["glob"]);
    let limit = args.get("limit").and_then(|v| v.as_u64());
    let mut text = format!(
        "{} {}",
        paint_with("grep", &styles.tool_title, color),
        paint_with(&format!("/{pattern}/"), &styles.accent, color),
    );
    text.push_str(&paint_with(
        &format!(" in {path}"),
        &styles.tool_output,
        color,
    ));
    if let Some(glob) = glob {
        text.push_str(&paint_with(
            &format!(" ({glob})"),
            &styles.tool_output,
            color,
        ));
    }
    if let Some(limit) = limit {
        text.push_str(&paint_with(
            &format!(" limit {limit}"),
            &styles.tool_output,
            color,
        ));
    }
    text
}

/// `find <pattern> in <path> (limit <n>)` header, mirroring TS
/// `formatFindCall`. The pattern is accented; path/limit use toolOutput.
fn find_header(args: &serde_json::Value, color: bool, styles: &TranscriptStyles) -> String {
    let pattern = string_arg(args, &["pattern"]).unwrap_or_default();
    let path = string_arg(args, &["path"]).unwrap_or_else(|| ".".to_string());
    let limit = args.get("limit").and_then(|v| v.as_u64());
    let mut text = format!(
        "{} {}",
        paint_with("find", &styles.tool_title, color),
        paint_with(&pattern, &styles.accent, color),
    );
    text.push_str(&paint_with(
        &format!(" in {path}"),
        &styles.tool_output,
        color,
    ));
    if let Some(limit) = limit {
        text.push_str(&paint_with(
            &format!(" (limit {limit})"),
            &styles.tool_output,
            color,
        ));
    }
    text
}

/// Render a tool's result body (indented two columns). Built-in tools tailor
/// the preview: `read` replaces tabs and paints output; `bash` shows the
/// *tail* of the output (TS parity) and surfaces truncation notes; others use
/// the generic head-truncated preview.
fn render_tool_result_body(
    name: &str,
    result: &str,
    is_error: bool,
    max_tool_result_lines: usize,
    color: bool,
    styles: &TranscriptStyles,
    per_block_view: bool,
) -> Vec<String> {
    let output_style = if is_error {
        styles.tool_error_text
    } else {
        styles.tool_output
    };
    let all_lines: Vec<&str> = result.lines().collect();

    let keep_all = max_tool_result_lines == usize::MAX;
    let limit = max_tool_result_lines.min(all_lines.len());

    let (shown, omitted) = if name == "bash" && !keep_all {
        // Tail preview: show the last `limit` logical lines.
        let start = all_lines.len().saturating_sub(limit);
        (all_lines[start..].to_vec(), start)
    } else {
        (
            all_lines[..limit.min(all_lines.len())].to_vec(),
            all_lines.len().saturating_sub(limit),
        )
    };

    let mut out = Vec::new();
    for line in &shown {
        let text = if name == "read" {
            replace_tabs(line)
        } else {
            (*line).to_string()
        };
        let painted = paint_with(&text, &output_style, color);
        out.push(format!("  {painted}"));
    }
    if omitted > 0 {
        let note = paint_with(
            &format!(
                "... {omitted} more lines ({})",
                if per_block_view {
                    "disclose block"
                } else {
                    "expand tools"
                }
            ),
            &styles.system,
            color,
        );
        out.push(format!("  {note}"));
    }
    out
}

fn render_tool_arguments(
    args: &serde_json::Value,
    display_state: TranscriptDisplayState,
    width: usize,
    color: bool,
    styles: &TranscriptStyles,
) -> Vec<String> {
    let serialized = serde_json::to_string_pretty(args).unwrap_or_else(|_| args.to_string());
    let source_lines = serialized.lines().collect::<Vec<_>>();
    let limit = match display_state {
        TranscriptDisplayState::Collapsed => 0,
        TranscriptDisplayState::Preview => 3,
        TranscriptDisplayState::Expanded => usize::MAX,
    };
    let shown = source_lines.len().min(limit);
    let mut lines = vec![format!(
        "  {}",
        paint_with(
            match display_state {
                TranscriptDisplayState::Collapsed => "arguments · hidden",
                TranscriptDisplayState::Preview => "arguments · preview",
                TranscriptDisplayState::Expanded => "arguments · expanded",
            },
            &styles.system,
            color,
        )
    )];
    let content_width = width.saturating_sub(4).max(1);
    for source in source_lines.iter().take(shown) {
        let painted = paint_with(source, &styles.tool_output, color);
        for wrapped in wrap_text_with_ansi(&painted, content_width) {
            lines.push(format!("    {wrapped}"));
        }
    }
    let omitted = source_lines.len().saturating_sub(shown);
    if omitted > 0 {
        lines.push(format!(
            "    {}",
            paint_with(
                &format!("... {omitted} more argument lines"),
                &styles.system,
                color,
            )
        ));
    }
    lines
}

/// Self-rendered `edit` block: no tool bg, diff lines colored by
/// added/removed/context, mirroring TS `renderShell: "self"`.
#[allow(
    clippy::too_many_arguments,
    reason = "edit rendering keeps independent diff and disclosure controls explicit"
)]
fn render_edit_block(
    args: &serde_json::Value,
    result: Option<&str>,
    is_error: bool,
    width: usize,
    color: bool,
    styles: &TranscriptStyles,
    max_result_lines: usize,
    per_block_view: bool,
) -> Vec<String> {
    let path = tool_target("edit", args);
    let status = match (result, is_error) {
        (None, _) => ToolStatus::Running,
        (Some(_), true) => ToolStatus::Error,
        (Some(_), false) => ToolStatus::Done,
    };
    let header = format!(
        "{} {} {}",
        paint_with("edit", &styles.tool_title, color),
        path,
        paint_with(status.label(), &status.style(styles), color),
    );
    let mut lines = vec![fit_line(&header, width)];
    let Some(result) = result else {
        return lines;
    };

    let output_style = if is_error {
        styles.tool_error_text
    } else {
        styles.tool_output
    };
    let result_lines = result.lines().collect::<Vec<_>>();
    for line in result_lines.iter().take(max_result_lines) {
        let styled = paint_diff_line(line, color, styles, output_style);
        lines.push(fit_line(&format!("  {styled}"), width));
    }
    let omitted = result_lines.len().saturating_sub(max_result_lines);
    if omitted > 0 {
        lines.push(fit_line(
            &format!(
                "  {}",
                paint_with(
                    &format!(
                        "... {omitted} more diff lines ({})",
                        if per_block_view {
                            "disclose block"
                        } else {
                            "expand tools"
                        }
                    ),
                    &styles.system,
                    color
                )
            ),
            width,
        ));
    }
    lines
}

/// Paint a single diff line with semantic colors: `+` added, `-` removed,
/// ` ` context, and hunk headers (`@@`/`---`/`+++`) dimmed.
fn paint_diff_line(line: &str, color: bool, styles: &TranscriptStyles, fallback: Style) -> String {
    let (prefix, style) = if line.starts_with("+++") || line.starts_with("---") {
        (line, styles.tool_diff_context)
    } else if let Some(rest) = line.strip_prefix('+') {
        (rest, styles.tool_diff_added)
    } else if let Some(rest) = line.strip_prefix('-') {
        (rest, styles.tool_diff_removed)
    } else if line.starts_with("@@") {
        (line, styles.tool_diff_context)
    } else if let Some(rest) = line.strip_prefix(' ') {
        (rest, styles.tool_diff_context)
    } else {
        (line, fallback)
    };
    // Preserve the leading marker (stripped above) so the diff is still
    // readable on colorless terminals.
    let marker = if line.starts_with('+') {
        "+"
    } else if line.starts_with('-') {
        "-"
    } else if line.starts_with(' ') {
        " "
    } else {
        ""
    };
    format!("{}{}", marker, paint_with(prefix, &style, color))
}

/// Replace tabs with three spaces, mirroring TS `replaceTabs`.
fn replace_tabs(text: &str) -> String {
    text.replace('\t', "   ")
}

fn tool_target(name: &str, args: &serde_json::Value) -> String {
    match name {
        "bash" => string_arg(args, &["command", "cmd"]).unwrap_or_else(|| "-".to_string()),
        "read" | "write" | "edit" => {
            string_arg(args, &["path", "file_path", "filePath"]).unwrap_or_else(|| "-".to_string())
        }
        _ => string_arg(
            args,
            &["path", "file_path", "filePath", "command", "pattern"],
        )
        .unwrap_or_else(|| "-".to_string()),
    }
}

fn string_arg(args: &serde_json::Value, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| {
        args.get(*key)
            .and_then(|value| value.as_str())
            .filter(|value| !value.trim().is_empty())
            .map(ToString::to_string)
    })
}

pub(super) fn editor_border_line(width: usize, style: &Style, color: bool) -> String {
    if width == 0 {
        return String::new();
    }
    fit_line(&paint_with(&"─".repeat(width), style, color), width)
}

pub(super) fn fit_line(line: &str, width: usize) -> String {
    if visible_width(line) <= width {
        line.to_string()
    } else {
        truncate_to_width(line, width)
    }
}

pub(super) fn running_status_text(frame: usize) -> String {
    let mut loader = Loader::new("running");
    for _ in 0..frame {
        loader.tick();
    }
    loader.render_text()
}

pub(super) fn format_tokens(count: u32) -> String {
    if count < 1000 {
        count.to_string()
    } else if count < 10000 {
        format!("{:.1}k", count as f64 / 1000.0)
    } else if count < 1000000 {
        format!("{}k", count / 1000)
    } else if count < 10000000 {
        format!("{:.1}M", count as f64 / 1000000.0)
    } else {
        format!("{}M", count / 1000000)
    }
}

/// Warning style for the context-usage percentage (70–90% band), matching
/// the TypeScript footer's `theme.fg("warning", ...)`.
pub(super) const WARNING: Style = Style::fg(Color::Yellow);

pub(super) fn abbreviate_cwd(cwd: &Path) -> String {
    let display = cwd.display().to_string();
    if let Ok(home) = std::env::var("HOME")
        && !home.is_empty()
        && display.starts_with(&home)
    {
        return format!("~{}", &display[home.len()..]);
    }
    display
}

#[cfg(test)]
mod tests {
    use base64::Engine;

    use super::*;
    use crate::interactive::transcript::TranscriptViewState;
    use coding_agent::api::settings::CodingAgentThemeSnapshot;

    #[test]
    fn transcript_styles_fallback_when_no_theme() {
        let styles = TranscriptStyles::from_theme(None);
        // Without a resolved theme we fall back to the built-in palette
        // constants, so the transcript still renders with sensible defaults.
        assert_eq!(styles.user_text, USER);
        assert!(styles.thinking.italic);
        assert_eq!(styles.thinking.fg, Color::Yellow);
        assert_eq!(styles.error, ERROR);
        // Backgrounds collapse to default (no bg fill) in fallback mode.
        assert_eq!(styles.user_bg.bg, Color::Default);
        assert_eq!(styles.tool_pending_bg.bg, Color::Default);
    }

    #[test]
    fn transcript_styles_resolve_from_dark_theme() {
        let resolved = CodingAgentThemeSnapshot::dark();
        let styles = TranscriptStyles::from_theme(Some(&resolved));

        // userMessageText -> "text" var -> #d4d4d4
        assert_eq!(styles.user_text.fg, Color::Rgb(0xd4, 0xd4, 0xd4));
        // userMessageBg -> #343541
        assert_eq!(styles.user_bg.bg, Color::Rgb(0x34, 0x35, 0x41));
        // thinkingText -> "gray" var -> #808080, italic preserved
        assert_eq!(styles.thinking.fg, Color::Rgb(0x80, 0x80, 0x80));
        assert!(styles.thinking.italic);
        // toolPendingBg -> #282832
        assert_eq!(styles.tool_pending_bg.bg, Color::Rgb(0x28, 0x28, 0x32));
        // toolSuccessBg -> #283228
        assert_eq!(styles.tool_success_bg.bg, Color::Rgb(0x28, 0x32, 0x28));
        // toolErrorBg -> #3c2828
        assert_eq!(styles.tool_error_bg.bg, Color::Rgb(0x3c, 0x28, 0x28));
        // toolTitle bold
        assert!(styles.tool_title.bold);
        // tool diffs + bash + warning tokens
        assert_eq!(styles.tool_diff_added.fg, Color::Rgb(0xb5, 0xbd, 0x68));
        assert_eq!(styles.tool_diff_removed.fg, Color::Rgb(0xcc, 0x66, 0x66));
        assert_eq!(styles.bash_mode.fg, Color::Rgb(0xb5, 0xbd, 0x68));
        assert!(styles.bash_mode.bold);
        assert_eq!(styles.warning.fg, Color::Rgb(0xff, 0xff, 0x00));
    }

    #[test]
    fn transcript_roles_share_one_reserved_content_gutter() {
        fn content_column(lines: &[String], needle: &str) -> usize {
            let line = lines
                .iter()
                .find(|line| line.contains(needle))
                .unwrap_or_else(|| panic!("missing {needle}: {lines:#?}"));
            let byte = line.find(needle).unwrap();
            visible_width(&line[..byte])
        }

        let mut transcript = Transcript::new();
        transcript.push(TranscriptItem::system("System notice"));
        transcript.push(TranscriptItem::assistant(
            "assistant-gutter",
            "Assistant content",
            true,
        ));
        transcript.push(TranscriptItem::Tool {
            call_id: "tool-gutter".into(),
            name: "read".into(),
            args: serde_json::json!({"path": "src/main.rs"}),
            result: Some("done".into()),
            is_error: false,
        });
        transcript.push(TranscriptItem::error("gutter failure"));
        let mut opts = test_opts(72, false);
        opts.selection_gutter = true;
        opts.selected_block = None;

        let lines = render_transcript_lines(&transcript, &opts);
        for needle in [
            "System notice",
            "Assistant content",
            "read src/main.rs",
            "Error:",
        ] {
            assert_eq!(content_column(&lines, needle), 2, "{needle}: {lines:#?}");
        }
        assert!(
            lines.iter().any(|line| line.starts_with("  System notice")),
            "non-selectable System rows must reserve the same gutter: {lines:#?}"
        );
    }

    #[test]
    fn markdown_theme_uses_resolved_colors() {
        // Regression: `markdown_theme()` must derive its colors from the
        // ResolvedTheme (dark.json), not the tui palette (Ansi16/256 +
        // dim). Before the fix, assistant markdown bodies rendered with
        // `Ansi256(244)` + `dim` while user/tool blocks used vivid RGB from
        // the same dark.json — splitting the transcript into "dim text vs.
        // bright blocks". Now every md* token resolves through the theme,
        // so the whole transcript shares one palette.
        let resolved = CodingAgentThemeSnapshot::dark();
        let md = markdown_theme_from_resolved(&resolved);

        // mdHeading -> #f0c674 (not tui Cyan)
        assert_eq!(md.heading.fg, Color::Rgb(0xf0, 0xc6, 0x74));
        assert!(md.heading.bold);
        // mdCodeBlock -> green #b5bd68 (not Ansi256(244) + dim)
        assert_eq!(md.code_block.fg, Color::Rgb(0xb5, 0xbd, 0x68));
        assert!(!md.code_block.dim);
        // mdQuote -> gray #808080 (not Ansi256(244) + dim)
        assert_eq!(md.quote.fg, Color::Rgb(0x80, 0x80, 0x80));
        assert!(!md.quote.dim);
        // mdCode (inline) -> accent #8abeb7 (not Yellow)
        assert_eq!(md.code.fg, Color::Rgb(0x8a, 0xbe, 0xb7));
        // mdLink -> #81a2be (not Cyan)
        assert_eq!(md.link.fg, Color::Rgb(0x81, 0xa2, 0xbe));
        // mdHr -> gray #808080
        assert_eq!(md.hr.fg, Color::Rgb(0x80, 0x80, 0x80));
        // bold/italic/underline/strikethrough are attribute-only (fg=Default),
        // mirroring TS theme.bold/italic/underline (inherit surrounding fg).
        assert_eq!(md.bold.fg, Color::Default);
        assert!(md.bold.bold);
        assert_eq!(md.italic.fg, Color::Default);
        assert!(md.italic.italic);
        assert_eq!(md.underline.fg, Color::Default);
        assert!(md.underline.underline);
        assert_eq!(md.strikethrough.fg, Color::Default);
        assert!(md.strikethrough.strikethrough);
        // highlight_code is left for the caller to mount.
        assert!(md.highlight_code.is_none());
    }

    /// Build render options with no resolved theme (fallback palette) and
    /// the given color flag, for layout-focused assertions.
    fn test_opts(width: usize, color: bool) -> TranscriptRenderOptions<'static> {
        TranscriptRenderOptions {
            width,
            max_tool_result_lines: 3,
            color,
            markdown_theme: MarkdownTheme::default(),
            hide_thinking_block: false,
            hidden_thinking_label: "Thinking...",
            styles: TranscriptStyles::from_theme(None),
            view: None,
            selected_block: None,
            selection_gutter: false,
            show_images: true,
            image_width_cells: 60,
            terminal_capabilities: TerminalCapabilities {
                images: None,
                true_color: false,
                hyperlinks: false,
            },
        }
    }

    fn png_base64(width: u32, height: u32) -> String {
        let mut png = vec![0_u8; 24];
        png[0..8].copy_from_slice(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]);
        png[16..20].copy_from_slice(&width.to_be_bytes());
        png[20..24].copy_from_slice(&height.to_be_bytes());
        base64::engine::general_purpose::STANDARD.encode(png)
    }

    #[test]
    fn user_message_renders_as_backgrounded_box_not_bare_prefix() {
        // Plan stage 1: user message is a backgrounded box (TS
        // UserMessageComponent), not a bare `user: <text>` prefix. The box
        // has top/bottom padding rows and left/right content padding.
        let mut transcript = Transcript::new();
        transcript.push(TranscriptItem::user("hello"));

        let lines = render_transcript_lines(&transcript, &test_opts(20, false));
        // Top pad + content + bottom pad = 3 rows.
        assert_eq!(lines.len(), 3, "{lines:?}");
        // Content row carries the text with one-space left padding, no `user:`.
        assert!(
            !lines[1].contains("user:"),
            "bare prefix must go: {lines:?}"
        );
        assert!(lines[1].contains("hello"), "{lines:?}");
        // Every row is padded to the full width (background fill), and none
        // overflow it.
        for line in &lines {
            assert_eq!(visible_width(line), 20, "row must fill width: {lines:?}");
        }
    }

    #[test]
    fn user_message_background_fills_full_width_with_color() {
        // Regression: with color enabled and a real theme, the user-message
        // background must cover the full row width — including the trailing
        // padding after the content. The content's own foreground reset
        // (\x1b[0m) must not bleed into a full reset that drops the
        // background for the rest of the row (TS theme.bg uses \x1b[49m,
        // a background-only reset, so nesting stays clean).
        let mut transcript = Transcript::new();
        transcript.push(TranscriptItem::user("hi"));

        let resolved = CodingAgentThemeSnapshot::dark();
        let styles = TranscriptStyles::from_theme(Some(&resolved));
        let opts = TranscriptRenderOptions {
            width: 30,
            max_tool_result_lines: 3,
            color: true,
            markdown_theme: MarkdownTheme::default(),
            hide_thinking_block: false,
            hidden_thinking_label: "Thinking...",
            styles,
            view: None,
            selected_block: None,
            selection_gutter: false,
            show_images: true,
            image_width_cells: 60,
            terminal_capabilities: TerminalCapabilities {
                images: None,
                true_color: false,
                hyperlinks: false,
            },
        };
        let lines = render_transcript_lines(&transcript, &opts);

        // Every row must carry the userMessageBg background escape and end
        // with a reset, so the background spans the whole width.
        for (i, line) in lines.iter().enumerate() {
            assert!(
                line.starts_with("\x1b[48;2;52;53;65m"),
                "row {i} missing bg open: {line:?}"
            );
            assert!(
                line.ends_with("\x1b[0m"),
                "row {i} missing bg close: {line:?}"
            );
            assert_eq!(visible_width(line), 30, "row {i} not full width: {line:?}");
        }

        // The content row's trailing padding must stay inside the
        // background span: the content's inner reset must be a
        // foreground-only reset (\x1b[39m), NOT a full reset (\x1b[0m),
        // so the background opened at the start of the row covers the
        // trailing spaces all the way to the row's final reset.
        let content = &lines[1];
        let hi_pos = content.find("hi").expect("content present");
        let after_hi = &content[hi_pos + 2..];
        assert!(
            after_hi.starts_with("\x1b[39m"),
            "content reset should be foreground-only (\\x1b[39m), got: {content:?}"
        );
        // No full reset appears before the final row reset, so the bg span is
        // unbroken across the trailing padding.
        let inner = &content[..content.len() - "\x1b[0m".len()];
        assert_eq!(
            inner.matches("\x1b[0m").count(),
            0,
            "inner full reset would break the bg span: {content:?}"
        );
        assert_eq!(
            inner.matches("\x1b[48;2;52;53;65m").count(),
            1,
            "bg should open exactly once: {content:?}"
        );
    }

    #[test]
    fn visible_thinking_block_has_label_and_indented_content() {
        // Plan stage 1: thinking uses a `thinking` label and indented content
        // in thinkingText, distinguishing it from the assistant body.
        let mut transcript = Transcript::new();
        transcript.push(TranscriptItem::Assistant {
            id: "a".to_string(),
            markdown: "the answer".to_string(),
            thinking: "need to check".to_string(),
            done: true,
        });

        let lines = render_transcript_lines(&transcript, &test_opts(40, false));
        let joined = lines.join("\n");
        assert!(joined.contains("thinking"), "label missing: {joined}");
        assert!(
            joined.contains("  need to check"),
            "content not indented: {joined}"
        );
        // Body follows, separated by a blank line.
        assert!(joined.contains("the answer"), "body missing: {joined}");
        assert!(
            joined.contains("\n\n"),
            "no blank between thinking and body: {joined}"
        );
    }

    #[test]
    fn hidden_thinking_block_shows_static_label_instead_of_vanishing() {
        // Plan stage 1: when thinking is hidden, show `Thinking...` rather
        // than dropping the block entirely.
        let mut transcript = Transcript::new();
        transcript.push(TranscriptItem::Assistant {
            id: "a".to_string(),
            markdown: String::new(),
            thinking: "secret reasoning".to_string(),
            done: true,
        });

        let mut opts = test_opts(40, false);
        opts.hide_thinking_block = true;
        let lines = render_transcript_lines(&transcript, &opts);
        let joined = lines.join("\n");
        assert!(
            joined.contains("Thinking..."),
            "hidden label missing: {joined}"
        );
        assert!(
            !joined.contains("secret reasoning"),
            "content leaked when hidden: {joined}"
        );
    }

    #[test]
    fn long_thinking_lines_wrap_to_width_instead_of_truncating() {
        // Regression: thinking text must word-wrap at the available width
        // (width − 2 for the indent) rather than being truncated with
        // fit_line.  Before the fix, each source line was passed through
        // fit_line which *cuts* overflow without wrapping, so long thinking
        // content would just get clipped at the right edge.
        let long_thought = "this is a very long thinking line that absolutely must wrap to the available terminal width";
        let mut transcript = Transcript::new();
        transcript.push(TranscriptItem::Assistant {
            id: "a".to_string(),
            markdown: String::new(),
            thinking: long_thought.to_string(),
            done: true,
        });

        for (color, label) in [(false, "colorless"), (true, "colored")] {
            for width in [30, 20] {
                let lines = render_transcript_lines(&transcript, &test_opts(width, color));
                // First line is the "thinking" label.
                assert!(
                    lines[0].contains("thinking"),
                    "{label} w={width}: label missing"
                );
                let think_lines: Vec<_> =
                    lines[1..].iter().filter(|l| !l.trim().is_empty()).collect();
                // At narrow widths, we should get at least 2 thinking lines
                // (the text wraps), not just 1 truncated line.
                assert!(
                    think_lines.len() >= 2,
                    "{label} w={width}: expected at least 2 wrapped thinking lines, got {}: {think_lines:?}",
                    think_lines.len()
                );
                // Every word of the original must be present (no truncation loss).
                let joined = lines.join("\n");
                for word in long_thought.split_whitespace() {
                    assert!(
                        joined.contains(word),
                        "{label} w={width}: word `{word}` lost: {joined}"
                    );
                }
                // No line overflows width.
                for line in &lines {
                    assert!(
                        visible_width(line) <= width,
                        "{label} w={width} overflow: {:?}",
                        line
                    );
                }
            }
        }
    }

    #[test]
    fn blocks_are_separated_by_one_blank_line() {
        // Plan stage 1 spacing policy: every visible block (user, assistant,
        // tool, error) is separated from the previous one by exactly one
        // blank line.
        let mut transcript = Transcript::new();
        transcript.push(TranscriptItem::user("q"));
        transcript.push(TranscriptItem::assistant("a", "reply", true));

        let lines = render_transcript_lines(&transcript, &test_opts(40, false));
        // user box (3 rows) + blank + assistant body (1 row)
        assert_eq!(lines.len(), 5, "{lines:?}");
        assert_eq!(lines[3], "", "expected blank separator: {lines:?}");
    }

    #[test]
    fn no_line_overflows_render_width() {
        // Plan width contract: every rendered line must satisfy
        // visible_width(line) <= width, across color and narrow widths.
        let mut transcript = Transcript::new();
        transcript.push(TranscriptItem::user(
            "a fairly long user prompt that needs wrapping",
        ));
        transcript.push(TranscriptItem::Assistant {
            id: "a".to_string(),
            markdown: "# Title\n\nsome *markdown* body with a lot of text in it".to_string(),
            thinking: "thinking line that is also somewhat long".to_string(),
            done: true,
        });
        transcript.push(TranscriptItem::Tool {
            call_id: "c".to_string(),
            name: "read".to_string(),
            args: serde_json::json!({"path": "src/very/deeply/nested/path/file.rs"}),
            result: Some("line content here\nand more".to_string()),
            is_error: false,
        });
        transcript.push(TranscriptItem::Tool {
            call_id: "c".to_string(),
            name: "grep".to_string(),
            args: serde_json::json!({
                "pattern": "someLongRegexPattern",
                "path": "src/very/deep/nested/dir",
                "glob": "*.rs",
                "limit": 100
            }),
            result: Some("src/lib.rs:1: match".to_string()),
            is_error: false,
        });
        transcript.push(TranscriptItem::Tool {
            call_id: "c".to_string(),
            name: "find".to_string(),
            args: serde_json::json!({
                "pattern": "**/*.rs",
                "path": "crates/very/deeply/nested",
                "limit": 1000
            }),
            result: Some("crates/lib.rs".to_string()),
            is_error: false,
        });
        transcript.push(TranscriptItem::Tool {
            call_id: "c".to_string(),
            name: "ls".to_string(),
            args: serde_json::json!({"path": "src/very/deeply/nested/path"}),
            result: Some("file.rs".to_string()),
            is_error: false,
        });

        for (color, label) in [(false, "colorless"), (true, "colored")] {
            for width in [40, 20] {
                let lines = render_transcript_lines(&transcript, &test_opts(width, color));
                for line in &lines {
                    assert!(
                        visible_width(line) <= width,
                        "{label} width={width} overflow: {:?}",
                        line
                    );
                }
            }
        }
    }

    #[test]
    fn read_header_shows_path_and_line_range() {
        // Plan stage 3 read parity: header is `read <path>:<range>` (no
        // `tool` prefix), with the line range in the warning color.
        let mut transcript = Transcript::new();
        transcript.push(TranscriptItem::Tool {
            call_id: "c".to_string(),
            name: "read".to_string(),
            args: serde_json::json!({"path": "src/lib.rs", "offset": 10, "limit": 5}),
            result: Some("body".to_string()),
            is_error: false,
        });
        let lines = render_transcript_lines(&transcript, &test_opts(60, false));
        assert!(
            lines[0]
                .trim()
                .starts_with("read src/lib.rs:10-14 completed"),
            "{}",
            lines[0]
        );
    }

    #[test]
    fn bash_header_uses_dollar_prefix_and_running_hint() {
        // Plan stage 3 bash parity: header is `$ <command>`; while pending
        // show `Running...`.
        let mut transcript = Transcript::new();
        transcript.push(TranscriptItem::Tool {
            call_id: "c".to_string(),
            name: "bash".to_string(),
            args: serde_json::json!({"command": "cargo test"}),
            result: None,
            is_error: false,
        });
        let lines = render_transcript_lines(&transcript, &test_opts(60, false));
        assert!(
            lines[0].trim().starts_with("$ cargo test running"),
            "{}",
            lines[0]
        );
        assert!(lines[1].trim().starts_with("Running..."), "{}", lines[1]);
    }

    #[test]
    fn bash_result_shows_tail_preview_not_head() {
        // Plan stage 3 bash parity: collapsed view shows the *last* N lines
        // (tail), not the first N, so the most recent output stays visible.
        let mut transcript = Transcript::new();
        transcript.push(TranscriptItem::Tool {
            call_id: "c".to_string(),
            name: "bash".to_string(),
            args: serde_json::json!({"command": "echo"}),
            result: Some("l1\nl2\nl3\nl4\nl5\nl6".to_string()),
            is_error: false,
        });
        let lines = render_transcript_lines(&transcript, &test_opts(60, false));
        let body: Vec<String> = lines
            .iter()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .collect();
        assert!(
            body.iter().any(|l| l.starts_with("l6")),
            "tail must include l6: {body:?}"
        );
        assert!(
            body.iter().any(|l| l.starts_with("l4")),
            "tail must include l4: {body:?}"
        );
        assert!(
            !body.iter().any(|l| l.starts_with("l1")),
            "head l1 should be hidden: {body:?}"
        );
        assert!(
            body.iter().any(|l| l.contains("3 more lines")),
            "omitted hint missing: {body:?}"
        );
    }

    #[test]
    fn edit_block_self_renders_diff_with_semantic_colors() {
        // Plan stage 3 edit parity: edit self-renders (no tool bg), with
        // added/removed/context lines colored separately.
        let diff = "--- src/lib.rs\n+++ src/lib.rs\n@@ -1,2 +1,2 @@\n context\n-old\n+new";
        let mut transcript = Transcript::new();
        transcript.push(TranscriptItem::Tool {
            call_id: "c".to_string(),
            name: "edit".to_string(),
            args: serde_json::json!({"file_path": "src/lib.rs"}),
            result: Some(diff.to_string()),
            is_error: false,
        });

        let colored = render_transcript_lines(&transcript, &test_opts(60, true));
        let joined = colored.join("\n");
        // Header is `edit <path> done` with no `tool` prefix.
        assert!(joined.contains("src/lib.rs"), "path missing: {joined}");
        assert!(joined.contains("completed"), "status missing: {joined}");
        assert!(
            !joined.contains("tool edit"),
            "should not use generic prefix: {joined}"
        );
        // Added/removed lines carry their semantic color escapes (green/red).
        // toolDiffAdded = green = ANSI 2, toolDiffRemoved = red = ANSI 1.
        assert!(
            joined.contains("\x1b[32m"),
            "added line not green: {joined}"
        );
        assert!(
            joined.contains("\x1b[31m"),
            "removed line not red: {joined}"
        );
        // The `+new` / `-old` markers are preserved, with added/removed
        // content colored green/red respectively.
        assert!(
            joined.contains("\x1b[32mnew"),
            "added content not green: {joined}"
        );
        assert!(
            joined.contains("\x1b[31mold"),
            "removed content not red: {joined}"
        );
        assert!(
            joined.contains("+\x1b[32m"),
            "added marker missing: {joined}"
        );
        assert!(
            joined.contains("-\x1b[31m"),
            "removed marker missing: {joined}"
        );
    }

    #[test]
    fn grep_header_shows_pattern_path_glob_and_limit() {
        // Plan stage 4 grep parity: header surfaces pattern (accent), path,
        // glob, and limit, mirroring TS formatGrepCall.
        let mut transcript = Transcript::new();
        transcript.push(TranscriptItem::Tool {
            call_id: "c".to_string(),
            name: "grep".to_string(),
            args: serde_json::json!({
                "pattern": "TODO",
                "path": "src",
                "glob": "*.rs",
                "limit": 50
            }),
            result: Some("src/lib.rs:1: TODO".to_string()),
            is_error: false,
        });
        let lines = render_transcript_lines(&transcript, &test_opts(80, false));
        let header = lines[0].trim();
        assert!(header.starts_with("grep"), "no grep prefix: {header}");
        assert!(header.contains("/TODO/"), "pattern missing: {header}");
        assert!(header.contains("in src"), "path missing: {header}");
        assert!(header.contains("(*.rs)"), "glob missing: {header}");
        assert!(header.contains("limit 50"), "limit missing: {header}");
        assert!(header.contains("completed"), "status missing: {header}");
    }

    #[test]
    fn find_header_shows_pattern_path_and_limit() {
        // Plan stage 4 find parity: header surfaces pattern (accent), path,
        // and limit, mirroring TS formatFindCall.
        let mut transcript = Transcript::new();
        transcript.push(TranscriptItem::Tool {
            call_id: "c".to_string(),
            name: "find".to_string(),
            args: serde_json::json!({
                "pattern": "**/*.rs",
                "path": "crates",
                "limit": 100
            }),
            result: Some("crates/lib.rs".to_string()),
            is_error: false,
        });
        let lines = render_transcript_lines(&transcript, &test_opts(80, false));
        let header = lines[0].trim();
        assert!(header.starts_with("find"), "no find prefix: {header}");
        assert!(header.contains("**/*.rs"), "pattern missing: {header}");
        assert!(header.contains("in crates"), "path missing: {header}");
        assert!(header.contains("limit 100"), "limit missing: {header}");
    }

    #[test]
    fn ls_header_shows_path_defaulting_to_dot() {
        // Plan stage 4 ls parity: header is `ls <path>`, defaulting to `.`
        // when no path is given, mirroring TS formatLsCall.
        let mut transcript = Transcript::new();
        transcript.push(TranscriptItem::Tool {
            call_id: "c".to_string(),
            name: "ls".to_string(),
            args: serde_json::json!({}),
            result: Some("file.rs".to_string()),
            is_error: false,
        });
        let lines = render_transcript_lines(&transcript, &test_opts(40, false));
        let header = lines[0].trim();
        assert!(header.starts_with("ls ."), "default path missing: {header}");

        let mut transcript2 = Transcript::new();
        transcript2.push(TranscriptItem::Tool {
            call_id: "c".to_string(),
            name: "ls".to_string(),
            args: serde_json::json!({"path": "src"}),
            result: Some("lib.rs".to_string()),
            is_error: false,
        });
        let lines2 = render_transcript_lines(&transcript2, &test_opts(40, false));
        let header2 = lines2[0].trim();
        assert!(
            header2.starts_with("ls src"),
            "explicit path missing: {header2}"
        );
    }

    #[test]
    fn write_header_shows_path() {
        // Plan stage 4 write parity: header is `write <path>`.
        let mut transcript = Transcript::new();
        transcript.push(TranscriptItem::Tool {
            call_id: "c".to_string(),
            name: "write".to_string(),
            args: serde_json::json!({"path": "src/main.rs", "content": "fn main(){}"}),
            result: Some("Successfully wrote 12 bytes to src/main.rs".to_string()),
            is_error: false,
        });
        let lines = render_transcript_lines(&transcript, &test_opts(60, false));
        let header = lines[0].trim();
        assert!(
            header.starts_with("write src/main.rs completed"),
            "{}",
            header
        );
    }

    // ---- error message wrapping ----

    #[test]
    fn long_error_wraps_to_multiple_lines() {
        // A long single-line error must wrap to the transcript width instead
        // of being truncated to one line.
        let mut transcript = Transcript::new();
        let long_text = "summarization failed: complete failed: HTTP 400 unexpected provider response payload that is quite long indeed";
        transcript.push(TranscriptItem::error(long_text.to_string()));

        let lines = render_transcript_lines(&transcript, &test_opts(40, false));
        assert!(
            lines.len() > 1,
            "long error should wrap to multiple lines: {lines:?}"
        );
        // First line carries the Error: label.
        assert!(
            lines[0].starts_with("Error: "),
            "first line missing label: {:?}",
            lines[0]
        );
        // No rendered line overflows the width.
        for line in &lines {
            assert!(
                visible_width(line) <= 40,
                "line overflows width: {:?} ({})",
                line,
                visible_width(line)
            );
        }
        // Full text is recoverable: every word of the original appears across
        // the wrapped lines (no ANSI with color=false).
        let recovered = lines
            .iter()
            .map(|l| l.strip_prefix("Error: ").unwrap_or(l))
            .collect::<Vec<_>>()
            .join(" ");
        for word in long_text.split_whitespace() {
            assert!(
                recovered.contains(word),
                "missing word {word:?} in recovered text: {recovered:?}"
            );
        }
    }

    #[test]
    fn multi_line_error_preserves_newlines_and_wraps_each_paragraph() {
        // Explicit newlines in the error are preserved as paragraph breaks,
        // and each paragraph wraps within the width.
        let mut transcript = Transcript::new();
        let text = "first paragraph that is long enough to wrap across several lines here\nsecond paragraph also long enough to wrap nicely within the width";
        transcript.push(TranscriptItem::error(text.to_string()));

        let lines = render_transcript_lines(&transcript, &test_opts(30, false));
        let all = lines.join("\n");
        for word in text.split_whitespace() {
            assert!(all.contains(word), "missing word {word:?}: {all:?}");
        }
        for line in &lines {
            assert!(
                visible_width(line) <= 30,
                "overflow: {:?} ({})",
                line,
                visible_width(line)
            );
        }
        assert!(lines.len() > 2, "both paragraphs should wrap: {lines:?}");
        // Only the very first rendered line carries the Error: label.
        assert!(lines[0].starts_with("Error: "));
        assert!(
            lines.iter().filter(|l| l.starts_with("Error: ")).count() == 1,
            "exactly one label expected: {lines:?}"
        );
    }

    #[test]
    fn colored_error_keeps_style_on_all_wrapped_lines() {
        // With color enabled, the fallback ERROR style (bold red) must be
        // applied to every wrapped line, not just the first.
        let mut transcript = Transcript::new();
        let long_text = "summarization failed: complete failed: HTTP 400 unexpected provider response payload that is quite long";
        transcript.push(TranscriptItem::error(long_text.to_string()));

        let lines = render_transcript_lines(&transcript, &test_opts(40, true));
        assert!(lines.len() > 1, "should wrap: {lines:?}");
        for line in &lines {
            if !line.is_empty() {
                assert!(
                    line.contains("\x1b[1;31m"),
                    "error style missing on line: {line:?}"
                );
                assert!(line.contains("\x1b[0m"), "reset missing on line: {line:?}");
            }
            assert!(
                visible_width(line) <= 40,
                "overflow with color: {:?} ({})",
                line,
                visible_width(line)
            );
        }
    }

    #[test]
    fn per_block_tool_preview_expands_only_the_selected_tool() {
        let mut transcript = Transcript::new();
        transcript.push(TranscriptItem::Tool {
            call_id: "call-1".into(),
            name: "bash".into(),
            args: serde_json::json!({"command": "printf lines"}),
            result: Some("one\ntwo\nthree\nfour\nfive".into()),
            is_error: false,
        });
        let mut view = TranscriptViewState::default();
        view.sync(&transcript);
        let mut opts = test_opts(50, false);
        opts.view = Some(view.snapshot());
        opts.selected_block = view.selected();
        opts.selection_gutter = true;

        let preview = render_transcript_lines(&transcript, &opts).join("\n");
        assert!(preview.contains("▌ $ printf lines completed"), "{preview}");
        assert!(preview.contains("three"), "{preview}");
        assert!(preview.contains("five"), "{preview}");
        assert!(
            !preview.lines().any(|line| line.trim() == "one"),
            "{preview}"
        );
        assert!(
            preview.contains("2 more lines (disclose block)"),
            "{preview}"
        );

        assert!(view.toggle_selected(&transcript));
        opts.view = Some(view.snapshot());
        let expanded = render_transcript_lines(&transcript, &opts).join("\n");
        for line in ["one", "two", "three", "four", "five"] {
            assert!(expanded.contains(line), "missing {line}: {expanded}");
        }
        assert!(!expanded.contains("more lines"), "{expanded}");
    }

    #[test]
    fn assistant_disclosure_changes_thinking_without_hiding_answer() {
        let mut transcript = Transcript::new();
        transcript.push(TranscriptItem::Assistant {
            id: "assistant-1".into(),
            markdown: "final answer".into(),
            thinking: "think one\nthink two\nthink three\nthink four\nthink five".into(),
            done: true,
        });
        let mut view = TranscriptViewState::default();
        view.sync(&transcript);
        let mut opts = test_opts(50, false);
        opts.selected_block = view.selected();
        opts.selection_gutter = true;
        opts.view = Some(view.snapshot());

        let preview = render_transcript_lines(&transcript, &opts).join("\n");
        assert!(preview.contains("▌ thinking · preview"), "{preview}");
        assert!(!preview.contains("think one"), "{preview}");
        assert!(preview.contains("think five"), "{preview}");
        assert!(preview.contains("final answer"), "{preview}");

        assert!(view.toggle_selected(&transcript));
        opts.view = Some(view.snapshot());
        let expanded = render_transcript_lines(&transcript, &opts).join("\n");
        assert!(expanded.contains("think one"), "{expanded}");
        assert!(expanded.contains("final answer"), "{expanded}");

        assert!(view.toggle_selected(&transcript));
        opts.view = Some(view.snapshot());
        let collapsed = render_transcript_lines(&transcript, &opts).join("\n");
        assert!(collapsed.contains("5 lines hidden"), "{collapsed}");
        assert!(!collapsed.contains("think five"), "{collapsed}");
        assert!(collapsed.contains("final answer"), "{collapsed}");
    }

    #[test]
    fn delegation_preview_identifies_target_task_and_final_summary() {
        let mut transcript = Transcript::new();
        transcript.push(TranscriptItem::Tool {
            call_id: "delegate-1".into(),
            name: "delegation".into(),
            args: serde_json::json!({
                "targetKind": "agent",
                "targetId": "review",
                "task": "review the parser",
                "status": "completed"
            }),
            result: Some("No blocking issues.\nOne follow-up suggestion.".into()),
            is_error: false,
        });
        let mut view = TranscriptViewState::default();
        view.sync(&transcript);
        let mut opts = test_opts(70, false);
        opts.view = Some(view.snapshot());
        opts.selected_block = view.selected();
        opts.selection_gutter = true;

        let preview = render_transcript_lines(&transcript, &opts).join("\n");
        assert!(
            preview.contains("▌ delegate agent review completed"),
            "{preview}"
        );
        assert!(preview.contains("task: review the parser"), "{preview}");
        assert!(preview.contains("No blocking issues."), "{preview}");

        assert!(view.toggle_selected(&transcript));
        assert!(view.toggle_selected(&transcript));
        opts.view = Some(view.snapshot());
        let collapsed = render_transcript_lines(&transcript, &opts).join("\n");
        assert!(collapsed.contains("delegate agent review completed"));
        assert!(!collapsed.contains("review the parser"), "{collapsed}");
        assert!(!collapsed.contains("No blocking issues."), "{collapsed}");
    }

    #[test]
    fn generic_tool_arguments_default_to_bounded_preview() {
        let mut transcript = Transcript::new();
        transcript.push(TranscriptItem::Tool {
            call_id: "plugin-1".into(),
            name: "plugin.inspect".into(),
            args: serde_json::json!({
                "alpha": 1,
                "beta": 2,
                "gamma": 3,
                "delta": 4
            }),
            result: Some("done".into()),
            is_error: false,
        });
        let mut view = TranscriptViewState::default();
        view.sync(&transcript);
        let mut opts = test_opts(60, false);
        opts.view = Some(view.snapshot());
        opts.selected_block = view.selected();
        opts.selection_gutter = true;

        let preview = render_transcript_lines(&transcript, &opts).join("\n");
        assert!(preview.contains("arguments · preview"), "{preview}");
        assert!(preview.contains("\"alpha\": 1"), "{preview}");
        assert!(preview.contains("more argument lines"), "{preview}");

        assert!(view.toggle_selected_arguments(&transcript));
        opts.view = Some(view.snapshot());
        let expanded = render_transcript_lines(&transcript, &opts).join("\n");
        assert!(expanded.contains("arguments · expanded"), "{expanded}");
        assert!(expanded.contains("\"delta\": 4"), "{expanded}");
        assert!(!expanded.contains("more argument lines"), "{expanded}");
    }

    #[test]
    fn transcript_image_honors_visibility_capability_and_width_settings() {
        let mut transcript = Transcript::new();
        transcript.push(TranscriptItem::Image {
            mime_type: "image/png".into(),
            data: png_base64(18, 18),
        });
        let mut opts = test_opts(80, false);
        opts.image_width_cells = 10;
        opts.terminal_capabilities = TerminalCapabilities {
            images: Some(tui::api::terminal::ImageProtocol::Kitty),
            true_color: true,
            hyperlinks: true,
        };

        let rendered = render_transcript_lines(&transcript, &opts);
        assert_eq!(rendered.len(), 5, "{rendered:?}");
        assert!(rendered[0].starts_with("\x1b_G"), "{rendered:?}");
        assert!(rendered[0].contains("c=10,r=5,i="), "{rendered:?}");
        assert!(rendered[1..].iter().all(String::is_empty));

        opts.show_images = false;
        let hidden = render_transcript_lines(&transcript, &opts);
        assert_eq!(hidden, ["[Image: image/png]"]);
        assert!(!hidden[0].contains("\x1b_G"));

        opts.show_images = true;
        opts.terminal_capabilities.images = None;
        let fallback = render_transcript_lines(&transcript, &opts);
        assert_eq!(fallback.len(), 1);
        assert!(fallback[0].contains("image/png"), "{fallback:?}");
        assert!(!fallback[0].contains("\x1b_G"));
    }
}
