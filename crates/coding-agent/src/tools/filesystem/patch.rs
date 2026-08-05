//! A bounded, capability-agnostic parser and applier for Codex-style patches.
//!
//! The module deliberately stops at pure text transformation. Filesystem
//! binding, mutation fences, and durable receipts remain owned by the caller.

use std::fmt;

use crate::tools::filesystem::text_match::seek_unique_sequence;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Patch {
    pub(crate) files: Vec<FilePatch>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FilePatch {
    pub(crate) path: String,
    pub(crate) operation: PatchOperation,
    pub(crate) hunks: Vec<PatchHunk>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PatchOperation {
    Add,
    Update,
    Delete,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PatchHunk {
    pub(crate) old_start: usize,
    pub(crate) old_count: usize,
    pub(crate) new_start: usize,
    pub(crate) new_count: usize,
    pub(crate) lines: Vec<PatchLine>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PatchLine {
    Context(String),
    Remove(String),
    Add(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PatchError {
    message: String,
}

impl PatchError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for PatchError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for PatchError {}

pub(crate) fn parse_patch(input: &str) -> Result<Patch, PatchError> {
    const BEGIN: &str = "*** Begin Patch";
    const END: &str = "*** End Patch";
    if input.len() > crate::limits::MAX_PATCH_INPUT_BYTES {
        return Err(PatchError::new(format!(
            "patch exceeds the {} byte safety limit",
            crate::limits::MAX_PATCH_INPUT_BYTES
        )));
    }
    let mut source = input
        .lines()
        .map(|line| line.strip_suffix('\r').unwrap_or(line));
    if source.next().map(str::trim) != Some(BEGIN) {
        return Err(PatchError::new("patch must start with `*** Begin Patch`"));
    }
    let mut body = Vec::new();
    let mut saw_end = false;
    for line in source.by_ref() {
        if line.trim() == END {
            saw_end = true;
            break;
        }
        body.push(line);
    }
    if !saw_end {
        return Err(PatchError::new("patch is missing `*** End Patch`"));
    }
    if source.any(|line| !line.trim().is_empty()) {
        return Err(PatchError::new(
            "patch contains content after `*** End Patch`",
        ));
    }
    let mut files = Vec::new();
    let mut index = 0usize;
    while index < body.len() {
        let line = body[index];
        let (operation, path) = if let Some(path) = line.strip_prefix("*** Add File: ") {
            (PatchOperation::Add, path)
        } else if let Some(path) = line.strip_prefix("*** Update File: ") {
            (PatchOperation::Update, path)
        } else if let Some(path) = line.strip_prefix("*** Delete File: ") {
            (PatchOperation::Delete, path)
        } else {
            return Err(PatchError::new(format!(
                "unexpected patch directive: {line}"
            )));
        };
        if path.trim().is_empty() || path.contains('\0') {
            return Err(PatchError::new(
                "patch file path must be non-empty and NUL-free",
            ));
        }
        index += 1;
        let start = index;
        while index < body.len() && !body[index].starts_with("*** ") {
            index += 1;
        }
        let file_lines = &body[start..index];
        let hunks = match operation {
            PatchOperation::Add => {
                let mut added = Vec::new();
                for line in file_lines {
                    let text = line
                        .strip_prefix('+')
                        .ok_or_else(|| PatchError::new("add-file lines must begin with `+`"))?;
                    added.push(PatchLine::Add(text.to_owned()));
                }
                vec![PatchHunk {
                    old_start: 0,
                    old_count: 0,
                    new_start: 1,
                    new_count: added.len(),
                    lines: added,
                }]
            }
            PatchOperation::Delete => {
                if !file_lines.is_empty() {
                    return Err(PatchError::new(
                        "delete-file directive cannot contain hunk lines",
                    ));
                }
                Vec::new()
            }
            PatchOperation::Update => parse_hunks(file_lines)?,
        };
        files.push(FilePatch {
            path: path.trim().to_owned(),
            operation,
            hunks,
        });
        if files.len() > crate::limits::MAX_PATCH_FILES {
            return Err(PatchError::new(format!(
                "patch contains more than {} file directives",
                crate::limits::MAX_PATCH_FILES
            )));
        }
    }
    if files.is_empty() {
        return Err(PatchError::new("patch must contain at least one file"));
    }
    let total_hunks = files.iter().map(|file| file.hunks.len()).sum::<usize>();
    let total_lines = files
        .iter()
        .flat_map(|file| &file.hunks)
        .map(|hunk| hunk.lines.len())
        .sum::<usize>();
    if total_hunks > crate::limits::MAX_PATCH_HUNKS {
        return Err(PatchError::new(format!(
            "patch contains more than {} hunks",
            crate::limits::MAX_PATCH_HUNKS
        )));
    }
    if total_lines > crate::limits::MAX_PATCH_LINES {
        return Err(PatchError::new(format!(
            "patch contains more than {} hunk lines",
            crate::limits::MAX_PATCH_LINES
        )));
    }
    Ok(Patch { files })
}

fn parse_hunks(lines: &[&str]) -> Result<Vec<PatchHunk>, PatchError> {
    if lines.len() > crate::limits::MAX_PATCH_LINES {
        return Err(PatchError::new(format!(
            "patch file contains more than {} hunk lines",
            crate::limits::MAX_PATCH_LINES
        )));
    }
    let mut hunks = Vec::new();
    let mut index = 0usize;
    while index < lines.len() {
        let header = lines[index];
        if !header.starts_with("@@") {
            return Err(PatchError::new(format!(
                "expected hunk header, got: {header}"
            )));
        }
        let (old_start, old_count, new_start, new_count) = if header.trim() == "@@" {
            (1, usize::MAX, 1, usize::MAX)
        } else {
            parse_hunk_header(header)?
        };
        index += 1;
        let mut hunk_lines = Vec::new();
        while index < lines.len() && !lines[index].starts_with("@@") {
            let line = lines[index];
            if line.is_empty() {
                return Err(PatchError::new("empty hunk line is invalid; use a prefix"));
            }
            let (kind, text) = line.split_at(1);
            match kind {
                " " => hunk_lines.push(PatchLine::Context(text.to_owned())),
                "-" => hunk_lines.push(PatchLine::Remove(text.to_owned())),
                "+" => hunk_lines.push(PatchLine::Add(text.to_owned())),
                "\\" => {
                    // `\ No newline at end of file` is metadata, not content.
                    if line != "\\ No newline at end of file" {
                        return Err(PatchError::new(format!("invalid hunk metadata: {line}")));
                    }
                }
                _ => return Err(PatchError::new(format!("invalid hunk line: {line}"))),
            }
            index += 1;
        }
        let old_seen = hunk_lines
            .iter()
            .filter(|line| matches!(line, PatchLine::Context(_) | PatchLine::Remove(_)))
            .count();
        let new_seen = hunk_lines
            .iter()
            .filter(|line| matches!(line, PatchLine::Context(_) | PatchLine::Add(_)))
            .count();
        if old_count != usize::MAX && (old_seen != old_count || new_seen != new_count) {
            return Err(PatchError::new(format!(
                "hunk count mismatch: expected -{old_count}/+{new_count}, got -{old_seen}/+{new_seen}"
            )));
        }
        hunks.push(PatchHunk {
            old_start,
            old_count,
            new_start,
            new_count,
            lines: hunk_lines,
        });
        if hunks.len() > crate::limits::MAX_PATCH_HUNKS {
            return Err(PatchError::new(format!(
                "patch contains more than {} hunks for one file",
                crate::limits::MAX_PATCH_HUNKS
            )));
        }
    }
    if hunks.is_empty() {
        return Err(PatchError::new("update-file directive must contain a hunk"));
    }
    Ok(hunks)
}

fn parse_hunk_header(header: &str) -> Result<(usize, usize, usize, usize), PatchError> {
    let body = header
        .strip_prefix("@@")
        .and_then(|value| value.split("@@").next())
        .ok_or_else(|| PatchError::new(format!("invalid hunk header: {header}")))?;
    let mut ranges = body.split_whitespace();
    let old = ranges
        .next()
        .ok_or_else(|| PatchError::new(format!("invalid hunk header: {header}")))?;
    let new = ranges
        .next()
        .ok_or_else(|| PatchError::new(format!("invalid hunk header: {header}")))?;
    let parse_range = |value: &str, prefix: char| -> Result<(usize, usize), PatchError> {
        let value = value
            .strip_prefix(prefix)
            .ok_or_else(|| PatchError::new(format!("invalid hunk range: {value}")))?;
        let mut pieces = value.split(',');
        let start = pieces
            .next()
            .and_then(|part| part.parse().ok())
            .ok_or_else(|| PatchError::new(format!("invalid hunk range: {value}")))?;
        let count = pieces
            .next()
            .map(|part| part.parse().ok())
            .unwrap_or(Some(1))
            .ok_or_else(|| PatchError::new(format!("invalid hunk range: {value}")))?;
        Ok((start, count))
    };
    let (old_start, old_count) = parse_range(old, '-')?;
    let (new_start, new_count) = parse_range(new, '+')?;
    Ok((old_start, old_count, new_start, new_count))
}

/// Apply one update hunk to a text snapshot. Matching is exact first and then
/// uses the same whitespace/Unicode normalization policy as `edit`.
pub(crate) fn apply_update(content: &str, file: &FilePatch) -> Result<String, PatchError> {
    if file.operation != PatchOperation::Update {
        return Err(PatchError::new("apply_update accepts update files only"));
    }
    let mut lines = split_lines(content);
    let mut cursor = 0usize;
    for hunk in &file.hunks {
        let pattern = hunk
            .lines
            .iter()
            .filter_map(|line| match line {
                PatchLine::Context(text) | PatchLine::Remove(text) => Some(text.as_str()),
                PatchLine::Add(_) => None,
            })
            .collect::<Vec<_>>();
        let search_start = if pattern.is_empty() {
            hunk.old_start.saturating_sub(1)
        } else {
            cursor
        };
        let found = seek_unique_sequence(&lines, &pattern, search_start)
            .ok_or_else(|| PatchError::new(format!("could not locate hunk in {}", file.path)))?;
        let mut index = found;
        let mut replacement = Vec::new();
        for line in &hunk.lines {
            match line {
                PatchLine::Context(_) => {
                    replacement.push(lines[index].clone());
                    index += 1;
                }
                PatchLine::Remove(_) => {
                    index += 1;
                }
                PatchLine::Add(text) => replacement.push(text.clone()),
            }
        }
        lines.splice(found..index, replacement);
        cursor = found;
    }
    Ok(join_lines(&lines, content.ends_with('\n')))
}

pub(crate) fn apply_file(
    existing: Option<&str>,
    file: &FilePatch,
) -> Result<Option<String>, PatchError> {
    match file.operation {
        PatchOperation::Add => {
            if existing.is_some_and(|content| !content.is_empty()) {
                return Err(PatchError::new(format!(
                    "cannot add {} because it already exists",
                    file.path
                )));
            }
            let lines = file
                .hunks
                .first()
                .map(|hunk| {
                    hunk.lines
                        .iter()
                        .map(|line| match line {
                            PatchLine::Add(text) => text.clone(),
                            _ => String::new(),
                        })
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            Ok(Some(join_lines(&lines, true)))
        }
        PatchOperation::Update => {
            let content = existing.ok_or_else(|| {
                PatchError::new(format!("cannot update missing file {}", file.path))
            })?;
            apply_update(content, file).map(Some)
        }
        PatchOperation::Delete => {
            if existing.is_none() {
                return Err(PatchError::new(format!(
                    "cannot delete missing file {}",
                    file.path
                )));
            }
            Ok(None)
        }
    }
}

fn split_lines(content: &str) -> Vec<String> {
    let mut lines = content.split('\n').map(str::to_owned).collect::<Vec<_>>();
    if content.ends_with('\n') {
        lines.pop();
    }
    lines
}

fn join_lines(lines: &[String], trailing_newline: bool) -> String {
    let mut result = lines.join("\n");
    if trailing_newline && !result.ends_with('\n') {
        result.push('\n');
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_applies_multiple_update_hunks() {
        let patch = parse_patch(
            "*** Begin Patch\n*** Update File: src.txt\n@@\n one\n-two\n+TWO\n@@\n three\n-four\n+FOUR\n*** End Patch\n",
        )
        .unwrap();
        let updated = apply_update("one\ntwo\nthree\nfour\n", &patch.files[0]).unwrap();
        assert_eq!(updated, "one\nTWO\nthree\nFOUR\n");
    }

    #[test]
    fn rejects_ambiguous_fuzzy_hunk() {
        let patch = parse_patch(
            "*** Begin Patch\n*** Update File: src.txt\n@@\n-same  \n+changed\n*** End Patch\n",
        )
        .unwrap();
        let error = apply_update("same\nsame\n", &patch.files[0]).unwrap_err();
        assert!(error.to_string().contains("could not locate"), "{error}");
    }

    #[test]
    fn rejects_trailing_content_and_oversized_input() {
        let trailing =
            parse_patch("*** Begin Patch\n*** Add File: a.txt\n+a\n*** End Patch\nnot-a-comment\n")
                .unwrap_err();
        assert!(trailing.to_string().contains("after `*** End Patch`"));

        let oversized = "x".repeat(crate::limits::MAX_PATCH_INPUT_BYTES + 1);
        let error = parse_patch(&oversized).unwrap_err();
        assert!(error.to_string().contains("safety limit"));
    }
}
