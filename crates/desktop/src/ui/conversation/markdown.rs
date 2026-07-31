//! Bounded Markdown preview for the native renderer.
//!
//! Sanitization is a presentation concern only: copy text is never mutated by
//! this module. Model-authored HTML and Markdown images are neutralized so
//! rendering a conversation cannot initiate ambient media loading.

pub const MAX_MARKDOWN_PREVIEW_BYTES: usize = 256 * 1024;
pub const MAX_MARKDOWN_LINE_BYTES: usize = 16 * 1024;
// Keep final parsing comfortably inside the 150 ms completion budget on the
// locked GPUI parser. Full copy text remains available outside this preview.
pub const MAX_MARKDOWN_LINES: usize = 3_072;
pub const MAX_MARKDOWN_NESTING: usize = 24;
pub const MAX_MARKDOWN_MARKERS_PER_LINE: usize = 128;
pub const MAX_MARKDOWN_TABLE_ROWS: usize = 256;
pub const MAX_MARKDOWN_TABLE_CELLS: usize = 64;
pub const MAX_CODE_BLOCK_PREVIEW_BYTES: usize = 128 * 1024;

const TRUNCATED_LINE_NOTICE: &str = "\n\n> … line truncated by desktop preview bounds …\n";
const TRUNCATED_CODE_NOTICE: &str = "\n… code block truncated by desktop preview bounds …\n";
const TRUNCATED_TABLE_NOTICE: &str = "\n\n> … table rows omitted by desktop preview bounds …\n";
const TRUNCATED_DOCUMENT_NOTICE: &str = "\n\n> … document truncated by desktop preview bounds …\n";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MarkdownPreview {
    pub text: String,
    pub truncated: bool,
    pub media_neutralized: bool,
}

/// Produce bounded Markdown for the native renderer without mutating copy text.
///
/// Model-authored HTML and Markdown images are neutralized because rendering a
/// conversation must not initiate ambient media loading. Explicit product
/// image attachments remain represented by the transcript block's image count.
pub fn bounded_markdown_preview(raw: &str) -> MarkdownPreview {
    let _span = tracing::trace_span!("desktop.preview.sanitize", input_bytes = raw.len()).entered();
    let mut preview = MarkdownPreviewBuilder::new();
    let mut fence = None;
    let mut code_bytes = 0;
    let mut code_notice_emitted = false;
    let mut consecutive_table_rows = 0;
    let mut table_notice_emitted = false;
    let mut lines = raw.split_inclusive('\n');

    for line_index in 0..MAX_MARKDOWN_LINES {
        let Some(raw_line) = lines.next() else {
            break;
        };
        let has_newline = raw_line.ends_with('\n');
        let line_without_newline = raw_line.strip_suffix('\n').unwrap_or(raw_line);
        let (line, line_truncated) = truncate_str(line_without_newline, MAX_MARKDOWN_LINE_BYTES);
        preview.truncated |= line_truncated;

        if let Some(delimiter) = fence_delimiter(line) {
            let closes_fence = fence.is_some_and(|open| open == delimiter);
            if fence.is_none() || closes_fence {
                preview.push(line);
                if has_newline {
                    preview.push("\n");
                }
                if closes_fence {
                    fence = None;
                    code_bytes = 0;
                    code_notice_emitted = false;
                } else {
                    fence = Some(delimiter);
                }
                continue;
            }
        }

        if fence.is_some() {
            let remaining = MAX_CODE_BLOCK_PREVIEW_BYTES.saturating_sub(code_bytes);
            let (bounded_code, code_truncated) = truncate_str(line, remaining);
            preview.push(bounded_code);
            code_bytes = code_bytes.saturating_add(bounded_code.len());
            if has_newline && !code_truncated {
                preview.push("\n");
                code_bytes = code_bytes.saturating_add(1);
            }
            if code_truncated || remaining == 0 {
                preview.truncated = true;
                if !code_notice_emitted {
                    preview.push(TRUNCATED_CODE_NOTICE);
                    code_notice_emitted = true;
                }
            }
            continue;
        }

        let (line, nesting_truncated) = cap_markdown_nesting(line);
        preview.truncated |= nesting_truncated;
        let table_cells = line.bytes().filter(|byte| *byte == b'|').count();
        let is_table_row = table_cells >= 2;
        if is_table_row {
            consecutive_table_rows += 1;
            if consecutive_table_rows > MAX_MARKDOWN_TABLE_ROWS {
                preview.truncated = true;
                if !table_notice_emitted {
                    preview.push(TRUNCATED_TABLE_NOTICE);
                    table_notice_emitted = true;
                }
                continue;
            }
        } else {
            consecutive_table_rows = 0;
            table_notice_emitted = false;
        }

        push_safe_markdown_line(&mut preview, &line);
        if has_newline {
            preview.push("\n");
        }
        if line_truncated {
            preview.push(TRUNCATED_LINE_NOTICE);
        }
        if preview.full() {
            break;
        }

        if line_index + 1 == MAX_MARKDOWN_LINES && lines.next().is_some() {
            preview.truncated = true;
            preview.push(TRUNCATED_DOCUMENT_NOTICE);
        }
    }

    if raw.len() > MAX_MARKDOWN_PREVIEW_BYTES || !preview.consumed_all_capacity() {
        preview.truncated = true;
    }
    preview.finish()
}

