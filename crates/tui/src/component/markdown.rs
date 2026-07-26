use pulldown_cmark::{Alignment, CodeBlockKind, Event, Options, Parser, Tag, TagEnd};
use unicode_segmentation::UnicodeSegmentation;

use crate::component::Component;
use crate::render::{Color, Style, color_enabled, paint_with};
use crate::render::{truncate_to_width, visible_width};
use crate::terminal::hyperlink;
use crate::theme::MarkdownTheme;

/// Zero-width-space marker prepended to code-block lines that should NOT
/// undergo word-wrapping (fence rows, content lines with preserved indent).
const SKIP_WRAP: &str = "\u{200B}";

/// Default styling applied to all paragraph and list text.
/// Headings, blockquotes, code blocks, and horizontal rules
/// use their own theme styling and are unaffected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DefaultTextStyle {
    /// Optional foreground color.
    pub fg: Option<Color>,
    /// Optional background color (applied at the line level,
    /// extending to the full terminal width).
    pub bg: Option<Color>,
    pub bold: bool,
    pub italic: bool,
    pub strikethrough: bool,
    pub underline: bool,
}

impl DefaultTextStyle {
    pub fn to_base_style(&self) -> Style {
        let mut s = Style::default();
        if let Some(c) = self.fg {
            s.fg = c;
        }
        if self.bold {
            s.bold = true;
        }
        if self.italic {
            s.italic = true;
        }
        if self.strikethrough {
            s.strikethrough = true;
        }
        if self.underline {
            s.underline = true;
        }
        s
    }
}

pub struct Markdown {
    text: String,
    padding_x: usize,
    padding_y: usize,
    theme: MarkdownTheme,
    hyperlinks_enabled: bool,
    default_style: Option<DefaultTextStyle>,
    /// Cache for rendered output.
    cached_text: Option<String>,
    cached_width: usize,
    cached_lines: Vec<String>,
}

impl Markdown {
    pub fn new(text: impl Into<String>) -> Self {
        Self::with_padding(text, 0, 0)
    }

    pub fn with_padding(text: impl Into<String>, padding_x: usize, padding_y: usize) -> Self {
        Self {
            text: text.into(),
            padding_x,
            padding_y,
            theme: MarkdownTheme::default(),
            hyperlinks_enabled: false,
            default_style: None,
            cached_text: None,
            cached_width: 0,
            cached_lines: Vec::new(),
        }
    }

    pub fn set_text(&mut self, text: impl Into<String>) {
        self.text = text.into();
        self.invalidate();
    }

    pub fn with_theme(mut self, theme: MarkdownTheme) -> Self {
        self.theme = theme;
        self
    }

    pub fn set_theme(&mut self, theme: MarkdownTheme) {
        self.theme = theme;
        self.invalidate();
    }

    pub fn theme(&self) -> MarkdownTheme {
        self.theme.clone()
    }

    pub fn set_hyperlinks_enabled(&mut self, enabled: bool) {
        self.hyperlinks_enabled = enabled;
        self.invalidate();
    }

    pub fn with_default_style(mut self, style: Option<DefaultTextStyle>) -> Self {
        self.default_style = style;
        self
    }

    pub fn set_default_style(&mut self, style: Option<DefaultTextStyle>) {
        self.default_style = style;
        self.invalidate();
    }

    pub fn default_style(&self) -> Option<DefaultTextStyle> {
        self.default_style
    }
}

impl Component for Markdown {
    fn invalidate(&mut self) {
        self.cached_text = None;
        self.cached_width = 0;
        self.cached_lines.clear();
    }

    fn render(&mut self, width: usize) -> Vec<String> {
        // Cache hit: if text and width haven't changed, return cached lines
        if self.cached_text.as_deref() == Some(&self.text) && self.cached_width == width {
            return self.cached_lines.clone();
        }

        if width == 0 {
            return Vec::new();
        }

        let content_width = width.saturating_sub(self.padding_x.saturating_mul(2));
        let content_width = content_width.max(1);
        let bg_color = self.default_style.as_ref().and_then(|ds| ds.bg);
        let mut lines = markdown_to_lines(
            &self.text,
            content_width,
            &self.theme,
            self.hyperlinks_enabled,
            &self.default_style,
        );

        // Apply top/bottom vertical padding
        for _ in 0..self.padding_y {
            lines.insert(0, String::new());
            lines.push(String::new());
        }

        if lines.is_empty() {
            return vec![String::new()];
        }

        // Apply horizontal padding and/or background color at line level
        let pad = " ".repeat(self.padding_x);
        if !pad.is_empty() || bg_color.is_some() {
            for line in &mut lines {
                *line = format!("{pad}{line}{pad}");
                if let Some(bg_c) = bg_color {
                    let bg_style = Style {
                        bg: bg_c,
                        ..Style::default()
                    };
                    let vw = visible_width(line);
                    if vw < width {
                        line.push_str(&" ".repeat(width - vw));
                    }
                    *line = paint_with(line, &bg_style, color_enabled());
                }
            }
        }

        // Update cache
        self.cached_text = Some(self.text.clone());
        self.cached_width = width;
        self.cached_lines = lines.clone();
        lines
    }
}

