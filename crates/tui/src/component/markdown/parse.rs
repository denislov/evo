//! Markdown block parsing with stable checkpoints.
//!
//! A parse produces two artifacts:
//! - `blocks`: styled, width-independent block fragments.
//! - `ParseCheckpoint`: the parser state at the last stable block boundary,
//!   enabling a subsequent append (`text = prefix + tail`) to resume parsing
//!   only the tail instead of re-parsing the whole document.
//!
//! Resuming is exact when the previous parse ended inside an open block.
//! pulldown-cmark closes open blocks at EOF and emits the closing events
//! with a span ending exactly at the text length; the checkpoint rolls those
//! closures back, and the tail re-parse replays them from the resumed state.
//! When the tail is blocked by a heuristic that cannot prove the open block
//! is resume-safe (unclosed inline markers such as `**`), the caller falls
//! back to a full parse; correctness never depends on resuming.

use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag, TagEnd};

use crate::component::markdown::style::{
    _clear_inline_tracking, append_inline_text, end_segment_interrupted, ensure_spacing,
    flush_current, is_inline_end, paint_markdown,
};
use crate::component::markdown::table::{CellContent, TableAccum, render_table};
use crate::component::markdown::wrap::SKIP_WRAP;
use crate::render::Style;
use crate::theme::MarkdownTheme;

use super::DefaultTextStyle;

/// Width-independent rendered output of a markdown parse.
pub(super) struct ParseOutcome {
    pub(super) blocks: Vec<String>,
    pub(super) checkpoint: ParseCheckpoint,
}

/// Stable checkpoint between two parses of the same (extending) document.
#[derive(Clone)]
pub(super) struct ParseCheckpoint {
    /// Full text that was parsed to produce this checkpoint.
    text: String,
    /// Parsing state to resume from. This is the state after the last
    /// content event: the EOF-closing events and any closing events that
    /// immediately precede them (pulldown-cmark can "prematurely" close a
    /// block at EOF, e.g. a quote paragraph ending in `\n> `) are rolled
    /// back so the tail can extend or re-close them.
    resume: ParseState,
    /// Span end of the last content event (Text/Code/SoftBreak/HardBreak)
    /// of the previous parse. Tail events whose span extends past this
    /// boundary continue a content event and are spliced at this point.
    last_content_end: usize,
    /// End-event spans rolled back into `resume`; a resumed parse replays
    /// each of them (when the tail extends its span) instead of skipping.
    rolled_back_ends: Vec<(usize, usize)>,
    /// True when the previous parse ended inside an open block whose
    /// EOF-closing events must be replayed on resume.
    ends_open: bool,
}

impl ParseCheckpoint {
    /// Full text parsed to produce this checkpoint.
    pub(super) fn prefix(&self) -> &str {
        &self.text
    }

    /// Whether resuming from this checkpoint is sound. Unsound when the
    /// trailing open block holds state pulldown-cmark could extend into the
    /// tail (an open table cell — its event spans shift as the cell fills,
    /// which the resume cannot splice).
    pub(super) fn resumable(&self) -> bool {
        if !self.ends_open {
            return true;
        }
        !self.resume.context.in_table_cell
    }
}

#[derive(Clone, Default)]
struct ParseState {
    blocks: Vec<String>,
    current: String,
    context: BlockContext,
}

#[derive(Clone, Default)]
pub(super) struct BlockContext {
    pub(super) heading: bool,
    pub(super) in_quote: bool,
    pub(super) in_code_block: bool,
    pub(super) code_block_lang: Option<String>,
    pub(super) inline_spans: Vec<InlineSpan>,
    pub(super) strong_starts: Vec<usize>,
    pub(super) emphasis_starts: Vec<usize>,
    pub(super) strikethrough_starts: Vec<usize>,
    pub(super) link_starts: Vec<LinkStart>,
    pub(super) table: Option<TableAccum>,
    pub(super) in_table_cell: bool,
    /// Optional base styling for paragraph / list-item text.
    /// `None` means no default style (plain terminal text).
    pub(super) base_style: Option<Style>,
    /// The kind of the most recently flushed block, used for spacing.
    pub(super) last_block: Option<BlockKind>,
    /// Depth of list nesting (0 = not in a list).
    pub(super) list_depth: usize,
}

