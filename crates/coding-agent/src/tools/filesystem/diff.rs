#[derive(Debug, Clone, PartialEq, Eq)]
enum DiffPart {
    Equal(Vec<String>),
    Removed(Vec<String>),
    Added(Vec<String>),
}

pub(crate) struct DiffString {
    pub diff: String,
    pub first_changed_line: Option<usize>,
}

fn split_lines(content: &str) -> Vec<String> {
    let mut lines = content.split('\n').map(str::to_string).collect::<Vec<_>>();
    if lines.last().is_some_and(|line| line.is_empty()) {
        lines.pop();
    }
    lines
}

fn diff_parts(old_content: &str, new_content: &str) -> Vec<DiffPart> {
    let old_lines = split_lines(old_content);
    let new_lines = split_lines(new_content);
    let old_len = old_lines.len();
    let new_len = new_lines.len();
    let mut lcs = vec![vec![0usize; new_len + 1]; old_len + 1];

    for old_idx in (0..old_len).rev() {
        for new_idx in (0..new_len).rev() {
            lcs[old_idx][new_idx] = if old_lines[old_idx] == new_lines[new_idx] {
                lcs[old_idx + 1][new_idx + 1] + 1
            } else {
                lcs[old_idx + 1][new_idx].max(lcs[old_idx][new_idx + 1])
            };
        }
    }

    let mut raw_parts = Vec::new();
    let (mut old_idx, mut new_idx) = (0usize, 0usize);
    while old_idx < old_len && new_idx < new_len {
        if old_lines[old_idx] == new_lines[new_idx] {
            raw_parts.push(DiffPart::Equal(vec![old_lines[old_idx].clone()]));
            old_idx += 1;
            new_idx += 1;
        } else if lcs[old_idx + 1][new_idx] >= lcs[old_idx][new_idx + 1] {
            raw_parts.push(DiffPart::Removed(vec![old_lines[old_idx].clone()]));
            old_idx += 1;
        } else {
            raw_parts.push(DiffPart::Added(vec![new_lines[new_idx].clone()]));
            new_idx += 1;
        }
    }
    while old_idx < old_len {
        raw_parts.push(DiffPart::Removed(vec![old_lines[old_idx].clone()]));
        old_idx += 1;
    }
    while new_idx < new_len {
        raw_parts.push(DiffPart::Added(vec![new_lines[new_idx].clone()]));
        new_idx += 1;
    }

    coalesce_parts(raw_parts)
}

fn coalesce_parts(parts: Vec<DiffPart>) -> Vec<DiffPart> {
    let mut coalesced: Vec<DiffPart> = Vec::new();
    for part in parts {
        match (coalesced.last_mut(), part) {
            (Some(DiffPart::Equal(existing)), DiffPart::Equal(mut lines))
            | (Some(DiffPart::Removed(existing)), DiffPart::Removed(mut lines))
            | (Some(DiffPart::Added(existing)), DiffPart::Added(mut lines)) => {
                existing.append(&mut lines);
            }
            (_, part) => coalesced.push(part),
        }
    }
    coalesced
}

