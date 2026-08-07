use super::*;

pub(super) fn conversation_copy_button(
    id: impl Into<ElementId>,
    block_id: String,
    hover_group: SharedString,
    selected: bool,
    cx: &gpui::Context<ConversationPane>,
) -> Button {
    conversation_hover_tool(
        DesktopIconButton::new(id, DesktopIcon::Copy, "Copy this bounded message")
            .build()
            .debug_selector(|| "desktop-copy-conversation-row".into())
            .on_click(cx.listener(move |_, _, _, cx| {
                cx.stop_propagation();
                cx.emit(ConversationPaneEvent::Copy {
                    block_id: block_id.clone(),
                });
            })),
        hover_group,
        selected,
    )
}

pub(super) fn conversation_hover_tool(
    button: Button,
    hover_group: SharedString,
    selected: bool,
) -> Button {
    // Keep the button paint-visible with zero opacity instead of using
    // `visibility: hidden`: GPUI registers tab stops during paint after its
    // hidden-visibility early return, so an invisible button could never
    // receive keyboard focus to reveal itself.
    button
        .opacity(0.)
        .group_hover(hover_group, |style| style.opacity(1.))
        .focus(|style| style.opacity(1.))
        .when(selected, |button| button.opacity(1.))
}

pub(super) fn conversation_recovery_button(
    id: impl Into<ElementId>,
    label: &'static str,
    tooltip: &'static str,
    identity: DesktopRecoveryIdentity,
    action: DesktopRecoveryAction,
    cx: &gpui::Context<ConversationPane>,
) -> Button {
    let tone = match action {
        DesktopRecoveryAction::Retry => DesktopCriticalTone::Neutral,
        DesktopRecoveryAction::MarkFailed | DesktopRecoveryAction::Abort => {
            DesktopCriticalTone::Dangerous
        }
    };
    DesktopCriticalButton::new(id, label, tooltip, tone)
        .build()
        .on_click(cx.listener(move |_, _, _, cx| {
            cx.stop_propagation();
            cx.emit(ConversationPaneEvent::Recovery {
                identity: identity.clone(),
                action,
            });
        }))
}

pub(super) fn user_message_width(text: &str) -> f32 {
    let maximum = USER_MESSAGE_MAX_WIDTH as f32;
    let maximum_content = maximum - USER_MESSAGE_HORIZONTAL_CHROME;
    let maximum_columns = (maximum_content / USER_MESSAGE_COLUMN_WIDTH).ceil() as usize;
    let mut line_columns = 0usize;
    let mut widest_line = 0usize;

    for character in text.chars() {
        if character == '\n' {
            widest_line = widest_line.max(line_columns);
            line_columns = 0;
            continue;
        }
        let character_columns = if character == '\t' {
            4
        } else {
            character.width().unwrap_or_default()
        };
        line_columns = line_columns.saturating_add(character_columns);
        if line_columns >= maximum_columns {
            return maximum;
        }
    }
    widest_line = widest_line.max(line_columns);

    (widest_line as f32 * USER_MESSAGE_COLUMN_WIDTH + USER_MESSAGE_HORIZONTAL_CHROME)
        .clamp(USER_MESSAGE_MIN_WIDTH, maximum)
}

/// Whether the row is an interior part of an assistant turn: tool calls and
/// delegations neither start a new identity segment nor carry the turn's
/// trailing copy affordance, so adjacent assistant rows must merge across
/// them exactly like they merge across plain tool calls.
pub(super) fn is_tool_group(kind: ConversationBlockKind) -> bool {
    matches!(
        kind,
        ConversationBlockKind::Tool | ConversationBlockKind::Delegation
    )
}

pub(super) fn conversation_identity_header_visible(
    kind: ConversationBlockKind,
    previous_kind: Option<ConversationBlockKind>,
) -> bool {
    kind != ConversationBlockKind::User
        && !(kind == ConversationBlockKind::Assistant && previous_kind.is_some_and(is_tool_group))
}