struct MarkdownPreviewBuilder {
    text: String,
    truncated: bool,
    media_neutralized: bool,
    capacity_exhausted: bool,
}

impl MarkdownPreviewBuilder {
    fn new() -> Self {
        Self {
            text: String::with_capacity(MAX_MARKDOWN_PREVIEW_BYTES.min(16 * 1024)),
            truncated: false,
            media_neutralized: false,
            capacity_exhausted: false,
        }
    }

    fn push(&mut self, value: &str) {
        let remaining = MAX_MARKDOWN_PREVIEW_BYTES.saturating_sub(self.text.len());
        let (value, truncated) = truncate_str(value, remaining);
        self.text.push_str(value);
        self.truncated |= truncated;
        self.capacity_exhausted |= truncated;
    }

    fn full(&self) -> bool {
        self.text.len() == MAX_MARKDOWN_PREVIEW_BYTES
    }

    fn consumed_all_capacity(&self) -> bool {
        !self.capacity_exhausted
    }

    fn finish(self) -> MarkdownPreview {
        MarkdownPreview {
            text: self.text,
            truncated: self.truncated,
            media_neutralized: self.media_neutralized,
        }
    }
}

fn fence_delimiter(line: &str) -> Option<char> {
    let line = line.trim_start();
    let delimiter = line.chars().next()?;
    if !matches!(delimiter, '`' | '~') {
        return None;
    }
    (line
        .chars()
        .take_while(|character| *character == delimiter)
        .count()
        >= 3)
        .then_some(delimiter)
}

fn cap_markdown_nesting(line: &str) -> (String, bool) {
    let leading_spaces = line.bytes().take_while(|byte| *byte == b' ').count();
    let retained_spaces = leading_spaces.min(MAX_MARKDOWN_NESTING * 4);
    let mut rest = &line[leading_spaces..];
    let mut blockquotes = 0;
    let mut prefix_bytes = 0;
    while let Some(after) = rest.strip_prefix('>') {
        blockquotes += 1;
        prefix_bytes += 1;
        rest = after;
        if let Some(after_space) = rest.strip_prefix(' ') {
            prefix_bytes += 1;
            rest = after_space;
        }
    }
    let retained_blockquotes = blockquotes.min(MAX_MARKDOWN_NESTING);
    let truncated = leading_spaces != retained_spaces || blockquotes != retained_blockquotes;
    if !truncated {
        return (line.to_owned(), false);
    }

    let mut bounded = String::with_capacity(line.len());
    bounded.push_str(&" ".repeat(retained_spaces));
    for _ in 0..retained_blockquotes {
        bounded.push_str("> ");
    }
    bounded.push_str(&line[leading_spaces + prefix_bytes..]);
    (bounded, true)
}

