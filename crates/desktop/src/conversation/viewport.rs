//! Conversation selection, follow-latest, and unseen-update state.

use super::{ConversationBlock, ConversationProjection};

/// A reader who moves farther than this from the bottom owns the viewport.
pub const FOLLOW_LATEST_PAUSE_THRESHOLD_PX: f32 = 48.0;
/// A paused reader must return this close to the bottom before following resumes.
pub const FOLLOW_LATEST_RESUME_THRESHOLD_PX: f32 = 32.0;

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

#[cfg(test)]
mod tests {
    use super::*;
    use coding_agent::api::view::{
        CodingAgentSessionTranscriptItem, CodingAgentTranscriptSnapshot,
    };

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
}
