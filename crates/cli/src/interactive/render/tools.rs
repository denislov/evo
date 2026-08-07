use tui::api::render::{Style, paint_with, wrap_text_with_ansi};

use crate::interactive::transcript::TranscriptDisplayState;

use super::{TranscriptStyles, fit_line, paint_bg_line};

#[allow(
    clippy::too_many_arguments,
    reason = "tool rendering keeps independent disclosure and presentation inputs explicit"
)]
pub(super) fn render_tool_block(
    name: &str,
    args: &serde_json::Value,
    result: Option<&str>,
    is_error: bool,
    width: usize,
    max_tool_result_lines: usize,
    color: bool,
    styles: &TranscriptStyles,
    display_state: TranscriptDisplayState,
    tool_argument_state: TranscriptDisplayState,
    per_block_view: bool,
) -> Vec<String> {
    let status = match (result, is_error) {
        (None, _) => ToolStatus::Running,
        (Some(_), true) => ToolStatus::Error,
        (Some(_), false) => ToolStatus::Done,
    };
    let bg = match status {
        ToolStatus::Running => &styles.tool_pending_bg,
        ToolStatus::Error => &styles.tool_error_bg,
        ToolStatus::Done => &styles.tool_success_bg,
    };

    let header = render_tool_header(name, args, status, color, styles);
    if display_state == TranscriptDisplayState::Collapsed {
        if name == "edit" {
            return vec![fit_line(&header, width)];
        }
        return vec![paint_bg_line(&header, width, bg, color)];
    }
    let result_limit = match display_state {
        TranscriptDisplayState::Collapsed => 0,
        TranscriptDisplayState::Preview => max_tool_result_lines,
        TranscriptDisplayState::Expanded => usize::MAX,
    };

    // `edit` self-renders its diff (TS renderShell: "self") so the diff's
    // added/removed/context colors aren't swallowed by a flat tool bg.
    if name == "edit" {
        return render_edit_block(
            args,
            result,
            is_error,
            width,
            color,
            styles,
            result_limit,
            per_block_view,
        );
    }

    // `web_search` is executed by the provider; its result carries a
    // structured action summary instead of transcript text, so it
    // self-renders search queries / opened pages instead of raw JSON.
    if name == "web_search" {
        return render_web_search_block(result, is_error, width, color, styles, result_limit);
    }

    let mut lines = vec![paint_bg_line(&header, width, bg, color)];
    if per_block_view && tool_argument_state != TranscriptDisplayState::Collapsed {
        for line in render_tool_arguments(args, tool_argument_state, width, color, styles) {
            lines.push(paint_bg_line(&line, width, bg, color));
        }
    }
    if name == "delegation"
        && let Some(task) = string_arg(args, &["task"])
    {
        let task = paint_with(&format!("task: {task}"), &styles.tool_output, color);
        lines.push(paint_bg_line(&format!("  {task}"), width, bg, color));
    }
    let Some(result) = result else {
        // Bash shows a running hint while pending; other tools just stop.
        if name == "bash" {
            let hint = paint_with("Running...", &styles.system, color);
            lines.push(paint_bg_line(&format!("  {hint}"), width, bg, color));
        }
        return lines;
    };

    let body = render_tool_result_body(
        name,
        result,
        is_error,
        result_limit,
        color,
        styles,
        per_block_view,
    );
    for line in body {
        lines.push(paint_bg_line(&line, width, bg, color));
    }
    lines
}

#[derive(Clone, Copy)]
enum ToolStatus {
    Running,
    Done,
    Error,
}

impl ToolStatus {
    fn label(self) -> &'static str {
        match self {
            ToolStatus::Running => "running",
            ToolStatus::Done => "completed",
            ToolStatus::Error => "failed",
        }
    }
    fn style(self, styles: &TranscriptStyles) -> Style {
        match self {
            ToolStatus::Running => styles.warning,
            ToolStatus::Done => styles.tool_diff_added,
            ToolStatus::Error => styles.tool_error_text,
        }
    }
}