fn push_safe_markdown_line(preview: &mut MarkdownPreviewBuilder, line: &str) {
    let mut characters = line.chars().peekable();
    let mut marker_count = 0;
    let mut table_cells = 0;
    while let Some(character) = characters.next() {
        if character == '!' && characters.peek() == Some(&'[') {
            preview.push("\\!");
            preview.media_neutralized = true;
            continue;
        }
        if character == '<' {
            preview.push("&lt;");
            preview.media_neutralized = true;
            continue;
        }
        if character == '|' {
            table_cells += 1;
            if table_cells > MAX_MARKDOWN_TABLE_CELLS {
                preview.push("\\|");
                preview.truncated = true;
                continue;
            }
        }
        if matches!(character, '*' | '_' | '~') {
            marker_count += 1;
            if marker_count > MAX_MARKDOWN_MARKERS_PER_LINE {
                preview.push("\\");
                preview.truncated = true;
            }
        }
        let mut encoded = [0; 4];
        preview.push(character.encode_utf8(&mut encoded));
    }
}

fn truncate_str(text: &str, max_bytes: usize) -> (&str, bool) {
    if text.len() <= max_bytes {
        return (text, false);
    }
    let mut boundary = max_bytes;
    while boundary > 0 && !text.is_char_boundary(boundary) {
        boundary -= 1;
    }
    (&text[..boundary], true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markdown_preview_neutralizes_ambient_media_and_bounds_nesting() {
        let raw = format!(
            "{}<img src=\"https://example.invalid/a.png\"> ![alt](https://example.invalid/b.png)",
            "> ".repeat(MAX_MARKDOWN_NESTING + 100)
        );
        let preview = bounded_markdown_preview(&raw);
        assert!(preview.truncated);
        assert!(preview.media_neutralized);
        assert!(!preview.text.contains("<img"));
        assert!(preview.text.contains("\\![alt]"));
        assert_eq!(
            preview
                .text
                .chars()
                .take_while(|character| matches!(character, '>' | ' '))
                .filter(|character| *character == '>')
                .count(),
            MAX_MARKDOWN_NESTING
        );
        assert!(preview.text.len() <= MAX_MARKDOWN_PREVIEW_BYTES);
    }

    #[test]
    fn markdown_preview_bounds_code_lines_tables_and_marker_pressure() {
        let code_line = format!("{}\n", "界".repeat(2_048));
        let code = code_line.repeat(100);
        let wide_table = format!(
            "{}\n",
            std::iter::repeat_n("cell", MAX_MARKDOWN_TABLE_CELLS + 20)
                .collect::<Vec<_>>()
                .join("|")
        );
        let tables = wide_table.repeat(MAX_MARKDOWN_TABLE_ROWS + 20);
        let raw = format!(
            "```\n{code}\n```\n{}\n{tables}",
            "*".repeat(MAX_MARKDOWN_MARKERS_PER_LINE + 20)
        );
        let preview = bounded_markdown_preview(&raw);
        assert!(preview.truncated);
        assert!(preview.text.contains("code block truncated"));
        assert!(preview.text.contains("table rows omitted"));
        assert!(preview.text.contains("\\*"));
        assert!(preview.text.contains("\\|"));
        assert!(preview.text.len() <= MAX_MARKDOWN_PREVIEW_BYTES);
        assert!(preview.text.is_char_boundary(preview.text.len()));
    }

    #[test]
    fn fenced_code_does_not_treat_literal_html_as_media() {
        let preview = bounded_markdown_preview("```html\n<img src=\"literal\">\n```\n");
        assert!(!preview.truncated);
        assert!(!preview.media_neutralized);
        assert!(preview.text.contains("<img src=\"literal\">"));
    }

    #[test]
    fn malformed_long_markdown_remains_bounded_and_unicode_valid() {
        let raw = format!(
            "{}{}",
            "[*".repeat(MAX_MARKDOWN_LINE_BYTES),
            "\n界".repeat(MAX_MARKDOWN_LINES + 100)
        );
        let preview = bounded_markdown_preview(&raw);
        assert!(preview.truncated);
        assert!(preview.text.len() <= MAX_MARKDOWN_PREVIEW_BYTES);
        assert!(preview.text.is_char_boundary(preview.text.len()));
    }
}
