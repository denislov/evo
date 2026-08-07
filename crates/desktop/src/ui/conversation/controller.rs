use std::{
    cell::RefCell,
    collections::{HashSet, VecDeque, vec_deque},
    iter::Peekable,
    rc::Rc,
    time::{Duration, Instant},
};

use gpui::{FollowMode, ListAlignment, ListOffset, ListState, ScrollStrategy, px};

use super::{
    ConversationBlockKind, ConversationItemKey, ConversationItemKind, ConversationProjection,
    ConversationRowRenderCache, ConversationRowRenderData, ConversationRowRenderSource,
    ConversationViewport, SubmittedPromptPreview,
};
use desktop::projection::{
    DesktopMessageOverlay, DesktopProjection, DesktopProjectionDelta, DesktopToolOverlay,
};

pub(crate) const RESIZE_DEBOUNCE: Duration = Duration::from_millis(67);
pub(crate) const MAX_DIRTY_SEQUENCES: usize = 256;
pub(crate) const MAX_EXPANDED_DETAILS: usize = 256;

const INITIAL_VISIBLE_ROWS: usize = 8;

/// The bounded conversation inputs the controller is allowed to read.
///
/// The controller never receives `NativeShell`, so row construction, layout and
/// reconciliation cannot reach global UI state, preferences or the command
/// ledger. Every borrow is a projection slice plus the optimistic submitted
/// prompt overlay that has no durable block yet.
pub(crate) struct ConversationSource<'a> {
    conversation: &'a ConversationProjection,
    submitted: Option<&'a SubmittedPromptPreview>,
    messages: &'a VecDeque<DesktopMessageOverlay>,
    tools: &'a VecDeque<DesktopToolOverlay>,
}

impl<'a> ConversationSource<'a> {
    pub(crate) fn new(
        projection: &'a DesktopProjection,
        submitted: Option<&'a SubmittedPromptPreview>,
    ) -> Self {
        Self {
            conversation: projection.conversation(),
            submitted,
            messages: projection.messages(),
            tools: projection.tools(),
        }
    }

    pub(crate) const fn conversation(&self) -> &'a ConversationProjection {
        self.conversation
    }

    pub(crate) fn session_id(&self) -> &'a str {
        self.conversation.session_id.as_str()
    }

    pub(crate) fn durable_count(&self) -> usize {
        self.conversation.blocks().len()
    }

    pub(crate) fn submitted_count(&self) -> usize {
        usize::from(self.submitted.is_some())
    }

    /// Durable blocks plus the optimistic submitted row plus live overlays.
    pub(crate) fn visible_count(&self) -> usize {
        self.durable_count() + self.submitted_count() + self.messages.len() + self.tools.len()
    }

    /// Live overlays in the order the turn actually produced them.
    ///
    /// Messages and tools are folded onto two independent queues, so neither
    /// one alone carries the interleaving. Concatenating them instead — every
    /// message, then every tool — sinks each running tool below the assistant
    /// message that came after it, and shifts it again on every new message.
    /// Both queues are already ascending in `started_sequence`, so this is a
    /// merge rather than a sort: no allocation, no reordering.
    pub(crate) fn live_overlays(&self) -> LiveOverlayIter<'a> {
        LiveOverlayIter {
            messages: self.messages.iter().peekable(),
            tools: self.tools.iter().peekable(),
        }
    }
}

/// One row of the live tail, in event order.
#[derive(Clone, Copy)]
pub(crate) enum LiveOverlay<'a> {
    Message(&'a DesktopMessageOverlay),
    Tool(&'a DesktopToolOverlay),
}

impl LiveOverlay<'_> {
    const fn updated_sequence(self) -> u64 {
        match self {
            Self::Message(message) => message.updated_sequence,
            Self::Tool(tool) => tool.updated_sequence,
        }
    }

    fn row_id(self) -> String {
        match self {
            Self::Message(message) => message_block_id(message),
            Self::Tool(tool) => tool_block_id(tool),
        }
    }
}

pub(crate) struct LiveOverlayIter<'a> {
    messages: Peekable<vec_deque::Iter<'a, DesktopMessageOverlay>>,
    tools: Peekable<vec_deque::Iter<'a, DesktopToolOverlay>>,
}

impl<'a> Iterator for LiveOverlayIter<'a> {
    type Item = LiveOverlay<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        let next_message = self.messages.peek().map(|message| message.started_sequence);
        let next_tool = self.tools.peek().map(|tool| tool.started_sequence);
        match (next_message, next_tool) {
            (Some(message), Some(tool)) if tool < message => {
                self.tools.next().map(LiveOverlay::Tool)
            }
            (Some(_), _) => self.messages.next().map(LiveOverlay::Message),
            (None, Some(_)) => self.tools.next().map(LiveOverlay::Tool),
            (None, None) => None,
        }
    }
}