/// Render a tool's header line. Built-in tools get friendly, TS-parity
/// headers (`read <path>:range`, `$ <command>`, `edit <path>`); others fall
/// back to the generic `tool <name> <target> <status>` shape.
fn render_tool_header(
    name: &str,
    args: &serde_json::Value,
    status: ToolStatus,
    color: bool,
    styles: &TranscriptStyles,
) -> String {
    let status_text = paint_with(status.label(), &status.style(styles), color);
    match name {
        "read" => {
            let path = tool_target(name, args);
            let range = read_line_range(args, color, styles);
            format!(
                "{} {}{} {}",
                paint_with("read", &styles.tool_title, color),
                path,
                range,
                status_text,
            )
        }
        "bash" => {
            let command = tool_target(name, args);
            format!(
                "{} {}",
                paint_with(&format!("$ {command}"), &styles.bash_mode, color),
                status_text,
            )
        }
        "grep" => format!("{} {}", grep_header(args, color, styles), status_text),
        "find" => format!("{} {}", find_header(args, color, styles), status_text),
        "ls" => {
            let path = string_arg(args, &["path"]).unwrap_or_else(|| ".".to_string());
            format!(
                "{} {} {}",
                paint_with("ls", &styles.tool_title, color),
                path,
                status_text,
            )
        }
        "write" | "edit" => {
            let path = tool_target(name, args);
            format!(
                "{} {} {}",
                paint_with(name, &styles.tool_title, color),
                path,
                status_text,
            )
        }
        "delegation" => {
            let target_kind = string_arg(args, &["targetKind", "target_kind"])
                .unwrap_or_else(|| "agent".to_string());
            let target_id =
                string_arg(args, &["targetId", "target_id"]).unwrap_or_else(|| "-".to_string());
            let live_status =
                string_arg(args, &["status"]).unwrap_or_else(|| status.label().to_string());
            format!(
                "{} {} {} {}",
                paint_with("delegate", &styles.tool_title, color),
                target_kind,
                target_id,
                paint_with(&live_status, &status.style(styles), color),
            )
        }
        _ => format!(
            "{} {} {} {}",
            paint_with("tool", &styles.tool_title, color),
            paint_with(name, &styles.tool_title, color),
            tool_target(name, args),
            status_text,
        ),
    }
}

/// `:<start>-<end>` range suffix for `read`, mirroring TS
/// `formatReadLineRange`, in the warning color.
fn read_line_range(args: &serde_json::Value, color: bool, styles: &TranscriptStyles) -> String {
    let offset = args.get("offset").and_then(|v| v.as_u64());
    let limit = args.get("limit").and_then(|v| v.as_u64());
    if offset.is_none() && limit.is_none() {
        return String::new();
    }
    let start = offset.unwrap_or(1);
    let end = limit.map(|l| start + l - 1);
    let range = match end {
        Some(e) => format!(":{start}-{e}"),
        None => format!(":{start}"),
    };
    paint_with(&range, &styles.warning, color)
}

/// `grep /<pattern>/ in <path> (<glob>) limit <n>` header, mirroring TS
/// `formatGrepCall`. The pattern is accented; path/glob/limit use toolOutput.
fn grep_header(args: &serde_json::Value, color: bool, styles: &TranscriptStyles) -> String {
    let pattern = string_arg(args, &["pattern"]).unwrap_or_default();
    let path = string_arg(args, &["path"]).unwrap_or_else(|| ".".to_string());
    let glob = string_arg(args, &["glob"]);
    let limit = args.get("limit").and_then(|v| v.as_u64());
    let mut text = format!(
        "{} {}",
        paint_with("grep", &styles.tool_title, color),
        paint_with(&format!("/{pattern}/"), &styles.accent, color),
    );
    text.push_str(&paint_with(
        &format!(" in {path}"),
        &styles.tool_output,
        color,
    ));
    if let Some(glob) = glob {
        text.push_str(&paint_with(
            &format!(" ({glob})"),
            &styles.tool_output,
            color,
        ));
    }
    if let Some(limit) = limit {
        text.push_str(&paint_with(
            &format!(" limit {limit}"),
            &styles.tool_output,
            color,
        ));
    }
    text
}

