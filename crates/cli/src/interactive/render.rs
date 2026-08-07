use std::hash::{Hash, Hasher};
use std::path::Path;
use std::sync::Arc;

use tui::api::component::{Component, Image, Loader, Markdown};
use tui::api::render::{
    Color, ERROR, SYSTEM, Style, TOOL_ERROR, TOOL_NAME, USER, paint_with, truncate_to_width,
    visible_width, wrap_text_with_ansi,
};
use tui::api::terminal::TerminalCapabilities;
use tui::api::theme::MarkdownTheme;

#[cfg(test)]
use crate::interactive::transcript::Transcript;
use crate::interactive::transcript::{
    TranscriptBlockId, TranscriptDisplayState, TranscriptItem, TranscriptViewSnapshot,
};
use coding_agent::api::settings::{
    CodingAgentResolvedColor, CodingAgentThemeBackground, CodingAgentThemeForeground,
    CodingAgentThemeSnapshot,
};

mod cache;
mod tools;

#[cfg(test)]
use cache::legacy_display_state;
pub(super) use cache::{TranscriptBlockRows, TranscriptRenderCache, TranscriptRowSnapshot};
use tools::render_tool_block;

/// Resolved visual styles for transcript blocks, derived from a
/// [`CodingAgentThemeSnapshot`] (when available) or falling back to the built-in
/// palette constants otherwise. Mirrors the TS `theme.fg`/`theme.bg`
/// calls used by the interactive transcript components.
#[derive(Debug, Clone, Copy)]
pub(super) struct TranscriptStyles {
    pub user_text: Style,
    pub user_bg: Style,
    pub thinking: Style,
    pub system: Style,
    pub error: Style,
    pub tool_title: Style,
    pub tool_output: Style,
    pub tool_pending_bg: Style,
    pub tool_success_bg: Style,
    pub tool_error_bg: Style,
    pub tool_error_text: Style,
    pub tool_diff_added: Style,
    pub tool_diff_removed: Style,
    pub tool_diff_context: Style,
    pub bash_mode: Style,
    pub warning: Style,
    pub accent: Style,
}

impl TranscriptStyles {
    /// Resolve styles from an optional [`CodingAgentThemeSnapshot`]. When `None`
    /// (e.g. in unit tests without a loaded theme), falls back to the
    /// built-in tui palette constants so the transcript still renders
    /// with sensible defaults.
    pub(super) fn from_theme(resolved: Option<&CodingAgentThemeSnapshot>) -> Self {
        match resolved {
            Some(theme) => Self::from_resolved(theme),
            None => Self::fallback(),
        }
    }

    fn from_resolved(theme: &CodingAgentThemeSnapshot) -> Self {
        let fg = |token: CodingAgentThemeForeground| Style::fg(to_color(theme.foreground(token)));
        let bg = |token: CodingAgentThemeBackground| Style {
            fg: Color::Default,
            bg: to_color(theme.background(token)),
            bold: false,
            dim: false,
            italic: false,
            underline: false,
            strikethrough: false,
            reverse: false,
        };
        Self {
            user_text: fg(CodingAgentThemeForeground::UserMessageText),
            user_bg: bg(CodingAgentThemeBackground::UserMessage),
            thinking: fg(CodingAgentThemeForeground::ThinkingText).italic(),
            system: Style::fg(Color::Default).dim(),
            error: fg(CodingAgentThemeForeground::Error).bold(),
            tool_title: fg(CodingAgentThemeForeground::ToolTitle).bold(),
            tool_output: fg(CodingAgentThemeForeground::ToolOutput),
            tool_pending_bg: bg(CodingAgentThemeBackground::ToolPending),
            tool_success_bg: bg(CodingAgentThemeBackground::ToolSuccess),
            tool_error_bg: bg(CodingAgentThemeBackground::ToolError),
            tool_error_text: fg(CodingAgentThemeForeground::Error),
            tool_diff_added: fg(CodingAgentThemeForeground::ToolDiffAdded),
            tool_diff_removed: fg(CodingAgentThemeForeground::ToolDiffRemoved),
            tool_diff_context: fg(CodingAgentThemeForeground::ToolDiffContext),
            bash_mode: fg(CodingAgentThemeForeground::BashMode).bold(),
            warning: fg(CodingAgentThemeForeground::Warning),
            accent: fg(CodingAgentThemeForeground::Accent),
        }
    }

