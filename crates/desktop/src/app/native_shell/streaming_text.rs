use std::sync::Arc;

use desktop::conversation::StreamingTextPhase;
use gpui::{
    AnyElement, ClipboardItem, ElementId, InteractiveElement as _, IntoElement as _,
    ParentElement as _, SharedString, Styled as _, Window, div,
};
use gpui_component::{button::Button, text::TextView};

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
            StreamingTextPhase::SettlingMarkdown | StreamingTextPhase::FinalMarkdown => {
                TextView::markdown(self.id, text, window, cx)
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
    }
}