pub(crate) fn generate_diff_string(
    old_content: &str,
    new_content: &str,
    context_lines: usize,
) -> DiffString {
    let parts = diff_parts(old_content, new_content);
    let old_lines = split_lines(old_content);
    let new_lines = split_lines(new_content);
    let max_line_num = old_lines.len().max(new_lines.len()).max(1);
    let line_num_width = max_line_num.to_string().len();

    let mut output = Vec::new();
    let mut old_line_num = 1usize;
    let mut new_line_num = 1usize;
    let mut last_was_change = false;
    let mut first_changed_line = None;

    for (index, part) in parts.iter().enumerate() {
        match part {
            DiffPart::Added(lines) | DiffPart::Removed(lines) => {
                if first_changed_line.is_none() {
                    first_changed_line = Some(new_line_num);
                }

                for line in lines {
                    match part {
                        DiffPart::Added(_) => {
                            output.push(format!(
                                "+{:>width$} {}",
                                new_line_num,
                                line,
                                width = line_num_width
                            ));
                            new_line_num += 1;
                        }
                        DiffPart::Removed(_) => {
                            output.push(format!(
                                "-{:>width$} {}",
                                old_line_num,
                                line,
                                width = line_num_width
                            ));
                            old_line_num += 1;
                        }
                        DiffPart::Equal(_) => unreachable!(),
                    }
                }
                last_was_change = true;
            }
            DiffPart::Equal(lines) => {
                let next_part_is_change = parts
                    .get(index + 1)
                    .is_some_and(|part| !matches!(part, DiffPart::Equal(_)));
                let has_leading_change = last_was_change;
                let has_trailing_change = next_part_is_change;

                if has_leading_change && has_trailing_change {
                    if lines.len() <= context_lines * 2 {
                        push_context_lines(
                            &mut output,
                            lines,
                            &mut old_line_num,
                            &mut new_line_num,
                            line_num_width,
                        );
                    } else {
                        let leading = &lines[..context_lines];
                        let trailing = &lines[lines.len() - context_lines..];
                        let skipped = lines.len() - leading.len() - trailing.len();
                        push_context_lines(
                            &mut output,
                            leading,
                            &mut old_line_num,
                            &mut new_line_num,
                            line_num_width,
                        );
                        output.push(format!(" {:>width$} ...", "", width = line_num_width));
                        old_line_num += skipped;
                        new_line_num += skipped;
                        push_context_lines(
                            &mut output,
                            trailing,
                            &mut old_line_num,
                            &mut new_line_num,
                            line_num_width,
                        );
                    }
                } else if has_leading_change {
                    let shown = lines.len().min(context_lines);
                    push_context_lines(
                        &mut output,
                        &lines[..shown],
                        &mut old_line_num,
                        &mut new_line_num,
                        line_num_width,
                    );
                    let skipped = lines.len() - shown;
                    if skipped > 0 {
                        output.push(format!(" {:>width$} ...", "", width = line_num_width));
                        old_line_num += skipped;
                        new_line_num += skipped;
                    }
                } else if has_trailing_change {
                    let skipped = lines.len().saturating_sub(context_lines);
                    if skipped > 0 {
                        output.push(format!(" {:>width$} ...", "", width = line_num_width));
                        old_line_num += skipped;
                        new_line_num += skipped;
                    }
                    push_context_lines(
                        &mut output,
                        &lines[skipped..],
                        &mut old_line_num,
                        &mut new_line_num,
                        line_num_width,
                    );
                } else {
                    old_line_num += lines.len();
                    new_line_num += lines.len();
                }

                last_was_change = false;
            }
        }
    }

    DiffString {
        diff: output.join("\n"),
        first_changed_line,
    }
}

fn push_context_lines(
    output: &mut Vec<String>,
    lines: &[String],
    old_line_num: &mut usize,
    new_line_num: &mut usize,
    line_num_width: usize,
) {
    for line in lines {
        output.push(format!(
            " {:>width$} {}",
            *old_line_num,
            line,
            width = line_num_width
        ));
        *old_line_num += 1;
        *new_line_num += 1;
    }
}

pub(crate) fn generate_unified_patch(path: &str, old_content: &str, new_content: &str) -> String {
    let old_lines = split_lines(old_content);
    let new_lines = split_lines(new_content);
    let old_count = old_lines.len();
    let new_count = new_lines.len();
    let mut output = vec![
        format!("--- {path}"),
        format!("+++ {path}"),
        format!("@@ -1,{old_count} +1,{new_count} @@"),
    ];

    for part in diff_parts(old_content, new_content) {
        match part {
            DiffPart::Equal(lines) => {
                output.extend(lines.into_iter().map(|line| format!(" {line}")));
            }
            DiffPart::Removed(lines) => {
                output.extend(lines.into_iter().map(|line| format!("-{line}")));
            }
            DiffPart::Added(lines) => {
                output.extend(lines.into_iter().map(|line| format!("+{line}")));
            }
        }
    }

    output.join("\n")
}

/// A matched replacement: byte offset and length in the base content, plus the
/// new text to splice in. Mirrors TS `TextReplacement`.
#[derive(Debug, Clone, Copy)]
pub(crate) struct TextReplacement<'a> {
    pub match_index: usize,
    pub match_length: usize,
    pub new_text: &'a str,
}

/// Byte span of a single line (including its line ending) within a string.
/// Mirrors TS `LineSpan`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct LineSpan {
    start: usize,
    end: usize,
}

/// Split content into lines, each retaining its line ending (like TS
/// `splitLinesWithEndings`'s `/[^\n]*\n|[^\n]+/g`). The concatenation of all
/// returned slices equals the original content.
fn split_lines_with_endings(content: &str) -> Vec<&str> {
    let mut lines = Vec::new();
    let mut start = 0;
    let bytes = content.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\n' {
            // include the newline in this slice
            lines.push(&content[start..=i]);
            start = i + 1;
        }
        i += 1;
    }
    if start < content.len() {
        lines.push(&content[start..]);
    }
    lines
}