/// `find <pattern> in <path> (limit <n>)` header, mirroring TS
/// `formatFindCall`. The pattern is accented; path/limit use toolOutput.
fn find_header(args: &serde_json::Value, color: bool, styles: &TranscriptStyles) -> String {
    let pattern = string_arg(args, &["pattern"]).unwrap_or_default();
    let path = string_arg(args, &["path"]).unwrap_or_else(|| ".".to_string());
    let limit = args.get("limit").and_then(|v| v.as_u64());
    let mut text = format!(
        "{} {}",
        paint_with("find", &styles.tool_title, color),
        paint_with(&pattern, &styles.accent, color),
    );
    text.push_str(&paint_with(
        &format!(" in {path}"),
        &styles.tool_output,
        color,
    ));
    if let Some(limit) = limit {
        text.push_str(&paint_with(
            &format!(" (limit {limit})"),
            &styles.tool_output,
            color,
        ));
    }
    text
}

/// Render a tool's result body (indented two columns). Built-in tools tailor
/// the preview: `read` replaces tabs and paints output; `bash` shows the
/// *tail* of the output (TS parity) and surfaces truncation notes; others use
/// the generic head-truncated preview.
fn render_tool_result_body(
    name: &str,
    result: &str,
    is_error: bool,
    max_tool_result_lines: usize,
    color: bool,
    styles: &TranscriptStyles,
    per_block_view: bool,
) -> Vec<String> {
    let output_style = if is_error {
        styles.tool_error_text
    } else {
        styles.tool_output
    };
    let all_lines: Vec<&str> = result.lines().collect();

    let keep_all = max_tool_result_lines == usize::MAX;
    let limit = max_tool_result_lines.min(all_lines.len());

    let (shown, omitted) = if name == "bash" && !keep_all {
        // Tail preview: show the last `limit` logical lines.
        let start = all_lines.len().saturating_sub(limit);
        (all_lines[start..].to_vec(), start)
    } else {
        (
            all_lines[..limit.min(all_lines.len())].to_vec(),
            all_lines.len().saturating_sub(limit),
        )
    };

    let mut out = Vec::new();
    for line in &shown {
        let text = if name == "read" {
            replace_tabs(line)
        } else {
            (*line).to_string()
        };
        let painted = paint_with(&text, &output_style, color);
        out.push(format!("  {painted}"));
    }
    if omitted > 0 {
        let note = paint_with(
            &format!(
                "... {omitted} more lines ({})",
                if per_block_view {
                    "disclose block"
                } else {
                    "expand tools"
                }
            ),
            &styles.system,
            color,
        );
        out.push(format!("  {note}"));
    }
    out
}

fn render_tool_arguments(
    args: &serde_json::Value,
    display_state: TranscriptDisplayState,
    width: usize,
    color: bool,
    styles: &TranscriptStyles,
) -> Vec<String> {
    let serialized = serde_json::to_string_pretty(args).unwrap_or_else(|_| args.to_string());
    let source_lines = serialized.lines().collect::<Vec<_>>();
    let limit = match display_state {
        TranscriptDisplayState::Collapsed => 0,
        TranscriptDisplayState::Preview => 3,
        TranscriptDisplayState::Expanded => usize::MAX,
    };
    let shown = source_lines.len().min(limit);
    let mut lines = vec![format!(
        "  {}",
        paint_with(
            match display_state {
                TranscriptDisplayState::Collapsed => "arguments · hidden",
                TranscriptDisplayState::Preview => "arguments · preview",
                TranscriptDisplayState::Expanded => "arguments · expanded",
            },
            &styles.system,
            color,
        )
    )];
    let content_width = width.saturating_sub(4).max(1);
    for source in source_lines.iter().take(shown) {
        let painted = paint_with(source, &styles.tool_output, color);
        for wrapped in wrap_text_with_ansi(&painted, content_width) {
            lines.push(format!("    {wrapped}"));
        }
    }
    let omitted = source_lines.len().saturating_sub(shown);
    if omitted > 0 {
        lines.push(format!(
            "    {}",
            paint_with(
                &format!("... {omitted} more argument lines"),
                &styles.system,
                color,
            )
        ));
    }
    lines
}