#[derive(Clone)]
pub(super) struct InlineSpan {
    pub(super) start: usize,
    pub(super) end: usize,
    pub(super) kind: InlineKind,
}

#[derive(Clone)]
pub(super) enum InlineKind {
    Code,
    Strong,
    Emphasis,
    Strikethrough,
    Link { url: String },
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum BlockKind {
    Paragraph,
    Heading,
    Code,
    Quote,
    Hr,
    List,
    Table,
}

#[derive(Clone)]
pub(super) struct LinkStart {
    pub(super) start: usize,
    pub(super) url: String,
}

/// Event tracking over a parse. pulldown-cmark closes open blocks at EOF and
/// emits the closing events with a span ending exactly at the text length;
/// blocks may also be closed "prematurely" just before EOF (e.g. a quote
/// paragraph ending in `\n> `). This records the state after the last
/// content event so a later resume can roll all trailing closing events back
/// and re-close or extend the blocks from the tail.
pub(super) struct EofTracking {
    /// State at the start of the current run of consecutive End events
    /// (the rollback point for an EOF-reached run).
    end_segment_start: Option<ParseState>,
    /// Whether the stream is inside a run of consecutive End events.
    pub(super) in_end_segment: bool,
    /// Rollback point: state after the last content event, captured at the
    /// first EOF-closing event (the trailing End segment).
    resume_state: Option<ParseState>,
    /// Span end of the most recent content event.
    last_content_end: usize,
    /// End-event spans of the current trailing End run (pending until the
    /// run is confirmed to reach EOF).
    pub(super) pending_ends: Vec<(usize, usize)>,
    /// End-event spans rolled back into the resume state; resumed parses
    /// replay these events when their span extends into the tail.
    rolled_back_ends: Vec<(usize, usize)>,
    /// State right after the last content event (blocks length, accumulated
    /// text, context). This is the rollback point: everything after it
    /// (closing events, and any opening events of an empty trailing block)
    /// is rolled back so the tail can extend or re-close the block.
    last_content_state: Option<(usize, String, BlockContext)>,
}

impl EofTracking {
    fn new() -> Self {
        Self {
            end_segment_start: None,
            in_end_segment: false,
            resume_state: None,
            last_content_end: 0,
            pending_ends: Vec::new(),
            rolled_back_ends: Vec::new(),
            last_content_state: None,
        }
    }
}

/// Full parse of `text`. The checkpoint covers the whole text.
pub(super) fn parse_blocks(
    text: &str,
    width: usize,
    theme: &MarkdownTheme,
    hyperlinks_enabled: bool,
    default_style: &Option<DefaultTextStyle>,
) -> ParseOutcome {
    let mut state = ParseState::default();
    state.context.base_style = default_style.as_ref().map(|ds| ds.to_base_style());
    let mut tracking = EofTracking::new();
    let parser = Parser::new_ext(text, Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TABLES)
        .into_offset_iter();
    parse_events(
        parser,
        text,
        0,
        false,
        &mut state,
        &mut tracking,
        width,
        theme,
        hyperlinks_enabled,
    );
    finish_state(text, state, tracking)
}

/// Resume parsing of `text` (which must extend `checkpoint.text` by a tail)
/// from a stable checkpoint. Returns the updated blocks and checkpoint.
/// Falls back to a full parse when the tail changes the block structure in a
/// way the resumed event stream cannot represent.
pub(super) fn resume_blocks(
    text: &str,
    checkpoint: ParseCheckpoint,
    width: usize,
    theme: &MarkdownTheme,
    hyperlinks_enabled: bool,
    default_style: &Option<DefaultTextStyle>,
) -> ParseOutcome {
    debug_assert!(text.starts_with(&checkpoint.text));
    let prefix_len = checkpoint.text.len();
    let mut state = checkpoint.resume;
    let mut tracking = EofTracking {
        last_content_end: checkpoint.last_content_end,
        rolled_back_ends: checkpoint.rolled_back_ends.clone(),
        ..EofTracking::new()
    };
    let parser = Parser::new_ext(text, Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TABLES)
        .into_offset_iter();
    let resumed = parse_events(
        parser,
        text,
        prefix_len,
        checkpoint.ends_open,
        &mut state,
        &mut tracking,
        width,
        theme,
        hyperlinks_enabled,
    );
    if resumed {
        finish_state(text, state, tracking)
    } else {
        parse_blocks(text, width, theme, hyperlinks_enabled, default_style)
    }
}

/// Consume a parser event stream. With `prefix_len > 0` the stream resumes a
/// previous parse: events fully inside the prefix are skipped (except the
/// EOF-closing prefix events that were rolled back, which are replayed when
/// `replay_eof_prefix` is set), and events crossing the boundary are spliced.
#[allow(clippy::too_many_arguments)]
fn parse_events<'a>(
    parser: impl Iterator<Item = (Event<'a>, std::ops::Range<usize>)>,
    text: &str,
    prefix_len: usize,
    replay_eof_prefix: bool,
    state: &mut ParseState,
    tracking: &mut EofTracking,
    width: usize,
    theme: &MarkdownTheme,
    hyperlinks_enabled: bool,
) -> bool {
    let text_len = text.len();
    for (event, range) in parser {
        if prefix_len > 0 {
            let is_content = matches!(
                event,
                Event::Text(_) | Event::Code(_) | Event::SoftBreak | Event::HardBreak
            );
            // Content events are fully inside the previous parse when their
            // span ends at or before the last consumed content; other events
            // (Start/Rule/End) when their span stays within the previous text.
            let inside_old = if is_content {
                range.end <= tracking.last_content_end
            } else {
                range.end <= prefix_len
            };
            if inside_old {
                // Fully inside the text consumed by the previous parse.
                // End events still run the tracking logic (so a closing run
                // reaching EOF is rolled back into the new checkpoint), and
                // rolled-back closures whose span ends exactly at the prefix
                // boundary are replayed. Everything else is already reflected
                // in the resume state.
                if matches!(event, Event::End(_)) {
                    let replay = record_end(&event, &range, text_len, state, tracking);
                    if is_inline_end(&event)
                        || (replay && range.end == prefix_len && replay_eof_prefix)
                    {
                        process_event(event, state, width, theme, hyperlinks_enabled);
                    }
                } else {
                    end_segment_interrupted(tracking);
                }
                continue;
            }
            if range.start < tracking.last_content_end || (!is_content && range.start < prefix_len)
            {
                // Event extends content the previous parse already consumed
                // (splice the tail part), or is a structural event (Start /
                // Rule / End) the previous parse already handled. This can
                // also reveal that the block structure changed, which is not
                // resumable.
                if !splice_boundary_event(
                    event,
                    range,
                    tracking.last_content_end,
                    text_len,
                    state,
                    tracking,
                    width,
                    theme,
                    hyperlinks_enabled,
                ) {
                    return false;
                }
                continue;
            }
        }
        record_and_process(
            event,
            range,
            text_len,
            state,
            tracking,
            width,
            theme,
            hyperlinks_enabled,
        );
    }
    flush_current(
        &mut state.blocks,
        &mut state.current,
        &mut state.context,
        theme,
        hyperlinks_enabled,
    );
    true
}

/// Apply an event while tracking whether it closes an open block at EOF.
#[allow(clippy::too_many_arguments)]
fn record_and_process(
    event: Event<'_>,
    range: std::ops::Range<usize>,
    text_len: usize,
    state: &mut ParseState,
    tracking: &mut EofTracking,
    width: usize,
    theme: &MarkdownTheme,
    hyperlinks_enabled: bool,
) {
    if matches!(event, Event::End(_)) {
        record_end(&event, &range, text_len, state, tracking);
    } else {
        end_segment_interrupted(tracking);
        if matches!(
            event,
            Event::Text(_) | Event::Code(_) | Event::SoftBreak | Event::HardBreak
        ) {
            tracking.last_content_end = range.end;
        }
    }
    let is_content = matches!(
        event,
        Event::Text(_) | Event::Code(_) | Event::SoftBreak | Event::HardBreak
    );
    process_event(event, state, width, theme, hyperlinks_enabled);
    if is_content {
        tracking.last_content_state = Some((
            state.blocks.len(),
            state.current.clone(),
            state.context.clone(),
        ));
    }
}

/// Track a closing event: extends the current End run, and when the run
/// reaches EOF rolls it back into the resume state. Returns whether the
/// event was rolled back by the previous parse (its closure is pending and
/// its span is being extended by the tail).
fn record_end(
    event: &Event<'_>,
    range: &std::ops::Range<usize>,
    text_len: usize,
    state: &mut ParseState,
    tracking: &mut EofTracking,
) -> bool {
    if !tracking.in_end_segment {
        // Start of a run of closing events: the rollback point is the
        // state before this run, but only if the run reaches EOF.
        tracking.end_segment_start = Some(state.clone());
        tracking.in_end_segment = true;
    }
    tracking.pending_ends.push((range.start, range.end));
    if range.end == text_len && tracking.resume_state.is_none() {
        // First EOF-closing event: roll back this run's events so the tail
        // can extend or re-close the block. A trailing empty list item
        // leaves only its bullet in `current`; the tail starts a new item
        // (which pushes its own bullet), so the residual bullet is dropped.
        if let Some(mut end_start) = tracking.end_segment_start.take() {
            // A trailing empty list item leaves only its bullet in
            // `current`; the tail starts a new item (which pushes its own
            // bullet), so the residual bullet is dropped.
            let bullet_only = !end_start.current.is_empty()
                && end_start
                    .current
                    .trim_end()
                    .chars()
                    .all(|c| c == '-' || c.is_whitespace());
            if bullet_only {
                end_start.current.clear();
            }
            state.blocks = end_start.blocks;
            state.current = end_start.current;
            state.context = end_start.context;
        }
        tracking.resume_state = Some(state.clone());
        tracking.rolled_back_ends = std::mem::take(&mut tracking.pending_ends);
    }
    let _ = event;
    tracking
        .rolled_back_ends
        .iter()
        .any(|(start, _)| *start == range.start)
}

/// Splice an event crossing the prefix boundary into the resumed state.
#[allow(clippy::too_many_arguments)]
fn splice_boundary_event(
    event: Event<'_>,
    range: std::ops::Range<usize>,
    last_content_end: usize,
    text_len: usize,
    state: &mut ParseState,
    tracking: &mut EofTracking,
    width: usize,
    theme: &MarkdownTheme,
    hyperlinks_enabled: bool,
) -> bool {
    match event {
        Event::Text(text) => {
            // The tail slice continues the same source event whose earlier
            // part was already accumulated into `current` by the previous
            // parse; append it verbatim (no separator insertion).
            let cut = last_content_end.saturating_sub(range.start);
            state.current.push_str(&text[cut.min(text.len())..]);
            tracking.last_content_end = range.end;
            end_segment_interrupted(tracking);
            true
        }
        Event::Code(text) => {
            let cut = last_content_end.saturating_sub(range.start);
            let tail = &text[cut.min(text.len())..];
            let start = state.current.len();
            state.current.push_str(tail);
            state.context.inline_spans.push(InlineSpan {
                start,
                end: state.current.len(),
                kind: InlineKind::Code,
            });
            tracking.last_content_end = range.end;
            end_segment_interrupted(tracking);
            true
        }
        Event::Start(Tag::Table(_)) if state.context.table.is_none() => {
            // The table is only recognized now that its separator row
            // arrived; the previous parse treated these rows as a paragraph.
            false
        }
        Event::Start(Tag::CodeBlock(_)) if !state.context.in_code_block => false,
        Event::Start(Tag::Heading { .. }) if !state.context.heading => false,
        Event::Start(Tag::List(_)) if state.context.list_depth == 0 => false,
        Event::Start(Tag::BlockQuote(_)) if !state.context.in_quote => false,
        Event::Start(Tag::Item) => {
            // An empty trailing item leaves no content in `current`; the
            // tail fills it, so the item must be re-opened (flush + bullet).
            end_segment_interrupted(tracking);
            if state.current.is_empty() {
                process_event(event, state, width, theme, hyperlinks_enabled);
            }
            true
        }
        Event::Start(_) => {
            end_segment_interrupted(tracking);
            true
        }
        Event::End(_) => {
            // Inline closures generate spans from their open state and are
            // idempotent, so they are always re-applied. Block closures are
            // replayed only when they were rolled back into the resume state
            // (their flush is pending and the tail extended their span);
            // normally closed blocks were already flushed and are reflected
            // in the resume state, so they are skipped.
            if is_inline_end(&event) || record_end(&event, &range, text_len, state, tracking) {
                process_event(event, state, width, theme, hyperlinks_enabled);
            }
            true
        }
        Event::SoftBreak | Event::HardBreak => {
            tracking.last_content_end = range.end;
            end_segment_interrupted(tracking);
            process_event(event, state, width, theme, hyperlinks_enabled);
            true
        }
        Event::Rule => {
            end_segment_interrupted(tracking);
            true
        }
        _ => true,
    }
}

fn finish_state(text: &str, state: ParseState, tracking: EofTracking) -> ParseOutcome {
    let (resume, ends_open) = match tracking.resume_state {
        Some(open) => (open, true),
        None => (state.clone(), false),
    };
    ParseOutcome {
        blocks: state.blocks,
        checkpoint: ParseCheckpoint {
            text: text.to_string(),
            resume,
            last_content_end: tracking.last_content_end,
            rolled_back_ends: tracking.rolled_back_ends,
            ends_open,
        },
    }
}

#[allow(clippy::too_many_arguments)]
fn process_event(
    event: Event<'_>,
    state: &mut ParseState,
    width: usize,
    theme: &MarkdownTheme,
    hyperlinks_enabled: bool,
) {
    let ParseState {
        blocks,
        current,
        context,
    } = state;
    match event {
        Event::Start(Tag::Heading { .. }) => {
            flush_current(blocks, current, context, theme, hyperlinks_enabled);
            ensure_spacing(blocks, context);
            context.heading = true;
        }
        Event::End(TagEnd::Heading(_)) => {
            flush_current(blocks, current, context, theme, hyperlinks_enabled);
            context.last_block = Some(BlockKind::Heading);
        }
        Event::Start(Tag::Paragraph) => {
            ensure_spacing(blocks, context);
        }
        Event::End(TagEnd::Paragraph) => {
            flush_current(blocks, current, context, theme, hyperlinks_enabled);
            context.last_block = Some(BlockKind::Paragraph);
        }
        Event::Start(Tag::List(_)) => {
            context.list_depth += 1;
        }
        Event::End(TagEnd::List(_)) => {
            context.list_depth = context.list_depth.saturating_sub(1);
            flush_current(blocks, current, context, theme, hyperlinks_enabled);
            context.last_block = Some(BlockKind::List);
        }
        Event::Start(Tag::Item) => {
            flush_current(blocks, current, context, theme, hyperlinks_enabled);
            if context.list_depth > 0 {
                current.push_str("- ");
            }
        }
        Event::End(TagEnd::Item) => {
            flush_current(blocks, current, context, theme, hyperlinks_enabled)
        }
        Event::Start(Tag::BlockQuote(_)) => {
            if context.list_depth > 0 {
                // Inside a list item: keep the list bullet in current.
                // Don't add "> " — the quote styling (dim) will be applied
                // by style_block at flush time.
                context.in_quote = true;
            } else {
                flush_current(blocks, current, context, theme, hyperlinks_enabled);
                ensure_spacing(blocks, context);
                context.in_quote = true;
                current.push_str("> ");
            }
        }
        Event::End(TagEnd::BlockQuote(_)) => {
            flush_current(blocks, current, context, theme, hyperlinks_enabled);
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
                flush_current(blocks, current, context, theme, hyperlinks_enabled);
                ensure_spacing(blocks, context);
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
                append_inline_text(current, &text, false);
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
                flush_current(blocks, current, context, theme, hyperlinks_enabled);
            }
        }
        Event::Rule => {
            flush_current(blocks, current, context, theme, hyperlinks_enabled);
            ensure_spacing(blocks, context);
            blocks.push(paint_markdown(&"-".repeat(width.min(20)), &theme.hr));
            context.last_block = Some(BlockKind::Hr);
        }
        // ── Table events ──────────────────────────────────────────
        Event::Start(Tag::Table(alignments)) => {
            flush_current(blocks, current, context, theme, hyperlinks_enabled);
            ensure_spacing(blocks, context);
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
                _clear_inline_tracking(context);
                context.in_table_cell = false;
            }
            current.clear();
            let base = context.base_style.as_ref();
            if let Some(table) = context.table.take() {
                render_table(&table, width, theme, hyperlinks_enabled, base, blocks);
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
                _clear_inline_tracking(context);
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
            _clear_inline_tracking(context);
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
                _clear_inline_tracking(context);
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
                _clear_inline_tracking(context);
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
            _clear_inline_tracking(context);
            context.in_table_cell = false;
        }
        _ => {}
    }
}