fn markdown_to_lines(
    text: &str,
    width: usize,
    theme: &MarkdownTheme,
    hyperlinks_enabled: bool,
    default_style: &Option<DefaultTextStyle>,
) -> Vec<String> {
    let parser = Parser::new_ext(text, Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TABLES);
    let mut blocks = Vec::new();
    let mut current = String::new();
    let mut context = BlockContext {
        base_style: default_style.as_ref().map(|ds| ds.to_base_style()),
        ..BlockContext::default()
    };
    for event in parser {
        match event {
            Event::Start(Tag::Heading { .. }) => {
                flush_current(
                    &mut current,
                    &mut blocks,
                    &mut context,
                    theme,
                    hyperlinks_enabled,
                );
                ensure_spacing(&mut blocks, &context);
                context.heading = true;
            }
            Event::End(TagEnd::Heading(_)) => {
                flush_current(
                    &mut current,
                    &mut blocks,
                    &mut context,
                    theme,
                    hyperlinks_enabled,
                );
                context.last_block = Some(BlockKind::Heading);
            }
            Event::Start(Tag::Paragraph) => {
                ensure_spacing(&mut blocks, &context);
            }
            Event::End(TagEnd::Paragraph) => {
                flush_current(
                    &mut current,
                    &mut blocks,
                    &mut context,
                    theme,
                    hyperlinks_enabled,
                );
                context.last_block = Some(BlockKind::Paragraph);
            }
            Event::Start(Tag::List(_)) => {
                context.list_depth += 1;
            }
            Event::End(TagEnd::List(_)) => {
                context.list_depth = context.list_depth.saturating_sub(1);
                flush_current(
                    &mut current,
                    &mut blocks,
                    &mut context,
                    theme,
                    hyperlinks_enabled,
                );
                context.last_block = Some(BlockKind::List);
            }
            Event::Start(Tag::Item) => {
                flush_current(
                    &mut current,
                    &mut blocks,
                    &mut context,
                    theme,
                    hyperlinks_enabled,
                );
                if context.list_depth > 0 {
                    current.push_str("- ");
                }
            }
            Event::End(TagEnd::Item) => flush_current(
                &mut current,
                &mut blocks,
                &mut context,
                theme,
                hyperlinks_enabled,
            ),
            Event::Start(Tag::BlockQuote(_)) => {
                if context.list_depth > 0 {
                    // Inside a list item: keep the list bullet in current.
                    // Don't add "> " — the quote styling (dim) will be applied
                    // by style_block at flush time.
                    context.in_quote = true;
                } else {
                    flush_current(
                        &mut current,
                        &mut blocks,
                        &mut context,
                        theme,
                        hyperlinks_enabled,
                    );
                    ensure_spacing(&mut blocks, &context);
                    context.in_quote = true;
                    current.push_str("> ");
                }
            }
            Event::End(TagEnd::BlockQuote(_)) => {
                flush_current(
                    &mut current,
                    &mut blocks,
                    &mut context,
                    theme,
                    hyperlinks_enabled,
                );
                context.last_block = Some(BlockKind::Quote);
            }
            Event::Start(Tag::CodeBlock(kind)) => {
                let fence_text = if context.list_depth > 0 {
                    // Inside a list item: the list bullet is already in current.
                    // Push the bullet + fence as a single block, then clear
                    // current for code content.
                    let bullet = current.to_string();
                    let lang = match &kind {
                        CodeBlockKind::Fenced(l) => l.trim(),
                        CodeBlockKind::Indented => "",
                    };
                    if lang.is_empty() {
                        format!("{bullet}```")
                    } else {
                        format!("{bullet}```{lang}")
                    }
                } else {
                    flush_current(
                        &mut current,
                        &mut blocks,
                        &mut context,
                        theme,
                        hyperlinks_enabled,
                    );
                    ensure_spacing(&mut blocks, &context);
                    String::new()
                };
                context.in_code_block = true;
                context.code_block_lang = match kind {
                    CodeBlockKind::Fenced(lang) => {
                        let lang = lang.trim();
                        if lang.is_empty() {
                            None
                        } else {
                            Some(lang.to_string())
                        }
                    }
                    CodeBlockKind::Indented => None,
                };
                let fence_line = if context.list_depth > 0 {
                    paint_markdown(&fence_text, &theme.code_block_border)
                } else {
                    paint_markdown("```", &theme.code_block_border)
                };
                blocks.push(format!("{SKIP_WRAP}{fence_line}"));
                if context.list_depth > 0 {
                    current.clear();
                }
            }
            Event::End(TagEnd::CodeBlock) => {
                // Flush accumulated code text. If a syntax highlighter is
                // configured (TS `MarkdownTheme.highlightCode`), use it;
                // otherwise fall back to the single code-block color.
                //
                // Trim streamed partial closing fences so code blocks do
                // not shrink/flicker when the final fence character arrives.
                // See https://github.com/earendil-works/pi/issues/5825.
                //
                // We only trim when the input ends WITHOUT a trailing newline
                // (meaning pulldown_cmark closed the block at EOF, not via a
                // proper closing fence). When the input HAS a trailing newline
                // the code block was properly closed, and any fence-like text
                // is genuine content.
                let has_trailing_newline = current.ends_with('\n');
                let code = if !has_trailing_newline {
                    let trimmed = current.trim_end();
                    let is_fence_char = |c: char| c == '`' || c == '~';
                    if let Some(last_newline) = trimmed.rfind('\n') {
                        let last_line = &trimmed[last_newline + 1..];
                        if !last_line.is_empty()
                            && last_line.chars().all(is_fence_char)
                            && last_line.len() < 3
                        {
                            &trimmed[..last_newline]
                        } else {
                            trimmed
                        }
                    } else if !trimmed.is_empty()
                        && trimmed.chars().all(is_fence_char)
                        && trimmed.len() < 3
                    {
                        ""
                    } else {
                        trimmed
                    }
                } else {
                    current.trim_end()
                };
                let lang = context.code_block_lang.take();
                if let Some(highlight) = &theme.highlight_code {
                    for source_line in highlight(code, lang.as_deref()) {
                        blocks.push(format!("{SKIP_WRAP}{source_line}"));
                    }
                } else {
                    for source_line in code.split('\n') {
                        let line = if source_line.is_empty() {
                            paint_markdown("   ", &theme.code_block)
                        } else {
                            paint_markdown(&format!("   {source_line}"), &theme.code_block)
                        };
                        blocks.push(format!("{SKIP_WRAP}{line}"));
                    }
                }
                current.clear();
                context.in_code_block = false;
                let close_fence = if context.list_depth > 0 {
                    paint_markdown("  ```", &theme.code_block_border)
                } else {
                    paint_markdown("```", &theme.code_block_border)
                };
                blocks.push(format!("{SKIP_WRAP}{close_fence}"));
                context.last_block = Some(BlockKind::Code);
            }
            Event::Text(text) => {
                if context.in_code_block {
                    current.push_str(&text);
                } else {
                    append_inline_text(&mut current, &text, false);
                }
            }
            Event::Code(text) => {
                let start = current.len();
                current.push_str(&text);
                context.inline_spans.push(InlineSpan {
                    start,
                    end: current.len(),
                    kind: InlineKind::Code,
                });
            }
            Event::Start(Tag::Strong) => context.strong_starts.push(current.len()),
            Event::End(TagEnd::Strong) => {
                if let Some(start) = context.strong_starts.pop() {
                    context.inline_spans.push(InlineSpan {
                        start,
                        end: current.len(),
                        kind: InlineKind::Strong,
                    });
                }
            }
            Event::Start(Tag::Emphasis) => context.emphasis_starts.push(current.len()),
            Event::End(TagEnd::Emphasis) => {
                if let Some(start) = context.emphasis_starts.pop() {
                    context.inline_spans.push(InlineSpan {
                        start,
                        end: current.len(),
                        kind: InlineKind::Emphasis,
                    });
                }
            }
            Event::Start(Tag::Strikethrough) => context.strikethrough_starts.push(current.len()),
            Event::End(TagEnd::Strikethrough) => {
                if let Some(start) = context.strikethrough_starts.pop() {
                    context.inline_spans.push(InlineSpan {
                        start,
                        end: current.len(),
                        kind: InlineKind::Strikethrough,
                    });
                }
            }
            Event::Start(Tag::Link { dest_url, .. }) => {
                context.link_starts.push(LinkStart {
                    start: current.len(),
                    url: dest_url.to_string(),
                });
            }
            Event::End(TagEnd::Link) => {
                if let Some(start) = context.link_starts.pop() {
                    context.inline_spans.push(InlineSpan {
                        start: start.start,
                        end: current.len(),
                        kind: InlineKind::Link { url: start.url },
                    });
                }
            }
            Event::SoftBreak => current.push(' '),
            Event::HardBreak => {
                if context.in_table_cell {
                    current.push('\n');
                } else {
                    flush_current(
                        &mut current,
                        &mut blocks,
                        &mut context,
                        theme,
                        hyperlinks_enabled,
                    );
                }
            }
            Event::Rule => {
                flush_current(
                    &mut current,
                    &mut blocks,
                    &mut context,
                    theme,
                    hyperlinks_enabled,
                );
                ensure_spacing(&mut blocks, &context);
                blocks.push(paint_markdown(&"-".repeat(width.min(20)), &theme.hr));
                context.last_block = Some(BlockKind::Hr);
            }
            // ── Table events ──────────────────────────────────────────
            Event::Start(Tag::Table(alignments)) => {
                flush_current(
                    &mut current,
                    &mut blocks,
                    &mut context,
                    theme,
                    hyperlinks_enabled,
                );
                ensure_spacing(&mut blocks, &context);
                context.table = Some(TableAccum {
                    alignments: alignments.to_vec(),
                    header_cells: vec![],
                    body_rows: vec![],
                    current_row: vec![],
                    in_header: false,
                });
            }
            Event::End(TagEnd::Table) => {
                // Save any last cell content that may be pending
                if context.in_table_cell {
                    if let Some(ref mut table) = context.table {
                        table.current_row.push(CellContent {
                            raw: current.clone(),
                            spans: context.inline_spans.clone(),
                        });
                    }
                    _clear_inline_tracking(&mut context);
                    context.in_table_cell = false;
                }
                current.clear();
                let base = context.base_style.as_ref();
                if let Some(table) = context.table.take() {
                    render_table(&table, width, theme, hyperlinks_enabled, base, &mut blocks);
                    context.last_block = Some(BlockKind::Table);
                }
            }
            Event::Start(Tag::TableHead) => {
                if let Some(ref mut table) = context.table {
                    table.in_header = true;
                }
            }
            Event::End(TagEnd::TableHead) => {
                // pulldown_cmark emits cells directly under TableHead (no TableRow
                // wrapper for the header), so collect the accumulated cells here.
                if context.in_table_cell {
                    if let Some(ref mut table) = context.table {
                        table.current_row.push(CellContent {
                            raw: current.clone(),
                            spans: context.inline_spans.clone(),
                        });
                    }
                    _clear_inline_tracking(&mut context);
                    context.in_table_cell = false;
                }
                current.clear();
                if let Some(ref mut table) = context.table {
                    table.in_header = false;
                    let row = std::mem::take(&mut table.current_row);
                    table.header_cells = row;
                }
            }
            Event::Start(Tag::TableRow) => {
                // Clear any leftover content when starting a new body row
                current.clear();
                context.inline_spans.clear();
                _clear_inline_tracking(&mut context);
                // Ensure we start with a fresh current_row (header already saved by End(TableHead))
                // Just in case, take the current_row so stale data doesn't accumulate.
                if let Some(ref mut table) = context.table {
                    // current_row should already be empty, but take it to be safe
                    let _ = std::mem::take(&mut table.current_row);
                }
            }
            Event::End(TagEnd::TableRow) => {
                // Flush pending cell content if the row ends without an End(TableCell)
                if context.in_table_cell {
                    if let Some(ref mut table) = context.table {
                        table.current_row.push(CellContent {
                            raw: current.clone(),
                            spans: context.inline_spans.clone(),
                        });
                    }
                    _clear_inline_tracking(&mut context);
                    context.in_table_cell = false;
                }
                current.clear();
                if let Some(ref mut table) = context.table {
                    let row = std::mem::take(&mut table.current_row);
                    if table.in_header {
                        table.header_cells = row;
                    } else {
                        table.body_rows.push(row);
                    }
                }
            }
            Event::Start(Tag::TableCell) => {
                // Flush any pending content from previous cell, then start fresh
                if context.in_table_cell {
                    if let Some(ref mut table) = context.table {
                        table.current_row.push(CellContent {
                            raw: current.clone(),
                            spans: context.inline_spans.clone(),
                        });
                    }
                    _clear_inline_tracking(&mut context);
                }
                current.clear();
                context.in_table_cell = true;
            }
            Event::End(TagEnd::TableCell) => {
                if let Some(ref mut table) = context.table {
                    table.current_row.push(CellContent {
                        raw: current.clone(),
                        spans: context.inline_spans.clone(),
                    });
                }
                _clear_inline_tracking(&mut context);
                context.in_table_cell = false;
            }
            _ => {}
        }
    }
    flush_current(
        &mut current,
        &mut blocks,
        &mut context,
        theme,
        hyperlinks_enabled,
    );

    let mut lines = Vec::new();
    for block in blocks {
        if block.starts_with(SKIP_WRAP) {
            // Pre-styled code-block line; do not word-wrap.
            lines.push(block.strip_prefix(SKIP_WRAP).unwrap_or(&block).to_string());
            continue;
        }
        for source_line in block.split('\n') {
            wrap_line(source_line, width, &mut lines);
        }
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

#[derive(Default)]
struct BlockContext {
    heading: bool,
    in_quote: bool,
    in_code_block: bool,
    code_block_lang: Option<String>,
    inline_spans: Vec<InlineSpan>,
    strong_starts: Vec<usize>,
    emphasis_starts: Vec<usize>,
    strikethrough_starts: Vec<usize>,
    link_starts: Vec<LinkStart>,
    table: Option<TableAccum>,
    in_table_cell: bool,
    /// Optional base styling for paragraph / list-item text.
    /// `None` means no default style (plain terminal text).
    base_style: Option<Style>,
    /// The kind of the most recently flushed block, used for spacing.
    last_block: Option<BlockKind>,
    /// Depth of list nesting (0 = not in a list).
    list_depth: usize,
}

#[derive(Clone)]
struct InlineSpan {
    start: usize,
    end: usize,
    kind: InlineKind,
}

#[derive(Clone)]
enum InlineKind {
    Code,
    Strong,
    Emphasis,
    Strikethrough,
    Link { url: String },
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum BlockKind {
    Paragraph,
    Heading,
    Code,
    Quote,
    Hr,
    List,
    Table,
}

#[derive(Clone)]
struct LinkStart {
    start: usize,
    url: String,
}

#[derive(Clone)]
struct CellContent {
    raw: String,
    spans: Vec<InlineSpan>,
}

struct TableAccum {
    alignments: Vec<Alignment>,
    header_cells: Vec<CellContent>,
    body_rows: Vec<Vec<CellContent>>,
    current_row: Vec<CellContent>,
    in_header: bool,
}

fn append_inline_text(current: &mut String, text: &str, in_code_block: bool) {
    if !in_code_block
        && !current.is_empty()
        && !current.ends_with([' ', '\n'])
        && !text.starts_with([' ', '\n'])
        && !starts_with_closing_punctuation(text)
    {
        current.push(' ');
    }
    current.push_str(text);
}

fn starts_with_closing_punctuation(text: &str) -> bool {
    matches!(
        text.chars().next(),
        Some('.' | ',' | ';' | ':' | '!' | '?' | ')' | ']' | '}')
    )
}

fn flush_current(
    current: &mut String,
    blocks: &mut Vec<String>,
    context: &mut BlockContext,
    theme: &MarkdownTheme,
    hyperlinks_enabled: bool,
) {
    let block = current.trim_end();
    if block.is_empty() {
        current.clear();
        context.inline_spans.clear();
        context.strong_starts.clear();
        context.emphasis_starts.clear();
        context.strikethrough_starts.clear();
        context.link_starts.clear();
        context.heading = false;
        context.in_quote = false;
        return;
    }

    let styled = style_block(block, context, theme, hyperlinks_enabled);
    blocks.push(styled);

    current.clear();
    context.inline_spans.clear();
    context.strong_starts.clear();
    context.emphasis_starts.clear();
    context.strikethrough_starts.clear();
    context.link_starts.clear();
    context.heading = false;
    context.in_quote = false;
}

fn style_block(
    block: &str,
    context: &BlockContext,
    theme: &MarkdownTheme,
    hyperlinks_enabled: bool,
) -> String {
    // For headings and blockquotes, pass their theme style as `base_style`
    // to `apply_inline_spans`. This injects the style prefix after each
    // inline span's ANSI reset (`\x1b[0m`), so inline code / bold inside
    // headings correctly restore the heading styling afterward.
    //
    // `apply_inline_spans` handles the initial prefix AND final reset,
    // so no outer `paint_markdown` wrapping is needed.
    let base_style: Option<&Style> = if context.heading {
        Some(&theme.heading)
    } else if context.in_quote {
        Some(&theme.quote)
    } else {
        context.base_style.as_ref()
    };
    apply_inline_spans(
        block,
        &context.inline_spans,
        theme,
        hyperlinks_enabled,
        base_style,
    )
}

fn apply_inline_spans(
    block: &str,
    spans: &[InlineSpan],
    theme: &MarkdownTheme,
    hyperlinks_enabled: bool,
    base_style: Option<&Style>,
) -> String {
    let base_prefix = base_style
        .filter(|_| color_enabled())
        .and_then(ansi_prefix)
        .unwrap_or_default();

    if spans.is_empty() {
        if base_prefix.is_empty() {
            return block.to_string();
        }
        // Full reset at end to match paint_markdown behavior
        return format!("{base_prefix}{block}\x1b[0m");
    }

    let mut spans = spans.to_vec();
    spans.sort_by_key(|span| (span.start, span.end));
    let mut out = String::new();
    if !base_prefix.is_empty() {
        out.push_str(&base_prefix);
    }
    let mut cursor = 0usize;
    for span in spans {
        let start = span.start.min(block.len());
        let end = span.end.min(block.len());
        if start < cursor {
            continue;
        }
        if start > cursor {
            out.push_str(&block[cursor..start]);
        }
        if end > start {
            out.push_str(&apply_inline_span(
                &block[start..end],
                &span.kind,
                theme,
                hyperlinks_enabled,
            ));
            // After the inline span's ANSI reset (\x1b[0m) re-apply
            // the base style prefix so subsequent text keeps the default style.
            if !base_prefix.is_empty() {
                out.push_str(&base_prefix);
            }
        }
        cursor = end;
    }
    if cursor < block.len() {
        out.push_str(&block[cursor..]);
    }

    // Strip trailing base prefix — it would otherwise leave dangling
    // open ANSI codes with no content to color.
    if !base_prefix.is_empty() && out.ends_with(&base_prefix) {
        out.truncate(out.len() - base_prefix.len());
    }

    // Full reset at end so the style doesn't leak to the next block.
    // This matches the behavior of paint_markdown.
    if !base_prefix.is_empty() {
        out.push_str("\x1b[0m");
    }

    out
}

fn apply_inline_span(
    text: &str,
    kind: &InlineKind,
    theme: &MarkdownTheme,
    hyperlinks_enabled: bool,
) -> String {
    match kind {
        InlineKind::Code => paint_markdown(text, &theme.code),
        InlineKind::Strong => paint_markdown(text, &theme.bold),
        InlineKind::Emphasis => paint_markdown(text, &theme.italic),
        InlineKind::Strikethrough => paint_markdown(text, &theme.strikethrough),
        InlineKind::Link { url } => {
            let styled = paint_markdown(text, &theme.link);
            if hyperlinks_enabled {
                hyperlink(&styled, url)
            } else {
                let href_for_comparison = url.strip_prefix("mailto:").unwrap_or(url);
                if text == url || text == href_for_comparison {
                    styled
                } else {
                    format!(
                        "{styled}{}",
                        paint_markdown(&format!(" ({url})"), &theme.link_url)
                    )
                }
            }
        }
    }
}

fn paint_markdown(text: &str, style: &Style) -> String {
    paint_with(text, style, color_enabled())
}

fn wrap_line(source: &str, width: usize, lines: &mut Vec<String>) {
    if source.is_empty() {
        lines.push(String::new());
        return;
    }

    if source.trim().is_empty() {
        lines.push(truncate_to_width(source, width));
        return;
    }

    let leading_whitespace: String = source.chars().take_while(|ch| ch.is_whitespace()).collect();
    let mut current = leading_whitespace;
    if visible_width(&current) >= width {
        lines.push(truncate_to_width(&current, width));
        current.clear();
    }

    for word in source.split_whitespace() {
        if visible_width(word) > width {
            if !current.is_empty() {
                lines.push(std::mem::take(&mut current));
            }
            split_long_word(word, width, lines);
            continue;
        }

        if current.is_empty() {
            current.push_str(word);
            continue;
        }

        let separator = if current.trim().is_empty() { "" } else { " " };
        let candidate = format!("{current}{separator}{word}");
        if visible_width(&candidate) <= width {
            current = candidate;
        } else {
            lines.push(std::mem::take(&mut current));
            current.push_str(word);
        }
    }

    if !current.is_empty() {
        lines.push(current);
    }
}

fn split_long_word(word: &str, width: usize, lines: &mut Vec<String>) {
    let mut current = String::new();
    let mut current_width = 0;
    for grapheme in word.graphemes(true) {
        let grapheme_width = visible_width(grapheme);
        if current_width + grapheme_width > width && !current.is_empty() {
            lines.push(std::mem::take(&mut current));
            current_width = 0;
        }
        current.push_str(grapheme);
        current_width += grapheme_width;
    }
    if !current.is_empty() {
        lines.push(truncate_to_width(&current, width));
    }
}

/// Clear inline span tracking structures (used between table cells).
/// Note: the caller must also clear `current` separately.
fn _clear_inline_tracking(context: &mut BlockContext) {
    context.inline_spans.clear();
    context.strong_starts.clear();
    context.emphasis_starts.clear();
    context.strikethrough_starts.clear();
    context.link_starts.clear();
}

/// Add a blank line before a new block-level element if the previous block
/// was also a block-level element (excluding lists, which handle their own
/// internal spacing).
fn ensure_spacing(blocks: &mut Vec<String>, context: &BlockContext) {
    if context.last_block.is_some() {
        blocks.push(String::new());
    }
}

/// Compute the ANSI prefix (everything before the text) that would be emitted
/// for a given [`Style`].  Returns `None` when color is disabled or the style
/// has no attributes set.
fn ansi_prefix(style: &Style) -> Option<String> {
    if !color_enabled() || !style.has_any() {
        return None;
    }
    let sentinel = "\0";
    let styled = paint_with(sentinel, style, true);
    styled.find('\0').map(|pos| styled[..pos].to_string())
}

/// Word-wrap a styled string, returning one line per element.
fn wrap_to_lines(text: &str, width: usize) -> Vec<String> {
    let mut lines = vec![];
    wrap_line(text, width, &mut lines);
    lines
}

fn push_table_line(blocks: &mut Vec<String>, line: String) {
    blocks.push(format!("{SKIP_WRAP}{line}"));
}

fn align_table_cell(text: &str, width: usize, alignment: Alignment) -> String {
    let padding = width.saturating_sub(visible_width(text));
    let (left, right) = match alignment {
        Alignment::Right => (padding, 0),
        Alignment::Center => (padding / 2, padding.saturating_sub(padding / 2)),
        Alignment::None | Alignment::Left => (0, padding),
    };
    format!("{}{}{}", " ".repeat(left), text, " ".repeat(right))
}

fn render_narrow_table(table: &TableAccum, blocks: &mut Vec<String>) {
    for (row_index, row) in table.body_rows.iter().enumerate() {
        if row_index > 0 {
            blocks.push(String::new());
        }
        for column in 0..table.alignments.len() {
            let label = table
                .header_cells
                .get(column)
                .map(|cell| cell.raw.replace('\t', " "))
                .filter(|label| !label.trim().is_empty())
                .unwrap_or_else(|| format!("Column {}", column + 1));
            let value = row
                .get(column)
                .map(|cell| cell.raw.replace('\t', " "))
                .unwrap_or_default();
            blocks.push(format!("{}: {}", label.trim(), value.trim()));
        }
    }
    if table.body_rows.is_empty() {
        for (column, cell) in table.header_cells.iter().enumerate() {
            blocks.push(format!(
                "Column {}: {}",
                column + 1,
                cell.raw.replace('\t', " ").trim()
            ));
        }
    }
}

/// Render a parsed table as ANSI-styled lines with box-drawing borders.
fn render_table(
    table: &TableAccum,
    total_width: usize,
    theme: &MarkdownTheme,
    hyperlinks_enabled: bool,
    base_style: Option<&Style>,
    blocks: &mut Vec<String>,
) {
    let num_cols = table.alignments.len();
    if num_cols == 0 {
        return;
    }

    // Border overhead per row: "│ a │ b │" = 2 + 3*(n-1) + 2 = 3n + 1
    let border_overhead = 3 * num_cols + 1;
    if total_width < border_overhead.saturating_add(num_cols) {
        render_narrow_table(table, blocks);
        return;
    }
    let available_for_cells = total_width - border_overhead;

    const MAX_UNBROKEN_WORD: usize = 30;

    // ── Compute column widths from raw text ──────────────────────────
    let cell_visible = |raw: &str| -> usize { visible_width(raw.replace('\t', " ").trim_end()) };
    let longest_word = |raw: &str| -> usize {
        raw.replace('\t', " ")
            .split_whitespace()
            .map(visible_width)
            .max()
            .unwrap_or(0)
            .clamp(1, MAX_UNBROKEN_WORD)
    };

    let mut natural_widths = vec![0usize; num_cols];
    let mut min_word_widths = vec![1usize; num_cols];

    for (i, cell) in table.header_cells.iter().enumerate().take(num_cols) {
        natural_widths[i] = natural_widths[i].max(cell_visible(&cell.raw));
        min_word_widths[i] = min_word_widths[i].max(longest_word(&cell.raw));
    }
    for row in &table.body_rows {
        for (i, cell) in row.iter().enumerate().take(num_cols) {
            natural_widths[i] = natural_widths[i].max(cell_visible(&cell.raw));
            min_word_widths[i] = min_word_widths[i].max(longest_word(&cell.raw));
        }
    }

    let total_natural: usize = natural_widths.iter().sum::<usize>() + border_overhead;
    let column_widths: Vec<usize> = if total_natural <= total_width {
        // ── Natural fit ──────────────────────────────────────────────
        natural_widths
            .iter()
            .zip(min_word_widths.iter())
            .map(|(nat, min)| (*nat).max(*min))
            .collect::<Vec<usize>>()
    } else {
        // ── Shrink proportionally ────────────────────────────────────
        let min_cells_width: usize = min_word_widths.iter().sum();
        let lower_bounds = if min_cells_width <= available_for_cells {
            min_word_widths.clone()
        } else {
            vec![1; num_cols]
        };
        let min_cells_width: usize = lower_bounds.iter().sum();
        let extra_width = available_for_cells.saturating_sub(min_cells_width);
        let total_grow_potential: usize = natural_widths
            .iter()
            .zip(lower_bounds.iter())
            .map(|(nat, min)| nat.saturating_sub(*min))
            .sum();

        let mut col_widths = lower_bounds.clone();
        if total_grow_potential > 0 {
            for i in 0..num_cols {
                let grow = (natural_widths[i].saturating_sub(lower_bounds[i]) as f64
                    / total_grow_potential as f64
                    * extra_width as f64) as usize;
                col_widths[i] += grow;
            }
        }

        // Distribute rounding leftovers
        let allocated: usize = col_widths.iter().sum();
        let mut remaining = available_for_cells.saturating_sub(allocated);
        'distribute: while remaining > 0 {
            for i in 0..num_cols {
                if remaining == 0 {
                    break 'distribute;
                }
                if col_widths[i] < natural_widths[i] {
                    col_widths[i] += 1;
                    remaining -= 1;
                }
            }
            if remaining > 0 {
                break;
            }
        }
        col_widths
    };

    // ── Style + wrap cells ───────────────────────────────────────────
    let style_cell = |cell: &CellContent| -> String {
        let normalized = cell.raw.replace('\t', " ");
        apply_inline_spans(
            normalized.trim_end(),
            &cell.spans,
            theme,
            hyperlinks_enabled,
            base_style,
        )
    };
    let wrap_cell = |cell: &CellContent, col_w: usize| -> Vec<String> {
        let styled = style_cell(cell);
        wrap_to_lines(&styled, col_w.max(1))
    };

    let empty_cell = CellContent {
        raw: String::new(),
        spans: vec![],
    };

    // ── Top border ────────────────────────────────────────────────────
    let top_parts: Vec<String> = column_widths.iter().map(|w| "─".repeat(*w)).collect();
    push_table_line(blocks, format!("┌─{}─┐", top_parts.join("─┬─")));

    // ── Header rows ───────────────────────────────────────────────────
    if !table.header_cells.is_empty() {
        let header_cell_lines: Vec<Vec<String>> = (0..num_cols)
            .map(|i| {
                let cell = table.header_cells.get(i).unwrap_or(&empty_cell);
                wrap_cell(cell, column_widths[i])
            })
            .collect();
        let header_line_count = header_cell_lines
            .iter()
            .map(|lines| lines.len())
            .max()
            .unwrap_or(0);

        for line_idx in 0..header_line_count {
            let row_parts: Vec<String> = header_cell_lines
                .iter()
                .enumerate()
                .map(|(ci, lines)| {
                    let text = lines.get(line_idx).map(|s| s.as_str()).unwrap_or("");
                    let padded = align_table_cell(text, column_widths[ci], table.alignments[ci]);
                    paint_markdown(&padded, &theme.bold)
                })
                .collect();
            push_table_line(blocks, format!("│ {} │", row_parts.join(" │ ")));
        }

        // Header / body separator: ├──┼──┤
        let sep_parts: Vec<String> = column_widths.iter().map(|w| "─".repeat(*w)).collect();
        push_table_line(blocks, format!("├─{}─┤", sep_parts.join("─┼─")));
    }

    // ── Body rows ─────────────────────────────────────────────────────
    for (row_idx, row) in table.body_rows.iter().enumerate() {
        let body_cell_lines: Vec<Vec<String>> = (0..num_cols)
            .map(|i| {
                let cell = row.get(i).unwrap_or(&empty_cell);
                wrap_cell(cell, column_widths[i])
            })
            .collect();
        let body_line_count = body_cell_lines
            .iter()
            .map(|lines| lines.len())
            .max()
            .unwrap_or(0);

        for line_idx in 0..body_line_count {
            let row_parts: Vec<String> = body_cell_lines
                .iter()
                .enumerate()
                .map(|(ci, lines)| {
                    let text = lines.get(line_idx).map(|s| s.as_str()).unwrap_or("");
                    align_table_cell(text, column_widths[ci], table.alignments[ci])
                })
                .collect();
            push_table_line(blocks, format!("│ {} │", row_parts.join(" │ ")));
        }

        // Row separator between data rows (no separator after last row)
        if row_idx < table.body_rows.len().saturating_sub(1) && !table.body_rows.is_empty() {
            let sep_parts: Vec<String> = column_widths.iter().map(|w| "─".repeat(*w)).collect();
            push_table_line(blocks, format!("├─{}─┤", sep_parts.join("─┼─")));
        }
    }

    // ── Bottom border ─────────────────────────────────────────────────
    let bottom_parts: Vec<String> = column_widths.iter().map(|w| "─".repeat(*w)).collect();
    push_table_line(blocks, format!("└─{}─┘", bottom_parts.join("─┴─")));
}
