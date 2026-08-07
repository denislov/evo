//! Kitty image protocol id tracking for [`Tui`] frames.

// ── Kitty image helpers ────────────────────────────────────────────────

/// Extract unique Kitty image IDs from a set of lines.
/// Matches Kitty sequences: `\x1b_G` ... `i=<id>` ...
pub(super) fn collect_kitty_image_ids(lines: &[String]) -> Vec<u32> {
    let mut ids = Vec::new();
    for line in lines {
        extract_kitty_image_ids(line, &mut ids);
    }
    ids.sort();
    ids.dedup();
    ids
}

/// Extract Kitty image IDs in a specific line range.
pub(super) fn collect_kitty_image_ids_in_range(
    lines: &[String],
    first: usize,
    last: usize,
) -> Vec<u32> {
    let mut ids = Vec::new();
    for line in lines.iter().take(last + 1).skip(first) {
        extract_kitty_image_ids(line, &mut ids);
    }
    ids.sort();
    ids.dedup();
    ids
}

/// Parse Kitty image IDs from a single line.
pub(super) fn extract_kitty_image_ids(line: &str, ids: &mut Vec<u32>) {
    if !line.contains("\x1b_G") {
        return;
    }
    // Find `i=<number>` parameter in the Kitty sequence header.
    // The header ends at the first `;` or `\x1b\\`.
    let header_start = match line.find("\x1b_G") {
        Some(pos) => pos + 3,
        None => return,
    };
    let header_end = line[header_start..]
        .find([';', '\x1b'])
        .map(|pos| header_start + pos)
        .unwrap_or_else(|| line.len());

    let header = &line[header_start..header_end];
    for param in header.split(',') {
        if let Some(value) = param.strip_prefix("i=")
            && let Ok(id) = value.parse::<u32>()
        {
            ids.push(id);
        }
    }
}
