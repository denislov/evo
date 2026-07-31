use std::sync::{Arc, OnceLock};
use std::time::Instant;

use desktop::ui::conversation::StreamingTextPhase;
use gpui::{
    AnyElement, ClipboardItem, Entity, InteractiveElement as _, IntoElement, ParentElement as _,
    SharedString, Styled as _, WeakEntity,
};
use gpui_component::text::{TextView, TextViewState};

use super::controls::{DesktopIcon, DesktopIconButton};
use crate::ui::conversation::pane::{ConversationPane, ConversationPaneEvent};

const MARKDOWN_COMPLETION_TRACE_ENV: &str = "EVO_DESKTOP_MARKDOWN_TRACE";

/// Lightweight conversation text renderer driven by the row's revision phase.
///
/// `StreamingPlainText` rows are raw text and own no parse state. Markdown rows
/// render a [`TextViewState`] the pane owns and feeds, so the parsed document
/// survives between frames and streaming deltas extend it incrementally on a
/// background task rather than re-parsing it synchronously in every frame.
pub(crate) struct StreamingText {
    text: Arc<str>,
    phase: StreamingTextPhase,
    markdown: Option<Entity<TextViewState>>,
    event_target: WeakEntity<ConversationPane>,
}

impl StreamingText {
    pub(crate) fn new(
        text: Arc<str>,
        phase: StreamingTextPhase,
        markdown: Option<Entity<TextViewState>>,
        event_target: WeakEntity<ConversationPane>,
    ) -> Self {
        Self {
            text,
            phase,
            markdown,
            event_target,
        }
    }

    pub(crate) fn into_any_element(self) -> AnyElement {
        match (self.phase, self.markdown) {
            (StreamingTextPhase::StreamingPlainText, _) | (_, None) => gpui::div()
                .w_full()
                .whitespace_normal()
                .child(SharedString::new(self.text))
                .into_any_element(),
            (_, Some(state)) => markdown_element(&state, self.event_target),
        }
    }
}

fn markdown_element(
    state: &Entity<TextViewState>,
    event_target: WeakEntity<ConversationPane>,
) -> AnyElement {
    TextView::new(state)
        .w_full()
        .min_w_0()
        .selectable(true)
        .code_block_actions(move |code_block, _, _| {
            let code = code_block.code().to_string();
            let event_target = event_target.clone();
            DesktopIconButton::new(
                "copy-markdown-code",
                DesktopIcon::Copy,
                "Copy this code block",
            )
            .build()
            .debug_selector(|| "desktop-copy-markdown-code".into())
            .on_click(move |_, _, cx| {
                cx.write_to_clipboard(ClipboardItem::new_string(code.clone()));
                if let Some(target) = event_target.upgrade() {
                    target.update(cx, |_, cx| {
                        cx.emit(ConversationPaneEvent::CopyCodeCompleted);
                    });
                }
            })
        })
        .into_any_element()
}

/// Report the cost of the parse that just ran on the main thread.
///
/// Only full-replace parses are timed, because those are the ones
/// `TextViewState::set_text` runs synchronously; appends hand their work to a
/// background task and return immediately. The trace therefore still bounds the
/// blocking parse cost a frame can absorb, which is what the native performance
/// gate samples, and it now measures the parse itself rather than a layout
/// request that happened to contain one.
pub(crate) fn trace_markdown_parse(state_key: &str, bytes: usize, started_at: Instant) {
    if !markdown_completion_trace_enabled() {
        return;
    }
    let elapsed_micros = started_at.elapsed().as_micros();
    let phase = if state_key.contains(":final:") {
        "final"
    } else {
        "settling"
    };
    tracing::trace!(
        state_key,
        phase,
        bytes,
        parse_to_layout_us = elapsed_micros,
        "desktop.markdown.parse_complete"
    );
    eprintln!(
        "desktop_trace\tmarkdown_parse_complete\tstate_key={state_key}\tphase={phase}\tbytes={bytes}\tmarkdown_parse_to_layout_us={elapsed_micros}"
    );
}

pub(crate) fn markdown_completion_trace_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| {
        std::env::var(MARKDOWN_COMPLETION_TRACE_ENV)
            .is_ok_and(|value| value == "1" || value.eq_ignore_ascii_case("true"))
    })
}
