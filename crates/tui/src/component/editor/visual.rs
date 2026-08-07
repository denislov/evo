//! Text layout helpers for [`Editor`]: wrapping, visual lines, grapheme
//! boundaries, and paste-text normalization.

use unicode_segmentation::UnicodeSegmentation;

use super::VisualLine;
use crate::render::{ansi_sequence_len, truncate_to_width, visible_width};

pub(super) fn wrap_multiline(text: &str, width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    for source_line in text.split('\n') {
        wrap_line(source_line, width, &mut lines);
    }
    if text.ends_with('\n') {
        lines.push(String::new());
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

pub(super) fn wrap_line(source: &str, width: usize, lines: &mut Vec<String>) {
    if source.is_empty() {
        lines.push(String::new());
        return;
    }

    let mut current = String::new();
    let mut current_width = 0;
    let mut pos = 0;
    while pos < source.len() {
        if let Some(len) = ansi_sequence_len(source, pos) {
            current.push_str(&source[pos..pos + len]);
            pos += len;
            continue;
        }

        let grapheme = source[pos..]
            .graphemes(true)
            .next()
            .expect("pos is inside source");
        let grapheme_width = visible_width(grapheme);
        if current_width + grapheme_width > width && !current.is_empty() {
            lines.push(std::mem::take(&mut current));
            current_width = 0;
        }
        current.push_str(grapheme);
        current_width += grapheme_width;
        pos += grapheme.len();
    }
    if !current.is_empty() {
        lines.push(current);
    }
}

pub(super) fn fit_render_line(line: &str, width: usize) -> String {
    let mut fitted = truncate_to_width(line, width);
    let fitted_width = visible_width(&fitted);
    if fitted_width < width {
        fitted.push_str(&" ".repeat(width - fitted_width));
    }
    fitted
}

pub(super) fn clean_paste_text(text: &str) -> String {
    let decoded = decode_csi_u_control_bytes(text);
    decoded
        .replace("\r\n", "\n")
        .replace('\r', "\n")
        .replace('\t', "    ")
        .chars()
        .filter(|ch| *ch == '\n' || *ch >= ' ')
        .collect()
}

fn decode_csi_u_control_bytes(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(start) = rest.find("\x1b[") {
        out.push_str(&rest[..start]);
        let after_start = &rest[start + 2..];
        let Some(end) = after_start.find(";5u") else {
            out.push_str(&rest[start..]);
            return out;
        };
        let code = &after_start[..end];
        if let Ok(codepoint) = code.parse::<u8>() {
            if codepoint.is_ascii_lowercase() {
                out.push(char::from(codepoint - b'a' + 1));
                rest = &after_start[end + 3..];
                continue;
            }
            if codepoint.is_ascii_uppercase() {
                out.push(char::from(codepoint - b'A' + 1));
                rest = &after_start[end + 3..];
                continue;
            }
        }
        out.push_str(&rest[start..start + 2 + end + 3]);
        rest = &after_start[end + 3..];
    }
    out.push_str(rest);
    out
}

pub(super) fn starts_like_path(text: &str) -> bool {
    matches!(text.chars().next(), Some('/' | '~' | '.'))
}

pub(super) fn paste_marker(paste_id: usize, text: &str) -> String {
    let line_count = text.split('\n').count();
    if line_count > 10 {
        format!("[paste #{paste_id} +{line_count} lines]")
    } else {
        format!("[paste #{paste_id} {} chars]", text.chars().count())
    }
}

pub(super) fn current_visual_line_index(text: &str, cursor: usize, width: usize) -> usize {
    let lines = visual_lines(text, width);
    current_visual_line_index_from_lines(&lines, cursor)
}

pub(super) fn current_visual_line_index_from_lines(lines: &[VisualLine], cursor: usize) -> usize {
    lines
        .iter()
        .enumerate()
        .position(|(index, line)| {
            cursor >= line.start
                && (cursor < line.end
                    || (cursor == line.end
                        && (index + 1 == lines.len() || lines[index + 1].start != cursor)))
        })
        .unwrap_or(0)
}

pub(super) fn cursor_from_line_col(
    lines: &[String],
    cursor_line: usize,
    cursor_col: usize,
) -> usize {
    let mut cursor = 0usize;
    for (index, line) in lines.iter().enumerate() {
        if index == cursor_line {
            return cursor + cursor_col.min(line.len());
        }
        cursor += line.len() + 1;
    }
    lines.join("\n").len()
}

pub(super) fn best_autocomplete_match(
    items: &[super::autocomplete::AutocompleteItem],
    prefix: &str,
) -> Option<usize> {
    if prefix.is_empty() {
        return None;
    }
    let mut first_prefix = None;
    for (index, item) in items.iter().enumerate() {
        if item.value == prefix {
            return Some(index);
        }
        if first_prefix.is_none() && item.value.starts_with(prefix) {
            first_prefix = Some(index);
        }
    }
    first_prefix
}

pub(super) fn previous_grapheme_boundary(text: &str, cursor: usize) -> usize {
    text[..cursor]
        .grapheme_indices(true)
        .next_back()
        .map(|(index, _)| index)
        .unwrap_or(0)
}

pub(super) fn next_grapheme_boundary(text: &str, cursor: usize) -> usize {
    text[cursor..]
        .grapheme_indices(true)
        .nth(1)
        .map(|(index, _)| cursor + index)
        .unwrap_or(text.len())
}

pub(super) fn current_line_start(text: &str, cursor: usize) -> usize {
    text[..cursor]
        .rfind('\n')
        .map(|index| index + '\n'.len_utf8())
        .unwrap_or(0)
}

pub(super) fn current_line_end(text: &str, cursor: usize) -> usize {
    text[cursor..]
        .find('\n')
        .map(|index| cursor + index)
        .unwrap_or(text.len())
}

pub(super) fn is_single_plain_word_grapheme(text: &str) -> bool {
    let mut graphemes = text.graphemes(true);
    let Some(grapheme) = graphemes.next() else {
        return false;
    };
    graphemes.next().is_none()
        && grapheme
            .chars()
            .all(|ch| !ch.is_whitespace() && !ch.is_ascii_punctuation())
}

pub(super) fn visual_lines(text: &str, width: usize) -> Vec<VisualLine> {
    let mut lines = Vec::new();
    let mut source_start = 0;

    for source_line in text.split_inclusive('\n') {
        let content = source_line.strip_suffix('\n').unwrap_or(source_line);
        push_visual_line_ranges(text, source_start, content.len(), width, &mut lines);
        source_start += source_line.len();
    }

    if text.is_empty() || text.ends_with('\n') {
        lines.push(VisualLine {
            start: text.len(),
            end: text.len(),
        });
    }

    lines
}

pub(super) fn visual_line_at_cursor(
    lines: &[VisualLine],
    cursor: usize,
    delta: isize,
) -> Option<usize> {
    lines.iter().enumerate().position(|(index, line)| {
        cursor >= line.start
            && (cursor < line.end
                || (cursor == line.end
                    && (delta > 0 || index + 1 == lines.len() || lines[index + 1].start != cursor)))
    })
}

fn push_visual_line_ranges(
    text: &str,
    source_start: usize,
    source_len: usize,
    width: usize,
    lines: &mut Vec<VisualLine>,
) {
    if source_len == 0 {
        lines.push(VisualLine {
            start: source_start,
            end: source_start,
        });
        return;
    }

    let source_end = source_start + source_len;
    let mut line_start = source_start;
    let mut current_width = 0;
    let mut pos = source_start;
    while pos < source_end {
        if let Some(len) = ansi_sequence_len(text, pos) {
            pos += len;
            continue;
        }

        let grapheme = text[pos..source_end]
            .graphemes(true)
            .next()
            .expect("pos is inside source");
        let grapheme_width = visible_width(grapheme);
        if current_width + grapheme_width > width && line_start < pos {
            lines.push(VisualLine {
                start: line_start,
                end: pos,
            });
            line_start = pos;
            current_width = 0;
        }
        current_width += grapheme_width;
        pos += grapheme.len();
    }

    lines.push(VisualLine {
        start: line_start,
        end: source_end,
    });
}

pub(super) fn cursor_at_visible_col(text: &str, line: VisualLine, desired_col: usize) -> usize {
    let mut current_col = 0;
    let mut pos = line.start;
    while pos < line.end {
        if let Some(len) = ansi_sequence_len(text, pos) {
            pos += len;
            continue;
        }

        let grapheme = text[pos..line.end]
            .graphemes(true)
            .next()
            .expect("pos is inside visual line");
        let next_col = current_col + visible_width(grapheme);
        if next_col > desired_col {
            return pos;
        }
        current_col = next_col;
        pos += grapheme.len();
    }
    line.end
}
