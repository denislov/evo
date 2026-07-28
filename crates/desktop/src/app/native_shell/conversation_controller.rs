use std::{
    borrow::Cow,
    cell::RefCell,
    collections::{HashMap, HashSet, VecDeque},
    rc::Rc,
    time::{Duration, Instant},
};

use gpui::{ScrollStrategy, px, size};
use gpui_component::VirtualListScrollHandle;

use crate::conversation::{
    ConversationBlockKind, ConversationItemKey, ConversationItemKind, ConversationProjection,
    ConversationRowLayoutInput, ConversationRowLayoutState, ConversationRowMeasurement,
    ConversationRowRenderCache, ConversationRowRenderData, ConversationRowRenderSource,
    ConversationViewport, SubmittedPromptPreview, TRANSCRIPT_COLLAPSED_PREVIEW_MAX_HEIGHT,
    conversation_block_height,
};
use desktop::projection::{
    DesktopMessageOverlay, DesktopMessageStatus, DesktopProjection, DesktopToolOverlay,
    DesktopToolStatus,
};

pub(super) const RESIZE_DEBOUNCE: Duration = Duration::from_millis(67);
pub(super) const MAX_SESSION_VIEW_STATES: usize = 256;
pub(super) const MAX_DIRTY_SEQUENCES: usize = 256;
pub(super) const MAX_EXPANDED_DETAILS: usize = 256;

const COLLAPSED_DETAIL_HEIGHT: f32 = 36.;
const INITIAL_VISIBLE_ROWS: usize = 8;

/// The bounded conversation inputs the controller is allowed to read.
///
/// The controller never receives `NativeShell`, so row construction, layout and
/// reconciliation cannot reach global UI state, preferences or the command
/// ledger. Every borrow is a projection slice plus the optimistic submitted
/// prompt overlay that has no durable block yet.
pub(super) struct ConversationSource<'a> {
    conversation: &'a ConversationProjection,
    submitted: Option<&'a SubmittedPromptPreview>,
    messages: &'a VecDeque<DesktopMessageOverlay>,
    tools: &'a VecDeque<DesktopToolOverlay>,
}

#[derive(Clone)]
struct ConversationToggleAnchor {
    item_key: ConversationItemKey,
    source_revision: u64,
    width_bucket: u32,
    details_expanded: bool,
    row_top: f32,
    scroll_top: f32,
    layout_applied: bool,
}

impl<'a> ConversationSource<'a> {
    pub(super) fn new(
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

    pub(super) const fn conversation(&self) -> &'a ConversationProjection {
        self.conversation
    }

    pub(super) fn session_id(&self) -> &'a str {
        self.conversation.session_id.as_str()
    }

    pub(super) fn durable_count(&self) -> usize {
        self.conversation.blocks().len()
    }

    pub(super) fn submitted_count(&self) -> usize {
        usize::from(self.submitted.is_some())
    }

    /// Durable blocks plus the optimistic submitted row plus live overlays.
    pub(super) fn visible_count(&self) -> usize {
        self.durable_count() + self.submitted_count() + self.messages.len() + self.tools.len()
    }
}

pub(super) struct ConversationController {
    viewport: ConversationViewport,
    pub(super) scroll: VirtualListScrollHandle,
    session_views: HashMap<String, ConversationSessionViewState>,
    pending_scroll_restore: Option<f32>,
    layout: ConversationRowLayoutState,
    live_layout: ConversationRowLayoutState,
    render_cache: ConversationRowRenderCache,
    render_rows: Rc<RefCell<Vec<ConversationRowRenderData>>>,
    render_heights: Rc<RefCell<Vec<f32>>>,
    row_sizes: Rc<RefCell<Rc<Vec<gpui::Size<gpui::Pixels>>>>>,
    render_full_dirty: bool,
    render_live_dirty: bool,
    render_dirty_sequences: VecDeque<u64>,
    render_sequence_overflow: bool,
    render_width_bucket: Option<u32>,
    width_pending: Option<(u32, Instant)>,
    height_refresh_deadline: Option<Instant>,
    height_refresh_full: bool,
    expanded_details: HashSet<String>,
    toggle_anchor: Option<ConversationToggleAnchor>,
}