    fn fallback() -> Self {
        Self {
            user_text: USER,
            user_bg: Style::default(),
            thinking: Style::fg(Color::Yellow).italic(),
            system: SYSTEM,
            error: ERROR,
            tool_title: TOOL_NAME.bold(),
            tool_output: Style::default(),
            tool_pending_bg: Style::default(),
            tool_success_bg: Style::default(),
            tool_error_bg: Style::default(),
            tool_error_text: TOOL_ERROR,
            tool_diff_added: Style::fg(Color::Green),
            tool_diff_removed: Style::fg(Color::Red),
            tool_diff_context: Style::fg(Color::Default).dim(),
            bash_mode: Style::fg(Color::Green).bold(),
            warning: Style::fg(Color::Yellow),
            accent: Style::fg(Color::Cyan),
        }
    }
}

fn to_color(color: CodingAgentResolvedColor) -> Color {
    match color {
        CodingAgentResolvedColor::Default => Color::Default,
        CodingAgentResolvedColor::Rgb(red, green, blue) => Color::Rgb(red, green, blue),
        CodingAgentResolvedColor::Ansi256(value) => Color::Ansi256(value),
    }
}

/// Build a [`MarkdownTheme`] from a [`CodingAgentThemeSnapshot`], mirroring TS
/// `getMarkdownTheme()` (theme.ts). Each `md*` token maps to its resolved
/// color; `bold`/`italic`/`underline`/`strikethrough` are attribute-only
/// (fg=Default) to match TS `theme.bold`/`theme.italic`/... which inherit
/// the surrounding foreground rather than imposing a fixed color. No `.dim()`
/// is layered on — dark.json's `gray`/`dimGray` vars already carry the
/// intended lightness, and stacking `dim` would diverge from TS.
///
/// `highlight_code` is left `None`; the caller (root `markdown_theme()`)
/// mounts the syntax-highlight callback separately.
pub(super) fn markdown_theme_from_resolved(theme: &CodingAgentThemeSnapshot) -> MarkdownTheme {
    let fg = |token: CodingAgentThemeForeground| Style::fg(to_color(theme.foreground(token)));
    MarkdownTheme {
        heading: fg(CodingAgentThemeForeground::MdHeading).bold(),
        link: fg(CodingAgentThemeForeground::MdLink),
        link_url: fg(CodingAgentThemeForeground::MdLinkUrl),
        code: fg(CodingAgentThemeForeground::MdCode),
        code_block: fg(CodingAgentThemeForeground::MdCodeBlock),
        code_block_border: fg(CodingAgentThemeForeground::MdCodeBlockBorder),
        quote: fg(CodingAgentThemeForeground::MdQuote),
        quote_border: fg(CodingAgentThemeForeground::MdQuoteBorder),
        hr: fg(CodingAgentThemeForeground::MdHr),
        list_bullet: fg(CodingAgentThemeForeground::MdListBullet),
        bold: Style::fg(Color::Default).bold(),
        italic: Style::fg(Color::Default).italic(),
        underline: Style::fg(Color::Default).underline(),
        strikethrough: Style::fg(Color::Default).strikethrough(),
        highlight_code: None,
    }
}

/// All inputs to transcript block rendering, bundling width, color,
/// markdown theme, thinking visibility, and resolved [`TranscriptStyles`].
/// Mirrors the props threaded through TS `UserMessageComponent` /
/// `AssistantMessageComponent` / `ToolExecutionComponent`.
#[derive(Clone)]
pub(super) struct TranscriptRenderOptions<'a> {
    pub width: usize,
    pub max_tool_result_lines: usize,
    pub color: bool,
    pub markdown_theme: MarkdownTheme,
    pub hide_thinking_block: bool,
    pub hidden_thinking_label: &'a str,
    pub styles: TranscriptStyles,
    pub view: Option<Arc<TranscriptViewSnapshot>>,
    pub selected_block: Option<TranscriptBlockId>,
    pub selection_gutter: bool,
    pub show_images: bool,
    pub image_width_cells: u32,
    pub terminal_capabilities: TerminalCapabilities,
}