/// Self-rendered `edit` block: no tool bg, diff lines colored by
/// added/removed/context, mirroring TS `renderShell: "self"`.
#[allow(
    clippy::too_many_arguments,
    reason = "edit rendering keeps independent diff and disclosure controls explicit"
)]
fn render_edit_block(
    args: &serde_json::Value,
    result: Option<&str>,
    is_error: bool,
    width: usize,
    color: bool,
    styles: &TranscriptStyles,
    max_result_lines: usize,
    per_block_view: bool,
) -> Vec<String> {
    let path = tool_target("edit", args);
    let status = match (result, is_error) {
        (None, _) => ToolStatus::Running,
        (Some(_), true) => ToolStatus::Error,
        (Some(_), false) => ToolStatus::Done,
    };
    let header = format!(
        "{} {} {}",
        paint_with("edit", &styles.tool_title, color),
        path,
        paint_with(status.label(), &status.style(styles), color),
    );
    let mut lines = vec![fit_line(&header, width)];
    let Some(result) = result else {
        return lines;
    };

    let output_style = if is_error {
        styles.tool_error_text
    } else {
        styles.tool_output
    };
    let result_lines = result.lines().collect::<Vec<_>>();
    for line in result_lines.iter().take(max_result_lines) {
        let styled = paint_diff_line(line, color, styles, output_style);
        lines.push(fit_line(&format!("  {styled}"), width));
    }
    let omitted = result_lines.len().saturating_sub(max_result_lines);
    if omitted > 0 {
        lines.push(fit_line(
            &format!(
                "  {}",
                paint_with(
                    &format!(
                        "... {omitted} more diff lines ({})",
                        if per_block_view {
                            "disclose block"
                        } else {
                            "expand tools"
                        }
                    ),
                    &styles.system,
                    color
                )
            ),
            width,
        ));
    }
    lines
}