impl Default for ConversationController {
    fn default() -> Self {
        Self {
            viewport: ConversationViewport::new(INITIAL_VISIBLE_ROWS),
            scroll: VirtualListScrollHandle::new(),
            session_views: HashMap::new(),
            pending_scroll_restore: None,
            layout: ConversationRowLayoutState::default(),
            live_layout: ConversationRowLayoutState::default(),
            render_cache: ConversationRowRenderCache::default(),
            render_rows: Rc::new(RefCell::new(Vec::new())),
            render_heights: Rc::new(RefCell::new(Vec::new())),
            row_sizes: Rc::new(RefCell::new(Rc::new(Vec::new()))),
            render_full_dirty: true,
            render_live_dirty: true,
            render_dirty_sequences: VecDeque::new(),
            render_sequence_overflow: false,
            render_width_bucket: None,
            width_pending: None,
            height_refresh_deadline: None,
            height_refresh_full: false,
            expanded_details: HashSet::new(),
            toggle_anchor: None,
        }
    }
}

#[derive(Clone)]
pub(super) struct ConversationRenderReader {
    rows: Rc<RefCell<Vec<ConversationRowRenderData>>>,
    heights: Rc<RefCell<Vec<f32>>>,
    row_sizes: Rc<RefCell<Rc<Vec<gpui::Size<gpui::Pixels>>>>>,
}

impl ConversationRenderReader {
    pub(super) fn row(&self, index: usize) -> Option<(ConversationRowRenderData, f32)> {
        let rows = self.rows.borrow();
        let row = rows.get(index)?.clone();
        let height = self
            .heights
            .borrow()
            .get(index)
            .copied()
            .unwrap_or(row.estimated_height);
        Some((row, height))
    }

    pub(super) fn len(&self) -> usize {
        self.rows.borrow().len()
    }

    pub(super) fn row_sizes(&self) -> Rc<Vec<gpui::Size<gpui::Pixels>>> {
        self.row_sizes.borrow().clone()
    }
}

#[derive(Clone)]
pub(super) struct ConversationSessionViewState {
    viewport: ConversationViewport,
    scroll_top: f32,
    expanded_details: HashSet<String>,
}

#[cfg(test)]
impl ConversationSessionViewState {
    pub(super) const fn scroll_top(&self) -> f32 {
        self.scroll_top
    }

    pub(super) const fn viewport(&self) -> &ConversationViewport {
        &self.viewport
    }
}

/// A height refresh the Root still has to arm on the GPUI executor.
pub(super) type ConversationRefresh = Option<(Duration, bool)>;

/// What the Root must do after the controller consumed a row measurement.
#[derive(Default)]
pub(super) struct ConversationMeasurementOutcome {
    pub(super) refresh: ConversationRefresh,
    pub(super) pane_dirty: bool,
}

impl ConversationController {
    pub(super) fn render_reader(&self) -> ConversationRenderReader {
        ConversationRenderReader {
            rows: Rc::clone(&self.render_rows),
            heights: Rc::clone(&self.render_heights),
            row_sizes: Rc::clone(&self.row_sizes),
        }
    }

    // ---- bounded presentation state readers -------------------------------

    pub(super) fn follow_latest_enabled(&self) -> bool {
        self.viewport.follow_latest()
    }

    pub(super) fn unseen_updates(&self) -> usize {
        self.viewport.unseen_updates()
    }

    pub(super) fn selected_block_id(&self) -> Option<&str> {
        self.viewport.selected_block_id()
    }

    pub(super) fn expanded_details(&self) -> &HashSet<String> {
        &self.expanded_details
    }

    pub(super) fn row_count(&self) -> usize {
        self.render_rows.borrow().len()
    }

    pub(super) fn row_at(&self, index: usize) -> Option<ConversationRowRenderData> {
        self.render_rows.borrow().get(index).cloned()
    }

    pub(super) fn row_index(&self, block_id: &str) -> Option<usize> {
        self.render_rows
            .borrow()
            .iter()
            .position(|row| row.item_key.row_id() == block_id)
    }

    pub(super) fn row_for_block(&self, block_id: &str) -> Option<ConversationRowRenderData> {
        self.render_rows
            .borrow()
            .iter()
            .find(|row| row.item_key.row_id() == block_id)
            .cloned()
    }

    pub(super) fn copy_selected(&self, conversation: &ConversationProjection) -> Option<String> {
        self.viewport.copy_selected(conversation)
    }

    pub(super) const fn active_width_bucket(&self) -> Option<u32> {
        self.render_width_bucket
    }

    pub(super) fn needs_row_refresh(&self) -> bool {
        self.render_full_dirty || self.render_live_dirty
    }