/// Byte spans of each line (with endings) in `content`. Mirrors TS
/// `getLineSpans`.
fn line_spans(content: &str) -> Vec<LineSpan> {
    let mut offset = 0;
    split_lines_with_endings(content)
        .iter()
        .map(|line| {
            let span = LineSpan {
                start: offset,
                end: offset + line.len(),
            };
            offset = span.end;
            span
        })
        .collect()
}

/// The `[start_line, end_line)` range (half-open) of lines touched by a
/// replacement, in base-content line coordinates. Mirrors TS
/// `getReplacementLineRange`.
fn replacement_line_range(
    lines: &[LineSpan],
    replacement: TextReplacement<'_>,
) -> Option<(usize, usize)> {
    let replacement_start = replacement.match_index;
    let replacement_end = replacement.match_index + replacement.match_length;

    let start_line = lines
        .iter()
        .position(|line| replacement_start >= line.start && replacement_start < line.end)?;

    let mut end_line = start_line;
    while end_line < lines.len() && lines[end_line].end < replacement_end {
        end_line += 1;
    }
    if end_line >= lines.len() {
        return None;
    }
    Some((start_line, end_line + 1))
}

/// Splice `replacements` into `content`, applying them in reverse offset
/// order so earlier offsets stay valid. `offset` is subtracted from each
/// `match_index` (the group's start offset) when the content is a slice.
/// Mirrors TS `applyReplacements`.
fn apply_replacements(
    content: &str,
    replacements: &[TextReplacement<'_>],
    offset: usize,
) -> String {
    let mut result = content.to_string();
    use std::cmp::Reverse;

    // Apply in reverse offset order so prior replacements don't shift later
    // offsets. Sort by match_index descending.
    let mut ordered: Vec<TextReplacement<'_>> = replacements.to_vec();
    ordered.sort_by_key(|r| Reverse(r.match_index));
    for replacement in ordered {
        let match_index = replacement.match_index - offset;
        let before = &result[..match_index];
        let after = &result[match_index + replacement.match_length..];
        result = format!("{before}{new_text}{after}", new_text = replacement.new_text);
    }
    result
}

/// Apply replacements matched against `base_content` to `original_content`
/// while preserving unchanged line blocks from the original.
///
/// Mirrors TS `applyReplacementsPreservingUnchangedLines`: each replacement is
/// widened to the lines it touches, those touched lines are rewritten from the
/// normalized base, and all other lines are copied back from `original_content`.
/// This keeps unchanged lines' original bytes (e.g. smart quotes) intact when
/// the match was done in fuzzy-normalized space.
///
/// Returns `None` if line counts differ or a replacement falls outside the
/// base content (callers surface an error).
pub(crate) fn apply_replacements_preserving_unchanged_lines(
    original_content: &str,
    base_content: &str,
    replacements: &[TextReplacement<'_>],
) -> Option<String> {
    let original_lines = split_lines_with_endings(original_content);
    let base_lines = line_spans(base_content);
    if original_lines.len() != base_lines.len() {
        return None;
    }

    // Group overlapping/nearby replacements (same line range) so they're
    // applied together within their line span.
    let mut sorted: Vec<TextReplacement<'_>> = replacements.to_vec();
    sorted.sort_by_key(|r| r.match_index);
    let mut groups: Vec<(usize, usize, Vec<TextReplacement<'_>>)> = Vec::new();
    for replacement in sorted {
        let (start_line, end_line) = replacement_line_range(&base_lines, replacement)?;
        if let Some(last) = groups.last_mut()
            && start_line < last.1
        {
            last.1 = last.1.max(end_line);
            last.2.push(replacement);
            continue;
        }
        groups.push((start_line, end_line, vec![replacement]));
    }

    let mut original_line_index = 0usize;
    let mut result = String::new();
    for (start_line, end_line, group_replacements) in &groups {
        // Unchanged lines before this group: copy from original.
        for line in &original_lines[original_line_index..*start_line] {
            result.push_str(line);
        }
        // Touched lines: rewrite from the normalized base, splicing in the
        // replacements within this group's byte span.
        let group_start_offset = base_lines[*start_line].start;
        let group_end_offset = base_lines[*end_line - 1].end;
        let group_slice = &base_content[group_start_offset..group_end_offset];
        result.push_str(&apply_replacements(
            group_slice,
            group_replacements,
            group_start_offset,
        ));
        original_line_index = *end_line;
    }
    // Trailing unchanged lines.
    for line in &original_lines[original_line_index..] {
        result.push_str(line);
    }
    Some(result)
}
