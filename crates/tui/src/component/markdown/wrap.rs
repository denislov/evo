//! Word wrapping for rendered markdown blocks.

use unicode_segmentation::UnicodeSegmentation;

use crate::render::{truncate_to_width, visible_width};

/// Zero-width-space marker prepended to code-block lines that should NOT
/// undergo word-wrapping (fence rows, content lines with preserved indent).
pub(super) const SKIP_WRAP: &str = "\u{200B}";

pub(super) fn wrap_to_lines(text: &str, width: usize) -> Vec<String> {
    let mut lines = vec![];
    wrap_line(text, width, &mut lines);
    lines
}

pub(super) fn wrap_line(source: &str, width: usize, lines: &mut Vec<String>) {
    if source.is_empty() {
        lines.push(String::new());
        return;
    }

    if source.trim().is_empty() {
        lines.push(truncate_to_width(source, width));
        return;
    }

    let leading_whitespace: String = source.chars().take_while(|ch| ch.is_whitespace()).collect();
    let mut current = leading_whitespace;
    if visible_width(&current) >= width {
        lines.push(truncate_to_width(&current, width));
        current.clear();
    }

    for word in source.split_whitespace() {
        if visible_width(word) > width {
            if !current.is_empty() {
                lines.push(std::mem::take(&mut current));
            }
            split_long_word(word, width, lines);
            continue;
        }

        if current.is_empty() {
            current.push_str(word);
            continue;
        }

        let separator = if current.trim().is_empty() { "" } else { " " };
        let candidate = format!("{current}{separator}{word}");
        if visible_width(&candidate) <= width {
            current = candidate;
        } else {
            lines.push(std::mem::take(&mut current));
            current.push_str(word);
        }
    }

    if !current.is_empty() {
        lines.push(current);
    }
}

fn split_long_word(word: &str, width: usize, lines: &mut Vec<String>) {
    let mut current = String::new();
    let mut current_width = 0;
    for grapheme in word.graphemes(true) {
        let grapheme_width = visible_width(grapheme);
        if current_width + grapheme_width > width && !current.is_empty() {
            lines.push(std::mem::take(&mut current));
            current_width = 0;
        }
        current.push_str(grapheme);
        current_width += grapheme_width;
    }
    if !current.is_empty() {
        lines.push(truncate_to_width(&current, width));
    }
}

/// Width-independent wrapping of parsed blocks. Lines carrying the
/// [`SKIP_WRAP`] marker are pre-styled code rows and are emitted verbatim.
pub(super) fn wrap_blocks(blocks: &[String], width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    for block in blocks {
        if block.starts_with(SKIP_WRAP) {
            // Pre-styled code-block line; do not word-wrap.
            lines.push(block.strip_prefix(SKIP_WRAP).unwrap_or(block).to_string());
            continue;
        }
        for source_line in block.split('\n') {
            wrap_line(source_line, width, &mut lines);
        }
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}