    // ---- selection --------------------------------------------------------

    pub(super) fn select_row(
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

    pub(super) fn scroll_to_row(&self, index: usize, strategy: ScrollStrategy) {
        self.scroll.scroll_to_item(index, strategy);
    }

    pub(super) fn reconcile_live_selection(&mut self, live_id: &str, durable_id: &str) {
        self.viewport.reconcile_live_selection(live_id, durable_id);
    }

    /// Toggle a row detail disclosure and mark the durable layout dirty.
    ///
    /// The expanded set is bounded; overflowing it collapses everything rather
    /// than retaining unbounded per-row UI state.
    pub(super) fn toggle_details(&mut self, block_id: &str) {
        let anchor = {
            let rows = self.render_rows.borrow();
            let heights = self.render_heights.borrow();
            rows.iter()
                .position(|row| row.item_key.row_id() == block_id)
                .and_then(|index| {
                    let row = rows.get(index)?;
                    let row_top = heights.get(..index)?.iter().copied().sum();
                    Some(ConversationToggleAnchor {
                        item_key: row.item_key.clone(),
                        source_revision: row.source_revision,
                        width_bucket: row.width_bucket,
                        details_expanded: false,
                        row_top,
                        scroll_top: (-f32::from(self.scroll.offset().y)).max(0.),
                        layout_applied: false,
                    })
                })
        };
        let details_expanded = if self.expanded_details.remove(block_id) {
            false
        } else {
            if self.expanded_details.len() >= MAX_EXPANDED_DETAILS {
                self.expanded_details.clear();
            }
            self.expanded_details.insert(block_id.to_owned());
            true
        };
        self.toggle_anchor = anchor.map(|mut anchor| {
            anchor.details_expanded = details_expanded;
            anchor
        });
        self.render_full_dirty = true;
    }

    // ---- projection delta -> controller dirty state ------------------------

    /// Apply a routed projection delta to the bounded row-dirty state.
    ///
    /// A session replacement invalidates durable and live rows. A conversation
    /// delta only records the event sequence so the next frame can reconcile
    /// the live tail without rescanning the whole history; once the bounded
    /// sequence window overflows the tail is rebuilt instead.
    pub(super) fn apply_delta(&mut self, sessions_replaced: bool, last_event_sequence: u64) {
        if sessions_replaced {
            self.render_full_dirty = true;
            self.render_live_dirty = true;
            self.render_dirty_sequences.clear();
            self.render_sequence_overflow = false;
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
    pub(super) fn reconcile_hydration(
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
    pub(super) fn reconcile_content(
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
            self.scroll
                .scroll_to_item(visible_blocks - 1, ScrollStrategy::Bottom);
        }
    }

    pub(super) fn mark_live_dirty(&mut self) {
        self.render_live_dirty = true;
    }

    // ---- session-scoped view state ----------------------------------------

    /// Park the outgoing session's viewport/scroll/expansion and restore the
    /// incoming session's, so conversation state never leaks across sessions.
    pub(super) fn reconcile_session_view(
        &mut self,
        previous_session_id: &str,
        next_session_id: &str,
    ) -> bool {
        if previous_session_id == next_session_id {
            return false;
        }
        self.toggle_anchor = None;
        let scroll_top = (-f32::from(self.scroll.offset().y)).max(0.);
        if self.session_views.len() >= MAX_SESSION_VIEW_STATES
            && !self.session_views.contains_key(previous_session_id)
            && let Some(stale) = self.session_views.keys().next().cloned()
        {
            self.session_views.remove(&stale);
        }
        self.session_views.insert(
            previous_session_id.to_owned(),
            ConversationSessionViewState {
                viewport: self.viewport.clone(),
                scroll_top: if scroll_top.is_finite() {
                    scroll_top.max(0.)
                } else {
                    0.
                },
                expanded_details: std::mem::take(&mut self.expanded_details),
            },
        );
        if let Some(state) = self.session_views.remove(next_session_id) {
            self.viewport = state.viewport;
            self.expanded_details = state.expanded_details;
            self.pending_scroll_restore = Some(state.scroll_top);
        } else {
            self.viewport = ConversationViewport::new(INITIAL_VISIBLE_ROWS);
            self.pending_scroll_restore = Some(0.);
        }
        true
    }

    // ---- scrolling --------------------------------------------------------

    pub(super) fn follow_latest(&mut self, visible_count: usize) {
        self.viewport.resume_latest(visible_count);
        self.align_scroll_to_bottom(visible_count);
    }

    pub(super) fn align_scroll_to_bottom(&self, block_count: usize) {
        if block_count == 0 {
            let mut offset = self.scroll.offset();
            offset.y = px(0.);
            self.scroll.set_offset(offset);
            return;
        }

        let viewport_height = f32::from(self.scroll.bounds().size.height);
        if viewport_height > 0. && self.render_heights.borrow().len() == block_count {
            let content_height = self.render_heights.borrow().iter().copied().sum::<f32>();
            let mut offset = self.scroll.offset();
            offset.y = px((viewport_height - content_height).min(0.));
            self.scroll.set_offset(offset);
        } else {
            self.scroll
                .scroll_to_item(block_count - 1, ScrollStrategy::Bottom);
        }
    }

    /// Returns whether follow-latest hysteresis changed and the pane and header
    /// have to be notified.
    pub(super) fn reconcile_scroll(&mut self) -> bool {
        let offset_y = f32::from(self.scroll.offset().y);
        let max_offset_y = f32::from(self.scroll.max_offset().y);
        self.viewport
            .reconcile_scroll_distance(distance_to_bottom(offset_y, max_offset_y))
    }

    // ---- measurement ------------------------------------------------------

    pub(super) fn submit_row_measurement(
        &mut self,
        source: &ConversationSource<'_>,
        measurement: &ConversationRowMeasurement,
    ) -> ConversationMeasurementOutcome {
        let mut outcome = ConversationMeasurementOutcome::default();
        let Some(index) = self
            .render_rows
            .borrow()
            .iter()
            .position(|row| row.item_key == measurement.item_key)
        else {
            tracing::trace!(target: "desktop", event = "row_measure_stale_drop", reason = "unmounted");
            return outcome;
        };
        let row = self.render_rows.borrow()[index].clone();
        let details_expanded = self.expanded_details.contains(row.item_key.row_id());
        let durable = row.durable;
        if row.source_revision != measurement.source_revision
            || row.width_bucket != measurement.width_bucket
            || row.text_phase != measurement.text_phase
            || details_expanded != measurement.details_expanded
        {
            tracing::trace!(target: "desktop", event = "row_measure_stale_drop", reason = "render_data");
            return outcome;
        }

        let layout = if durable {
            &mut self.layout
        } else {
            &mut self.live_layout
        };
        let is_toggle_measurement = self.toggle_anchor.as_ref().is_some_and(|anchor| {
            anchor.layout_applied
                && anchor.item_key == measurement.item_key
                && anchor.source_revision == measurement.source_revision
                && anchor.width_bucket == measurement.width_bucket
                && anchor.details_expanded == measurement.details_expanded
        });
        let Some(resolution) = layout.submit_measurement(measurement, Instant::now()) else {
            return outcome;
        };
        if is_toggle_measurement {
            self.toggle_anchor = None;
        }
        if let Some(delay) = resolution.next_refresh_after {
            outcome.refresh = Some((delay, durable));
        }
        if !resolution.height_changed {
            return outcome;
        }

        let paused_scroll_top =
            (!is_toggle_measurement && !self.viewport.follow_latest()).then(|| {
                let scroll_top = (-f32::from(self.scroll.offset().y)).max(0.);
                let render_heights = self.render_heights.borrow();
                compensate_scroll_top_for_single_row_height(
                    &render_heights,
                    index,
                    resolution.height,
                    scroll_top,
                )
            });
        self.render_heights.borrow_mut()[index] = resolution.height;
        let mut row_sizes = self.row_sizes.borrow_mut();
        if let Some(row_size) = Rc::make_mut(&mut row_sizes).get_mut(index) {
            row_size.height = px(resolution.height);
        }
        drop(row_sizes);

        if is_toggle_measurement {
            tracing::trace!(target: "desktop", event = "toggle_scroll_anchor_hold");
        } else if self.viewport.follow_latest() {
            self.align_scroll_to_bottom(source.visible_count());
        } else if let Some(adjusted) = paused_scroll_top {
            let current = (-f32::from(self.scroll.offset().y)).max(0.);
            if (current - adjusted).abs() > 0.5 {
                let mut offset = self.scroll.offset();
                offset.y = px(-adjusted);
                self.scroll.set_offset(offset);
                tracing::trace!(
                    target: "desktop",
                    event = "scroll_anchor_compensate",
                    delta = adjusted - current,
                );
            }
        }
        outcome.pane_dirty = true;
        outcome
    }

    // ---- width debounce ---------------------------------------------------

    /// Resolve the width bucket to render at, debouncing live resizes so a drag
    /// does not rebuild the full transcript on every frame.
    pub(super) fn width_for_render(&mut self, requested: u32) -> (u32, Option<(u32, Instant)>) {
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
    pub(super) fn commit_pending_width(&mut self, requested: u32, deadline: Instant) -> bool {
        if self.width_pending == Some((requested, deadline)) {
            self.render_full_dirty = true;
            true
        } else {
            false
        }
    }

    // ---- height refresh deadline ------------------------------------------

    /// Arm the earliest pending height refresh. Returns the delay the Root has
    /// to wait on when this call owns the new deadline.
    pub(super) fn arm_height_refresh(
        &mut self,
        refresh: ConversationRefresh,
    ) -> Option<(Duration, Instant)> {
        let (delay, requires_full) = refresh?;
        let deadline = Instant::now() + delay;
        if self
            .height_refresh_deadline
            .is_none_or(|scheduled| scheduled > deadline)
        {
            self.height_refresh_deadline = Some(deadline);
            self.height_refresh_full = requires_full;
            Some((delay, deadline))
        } else {
            if requires_full {
                self.height_refresh_full = true;
            }
            None
        }
    }

    /// Consume an armed height-refresh deadline once its timer fired.
    pub(super) fn fire_height_refresh(&mut self, deadline: Instant) -> bool {
        if self.height_refresh_deadline != Some(deadline) {
            return false;
        }
        self.height_refresh_deadline = None;
        if self.height_refresh_full {
            self.render_full_dirty = true;
        } else {
            self.render_live_dirty = true;
        }
        self.height_refresh_full = false;
        true
    }

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
            },
            panel_width,
        )
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
                },
                panel_width,
            ));
        }

        if let Some(row) = self.resolve_submitted_row(source, &session_id, panel_width) {
            rows.push(row);
        }
        for message in source.messages {
            rows.push(self.resolve_message_row(message, &session_id, panel_width));
        }
        for tool in source.tools {
            rows.push(self.resolve_tool_row(tool, &session_id, panel_width));
        }

        self.render_cache.finish_frame();
        debug_assert_eq!(rows.len(), expected_count);
        *self.render_rows.borrow_mut() = rows;
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
        for message in source.messages {
            live_rows.push(self.resolve_message_row(message, &session_id, panel_width));
        }
        for tool in source.tools {
            live_rows.push(self.resolve_tool_row(tool, &session_id, panel_width));
        }
        self.render_cache.finish_incremental();

        let mut render_rows = self.render_rows.borrow_mut();
        render_rows.truncate(durable_count);
        render_rows.extend(live_rows);
        drop(render_rows);
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
    ) -> Result<Option<Duration>, ()> {
        if self.render_sequence_overflow || self.render_dirty_sequences.is_empty() {
            return Err(());
        }

        let durable_count = source.durable_count();
        let submitted_count = source.submitted_count();
        let session_id = source.session_id().to_owned();
        let sequences = std::mem::take(&mut self.render_dirty_sequences);
        let now = Instant::now();
        let mut next_refresh_after = None;
        self.render_cache.begin_frame();

        for sequence in sequences {
            if let Some((position, message)) = source
                .messages
                .iter()
                .enumerate()
                .find(|(_, message)| message.updated_sequence == sequence)
            {
                let row = self.resolve_message_row(message, &session_id, panel_width);
                let desired_index = durable_count + submitted_count + position;
                let refresh =
                    self.upsert_render_row(durable_count, desired_index, row, panel_width, now);
                next_refresh_after = minimum_duration(next_refresh_after, refresh);
            }

            if let Some((position, tool)) = source
                .tools
                .iter()
                .enumerate()
                .find(|(_, tool)| tool.updated_sequence == sequence)
            {
                let row = self.resolve_tool_row(tool, &session_id, panel_width);
                let desired_index =
                    durable_count + submitted_count + source.messages.len() + position;
                let refresh =
                    self.upsert_render_row(durable_count, desired_index, row, panel_width, now);
                next_refresh_after = minimum_duration(next_refresh_after, refresh);
            }
        }
        self.render_cache.finish_incremental();
        self.live_rows_match(source)
            .then_some(next_refresh_after)
            .ok_or(())
    }

    fn upsert_render_row(
        &mut self,
        durable_count: usize,
        desired_index: usize,
        row: ConversationRowRenderData,
        panel_width: u32,
        now: Instant,
    ) -> Option<Duration> {
        let layout = self.live_layout.resolve_one(
            row_layout_input(&row, &self.expanded_details, panel_width),
            panel_width,
            now,
        );
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
        let height_index = upsert_indexed_item(
            &mut self.render_heights.borrow_mut(),
            existing_index,
            desired_index,
            layout.height,
        );
        let mut row_sizes = self.row_sizes.borrow_mut();
        let size_index = upsert_indexed_item(
            Rc::make_mut(&mut row_sizes),
            existing_index,
            desired_index,
            size(px(0.), px(layout.height)),
        );
        debug_assert_eq!(row_index, height_index);
        debug_assert_eq!(row_index, size_index);
        layout.next_refresh_after
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
        for message in source.messages {
            let Some(row) = render_rows.get(index) else {
                return false;
            };
            if row.item_key.row_id() != message_block_id(message)
                || row.source_revision != message.updated_sequence
            {
                return false;
            }
            index += 1;
        }
        for tool in source.tools {
            let Some(row) = render_rows.get(index) else {
                return false;
            };
            if row.item_key.row_id() != tool_block_id(tool)
                || row.source_revision != tool.updated_sequence
            {
                return false;
            }
            index += 1;
        }
        index == render_rows.len()
    }

    // ---- frame preparation -------------------------------------------------

    /// Bring rows, heights and scroll position in line with the projection for
    /// one frame at `layout_width`, and report the next height refresh.
    pub(super) fn prepare_rows(
        &mut self,
        source: &ConversationSource<'_>,
        layout_width: u32,
    ) -> ConversationRefresh {
        let visible_conversation_count = source.visible_count();
        let _span = tracing::trace_span!(
            "desktop.render.prepare_rows",
            layout_width,
            visible_rows = visible_conversation_count
        )
        .entered();
        let session_scroll_restore = self.pending_scroll_restore.take();
        let toggle_anchor = self
            .toggle_anchor
            .as_ref()
            .filter(|anchor| !anchor.layout_applied)
            .cloned();
        let previous_scroll_top = (session_scroll_restore.is_none()
            && !self.viewport.follow_latest())
        .then(|| (-f32::from(self.scroll.offset().y)).max(0.0));
        let full_render_update =
            self.render_full_dirty || self.render_width_bucket != Some(layout_width);
        let mut paused_scroll_top = None;
        let mut toggle_scroll_top = None;
        let mut next_refresh_after = None;
        let mut refresh_requires_full = false;
        if full_render_update {
            self.rebuild_rows(source, layout_width);
            let row_layout_inputs = self
                .render_rows
                .borrow()
                .iter()
                .map(|row| row_layout_input(row, &self.expanded_details, layout_width))
                .collect::<Vec<_>>();
            let row_layout = self.layout.resolve(
                row_layout_inputs,
                layout_width,
                Instant::now(),
                toggle_anchor
                    .as_ref()
                    .map(|anchor| anchor.row_top)
                    .or(previous_scroll_top),
            );
            if let Some(anchor) = &toggle_anchor {
                toggle_scroll_top = row_layout.paused_scroll_top.map(|resolved_row_top| {
                    (anchor.scroll_top + resolved_row_top - anchor.row_top).max(0.)
                });
                if toggle_scroll_top.is_some() {
                    let resolved_identity = self
                        .render_rows
                        .borrow()
                        .iter()
                        .find(|row| row.item_key == anchor.item_key)
                        .map(|row| (row.source_revision, row.width_bucket));
                    if let Some((source_revision, width_bucket)) = resolved_identity {
                        if let Some(current) = self
                            .toggle_anchor
                            .as_mut()
                            .filter(|current| current.item_key == anchor.item_key)
                        {
                            current.source_revision = source_revision;
                            current.width_bucket = width_bucket;
                            current.layout_applied = true;
                        }
                    } else {
                        self.toggle_anchor = None;
                    }
                } else {
                    self.toggle_anchor = None;
                }
            } else {
                paused_scroll_top = row_layout.paused_scroll_top;
            }
            next_refresh_after = row_layout.next_refresh_after;
            refresh_requires_full = next_refresh_after.is_some();
            let row_sizes = Rc::new(
                row_layout
                    .heights
                    .iter()
                    .map(|height| size(px(0.), px(*height)))
                    .collect(),
            );
            *self.render_heights.borrow_mut() = row_layout.heights;
            *self.row_sizes.borrow_mut() = row_sizes;

            let durable_count = source.durable_count();
            let live_inputs = self.render_rows.borrow()[durable_count..]
                .iter()
                .map(|row| row_layout_input(row, &self.expanded_details, layout_width))
                .collect();
            let _ = self
                .live_layout
                .resolve(live_inputs, layout_width, Instant::now(), None);
            self.render_width_bucket = Some(layout_width);
            self.render_full_dirty = false;
            self.render_live_dirty = false;
        } else if self.render_live_dirty {
            let durable_count = source.durable_count();
            match self.update_rows_by_sequence(source, layout_width) {
                Ok(refresh) => {
                    next_refresh_after = refresh;
                    self.render_sequence_overflow = false;
                }
                Err(()) => {
                    self.rebuild_live_rows(source, layout_width);
                    let live_inputs = self.render_rows.borrow()[durable_count..]
                        .iter()
                        .map(|row| row_layout_input(row, &self.expanded_details, layout_width))
                        .collect();
                    let live_layout =
                        self.live_layout
                            .resolve(live_inputs, layout_width, Instant::now(), None);
                    next_refresh_after = live_layout.next_refresh_after;
                    let mut render_heights = self.render_heights.borrow_mut();
                    render_heights.truncate(durable_count);
                    render_heights.extend(live_layout.heights.iter().copied());
                    let mut row_sizes = self.row_sizes.borrow_mut();
                    let sizes = Rc::make_mut(&mut row_sizes);
                    sizes.truncate(durable_count);
                    sizes.extend(
                        live_layout
                            .heights
                            .into_iter()
                            .map(|height| size(px(0.), px(height))),
                    );
                }
            }
            self.render_live_dirty = false;
        }
        let mut text_phase_requires_full = false;
        let text_phase_refresh = self
            .render_rows
            .borrow()
            .iter()
            .filter_map(|row| {
                let delay = row.next_text_phase_after?;
                text_phase_requires_full |= row.durable;
                Some(delay)
            })
            .min();
        next_refresh_after = minimum_duration(next_refresh_after, text_phase_refresh);
        refresh_requires_full |= text_phase_requires_full;
        debug_assert_eq!(self.render_rows.borrow().len(), visible_conversation_count);
        debug_assert_eq!(
            self.render_heights.borrow().len(),
            visible_conversation_count
        );
        let toggle_anchor_applied = toggle_scroll_top.is_some();
        if let Some(toggle_scroll_top) = toggle_scroll_top {
            let mut offset = self.scroll.offset();
            offset.y = px(-toggle_scroll_top);
            self.scroll.set_offset(offset);
            tracing::trace!(
                target: "desktop",
                event = "toggle_scroll_anchor_apply",
                scroll_top = toggle_scroll_top,
            );
        } else if let Some(restored_scroll_top) = session_scroll_restore
            && !self.viewport.follow_latest()
        {
            let mut offset = self.scroll.offset();
            offset.y = px(-restored_scroll_top);
            self.scroll.set_offset(offset);
            tracing::trace!(
                target: "desktop",
                event = "session_scroll_restore",
                scroll_top = restored_scroll_top,
            );
        } else if let (Some(previous_scroll_top), Some(adjusted_scroll_top)) =
            (previous_scroll_top, paused_scroll_top)
            && (previous_scroll_top - adjusted_scroll_top).abs() > 0.5
        {
            let mut offset = self.scroll.offset();
            offset.y = px(-adjusted_scroll_top);
            self.scroll.set_offset(offset);
        }
        if self.viewport.follow_latest() && !toggle_anchor_applied {
            self.align_scroll_to_bottom(visible_conversation_count);
        }
        next_refresh_after.map(|delay| (delay, refresh_requires_full))
    }
}

