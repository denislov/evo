//! Bounded conversation, viewport, selection, and composer state.
//!
//! These reducers remain independent of GPUI. The renderer may virtualize the
//! resulting blocks without owning product transcript truth.

use std::borrow::Cow;
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};

use coding_agent::api::view::{CodingAgentSessionTranscriptItem, CodingAgentTranscriptSnapshot};
use unicode_width::UnicodeWidthStr as _;

use crate::shell::USER_MESSAGE_WIDTH_PERCENT;

mod copy;
mod markdown;

pub use copy::{MAX_COPY_BYTES, conversation_copy_text};
#[allow(unused_imports)]
pub use markdown::{
    MAX_CODE_BLOCK_PREVIEW_BYTES, MAX_MARKDOWN_LINE_BYTES, MAX_MARKDOWN_LINES,
    MAX_MARKDOWN_MARKERS_PER_LINE, MAX_MARKDOWN_NESTING, MAX_MARKDOWN_PREVIEW_BYTES,
    MAX_MARKDOWN_TABLE_CELLS, MAX_MARKDOWN_TABLE_ROWS, MarkdownPreview, bounded_markdown_preview,
};

pub const MAX_TRANSCRIPT_BLOCKS: usize = 10_000;
pub const MAX_TRANSCRIPT_BYTES: usize = 32 * 1024 * 1024;
pub const MAX_BLOCK_TEXT_BYTES: usize = 1024 * 1024;
pub const MAX_THINKING_TEXT_BYTES: usize = 512 * 1024;
pub const MAX_TOOL_ARGUMENT_BYTES: usize = 256 * 1024;
pub const MAX_COMPOSER_BYTES: usize = 1024 * 1024;
/// A reader who moves farther than this from the bottom owns the viewport.
pub const FOLLOW_LATEST_PAUSE_THRESHOLD_PX: f32 = 48.0;
/// A paused reader must return this close to the bottom before following resumes.
pub const FOLLOW_LATEST_RESUME_THRESHOLD_PX: f32 = 32.0;
pub const STREAMING_ROW_HEIGHT_INTERVAL: Duration = Duration::from_millis(67);
pub const STREAMING_MARKDOWN_SETTLE_DELAY: Duration = Duration::from_millis(100);
pub const MAX_SETTLING_MARKDOWN_BYTES: usize = 64 * 1024;
pub const CONVERSATION_WIDTH_BUCKET_PX: u32 = 24;
pub const MAX_ROW_RENDER_CACHE_ENTRIES: usize = MAX_TRANSCRIPT_BLOCKS + 256;
pub const MAX_ROW_RENDER_CACHE_BYTES: usize = 40 * 1024 * 1024;
/// Maximum height used only for an explicitly collapsed secondary-detail preview.
///
/// Normal conversation rows are not capped: their estimate is replaced by a
/// layout measurement from the rendered GPUI element.
pub const TRANSCRIPT_COLLAPSED_PREVIEW_MAX_HEIGHT: f32 = 680.0;

fn estimated_text_rows(text: &str, columns: usize, limit: usize) -> usize {
    if text.is_empty() {
        return 0;
    }
    let mut rows = 0usize;
    for line in text.lines().take(limit) {
        rows = rows.saturating_add(line.width().max(1).div_ceil(columns));
        if rows >= limit {
            return limit;
        }
    }
    rows.max(1).min(limit)
}