#[cfg(test)]
pub(super) fn render_transcript_lines(
    transcript: &Transcript,
    opts: &TranscriptRenderOptions<'_>,
) -> Vec<String> {
    let TranscriptRenderOptions {
        width,
        max_tool_result_lines,
        color,
        markdown_theme,
        hide_thinking_block,
        hidden_thinking_label,
        styles,
        view,
        selected_block,
        selection_gutter,
        show_images,
        image_width_cells,
        terminal_capabilities,
    } = opts.clone();

    let mut lines = Vec::new();
    // Spacing policy: insert one blank line before every visible block except
    // the very first one. "Visible" excludes leading System welcome lines,
    // which keep their existing dim treatment. This replaces the old
    // ad-hoc "rule between finished tool and assistant" separator.
    let mut emitted_visible_block = false;

    for (render_key, item) in transcript.render_entries() {
        let block_id = render_key.block_id();
        let display_state = view.as_ref().map_or_else(
            || legacy_display_state(item),
            |view| view.display_state(block_id, item),
        );
        let tool_argument_state = view
            .as_ref()
            .map_or(TranscriptDisplayState::Collapsed, |view| {
                view.tool_argument_state(block_id, item)
            });
        let item_selection_gutter = selection_gutter;
        let block = render_block(
            item,
            width,
            max_tool_result_lines,
            color,
            &markdown_theme,
            hide_thinking_block,
            hidden_thinking_label,
            styles,
            display_state,
            tool_argument_state,
            transcript_image_id(render_key.transcript_id, render_key.item_id),
            item.selectable() && selected_block == Some(block_id),
            item_selection_gutter,
            show_images,
            image_width_cells,
            terminal_capabilities,
        );
        if block.is_empty() {
            continue;
        }
        let is_visible_block = !matches!(item, TranscriptItem::System { .. });
        if is_visible_block && emitted_visible_block {
            lines.push(String::new());
        }
        lines.extend(block);
        if is_visible_block {
            emitted_visible_block = true;
        }
    }

    lines
}

fn render_profile_hash(opts: &TranscriptRenderOptions<'_>) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    opts.width.hash(&mut hasher);
    opts.max_tool_result_lines.hash(&mut hasher);
    opts.color.hash(&mut hasher);
    opts.hide_thinking_block.hash(&mut hasher);
    opts.hidden_thinking_label.hash(&mut hasher);
    format!("{:?}", opts.markdown_theme).hash(&mut hasher);
    format!("{:?}", opts.styles).hash(&mut hasher);
    opts.show_images.hash(&mut hasher);
    opts.image_width_cells.hash(&mut hasher);
    format!("{:?}", opts.terminal_capabilities).hash(&mut hasher);
    hasher.finish()
}

fn render_row_profile_hash(opts: &TranscriptRenderOptions<'_>, profile_hash: u64) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    profile_hash.hash(&mut hasher);
    opts.view
        .as_ref()
        .map(|view| view.revision())
        .hash(&mut hasher);
    opts.selected_block.hash(&mut hasher);
    opts.selection_gutter.hash(&mut hasher);
    hasher.finish()
}

/// Render a single transcript item into zero or more lines. Each visible
/// item is a self-contained "block"; the caller inserts spacing between
/// blocks.
#[allow(clippy::too_many_arguments)]
fn render_block(
    item: &TranscriptItem,
    width: usize,
    max_tool_result_lines: usize,
    color: bool,
    markdown_theme: &MarkdownTheme,
    hide_thinking_block: bool,
    hidden_thinking_label: &str,
    styles: TranscriptStyles,
    display_state: TranscriptDisplayState,
    tool_argument_state: TranscriptDisplayState,
    image_id: u32,
    selected: bool,
    selection_gutter: bool,
    show_images: bool,
    image_width_cells: u32,
    terminal_capabilities: TerminalCapabilities,
) -> Vec<String> {
    let content_width = if selection_gutter {
        width.saturating_sub(2).max(1)
    } else {
        width
    };
    let lines = match item {
        TranscriptItem::User { text } => {
            render_user_message(text, content_width, color, markdown_theme, &styles)
        }
        TranscriptItem::System { text } => text
            .split('\n')
            .map(|line| fit_line(&paint_with(line, &styles.system, color), content_width))
            .collect(),
        TranscriptItem::Assistant {
            markdown,
            thinking,
            thinking_seconds,
            ..
        } => render_assistant_message(
            markdown,
            thinking,
            *thinking_seconds,
            content_width,
            color,
            markdown_theme,
            hide_thinking_block,
            hidden_thinking_label,
            &styles,
            display_state,
        ),
        TranscriptItem::Tool {
            name,
            args,
            result,
            is_error,
            ..
        } => render_tool_block(
            name,
            args,
            result.as_deref(),
            *is_error,
            content_width,
            max_tool_result_lines,
            color,
            &styles,
            display_state,
            tool_argument_state,
            selection_gutter,
        ),
        TranscriptItem::Image { mime_type, data } => render_image_block(
            mime_type,
            data,
            content_width,
            show_images,
            image_width_cells,
            terminal_capabilities,
            image_id,
            &styles,
            color,
        ),
        TranscriptItem::Error { text } => render_error_message(text, content_width, color, &styles),
    };
    if selection_gutter {
        apply_selection_gutter(lines, width, selected)
    } else {
        lines
    }
}