#[cfg(test)]
impl ConversationController {
    pub(super) const fn viewport_for_tests(&mut self) -> &mut ConversationViewport {
        &mut self.viewport
    }

    pub(super) const fn expanded_details_for_tests(&mut self) -> &mut HashSet<String> {
        &mut self.expanded_details
    }

    pub(super) const fn pending_scroll_restore_for_tests(&self) -> Option<f32> {
        self.pending_scroll_restore
    }

    pub(super) fn session_view_for_tests(
        &self,
        session_id: &str,
    ) -> Option<&ConversationSessionViewState> {
        self.session_views.get(session_id)
    }

    pub(super) fn set_scroll_top_for_tests(&self, scroll_top: f32) {
        let mut offset = self.scroll.offset();
        offset.y = px(-scroll_top);
        self.scroll.set_offset(offset);
    }

    pub(super) fn scroll_top_for_tests(&self) -> f32 {
        (-f32::from(self.scroll.offset().y)).max(0.)
    }

    pub(super) fn render_heights_for_tests(&self) -> Rc<RefCell<Vec<f32>>> {
        Rc::clone(&self.render_heights)
    }

    pub(super) fn last_row_id_for_tests(&self) -> Option<String> {
        self.render_rows
            .borrow()
            .last()
            .map(|row| row.item_key.row_id().to_owned())
    }
}

