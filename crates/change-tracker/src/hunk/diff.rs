use std::path::Path;

use sha2::{Digest, Sha256};
use similar::TextDiff;

use super::{ChangeSource, HunkId, HunkRange, TrackingContext};

#[derive(Clone)]
pub(super) struct HunkIdentity {
    pub(super) id: HunkId,
    pub(super) fingerprint: String,
    pub(super) range: HunkRange,
    pub(super) source: ChangeSource,
    pub(super) context: Option<TrackingContext>,
    pub(super) before_revision: Option<String>,
    pub(super) after_revision: String,
}

pub(super) struct ParsedHunk {
    pub(super) fingerprint: String,
    pub(super) range: HunkRange,
    pub(super) diff: Option<String>,
}

pub(super) fn bounded_unified_diff(
    path: &Path,
    before: &[u8],
    after: &[u8],
    max_bytes: usize,
    max_lines: usize,
) -> Option<String> {
    if before.iter().filter(|byte| **byte == b'\n').count() > max_lines
        || after.iter().filter(|byte| **byte == b'\n').count() > max_lines
    {
        return None;
    }
    let before = std::str::from_utf8(before).ok()?;
    let after = std::str::from_utf8(after).ok()?;
    let diff = TextDiff::from_lines(before, after)
        .unified_diff()
        .context_radius(3)
        .header(&path.to_string_lossy(), &path.to_string_lossy())
        .to_string();
    (diff.len() <= max_bytes).then_some(diff)
}

pub(super) fn parse_hunks(diff: Option<&str>, after_revision: &str) -> Vec<ParsedHunk> {
    let Some(diff) = diff else {
        return vec![synthetic_hunk(after_revision)];
    };
    let mut hunks = Vec::new();
    let mut current_header: Option<HunkRange> = None;
    let mut current_lines = Vec::new();
    for line in diff.lines() {
        if let Some(range) = parse_hunk_header(line) {
            if let Some(previous) = current_header.take() {
                hunks.push(parsed_hunk(previous, &current_lines));
                current_lines.clear();
            }
            current_header = Some(range);
        } else if current_header.is_some() {
            current_lines.push(line.to_owned());
        }
    }
    if let Some(range) = current_header {
        hunks.push(parsed_hunk(range, &current_lines));
    }
    if hunks.is_empty() {
        vec![synthetic_hunk(after_revision)]
    } else {
        hunks
    }
}

fn parse_hunk_header(line: &str) -> Option<HunkRange> {
    let body = line.strip_prefix("@@ -")?.split(" @@").next()?;
    let (old, new) = body.split_once(" +")?;
    let (old_start, old_lines) = parse_range(old)?;
    let (new_start, new_lines) = parse_range(new)?;
    Some(HunkRange {
        old_start,
        old_lines,
        new_start,
        new_lines,
    })
}

fn parse_range(value: &str) -> Option<(usize, usize)> {
    let (start, lines) = value.split_once(',').unwrap_or((value, "1"));
    Some((start.parse().ok()?, lines.parse().ok()?))
}

fn parsed_hunk(range: HunkRange, lines: &[String]) -> ParsedHunk {
    let changed = lines
        .iter()
        .filter(|line| line.starts_with('+') || line.starts_with('-'))
        .cloned()
        .collect::<Vec<_>>()
        .join("\n");
    let fingerprint = content_fingerprint(changed.as_bytes());
    let mut body = format!(
        "@@ -{},{} +{},{} @@",
        range.old_start, range.old_lines, range.new_start, range.new_lines
    );
    if !lines.is_empty() {
        body.push('\n');
        body.push_str(&lines.join("\n"));
    }
    ParsedHunk {
        fingerprint,
        range,
        diff: Some(body),
    }
}

fn synthetic_hunk(after_revision: &str) -> ParsedHunk {
    ParsedHunk {
        fingerprint: after_revision.to_owned(),
        range: HunkRange {
            old_start: 0,
            old_lines: 0,
            new_start: 0,
            new_lines: 0,
        },
        diff: None,
    }
}

pub(super) fn best_identity_match(
    parsed: &ParsedHunk,
    old: &[HunkIdentity],
    used: &[bool],
) -> Option<usize> {
    old.iter()
        .enumerate()
        .filter(|(index, identity)| !used[*index] && identity.fingerprint == parsed.fingerprint)
        .min_by_key(|(_, identity)| identity.range.new_start.abs_diff(parsed.range.new_start))
        .map(|(index, _)| index)
        .or_else(|| {
            old.iter()
                .enumerate()
                .filter(|(index, _)| !used[*index])
                .max_by_key(|(_, identity)| range_overlap(identity.range, parsed.range))
                .filter(|(_, identity)| range_overlap(identity.range, parsed.range) > 0)
                .map(|(index, _)| index)
        })
}

fn range_overlap(left: HunkRange, right: HunkRange) -> usize {
    let left_end = left.new_start.saturating_add(left.new_lines);
    let right_end = right.new_start.saturating_add(right.new_lines);
    left_end
        .min(right_end)
        .saturating_sub(left.new_start.max(right.new_start))
}

fn content_fingerprint(content: &[u8]) -> String {
    format!("{:x}", Sha256::digest(content))
}

pub(super) fn replace_line_range(
    base: &[u8],
    base_start: usize,
    base_count: usize,
    source: &[u8],
    source_start: usize,
    source_count: usize,
) -> Result<Vec<u8>, String> {
    let base = std::str::from_utf8(base).map_err(|_| "base content is not UTF-8")?;
    let source = std::str::from_utf8(source).map_err(|_| "source content is not UTF-8")?;
    let base_lines = split_lines_preserving_endings(base);
    let source_lines = split_lines_preserving_endings(source);
    let base_index = line_index(base_start);
    let source_index = line_index(source_start);
    let base_end = base_index
        .checked_add(base_count)
        .filter(|end| *end <= base_lines.len())
        .ok_or("base hunk range is outside content")?;
    let source_end = source_index
        .checked_add(source_count)
        .filter(|end| *end <= source_lines.len())
        .ok_or("source hunk range is outside content")?;
    if base_index > base_lines.len() || source_index > source_lines.len() {
        return Err("hunk start is outside content".into());
    }
    let capacity = base.len().saturating_add(
        source_lines[source_index..source_end]
            .iter()
            .map(|line| line.len())
            .sum(),
    );
    let mut patched = String::with_capacity(capacity);
    patched.extend(base_lines[..base_index].iter().copied());
    patched.extend(source_lines[source_index..source_end].iter().copied());
    patched.extend(base_lines[base_end..].iter().copied());
    Ok(patched.into_bytes())
}

fn split_lines_preserving_endings(content: &str) -> Vec<&str> {
    content.split_inclusive('\n').collect()
}

fn line_index(start: usize) -> usize {
    start.saturating_sub(1)
}