pub(super) fn conversation_copy_footer_visible(
    kind: ConversationBlockKind,
    next_kind: Option<ConversationBlockKind>,
) -> bool {
    !(is_tool_group(kind)
        || kind == ConversationBlockKind::Assistant && next_kind.is_some_and(is_tool_group))
}

/// Collapsed-header summary for a delegation: the first line of the task.
pub(super) fn delegation_task_summary(text: &str) -> String {
    text.lines().next().unwrap_or_default().to_owned()
}

pub(super) fn tool_name_from_title(title: &str) -> &str {
    title
        .strip_prefix("Tool · ")
        .and_then(|title| title.split(" · ").next())
        .unwrap_or(title)
}

pub(super) fn tool_display_label(name: &str) -> &'static str {
    match name {
        "bash" | "shell" => "Shell",
        "edit" => "Edit",
        "write" => "Write",
        "read" => "Read",
        "ls" => "Files",
        "find" => "Find",
        "grep" => "Search",
        "web_search" => "Web search",
        _ => "Tool",
    }
}

pub(super) fn tool_is_expandable(name: &str) -> bool {
    !matches!(name, "read")
}

pub(super) fn tool_disclosure_icon(expanded: bool) -> DesktopIcon {
    if expanded {
        DesktopIcon::ChevronDown
    } else {
        DesktopIcon::ChevronRight
    }
}

pub(super) fn tool_arguments_json(detail: &str, text: &str) -> Option<serde_json::Value> {
    [detail, text]
        .into_iter()
        .find_map(|s| serde_json::from_str(s).ok())
}

pub(super) fn edit_replacements(args: &serde_json::Value) -> Vec<(&str, &str)> {
    let mut replacements = args
        .get("edits")
        .and_then(|value| value.as_array())
        .into_iter()
        .flatten()
        .filter_map(|edit| {
            Some((
                edit.get("oldText")?.as_str()?,
                edit.get("newText")?.as_str()?,
            ))
        })
        .collect::<Vec<_>>();
    if let (Some(old), Some(new)) = (
        args.get("oldText").and_then(|value| value.as_str()),
        args.get("newText").and_then(|value| value.as_str()),
    ) {
        replacements.push((old, new));
    }
    replacements
}

pub(super) fn tool_summary(name: &str, detail: &str, text: &str) -> String {
    let Some(args) = tool_arguments_json(detail, text) else {
        return String::new();
    };
    match name {
        "bash" | "shell" => args
            .get("command")
            .and_then(|v| v.as_str())
            .map(str::to_owned)
            .unwrap_or_default(),
        "read" => {
            let path = args.get("path").and_then(|v| v.as_str()).unwrap_or("");
            let offset = args.get("offset").and_then(|v| v.as_u64());
            let limit = args.get("limit").and_then(|v| v.as_u64());
            match (offset, limit) {
                (Some(o), Some(l)) => format!("{path} [{o},{l}]"),
                (Some(o), None) => format!("{path} [{o},]"),
                (None, Some(l)) => format!("{path} [,{l}]"),
                (None, None) => path.to_owned(),
            }
        }
        "edit" => {
            let path = args.get("path").and_then(|v| v.as_str()).unwrap_or("");
            let (added, removed) = edit_replacements(&args).into_iter().fold(
                (0usize, 0usize),
                |(added, removed), (old, new)| {
                    (added + new.lines().count(), removed + old.lines().count())
                },
            );
            format!("{path} +{added} -{removed}")
        }
        "write" => {
            let path = args.get("path").and_then(|v| v.as_str()).unwrap_or("");
            let added = args
                .get("content")
                .and_then(|v| v.as_str())
                .map_or(0, |content| content.lines().count());
            format!("{path} +{added}")
        }
        "ls" => {
            let path = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");
            let entries = count_entries(text);
            if entries == 0 {
                format!("{path} · empty")
            } else {
                format!("{path} · {}", pluralized(entries, "entry", "entries"))
            }
        }
        "find" => {
            let pattern = args
                .get("pattern")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            let matches = count_entries(text);
            if matches == 0 {
                format!("{pattern} · no matches")
            } else {
                format!("{pattern} · {}", pluralized(matches, "match", "matches"))
            }
        }
        "grep" => {
            let pattern = args
                .get("pattern")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            let matches = count_grep_matches(text);
            if matches == 0 {
                format!("{pattern} · no matches")
            } else {
                format!("{pattern} · {}", pluralized(matches, "match", "matches"))
            }
        }
        "web_search" => web_search_summary(text),
        _ => String::new(),
    }
}

