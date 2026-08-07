//! Free functions shared by the surface modules: frame validation, diffing,
//! overlay geometry, and column splicing.

use crate::render::surface::TuiError;
use crate::render::{
    OverlayAnchor, OverlayOptions, SizeValue, drop_columns, truncate_to_width, visible_width,
};

/// Reset sequence inserted between composite segments to prevent colour bleed.
const SEGMENT_RESET: &str = "\x1b[0m\x1b]8;;\x07";

// ── Helpers ────────────────────────────────────────────────────────────

pub(super) fn validate_lines(lines: &[String], max_width: usize) -> Result<(), TuiError> {
    for (line_index, line) in lines.iter().enumerate() {
        let width = visible_width(line);
        if width > max_width {
            return Err(TuiError::LineTooWide {
                line_index,
                width,
                max_width,
                line: line.clone(),
            });
        }
    }
    Ok(())
}

pub(super) fn viewport_top(line_count: usize, height: usize) -> usize {
    line_count.saturating_sub(height)
}

pub(super) fn fullscreen_frame(mut lines: Vec<String>, height: usize) -> Vec<String> {
    if height == 0 {
        return Vec::new();
    }
    if lines.len() > height {
        lines.drain(..lines.len() - height);
        return lines;
    }
    let mut frame = vec![String::new(); height - lines.len()];
    frame.append(&mut lines);
    frame
}

pub(super) fn last_line_width(lines: &[String]) -> usize {
    lines.last().map(|line| visible_width(line)).unwrap_or(0)
}

pub(super) fn changed_line_range(previous: &[String], next: &[String]) -> Option<(usize, usize)> {
    let shared = previous.len().min(next.len());
    let mut first = None;
    let mut last = None;

    for index in 0..shared {
        if previous[index] != next[index] {
            first.get_or_insert(index);
            last = Some(index);
        }
    }

    if previous.len() != next.len() {
        let first_changed = first.unwrap_or(shared);
        let last_changed = previous.len().max(next.len()).saturating_sub(1);
        Some((first_changed, last_changed))
    } else {
        first.map(|first_changed| (first_changed, last.expect("first change has last change")))
    }
}

pub(super) fn resolve_overlay_width(options: &OverlayOptions, terminal_width: usize) -> usize {
    let available = terminal_width.saturating_sub(options.margin.left + options.margin.right);
    let mut width = options
        .width
        .map(|size| resolve_size(size, available))
        .unwrap_or(available);
    if let Some(min_width) = options.min_width {
        width = width.max(min_width);
    }
    width.min(available).max(1)
}

pub(super) fn resolve_size(size: SizeValue, available: usize) -> usize {
    match size {
        SizeValue::Columns(columns) => columns,
        SizeValue::Percent(percent) => available.saturating_mul(percent as usize) / 100,
    }
}

pub(super) fn overlay_position(
    options: &OverlayOptions,
    terminal_width: usize,
    terminal_height: usize,
    overlay_width: usize,
    overlay_height: usize,
) -> (usize, usize) {
    let min_row = options.margin.top;
    let min_col = options.margin.left;
    let max_row = terminal_height
        .saturating_sub(options.margin.bottom)
        .saturating_sub(overlay_height);
    let max_col = terminal_width
        .saturating_sub(options.margin.right)
        .saturating_sub(overlay_width);

    let (mut row, mut col) = match options.anchor {
        OverlayAnchor::Center => (
            terminal_height.saturating_sub(overlay_height) / 2,
            terminal_width.saturating_sub(overlay_width) / 2,
        ),
        OverlayAnchor::TopLeft => (min_row, min_col),
        OverlayAnchor::TopRight => (min_row, max_col),
        OverlayAnchor::BottomLeft => (max_row, min_col),
        OverlayAnchor::BottomRight => (max_row, max_col),
        OverlayAnchor::TopCenter => (min_row, terminal_width.saturating_sub(overlay_width) / 2),
        OverlayAnchor::BottomCenter => (max_row, terminal_width.saturating_sub(overlay_width) / 2),
        OverlayAnchor::LeftCenter => (terminal_height.saturating_sub(overlay_height) / 2, min_col),
        OverlayAnchor::RightCenter => (terminal_height.saturating_sub(overlay_height) / 2, max_col),
    };

    if let Some(size) = options.row {
        row = resolve_size(size, terminal_height);
    }
    if let Some(size) = options.col {
        col = resolve_size(size, terminal_width);
    }

    row = apply_offset(row, options.offset_y).clamp(min_row, max_row.max(min_row));
    col = apply_offset(col, options.offset_x).clamp(min_col, max_col.max(min_col));
    (row, col)
}

pub(super) fn apply_offset(value: usize, offset: isize) -> usize {
    if offset.is_negative() {
        value.saturating_sub(offset.unsigned_abs())
    } else {
        value.saturating_add(offset as usize)
    }
}

pub(super) fn fit_to_width(line: &str, width: usize) -> String {
    let mut fitted = truncate_to_width(line, width);
    let visible = visible_width(&fitted);
    if visible < width {
        fitted.push_str(&" ".repeat(width - visible));
    }
    fitted
}

/// Splice `replacement` into `base` at column `col` with `width`.
/// Inserts [`SEGMENT_RESET`] between the before/overlay/after segments to
/// prevent colour bleed — mirrors TS `compositeLineAt` + `SEGMENT_RESET`.
pub(super) fn splice_by_columns(base: &str, col: usize, width: usize, replacement: &str) -> String {
    let mut prefix = truncate_to_width(base, col);
    let prefix_width = visible_width(&prefix);
    if prefix_width < col {
        prefix.push_str(&" ".repeat(col - prefix_width));
    }

    let suffix = drop_columns(base, col + width);

    // Insert SEGMENT_RESET between segments to prevent colour bleed,
    // mirroring TS `compositeLineAt()` which uses `SEGMENT_RESET`.
    format!("{prefix}{SEGMENT_RESET}{replacement}{SEGMENT_RESET}{suffix}")
}
