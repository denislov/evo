pub const DEFAULT_MAX_LINES: usize = 2000;
pub const DEFAULT_MAX_BYTES: usize = 50 * 1024;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TruncationLimit {
    pub max_lines: usize,
    pub max_bytes: usize,
}

impl Default for TruncationLimit {
    fn default() -> Self {
        Self {
            max_lines: DEFAULT_MAX_LINES,
            max_bytes: DEFAULT_MAX_BYTES,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TruncationResult {
    pub content: String,
    pub truncated: bool,
    pub truncated_by: Option<String>,
    pub total_lines: usize,
    pub total_bytes: usize,
    pub output_lines: usize,
    pub output_bytes: usize,
    pub last_line_partial: bool,
    pub first_line_exceeds_limit: bool,
    pub max_lines: usize,
    pub max_bytes: usize,
}

pub fn format_size(bytes: usize) -> String {
    if bytes < 1024 {
        format!("{bytes}B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1}KB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1}MB", bytes as f64 / (1024.0 * 1024.0))
    }
}

pub fn truncate_head(content: &str, limit: TruncationLimit) -> TruncationResult {
    let total_bytes = content.len();
    let lines: Vec<&str> = content.split('\n').collect();
    let total_lines = lines.len();

    if total_lines <= limit.max_lines && total_bytes <= limit.max_bytes {
        return no_truncation(content, total_lines, total_bytes, limit);
    }

    let first_line_bytes = lines.first().map(|line| line.len()).unwrap_or_default();
    if first_line_bytes > limit.max_bytes {
        return TruncationResult {
            content: String::new(),
            truncated: true,
            truncated_by: Some("bytes".into()),
            total_lines,
            total_bytes,
            output_lines: 0,
            output_bytes: 0,
            last_line_partial: false,
            first_line_exceeds_limit: true,
            max_lines: limit.max_lines,
            max_bytes: limit.max_bytes,
        };
    }

    let mut output = Vec::new();
    let mut output_bytes = 0usize;
    let mut truncated_by = "lines";
    for (idx, line) in lines.iter().enumerate() {
        if idx >= limit.max_lines {
            truncated_by = "lines";
            break;
        }
        let line_bytes = line.len() + usize::from(!output.is_empty());
        if output_bytes + line_bytes > limit.max_bytes {
            truncated_by = "bytes";
            break;
        }
        output.push(*line);
        output_bytes += line_bytes;
    }

    let content = output.join("\n");
    let output_bytes = content.len();
    TruncationResult {
        content,
        truncated: true,
        truncated_by: Some(truncated_by.into()),
        total_lines,
        total_bytes,
        output_lines: output.len(),
        output_bytes,
        last_line_partial: false,
        first_line_exceeds_limit: false,
        max_lines: limit.max_lines,
        max_bytes: limit.max_bytes,
    }
}

pub fn truncate_tail(content: &str, limit: TruncationLimit) -> TruncationResult {
    let total_bytes = content.len();
    let mut lines: Vec<&str> = content.split('\n').collect();
    if lines.len() > 1 && lines.last() == Some(&"") {
        lines.pop();
    }
    let total_lines = lines.len();

    if total_lines <= limit.max_lines && total_bytes <= limit.max_bytes {
        return no_truncation(content, total_lines, total_bytes, limit);
    }

    let mut output = Vec::new();
    let mut output_bytes = 0usize;
    let mut truncated_by = "lines";
    let mut last_line_partial = false;

    for line in lines.iter().rev() {
        if output.len() >= limit.max_lines {
            truncated_by = "lines";
            break;
        }
        let line_bytes = line.len() + usize::from(!output.is_empty());
        if output_bytes + line_bytes > limit.max_bytes {
            truncated_by = "bytes";
            if output.is_empty() {
                let tail = truncate_str_from_end(line, limit.max_bytes);
                output.push(tail);
                last_line_partial = true;
            }
            break;
        }
        output.push((*line).to_string());
        output_bytes += line_bytes;
    }

    output.reverse();
    let content = output.join("\n");
    let output_bytes = content.len();
    TruncationResult {
        content,
        truncated: true,
        truncated_by: Some(truncated_by.into()),
        total_lines,
        total_bytes,
        output_lines: output.len(),
        output_bytes,
        last_line_partial,
        first_line_exceeds_limit: false,
        max_lines: limit.max_lines,
        max_bytes: limit.max_bytes,
    }
}

pub fn truncate_line(line: &str, max_chars: usize) -> (String, bool) {
    if line.chars().count() <= max_chars {
        return (line.to_string(), false);
    }
    let mut output = line.chars().take(max_chars).collect::<String>();
    output.push_str("... [truncated]");
    (output, true)
}

fn no_truncation(
    content: &str,
    total_lines: usize,
    total_bytes: usize,
    limit: TruncationLimit,
) -> TruncationResult {
    TruncationResult {
        content: content.to_string(),
        truncated: false,
        truncated_by: None,
        total_lines,
        total_bytes,
        output_lines: total_lines,
        output_bytes: total_bytes,
        last_line_partial: false,
        first_line_exceeds_limit: false,
        max_lines: limit.max_lines,
        max_bytes: limit.max_bytes,
    }
}

fn truncate_str_from_end(text: &str, max_bytes: usize) -> String {
    if max_bytes == 0 {
        return String::new();
    }
    let mut bytes = 0usize;
    let mut chars = Vec::new();
    for ch in text.chars().rev() {
        let len = ch.len_utf8();
        if bytes + len > max_bytes {
            break;
        }
        bytes += len;
        chars.push(ch);
    }
    chars.into_iter().rev().collect()
}