pub(crate) struct ConversationController {
    viewport: ConversationViewport,
    pub(crate) scroll: ListState,
    render_cache: ConversationRowRenderCache,
    render_rows: Rc<RefCell<Vec<ConversationRowRenderData>>>,
    render_full_dirty: bool,
    render_live_dirty: bool,
    render_dirty_sequences: VecDeque<u64>,
    render_sequence_overflow: bool,
    render_width_bucket: Option<u32>,
    width_pending: Option<(u32, Instant)>,
    expanded_details: HashSet<String>,
}

mod rows;
#[cfg(test)]
mod tests;

impl Default for ConversationController {
    fn default() -> Self {
        let scroll = ListState::new(0, ListAlignment::Top, px(1_000.));
        scroll.set_follow_mode(FollowMode::Tail);
        Self {
            viewport: ConversationViewport::new(INITIAL_VISIBLE_ROWS),
            scroll,
            render_cache: ConversationRowRenderCache::default(),
            render_rows: Rc::new(RefCell::new(Vec::new())),
            render_full_dirty: true,
            render_live_dirty: true,
            render_dirty_sequences: VecDeque::new(),
            render_sequence_overflow: false,
            render_width_bucket: None,
            width_pending: None,
            expanded_details: HashSet::new(),
        }
    }
}

#[derive(Clone)]
pub(crate) struct ConversationRenderReader {
    rows: Rc<RefCell<Vec<ConversationRowRenderData>>>,
}

impl ConversationRenderReader {
    pub(crate) fn row(&self, index: usize) -> Option<ConversationRowRenderData> {
        self.rows.borrow().get(index).cloned()
    }

    pub(crate) fn len(&self) -> usize {
        self.rows.borrow().len()
    }
}

impl ConversationController {
    pub(crate) fn render_reader(&self) -> ConversationRenderReader {
        ConversationRenderReader {
            rows: Rc::clone(&self.render_rows),
        }
    }

    // ---- bounded presentation state readers -------------------------------

    pub(crate) fn follow_latest_enabled(&self) -> bool {
        self.viewport.follow_latest()
    }

    pub(crate) fn unseen_updates(&self) -> usize {
        self.viewport.unseen_updates()
    }

    pub(crate) fn selected_block_id(&self) -> Option<&str> {
        self.viewport.selected_block_id()
    }

    pub(crate) fn expanded_details(&self) -> &HashSet<String> {
        &self.expanded_details
    }

    pub(crate) fn row_count(&self) -> usize {
        self.render_rows.borrow().len()
    }

    pub(crate) fn row_at(&self, index: usize) -> Option<ConversationRowRenderData> {
        self.render_rows.borrow().get(index).cloned()
    }

    pub(crate) fn row_index(&self, block_id: &str) -> Option<usize> {
        self.render_rows
            .borrow()
            .iter()
            .position(|row| row.item_key.row_id() == block_id)
    }

    pub(crate) fn row_for_block(&self, block_id: &str) -> Option<ConversationRowRenderData> {
        self.render_rows
            .borrow()
            .iter()
            .find(|row| row.item_key.row_id() == block_id)
            .cloned()
    }

    pub(crate) fn copy_selected(&self, conversation: &ConversationProjection) -> Option<String> {
        self.viewport.copy_selected(conversation)
    }

    pub(crate) const fn active_width_bucket(&self) -> Option<u32> {
        self.render_width_bucket
    }

    pub(crate) fn needs_row_refresh(&self) -> bool {
        self.render_full_dirty || self.render_live_dirty
    }

    // ---- selection --------------------------------------------------------

    pub(crate) fn select_row(
        &mut self,
        block_id: String,
        durable: bool,
        conversation: &ConversationProjection,
    ) {
        if durable {
            self.viewport.select(block_id, conversation);
        } else {
            self.viewport.select_live(block_id);
        }
    }

    pub(crate) fn scroll_to_row(&self, index: usize, strategy: ScrollStrategy) {
        match strategy {
            ScrollStrategy::Top => self.scroll.scroll_to(ListOffset {
                item_ix: index,
                offset_in_item: px(0.),
            }),
            ScrollStrategy::Bottom | ScrollStrategy::Center | ScrollStrategy::Nearest => {
                self.scroll.scroll_to_reveal_item(index);
            }
        }
    }