fn transcript_image_id(transcript_id: u64, item_id: u64) -> u32 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    transcript_id.hash(&mut hasher);
    item_id.hash(&mut hasher);
    let id = hasher.finish() as u32;
    id.max(1)
}

#[allow(clippy::too_many_arguments)]
fn render_image_block(
    mime_type: &str,
    data: &str,
    width: usize,
    show_images: bool,
    image_width_cells: u32,
    terminal_capabilities: TerminalCapabilities,
    image_id: u32,
    styles: &TranscriptStyles,
    color: bool,
) -> Vec<String> {
    if !show_images {
        return vec![fit_line(
            &paint_with(&format!("[Image: {mime_type}]"), &styles.system, color),
            width,
        )];
    }
    let max_width = image_width_cells.max(1).min(width.max(1) as u32);
    let mut image = Image::new(data, mime_type)
        .capabilities(terminal_capabilities)
        .max_width_cells(max_width)
        .image_id(image_id);
    image.render(width)
}

fn apply_selection_gutter(lines: Vec<String>, width: usize, selected: bool) -> Vec<String> {
    lines
        .into_iter()
        .enumerate()
        .map(|(index, line)| {
            let marker = if selected && index == 0 { "▌ " } else { "  " };
            fit_line(&format!("{marker}{line}"), width)
        })
        .collect()
}

/// Render a user message as a backgrounded box (TS `UserMessageComponent`):
/// one padding row top/bottom, content padded left/right by one column,
/// painted with `userMessageBg` / `userMessageText`.
fn render_user_message(
    text: &str,
    width: usize,
    color: bool,
    markdown_theme: &MarkdownTheme,
    styles: &TranscriptStyles,
) -> Vec<String> {
    if width == 0 {
        return Vec::new();
    }
    // Inner content width after left/right padding (min 1).
    let padding_x = 1usize.min(width.saturating_sub(1) / 2);
    let content_width = width.saturating_sub(padding_x * 2).max(1);
    let left_pad = " ".repeat(padding_x);

    let mut content_lines = Vec::new();
    let mut md = Markdown::new(text).with_theme(markdown_theme.clone());
    for line in md.render(content_width) {
        content_lines.push(format!(
            "{left_pad}{}",
            paint_with(&line, &styles.user_text, color)
        ));
    }
    if content_lines.is_empty() {
        content_lines.push(left_pad.clone());
    }

    let mut lines = Vec::new();
    // Top padding row (background-filled blank line).
    lines.push(paint_bg_line("", width, &styles.user_bg, color));
    for line in content_lines {
        lines.push(paint_bg_line(&line, width, &styles.user_bg, color));
    }
    lines.push(paint_bg_line("", width, &styles.user_bg, color));
    lines
}