/// Summary line for a completed provider web-search item. The `summary`
/// carries the terminal item JSON (`{"status": ..., "action": {...}}`) so the
/// action type, search queries and opened-page URL survive into the
/// transcript; legacy items fall back to an empty summary.
pub(super) fn web_search_summary(summary: &str) -> String {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(summary) else {
        return String::new();
    };
    let Some(action) = value.get("action") else {
        return String::new();
    };
    match action.get("type").and_then(serde_json::Value::as_str) {
        Some("search") => {
            let queries = action
                .get("queries")
                .and_then(serde_json::Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(serde_json::Value::as_str)
                .filter(|query| !query.starts_with("ws_call_id="))
                .collect::<Vec<_>>();
            match queries.len() {
                0 => "搜索完成".into(),
                1 => format!("搜索：{}", queries[0]),
                n => format!("搜索 {} 个查询：{}", n, queries[0]),
            }
        }
        Some("open_page") => action
            .get("url")
            .and_then(serde_json::Value::as_str)
            .map(|url| format!("打开页面：{}", strip_ws_call_id(url)))
            .unwrap_or_default(),
        _ => String::new(),
    }
}

/// Removes the `#ws_call_id=...` marker DeepSeek appends to opened-page URLs.
pub(super) fn strip_ws_call_id(url: &str) -> &str {
    url.split('#').next().unwrap_or(url)
}

/// Non-empty content lines of an ls/find/grep result, excluding the trailing
/// `[notice]` block those tools append after a blank line. Content lines that
/// merely start with '[' (e.g. a grep match of `[foo]`) are kept.
pub(super) fn tool_result_lines(text: &str) -> Vec<&str> {
    let mut lines = Vec::new();
    let mut in_notice = false;
    for line in text.lines().map(str::trim_end) {
        if line.is_empty() {
            in_notice = true;
        } else if !in_notice {
            lines.push(line);
        }
    }
    lines
}

/// The empty-state messages ls/find/grep emit when nothing matched.
pub(super) fn is_empty_state_line(line: &str) -> bool {
    matches!(
        line,
        "(empty directory)" | "No files found matching pattern" | "No matches found"
    )
}

/// Entry count of an ls/find result, treating empty-state messages as zero
/// entries.
pub(super) fn count_entries(text: &str) -> usize {
    let lines = tool_result_lines(text);
    if lines
        .first()
        .is_some_and(|first| is_empty_state_line(first))
    {
        0
    } else {
        lines.len()
    }
}

/// Number of `path:line: content` match lines in a grep result, so context
/// lines around a match do not inflate the count.
pub(super) fn count_grep_matches(text: &str) -> usize {
    tool_result_lines(text)
        .iter()
        .filter(|line| parse_grep_match(line).is_some())
        .count()
}

pub(super) fn pluralized(count: usize, singular: &str, plural: &str) -> String {
    if count == 1 {
        format!("1 {singular}")
    } else {
        format!("{count} {plural}")
    }
}

/// Split a grep match line `path:line: content`. The path and content may
/// themselves contain `: `, `:` or digits, so the split anchors on the *last*
/// `: <digits>: ` segment — the emitters format every match that way.
pub(super) fn parse_grep_match(line: &str) -> Option<(&str, &str, &str)> {
    let mut anchor: Option<(usize, usize)> = None; // (colon index, digit count)
    for (index, _) in line.match_indices(':') {
        let after = &line[index + 1..];
        let digits = after
            .bytes()
            .take_while(|byte| byte.is_ascii_digit())
            .count();
        if digits == 0 {
            continue;
        }
        if after.as_bytes().get(digits) == Some(&b':')
            && after.as_bytes().get(digits + 1) == Some(&b' ')
        {
            anchor = Some((index, digits));
        }
    }
    let (colon, digits) = anchor?;
    let after = &line[colon + 1..];
    let (line_no, content) = after.split_at(digits);
    Some((&line[..colon], line_no, &content[2..]))
}

/// Split a grep context line `path-line- content` shown around a match.
pub(super) fn parse_grep_context(line: &str) -> Option<(&str, &str, &str)> {
    let mut anchor: Option<(usize, usize)> = None; // (dash index, digit count)
    for (index, _) in line.match_indices('-') {
        let after = &line[index + 1..];
        let digits = after
            .bytes()
            .take_while(|byte| byte.is_ascii_digit())
            .count();
        if digits == 0 {
            continue;
        }
        if after.as_bytes().get(digits) == Some(&b'-')
            && after.as_bytes().get(digits + 1) == Some(&b' ')
        {
            anchor = Some((index, digits));
        }
    }
    let (dash, digits) = anchor?;
    let after = &line[dash + 1..];
    let (line_no, content) = after.split_at(digits);
    Some((&line[..dash], line_no, &content[2..]))
}

/// Directory listings (`ls`, `find`) paint directory entries with the accent
/// color, keep files neutral and dim the notice and empty-state lines.
pub(super) fn ls_find_view(text: &str, theme: &SemanticTheme) -> gpui::AnyElement {
    let mut container = div().flex().flex_col();
    for line in tool_result_lines(text) {
        let directory = line.ends_with('/');
        let muted = is_empty_state_line(line);
        container = container.child(
            div()
                .text_color(rgb(if directory {
                    theme.accent.value()
                } else if muted {
                    theme.subtle_text.value()
                } else {
                    theme.text.value()
                }))
                .child(SharedString::new(line)),
        );
    }
    container.into_any_element()
}

/// Grep results keep the path neutral, highlight the line number on match
/// lines and dim context, notice and empty-state lines.
pub(super) fn grep_view(text: &str, theme: &SemanticTheme) -> gpui::AnyElement {
    let mut container = div().flex().flex_col();
    for line in text.lines() {
        let line = line.trim_end();
        if line.is_empty() {
            continue;
        }
        if let Some((path, line_no, content)) = parse_grep_match(line) {
            container = container.child(
                div()
                    .flex()
                    .child(
                        div()
                            .text_color(rgb(theme.subtle_text.value()))
                            .child(SharedString::new(format!("{path}:"))),
                    )
                    .child(
                        div()
                            .text_color(rgb(theme.accent.value()))
                            .child(SharedString::new(format!("{line_no}: "))),
                    )
                    .child(
                        div()
                            .text_color(rgb(theme.text.value()))
                            .child(SharedString::new(content)),
                    ),
            );
        } else {
            let muted = line.starts_with('[')
                || is_empty_state_line(line)
                || parse_grep_context(line).is_some();
            container = container.child(
                div()
                    .text_color(rgb(if muted {
                        theme.subtle_text.value()
                    } else {
                        theme.text.value()
                    }))
                    .child(SharedString::new(line)),
            );
        }
    }
    container.into_any_element()
}

/// Renders a completed provider web-search item: one line per search query,
/// or the opened-page URL. The terminal `summary` carries the item JSON
/// (`{"status": ..., "action": {...}}`); legacy items render their raw text.
pub(super) fn web_search_view(summary: &str, theme: &SemanticTheme) -> gpui::AnyElement {
    let mut container = div().flex().flex_col();
    let Ok(value) = serde_json::from_str::<serde_json::Value>(summary) else {
        container = container.child(
            div()
                .text_color(rgb(theme.text.value()))
                .child(SharedString::new(summary)),
        );
        return container.into_any_element();
    };
    let Some(action) = value.get("action") else {
        container = container.child(
            div()
                .text_color(rgb(theme.text.value()))
                .child(SharedString::new(summary)),
        );
        return container.into_any_element();
    };
    match action.get("type").and_then(serde_json::Value::as_str) {
        Some("search") => {
            let queries = action
                .get("queries")
                .and_then(serde_json::Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(serde_json::Value::as_str)
                .filter(|query| !query.starts_with("ws_call_id="))
                .collect::<Vec<_>>();
            if queries.is_empty() {
                container = container.child(
                    div()
                        .text_color(rgb(theme.subtle_text.value()))
                        .child(SharedString::new("搜索完成，无查询记录")),
                );
            } else {
                for query in queries {
                    container = container.child(
                        div()
                            .text_color(rgb(theme.text.value()))
                            .child(SharedString::new(format!("• {query}"))),
                    );
                }
            }
        }
        Some("open_page") => {
            if let Some(url) = action
                .get("url")
                .and_then(serde_json::Value::as_str)
                .map(strip_ws_call_id)
            {
                container = container.child(
                    div()
                        .text_color(rgb(theme.accent.value()))
                        .child(SharedString::new(url)),
                );
            }
        }
        _ => {
            container = container.child(
                div()
                    .text_color(rgb(theme.text.value()))
                    .child(SharedString::new(summary)),
            );
        }
    }
    container.into_any_element()
}

pub(super) fn edit_diff_view(detail: &str, text: &str, theme: &SemanticTheme) -> gpui::AnyElement {
    let args = tool_arguments_json(detail, text);
    let mut container = div().flex().flex_col();
    if let Some(args) = args.as_ref() {
        for (old, new) in edit_replacements(args) {
            for line in old.lines() {
                container = container.child(
                    div()
                        .text_color(rgb(theme.danger.value()))
                        .child(SharedString::new(format!("- {line}"))),
                );
            }
            for line in new.lines() {
                container = container.child(
                    div()
                        .text_color(rgb(theme.accent.value()))
                        .child(SharedString::new(format!("+ {line}"))),
                );
            }
        }
    }
    container.into_any_element()
}

pub(super) fn write_diff_view(detail: &str, text: &str, theme: &SemanticTheme) -> gpui::AnyElement {
    let args = tool_arguments_json(detail, text);
    let mut container = div().flex().flex_col();
    if let Some(args) = args.as_ref()
        && let Some(content) = args.get("content").and_then(|v| v.as_str())
    {
        for line in content.lines() {
            container = container.child(
                div()
                    .text_color(rgb(theme.accent.value()))
                    .child(SharedString::new(format!("+ {line}"))),
            );
        }
    }
    container.into_any_element()
}

pub(super) fn write_diff_text(detail: &str, text: &str) -> String {
    let Some(args) = tool_arguments_json(detail, text) else {
        return String::new();
    };
    args.get("content")
        .and_then(|v| v.as_str())
        .map(|content| {
            content
                .lines()
                .map(|line| format!("+ {line}"))
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default()
}

pub(super) fn edit_diff_text(detail: &str, text: &str) -> String {
    let Some(args) = tool_arguments_json(detail, text) else {
        return String::new();
    };
    edit_replacements(&args)
        .into_iter()
        .flat_map(|(old, new)| {
            old.lines()
                .map(|line| format!("- {line}"))
                .chain(new.lines().map(|line| format!("+ {line}")))
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub(super) fn structured_tool_command(detail: &str, text: &str) -> Option<String> {
    [detail, text].into_iter().find_map(|arguments| {
        serde_json::from_str::<serde_json::Value>(arguments)
            .ok()?
            .get("command")?
            .as_str()
            .map(str::to_owned)
    })
}

pub(super) fn tool_detail_copy_text(title: &str, detail: &str, text: &str) -> String {
    match tool_name_from_title(title) {
        "bash" | "shell" => {
            let command = structured_tool_command(detail, text).unwrap_or_default();
            conversation_copy_text(&format!("$ {command}"), text)
        }
        "edit" => conversation_copy_text(&edit_diff_text(detail, text), ""),
        "write" => {
            let diff = write_diff_text(detail, text);
            if diff.is_empty() {
                conversation_copy_text(text, "")
            } else {
                conversation_copy_text(&diff, "")
            }
        }
        _ if title.starts_with(DELEGATION_TITLE_PREFIX) => conversation_copy_text(text, detail),
        _ => conversation_copy_text(text, ""),
    }
}
