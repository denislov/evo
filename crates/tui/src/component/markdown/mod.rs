//! Markdown rendering component with stable-parse checkpoint caching.
//!
//! [`Markdown::render`] re-parses the document only when it changed in a way
//! the previous parse cannot extend: appends to a document whose trailing
//! block was parseable resume from the stored [`parse::ParseCheckpoint`] and
//! re-parse just the tail (tail rerender). Everything else falls back to a
//! full parse; the checkpoint never affects output, only parse cost.

use crate::component::Component;
use crate::component::markdown::parse::{ParseCheckpoint, parse_blocks, resume_blocks};
use crate::component::markdown::wrap::wrap_blocks;
use crate::render::{Color, Style, color_enabled, paint_with, visible_width};
use crate::theme::MarkdownTheme;

mod parse;
mod style;
mod table;
mod wrap;

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
    /// Parse checkpoint for tail rerender.
    checkpoint: Option<ParseCheckpoint>,
    /// Number of times a render resumed from the checkpoint instead of
    /// re-parsing the whole document (test-support instrumentation).
    #[cfg(feature = "test-support")]
    checkpoint_resumes: usize,
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
            checkpoint: None,
            #[cfg(feature = "test-support")]
            checkpoint_resumes: 0,
        }
    }

    pub fn set_text(&mut self, text: impl Into<String>) {
        let new_text = text.into();
        let extends_checkpoint = self
            .checkpoint
            .as_ref()
            .is_some_and(|checkpoint| new_text.starts_with(checkpoint.prefix()));
        self.text = new_text;
        if extends_checkpoint {
            // The new text extends the parsed prefix: keep the checkpoint so
            // the next render can resume parsing from the tail.
            self.clear_render_cache();
        } else {
            self.invalidate();
        }
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

    /// How many renders resumed from a parse checkpoint instead of parsing
    /// the whole document. Only compiled with `test-support`.
    #[cfg(feature = "test-support")]
    pub fn checkpoint_resumes(&self) -> usize {
        self.checkpoint_resumes
    }

    fn clear_render_cache(&mut self) {
        self.cached_text = None;
        self.cached_width = 0;
        self.cached_lines.clear();
    }

    /// Parse (or resume) the current text into width-independent blocks.
    fn render_blocks(&mut self, width: usize) -> Vec<String> {
        let can_resume = self.checkpoint.as_ref().is_some_and(|checkpoint| {
            checkpoint.resumable()
                && self.text.starts_with(checkpoint.prefix())
                && !has_unclosed_inline(checkpoint.prefix())
        });
        let outcome = if can_resume {
            let checkpoint = self.checkpoint.take().expect("checked above");
            #[cfg(feature = "test-support")]
            {
                self.checkpoint_resumes += 1;
            }
            resume_blocks(
                &self.text,
                checkpoint,
                width,
                &self.theme,
                self.hyperlinks_enabled,
                &self.default_style,
            )
        } else {
            parse_blocks(
                &self.text,
                width,
                &self.theme,
                self.hyperlinks_enabled,
                &self.default_style,
            )
        };
        self.checkpoint = Some(outcome.checkpoint);
        outcome.blocks
    }
}

impl Component for Markdown {
    fn invalidate(&mut self) {
        self.clear_render_cache();
        self.checkpoint = None;
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
        let blocks = self.render_blocks(content_width);
        let mut lines = wrap_blocks(&blocks, content_width);

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

/// Heuristic: the trailing open block of a checkpoint contains inline
/// markers (`**`, `*`, `` ` ``, `~~`, `[`) that pulldown-cmark has not yet
/// resolved into spans. Such a tail cannot be resumed exactly, so the caller
/// must fall back to a full parse. Conservative false positives are safe:
/// they only cost a full re-parse, never a wrong result.
fn has_unclosed_inline(text: &str) -> bool {
    if text.matches('`').count() % 2 == 1 {
        return true;
    }
    if text.matches("**").count() % 2 == 1 {
        return true;
    }
    if text.matches('*').count() % 2 == 1 {
        return true;
    }
    if text.matches("~~").count() % 2 == 1 {
        return true;
    }
    if text.matches('[').count() > text.matches(']').count() {
        return true;
    }
    false
}
