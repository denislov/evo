use gpui::{
    ElementId, EventEmitter, IntoElement, ParentElement as _, Render, SharedString, Styled as _,
    WeakEntity, Window, div, prelude::*, px, relative, rgb,
};
use gpui_component::{button::Button, v_virtual_list};

use super::{
    ConversationBlockKind, NativeShell, conversation_block_visual, streaming_text::StreamingText,
};
use desktop::conversation::compact_duration;
use desktop::shell::{
    ASSISTANT_MESSAGE_MAX_WIDTH, MONOSPACE_FONT_FAMILY, SemanticTheme, USER_MESSAGE_MAX_WIDTH,
    USER_MESSAGE_WIDTH_PERCENT,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ConversationPaneEvent {
    Select { block_id: String, durable: bool },
    Copy { block_id: String },
    ToggleDetails { block_id: String },
    Scrolled,
    FollowLatest,
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
        let (
            transcript_rows,
            scroll_handle,
            visible_count,
            event_count,
            message_count,
            tool_count,
            omitted_count,
            follow_latest,
            unseen_updates,
        ) = {
            let owner = owner.read(cx);
            (
                owner.conversation_row_sizes.clone(),
                owner.conversation_scroll.clone(),
                owner.visible_conversation_count(),
                owner.projection.recent_events().len(),
                owner.projection.messages().len(),
                owner.projection.tools().len(),
                owner.projection.conversation().omitted_blocks(),
                owner.conversation_viewport.follow_latest(),
                owner.conversation_viewport.unseen_updates(),
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
                        let (block, row_height, selected, detail_expanded) = {
                            let owner = owner.read(cx);
                            let block = owner.conversation_render_rows.get(index)?.clone();
                            let height = owner
                                .conversation_render_heights
                                .get(index)
                                .copied()
                                .unwrap_or(block.measured_height);
                            let selected = owner.conversation_viewport.selected_block_id()
                                == Some(block.item_key.row_id());
                            let detail_expanded = owner
                                .conversation_expanded_details
                                .contains(block.item_key.row_id());
                            (block, height, selected, detail_expanded)
                        };
                        let block_id = block.item_key.row_id().to_owned();
                        let copy_block_id = block_id.clone();
                        let toggle_block_id = block_id.clone();
                        let reasoning_toggle_block_id = block_id.clone();
                        let tool_toggle_block_id = block_id.clone();
                        let hover_group = SharedString::new(format!(
                            "conversation-card:{}",
                            block.item_key.stable_id()
                        ));
                        let durable = block.durable;
                        let markdown_id = ElementId::Name(SharedString::new(
                            block.markdown_state_key.clone(),
                        ));
                        let detail_markdown_id = ElementId::Name(SharedString::new(
                            block.detail_markdown_state_key.clone(),
                        ));
                        let text_phase = block.text_phase;
                        let text = block.text.clone();
                        let detail_text = block.detail.clone();
                        let theme = SemanticTheme::GEEK_DARK;
                        let visual = conversation_block_visual(block.kind, block.is_error, theme);
                        let card_border = if selected {
                            theme.focus_ring
                        } else if block.is_error || block.kind == ConversationBlockKind::Diagnostic {
                            theme.danger
                        } else {
                            theme.border
                        };
                        let is_assistant = block.kind == ConversationBlockKind::Assistant;
                        let reasoning_duration_label = block
                            .reasoning_duration_millis
                            .map(compact_duration);
                        let is_tool = block.kind == ConversationBlockKind::Tool;
                        let has_collapsible_detail = (is_assistant && !detail_text.is_empty())
                            || (is_tool && (!text.is_empty() || !detail_text.is_empty()));
                        let terminal_label = if block.is_error {
                            Some("failed")
                        } else if is_tool && block.done {
                            Some("completed")
                        } else if !block.done {
                            Some(if is_tool { "running" } else { "streaming" })
                        } else {
                            None
                        };
                        Some(
                            div()
                                .id((
                                    ElementId::from("conversation-block"),
                                    SharedString::new(block.item_key.stable_id_arc()),
                                ))
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
                                        .relative()
                                        .group(hover_group.clone())
                                        .w_full()
                                        .h_full()
                                        .when(visual.align_right, |card| {
                                            card.w(relative(
                                                USER_MESSAGE_WIDTH_PERCENT as f32 / 100.,
                                            ))
                                                .max_w(px(USER_MESSAGE_MAX_WIDTH as f32))
                                        })
                                        .when(!visual.align_right, |card| {
                                            card.max_w(px(ASSISTANT_MESSAGE_MAX_WIDTH as f32))
                                        })
                                        .overflow_hidden()
                                        .rounded_lg()
                                        .border_l_2()
                                        .border_color(rgb(card_border.value()))
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
                                                .gap_3()
                                                .pr_24()
                                                .child(
                                                    div()
                                                        .flex()
                                                        .flex_1()
                                                        .min_w_0()
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
                                                            .flex_shrink_0()
                                                            .text_xs()
                                                            .text_color(rgb(visual.accent.value()))
                                                            .child(label),
                                                    )
                                                }),
                                        )
                                        .when(
                                            is_assistant
                                                && !detail_text.is_empty()
                                                && !detail_expanded,
                                            |card| {
                                                card.child(
                                                    div()
                                                        .rounded_md()
                                                        .border_l_3()
                                                        .border_color(rgb(theme.reasoning.value()))
                                                        .bg(rgb(theme.thinking_surface.value()))
                                                        .px_3()
                                                        .py_2()
                                                        .flex()
                                                        .items_center()
                                                        .justify_between()
                                                        .text_xs()
                                                        .text_color(rgb(theme.reasoning.value()))
                                                        .child(if let Some(duration) = &reasoning_duration_label {
                                                            SharedString::new(format!(
                                                                "◇ Reasoning · {duration} · collapsed"
                                                            ))
                                                        } else if block.done {
                                                            SharedString::new("◇ Reasoning · collapsed")
                                                        } else {
                                                            SharedString::new(
                                                                "◇ Reasoning · streaming · collapsed",
                                                            )
                                                        })
                                                        .child(
                                                            Button::new((
                                                                "show-reasoning",
                                                                index,
                                                            ))
                                                            .compact()
                                                            .label("Show")
                                                            .tooltip("Show reasoning details")
                                                            .on_click(cx.listener(
                                                                move |_, _, _, cx| {
                                                                    cx.stop_propagation();
                                                                    cx.emit(
                                                                        ConversationPaneEvent::ToggleDetails {
                                                                            block_id: reasoning_toggle_block_id.clone(),
                                                                        },
                                                                    );
                                                                },
                                                            )),
                                                        ),
                                                )
                                            },
                                        )
                                        .when(
                                            is_assistant
                                                && !detail_text.is_empty()
                                                && detail_expanded,
                                            |card| {
                                            card.child(
                                                div()
                                                    .rounded_md()
                                                    .border_l_3()
                                                    .border_color(rgb(theme.reasoning.value()))
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
                                                                theme.reasoning.value(),
                                                            ))
                                                            .child(match &reasoning_duration_label {
                                                                Some(duration) => SharedString::new(
                                                                    format!("◇ REASONING · {duration}"),
                                                                ),
                                                                None => SharedString::new(
                                                                    "◇ REASONING",
                                                                ),
                                                            }),
                                                    )
                                                    .child(
                                                        StreamingText::new(
                                                            detail_markdown_id.clone(),
                                                            detail_text.clone(),
                                                            text_phase,
                                                        )
                                                        .into_any_element(window, cx),
                                                    ),
                                            )
                                        },
                                        )
                                        .when(!text.is_empty() && (!is_tool || detail_expanded), |card| {
                                            card.child(
                                                StreamingText::new(
                                                    markdown_id,
                                                    text,
                                                    text_phase,
                                                )
                                                .into_any_element(window, cx),
                                            )
                                        })
                                        .when(
                                            !is_assistant
                                                && !detail_text.is_empty()
                                                && (!is_tool || detail_expanded),
                                            |card| {
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
                                                        detail
                                                            .font_family(MONOSPACE_FONT_FAMILY)
                                                            .text_xs()
                                                    })
                                                    .text_color(rgb(theme.muted_text.value()))
                                                    .child(
                                                        StreamingText::new(
                                                            detail_markdown_id,
                                                            detail_text,
                                                            text_phase,
                                                        )
                                                        .into_any_element(window, cx),
                                                    ),
                                            )
                                        },
                                        )
                                        .when(is_tool && has_collapsible_detail && !detail_expanded, |card| {
                                            card.child(
                                                div()
                                                    .rounded_md()
                                                    .bg(rgb(theme.canvas.value()))
                                                    .px_3()
                                                    .py_2()
                                                    .flex()
                                                    .items_center()
                                                    .justify_between()
                                                    .font_family(MONOSPACE_FONT_FAMILY)
                                                    .text_xs()
                                                    .text_color(rgb(theme.muted_text.value()))
                                                    .child(if block.done {
                                                        "output + arguments collapsed"
                                                    } else {
                                                        "running · output + arguments collapsed"
                                                    })
                                                    .child(
                                                        Button::new(("show-tool-details", index))
                                                            .compact()
                                                            .label("Show")
                                                            .tooltip("Show tool output and arguments")
                                                            .on_click(cx.listener(
                                                                move |_, _, _, cx| {
                                                                    cx.stop_propagation();
                                                                    cx.emit(
                                                                        ConversationPaneEvent::ToggleDetails {
                                                                            block_id: tool_toggle_block_id.clone(),
                                                                        },
                                                                    );
                                                                },
                                                            )),
                                                    ),
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
                                        })
                                        .child(
                                            div()
                                                .absolute()
                                                .top_2()
                                                .right_2()
                                                .flex()
                                                .gap_1()
                                                .invisible()
                                                .group_hover(hover_group, |style| style.visible())
                                                .when(selected, |actions| actions.visible())
                                                .when(has_collapsible_detail, |actions| {
                                                    actions.child(
                                                        Button::new((
                                                            "toggle-conversation-details",
                                                            index,
                                                        ))
                                                        .compact()
                                                        .label(if detail_expanded {
                                                            "Hide"
                                                        } else {
                                                            "More"
                                                        })
                                                        .tooltip(
                                                            "Show or hide secondary message details",
                                                        )
                                                        .on_click(cx.listener(
                                                            move |_, _, _, cx| {
                                                                cx.stop_propagation();
                                                                cx.emit(
                                                                    ConversationPaneEvent::ToggleDetails {
                                                                        block_id: toggle_block_id.clone(),
                                                                    },
                                                                );
                                                            },
                                                        )),
                                                    )
                                                })
                                                .child(
                                                    Button::new(("copy-conversation-row", index))
                                                        .compact()
                                                        .label("Copy")
                                                        .tooltip("Copy this bounded message")
                                                        .on_click(cx.listener(
                                                            move |_, _, _, cx| {
                                                                cx.stop_propagation();
                                                                cx.emit(
                                                                    ConversationPaneEvent::Copy {
                                                                        block_id: copy_block_id
                                                                            .clone(),
                                                                    },
                                                                );
                                                            },
                                                        )),
                                                ),
                                        ),
                                ),
                        )
                    })
                    .collect::<Vec<_>>()
            },
        )
        .track_scroll(&scroll_handle);

        let theme = SemanticTheme::GEEK_DARK;
        let follow_latest_label = if unseen_updates == 0 {
            "Latest ↓".to_owned()
        } else {
            format!("↓ {unseen_updates} new")
        };
        div()
            .relative()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .when(visible_count == 0, |content| {
                content.child(
                    div()
                        .p_5()
                        .flex()
                        .flex_col()
                        .gap_3()
                        .text_color(rgb(theme.muted_text.value()))
                        .child("Native runtime connected")
                        .child("No durable conversation blocks yet.")
                        .child(
                            div()
                                .font_family(MONOSPACE_FONT_FAMILY)
                                .flex()
                                .flex_col()
                                .gap_1()
                                .child(format!("project events   {event_count}"))
                                .child(format!("message overlays {message_count}"))
                                .child(format!("tool overlays    {tool_count}")),
                        ),
                )
            })
            .when(visible_count > 0, |content| {
                content
                    .when(omitted_count > 0, |content| {
                        content.child(
                            div()
                                .px_4()
                                .py_2()
                                .text_color(rgb(theme.warning.value()))
                                .child(format!(
                                    "{omitted_count} older blocks omitted by desktop retention bounds"
                                )),
                        )
                    })
                    .child(
                        div()
                            .id("conversation-scroll-region")
                            .flex_1()
                            .min_h_0()
                            .on_scroll_wheel(cx.listener(|_, _, _, cx| {
                                cx.emit(ConversationPaneEvent::Scrolled);
                            }))
                            .child(transcript_list),
                    )
                    .when(!follow_latest, |content| {
                        content.child(
                            div()
                                .absolute()
                                .right_4()
                                .bottom_4()
                                .rounded_lg()
                                .border_1()
                                .border_color(rgb(theme.accent.value()))
                                .bg(rgb(theme.elevated.value()))
                                .child(
                                    Button::new("follow-latest")
                                        .compact()
                                        .label(follow_latest_label.clone())
                                        .tooltip("Jump to latest output · End")
                                        .on_click(cx.listener(|_, _, _, cx| {
                                            cx.emit(ConversationPaneEvent::FollowLatest);
                                        })),
                                ),
                        )
                    })
            })
    }
}
