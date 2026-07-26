use std::sync::Arc;
use std::time::Duration;

use desktop::conversation::StreamingTextPhase;
use gpui::{
    AnyElement, ClipboardItem, ElementId, InteractiveElement as _, IntoElement as _,
    ParentElement as _, SharedString, Styled as _, Timer, Window, div, px,
};
use gpui_component::{button::Button, text::TextView};

// gpui-component 0.5.1 debounces TextView updates for 200 ms. Its public API
// has no parse-completion callback, so retain the plain-text fallback for twice
// that fixed debounce; TextView still notifies its parent if parsing runs longer.
pub(super) const MARKDOWN_BACKGROUND_WAIT: Duration = Duration::from_millis(400);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MarkdownLoadPhase {
    Prewarm,
    PrewarmScheduled,
    Parsing,
    ParsingScheduled,
    Ready,
}

struct MarkdownLoadState {
    phase: MarkdownLoadPhase,
    parse_requests: u64,
}

impl MarkdownLoadState {
    fn new() -> Self {
        Self {
            phase: MarkdownLoadPhase::Prewarm,
            parse_requests: 0,
        }
    }

    fn schedule_prewarm(&mut self) -> bool {
        if self.phase != MarkdownLoadPhase::Prewarm {
            return false;
        }
        self.phase = MarkdownLoadPhase::PrewarmScheduled;
        true
    }

    fn finish_prewarm(&mut self) {
        if self.phase == MarkdownLoadPhase::PrewarmScheduled {
            self.phase = MarkdownLoadPhase::Parsing;
        }
    }

    fn schedule_parse(&mut self) -> bool {
        if self.phase != MarkdownLoadPhase::Parsing {
            return false;
        }
        self.phase = MarkdownLoadPhase::ParsingScheduled;
        self.parse_requests = self.parse_requests.saturating_add(1);
        true
    }

    fn finish_parse(&mut self) {
        if self.phase == MarkdownLoadPhase::ParsingScheduled {
            self.phase = MarkdownLoadPhase::Ready;
        }
    }
}

/// Lightweight conversation text renderer driven by the row's revision phase.
pub(super) struct StreamingText {
    id: ElementId,
    text: Arc<str>,
    phase: StreamingTextPhase,
}

impl StreamingText {
    pub(super) fn new(id: ElementId, text: Arc<str>, phase: StreamingTextPhase) -> Self {
        Self { id, text, phase }
    }

    pub(super) fn into_any_element(self, window: &mut Window, cx: &mut gpui::App) -> AnyElement {
        let text = SharedString::new(self.text);
        match self.phase {
            StreamingTextPhase::StreamingPlainText => div()
                .w_full()
                .whitespace_normal()
                .child(text)
                .into_any_element(),
            StreamingTextPhase::SettlingMarkdown => {
                deferred_markdown_element(self.id, text, false, window, cx)
            }
            StreamingTextPhase::FinalMarkdown => {
                deferred_markdown_element(self.id, text, true, window, cx)
            }
        }
    }
}

fn deferred_markdown_element(
    id: ElementId,
    text: SharedString,
    final_state: bool,
    window: &mut Window,
    cx: &mut gpui::App,
) -> AnyElement {
    let load_state = window.use_keyed_state(
        SharedString::from(format!("{id}/desktop-markdown-load")),
        cx,
        |_, _| MarkdownLoadState::new(),
    );
    let phase = load_state.read(cx).phase;

    if phase == MarkdownLoadPhase::Prewarm
        && load_state.update(cx, |state, _| state.schedule_prewarm())
    {
        let load_state = load_state.clone();
        cx.defer(move |cx| {
            load_state.update(cx, |state, cx| {
                state.finish_prewarm();
                cx.notify();
            });
        });
    }

    let phase = load_state.read(cx).phase;
    if phase == MarkdownLoadPhase::Parsing
        && load_state.update(cx, |state, _| state.schedule_parse())
    {
        tracing::trace!(
            input_bytes = text.len(),
            final_state,
            "desktop.markdown.parse_request"
        );
        let load_state = load_state.clone();
        cx.spawn(async move |cx| {
            Timer::after(MARKDOWN_BACKGROUND_WAIT).await;
            let _ = cx.update(|cx| {
                load_state.update(cx, |state, cx| {
                    state.finish_parse();
                    cx.notify();
                });
            });
        })
        .detach();
    }

    match load_state.read(cx).phase {
        MarkdownLoadPhase::Ready => markdown_element(id, text, window, cx),
        MarkdownLoadPhase::Prewarm | MarkdownLoadPhase::PrewarmScheduled => {
            plain_with_hidden_markdown(id, text, SharedString::default(), window, cx)
        }
        MarkdownLoadPhase::Parsing | MarkdownLoadPhase::ParsingScheduled => {
            plain_with_hidden_markdown(id, text.clone(), text, window, cx)
        }
    }
}

fn plain_with_hidden_markdown(
    id: ElementId,
    fallback: SharedString,
    markdown: SharedString,
    window: &mut Window,
    cx: &mut gpui::App,
) -> AnyElement {
    div()
        .w_full()
        .whitespace_normal()
        .child(fallback)
        .child(
            div()
                .h(px(0.))
                .overflow_hidden()
                .invisible()
                .child(markdown_element(id, markdown, window, cx)),
        )
        .into_any_element()
}

fn markdown_element(
    id: ElementId,
    text: SharedString,
    window: &mut Window,
    cx: &mut gpui::App,
) -> AnyElement {
    TextView::markdown(id, text, window, cx)
        .code_block_actions(|code_block, _, _| {
            let code = code_block.code().to_string();
            Button::new("copy-markdown-code")
                .debug_selector(|| "desktop-copy-markdown-code".into())
                .compact()
                .label("Copy code")
                .tooltip("Copy this code block")
                .on_click(move |_, _, cx| {
                    cx.write_to_clipboard(ClipboardItem::new_string(code.clone()));
                })
        })
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markdown_load_schedules_one_background_parse() {
        let mut state = MarkdownLoadState::new();
        assert!(state.schedule_prewarm());
        assert!(!state.schedule_prewarm());
        state.finish_prewarm();
        assert_eq!(state.phase, MarkdownLoadPhase::Parsing);
        assert!(state.schedule_parse());
        assert!(!state.schedule_parse());
        assert_eq!(state.parse_requests, 1);
        state.finish_parse();
        assert_eq!(state.phase, MarkdownLoadPhase::Ready);
        assert_eq!(state.parse_requests, 1);
    }
}
