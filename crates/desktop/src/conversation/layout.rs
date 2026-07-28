//! Conversation row measurement, height commit, and width-bucket state.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use super::{ConversationItemKey, render_cache::StreamingTextPhase};

pub const STREAMING_ROW_HEIGHT_INTERVAL: Duration = Duration::from_millis(67);
pub const CONVERSATION_WIDTH_BUCKET_PX: u32 = 24;
/// Maximum height used only for an explicitly collapsed secondary-detail preview.
///
/// Normal conversation rows are not capped: their estimate is replaced by a
/// layout measurement from the rendered GPUI element.
pub const TRANSCRIPT_COLLAPSED_PREVIEW_MAX_HEIGHT: f32 = 680.0;

pub const fn conversation_width_bucket(panel_width: u32) -> u32 {
    let bucket = panel_width / CONVERSATION_WIDTH_BUCKET_PX;
    if bucket == 0 {
        CONVERSATION_WIDTH_BUCKET_PX
    } else {
        bucket * CONVERSATION_WIDTH_BUCKET_PX
    }
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

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;
    use crate::conversation::{ConversationBlockKind, ConversationItemKind, MAX_TRANSCRIPT_BLOCKS};

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
}
