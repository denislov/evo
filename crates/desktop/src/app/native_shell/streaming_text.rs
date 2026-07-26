use std::sync::Arc;

use desktop::conversation::StreamingTextPhase;
use gpui::{
    AnyElement, ElementId, IntoElement as _, ParentElement as _, SharedString, Styled as _, Window,
    div,
};
use gpui_component::text::TextView;

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
                TextView::markdown(self.id, text, window, cx).into_any_element()
            }
        }
    }
}
