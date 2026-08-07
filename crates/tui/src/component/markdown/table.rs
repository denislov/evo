//! Markdown table rendering (box-drawing layout with narrow fallback).

use pulldown_cmark::Alignment;

use crate::component::markdown::parse::InlineSpan;
use crate::component::markdown::style::apply_inline_spans;
use crate::component::markdown::wrap::{SKIP_WRAP, wrap_to_lines};
use crate::render::Style;
use crate::render::{color_enabled, paint_with, visible_width};
use crate::theme::MarkdownTheme;

#[derive(Clone)]
pub(super) struct CellContent {
    pub(super) raw: String,
    pub(super) spans: Vec<InlineSpan>,
}

pub(super) struct TableAccum {
    pub(super) alignments: Vec<Alignment>,
    pub(super) header_cells: Vec<CellContent>,
    pub(super) body_rows: Vec<Vec<CellContent>>,
    pub(super) current_row: Vec<CellContent>,
    pub(super) in_header: bool,
}

impl Clone for TableAccum {
    fn clone(&self) -> Self {
        Self {
            alignments: self.alignments.clone(),
            header_cells: self.header_cells.clone(),
            body_rows: self.body_rows.clone(),
            current_row: self.current_row.clone(),
            in_header: self.in_header,
        }
    }
}

fn push_table_line(blocks: &mut Vec<String>, line: String) {
    blocks.push(format!("{SKIP_WRAP}{line}"));
}

fn align_table_cell(text: &str, width: usize, alignment: Alignment) -> String {
    let padding = width.saturating_sub(visible_width(text));
    let (left, right) = match alignment {
        Alignment::Right => (padding, 0),
        Alignment::Center => (padding / 2, padding.saturating_sub(padding / 2)),
        Alignment::None | Alignment::Left => (0, padding),
    };
    format!("{}{}{}", " ".repeat(left), text, " ".repeat(right))
}

fn render_narrow_table(table: &TableAccum, blocks: &mut Vec<String>) {
    for (row_index, row) in table.body_rows.iter().enumerate() {
        if row_index > 0 {
            blocks.push(String::new());
        }
        for column in 0..table.alignments.len() {
            let label = table
                .header_cells
                .get(column)
                .map(|cell| cell.raw.replace('\t', " "))
                .filter(|label| !label.trim().is_empty())
                .unwrap_or_else(|| format!("Column {}", column + 1));
            let value = row
                .get(column)
                .map(|cell| cell.raw.replace('\t', " "))
                .unwrap_or_default();
            blocks.push(format!("{}: {}", label.trim(), value.trim()));
        }
    }
    if table.body_rows.is_empty() {
        for (column, cell) in table.header_cells.iter().enumerate() {
            blocks.push(format!(
                "Column {}: {}",
                column + 1,
                cell.raw.replace('\t', " ").trim()
            ));
        }
    }
}

fn paint_markdown(text: &str, style: &Style) -> String {
    paint_with(text, style, color_enabled())
}

