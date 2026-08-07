use super::*;

pub(super) fn follow_latest_label(unseen_updates: usize) -> String {
    if unseen_updates == 0 {
        "Latest ↓".to_owned()
    } else {
        format!("↓ {unseen_updates} new")
    }
}

pub(super) fn empty_conversation(
    event_count: usize,
    message_count: usize,
    tool_count: usize,
    theme: SemanticTheme,
) -> gpui::Div {
    div()
        .p_token(DesignSpace::Xl)
        .flex()
        .flex_col()
        .gap_token(DesignSpace::Md)
        .text_color(rgb(theme.muted_text.value()))
        .child("Native runtime connected")
        .child("No durable conversation blocks yet.")
        .child(
            div()
                .font_family(MONOSPACE_FONT_FAMILY)
                .flex()
                .flex_col()
                .gap_token(DesignSpace::Xs)
                .child(format!("project events   {event_count}"))
                .child(format!("message overlays {message_count}"))
                .child(format!("tool overlays    {tool_count}")),
        )
}
