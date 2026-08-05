pub use agent_core::api::execution::{TruncationLimit, TruncationResult, format_size};

pub const DEFAULT_MAX_LINES: usize = 2000;
pub const DEFAULT_MAX_BYTES: usize = 50 * 1024;

pub const fn default_truncation_limit() -> TruncationLimit {
    TruncationLimit {
        max_lines: DEFAULT_MAX_LINES,
        max_bytes: DEFAULT_MAX_BYTES,
    }
}

fn product_line_count(content: &str) -> usize {
    if content.is_empty() {
        0
    } else {
        content.split_terminator('\n').count()
    }
}

/// Preserve the coding-tool convention that an empty string has zero lines and
/// a trailing newline does not add an empty line. Truncated output itself is
/// shaped by the shared core implementation.
pub fn truncate_head(content: &str, limit: TruncationLimit) -> TruncationResult {
    let total_lines = product_line_count(content);
    if total_lines <= limit.max_lines && content.len() <= limit.max_bytes {
        return TruncationResult {
            content: content.to_string(),
            truncated: false,
            truncated_by: None,
            total_lines,
            total_bytes: content.len(),
            output_lines: total_lines,
            output_bytes: content.len(),
            last_line_partial: false,
            first_line_exceeds_limit: false,
            max_lines: limit.max_lines,
            max_bytes: limit.max_bytes,
        };
    }

    agent_core::api::execution::truncate_head(content, limit)
}