/// Self-rendered `web_search` block. `web_search` runs inside the provider,
/// so its result carries a structured action summary
/// (`{"status": ..., "action": {"type": "search", "queries": [...]}}` or an
/// `open_page` URL) rather than transcript text. Search queries and opened
/// pages render as readable lines instead of raw JSON; legacy items without
/// an action fall back to the generic result body.
fn render_web_search_block(
    result: Option<&str>,
    is_error: bool,
    width: usize,
    color: bool,
    styles: &TranscriptStyles,
    max_result_lines: usize,
) -> Vec<String> {
    let status = match (result, is_error) {
        (None, _) => ToolStatus::Running,
        (Some(_), true) => ToolStatus::Error,
        (Some(_), false) => ToolStatus::Done,
    };
    let action = if is_error {
        None
    } else {
        result.and_then(parse_web_search_action)
    };
    let verb = match &action {
        Some(WebSearchAction::OpenPage { .. }) => "open page",
        _ => "search",
    };
    let mut lines = vec![fit_line(
        &format!(
            "{} {}",
            paint_with(verb, &styles.tool_title, color),
            paint_with(status.label(), &status.style(styles), color),
        ),
        width,
    )];
    let Some(action) = action else {
        // Running, errored, or legacy: surface the raw summary text so the
        // status/action payload stays visible instead of vanishing.
        if let Some(result) = result {
            lines.extend(render_tool_result_body(
                "web_search",
                result,
                is_error,
                max_result_lines,
                color,
                styles,
                true,
            ));
        }
        return lines;
    };
    match action {
        WebSearchAction::Search { queries } => {
            for query in queries.iter().take(max_result_lines) {
                let painted = paint_with(query, &styles.tool_output, color);
                for wrapped in wrap_text_with_ansi(&painted, width.saturating_sub(2).max(1)) {
                    lines.push(fit_line(&format!("  {wrapped}"), width));
                }
            }
            let omitted = queries.len().saturating_sub(max_result_lines);
            if omitted > 0 {
                lines.push(fit_line(
                    &format!(
                        "  {}",
                        paint_with(
                            &format!("... {omitted} more queries (disclose block)"),
                            &styles.system,
                            color,
                        )
                    ),
                    width,
                ));
            }
        }
        WebSearchAction::OpenPage { url } => {
            let painted = paint_with(&url, &styles.tool_output, color);
            for wrapped in wrap_text_with_ansi(&painted, width.saturating_sub(2).max(1)) {
                lines.push(fit_line(&format!("  {wrapped}"), width));
            }
        }
    }
    lines
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum WebSearchAction {
    Search { queries: Vec<String> },
    OpenPage { url: String },
}

/// Parse the provider web-search action summary, stripping DeepSeek's
/// `ws_call_id=` markers from queries and opened-page URLs (mirrors the
/// desktop `web_search_summary`).
fn parse_web_search_action(result: &str) -> Option<WebSearchAction> {
    let value: serde_json::Value = serde_json::from_str(result).ok()?;
    let action = value.get("action")?;
    match action.get("type").and_then(serde_json::Value::as_str) {
        Some("search") => {
            let queries = action
                .get("queries")
                .and_then(serde_json::Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(serde_json::Value::as_str)
                .filter(|query| !query.starts_with("ws_call_id="))
                .map(ToString::to_string)
                .collect::<Vec<_>>();
            Some(WebSearchAction::Search { queries })
        }
        Some("open_page") => {
            let url = action
                .get("url")
                .and_then(serde_json::Value::as_str)?
                .split('#')
                .next()
                .unwrap_or_default()
                .to_string();
            Some(WebSearchAction::OpenPage { url })
        }
        _ => None,
    }
}

/// Paint a single diff line with semantic colors: `+` added, `-` removed,
/// ` ` context, and hunk headers (`@@`/`---`/`+++`) dimmed.
fn paint_diff_line(line: &str, color: bool, styles: &TranscriptStyles, fallback: Style) -> String {
    let (prefix, style) = if line.starts_with("+++") || line.starts_with("---") {
        (line, styles.tool_diff_context)
    } else if let Some(rest) = line.strip_prefix('+') {
        (rest, styles.tool_diff_added)
    } else if let Some(rest) = line.strip_prefix('-') {
        (rest, styles.tool_diff_removed)
    } else if line.starts_with("@@") {
        (line, styles.tool_diff_context)
    } else if let Some(rest) = line.strip_prefix(' ') {
        (rest, styles.tool_diff_context)
    } else {
        (line, fallback)
    };
    // Preserve the leading marker (stripped above) so the diff is still
    // readable on colorless terminals.
    let marker = if line.starts_with('+') {
        "+"
    } else if line.starts_with('-') {
        "-"
    } else if line.starts_with(' ') {
        " "
    } else {
        ""
    };
    format!("{}{}", marker, paint_with(prefix, &style, color))
}

/// Replace tabs with three spaces, mirroring TS `replaceTabs`.
fn replace_tabs(text: &str) -> String {
    text.replace('\t', "   ")
}

fn tool_target(name: &str, args: &serde_json::Value) -> String {
    match name {
        "bash" => string_arg(args, &["command", "cmd"]).unwrap_or_else(|| "-".to_string()),
        "read" | "write" | "edit" => {
            string_arg(args, &["path", "file_path", "filePath"]).unwrap_or_else(|| "-".to_string())
        }
        _ => string_arg(
            args,
            &["path", "file_path", "filePath", "command", "pattern"],
        )
        .unwrap_or_else(|| "-".to_string()),
    }
}

fn string_arg(args: &serde_json::Value, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| {
        args.get(*key)
            .and_then(|value| value.as_str())
            .filter(|value| !value.trim().is_empty())
            .map(ToString::to_string)
    })
}
