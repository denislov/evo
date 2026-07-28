//! Revision-aware, memory-bounded transcript row presentation cache.

use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use unicode_width::UnicodeWidthStr as _;

use super::{
    ConversationBlockKind, ConversationItemKey, MAX_TRANSCRIPT_BLOCKS, bounded_markdown_preview,
};
use crate::shell::USER_MESSAGE_WIDTH_PERCENT;

pub const STREAMING_MARKDOWN_SETTLE_DELAY: Duration = Duration::from_millis(100);
pub const MAX_SETTLING_MARKDOWN_BYTES: usize = 64 * 1024;
pub const MAX_ROW_RENDER_CACHE_ENTRIES: usize = MAX_TRANSCRIPT_BLOCKS + 256;
pub const MAX_ROW_RENDER_CACHE_BYTES: usize = 40 * 1024 * 1024;

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
    #[cfg(test)]
    pub(super) const fn retained_bytes(&self) -> usize {
        self.retained_bytes
    }

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

#[cfg(test)]
mod tests {
    use std::borrow::Cow;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use super::{
        ConversationRowRenderCache, ConversationRowRenderSource, MAX_SETTLING_MARKDOWN_BYTES,
        STREAMING_MARKDOWN_SETTLE_DELAY, StreamingTextPhase,
    };
    use crate::conversation::{ConversationBlockKind, ConversationItemKey, ConversationItemKind};

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
    fn conversation_row_estimates_use_display_width_for_unicode() {
        assert_eq!(super::estimated_text_rows("abcdefghij", 10, 20), 1);
        assert_eq!(super::estimated_text_rows("界界界界界", 10, 20), 1);
        assert_eq!(super::estimated_text_rows("界界界界界界", 10, 20), 2);
        assert_eq!(super::estimated_text_rows("🙂🙂🙂🙂🙂", 10, 20), 1);
        assert_eq!(
            super::estimated_text_rows("e\u{301}e\u{301}e\u{301}", 3, 20),
            1
        );
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
}