pub(super) fn distance_to_bottom(offset_y: f32, max_offset_y: f32) -> f32 {
    (max_offset_y.max(0.0) + offset_y.min(0.0)).max(0.0)
}

pub(super) fn minimum_duration(
    current: Option<Duration>,
    next: Option<Duration>,
) -> Option<Duration> {
    match (current, next) {
        (Some(current), Some(next)) => Some(current.min(next)),
        (Some(current), None) => Some(current),
        (None, next) => next,
    }
}

pub(super) fn compensate_scroll_top_for_single_row_height(
    heights: &[f32],
    changed_index: usize,
    measured_height: f32,
    scroll_top: f32,
) -> f32 {
    if heights.is_empty() || changed_index >= heights.len() {
        return scroll_top.max(0.);
    }
    let scroll_top = if scroll_top.is_finite() {
        scroll_top.max(0.)
    } else {
        0.
    };
    let mut anchor_top = 0.;
    let mut anchor = heights.len() - 1;
    let mut intra_row = heights[anchor].max(0.);
    for (index, height) in heights.iter().copied().enumerate() {
        if scroll_top < anchor_top + height {
            anchor = index;
            intra_row = (scroll_top - anchor_top).max(0.);
            break;
        }
        anchor_top += height;
    }

    if changed_index < anchor {
        (scroll_top + measured_height - heights[changed_index]).max(0.)
    } else if changed_index == anchor {
        let changed_top = heights[..changed_index].iter().copied().sum::<f32>();
        changed_top + intra_row.clamp(0., measured_height)
    } else {
        scroll_top
    }
}

