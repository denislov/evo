//! Bounded, GPUI-independent presentation for product-authorized file reviews.

use coding_agent::api::review::{
    CodingAgentExternalEditorTarget, CodingAgentFileReview, CodingAgentFileReviewRequest,
};

pub(crate) const MAX_VISIBLE_FILE_CHANGES: usize = 64;
pub(crate) const MAX_REVIEW_ROWS: usize = 480;
pub(crate) const MAX_REVIEW_LINE_BYTES: usize = 2 * 1024;
pub(crate) const MAX_REVIEW_RENDER_BYTES: usize = 160 * 1024;
pub(crate) const MAX_REVIEW_CLIPBOARD_BYTES: usize = 128 * 1024;
const MAX_REVIEW_PATH_BYTES: usize = 4 * 1024;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DesktopReviewLineKind {
    FileHeader,
    HunkHeader,
    Added,
    Removed,
    Context,
    Fold,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DesktopReviewLine {
    pub(crate) kind: DesktopReviewLineKind,
    pub(crate) text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DesktopClipboardExport {
    pub(crate) text: String,
    pub(crate) truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DesktopFileReviewDocument {
    pub(crate) request: CodingAgentFileReviewRequest,
    pub(crate) display_path: String,
    pub(crate) display_path_truncated: bool,
    pub(crate) mutation_kind: String,
    pub(crate) total_bytes: usize,
    pub(crate) total_lines: usize,
    pub(crate) source_truncated: bool,
    pub(crate) rows_truncated: bool,
    pub(crate) using_diff: bool,
    pub(crate) rows: Vec<DesktopReviewLine>,
    pub(crate) external_editor_target: Option<CodingAgentExternalEditorTarget>,
}

impl DesktopFileReviewDocument {
    pub(crate) fn from_product(review: CodingAgentFileReview) -> Self {
        let request = CodingAgentFileReviewRequest::new(review.change, review.revision);
        let (display_path, display_path_truncated) =
            truncate_utf8(&review.display_path, MAX_REVIEW_PATH_BYTES);
        let using_diff = review.diff.is_some();
        let (rows, rows_truncated) = match review.diff.as_deref() {
            Some(diff) => project_unified_diff(diff),
            None => project_file_content(
                &review.content,
                review.line_count,
                review.first_changed_line,
            ),
        };
        Self {
            request,
            display_path,
            display_path_truncated,
            mutation_kind: truncate_utf8(&review.mutation_kind, 64).0,
            total_bytes: review.total_bytes,
            total_lines: review.line_count,
            source_truncated: review.content_truncated || review.diff_truncated,
            rows_truncated,
            using_diff,
            rows,
            external_editor_target: review.external_editor_target,
        }
    }

    pub(crate) fn clipboard_export(&self) -> DesktopClipboardExport {
        let mut text = String::with_capacity(MAX_REVIEW_CLIPBOARD_BYTES.min(8 * 1024));
        let mut truncated = self.source_truncated || self.rows_truncated;
        for row in &self.rows {
            let prefix = match row.kind {
                DesktopReviewLineKind::Added => "+",
                DesktopReviewLineKind::Removed => "-",
                DesktopReviewLineKind::Context => " ",
                DesktopReviewLineKind::Fold => "… ",
                DesktopReviewLineKind::FileHeader | DesktopReviewLineKind::HunkHeader => "",
            };
            let required = prefix.len() + row.text.len() + 1;
            if text.len().saturating_add(required) > MAX_REVIEW_CLIPBOARD_BYTES {
                truncated = true;
                break;
            }
            text.push_str(prefix);
            text.push_str(&row.text);
            text.push('\n');
        }
        DesktopClipboardExport { text, truncated }
    }

    pub(crate) fn path_clipboard_export(&self) -> DesktopClipboardExport {
        let (text, truncated) = truncate_utf8(&self.display_path, MAX_REVIEW_PATH_BYTES);
        DesktopClipboardExport {
            text,
            truncated: self.display_path_truncated || truncated,
        }
    }
}

fn project_unified_diff(diff: &str) -> (Vec<DesktopReviewLine>, bool) {
    let mut builder = ReviewRows::default();
    let mut previous_old_end = None;
    for line in diff.lines() {
        let (kind, text) = if line.starts_with("--- ") || line.starts_with("+++ ") {
            (DesktopReviewLineKind::FileHeader, line)
        } else if line.starts_with("@@ ") {
            if let Some((old_start, old_len)) = parse_old_hunk_range(line) {
                let omitted = previous_old_end
                    .map(|end| old_start.saturating_sub(end))
                    .unwrap_or_else(|| old_start.saturating_sub(1));
                if omitted > 0
                    && !builder.push(
                        DesktopReviewLineKind::Fold,
                        &format!("{omitted} unchanged line(s) collapsed"),
                    )
                {
                    break;
                }
                previous_old_end = Some(old_start.saturating_add(old_len));
            }
            (DesktopReviewLineKind::HunkHeader, line)
        } else if let Some(text) = line.strip_prefix('+') {
            (DesktopReviewLineKind::Added, text)
        } else if let Some(text) = line.strip_prefix('-') {
            (DesktopReviewLineKind::Removed, text)
        } else if let Some(text) = line.strip_prefix(' ') {
            (DesktopReviewLineKind::Context, text)
        } else {
            (DesktopReviewLineKind::Context, line)
        };
        if !builder.push(kind, text) {
            break;
        }
    }
    builder.finish()
}

fn project_file_content(
    content: &str,
    total_lines: usize,
    first_changed_line: Option<usize>,
) -> (Vec<DesktopReviewLine>, bool) {
    const LINES_BEFORE: usize = 80;
    const LINES_AFTER: usize = 160;
    let center = first_changed_line
        .unwrap_or(1)
        .max(1)
        .min(total_lines.max(1));
    let start = center.saturating_sub(LINES_BEFORE).max(1);
    let end = center.saturating_add(LINES_AFTER).min(total_lines.max(1));
    let mut builder = ReviewRows::default();
    if start > 1 {
        let _ = builder.push(
            DesktopReviewLineKind::Fold,
            &format!("{} unchanged line(s) collapsed", start - 1),
        );
    }
    for (index, line) in content.lines().enumerate() {
        let number = index + 1;
        if number < start {
            continue;
        }
        if number > end {
            break;
        }
        if !builder.push(DesktopReviewLineKind::Context, line) {
            break;
        }
    }
    if end < total_lines && !builder.exhausted {
        let _ = builder.push(
            DesktopReviewLineKind::Fold,
            &format!("{} unchanged line(s) collapsed", total_lines - end),
        );
    }
    builder.finish()
}

#[derive(Default)]
struct ReviewRows {
    rows: Vec<DesktopReviewLine>,
    rendered_bytes: usize,
    exhausted: bool,
    truncated: bool,
}

impl ReviewRows {
    fn push(&mut self, kind: DesktopReviewLineKind, text: &str) -> bool {
        if self.rows.len() >= MAX_REVIEW_ROWS {
            self.exhausted = true;
            return false;
        }
        let remaining = MAX_REVIEW_RENDER_BYTES.saturating_sub(self.rendered_bytes);
        if remaining == 0 {
            self.exhausted = true;
            return false;
        }
        let line_limit = remaining.min(MAX_REVIEW_LINE_BYTES);
        let (mut text, line_truncated) = truncate_utf8(text, line_limit);
        if line_truncated {
            self.truncated = true;
            const MARKER: &str = " … [line truncated]";
            if line_limit >= MARKER.len() {
                text = truncate_utf8(&text, line_limit - MARKER.len()).0;
                text.push_str(MARKER);
            }
        }
        self.rendered_bytes = self.rendered_bytes.saturating_add(text.len());
        self.rows.push(DesktopReviewLine { kind, text });
        true
    }

    fn finish(self) -> (Vec<DesktopReviewLine>, bool) {
        (self.rows, self.exhausted || self.truncated)
    }
}

fn parse_old_hunk_range(line: &str) -> Option<(usize, usize)> {
    let token = line.strip_prefix("@@ -")?.split_whitespace().next()?;
    let mut fields = token.split(',');
    let start = fields.next()?.parse().ok()?;
    let len = fields.next().map(str::parse).transpose().ok()?.unwrap_or(1);
    Some((start, len))
}

fn truncate_utf8(value: &str, max_bytes: usize) -> (String, bool) {
    if value.len() <= max_bytes {
        return (value.to_owned(), false);
    }
    let mut end = max_bytes;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    (value[..end].to_owned(), true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use coding_agent::api::review::{CodingAgentFileChangeIdentity, CodingAgentFileRevision};

    fn review(content: String, diff: Option<String>) -> CodingAgentFileReview {
        CodingAgentFileReview {
            change: CodingAgentFileChangeIdentity {
                operation_id: "operation-review".into(),
                tool_call_id: Some("call-review".into()),
                path: "src/lib.rs".into(),
            },
            revision: CodingAgentFileRevision::new(9),
            display_path: "src/lib.rs".into(),
            mutation_kind: "edit".into(),
            total_bytes: content.len(),
            line_count: content.lines().count(),
            content,
            content_truncated: false,
            diff,
            diff_truncated: false,
            first_changed_line: Some(2),
            added_lines: Some(1),
            removed_lines: Some(1),
            external_editor_target: None,
        }
    }

    #[test]
    fn unified_diff_is_bounded_and_marks_collapsed_unchanged_ranges() {
        let diff = "--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -20,3 +20,3 @@\n old\n-old\n+new\n keep\n@@ -200,2 +200,2 @@\n-old2\n+new2\n";
        let document =
            DesktopFileReviewDocument::from_product(review("new\n".into(), Some(diff.into())));
        assert!(document.using_diff);
        assert!(document.rows.iter().any(|row| {
            row.kind == DesktopReviewLineKind::Fold && row.text.contains("19 unchanged")
        }));
        assert!(document.rows.iter().any(|row| {
            row.kind == DesktopReviewLineKind::Fold && row.text.contains("177 unchanged")
        }));
        assert!(
            document
                .rows
                .iter()
                .any(|row| row.kind == DesktopReviewLineKind::Added && row.text == "new")
        );
    }

    #[test]
    fn huge_long_line_and_clipboard_projection_stay_within_hard_limits() {
        let content = format!("before\n{}\nafter\n", "界".repeat(200_000));
        let document = DesktopFileReviewDocument::from_product(review(content, None));
        assert!(
            document
                .rows
                .iter()
                .all(|row| row.text.len() <= MAX_REVIEW_LINE_BYTES)
        );
        assert!(
            document
                .rows
                .iter()
                .any(|row| row.text.contains("[line truncated]"))
        );
        let export = document.clipboard_export();
        assert!(export.text.len() <= MAX_REVIEW_CLIPBOARD_BYTES);
    }

    #[test]
    fn clipboard_export_stops_before_allocating_past_its_byte_limit() {
        let diff = (0..MAX_REVIEW_ROWS)
            .map(|index| format!("+{index:04} {}", "x".repeat(1_900)))
            .collect::<Vec<_>>()
            .join("\n");
        let document =
            DesktopFileReviewDocument::from_product(review("bounded\n".into(), Some(diff)));
        let export = document.clipboard_export();
        assert!(export.truncated);
        assert!(export.text.len() <= MAX_REVIEW_CLIPBOARD_BYTES);
        assert!(export.text.is_char_boundary(export.text.len()));
    }

    #[test]
    fn display_path_and_path_clipboard_export_share_the_utf8_safe_bound() {
        let mut product = review("bounded\n".into(), None);
        product.display_path = "界".repeat(MAX_REVIEW_PATH_BYTES);
        let document = DesktopFileReviewDocument::from_product(product);

        assert!(document.display_path.len() <= MAX_REVIEW_PATH_BYTES);
        assert!(
            document
                .display_path
                .is_char_boundary(document.display_path.len())
        );
        let export = document.path_clipboard_export();
        assert!(export.truncated);
        assert!(export.text.len() <= MAX_REVIEW_PATH_BYTES);
        assert!(export.text.is_char_boundary(export.text.len()));
    }
}