/// Render a parsed table as ANSI-styled lines with box-drawing borders.
pub(super) fn render_table(
    table: &TableAccum,
    total_width: usize,
    theme: &MarkdownTheme,
    hyperlinks_enabled: bool,
    base_style: Option<&Style>,
    blocks: &mut Vec<String>,
) {
    let num_cols = table.alignments.len();
    if num_cols == 0 {
        return;
    }

    // Border overhead per row: "│ a │ b │" = 2 + 3*(n-1) + 2 = 3n + 1
    let border_overhead = 3 * num_cols + 1;
    if total_width < border_overhead.saturating_add(num_cols) {
        render_narrow_table(table, blocks);
        return;
    }
    let available_for_cells = total_width - border_overhead;

    const MAX_UNBROKEN_WORD: usize = 30;

    // ── Compute column widths from raw text ──────────────────────────
    let cell_visible = |raw: &str| -> usize { visible_width(raw.replace('\t', " ").trim_end()) };
    let longest_word = |raw: &str| -> usize {
        raw.replace('\t', " ")
            .split_whitespace()
            .map(visible_width)
            .max()
            .unwrap_or(0)
            .clamp(1, MAX_UNBROKEN_WORD)
    };

    let mut natural_widths = vec![0usize; num_cols];
    let mut min_word_widths = vec![1usize; num_cols];

    for (i, cell) in table.header_cells.iter().enumerate().take(num_cols) {
        natural_widths[i] = natural_widths[i].max(cell_visible(&cell.raw));
        min_word_widths[i] = min_word_widths[i].max(longest_word(&cell.raw));
    }
    for row in &table.body_rows {
        for (i, cell) in row.iter().enumerate().take(num_cols) {
            natural_widths[i] = natural_widths[i].max(cell_visible(&cell.raw));
            min_word_widths[i] = min_word_widths[i].max(longest_word(&cell.raw));
        }
    }

    let total_natural: usize = natural_widths.iter().sum::<usize>() + border_overhead;
    let column_widths: Vec<usize> = if total_natural <= total_width {
        // ── Natural fit ──────────────────────────────────────────────
        natural_widths
            .iter()
            .zip(min_word_widths.iter())
            .map(|(nat, min)| (*nat).max(*min))
            .collect::<Vec<usize>>()
    } else {
        // ── Shrink proportionally ────────────────────────────────────
        let min_cells_width: usize = min_word_widths.iter().sum();
        let lower_bounds = if min_cells_width <= available_for_cells {
            min_word_widths.clone()
        } else {
            vec![1; num_cols]
        };
        let min_cells_width: usize = lower_bounds.iter().sum();
        let extra_width = available_for_cells.saturating_sub(min_cells_width);
        let total_grow_potential: usize = natural_widths
            .iter()
            .zip(lower_bounds.iter())
            .map(|(nat, min)| nat.saturating_sub(*min))
            .sum();

        let mut col_widths = lower_bounds.clone();
        if total_grow_potential > 0 {
            for i in 0..num_cols {
                let grow = (natural_widths[i].saturating_sub(lower_bounds[i]) as f64
                    / total_grow_potential as f64
                    * extra_width as f64) as usize;
                col_widths[i] += grow;
            }
        }

        // Distribute rounding leftovers
        let allocated: usize = col_widths.iter().sum();
        let mut remaining = available_for_cells.saturating_sub(allocated);
        'distribute: while remaining > 0 {
            for i in 0..num_cols {
                if remaining == 0 {
                    break 'distribute;
                }
                if col_widths[i] < natural_widths[i] {
                    col_widths[i] += 1;
                    remaining -= 1;
                }
            }
            if remaining > 0 {
                break;
            }
        }
        col_widths
    };

    // ── Style + wrap cells ───────────────────────────────────────────
    let style_cell = |cell: &CellContent| -> String {
        let normalized = cell.raw.replace('\t', " ");
        apply_inline_spans(
            normalized.trim_end(),
            &cell.spans,
            theme,
            hyperlinks_enabled,
            base_style,
        )
    };
    let wrap_cell = |cell: &CellContent, col_w: usize| -> Vec<String> {
        let styled = style_cell(cell);
        wrap_to_lines(&styled, col_w.max(1))
    };

    let empty_cell = CellContent {
        raw: String::new(),
        spans: vec![],
    };

    // ── Top border ────────────────────────────────────────────────────
    let top_parts: Vec<String> = column_widths.iter().map(|w| "─".repeat(*w)).collect();
    push_table_line(blocks, format!("┌─{}─┐", top_parts.join("─┬─")));

    // ── Header rows ───────────────────────────────────────────────────
    if !table.header_cells.is_empty() {
        let header_cell_lines: Vec<Vec<String>> = (0..num_cols)
            .map(|i| {
                let cell = table.header_cells.get(i).unwrap_or(&empty_cell);
                wrap_cell(cell, column_widths[i])
            })
            .collect();
        let header_line_count = header_cell_lines
            .iter()
            .map(|lines| lines.len())
            .max()
            .unwrap_or(0);

        for line_idx in 0..header_line_count {
            let row_parts: Vec<String> = header_cell_lines
                .iter()
                .enumerate()
                .map(|(ci, lines)| {
                    let text = lines.get(line_idx).map(|s| s.as_str()).unwrap_or("");
                    let padded = align_table_cell(text, column_widths[ci], table.alignments[ci]);
                    paint_markdown(&padded, &theme.bold)
                })
                .collect();
            push_table_line(blocks, format!("│ {} │", row_parts.join(" │ ")));
        }

        // Header / body separator: ├──┼──┤
        let sep_parts: Vec<String> = column_widths.iter().map(|w| "─".repeat(*w)).collect();
        push_table_line(blocks, format!("├─{}─┤", sep_parts.join("─┼─")));
    }

    // ── Body rows ─────────────────────────────────────────────────────
    for (row_idx, row) in table.body_rows.iter().enumerate() {
        let body_cell_lines: Vec<Vec<String>> = (0..num_cols)
            .map(|i| {
                let cell = row.get(i).unwrap_or(&empty_cell);
                wrap_cell(cell, column_widths[i])
            })
            .collect();
        let body_line_count = body_cell_lines
            .iter()
            .map(|lines| lines.len())
            .max()
            .unwrap_or(0);

        for line_idx in 0..body_line_count {
            let row_parts: Vec<String> = body_cell_lines
                .iter()
                .enumerate()
                .map(|(ci, lines)| {
                    let text = lines.get(line_idx).map(|s| s.as_str()).unwrap_or("");
                    align_table_cell(text, column_widths[ci], table.alignments[ci])
                })
                .collect();
            push_table_line(blocks, format!("│ {} │", row_parts.join(" │ ")));
        }

        // Row separator between data rows (no separator after last row)
        if row_idx < table.body_rows.len().saturating_sub(1) && !table.body_rows.is_empty() {
            let sep_parts: Vec<String> = column_widths.iter().map(|w| "─".repeat(*w)).collect();
            push_table_line(blocks, format!("├─{}─┤", sep_parts.join("─┼─")));
        }
    }

    // ── Bottom border ─────────────────────────────────────────────────
    let bottom_parts: Vec<String> = column_widths.iter().map(|w| "─".repeat(*w)).collect();
    push_table_line(blocks, format!("└─{}─┘", bottom_parts.join("─┴─")));
}