/// Render an assistant message (TS `AssistantMessageComponent`): no
/// background, optional thinking block, then markdown body at the common
/// transcript content origin. Thinking and body are separated by one blank line only when the
/// body has visible content.
#[allow(clippy::too_many_arguments)]
fn render_assistant_message(
    markdown: &str,
    thinking: &str,
    thinking_seconds: Option<f64>,
    width: usize,
    color: bool,
    markdown_theme: &MarkdownTheme,
    hide_thinking_block: bool,
    hidden_thinking_label: &str,
    styles: &TranscriptStyles,
    display_state: TranscriptDisplayState,
) -> Vec<String> {
    let mut lines = Vec::new();
    let has_thinking = !thinking.trim().is_empty();
    let has_body = !markdown.trim().is_empty();

    if has_thinking {
        if hide_thinking_block {
            // Hidden thinking still surfaces a static label (TS behavior),
            // so users know reasoning happened without dumping its content.
            let label = match thinking_seconds {
                Some(seconds) => format!("Thought for {seconds:.1}s"),
                None => hidden_thinking_label.to_string(),
            };
            lines.push(fit_line(
                &paint_with(&label, &styles.thinking, color),
                width,
            ));
        } else {
            let thinking_lines = thinking.lines().collect::<Vec<_>>();
            const MAX_THINKING_PREVIEW_ROWS: usize = 4;
            let (shown, label) = match display_state {
                TranscriptDisplayState::Collapsed => {
                    let label = match thinking_seconds {
                        Some(seconds) => format!("Thought for {seconds:.1}s"),
                        None => format!("thinking · {} lines hidden", thinking_lines.len()),
                    };
                    (Vec::new(), label)
                }
                TranscriptDisplayState::Preview => {
                    let label = match thinking_seconds {
                        Some(seconds) => format!("Thought for {seconds:.1}s"),
                        None => {
                            let start = thinking_lines
                                .len()
                                .saturating_sub(MAX_THINKING_PREVIEW_ROWS);
                            if start > 0 {
                                format!("thinking · preview · {start} earlier lines")
                            } else {
                                "thinking · preview".to_string()
                            }
                        }
                    };
                    let start = thinking_lines
                        .len()
                        .saturating_sub(MAX_THINKING_PREVIEW_ROWS);
                    (thinking_lines[start..].to_vec(), label)
                }
                TranscriptDisplayState::Expanded => {
                    let label = match thinking_seconds {
                        Some(seconds) => format!("Thought for {seconds:.1}s"),
                        None => "thinking · expanded".to_string(),
                    };
                    (thinking_lines.clone(), label)
                }
            };
            lines.push(fit_line(&paint_with(&label, &styles.system, color), width));
            let think_width = width.saturating_sub(2).max(1);
            let mut content_lines = Vec::new();
            for line in shown {
                let painted = paint_with(line, &styles.thinking, color);
                for wrapped in wrap_text_with_ansi(&painted, think_width) {
                    content_lines.push(fit_line(&format!("  {wrapped}"), width));
                }
            }
            if display_state == TranscriptDisplayState::Preview {
                // Capped preview height: keep the trailing rows so long
                // streaming thinking cannot grow the block past the cap.
                let start = content_lines
                    .len()
                    .saturating_sub(MAX_THINKING_PREVIEW_ROWS);
                content_lines.drain(..start);
            }
            lines.extend(content_lines);
        }
        if has_body {
            lines.push(String::new());
        }
    }

    if has_body {
        let mut md = Markdown::new(markdown).with_theme(markdown_theme.clone());
        for line in md.render(width) {
            lines.push(fit_line(&line, width));
        }
    }

    lines
}

/// Render an error item with an `Error:` label (TS assistant-message error
/// fallback style).
///
/// Long errors wrap to the available transcript width (mirrors TS error
/// rendering) instead of being truncated to a single line. The `Error:` label
/// prefixes only the first rendered line; continuation lines wrap at column 0.
/// `fit_line` is kept as a final safety clamp so ANSI-bearing wrapped lines
/// can never overflow the width.
fn render_error_message(
    text: &str,
    width: usize,
    color: bool,
    styles: &TranscriptStyles,
) -> Vec<String> {
    let label = paint_with("Error:", &styles.error, color);
    // The first rendered line shares its row with the `Error: ` label (label
    // plus one separating space), so the first source line wraps to the
    // reduced width; later lines use the full width.
    let first_width = width.saturating_sub(visible_width("Error: ")).max(1);

    let mut out: Vec<String> = Vec::new();
    for source_line in text.split('\n') {
        let wrap_width = if out.is_empty() { first_width } else { width };
        for wrapped_line in wrap_text_with_ansi(source_line, wrap_width) {
            let body = paint_with(&wrapped_line, &styles.error, color);
            if out.is_empty() {
                out.push(fit_line(&format!("{label} {body}"), width));
            } else {
                out.push(fit_line(&body, width));
            }
        }
    }
    out
}