    pub(crate) fn reconcile_live_selection(&mut self, live_id: &str, durable_id: &str) {
        self.viewport.reconcile_live_selection(live_id, durable_id);
    }

    /// Toggle a row detail disclosure and mark the durable layout dirty.
    ///
    /// The expanded set is bounded; overflowing it collapses everything rather
    /// than retaining unbounded per-row UI state.
    pub(crate) fn toggle_details(&mut self, block_id: &str) {
        let row_index = self
            .render_rows
            .borrow()
            .iter()
            .position(|row| row.item_key.row_id() == block_id);
        let collapsed = self.expanded_details.remove(block_id);
        if !collapsed {
            if self.expanded_details.len() >= MAX_EXPANDED_DETAILS {
                self.expanded_details.clear();
            }
            self.expanded_details.insert(block_id.to_owned());
        }
        if let Some(index) = row_index {
            self.scroll.remeasure_items(index..index + 1);
        }
        self.render_full_dirty = true;
    }

    // ---- projection delta -> controller dirty state ------------------------

    /// Apply a routed projection delta to the bounded row-dirty state.
    ///
    /// A session replacement invalidates durable and live rows. A conversation
    /// delta only records the event sequence so the next frame can reconcile
    /// the live tail without rescanning the whole history; once the bounded
    /// sequence window overflows the tail is rebuilt instead.
    pub(crate) fn apply_projection_delta(
        &mut self,
        replaced: bool,
        delta: Option<&DesktopProjectionDelta>,
        last_event_sequence: u64,
    ) {
        if replaced {
            self.render_full_dirty = true;
            self.render_live_dirty = true;
            self.render_dirty_sequences.clear();
            self.render_sequence_overflow = false;
            return;
        }
        if !delta.is_some_and(|delta| delta.conversation || delta.tools) {
            return;
        }

        self.render_live_dirty = true;
        if self.render_sequence_overflow {
            // A bounded tail reconcile is already required.
        } else if self.render_dirty_sequences.len() == MAX_DIRTY_SEQUENCES {
            self.render_dirty_sequences.clear();
            self.render_sequence_overflow = true;
        } else {
            self.render_dirty_sequences.push_back(last_event_sequence);
        }
    }

    /// Reconcile the viewport after the projection replaced the transcript.
    pub(crate) fn reconcile_hydration(
        &mut self,
        source: &ConversationSource<'_>,
        content_revision: u64,
    ) {
        let visible_blocks = source.visible_count();
        self.viewport
            .reconcile_hydration(source.conversation(), visible_blocks, content_revision);
        self.follow_latest_tail(visible_blocks);
    }

    /// Reconcile the viewport after a streaming content change.
    pub(crate) fn reconcile_content(
        &mut self,
        source: &ConversationSource<'_>,
        content_revision: u64,
    ) {
        let visible_blocks = source.visible_count();
        self.viewport
            .on_content_changed(visible_blocks, content_revision);
        self.follow_latest_tail(visible_blocks);
    }

    fn follow_latest_tail(&self, visible_blocks: usize) {
        if self.viewport.follow_latest() && visible_blocks > 0 {
            self.scroll.scroll_to_end();
        }
    }

    pub(crate) fn mark_live_dirty(&mut self) {
        self.render_live_dirty = true;
    }

    // ---- scrolling --------------------------------------------------------

    pub(crate) fn follow_latest(&mut self, visible_count: usize) {
        self.viewport.resume_latest(visible_count);
        self.align_scroll_to_bottom(visible_count);
    }

    /// The single way this controller pins the native list to the newest row.
    ///
    /// `FollowMode::Tail` keeps that invariant active while the last item's
    /// natural height changes, including after asynchronous Markdown parses.
    pub(crate) fn align_scroll_to_bottom(&self, block_count: usize) {
        self.scroll.set_follow_mode(FollowMode::Tail);
        if block_count > 0 {
            self.scroll.scroll_to_end();
        }
    }

    /// Returns whether follow-latest hysteresis changed and the pane and header
    /// have to be notified.
    pub(crate) fn reconcile_scroll(&mut self) -> bool {
        let offset_y = f32::from(self.scroll.scroll_px_offset_for_scrollbar().y);
        let max_offset_y = f32::from(self.scroll.max_offset_for_scrollbar().y);
        let changed = self
            .viewport
            .reconcile_scroll_distance(distance_to_bottom(offset_y, max_offset_y));
        if self.viewport.follow_latest() {
            self.scroll.set_follow_mode(FollowMode::Tail);
        } else {
            self.scroll.set_follow_mode(FollowMode::Normal);
        }
        changed
    }

