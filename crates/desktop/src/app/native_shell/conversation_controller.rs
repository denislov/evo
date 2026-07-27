use std::{
    collections::HashSet,
    rc::Rc,
    time::{Duration, Instant},
};

use gpui::{Context, ScrollStrategy, px};

use crate::conversation::{
    ConversationBlockKind, ConversationRowLayoutInput, ConversationRowMeasurement,
    ConversationRowRenderData, ConversationViewport, TRANSCRIPT_COLLAPSED_PREVIEW_MAX_HEIGHT,
    conversation_block_height,
};

use super::NativeShell;

pub(super) const RESIZE_DEBOUNCE: Duration = Duration::from_millis(67);
pub(super) const MAX_SESSION_VIEW_STATES: usize = 256;

const COLLAPSED_DETAIL_HEIGHT: f32 = 36.;

#[derive(Clone)]
pub(super) struct ConversationSessionViewState {
    pub(super) viewport: ConversationViewport,
    pub(super) scroll_top: f32,
    pub(super) expanded_details: HashSet<String>,
}

pub(super) fn reconcile_session_view_state(
    viewport: &mut ConversationViewport,
    expanded_details: &mut HashSet<String>,
    states: &mut std::collections::HashMap<String, ConversationSessionViewState>,
    pending_scroll_restore: &mut Option<f32>,
    previous_session_id: &str,
    next_session_id: &str,
    scroll_top: f32,
) -> bool {
    if previous_session_id == next_session_id {
        return false;
    }
    if states.len() >= MAX_SESSION_VIEW_STATES
        && !states.contains_key(previous_session_id)
        && let Some(stale) = states.keys().next().cloned()
    {
        states.remove(&stale);
    }
    states.insert(
        previous_session_id.to_owned(),
        ConversationSessionViewState {
            viewport: viewport.clone(),
            scroll_top: if scroll_top.is_finite() {
                scroll_top.max(0.)
            } else {
                0.
            },
            expanded_details: std::mem::take(expanded_details),
        },
    );
    if let Some(state) = states.remove(next_session_id) {
        *viewport = state.viewport;
        *expanded_details = state.expanded_details;
        *pending_scroll_restore = Some(state.scroll_top);
    } else {
        *viewport = ConversationViewport::new(8);
        *pending_scroll_restore = Some(0.);
    }
    true
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
        return row.estimated_height;
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

pub(super) fn message_block_id(message: &desktop::projection::DesktopMessageOverlay) -> String {
    message.message_id.as_ref().map_or_else(
        || format!("assistant:{}:{}", message.operation_id, message.turn_id),
        |message_id| format!("assistant:{message_id}"),
    )
}

pub(super) fn tool_block_id(tool: &desktop::projection::DesktopToolOverlay) -> String {
    format!("tool:{}", tool.tool_call_id)
}

pub(super) fn follow_latest(shell: &mut NativeShell, cx: &mut Context<NativeShell>) {
    let block_count = shell.visible_conversation_count();
    shell.conversation_viewport.resume_latest(block_count);
    align_scroll_to_bottom(shell);
    cx.notify();
}

pub(super) fn align_scroll_to_bottom(shell: &mut NativeShell) {
    let block_count = shell.visible_conversation_count();
    if block_count == 0 {
        let mut offset = shell.conversation_scroll.offset();
        offset.y = px(0.);
        shell.conversation_scroll.set_offset(offset);
        return;
    }

    let viewport_height = f32::from(shell.conversation_scroll.bounds().size.height);
    if viewport_height > 0. && shell.conversation_render_heights.len() == block_count {
        let content_height = shell
            .conversation_render_heights
            .iter()
            .copied()
            .sum::<f32>();
        let mut offset = shell.conversation_scroll.offset();
        offset.y = px((viewport_height - content_height).min(0.));
        shell.conversation_scroll.set_offset(offset);
    } else {
        shell
            .conversation_scroll
            .scroll_to_item(block_count - 1, ScrollStrategy::Bottom);
    }
}

pub(super) fn reconcile_scroll(shell: &mut NativeShell, cx: &mut Context<NativeShell>) {
    let offset_y = f32::from(shell.conversation_scroll.offset().y);
    let max_offset_y = f32::from(shell.conversation_scroll.max_offset().y);
    let distance_to_bottom = distance_to_bottom(offset_y, max_offset_y);
    if shell
        .conversation_viewport
        .reconcile_scroll_distance(distance_to_bottom)
    {
        cx.notify();
    }
}

pub(super) fn submit_row_measurement(
    shell: &mut NativeShell,
    measurement: &ConversationRowMeasurement,
    cx: &mut Context<NativeShell>,
) {
    let Some(index) = shell
        .conversation_render_rows
        .iter()
        .position(|row| row.item_key == measurement.item_key)
    else {
        tracing::trace!(target: "desktop", event = "row_measure_stale_drop", reason = "unmounted");
        return;
    };
    let row = &shell.conversation_render_rows[index];
    let details_expanded = shell
        .conversation_expanded_details
        .contains(row.item_key.row_id());
    let durable = row.durable;
    if row.source_revision != measurement.source_revision
        || row.width_bucket != measurement.width_bucket
        || row.text_phase != measurement.text_phase
        || details_expanded != measurement.details_expanded
    {
        tracing::trace!(target: "desktop", event = "row_measure_stale_drop", reason = "render_data");
        return;
    }

    let layout = if durable {
        &mut shell.conversation_layout
    } else {
        &mut shell.conversation_live_layout
    };
    let Some(resolution) = layout.submit_measurement(measurement, Instant::now()) else {
        return;
    };
    if let Some(delay) = resolution.next_refresh_after {
        shell.schedule_conversation_height_refresh(Some((delay, durable)), cx);
    }
    if !resolution.height_changed {
        return;
    }

    let paused_scroll_top = (!shell.conversation_viewport.follow_latest()).then(|| {
        let scroll_top = (-f32::from(shell.conversation_scroll.offset().y)).max(0.);
        compensate_scroll_top_for_single_row_height(
            &shell.conversation_render_heights,
            index,
            resolution.height,
            scroll_top,
        )
    });
    shell.conversation_render_heights[index] = resolution.height;
    if let Some(row_size) = Rc::make_mut(&mut shell.conversation_row_sizes).get_mut(index) {
        row_size.height = px(resolution.height);
    }

    if shell.conversation_viewport.follow_latest() {
        align_scroll_to_bottom(shell);
    } else if let Some(adjusted) = paused_scroll_top {
        let current = (-f32::from(shell.conversation_scroll.offset().y)).max(0.);
        if (current - adjusted).abs() > 0.5 {
            let mut offset = shell.conversation_scroll.offset();
            offset.y = px(-adjusted);
            shell.conversation_scroll.set_offset(offset);
            tracing::trace!(
                target: "desktop",
                event = "scroll_anchor_compensate",
                delta = adjusted - current,
            );
        }
    }
    shell.conversation_pane.update(cx, |_, cx| cx.notify());
}