pub fn conversation_block_height(
    kind: ConversationBlockKind,
    text: &str,
    detail: &str,
    panel_width: u32,
) -> f32 {
    let effective_width = if kind == ConversationBlockKind::User {
        panel_width.saturating_mul(USER_MESSAGE_WIDTH_PERCENT) / 100
    } else {
        panel_width
    };
    let columns = (effective_width.saturating_sub(128) as usize / 8).max(24);
    let main_rows = estimated_text_rows(text, columns, 22);
    let detail_rows = estimated_text_rows(detail, columns.saturating_sub(4).max(20), 14);
    let chrome = match kind {
        ConversationBlockKind::Diagnostic => 58.0,
        ConversationBlockKind::User => 66.0,
        _ => 72.0,
    };
    let main_height = main_rows.max(1) as f32 * 22.0;
    let detail_height = if detail_rows == 0 {
        0.0
    } else if kind == ConversationBlockKind::Assistant {
        42.0 + detail_rows as f32 * 19.0
    } else {
        24.0 + detail_rows as f32 * 19.0
    };
    let minimum = match kind {
        ConversationBlockKind::Diagnostic => 86.0,
        ConversationBlockKind::User => 94.0,
        ConversationBlockKind::Tool => 106.0,
        _ => 110.0,
    };
    (chrome + main_height + detail_height).max(minimum)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ConversationBlockKind {
    User,
    Assistant,
    Tool,
    Delegation,
    CompactionSummary,
    BranchSummary,
    Diagnostic,
}

impl ConversationBlockKind {
    const fn key(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Assistant => "assistant",
            Self::Tool => "tool",
            Self::Delegation => "delegation",
            Self::CompactionSummary => "compaction",
            Self::BranchSummary => "branch",
            Self::Diagnostic => "diagnostic",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ConversationItemKind {
    Durable(ConversationBlockKind),
    Submitted,
    LiveMessage,
    LiveTool,
}

impl ConversationItemKind {
    const fn key(self) -> &'static str {
        match self {
            Self::Durable(kind) => kind.key(),
            Self::Submitted => "submitted",
            Self::LiveMessage => "live-message",
            Self::LiveTool => "live-tool",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ConversationItemKey {
    session_id: Arc<str>,
    kind: ConversationItemKind,
    row_id: Arc<str>,
    stable_id: Arc<str>,
}

impl ConversationItemKey {
    pub fn new(session_id: &str, kind: ConversationItemKind, row_id: &str) -> Self {
        Self {
            session_id: Arc::from(session_id),
            kind,
            row_id: Arc::from(row_id),
            stable_id: Arc::from(format!(
                "{}:{session_id}:{}:{}:{row_id}",
                session_id.len(),
                kind.key(),
                row_id.len()
            )),
        }
    }

    pub fn row_id(&self) -> &str {
        &self.row_id
    }

    pub fn stable_id(&self) -> &str {
        &self.stable_id
    }

    pub fn stable_id_arc(&self) -> Arc<str> {
        Arc::clone(&self.stable_id)
    }

    fn markdown_state_key(&self, detail: bool, revision: u64, final_state: bool) -> Arc<str> {
        let namespace = if detail {
            "transcript-detail-markdown"
        } else {
            "transcript-markdown"
        };
        let phase = if final_state { "final" } else { "settling" };
        Arc::from(format!("{namespace}:{}:{phase}:{revision}", self.stable_id))
    }

    fn retained_bytes(&self) -> usize {
        self.session_id.len() + self.row_id.len() + self.stable_id.len()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConversationBlock {
    pub id: String,
    /// Stable content revision computed once while hydrating product state.
    pub source_revision: u64,
    pub kind: ConversationBlockKind,
    pub title: String,
    pub text: String,
    pub detail: String,
    pub done: bool,
    pub is_error: bool,
    pub image_count: usize,
    pub reasoning_duration_millis: Option<u64>,
    pub truncated: bool,
}

impl ConversationBlock {
    pub fn copy_text(&self) -> String {
        conversation_copy_text(&self.text, &self.detail)
    }

    fn retained_bytes(&self) -> usize {
        self.id.len() + self.title.len() + self.text.len() + self.detail.len()
    }

    fn refresh_source_revision(&mut self) {
        self.source_revision = conversation_block_revision(self);
    }
}

#[derive(Debug)]
pub struct ConversationRowRenderSource<'a> {
    pub item_key: ConversationItemKey,
    pub source_revision: u64,
    pub title: Cow<'a, str>,
    pub text: &'a str,
    pub detail: &'a str,
    pub kind: ConversationBlockKind,
    pub done: bool,
    pub is_error: bool,
    pub image_count: usize,
    pub reasoning_duration_millis: Option<u64>,
    pub truncated: bool,
    pub durable: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamingTextPhase {
    StreamingPlainText,
    SettlingMarkdown,
    FinalMarkdown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConversationRowHeightSource {
    Estimated,
    Measured,
}

/// A row height observed after GPUI has laid out the actual conversation card.
///
/// Every presentation input that can change geometry is carried with the
/// result, allowing late prepaint callbacks to be rejected instead of
/// overwriting a newer streaming revision or a different responsive layout.
#[derive(Debug, Clone, PartialEq)]
pub struct ConversationRowMeasurement {
    pub item_key: ConversationItemKey,
    pub source_revision: u64,
    pub width_bucket: u32,
    pub text_phase: StreamingTextPhase,
    pub details_expanded: bool,
    pub height: f32,
}

/// Cheaply cloned render input for a conversation row.
///
/// Completed Markdown and its stable GPUI state keys remain frozen until the
/// source revision changes. Width changes only invalidate the measured height.
#[derive(Debug, Clone)]
pub struct ConversationRowRenderData {
    pub item_key: ConversationItemKey,
    pub source_revision: u64,
    pub sanitized_revision: u64,
    pub title: Arc<str>,
    pub text: Arc<str>,
    pub detail: Arc<str>,
    pub markdown_state_key: Arc<str>,
    pub detail_markdown_state_key: Arc<str>,
    pub text_phase: StreamingTextPhase,
    pub next_text_phase_after: Option<Duration>,
    pub kind: ConversationBlockKind,
    pub done: bool,
    pub is_error: bool,
    pub image_count: usize,
    pub reasoning_duration_millis: Option<u64>,
    pub preview_truncated: bool,
    pub media_neutralized: bool,
    pub durable: bool,
    pub width_bucket: u32,
    pub estimated_height: f32,
}

impl ConversationRowRenderData {
    fn retained_bytes(&self) -> usize {
        // `item_key` is also owned by the HashMap, so account for both copies.
        self.item_key.retained_bytes() * 2
            + self.title.len()
            + self.text.len()
            + self.detail.len()
            + self.markdown_state_key.len()
            + self.detail_markdown_state_key.len()
    }
}

#[derive(Debug)]
struct ConversationRowRenderCacheEntry {
    data: ConversationRowRenderData,
    retained_bytes: usize,
    touched_generation: u64,
    source_updated_at: Instant,
}

/// Revision-aware, memory-bounded cache for transcript row presentation.
#[derive(Debug)]
pub struct ConversationRowRenderCache {
    entries: HashMap<ConversationItemKey, ConversationRowRenderCacheEntry>,
    retained_bytes: usize,
    generation: u64,
    max_entries: usize,
    max_retained_bytes: usize,
    #[cfg(test)]
    sanitization_count: usize,
}

impl Default for ConversationRowRenderCache {
    fn default() -> Self {
        Self {
            entries: HashMap::new(),
            retained_bytes: 0,
            generation: 0,
            max_entries: MAX_ROW_RENDER_CACHE_ENTRIES,
            max_retained_bytes: MAX_ROW_RENDER_CACHE_BYTES,
            #[cfg(test)]
            sanitization_count: 0,
        }
    }
}

impl ConversationRowRenderCache {
    pub fn begin_frame(&mut self) {
        self.generation = self.generation.wrapping_add(1).max(1);
    }

    pub fn resolve(
        &mut self,
        source: ConversationRowRenderSource<'_>,
        width_bucket: u32,
    ) -> ConversationRowRenderData {
        self.resolve_at(source, width_bucket, Instant::now())
    }

    fn resolve_at(
        &mut self,
        source: ConversationRowRenderSource<'_>,
        width_bucket: u32,
        now: Instant,
    ) -> ConversationRowRenderData {
        if let Some(entry) = self.entries.get_mut(&source.item_key)
            && entry.data.source_revision > source.source_revision
        {
            entry.touched_generation = self.generation;
            return entry.data.clone();
        }
        if let Some(entry) = self.entries.get_mut(&source.item_key)
            && entry.data.source_revision == source.source_revision
            && entry.data.sanitized_revision == source.source_revision
            && entry.data.done == source.done
        {
            entry.touched_generation = self.generation;
            if !source.done
                && entry.data.text_phase == StreamingTextPhase::StreamingPlainText
                && entry.data.next_text_phase_after.is_some()
            {
                let elapsed = now.saturating_duration_since(entry.source_updated_at);
                if elapsed >= STREAMING_MARKDOWN_SETTLE_DELAY {
                    entry.data.text_phase = StreamingTextPhase::SettlingMarkdown;
                    entry.data.next_text_phase_after = None;
                } else {
                    entry.data.next_text_phase_after =
                        Some(STREAMING_MARKDOWN_SETTLE_DELAY.saturating_sub(elapsed));
                }
            }
            if entry.data.width_bucket != width_bucket {
                entry.data.width_bucket = width_bucket;
                entry.data.estimated_height = conversation_block_height(
                    entry.data.kind,
                    &entry.data.text,
                    &entry.data.detail,
                    width_bucket,
                );
            }
            return entry.data.clone();
        }

        let (text, detail, preview_truncated, media_neutralized) = if source.done {
            #[cfg(test)]
            {
                self.sanitization_count = self.sanitization_count.saturating_add(1);
            }
            let text = bounded_markdown_preview(source.text);
            let detail = bounded_markdown_preview(source.detail);
            (
                Arc::<str>::from(text.text),
                Arc::<str>::from(detail.text),
                source.truncated || text.truncated || detail.truncated,
                text.media_neutralized || detail.media_neutralized,
            )
        } else {
            (
                Arc::<str>::from(source.text),
                Arc::<str>::from(source.detail),
                source.truncated,
                false,
            )
        };
        let data = ConversationRowRenderData {
            markdown_state_key: source.item_key.markdown_state_key(
                false,
                source.source_revision,
                source.done,
            ),
            detail_markdown_state_key: source.item_key.markdown_state_key(
                true,
                source.source_revision,
                source.done,
            ),
            text_phase: if source.done {
                StreamingTextPhase::FinalMarkdown
            } else {
                StreamingTextPhase::StreamingPlainText
            },
            next_text_phase_after: (!source.done
                && source.text.len().saturating_add(source.detail.len())
                    <= MAX_SETTLING_MARKDOWN_BYTES)
                .then_some(STREAMING_MARKDOWN_SETTLE_DELAY),
            item_key: source.item_key.clone(),
            source_revision: source.source_revision,
            sanitized_revision: source.source_revision,
            title: Arc::from(source.title.as_ref()),
            estimated_height: conversation_block_height(source.kind, &text, &detail, width_bucket),
            text,
            detail,
            kind: source.kind,
            done: source.done,
            is_error: source.is_error,
            image_count: source.image_count,
            reasoning_duration_millis: source.reasoning_duration_millis,
            preview_truncated,
            media_neutralized,
            durable: source.durable,
            width_bucket,
        };
        let retained_bytes = data.retained_bytes();
        let entry = ConversationRowRenderCacheEntry {
            data: data.clone(),
            retained_bytes,
            touched_generation: self.generation,
            source_updated_at: now,
        };
        if let Some(previous) = self.entries.insert(source.item_key, entry) {
            self.retained_bytes = self.retained_bytes.saturating_sub(previous.retained_bytes);
        }
        self.retained_bytes = self.retained_bytes.saturating_add(retained_bytes);
        data
    }

    pub fn finish_frame(&mut self) {
        let generation = self.generation;
        self.retain(|entry| entry.touched_generation == generation);
        self.enforce_bounds();
    }

    /// Finish a partial row update without treating untouched transcript rows
    /// as stale. Full replacement frames use `finish_frame` to sweep sessions.
    pub fn finish_incremental(&mut self) {
        self.enforce_bounds();
    }

    fn enforce_bounds(&mut self) {
        while self.entries.len() > self.max_entries || self.retained_bytes > self.max_retained_bytes
        {
            let Some(key) = self
                .entries
                .iter()
                .min_by_key(|(_, entry)| entry.touched_generation)
                .map(|(key, _)| key.clone())
            else {
                break;
            };
            self.remove(&key);
        }
    }

    fn retain(&mut self, mut predicate: impl FnMut(&ConversationRowRenderCacheEntry) -> bool) {
        self.entries.retain(|_, entry| {
            let keep = predicate(entry);
            if !keep {
                self.retained_bytes = self.retained_bytes.saturating_sub(entry.retained_bytes);
            }
            keep
        });
    }

    fn remove(&mut self, key: &ConversationItemKey) {
        if let Some(entry) = self.entries.remove(key) {
            self.retained_bytes = self.retained_bytes.saturating_sub(entry.retained_bytes);
        }
    }

    #[cfg(test)]
    fn with_limits(max_entries: usize, max_retained_bytes: usize) -> Self {
        Self {
            max_entries,
            max_retained_bytes,
            ..Self::default()
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConversationProjection {
    pub session_id: String,
    pub active_leaf_id: Option<String>,
    blocks: VecDeque<ConversationBlock>,
    omitted_blocks: usize,
    retained_bytes: usize,
}

impl ConversationProjection {
    pub fn hydrate(snapshot: CodingAgentTranscriptSnapshot) -> Self {
        let mut projection = Self {
            session_id: snapshot.session_id,
            active_leaf_id: snapshot.active_leaf_id,
            blocks: VecDeque::new(),
            omitted_blocks: 0,
            retained_bytes: 0,
        };
        for (index, item) in snapshot.items.into_iter().enumerate() {
            projection.push_bounded(block_from_product(index, item));
        }
        projection
    }

    pub fn blocks(&self) -> &VecDeque<ConversationBlock> {
        &self.blocks
    }

    pub const fn omitted_blocks(&self) -> usize {
        self.omitted_blocks
    }

    #[cfg(test)]
    pub const fn retained_bytes(&self) -> usize {
        self.retained_bytes
    }

    pub fn block(&self, id: &str) -> Option<&ConversationBlock> {
        self.blocks.iter().find(|block| block.id == id)
    }

    fn push_bounded(&mut self, block: ConversationBlock) {
        if block.kind == ConversationBlockKind::Diagnostic {
            let merged_bytes = self.blocks.back_mut().and_then(|previous| {
                let equivalent = previous.kind == ConversationBlockKind::Diagnostic
                    && diagnostic_equivalence_key(&previous.text)
                        == diagnostic_equivalence_key(&block.text);
                if !equivalent {
                    return None;
                }

                let before = previous.retained_bytes();
                previous.title = next_diagnostic_title(&previous.title);
                previous.truncated |= block.truncated;
                previous.refresh_source_revision();
                Some((before, previous.retained_bytes()))
            });
            if let Some((before, after)) = merged_bytes {
                self.retained_bytes = self
                    .retained_bytes
                    .saturating_sub(before)
                    .saturating_add(after);
                return;
            }
        }

        self.retained_bytes = self.retained_bytes.saturating_add(block.retained_bytes());
        self.blocks.push_back(block);
        while self.blocks.len() > MAX_TRANSCRIPT_BLOCKS
            || self.retained_bytes > MAX_TRANSCRIPT_BYTES
        {
            let Some(evicted) = self.blocks.pop_front() else {
                break;
            };
            self.retained_bytes = self.retained_bytes.saturating_sub(evicted.retained_bytes());
            self.omitted_blocks = self.omitted_blocks.saturating_add(1);
        }
    }
}

fn diagnostic_equivalence_key(message: &str) -> &str {
    message
        .strip_prefix("provider error: ")
        .unwrap_or(message)
        .trim()
}

fn next_diagnostic_title(title: &str) -> String {
    let count = title
        .strip_prefix("Diagnostic · ")
        .and_then(|value| value.strip_suffix(" related events"))
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(1)
        .saturating_add(1);
    format!("Diagnostic · {count} related events")
}

pub const fn conversation_width_bucket(panel_width: u32) -> u32 {
    let bucket = panel_width / CONVERSATION_WIDTH_BUCKET_PX;
    if bucket == 0 {
        CONVERSATION_WIDTH_BUCKET_PX
    } else {
        bucket * CONVERSATION_WIDTH_BUCKET_PX
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ConversationRowLayoutInput {
    pub item_key: ConversationItemKey,
    pub source_revision: u64,
    pub text_phase: StreamingTextPhase,
    pub details_expanded: bool,
    pub estimated_height: f32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ConversationRowLayoutResolution {
    pub heights: Vec<f32>,
    pub paused_scroll_top: Option<f32>,
    pub next_refresh_after: Option<Duration>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ConversationRowLayoutSingleResolution {
    pub height: f32,
    pub source: ConversationRowHeightSource,
    pub height_changed: bool,
    pub next_refresh_after: Option<Duration>,
}

#[derive(Debug, Clone)]
struct ConversationRowHeight {
    committed: f32,
    estimate: f32,
    measured: Option<f32>,
    source_revision: u64,
    width_bucket: u32,
    text_phase: StreamingTextPhase,
    details_expanded: bool,
    last_commit_at: Instant,
}

#[derive(Debug, Default)]
pub struct ConversationRowLayoutState {
    rows: HashMap<String, ConversationRowHeight>,
    order: Vec<String>,
    #[cfg(test)]
    full_input_visits: usize,
    #[cfg(test)]
    single_row_updates: usize,
}

impl ConversationRowLayoutState {
    pub fn resolve_one(
        &mut self,
        input: ConversationRowLayoutInput,
        width_bucket: u32,
        now: Instant,
    ) -> ConversationRowLayoutSingleResolution {
        let _span = tracing::trace_span!(
            "desktop.list.height_update",
            width_bucket,
            streaming = input.text_phase == StreamingTextPhase::StreamingPlainText
        )
        .entered();
        #[cfg(test)]
        {
            self.single_row_updates = self.single_row_updates.saturating_add(1);
        }
        let key = input.item_key.stable_id().to_owned();
        let estimate = sanitize_row_height(input.estimated_height);
        let is_new = !self.rows.contains_key(&key);
        let row = self
            .rows
            .entry(key.clone())
            .or_insert(ConversationRowHeight {
                committed: estimate,
                estimate,
                measured: None,
                source_revision: input.source_revision,
                width_bucket,
                text_phase: input.text_phase,
                details_expanded: input.details_expanded,
                last_commit_at: now,
            });
        let width_changed = row.width_bucket != width_bucket;
        let phase_changed = row.text_phase != input.text_phase;
        let details_changed = row.details_expanded != input.details_expanded;
        let revision_changed = row.source_revision != input.source_revision;
        if width_changed || phase_changed || details_changed || revision_changed {
            row.measured = None;
        }
        row.estimate = estimate;
        let target_height = row.measured.unwrap_or(row.estimate);
        let target_changed = (row.committed - target_height).abs() > f32::EPSILON;
        let mut next_refresh_after = None;
        if target_changed {
            let elapsed = now
                .checked_duration_since(row.last_commit_at)
                .unwrap_or_default();
            if width_changed
                || phase_changed
                || details_changed
                || input.text_phase != StreamingTextPhase::StreamingPlainText
                || elapsed >= STREAMING_ROW_HEIGHT_INTERVAL
            {
                row.committed = target_height;
                row.last_commit_at = now;
            } else {
                next_refresh_after = Some(STREAMING_ROW_HEIGHT_INTERVAL.saturating_sub(elapsed));
            }
        }
        row.source_revision = input.source_revision;
        row.width_bucket = width_bucket;
        row.text_phase = input.text_phase;
        row.details_expanded = input.details_expanded;
        if is_new {
            self.order.push(key);
        }
        ConversationRowLayoutSingleResolution {
            height: row.committed,
            source: if row
                .measured
                .is_some_and(|height| (row.committed - height).abs() <= 0.5)
            {
                ConversationRowHeightSource::Measured
            } else {
                ConversationRowHeightSource::Estimated
            },
            height_changed: target_changed && (row.committed - target_height).abs() <= f32::EPSILON,
            next_refresh_after,
        }
    }

    /// Submit one actual GPUI row measurement without revisiting historical rows.
    /// Returns `None` when the callback belongs to stale presentation input.
    pub fn submit_measurement(
        &mut self,
        measurement: &ConversationRowMeasurement,
        now: Instant,
    ) -> Option<ConversationRowLayoutSingleResolution> {
        let _span = tracing::trace_span!(
            "desktop.row_measure",
            source_revision = measurement.source_revision,
            width_bucket = measurement.width_bucket,
            height = measurement.height,
        )
        .entered();
        if !measurement.height.is_finite() || measurement.height <= 0. {
            tracing::trace!(target: "desktop", event = "row_measure_stale_drop", reason = "invalid");
            return None;
        }
        let key = measurement.item_key.stable_id();
        let Some(row) = self.rows.get_mut(key) else {
            tracing::trace!(target: "desktop", event = "row_measure_stale_drop", reason = "missing");
            return None;
        };
        if row.source_revision != measurement.source_revision
            || row.width_bucket != measurement.width_bucket
            || row.text_phase != measurement.text_phase
            || row.details_expanded != measurement.details_expanded
        {
            tracing::trace!(target: "desktop", event = "row_measure_stale_drop", reason = "identity");
            return None;
        }

        let measured = sanitize_row_height(measurement.height);
        row.measured = Some(measured);
        let target_changed = (row.committed - measured).abs() > 0.5;
        let mut next_refresh_after = None;
        let mut height_changed = false;
        if target_changed {
            let elapsed = now
                .checked_duration_since(row.last_commit_at)
                .unwrap_or_default();
            if row.text_phase != StreamingTextPhase::StreamingPlainText
                || elapsed >= STREAMING_ROW_HEIGHT_INTERVAL
            {
                row.committed = measured;
                row.last_commit_at = now;
                height_changed = true;
                tracing::trace!(target: "desktop", event = "row_height_commit", height = measured);
            } else {
                next_refresh_after = Some(STREAMING_ROW_HEIGHT_INTERVAL.saturating_sub(elapsed));
            }
        }

        Some(ConversationRowLayoutSingleResolution {
            height: row.committed,
            source: if (row.committed - measured).abs() <= 0.5 {
                ConversationRowHeightSource::Measured
            } else {
                ConversationRowHeightSource::Estimated
            },
            height_changed,
            next_refresh_after,
        })
    }

    pub fn resolve(
        &mut self,
        inputs: Vec<ConversationRowLayoutInput>,
        width_bucket: u32,
        now: Instant,
        paused_scroll_top: Option<f32>,
    ) -> ConversationRowLayoutResolution {
        let _span = tracing::trace_span!(
            "desktop.list.layout",
            width_bucket,
            row_count = inputs.len()
        )
        .entered();
        #[cfg(test)]
        {
            self.full_input_visits = self.full_input_visits.saturating_add(inputs.len());
        }
        let anchor = paused_scroll_top.and_then(|scroll_top| self.anchor_at(scroll_top));
        let mut previous_rows = std::mem::take(&mut self.rows);
        let mut next_rows = HashMap::with_capacity(inputs.len());
        let mut next_order = Vec::with_capacity(inputs.len());
        let mut heights = Vec::with_capacity(inputs.len());
        let mut next_refresh_after: Option<Duration> = None;

        for input in inputs {
            let key = input.item_key.stable_id().to_owned();
            let estimate = sanitize_row_height(input.estimated_height);
            let mut row = previous_rows.remove(&key).unwrap_or(ConversationRowHeight {
                committed: estimate,
                estimate,
                measured: None,
                source_revision: input.source_revision,
                width_bucket,
                text_phase: input.text_phase,
                details_expanded: input.details_expanded,
                last_commit_at: now,
            });

            let width_changed = row.width_bucket != width_bucket;
            let phase_changed = row.text_phase != input.text_phase;
            let details_changed = row.details_expanded != input.details_expanded;
            let revision_changed = row.source_revision != input.source_revision;
            if width_changed || phase_changed || details_changed || revision_changed {
                row.measured = None;
            }
            row.estimate = estimate;
            let target_height = row.measured.unwrap_or(row.estimate);
            let target_changed = (row.committed - target_height).abs() > f32::EPSILON;
            if target_changed {
                let elapsed = now
                    .checked_duration_since(row.last_commit_at)
                    .unwrap_or_default();
                if width_changed
                    || phase_changed
                    || details_changed
                    || input.text_phase != StreamingTextPhase::StreamingPlainText
                    || elapsed >= STREAMING_ROW_HEIGHT_INTERVAL
                {
                    row.committed = target_height;
                    row.last_commit_at = now;
                } else {
                    let remaining = STREAMING_ROW_HEIGHT_INTERVAL.saturating_sub(elapsed);
                    next_refresh_after = Some(
                        next_refresh_after.map_or(remaining, |scheduled| scheduled.min(remaining)),
                    );
                }
            }

            row.source_revision = input.source_revision;
            row.width_bucket = width_bucket;
            row.text_phase = input.text_phase;
            row.details_expanded = input.details_expanded;
            heights.push(row.committed);
            next_order.push(key.clone());
            next_rows.insert(key, row);
        }

        self.rows = next_rows;
        self.order = next_order;
        let paused_scroll_top = anchor
            .and_then(|(key, intra_row_offset)| self.scroll_top_for_anchor(&key, intra_row_offset));

        ConversationRowLayoutResolution {
            heights,
            paused_scroll_top,
            next_refresh_after,
        }
    }

    fn anchor_at(&self, scroll_top: f32) -> Option<(String, f32)> {
        if self.order.is_empty() {
            return None;
        }
        let scroll_top = if scroll_top.is_finite() {
            scroll_top.max(0.0)
        } else {
            0.0
        };
        let mut row_top = 0.0;
        for key in &self.order {
            let height = self.rows.get(key)?.committed;
            if scroll_top < row_top + height {
                return Some((key.clone(), (scroll_top - row_top).max(0.0)));
            }
            row_top += height;
        }
        let key = self.order.last()?.clone();
        let height = self.rows.get(&key)?.committed;
        Some((key, height))
    }

    fn scroll_top_for_anchor(&self, anchor_key: &str, intra_row_offset: f32) -> Option<f32> {
        let mut row_top = 0.0;
        for key in &self.order {
            let height = self.rows.get(key)?.committed;
            if key == anchor_key {
                return Some(row_top + intra_row_offset.clamp(0.0, height));
            }
            row_top += height;
        }
        None
    }
}

fn sanitize_row_height(height: f32) -> f32 {
    if height.is_finite() {
        height.max(1.0)
    } else {
        1.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConversationViewport {
    selected_block_id: Option<String>,
    first_visible: usize,
    visible_count: usize,
    follow_latest: bool,
    block_count: usize,
    unseen_updates: usize,
    content_revision: Option<u64>,
}

impl ConversationViewport {
    pub fn new(visible_count: usize) -> Self {
        Self {
            selected_block_id: None,
            first_visible: 0,
            visible_count: visible_count.max(1),
            follow_latest: true,
            block_count: 0,
            unseen_updates: 0,
            content_revision: None,
        }
    }

    pub fn selected_block_id(&self) -> Option<&str> {
        self.selected_block_id.as_deref()
    }

    #[cfg(test)]
    pub const fn first_visible(&self) -> usize {
        self.first_visible
    }

    pub const fn follow_latest(&self) -> bool {
        self.follow_latest
    }

    pub const fn unseen_updates(&self) -> usize {
        self.unseen_updates
    }

    /// Reconcile follow-latest with the actual pixel distance from the bottom.
    ///
    /// Separate pause and resume thresholds add hysteresis around the bottom so
    /// fractional layout changes do not flicker between modes.
    pub fn reconcile_scroll_distance(&mut self, distance_to_bottom: f32) -> bool {
        let previous_follow_latest = self.follow_latest;
        let previous_unseen_updates = self.unseen_updates;
        let distance_to_bottom = if distance_to_bottom.is_finite() {
            distance_to_bottom.max(0.0)
        } else {
            f32::INFINITY
        };

        if self.follow_latest {
            if distance_to_bottom > FOLLOW_LATEST_PAUSE_THRESHOLD_PX {
                self.follow_latest = false;
            }
        } else if distance_to_bottom <= FOLLOW_LATEST_RESUME_THRESHOLD_PX {
            self.follow_latest = true;
            self.unseen_updates = 0;
        }

        self.follow_latest != previous_follow_latest
            || self.unseen_updates != previous_unseen_updates
    }

    #[cfg(test)]
    pub fn pause_follow_latest(&mut self) {
        self.follow_latest = false;
    }

    pub fn resume_latest(&mut self, block_count: usize) {
        self.follow_latest = true;
        self.unseen_updates = 0;
        self.on_blocks_changed(block_count);
    }

    pub fn select(&mut self, block_id: impl Into<String>, projection: &ConversationProjection) {
        let block_id = block_id.into();
        if projection.block(&block_id).is_some() {
            self.selected_block_id = Some(block_id);
        }
    }

    /// Select a currently visible live overlay that is not committed yet.
    ///
    /// The overlay must use the same typed message/tool identity as its future
    /// durable block so hydration can preserve the selection.
    pub fn select_live(&mut self, block_id: impl Into<String>) {
        self.selected_block_id = Some(block_id.into());
    }

    pub fn reconcile_hydration(
        &mut self,
        projection: &ConversationProjection,
        visible_block_count: usize,
        content_revision: u64,
    ) {
        if self
            .selected_block_id
            .as_deref()
            .is_some_and(|id| projection.block(id).is_none())
        {
            self.selected_block_id = None;
        }
        self.on_content_changed(visible_block_count, content_revision);
    }

    pub fn reconcile_live_selection(&mut self, live_id: &str, durable_id: &str) {
        if self.selected_block_id.as_deref() == Some(live_id) {
            self.selected_block_id = Some(durable_id.to_owned());
        }
    }

    #[cfg(test)]
    pub fn user_scrolled(&mut self, first_visible: usize, block_count: usize) {
        let max_first = block_count.saturating_sub(self.visible_count);
        self.first_visible = first_visible.min(max_first);
        self.follow_latest = self.first_visible.saturating_add(self.visible_count) >= block_count;
        self.block_count = block_count;
        if self.follow_latest {
            self.unseen_updates = 0;
        }
    }

    pub fn on_blocks_changed(&mut self, block_count: usize) {
        self.reconcile_blocks(block_count, 0);
    }

    /// Record a projection revision that can change an existing streaming row
    /// without increasing the row count.
    pub fn on_content_changed(&mut self, block_count: usize, content_revision: u64) {
        let revision_changed = self
            .content_revision
            .replace(content_revision)
            .is_some_and(|previous| previous != content_revision);
        self.reconcile_blocks(block_count, usize::from(revision_changed));
    }

    fn reconcile_blocks(&mut self, block_count: usize, minimum_unseen_updates: usize) {
        let appended = block_count.saturating_sub(self.block_count);
        self.block_count = block_count;
        let max_first = block_count.saturating_sub(self.visible_count);
        if self.follow_latest {
            self.first_visible = max_first;
            self.unseen_updates = 0;
        } else {
            self.first_visible = self.first_visible.min(max_first);
            self.unseen_updates = self
                .unseen_updates
                .saturating_add(appended.max(minimum_unseen_updates));
        }
    }

    #[cfg(test)]
    pub fn home(&mut self, projection: &ConversationProjection) {
        self.follow_latest = false;
        self.block_count = projection.blocks.len();
        self.first_visible = 0;
        self.selected_block_id = projection.blocks.front().map(|block| block.id.clone());
    }

    #[cfg(test)]
    pub fn end(&mut self, projection: &ConversationProjection) {
        self.follow_latest = true;
        self.on_blocks_changed(projection.blocks.len());
        self.selected_block_id = projection.blocks.back().map(|block| block.id.clone());
    }

    pub fn copy_selected(&self, projection: &ConversationProjection) -> Option<String> {
        projection
            .block(self.selected_block_id.as_deref()?)
            .map(ConversationBlock::copy_text)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ComposerAdmission {
    Idle,
    Pending {
        command_id: u64,
        kind: ComposerSubmissionKind,
        payload: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComposerSubmissionKind {
    Prompt,
    Steer,
    FollowUp,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubmittedPromptPreview {
    pub command_id: u64,
    pub payload: String,
}

impl SubmittedPromptPreview {
    pub fn block_id(&self) -> String {
        format!("submitted-user:{}", self.command_id)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ComposerSubmitError {
    #[error("composer draft is empty")]
    Empty,
    #[error("composer draft exceeds {MAX_COMPOSER_BYTES} bytes")]
    TooLarge,
    #[error("composer submission is already awaiting admission")]
    AdmissionPending,
    #[error("composer completion does not match pending command")]
    StaleCompletion,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComposerState {
    draft: String,
    admission: ComposerAdmission,
    submitted: Option<SubmittedPromptPreview>,
    rejection: Option<String>,
}

impl Default for ComposerState {
    fn default() -> Self {
        Self {
            draft: String::new(),
            admission: ComposerAdmission::Idle,
            submitted: None,
            rejection: None,
        }
    }
}

impl ComposerState {
    pub fn draft(&self) -> &str {
        &self.draft
    }

    pub fn admission(&self) -> &ComposerAdmission {
        &self.admission
    }

    pub fn rejection(&self) -> Option<&str> {
        self.rejection.as_deref()
    }

    pub fn submitted(&self) -> Option<&SubmittedPromptPreview> {
        self.submitted.as_ref()
    }

    pub fn edit(&mut self, draft: impl Into<String>) {
        self.draft = draft.into();
        self.rejection = None;
    }

    pub fn begin_submit(
        &mut self,
        command_id: u64,
        kind: ComposerSubmissionKind,
    ) -> Result<&str, ComposerSubmitError> {
        if matches!(self.admission, ComposerAdmission::Pending { .. }) {
            return Err(ComposerSubmitError::AdmissionPending);
        }
        if self.draft.trim().is_empty() {
            return Err(ComposerSubmitError::Empty);
        }
        if self.draft.len() > MAX_COMPOSER_BYTES {
            return Err(ComposerSubmitError::TooLarge);
        }
        self.admission = ComposerAdmission::Pending {
            command_id,
            kind,
            payload: self.draft.clone(),
        };
        let ComposerAdmission::Pending { payload, .. } = &self.admission else {
            unreachable!("composer admission was just installed");
        };
        Ok(payload)
    }

    pub fn accepted(&mut self, command_id: u64) -> Result<(), ComposerSubmitError> {
        let ComposerAdmission::Pending {
            command_id: pending,
            kind,
            payload,
        } = &self.admission
        else {
            return Err(ComposerSubmitError::StaleCompletion);
        };
        if *pending != command_id {
            return Err(ComposerSubmitError::StaleCompletion);
        }
        if *kind == ComposerSubmissionKind::Prompt {
            self.submitted = Some(SubmittedPromptPreview {
                command_id,
                payload: payload.clone(),
            });
        }
        if self.draft == *payload {
            self.draft.clear();
        }
        self.admission = ComposerAdmission::Idle;
        self.rejection = None;
        Ok(())
    }

    /// Reconcile an accepted client-local prompt with completed durable truth.
    ///
    /// Returns the live and durable block identities when the prompt was
    /// retained. If completed hydration does not contain it, the exact payload
    /// is restored to the draft instead of being silently lost.
    pub fn reconcile_completed_submission(
        &mut self,
        projection: &ConversationProjection,
    ) -> Option<(String, String)> {
        let submitted = self.submitted.take()?;
        if let Some(block) = projection.blocks().iter().rev().find(|block| {
            block.kind == ConversationBlockKind::User && block.text == submitted.payload
        }) {
            self.rejection = None;
            return Some((submitted.block_id(), block.id.clone()));
        }
        if self.draft.is_empty() {
            self.draft = submitted.payload;
        }
        self.rejection =
            Some("Accepted prompt was not retained; the exact draft was restored.".into());
        None
    }

    pub fn rejected(
        &mut self,
        command_id: u64,
        message: impl Into<String>,
    ) -> Result<(), ComposerSubmitError> {
        let ComposerAdmission::Pending {
            command_id: pending,
            ..
        } = &self.admission
        else {
            return Err(ComposerSubmitError::StaleCompletion);
        };
        if *pending != command_id {
            return Err(ComposerSubmitError::StaleCompletion);
        }
        self.admission = ComposerAdmission::Idle;
        self.rejection = Some(truncate_bytes(message.into(), MAX_BLOCK_TEXT_BYTES).0);
        Ok(())
    }
}

fn block_from_product(index: usize, item: CodingAgentSessionTranscriptItem) -> ConversationBlock {
    let reasoning_duration_millis = match &item {
        CodingAgentSessionTranscriptItem::Assistant {
            reasoning_duration_millis,
            ..
        } => *reasoning_duration_millis,
        _ => None,
    };
    let (kind, source_id, title, text, detail, done, is_error, image_count, truncated) = match item
    {
        CodingAgentSessionTranscriptItem::User { text } => {
            let (text, truncated) = truncate_bytes(text, MAX_BLOCK_TEXT_BYTES);
            (
                ConversationBlockKind::User,
                String::new(),
                "You".into(),
                text,
                String::new(),
                true,
                false,
                0,
                truncated,
            )
        }
        CodingAgentSessionTranscriptItem::Assistant {
            id,
            text,
            thinking,
            images,
            done,
            ..
        } => {
            let (text, text_truncated) = truncate_bytes(text, MAX_BLOCK_TEXT_BYTES);
            let (thinking, thinking_truncated) = truncate_bytes(thinking, MAX_THINKING_TEXT_BYTES);
            (
                ConversationBlockKind::Assistant,
                id,
                "Assistant".into(),
                text,
                thinking,
                done,
                false,
                images.len(),
                text_truncated || thinking_truncated,
            )
        }
        CodingAgentSessionTranscriptItem::Tool {
            call_id,
            name,
            args,
            result,
            is_error,
            duration_millis,
        } => {
            let arguments = serde_json::to_string_pretty(&args)
                .unwrap_or_else(|_| "<invalid tool arguments>".into());
            let (arguments, args_truncated) = truncate_bytes(arguments, MAX_TOOL_ARGUMENT_BYTES);
            let (result, result_truncated) =
                truncate_bytes(result.unwrap_or_default(), MAX_BLOCK_TEXT_BYTES);
            (
                ConversationBlockKind::Tool,
                call_id,
                tool_title(&name, duration_millis),
                result,
                arguments,
                true,
                is_error,
                0,
                args_truncated || result_truncated,
            )
        }
        CodingAgentSessionTranscriptItem::Delegation {
            tool_call_id,
            target_kind,
            target_id,
            task,
            status,
            summary,
            ..
        } => {
            let (task, task_truncated) = truncate_bytes(task, MAX_BLOCK_TEXT_BYTES);
            let (summary, summary_truncated) =
                truncate_bytes(summary.unwrap_or(status), MAX_BLOCK_TEXT_BYTES);
            (
                ConversationBlockKind::Delegation,
                tool_call_id,
                format!("Delegation · {target_kind:?} · {target_id}"),
                task,
                summary,
                true,
                false,
                0,
                task_truncated || summary_truncated,
            )
        }
        CodingAgentSessionTranscriptItem::CompactionSummary { summary } => summary_block(
            ConversationBlockKind::CompactionSummary,
            "Compaction",
            summary,
        ),
        CodingAgentSessionTranscriptItem::BranchSummary { summary } => summary_block(
            ConversationBlockKind::BranchSummary,
            "Branch summary",
            summary,
        ),
        CodingAgentSessionTranscriptItem::Diagnostic { message } => {
            let (message, truncated) = truncate_bytes(message, MAX_BLOCK_TEXT_BYTES);
            (
                ConversationBlockKind::Diagnostic,
                String::new(),
                "Diagnostic".into(),
                message,
                String::new(),
                true,
                true,
                0,
                truncated,
            )
        }
    };
    let id = if source_id.is_empty() {
        format!("{index:08}:{}", kind.key())
    } else {
        format!("{}:{source_id}", kind.key())
    };
    let mut block = ConversationBlock {
        id,
        source_revision: 0,
        kind,
        title,
        text,
        detail,
        done,
        is_error,
        image_count,
        reasoning_duration_millis,
        truncated,
    };
    block.refresh_source_revision();
    block
}

fn tool_title(name: &str, duration_millis: Option<u64>) -> String {
    match duration_millis {
        Some(duration_millis) => {
            format!("Tool · {name} · {}", compact_duration(duration_millis))
        }
        None => format!("Tool · {name}"),
    }
}

/// Formats an authoritative lifecycle duration using stable compact units.
pub fn compact_duration(duration_millis: u64) -> String {
    if duration_millis < 1_000 {
        return format!("{duration_millis} ms");
    }
    if duration_millis < 60_000 {
        let rounded_tenths = duration_millis.saturating_add(50) / 100;
        if rounded_tenths < 600 {
            return format!("{}.{:01} s", rounded_tenths / 10, rounded_tenths % 10);
        }
    }
    let rounded_seconds = duration_millis.saturating_add(500) / 1_000;
    format!("{}m {:02}s", rounded_seconds / 60, rounded_seconds % 60)
}

fn conversation_block_revision(block: &ConversationBlock) -> u64 {
    const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

    fn update(mut hash: u64, bytes: &[u8]) -> u64 {
        for byte in bytes {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(FNV_PRIME);
        }
        hash
    }

    let mut hash = FNV_OFFSET;
    for value in [
        block.id.as_bytes(),
        block.kind.key().as_bytes(),
        block.title.as_bytes(),
        block.text.as_bytes(),
        block.detail.as_bytes(),
    ] {
        hash = update(hash, &(value.len() as u64).to_le_bytes());
        hash = update(hash, value);
    }
    hash = update(hash, &[u8::from(block.done)]);
    hash = update(hash, &[u8::from(block.is_error)]);
    hash = update(hash, &(block.image_count as u64).to_le_bytes());
    hash = update(
        hash,
        &block
            .reasoning_duration_millis
            .unwrap_or(u64::MAX)
            .to_le_bytes(),
    );
    update(hash, &[u8::from(block.truncated)])
}

fn summary_block(
    kind: ConversationBlockKind,
    title: &str,
    summary: String,
) -> (
    ConversationBlockKind,
    String,
    String,
    String,
    String,
    bool,
    bool,
    usize,
    bool,
) {
    let (summary, truncated) = truncate_bytes(summary, MAX_BLOCK_TEXT_BYTES);
    (
        kind,
        String::new(),
        title.into(),
        summary,
        String::new(),
        true,
        false,
        0,
        truncated,
    )
}

fn truncate_bytes(mut text: String, max_bytes: usize) -> (String, bool) {
    if text.len() <= max_bytes {
        return (text, false);
    }
    let mut boundary = max_bytes;
    while boundary > 0 && !text.is_char_boundary(boundary) {
        boundary -= 1;
    }
    text.truncate(boundary);
    (text, true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn transcript(items: Vec<CodingAgentSessionTranscriptItem>) -> CodingAgentTranscriptSnapshot {
        CodingAgentTranscriptSnapshot {
            session_id: "session-1".into(),
            active_leaf_id: Some("leaf-1".into()),
            items,
        }
    }

    fn user(index: usize) -> CodingAgentSessionTranscriptItem {
        CodingAgentSessionTranscriptItem::User {
            text: format!("message {index}"),
        }
    }

    fn row_layout(key: &str, target_height: f32, streaming: bool) -> ConversationRowLayoutInput {
        ConversationRowLayoutInput {
            item_key: ConversationItemKey::new(
                "layout-test-session",
                ConversationItemKind::Durable(ConversationBlockKind::Assistant),
                key,
            ),
            source_revision: 1,
            text_phase: if streaming {
                StreamingTextPhase::StreamingPlainText
            } else {
                StreamingTextPhase::FinalMarkdown
            },
            details_expanded: false,
            estimated_height: target_height,
        }
    }

    fn render_source<'a>(
        key: &'a str,
        revision: u64,
        text: &'a str,
        done: bool,
    ) -> ConversationRowRenderSource<'a> {
        ConversationRowRenderSource {
            item_key: ConversationItemKey::new(
                "test-session",
                ConversationItemKind::Durable(ConversationBlockKind::Assistant),
                key,
            ),
            source_revision: revision,
            title: Cow::Borrowed("Assistant"),
            text,
            detail: "",
            kind: ConversationBlockKind::Assistant,
            done,
            is_error: false,
            image_count: 0,
            reasoning_duration_millis: None,
            truncated: false,
            durable: true,
        }
    }

    fn cache_contains_row(cache: &ConversationRowRenderCache, row_id: &str) -> bool {
        cache.entries.keys().any(|key| key.row_id() == row_id)
    }

    #[test]
    fn completed_row_cache_sanitizes_once_and_freezes_revision_state() {
        let mut cache = ConversationRowRenderCache::default();
        let large = format!(
            "# Answer\n\n![remote](https://invalid/image)\n\n{}",
            "x".repeat(64_000)
        );
        cache.begin_frame();
        let first = cache.resolve(render_source("session:assistant:1", 7, &large, true), 960);
        let second = cache.resolve(
            render_source(
                "session:assistant:1",
                7,
                "ignored without a new revision",
                true,
            ),
            960,
        );
        cache.finish_frame();

        assert_eq!(cache.sanitization_count, 1);
        assert_eq!(first.sanitized_revision, 7);
        assert!(Arc::ptr_eq(&first.text, &second.text));
        assert!(Arc::ptr_eq(
            &first.markdown_state_key,
            &second.markdown_state_key
        ));
        assert_eq!(first.text, second.text);
        assert!(first.media_neutralized);
    }

    #[test]
    fn width_change_remeasures_without_resanitizing_or_cloning_text() {
        let mut cache = ConversationRowRenderCache::default();
        let text = "wide conversation content ".repeat(200);
        cache.begin_frame();
        let wide = cache.resolve(render_source("session:assistant:2", 1, &text, true), 1_200);
        let narrow = cache.resolve(render_source("session:assistant:2", 1, &text, true), 480);
        cache.finish_frame();

        assert_eq!(cache.sanitization_count, 1);
        assert!(Arc::ptr_eq(&wide.text, &narrow.text));
        assert_ne!(wide.width_bucket, narrow.width_bucket);
        assert!(narrow.estimated_height >= wide.estimated_height);
    }

    #[test]
    fn streaming_row_cache_reuses_arc_until_source_revision_changes() {
        let mut cache = ConversationRowRenderCache::default();
        cache.begin_frame();
        let first = cache.resolve(
            render_source("session:assistant:3", 1, "partial", false),
            800,
        );
        let same = cache.resolve(
            render_source("session:assistant:3", 1, "partial", false),
            800,
        );
        let updated = cache.resolve(
            render_source("session:assistant:3", 2, "partial update", false),
            800,
        );
        cache.finish_frame();

        assert_eq!(cache.sanitization_count, 0);
        assert!(Arc::ptr_eq(&first.text, &same.text));
        assert!(!Arc::ptr_eq(&same.text, &updated.text));
        assert_eq!(&*updated.text, "partial update");
    }

    #[test]
    fn session_scoped_cache_keys_prevent_cross_session_state_reuse() {
        let mut cache = ConversationRowRenderCache::default();
        cache.begin_frame();
        let first = cache.resolve(
            ConversationRowRenderSource {
                item_key: ConversationItemKey::new(
                    "session-a",
                    ConversationItemKind::Durable(ConversationBlockKind::User),
                    "user:0",
                ),
                source_revision: 7,
                title: Cow::Borrowed("You"),
                text: "session A content",
                detail: "",
                kind: ConversationBlockKind::User,
                done: true,
                is_error: false,
                image_count: 0,
                reasoning_duration_millis: None,
                truncated: false,
                durable: true,
            },
            900,
        );
        cache.finish_frame();

        cache.begin_frame();
        let second = cache.resolve(
            ConversationRowRenderSource {
                item_key: ConversationItemKey::new(
                    "session-b",
                    ConversationItemKind::Durable(ConversationBlockKind::User),
                    "user:0",
                ),
                source_revision: 7,
                title: Cow::Borrowed("You"),
                text: "session B content",
                detail: "",
                kind: ConversationBlockKind::User,
                done: true,
                is_error: false,
                image_count: 0,
                reasoning_duration_millis: None,
                truncated: false,
                durable: true,
            },
            900,
        );
        cache.finish_frame();

        assert_eq!(first.item_key.row_id(), second.item_key.row_id());
        assert_eq!(first.source_revision, second.source_revision);
        assert_ne!(first.item_key, second.item_key);
        assert_eq!(first.text.as_ref(), "session A content");
        assert_eq!(second.text.as_ref(), "session B content");
        assert_eq!(cache.entries.len(), 1);
        assert!(cache.entries.contains_key(&second.item_key));
    }

    #[test]
    fn typed_item_key_scopes_session_kind_and_render_state() {
        let durable = ConversationItemKey::new(
            "session-a",
            ConversationItemKind::Durable(ConversationBlockKind::Assistant),
            "assistant:1",
        );
        let live = ConversationItemKey::new(
            "session-a",
            ConversationItemKind::LiveMessage,
            "assistant:1",
        );
        let other_session = ConversationItemKey::new(
            "session-b",
            ConversationItemKind::Durable(ConversationBlockKind::Assistant),
            "assistant:1",
        );

        assert_ne!(durable, live);
        assert_ne!(durable, other_session);
        assert_eq!(durable.row_id(), "assistant:1");
        assert!(
            durable
                .stable_id()
                .contains("session-a:assistant:11:assistant:1")
        );
        assert!(
            durable
                .markdown_state_key(false, 7, false)
                .contains(durable.stable_id())
        );
        assert!(
            durable
                .markdown_state_key(true, 7, true)
                .contains(durable.stable_id())
        );
        assert_ne!(
            durable.markdown_state_key(false, 7, false),
            durable.markdown_state_key(false, 7, true)
        );
        assert_ne!(
            durable.markdown_state_key(false, 7, true),
            durable.markdown_state_key(false, 8, true)
        );
        assert_ne!(
            ConversationItemKey::new(
                "a:b",
                ConversationItemKind::Durable(ConversationBlockKind::User),
                "c",
            )
            .stable_id(),
            ConversationItemKey::new(
                "a",
                ConversationItemKind::Durable(ConversationBlockKind::User),
                "b:c",
            )
            .stable_id()
        );
    }

    #[test]
    fn streaming_to_final_revision_sanitizes_once_and_freezes_final_state() {
        let mut cache = ConversationRowRenderCache::default();
        cache.begin_frame();
        let streaming = cache.resolve(
            ConversationRowRenderSource {
                item_key: ConversationItemKey::new(
                    "session-a",
                    ConversationItemKind::LiveMessage,
                    "assistant:1",
                ),
                source_revision: 1,
                title: Cow::Borrowed("Assistant"),
                text: "**partial",
                detail: "reasoning in progress",
                kind: ConversationBlockKind::Assistant,
                done: false,
                is_error: false,
                image_count: 0,
                reasoning_duration_millis: None,
                truncated: false,
                durable: false,
            },
            900,
        );
        assert_eq!(streaming.text.as_ref(), "**partial");
        assert_eq!(cache.sanitization_count, 0);

        cache.begin_frame();
        let final_row = cache.resolve(
            ConversationRowRenderSource {
                item_key: ConversationItemKey::new(
                    "session-a",
                    ConversationItemKind::LiveMessage,
                    "assistant:1",
                ),
                source_revision: 2,
                title: Cow::Borrowed("Assistant"),
                text: "**final**",
                detail: "reasoning complete",
                kind: ConversationBlockKind::Assistant,
                done: true,
                is_error: false,
                image_count: 0,
                reasoning_duration_millis: Some(2_430),
                truncated: false,
                durable: true,
            },
            900,
        );
        assert_eq!(cache.sanitization_count, 1);
        assert_eq!(final_row.sanitized_revision, 2);
        assert!(!Arc::ptr_eq(&streaming.text, &final_row.text));

        let frozen = cache.resolve(
            ConversationRowRenderSource {
                item_key: ConversationItemKey::new(
                    "session-a",
                    ConversationItemKind::LiveMessage,
                    "assistant:1",
                ),
                source_revision: 2,
                title: Cow::Borrowed("Assistant"),
                text: "ignored identical revision payload",
                detail: "ignored identical revision detail",
                kind: ConversationBlockKind::Assistant,
                done: true,
                is_error: false,
                image_count: 0,
                reasoning_duration_millis: Some(2_430),
                truncated: false,
                durable: true,
            },
            900,
        );
        assert_eq!(cache.sanitization_count, 1);
        assert!(Arc::ptr_eq(&final_row.text, &frozen.text));
        assert!(Arc::ptr_eq(&final_row.detail, &frozen.detail));
    }

    #[test]
    fn streaming_revision_settles_after_quiet_window_and_rejects_stale_results() {
        let mut cache = ConversationRowRenderCache::default();
        let started = Instant::now();
        cache.begin_frame();
        let active = cache.resolve_at(
            render_source("assistant:quiet", 2, "new revision", false),
            900,
            started,
        );
        assert_eq!(active.text_phase, StreamingTextPhase::StreamingPlainText);
        assert_eq!(
            active.next_text_phase_after,
            Some(STREAMING_MARKDOWN_SETTLE_DELAY)
        );

        let before_settle = cache.resolve_at(
            render_source("assistant:quiet", 2, "new revision", false),
            900,
            started + STREAMING_MARKDOWN_SETTLE_DELAY - Duration::from_millis(1),
        );
        assert_eq!(
            before_settle.text_phase,
            StreamingTextPhase::StreamingPlainText
        );
        assert_eq!(
            before_settle.next_text_phase_after,
            Some(Duration::from_millis(1))
        );

        let settled = cache.resolve_at(
            render_source("assistant:quiet", 2, "new revision", false),
            900,
            started + STREAMING_MARKDOWN_SETTLE_DELAY,
        );
        assert_eq!(settled.text_phase, StreamingTextPhase::SettlingMarkdown);
        assert_eq!(settled.next_text_phase_after, None);

        let stale = cache.resolve_at(
            render_source("assistant:quiet", 1, "stale revision", false),
            900,
            started + STREAMING_MARKDOWN_SETTLE_DELAY + Duration::from_millis(1),
        );
        assert_eq!(stale.source_revision, 2);
        assert_eq!(stale.text.as_ref(), "new revision");
        assert_eq!(stale.text_phase, StreamingTextPhase::SettlingMarkdown);
        assert_eq!(stale.markdown_state_key, settled.markdown_state_key);

        let final_row = cache.resolve_at(
            render_source("assistant:quiet", 3, "**final**", true),
            900,
            started + STREAMING_MARKDOWN_SETTLE_DELAY + Duration::from_millis(2),
        );
        assert_eq!(final_row.text_phase, StreamingTextPhase::FinalMarkdown);
        assert_eq!(final_row.next_text_phase_after, None);
        assert_ne!(final_row.markdown_state_key, settled.markdown_state_key);

        let oversized = "x".repeat(MAX_SETTLING_MARKDOWN_BYTES + 1);
        let oversized_row = cache.resolve_at(
            render_source("assistant:oversized", 1, &oversized, false),
            900,
            started + STREAMING_MARKDOWN_SETTLE_DELAY,
        );
        assert_eq!(
            oversized_row.text_phase,
            StreamingTextPhase::StreamingPlainText
        );
        assert_eq!(oversized_row.next_text_phase_after, None);
    }

    #[test]
    fn row_render_cache_drops_stale_entries_and_enforces_bounds() {
        let mut cache = ConversationRowRenderCache::with_limits(2, 128 * 1024);
        cache.begin_frame();
        cache.resolve(render_source("old", 1, "old", false), 800);
        cache.finish_frame();
        assert!(cache_contains_row(&cache, "old"));

        cache.begin_frame();
        for key in ["new-a", "new-b", "new-c"] {
            cache.resolve(render_source(key, 1, &"x".repeat(8_000), false), 800);
        }
        cache.finish_frame();

        assert!(!cache_contains_row(&cache, "old"));
        assert!(cache.entries.len() <= 2);
        assert!(cache.retained_bytes <= 128 * 1024);
    }

    #[test]
    fn incremental_cache_finish_preserves_untouched_history_until_full_sweep() {
        let mut cache = ConversationRowRenderCache::default();
        cache.begin_frame();
        cache.resolve(render_source("durable", 1, "history", true), 800);
        cache.finish_frame();

        cache.begin_frame();
        cache.resolve(render_source("live", 2, "streaming", false), 800);
        cache.finish_incremental();
        assert!(cache_contains_row(&cache, "durable"));
        assert!(cache_contains_row(&cache, "live"));

        cache.begin_frame();
        cache.resolve(render_source("replacement", 3, "new session", true), 800);
        cache.finish_frame();
        assert_eq!(cache.entries.len(), 1);
        assert!(cache_contains_row(&cache, "replacement"));
    }

    #[test]
    fn conversation_row_estimates_use_display_width_for_unicode() {
        assert_eq!(estimated_text_rows("abcdefghij", 10, 20), 1);
        assert_eq!(estimated_text_rows("界界界界界", 10, 20), 1);
        assert_eq!(estimated_text_rows("界界界界界界", 10, 20), 2);
        assert_eq!(estimated_text_rows("🙂🙂🙂🙂🙂", 10, 20), 1);
        assert_eq!(estimated_text_rows("e\u{301}e\u{301}e\u{301}", 3, 20), 1);
    }

    #[test]
    fn adjacent_equivalent_diagnostics_coalesce_for_presentation() {
        let message = "invalid terminal tool-call name";
        let projection = ConversationProjection::hydrate(transcript(vec![
            CodingAgentSessionTranscriptItem::Diagnostic {
                message: message.into(),
            },
            CodingAgentSessionTranscriptItem::Diagnostic {
                message: message.into(),
            },
            CodingAgentSessionTranscriptItem::Diagnostic {
                message: format!("provider error: {message}"),
            },
            user(1),
            CodingAgentSessionTranscriptItem::Diagnostic {
                message: message.into(),
            },
        ]));

        assert_eq!(projection.blocks().len(), 3);
        let diagnostic = projection.blocks().front().unwrap();
        assert_eq!(diagnostic.title, "Diagnostic · 3 related events");
        assert_eq!(diagnostic.text, message);
        assert_eq!(projection.blocks().back().unwrap().title, "Diagnostic");
    }

    #[test]
    fn long_transcript_retains_the_newest_ten_thousand_stable_blocks() {
        let projection =
            ConversationProjection::hydrate(transcript((0..10_500).map(user).collect::<Vec<_>>()));
        assert_eq!(projection.blocks().len(), MAX_TRANSCRIPT_BLOCKS);
        assert_eq!(projection.omitted_blocks(), 500);
        assert_eq!(projection.blocks().front().unwrap().text, "message 500");
        assert_eq!(projection.blocks().back().unwrap().text, "message 10499");
    }

    #[test]
    fn release_fixture_retains_ten_thousand_blocks_and_at_least_ten_mib() {
        let payload = "x".repeat(1_280);
        let items = (0..MAX_TRANSCRIPT_BLOCKS)
            .map(|index| CodingAgentSessionTranscriptItem::User {
                text: format!("{index}:{payload}"),
            })
            .collect::<Vec<_>>();
        let fixture_bytes = items
            .iter()
            .map(|item| match item {
                CodingAgentSessionTranscriptItem::User { text } => text.len(),
                _ => 0,
            })
            .sum::<usize>();
        assert!(fixture_bytes >= 10 * 1024 * 1024);

        let projection = ConversationProjection::hydrate(transcript(items));
        assert_eq!(projection.blocks().len(), MAX_TRANSCRIPT_BLOCKS);
        assert_eq!(projection.omitted_blocks(), 0);
        assert!(projection.retained_bytes() <= MAX_TRANSCRIPT_BYTES);
        assert!(projection.blocks().front().unwrap().text.starts_with("0:"));
        assert!(
            projection
                .blocks()
                .back()
                .unwrap()
                .text
                .starts_with("9999:")
        );
    }

    #[test]
    #[ignore = "release performance gate"]
    fn desktop_release_empty_conversation_baseline() {
        let _performance_guard = crate::allocation_probe::serial_guard();
        let projection = ConversationProjection::hydrate(transcript(Vec::new()));
        let viewport = ConversationViewport::new(30);
        let composer = ComposerState::default();
        std::hint::black_box((&projection, &viewport, &composer));
        println!("desktop_perf\tempty_blocks={}", projection.blocks().len());
    }

    #[test]
    #[ignore = "release performance gate"]
    fn desktop_release_ten_mib_interaction_baseline() {
        let _performance_guard = crate::allocation_probe::serial_guard();
        const SAMPLE_COUNT: usize = 500;
        const VISIBLE_BLOCKS: usize = 30;
        const FRAME_BUDGET_MICROS: u128 = 16_700;
        const HYDRATION_ALLOCATION_BUDGET: u64 = 40_064;
        const HYDRATION_ALLOCATED_BYTE_BUDGET: u64 = 8 * 1024 * 1024;
        const HYDRATION_RSS_GROWTH_BUDGET: u64 = 64 * 1024 * 1024;

        let payload = format!(
            "# streamed message\n\n{}\n\n```text\n{}\n```",
            "x".repeat(1_152),
            "tool progress".repeat(8)
        );
        let items = (0..MAX_TRANSCRIPT_BLOCKS)
            .map(|index| CodingAgentSessionTranscriptItem::User {
                text: format!("{index}:{payload}"),
            })
            .collect::<Vec<_>>();
        let fixture_bytes = items
            .iter()
            .map(|item| match item {
                CodingAgentSessionTranscriptItem::User { text } => text.len(),
                _ => 0,
            })
            .sum::<usize>();
        assert!(fixture_bytes >= 10 * 1024 * 1024);

        let rss_before = crate::allocation_probe::resident_bytes();
        let allocations_before = crate::allocation_probe::snapshot();
        let hydration_started = std::time::Instant::now();
        let projection = ConversationProjection::hydrate(transcript(items));
        let hydration_micros = hydration_started.elapsed().as_micros();
        let hydration_allocations =
            crate::allocation_probe::snapshot().delta_since(allocations_before);
        let rss_after = crate::allocation_probe::resident_bytes();
        let rss_growth = resident_growth(rss_before, rss_after);
        assert_eq!(projection.blocks().len(), MAX_TRANSCRIPT_BLOCKS);

        let mut viewport = ConversationViewport::new(VISIBLE_BLOCKS);
        viewport.on_blocks_changed(projection.blocks().len());
        let mut scroll_samples = Vec::with_capacity(SAMPLE_COUNT);
        for sample in 0..SAMPLE_COUNT {
            let first_visible =
                (sample * 97) % (MAX_TRANSCRIPT_BLOCKS.saturating_sub(VISIBLE_BLOCKS));
            let started = std::time::Instant::now();
            viewport.user_scrolled(first_visible, projection.blocks().len());
            let visible = projection
                .blocks()
                .iter()
                .skip(viewport.first_visible())
                .take(VISIBLE_BLOCKS)
                .map(|block| bounded_markdown_preview(&block.text))
                .collect::<Vec<_>>();
            std::hint::black_box(visible);
            scroll_samples.push(started.elapsed().as_micros());
        }

        let mut composer = ComposerState::default();
        let mut input_samples = Vec::with_capacity(SAMPLE_COUNT);
        for sample in 0..SAMPLE_COUNT {
            let draft = format!("ordinary desktop input {sample} {}", "x".repeat(256));
            let started = std::time::Instant::now();
            composer.edit(draft);
            std::hint::black_box(composer.draft());
            input_samples.push(started.elapsed().as_nanos());
        }

        let scroll_p95_micros = percentile_95(&mut scroll_samples);
        let input_p95_micros = percentile_95(&mut input_samples).div_ceil(1_000);
        println!(
            "desktop_perf\tplatform={}\tfixture_bytes={fixture_bytes}\thydration_us={hydration_micros}\t\
             hydration_allocations={}\thydration_allocated_bytes={}\tretained_bytes={}\t\
             rss_supported={}\trss_before_bytes={}\trss_after_bytes={}\trss_growth_bytes={}\t\
             scroll_render_p95_us={scroll_p95_micros}\tinput_p95_us={input_p95_micros}",
            std::env::consts::OS,
            hydration_allocations.count(),
            hydration_allocations.bytes(),
            projection.retained_bytes(),
            rss_before.is_some() && rss_after.is_some(),
            rss_before.unwrap_or_default(),
            rss_after.unwrap_or_default(),
            rss_growth.unwrap_or_default()
        );
        assert!(
            scroll_p95_micros <= FRAME_BUDGET_MICROS,
            "10k transcript scroll/render preparation P95 exceeded one frame: \
             {scroll_p95_micros} us"
        );
        assert!(
            input_p95_micros <= FRAME_BUDGET_MICROS,
            "composer input P95 exceeded one frame: {input_p95_micros} us"
        );
        assert!(
            hydration_allocations.count() <= HYDRATION_ALLOCATION_BUDGET,
            "10k transcript hydration allocation count exceeded the linear budget: {}",
            hydration_allocations.count()
        );
        assert!(
            hydration_allocations.bytes() <= HYDRATION_ALLOCATED_BYTE_BUDGET,
            "10 MiB transcript hydration copied too much retained content: {} bytes",
            hydration_allocations.bytes()
        );
        if let Some(rss_growth) = rss_growth {
            assert!(
                rss_growth <= HYDRATION_RSS_GROWTH_BUDGET,
                "10 MiB transcript hydration RSS growth exceeded 64 MiB: {rss_growth} bytes"
            );
        }
    }

    #[test]
    #[ignore = "release performance gate"]
    fn desktop_release_scale_content_and_streaming_matrix() {
        let _performance_guard = crate::allocation_probe::serial_guard();
        const FRAME_BUDGET_MICROS: u128 = 16_700;
        const FINAL_PARSE_BUDGET_MICROS: u128 = 150_000;
        const HYDRATION_RSS_GROWTH_BUDGET: u64 = 64 * 1024 * 1024;

        for block_count in [1, 100, 1_000, MAX_TRANSCRIPT_BLOCKS] {
            let items = (0..block_count)
                .map(user)
                .collect::<Vec<CodingAgentSessionTranscriptItem>>();
            let rss_before = crate::allocation_probe::resident_bytes();
            let allocations_before = crate::allocation_probe::snapshot();
            let started = std::time::Instant::now();
            let projection = ConversationProjection::hydrate(transcript(items));
            let hydration_micros = started.elapsed().as_micros();
            let hydration_allocations =
                crate::allocation_probe::snapshot().delta_since(allocations_before);
            let rss_after = crate::allocation_probe::resident_bytes();
            let rss_growth = resident_growth(rss_before, rss_after);
            println!(
                "desktop_perf\tplatform={}\tscale_blocks={block_count}\thydration_us={hydration_micros}\t\
                 hydration_allocations={}\thydration_allocated_bytes={}\tretained_bytes={}\t\
                 rss_supported={}\trss_before_bytes={}\trss_after_bytes={}\trss_growth_bytes={}",
                std::env::consts::OS,
                hydration_allocations.count(),
                hydration_allocations.bytes(),
                projection.retained_bytes(),
                rss_before.is_some() && rss_after.is_some(),
                rss_before.unwrap_or_default(),
                rss_after.unwrap_or_default(),
                rss_growth.unwrap_or_default()
            );
            assert_eq!(projection.blocks().len(), block_count);
            assert!(
                hydration_allocations.count() <= block_count as u64 * 4 + 64,
                "{block_count}-block hydration allocation count exceeded the linear budget"
            );
            assert!(
                hydration_allocations.bytes() <= block_count as u64 * 512 + 4_096,
                "{block_count}-block hydration allocated-byte count exceeded the linear budget"
            );
            if let Some(rss_growth) = rss_growth {
                assert!(
                    rss_growth <= HYDRATION_RSS_GROWTH_BUDGET,
                    "{block_count}-block hydration RSS growth exceeded 64 MiB"
                );
            }
        }

        let table_row = format!(
            "| {} |\n",
            (0..32).map(|_| "cell").collect::<Vec<_>>().join(" | ")
        );
        let content_cases = [
            (
                "markdown_256k",
                format!(
                    "# heading\n\n{}",
                    "paragraph **bold** `code`\n".repeat(10_000)
                ),
            ),
            ("reasoning_512k", "reasoning step 中文 🧠\n".repeat(24_000)),
            (
                "bash_output",
                format!("```text\n{}\n```", "build output\n".repeat(80_000)),
            ),
            ("table", table_row.repeat(1_000)),
            (
                "code_cjk_emoji",
                format!(
                    "```rust\n{}\n```\n{}",
                    "fn main() {}\n".repeat(12_000),
                    "中文🙂🚀\n".repeat(12_000)
                ),
            ),
        ];
        for (name, payload) in content_cases {
            let mut samples = Vec::with_capacity(20);
            for _ in 0..20 {
                let started = std::time::Instant::now();
                let preview = bounded_markdown_preview(&payload);
                std::hint::black_box(preview);
                samples.push(started.elapsed().as_micros());
            }
            let preview_p95_micros = percentile_95(&mut samples);
            println!(
                "desktop_perf\tcontent={name}\tinput_bytes={}\tpreview_sanitize_p95_us={preview_p95_micros}",
                payload.len()
            );
            assert!(
                preview_p95_micros <= FINAL_PARSE_BUDGET_MICROS,
                "{name} bounded preview sanitize P95 exceeded 150ms: {preview_p95_micros} us"
            );
        }

        let streaming_text = "streaming 中文 🙂 output ".repeat(128);
        for events_per_second in [10, 50, 200] {
            let mut cache = ConversationRowRenderCache::default();
            let mut samples = Vec::with_capacity(events_per_second);
            for revision in 1..=events_per_second {
                cache.begin_frame();
                let started = std::time::Instant::now();
                let row = cache.resolve(
                    ConversationRowRenderSource {
                        item_key: ConversationItemKey::new(
                            "performance-session",
                            ConversationItemKind::LiveMessage,
                            "live:assistant:performance",
                        ),
                        source_revision: revision as u64,
                        title: Cow::Borrowed("Assistant"),
                        text: &streaming_text,
                        detail: "",
                        kind: ConversationBlockKind::Assistant,
                        done: false,
                        is_error: false,
                        image_count: 0,
                        reasoning_duration_millis: None,
                        truncated: false,
                        durable: false,
                    },
                    900,
                );
                std::hint::black_box(row);
                cache.finish_incremental();
                samples.push(started.elapsed().as_micros());
            }
            let event_p95_micros = percentile_95(&mut samples);
            println!(
                "desktop_perf\tstream_events_per_s={events_per_second}\t\
                 event_p95_us={event_p95_micros}\tcache_bytes={}",
                cache.retained_bytes
            );
            assert!(
                event_p95_micros <= FRAME_BUDGET_MICROS,
                "{events_per_second} events/s row preparation P95 exceeded one frame: \
                 {event_p95_micros} us"
            );
        }
    }

    fn percentile_95(samples: &mut [u128]) -> u128 {
        samples.sort_unstable();
        let index = (samples.len() * 95).div_ceil(100).saturating_sub(1);
        samples[index]
    }

    fn resident_growth(before: Option<u64>, after: Option<u64>) -> Option<u64> {
        Some(after?.saturating_sub(before?))
    }

    #[test]
    fn long_unicode_text_is_truncated_on_a_scalar_boundary() {
        let projection = ConversationProjection::hydrate(transcript(vec![
            CodingAgentSessionTranscriptItem::User {
                text: "界".repeat(MAX_BLOCK_TEXT_BYTES),
            },
        ]));
        let block = projection.blocks().front().unwrap();
        assert!(block.truncated);
        assert!(block.text.len() <= MAX_BLOCK_TEXT_BYTES);
        assert!(block.text.is_char_boundary(block.text.len()));
    }

    #[test]
    fn selection_survives_matching_hydration_and_copy_is_bounded() {
        let projection = ConversationProjection::hydrate(transcript(vec![user(0), user(1)]));
        let selected = projection.blocks().front().unwrap().id.clone();
        let mut viewport = ConversationViewport::new(1);
        viewport.select(selected.clone(), &projection);
        viewport.reconcile_hydration(&projection, projection.blocks().len(), 1);
        assert_eq!(viewport.selected_block_id(), Some(selected.as_str()));
        assert_eq!(
            viewport.copy_selected(&projection).as_deref(),
            Some("message 0")
        );
    }

    #[test]
    fn tool_duration_uses_compact_stable_units() {
        assert_eq!(compact_duration(0), "0 ms");
        assert_eq!(compact_duration(999), "999 ms");
        assert_eq!(compact_duration(1_049), "1.0 s");
        assert_eq!(compact_duration(1_050), "1.1 s");
        assert_eq!(compact_duration(59_949), "59.9 s");
        assert_eq!(compact_duration(59_950), "1m 00s");
        assert_eq!(compact_duration(60_000), "1m 00s");
        assert_eq!(compact_duration(125_600), "2m 06s");
    }

    #[test]
    fn assistant_hydration_preserves_reasoning_duration_for_disclosure() {
        let projection = ConversationProjection::hydrate(transcript(vec![
            CodingAgentSessionTranscriptItem::Assistant {
                id: "message-reasoning".into(),
                text: "answer".into(),
                thinking: "reasoning".into(),
                images: Vec::new(),
                done: true,
                reasoning_duration_millis: Some(2_430),
            },
        ]));
        let block = projection.blocks().front().unwrap();
        assert_eq!(block.reasoning_duration_millis, Some(2_430));
        assert_eq!(
            compact_duration(block.reasoning_duration_millis.unwrap()),
            "2.4 s"
        );
    }

    #[test]
    fn live_row_copy_uses_the_same_bounded_utf8_safe_projection() {
        assert_eq!(conversation_copy_text("answer", "detail"), "answer\ndetail");
        let copied = conversation_copy_text("", &"界".repeat(MAX_COPY_BYTES));
        assert!(copied.len() <= MAX_COPY_BYTES);
        assert!(copied.is_char_boundary(copied.len()));
    }

    #[test]
    fn live_assistant_selection_reconciles_to_the_durable_identity() {
        let projection = ConversationProjection::hydrate(transcript(vec![
            CodingAgentSessionTranscriptItem::Assistant {
                id: "message-7".into(),
                text: "done".into(),
                thinking: String::new(),
                images: Vec::new(),
                done: true,
                reasoning_duration_millis: None,
            },
        ]));
        let mut viewport = ConversationViewport::new(1);
        viewport.select_live("assistant:message-7");
        viewport.reconcile_hydration(&projection, projection.blocks().len(), 1);
        assert_eq!(viewport.selected_block_id(), Some("assistant:message-7"));
    }

    #[test]
    fn scrolled_reader_is_not_forced_to_latest() {
        let projection = ConversationProjection::hydrate(transcript((0..20).map(user).collect()));
        let mut viewport = ConversationViewport::new(5);
        viewport.on_blocks_changed(projection.blocks().len());
        assert_eq!(viewport.first_visible(), 15);
        assert!(viewport.follow_latest());

        viewport.user_scrolled(4, projection.blocks().len());
        assert!(!viewport.follow_latest());
        viewport.on_blocks_changed(25);
        assert_eq!(viewport.first_visible(), 4);

        viewport.end(&projection);
        assert!(viewport.follow_latest());
        assert_eq!(viewport.first_visible(), 15);
        viewport.home(&projection);
        assert_eq!(viewport.first_visible(), 0);
    }

    #[test]
    fn explicit_pause_and_resume_control_follow_latest() {
        let mut viewport = ConversationViewport::new(5);
        viewport.on_blocks_changed(20);
        assert_eq!(viewport.first_visible(), 15);

        viewport.pause_follow_latest();
        viewport.on_blocks_changed(25);
        assert!(!viewport.follow_latest());
        assert_eq!(viewport.first_visible(), 15);

        viewport.resume_latest(25);
        assert!(viewport.follow_latest());
        assert_eq!(viewport.first_visible(), 20);
        assert_eq!(viewport.unseen_updates(), 0);
    }

    #[test]
    fn pixel_scroll_distance_uses_hysteresis_and_ignores_bottom_jitter() {
        let mut viewport = ConversationViewport::new(5);

        assert!(!viewport.reconcile_scroll_distance(47.9));
        assert!(viewport.follow_latest());
        assert!(viewport.reconcile_scroll_distance(48.1));
        assert!(!viewport.follow_latest());

        assert!(!viewport.reconcile_scroll_distance(32.1));
        assert!(!viewport.follow_latest());
        assert!(viewport.reconcile_scroll_distance(31.9));
        assert!(viewport.follow_latest());
        assert!(!viewport.reconcile_scroll_distance(0.0));
    }

    #[test]
    fn single_row_layout_update_does_not_revisit_ten_thousand_history_rows() {
        let now = Instant::now();
        let mut layout = ConversationRowLayoutState::default();
        let inputs = (0..MAX_TRANSCRIPT_BLOCKS)
            .map(|index| row_layout(&format!("row-{index}"), 100.0, false))
            .collect();
        let initial = layout.resolve(inputs, 960, now, None);
        assert_eq!(initial.heights.len(), MAX_TRANSCRIPT_BLOCKS);
        assert_eq!(layout.full_input_visits, MAX_TRANSCRIPT_BLOCKS);

        let update = layout.resolve_one(
            row_layout("row-9999", 144.0, false),
            960,
            now + Duration::from_millis(1),
        );
        assert_eq!(update.height, 144.0);
        assert_eq!(layout.full_input_visits, MAX_TRANSCRIPT_BLOCKS);
        assert_eq!(layout.single_row_updates, 1);
        assert_eq!(layout.rows.len(), MAX_TRANSCRIPT_BLOCKS);
    }

    #[test]
    fn actual_row_measurement_is_identity_bound_and_updates_only_one_row() {
        let now = Instant::now();
        let mut layout = ConversationRowLayoutState::default();
        let input = row_layout("measured-final", 120., false);
        let item_key = input.item_key.clone();
        let stable_id = item_key.stable_id().to_owned();
        layout.resolve(vec![input], 960, now, None);
        let visits_before = layout.full_input_visits;

        let accepted = layout
            .submit_measurement(
                &ConversationRowMeasurement {
                    item_key: item_key.clone(),
                    source_revision: 1,
                    width_bucket: 960,
                    text_phase: StreamingTextPhase::FinalMarkdown,
                    details_expanded: false,
                    height: 734.25,
                },
                now + Duration::from_millis(1),
            )
            .expect("current final measurement is accepted");
        assert_eq!(accepted.height, 734.25);
        assert_eq!(accepted.source, ConversationRowHeightSource::Measured);
        assert!(accepted.height_changed);
        assert_eq!(layout.full_input_visits, visits_before);

        for stale in [
            ConversationRowMeasurement {
                item_key: item_key.clone(),
                source_revision: 0,
                width_bucket: 960,
                text_phase: StreamingTextPhase::FinalMarkdown,
                details_expanded: false,
                height: 900.,
            },
            ConversationRowMeasurement {
                item_key: item_key.clone(),
                source_revision: 1,
                width_bucket: 936,
                text_phase: StreamingTextPhase::FinalMarkdown,
                details_expanded: false,
                height: 900.,
            },
            ConversationRowMeasurement {
                item_key: item_key.clone(),
                source_revision: 1,
                width_bucket: 960,
                text_phase: StreamingTextPhase::SettlingMarkdown,
                details_expanded: false,
                height: 900.,
            },
            ConversationRowMeasurement {
                item_key,
                source_revision: 1,
                width_bucket: 960,
                text_phase: StreamingTextPhase::FinalMarkdown,
                details_expanded: true,
                height: 900.,
            },
        ] {
            assert!(
                layout
                    .submit_measurement(&stale, now + Duration::from_millis(2))
                    .is_none(),
                "stale measurement must not replace the committed final bounds"
            );
        }
        assert_eq!(layout.rows[&stable_id].committed, 734.25);
        assert_eq!(layout.full_input_visits, visits_before);
    }

    #[test]
    fn single_streaming_row_update_keeps_fifteen_hz_throttle_and_final_settle() {
        let now = Instant::now();
        let mut layout = ConversationRowLayoutState::default();
        layout.resolve(vec![row_layout("live", 100.0, true)], 960, now, None);

        let throttled = layout.resolve_one(
            row_layout("live", 140.0, true),
            960,
            now + Duration::from_millis(1),
        );
        assert_eq!(throttled.height, 100.0);
        assert!(throttled.next_refresh_after.is_some());

        let committed = layout.resolve_one(
            row_layout("live", 140.0, true),
            960,
            now + STREAMING_ROW_HEIGHT_INTERVAL,
        );
        assert_eq!(committed.height, 140.0);
        assert_eq!(committed.next_refresh_after, None);

        let settled = layout.resolve_one(
            row_layout("live", 160.0, false),
            960,
            now + STREAMING_ROW_HEIGHT_INTERVAL + Duration::from_millis(1),
        );
        assert_eq!(settled.height, 160.0);
        assert_eq!(settled.next_refresh_after, None);
    }

    #[test]
    fn streaming_row_height_commits_at_fifteen_hz_and_settles_immediately() {
        let started = Instant::now();
        let mut layout = ConversationRowLayoutState::default();
        let initial = layout.resolve(
            vec![row_layout("assistant:1", 100.0, true)],
            600,
            started,
            None,
        );
        assert_eq!(initial.heights, vec![100.0]);

        let early = layout.resolve(
            vec![row_layout("assistant:1", 120.0, true)],
            600,
            started + Duration::from_millis(16),
            None,
        );
        assert_eq!(early.heights, vec![100.0]);
        assert_eq!(early.next_refresh_after, Some(Duration::from_millis(51)));

        let latest_before_deadline = layout.resolve(
            vec![row_layout("assistant:1", 140.0, true)],
            600,
            started + Duration::from_millis(66),
            None,
        );
        assert_eq!(latest_before_deadline.heights, vec![100.0]);
        assert_eq!(
            latest_before_deadline.next_refresh_after,
            Some(Duration::from_millis(1))
        );

        let committed = layout.resolve(
            vec![row_layout("assistant:1", 140.0, true)],
            600,
            started + STREAMING_ROW_HEIGHT_INTERVAL,
            None,
        );
        assert_eq!(committed.heights, vec![140.0]);
        assert_eq!(committed.next_refresh_after, None);

        let settled = layout.resolve(
            vec![row_layout("assistant:1", 150.0, false)],
            600,
            started + Duration::from_millis(68),
            None,
        );
        assert_eq!(settled.heights, vec![150.0]);
        assert_eq!(settled.next_refresh_after, None);
    }

    #[test]
    fn paused_anchor_survives_growth_insertion_and_eviction_above_it() {
        let started = Instant::now();
        let mut layout = ConversationRowLayoutState::default();
        layout.resolve(
            vec![
                row_layout("a", 100.0, false),
                row_layout("b", 100.0, false),
                row_layout("c", 100.0, false),
            ],
            600,
            started,
            None,
        );

        let grown = layout.resolve(
            vec![
                row_layout("a", 140.0, false),
                row_layout("b", 130.0, false),
                row_layout("c", 100.0, false),
            ],
            600,
            started + Duration::from_millis(1),
            Some(150.0),
        );
        assert_eq!(grown.paused_scroll_top, Some(190.0));

        let inserted = layout.resolve(
            vec![
                row_layout("x", 30.0, false),
                row_layout("a", 140.0, false),
                row_layout("b", 130.0, false),
                row_layout("c", 100.0, false),
            ],
            600,
            started + Duration::from_millis(2),
            grown.paused_scroll_top,
        );
        assert_eq!(inserted.paused_scroll_top, Some(220.0));

        let evicted = layout.resolve(
            vec![
                row_layout("x", 30.0, false),
                row_layout("b", 130.0, false),
                row_layout("c", 100.0, false),
            ],
            600,
            started + Duration::from_millis(3),
            inserted.paused_scroll_top,
        );
        assert_eq!(evicted.paused_scroll_top, Some(80.0));
    }

    #[test]
    fn width_bucket_changes_commit_streaming_height_immediately() {
        assert_eq!(conversation_width_bucket(610), 600);
        assert_eq!(conversation_width_bucket(623), 600);
        assert_eq!(conversation_width_bucket(624), 624);

        let started = Instant::now();
        let mut layout = ConversationRowLayoutState::default();
        layout.resolve(
            vec![row_layout("assistant:1", 100.0, true)],
            600,
            started,
            None,
        );
        let resized = layout.resolve(
            vec![row_layout("assistant:1", 180.0, true)],
            624,
            started + Duration::from_millis(1),
            None,
        );
        assert_eq!(resized.heights, vec![180.0]);
        assert_eq!(resized.next_refresh_after, None);
    }

    #[test]
    fn paused_reader_accumulates_appended_blocks_until_resuming() {
        let mut viewport = ConversationViewport::new(5);
        viewport.on_blocks_changed(20);
        viewport.pause_follow_latest();

        viewport.on_blocks_changed(23);
        viewport.on_blocks_changed(25);
        assert_eq!(viewport.unseen_updates(), 5);

        assert!(viewport.reconcile_scroll_distance(0.0));
        assert!(viewport.follow_latest());
        assert_eq!(viewport.unseen_updates(), 0);
    }

    #[test]
    fn paused_reader_counts_streaming_revisions_without_new_rows() {
        let mut viewport = ConversationViewport::new(5);
        viewport.on_content_changed(20, 40);
        viewport.pause_follow_latest();

        viewport.on_content_changed(20, 41);
        viewport.on_content_changed(20, 41);
        assert_eq!(viewport.unseen_updates(), 1);

        viewport.on_content_changed(22, 42);
        assert_eq!(viewport.unseen_updates(), 3);
    }

    #[test]
    fn composer_submits_exactly_once_and_retains_rejected_draft() {
        let mut composer = ComposerState::default();
        composer.edit("  exact payload\n");
        assert_eq!(
            composer
                .begin_submit(7, ComposerSubmissionKind::Prompt)
                .unwrap(),
            "  exact payload\n"
        );
        assert_eq!(
            composer.begin_submit(8, ComposerSubmissionKind::Prompt),
            Err(ComposerSubmitError::AdmissionPending)
        );
        composer.rejected(7, "queue full").unwrap();
        assert_eq!(composer.draft(), "  exact payload\n");
        assert_eq!(composer.rejection(), Some("queue full"));

        assert_eq!(
            composer
                .begin_submit(9, ComposerSubmissionKind::Prompt)
                .unwrap(),
            "  exact payload\n"
        );
        composer.accepted(9).unwrap();
        assert_eq!(composer.draft(), "");
        assert_eq!(composer.admission(), &ComposerAdmission::Idle);
        assert_eq!(
            composer.submitted(),
            Some(&SubmittedPromptPreview {
                command_id: 9,
                payload: "  exact payload\n".into(),
            })
        );
    }

    #[test]
    fn submitted_prompt_reconciles_selection_to_durable_user_block() {
        let projection = ConversationProjection::hydrate(transcript(vec![
            CodingAgentSessionTranscriptItem::User {
                text: "exact payload".into(),
            },
        ]));
        let mut composer = ComposerState::default();
        composer.edit("exact payload");
        composer
            .begin_submit(7, ComposerSubmissionKind::Prompt)
            .unwrap();
        composer.accepted(7).unwrap();
        let live_id = composer.submitted().unwrap().block_id();
        let mut viewport = ConversationViewport::new(5);
        viewport.select_live(live_id.clone());

        let (reconciled_live, durable_id) = composer
            .reconcile_completed_submission(&projection)
            .unwrap();
        assert_eq!(reconciled_live, live_id);
        viewport.reconcile_live_selection(&reconciled_live, &durable_id);
        viewport.reconcile_hydration(&projection, projection.blocks().len(), 1);
        assert_eq!(viewport.selected_block_id(), Some(durable_id.as_str()));
        assert!(composer.submitted().is_none());
        assert!(composer.rejection().is_none());
    }

    #[test]
    fn completed_hydration_without_submitted_prompt_restores_exact_draft() {
        let projection = ConversationProjection::hydrate(transcript(Vec::new()));
        let mut composer = ComposerState::default();
        composer.edit("  exact payload\n");
        composer
            .begin_submit(7, ComposerSubmissionKind::Prompt)
            .unwrap();
        composer.accepted(7).unwrap();

        assert!(
            composer
                .reconcile_completed_submission(&projection)
                .is_none()
        );
        assert_eq!(composer.draft(), "  exact payload\n");
        assert!(composer.rejection().unwrap().contains("not retained"));
    }

    #[test]
    fn accepted_steer_clears_exact_draft_without_creating_user_overlay() {
        let mut composer = ComposerState::default();
        composer.edit("steer exactly");
        assert_eq!(
            composer
                .begin_submit(8, ComposerSubmissionKind::Steer)
                .unwrap(),
            "steer exactly"
        );
        composer.accepted(8).unwrap();
        assert!(composer.draft().is_empty());
        assert!(composer.submitted().is_none());
        assert_eq!(composer.admission(), &ComposerAdmission::Idle);
    }

    #[test]
    fn rejected_follow_up_retains_exact_draft() {
        let mut composer = ComposerState::default();
        composer.edit("  follow up exactly\n");
        composer
            .begin_submit(9, ComposerSubmissionKind::FollowUp)
            .unwrap();
        composer.rejected(9, "operation completed").unwrap();

        assert_eq!(composer.draft(), "  follow up exactly\n");
        assert_eq!(composer.rejection(), Some("operation completed"));
        assert!(composer.submitted().is_none());
        assert_eq!(composer.admission(), &ComposerAdmission::Idle);
    }

    #[test]
    fn composer_rejects_empty_oversized_and_stale_completion() {
        let mut composer = ComposerState::default();
        assert_eq!(
            composer.begin_submit(1, ComposerSubmissionKind::Prompt),
            Err(ComposerSubmitError::Empty)
        );
        composer.edit("x".repeat(MAX_COMPOSER_BYTES + 1));
        assert_eq!(
            composer.begin_submit(2, ComposerSubmissionKind::Prompt),
            Err(ComposerSubmitError::TooLarge)
        );
        composer.edit("valid");
        composer
            .begin_submit(3, ComposerSubmissionKind::Prompt)
            .unwrap();
        assert_eq!(
            composer.accepted(4),
            Err(ComposerSubmitError::StaleCompletion)
        );
        assert_eq!(composer.draft(), "valid");
    }
}
