use gpui::{
    ElementId, EventEmitter, IntoElement, ParentElement as _, Render, SharedString, Styled as _,
    WeakEntity, Window, div, prelude::*, px, relative, rgb,
};
use gpui_component::v_virtual_list;

use super::{
    ConversationBlockKind, NativeShell, conversation_block_visual, conversation_text_element,
    conversation_text_render_mode,
};
use desktop::shell::SemanticTheme;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ConversationPaneEvent {
    Select { block_id: String, durable: bool },
}

pub(super) struct ConversationPane {
    owner: WeakEntity<NativeShell>,
}

impl ConversationPane {
    pub(super) fn new(owner: WeakEntity<NativeShell>) -> Self {
        Self { owner }
    }
}

impl EventEmitter<ConversationPaneEvent> for ConversationPane {}

impl Render for ConversationPane {
    fn render(&mut self, _window: &mut Window, cx: &mut gpui::Context<Self>) -> impl IntoElement {
        let Some(owner) = self.owner.upgrade() else {
            return div().flex_1();
        };
        let (transcript_rows, scroll_handle) = {
            let owner = owner.read(cx);
            (
                owner.conversation_row_sizes.clone(),
                owner.conversation_scroll.clone(),
            )
        };
        let transcript_list = v_virtual_list(
            cx.entity(),
            "conversation-transcript",
            transcript_rows,
            |this, visible_range, window, cx| {
                let Some(owner) = this.owner.upgrade() else {
                    return Vec::new();
                };
                visible_range
                    .filter_map(|index| {
                        let (block, row_height, selected) = {
                            let owner = owner.read(cx);
                            let block = owner.conversation_render_rows.get(index)?.clone();
                            let height = owner
                                .conversation_render_heights
                                .get(index)
                                .copied()
                                .unwrap_or(block.measured_height);
                            let selected = owner.conversation_viewport.selected_block_id()
                                == Some(block.row_id.as_ref());
                            (block, height, selected)
                        };
                        let block_id = block.row_id.to_string();
                        let durable = block.durable;
                        let markdown_id = ElementId::Name(SharedString::new(
                            block.markdown_state_key.clone(),
                        ));
                        let detail_markdown_id = ElementId::Name(SharedString::new(
                            block.detail_markdown_state_key.clone(),
                        ));
                        let text_render_mode = conversation_text_render_mode(block.done);
                        let text = block.text.clone();
                        let detail_text = block.detail.clone();
                        let theme = SemanticTheme::GEEK_DARK;
                        let visual = conversation_block_visual(block.kind, block.is_error, theme);
                        let is_assistant = block.kind == ConversationBlockKind::Assistant;
                        let is_tool = block.kind == ConversationBlockKind::Tool;
                        let terminal_label = if block.is_error {
                            Some("failed")
                        } else if !block.done {
                            Some("streaming")
                        } else {
                            None
                        };
                        Some(
                            div()
                                .id(("conversation-block", index))
                                .h(px(row_height))
                                .px_4()
                                .py_1()
                                .flex()
                                .items_start()
                                .when(visual.align_right, |row| row.justify_end())
                                .on_click(cx.listener(move |_, _, _, cx| {
                                    cx.emit(ConversationPaneEvent::Select {
                                        block_id: block_id.clone(),
                                        durable,
                                    });
                                }))
                                .child(
                                    div()
                                        .w_full()
                                        .h_full()
                                        .when(visual.align_right, |card| card.w(relative(0.82)))
                                        .overflow_hidden()
                                        .rounded_lg()
                                        .border_1()
                                        .border_color(if selected {
                                            rgb(theme.focus_ring.value())
                                        } else {
                                            rgb(visual.accent.value())
                                        })
                                        .bg(rgb(visual.surface.value()))
                                        .px_4()
                                        .py_3()
                                        .flex()
                                        .flex_col()
                                        .gap_2()
                                        .child(
                                            div()
                                                .flex()
                                                .items_center()
                                                .justify_between()
                                                .child(
                                                    div()
                                                        .flex()
                                                        .items_center()
                                                        .gap_2()
                                                        .child(
                                                            div()
                                                                .px_2()
                                                                .py_1()
                                                                .rounded_md()
                                                                .bg(rgb(theme.elevated.value()))
                                                                .text_xs()
                                                                .font_weight(
                                                                    gpui::FontWeight::SEMIBOLD,
                                                                )
                                                                .text_color(rgb(
                                                                    visual.accent.value(),
                                                                ))
                                                                .child(visual.glyph),
                                                        )
                                                        .child(
                                                            div()
                                                                .text_sm()
                                                                .font_weight(
                                                                    gpui::FontWeight::MEDIUM,
                                                                )
                                                                .text_color(rgb(theme.text.value()))
                                                                .child(SharedString::new(
                                                                    block.title.clone(),
                                                                )),
                                                        ),
                                                )
                                                .when_some(terminal_label, |header, label| {
                                                    header.child(
                                                        div()
                                                            .text_xs()
                                                            .text_color(rgb(visual.accent.value()))
                                                            .child(label),
                                                    )
                                                }),
                                        )
                                        .when(is_assistant && !detail_text.is_empty(), |card| {
                                            card.child(
                                                div()
                                                    .rounded_md()
                                                    .border_l_3()
                                                    .border_color(rgb(theme.focus_ring.value()))
                                                    .bg(rgb(theme.thinking_surface.value()))
                                                    .px_3()
                                                    .py_2()
                                                    .flex()
                                                    .flex_col()
                                                    .gap_1()
                                                    .child(
                                                        div()
                                                            .text_xs()
                                                            .font_weight(
                                                                gpui::FontWeight::SEMIBOLD,
                                                            )
                                                            .text_color(rgb(
                                                                theme.focus_ring.value(),
                                                            ))
                                                            .child("◇ REASONING"),
                                                    )
                                                    .child(conversation_text_element(
                                                        detail_markdown_id.clone(),
                                                        detail_text.clone(),
                                                        text_render_mode,
                                                        window,
                                                        cx,
                                                    )),
                                            )
                                        })
                                        .when(!text.is_empty(), |card| {
                                            card.child(conversation_text_element(
                                                markdown_id,
                                                text,
                                                text_render_mode,
                                                window,
                                                cx,
                                            ))
                                        })
                                        .when(!is_assistant && !detail_text.is_empty(), |card| {
                                            card.child(
                                                div()
                                                    .mt_1()
                                                    .rounded_md()
                                                    .border_1()
                                                    .border_color(rgb(theme.border.value()))
                                                    .bg(rgb(theme.canvas.value()))
                                                    .px_3()
                                                    .py_2()
                                                    .when(is_tool, |detail| {
                                                        detail.font_family("monospace").text_xs()
                                                    })
                                                    .text_color(rgb(theme.muted_text.value()))
                                                    .child(conversation_text_element(
                                                        detail_markdown_id,
                                                        detail_text,
                                                        text_render_mode,
                                                        window,
                                                        cx,
                                                    )),
                                            )
                                        })
                                        .when(block.preview_truncated, |card| {
                                            card.child(
                                                div().text_color(rgb(theme.warning.value())).child(
                                                    "! preview truncated at desktop safety limit",
                                                ),
                                            )
                                        })
                                        .when(block.media_neutralized, |card| {
                                            card.child(
                                                div()
                                                    .text_color(rgb(theme.muted_text.value()))
                                                    .child(
                                                        "remote/inline media disabled in transcript",
                                                    ),
                                            )
                                        })
                                        .when(block.image_count > 0, |card| {
                                            card.child(format!(
                                                "▧ {} retained image attachment(s)",
                                                block.image_count
                                            ))
                                        }),
                                ),
                        )
                    })
                    .collect::<Vec<_>>()
            },
        )
        .track_scroll(&scroll_handle);

        div().flex_1().min_h_0().child(transcript_list)
    }
}