/// Paint a line with a background style, padding it to the full render
/// width so the background fills the row (mirrors the generic TUI `Box` background
/// handling). When color is disabled this collapses to a plain padded line,
/// so layout (spacing/indent) is preserved on colorless terminals.
///
/// `text` may already carry foreground ANSI codes (e.g. the user-message
/// text color). Those nested resets would normally drop the background for
/// the rest of the row, so when a background is applied we rewrite inner
/// `\x1b[0m` (full reset) to `\x1b[39m` (foreground-only reset, mirroring
/// TS `theme.bg` which closes with `\x1b[49m`). This keeps the background
/// span unbroken across the trailing padding.
fn paint_bg_line(text: &str, width: usize, bg: &Style, color: bool) -> String {
    let padded = pad_to_width(text, width);
    if !color || bg.bg == Color::Default {
        // No background to apply: keep the padded line verbatim (foreground
        // codes, if any, stay as-is).
        return padded;
    }
    // Rewrite inner full-resets so the background survives the content's
    // own foreground reset.
    let content = padded.replace("\x1b[0m", "\x1b[39m");
    let bg_style = Style {
        fg: Color::Default,
        bg: bg.bg,
        bold: false,
        dim: false,
        italic: false,
        underline: false,
        strikethrough: false,
        reverse: false,
    };
    paint_with(&content, &bg_style, color)
}

/// Pad `text` with trailing spaces to `width`, truncating if it overflows.
fn pad_to_width(text: &str, width: usize) -> String {
    let mut line = if visible_width(text) <= width {
        text.to_string()
    } else {
        truncate_to_width(text, width)
    };
    let line_width = visible_width(&line);
    if line_width < width {
        line.push_str(&" ".repeat(width - line_width));
    }
    line
}

#[allow(
    clippy::too_many_arguments,
    reason = "tool rendering keeps independent presentation controls explicit"
)]
pub(super) fn editor_border_line(width: usize, style: &Style, color: bool) -> String {
    if width == 0 {
        return String::new();
    }
    fit_line(&paint_with(&"─".repeat(width), style, color), width)
}

pub(super) fn fit_line(line: &str, width: usize) -> String {
    if visible_width(line) <= width {
        line.to_string()
    } else {
        truncate_to_width(line, width)
    }
}

/// Wrap a modal surface in a visible box border so dialogs read as modal
/// surfaces instead of plain transcript text. `lines` must already be sized
/// to at most `width - 4` (the content column between the two border
/// columns); short lines are padded so the right border column stays
/// aligned. The returned lines are exactly `width` wide. Falls back to the
/// raw lines when the width is too small for a border.
pub(super) fn framed_modal_lines(
    lines: Vec<String>,
    width: usize,
    style: &Style,
    color: bool,
) -> Vec<String> {
    if width < 5 || lines.is_empty() {
        return lines;
    }
    let content_width = width.saturating_sub(3);
    let mut framed = Vec::with_capacity(lines.len() + 2);
    framed.push(paint_with(
        &format!("┌{}┐", "─".repeat(width.saturating_sub(2))),
        style,
        color,
    ));
    for line in lines {
        framed.push(format!(
            "{}{}{}",
            paint_with("│ ", style, color),
            pad_to_width(&line, content_width),
            paint_with("│", style, color)
        ));
    }
    framed.push(paint_with(
        &format!("└{}┘", "─".repeat(width.saturating_sub(2))),
        style,
        color,
    ));
    framed
}

pub(super) fn running_status_text(frame: usize) -> String {
    let mut loader = Loader::new("running");
    for _ in 0..frame {
        loader.tick();
    }
    loader.render_text()
}

pub(super) fn format_tokens(count: u32) -> String {
    if count < 1000 {
        count.to_string()
    } else if count < 10000 {
        format!("{:.1}k", count as f64 / 1000.0)
    } else if count < 1000000 {
        format!("{}k", count / 1000)
    } else if count < 10000000 {
        format!("{:.1}M", count as f64 / 1000000.0)
    } else {
        format!("{}M", count / 1000000)
    }
}

/// Warning style for the context-usage percentage (70–90% band), matching
/// the TypeScript footer's `theme.fg("warning", ...)`.
pub(super) const WARNING: Style = Style::fg(Color::Yellow);

pub(super) fn abbreviate_cwd(cwd: &Path) -> String {
    let display = cwd.display().to_string();
    if let Ok(home) = std::env::var("HOME")
        && !home.is_empty()
        && display.starts_with(&home)
    {
        return format!("~{}", &display[home.len()..]);
    }
    display
}

#[cfg(test)]
#[path = "render_tests.rs"]
mod tests;
