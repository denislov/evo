pub use agent_core::api::execution::{
    TruncationLimit, TruncationResult, format_size, truncate_tail,
};

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_truncation_preserves_product_line_counting() {
        let empty = truncate_head("", default_truncation_limit());
        assert_eq!(empty.total_lines, 0);

        let trailing_newline = truncate_head("a\nb\n", default_truncation_limit());
        assert!(!trailing_newline.truncated);
        assert_eq!(trailing_newline.content, "a\nb\n");
        assert_eq!(trailing_newline.total_lines, 2);
    }

    #[test]
    fn head_limits_delegate_to_core_contract() {
        let content = (0..10)
            .map(|i| i.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        let result = truncate_head(
            &content,
            TruncationLimit {
                max_lines: 3,
                max_bytes: DEFAULT_MAX_BYTES,
            },
        );
        assert!(result.truncated);
        assert_eq!(result.truncated_by.as_deref(), Some("lines"));
        assert_eq!(result.output_lines, 3);
        assert_eq!(result.content, "0\n1\n2");
    }

    #[test]
    fn tail_partial_last_line_is_unicode_safe() {
        let result = truncate_tail(
            "héllo-world",
            TruncationLimit {
                max_lines: DEFAULT_MAX_LINES,
                max_bytes: 5,
            },
        );
        assert!(result.last_line_partial);
        assert!(result.content.len() <= 5);
        assert!(std::str::from_utf8(result.content.as_bytes()).is_ok());
    }

    #[test]
    fn size_format_uses_core_contract() {
        assert_eq!(format_size(512), "512B");
        assert_eq!(format_size(51200), "50.0KB");
    }
}
