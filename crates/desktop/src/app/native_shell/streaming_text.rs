use std::cell::Cell;
use std::sync::{Arc, OnceLock};
use std::time::Instant;

use desktop::conversation::StreamingTextPhase;
use gpui::{
    AnyElement, App, Bounds, ClipboardItem, Element, ElementId, GlobalElementId,
    InspectorElementId, InteractiveElement as _, IntoElement, LayoutId, ParentElement as _, Pixels,
    SharedString, Styled as _, Window, div,
};
use gpui_component::{button::Button, text::TextView};

const MARKDOWN_COMPLETION_TRACE_ENV: &str = "EVO_DESKTOP_MARKDOWN_TRACE";

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

    pub(super) fn into_any_element(self, _window: &mut Window, _cx: &mut gpui::App) -> AnyElement {
        let text = SharedString::new(self.text);
        match self.phase {
            StreamingTextPhase::StreamingPlainText => div()
                .w_full()
                .whitespace_normal()
                .child(text)
                .into_any_element(),
            StreamingTextPhase::SettlingMarkdown | StreamingTextPhase::FinalMarkdown => {
                markdown_element(self.id, text)
            }
        }
    }
}

fn markdown_element(id: ElementId, text: SharedString) -> AnyElement {
    MarkdownCompletionTraceElement::new(id, text).into_any_element()
}

/// Transparent production wrapper around the real gpui-component Markdown view.
///
/// `TextView::markdown` performs a full-replace parse synchronously from its
/// `request_layout` implementation. Timing the delegated request therefore
/// produces a conservative parse-to-layout completion bound without forking the
/// component or moving parsing out of its production path.
struct MarkdownCompletionTraceElement {
    id: ElementId,
    state_key: SharedString,
    text: SharedString,
}

struct MarkdownCompletionTraceLayoutState {
    element: AnyElement,
}

impl MarkdownCompletionTraceElement {
    fn new(id: ElementId, text: SharedString) -> Self {
        let state_key = match &id {
            ElementId::Name(name) => name.clone(),
            _ => SharedString::new(format!("{id:?}")),
        };
        Self {
            id,
            state_key,
            text,
        }
    }

    fn markdown(&self) -> AnyElement {
        TextView::markdown(self.id.clone(), self.text.clone())
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
}

impl IntoElement for MarkdownCompletionTraceElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for MarkdownCompletionTraceElement {
    type RequestLayoutState = MarkdownCompletionTraceLayoutState;
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let mut element = self.markdown();
        if !markdown_completion_trace_enabled() {
            let layout_id = element.request_layout(window, cx);
            return (layout_id, MarkdownCompletionTraceLayoutState { element });
        }

        let trace_state_key = SharedString::new(format!(
            "desktop-markdown-completion-trace:{}",
            self.state_key
        ));
        let completed = window.use_keyed_state(trace_state_key, cx, |_, _| Cell::new(false));
        let should_trace = !completed.read(cx).get();
        let started_at = should_trace.then(Instant::now);
        let layout_id = element.request_layout(window, cx);

        if let Some(started_at) = started_at {
            let elapsed_micros = started_at.elapsed().as_micros();
            let phase = if self.state_key.contains(":final:") {
                "final"
            } else {
                "settling"
            };
            // Interior mutation avoids scheduling another frame solely for
            // instrumentation state after this one-shot completion.
            completed.read(cx).set(true);
            tracing::trace!(
                state_key = self.state_key.as_ref(),
                phase,
                bytes = self.text.len(),
                parse_to_layout_us = elapsed_micros,
                "desktop.markdown.parse_complete"
            );
            eprintln!(
                "desktop_trace\tmarkdown_parse_complete\tstate_key={}\tphase={}\tbytes={}\tmarkdown_parse_to_layout_us={}",
                self.state_key,
                phase,
                self.text.len(),
                elapsed_micros
            );
        }

        (layout_id, MarkdownCompletionTraceLayoutState { element })
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) {
        request_layout.element.prepaint(window, cx);
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        request_layout: &mut Self::RequestLayoutState,
        _: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        request_layout.element.paint(window, cx);
    }
}

fn markdown_completion_trace_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| {
        std::env::var(MARKDOWN_COMPLETION_TRACE_ENV)
            .is_ok_and(|value| value == "1" || value.eq_ignore_ascii_case("true"))
    })
}