pub(super) fn row_target_height(
    row: &ConversationRowRenderData,
    expanded_details: &HashSet<String>,
    panel_width: u32,
) -> f32 {
    if expanded_details.contains(row.item_key.row_id()) {
        return if row.width_bucket == panel_width {
            row.estimated_height
        } else {
            conversation_block_height(row.kind, &row.text, &row.detail, panel_width)
        };
    }
    let collapsed = match row.kind {
        ConversationBlockKind::Assistant if !row.detail.is_empty() => Some(
            conversation_block_height(row.kind, &row.text, "", panel_width),
        ),
        ConversationBlockKind::Tool if !row.text.is_empty() || !row.detail.is_empty() => {
            Some(conversation_block_height(row.kind, "", "", panel_width))
        }
        _ => None,
    };
    collapsed.map_or(row.estimated_height, |height| {
        (height + COLLAPSED_DETAIL_HEIGHT).min(TRANSCRIPT_COLLAPSED_PREVIEW_MAX_HEIGHT)
    })
}

pub(super) fn row_layout_input(
    row: &ConversationRowRenderData,
    expanded_details: &HashSet<String>,
    panel_width: u32,
) -> ConversationRowLayoutInput {
    ConversationRowLayoutInput {
        item_key: row.item_key.clone(),
        source_revision: row.source_revision,
        text_phase: row.text_phase,
        details_expanded: expanded_details.contains(row.item_key.row_id()),
        estimated_height: row_target_height(row, expanded_details, panel_width),
    }
}

pub(super) fn upsert_indexed_item<T>(
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

pub(super) fn message_block_id(message: &DesktopMessageOverlay) -> String {
    message.message_id.as_ref().map_or_else(
        || format!("assistant:{}:{}", message.operation_id, message.turn_id),
        |message_id| format!("assistant:{message_id}"),
    )
}

pub(super) fn tool_block_id(tool: &DesktopToolOverlay) -> String {
    format!("tool:{}", tool.tool_call_id)
}
