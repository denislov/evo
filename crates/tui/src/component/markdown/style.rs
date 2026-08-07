//! Block styling: applying inline spans (code / strong / emphasis /
//! strikethrough / links) and flushing accumulated text into styled blocks.

use crate::component::markdown::parse::{BlockContext, InlineKind, InlineSpan};
use crate::render::Style;
use crate::render::{color_enabled, paint_with};
use crate::terminal::hyperlink;
use crate::theme::MarkdownTheme;

pub(super) fn append_inline_text(current: &mut String, text: &str, in_code_block: bool) {
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

pub(super) fn flush_current(
    blocks: &mut Vec<String>,
    current: &mut String,
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

pub(super) fn apply_inline_spans(
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

pub(super) fn paint_markdown(text: &str, style: &Style) -> String {
    paint_with(text, style, color_enabled())
}

/// Clear inline span tracking structures (used between table cells).
/// Note: the caller must also clear `current` separately.
pub(super) fn _clear_inline_tracking(context: &mut BlockContext) {
    context.inline_spans.clear();
    context.strong_starts.clear();
    context.emphasis_starts.clear();
    context.strikethrough_starts.clear();
    context.link_starts.clear();
}

/// Add a blank line before a new block-level element if the previous block
/// was also a block-level element (excluding lists, which handle their own
/// internal spacing).
pub(super) fn ensure_spacing(blocks: &mut Vec<String>, context: &BlockContext) {
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

/// Inline closing events (Strong/Emphasis/Strikethrough/Link) only generate
/// spans from their matching open state. They are idempotent (a missing open
/// state is a no-op), so a resumed parse re-applies them unconditionally
/// instead of skipping them like block closures.
pub(super) fn is_inline_end(event: &pulldown_cmark::Event<'_>) -> bool {
    matches!(
        event,
        pulldown_cmark::Event::End(
            pulldown_cmark::TagEnd::Strong
                | pulldown_cmark::TagEnd::Emphasis
                | pulldown_cmark::TagEnd::Strikethrough
                | pulldown_cmark::TagEnd::Link
        )
    )
}

/// A content event interrupts a run of closing events: the run can no longer
/// reach EOF, so its pending spans must be discarded.
pub(super) fn end_segment_interrupted(
    tracking: &mut crate::component::markdown::parse::EofTracking,
) {
    tracking.in_end_segment = false;
    tracking.pending_ends.clear();
}