    // ---- width debounce ---------------------------------------------------

    /// Resolve the width bucket to render at, debouncing live resizes so a drag
    /// does not rebuild the full transcript on every frame.
    pub(crate) fn width_for_render(&mut self, requested: u32) -> (u32, Option<(u32, Instant)>) {
        let Some(active) = self.render_width_bucket else {
            self.width_pending = None;
            return (requested, None);
        };
        if active == requested {
            self.width_pending = None;
            return (active, None);
        }

        let now = Instant::now();
        if let Some((pending, deadline)) = self.width_pending
            && pending == requested
        {
            if now >= deadline {
                self.width_pending = None;
                self.render_full_dirty = true;
                return (requested, None);
            }
            return (active, None);
        }

        let deadline = now + RESIZE_DEBOUNCE;
        self.width_pending = Some((requested, deadline));
        (active, Some((requested, deadline)))
    }

    /// Commit a debounced width once its timer fired, if it is still pending.
    pub(crate) fn commit_pending_width(&mut self, requested: u32, deadline: Instant) -> bool {
        if self.width_pending == Some((requested, deadline)) {
            self.render_full_dirty = true;
            true
        } else {
            false
        }
    }

    pub(crate) fn commit_current_pending_width(&mut self) -> bool {
        let Some((requested, deadline)) = self.width_pending else {
            return false;
        };
        self.commit_pending_width(requested, deadline)
    }
}

#[cfg(test)]
impl ConversationController {
    pub(crate) fn last_row_id_for_tests(&self) -> Option<String> {
        self.render_rows
            .borrow()
            .last()
            .map(|row| row.item_key.row_id().to_owned())
    }
}

pub(crate) fn distance_to_bottom(offset_y: f32, max_offset_y: f32) -> f32 {
    (max_offset_y.max(0.0) + offset_y.min(0.0)).max(0.0)
}

pub(crate) fn adjacent_conversation_index(
    row_count: usize,
    current_index: Option<usize>,
    reverse: bool,
) -> Option<usize> {
    let last_index = row_count.checked_sub(1)?;
    Some(
        match (current_index.filter(|index| *index < row_count), reverse) {
            (Some(index), true) => index.saturating_sub(1),
            (Some(index), false) => index.saturating_add(1).min(last_index),
            (None, true) => last_index,
            (None, false) => 0,
        },
    )
}

pub(crate) fn upsert_indexed_item<T>(
    items: &mut Vec<T>,
    existing_index: Option<usize>,
    mut desired_index: usize,
    item: T,
) -> usize {
    if let Some(existing_index) = existing_index {
        if existing_index == desired_index {
            items[existing_index] = item;
            return existing_index;
        }
        items.remove(existing_index);
        if existing_index < desired_index {
            desired_index = desired_index.saturating_sub(1);
        }
    }
    desired_index = desired_index.min(items.len());
    items.insert(desired_index, item);
    desired_index
}

/// Reconcile GPUI's measured list items with a replacement row projection.
///
/// Stable prefix/suffix items retain their measured size hints. Changed items
/// are spliced, then every surviving item is marked for natural remeasurement;
/// GPUI only lays out the visible/overdraw range and preserves the current
/// logical scroll anchor while doing so.
fn sync_list_rows(
    list: &ListState,
    previous: &[ConversationRowRenderData],
    next: &[ConversationRowRenderData],
) {
    let mut prefix = 0;
    while prefix < previous.len().min(next.len())
        && previous[prefix].item_key == next[prefix].item_key
    {
        prefix += 1;
    }

    let mut suffix = 0;
    while suffix
        < previous
            .len()
            .saturating_sub(prefix)
            .min(next.len().saturating_sub(prefix))
        && previous[previous.len() - 1 - suffix].item_key == next[next.len() - 1 - suffix].item_key
    {
        suffix += 1;
    }

    let old_end = previous.len().saturating_sub(suffix);
    let replacement_count = next.len().saturating_sub(prefix + suffix);
    if prefix != old_end || replacement_count != 0 {
        list.splice(prefix..old_end, replacement_count);
    }
    if !next.is_empty() {
        list.remeasure_items(0..next.len());
    }
}

pub(crate) fn message_block_id(message: &DesktopMessageOverlay) -> String {
    message.message_id.as_ref().map_or_else(
        || format!("assistant:{}:{}", message.operation_id, message.turn_id),
        |message_id| format!("assistant:{message_id}"),
    )
}

pub(crate) fn tool_block_id(tool: &DesktopToolOverlay) -> String {
    format!("tool:{}", tool.tool_call_id)
}
