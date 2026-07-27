use gpui::{
    ElementId, EventEmitter, IntoElement, ParentElement as _, Render, Role, SharedString,
    Styled as _, WeakEntity, Window, div, prelude::*, px, relative, rgb,
};
use gpui_component::{ElementExt as _, button::Button, v_virtual_list};

use super::{
    ConversationBlockKind, NativeShell, conversation_block_visual,
    desktop_style::{DesignRadius, DesignSpace, DesignText, DesktopStyledExt as _},
    streaming_text::StreamingText,
};
use desktop::conversation::{
    ConversationRowMeasurement, TRANSCRIPT_COLLAPSED_PREVIEW_MAX_HEIGHT, compact_duration,
};
use desktop::projection::DesktopRecoveryStatus;
use desktop::runtime::{DesktopRecoveryAction, DesktopRecoveryIdentity};
use desktop::shell::{
    ASSISTANT_MESSAGE_MAX_WIDTH, CONVERSATION_ROW_VERTICAL_PADDING_PX, MONOSPACE_FONT_FAMILY,
    SemanticTheme, USER_MESSAGE_MAX_WIDTH, USER_MESSAGE_WIDTH_PERCENT,
};

#[derive(Debug, Clone, PartialEq)]
pub(super) enum ConversationPaneEvent {
    Select {
        block_id: String,
        durable: bool,
    },
    Copy {
        block_id: String,
    },
    CopyToolCommand {
        block_id: String,
    },
    CopyToolOutput {
        block_id: String,
    },
    CopyCodeCompleted,
    ToggleDetails {
        block_id: String,
    },
    OpenFull {
        block_id: String,
    },
    OpenToolOutput {
        block_id: String,
    },
    Recovery {
        identity: DesktopRecoveryIdentity,
        action: DesktopRecoveryAction,
    },
    Measured(ConversationRowMeasurement),
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
            return div()
                .id("conversation-log")
                .role(Role::Log)
                .aria_label("Conversation messages")
                .flex_1();
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
                        let (
                            block,
                            row_height,
                            selected,
                            detail_expanded,
                            full_view_open,
                            diagnostic_recovery,
                            row_count,
                        ) = {
                            let owner = owner.read(cx);
                            let block = owner.conversation_render_rows.get(index)?.clone();
                            let height = owner
                                .conversation_render_heights
                                .get(index)
                                .copied()
                                .unwrap_or(block.estimated_height);
                            let selected = owner.conversation_viewport.selected_block_id()
                                == Some(block.item_key.row_id());
                            let detail_expanded = owner
                                .conversation_expanded_details
                                .contains(block.item_key.row_id());
                            let full_view_open = owner
                                .conversation_full_message
                                .as_ref()
                                .is_some_and(|message| {
                                    message.block_id == block.item_key.row_id()
                                });
                            let diagnostic_recovery = (block.kind
                                == ConversationBlockKind::Diagnostic)
                                .then(|| {
                                    owner.projection.recoveries().iter().find_map(|recovery| {
                                        (recovery.status == DesktopRecoveryStatus::Pending
                                            && recovery.authoritative)
                                            .then(|| recovery.identity.clone())
                                            .flatten()
                                    })
                                })
                                .flatten();
                            let row_count = owner.conversation_render_rows.len();
                            (
                                block,
                                height,
                                selected,
                                detail_expanded,
                                full_view_open,
                                diagnostic_recovery,
                                row_count,
                            )
                        };
                        let block_id = block.item_key.row_id().to_owned();
                        let select_block_id = block_id.clone();
                        let copy_block_id = block_id.clone();
                        let copy_tool_command_block_id = block_id.clone();
                        let copy_tool_output_block_id = block_id.clone();
                        let toggle_block_id = block_id.clone();
                        let reasoning_toggle_block_id = block_id.clone();
                        let tool_header_toggle_block_id = block_id.clone();
                        let full_block_id = block_id.clone();
                        let open_tool_output_block_id = block_id.clone();
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
                        let measurement = ConversationRowMeasurement {
                            item_key: block.item_key.clone(),
                            source_revision: block.source_revision,
                            width_bucket: block.width_bucket,
                            text_phase,
                            details_expanded: detail_expanded,
                            height: 0.,
                        };
                        let measurement_owner = cx.entity().downgrade();
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
                        let tool_command = is_tool
                            .then(|| owner.read(cx).tool_command(&block_id))
                            .flatten();
                        let tool_exit_code = is_tool.then(|| {
                            tool_exit_code_label(
                                &block.title,
                                &text,
                                block.done,
                                block.is_error,
                            )
                        });
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
                        let accessible_label = terminal_label.map_or_else(
                            || block.title.to_string(),
                            |state| format!("{}, {state}", block.title),
                        );
                        Some(
                            div()
                                .id((
                                    ElementId::from("conversation-block"),
                                    SharedString::new(block.item_key.stable_id_arc()),
                                ))
                                .when(index + 1 == row_count, |row| {
                                    row.debug_selector(|| "conversation-last-row".to_owned())
                                })
                                .role(Role::ListItem)
                                .aria_label(accessible_label)
                                .aria_selected(selected)
                                .aria_position_in_set(index + 1)
                                .aria_size_of_set(row_count)
                                .when(block.preview_truncated, |row| {
                                    row.aria_expanded(full_view_open)
                                })
                                .when(selected, |row| row.aria_active_descendant())
                                .h(px(row_height))
                                .w_full()
                                .min_w_0()
                                .px_token(DesignSpace::Lg)
                                .py_token(DesignSpace::Xs)
                                .flex()
                                .items_start()
                                .when(visual.align_right, |row| row.justify_end())
                                .child(
                                    div()
                                        .relative()
                                        .group(hover_group.clone())
                                        .id((
                                            ElementId::from("conversation-card"),
                                            SharedString::new(block.item_key.stable_id_arc()),
                                        ))
                                        .when(index + 1 == row_count, |card| {
                                            card.debug_selector(|| {
                                                "conversation-last-card".to_owned()
                                            })
                                        })
                                        .w_full()
                                        .min_w_0()
                                        .when(block.preview_truncated, |card| {
                                            card.max_h(px(TRANSCRIPT_COLLAPSED_PREVIEW_MAX_HEIGHT))
                                                .overflow_hidden()
                                        })
                                        .on_prepaint(move |bounds, _, cx| {
                                            let Some(pane) = measurement_owner.upgrade() else {
                                                return;
                                            };
                                            let mut measurement = measurement;
                                            // The measured card excludes the row's `py_1`
                                            // padding (4px on each edge).
                                            measurement.height = f32::from(bounds.size.height)
                                                + CONVERSATION_ROW_VERTICAL_PADDING_PX as f32;
                                            pane.update(cx, |_, cx| {
                                                cx.emit(ConversationPaneEvent::Measured(
                                                    measurement,
                                                ));
                                            });
                                        })
                                        .when(visual.align_right, |card| {
                                            card.w(relative(
                                                USER_MESSAGE_WIDTH_PERCENT as f32 / 100.,
                                            ))
                                                .max_w(px(USER_MESSAGE_MAX_WIDTH as f32))
                                        })
                                        .when(!visual.align_right, |card| {
                                            card.max_w(px(ASSISTANT_MESSAGE_MAX_WIDTH as f32))
                                        })
                                        .rounded_token(DesignRadius::Lg)
                                        .when(!is_assistant, |card| {
                                            card.border_l_2()
                                                .border_color(rgb(card_border.value()))
                                        })
                                        .bg(rgb(visual.surface.value()))
                                        .hover(move |style| {
                                            style.bg(rgb(theme.hover.value()))
                                        })
                                        .when(selected, |card| {
                                            card.bg(rgb(theme.selection.value()))
                                        })
                                        .px_token(DesignSpace::Lg)
                                        .py_token(DesignSpace::Md)
                                        .flex()
                                        .flex_col()
                                        .gap_token(DesignSpace::Sm)
                                        .child(
                                            div()
                                                .id(("conversation-row-header", index))
                                                .flex()
                                                .items_center()
                                                .justify_between()
                                                .gap_token(DesignSpace::Md)
                                                .pr_24()
                                                .on_click(cx.listener(move |_, _, _, cx| {
                                                    cx.emit(ConversationPaneEvent::Select {
                                                        block_id: select_block_id.clone(),
                                                        durable,
                                                    });
                                                }))
                                                .child(
                                                    div()
                                                        .flex()
                                                        .flex_1()
                                                        .min_w_0()
                                                        .items_center()
                                                        .gap_token(DesignSpace::Sm)
                                                        .when(
                                                            block.kind
                                                                != ConversationBlockKind::User,
                                                            |metadata| {
                                                                metadata.child(
                                                                    div()
                                                                        .px_token(DesignSpace::Sm)
                                                                        .py_token(DesignSpace::Xs)
                                                                        .rounded_token(
                                                                            DesignRadius::Sm,
                                                                        )
                                                                        .bg(rgb(
                                                                            theme
                                                                                .elevated
                                                                                .value(),
                                                                        ))
                                                                        .text_token(
                                                                            DesignText::Metadata,
                                                                        )
                                                                        .font_weight(
                                                                            gpui::FontWeight::SEMIBOLD,
                                                                        )
                                                                        .text_color(rgb(
                                                                            visual.accent.value(),
                                                                        ))
                                                                        .child(visual.glyph),
                                                                )
                                                            },
                                                        )
                                                        .child(
                                                            div()
                                                                .text_token(DesignText::Body)
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
                                                            .text_token(DesignText::Metadata)
                                                            .text_color(rgb(visual.accent.value()))
                                                            .child(label),
                                                    )
                                                })
                                                .when(
                                                    is_tool && has_collapsible_detail,
                                                    |header| {
                                                        header.child(
                                                            Button::new((
                                                                "show-tool-details",
                                                                index,
                                                            ))
                                                            .compact()
                                                            .label(if detail_expanded {
                                                                "Hide"
                                                            } else {
                                                                "Show"
                                                            })
                                                            .tooltip(
                                                                "Show or hide tool output and arguments",
                                                            )
                                                            .on_click(cx.listener(
                                                                move |_, _, _, cx| {
                                                                    cx.stop_propagation();
                                                                    cx.emit(
                                                                        ConversationPaneEvent::ToggleDetails {
                                                                            block_id: tool_header_toggle_block_id.clone(),
                                                                        },
                                                                    );
                                                                },
                                                            )),
                                                        )
                                                    },
                                                ),
                                        )
                                        .when(
                                            is_assistant
                                                && !detail_text.is_empty()
                                                && !detail_expanded,
                                            |card| {
                                                card.child(
                                                    div()
                                                        .rounded_token(DesignRadius::Md)
                                                        .border_l_3()
                                                        .border_color(rgb(theme.reasoning.value()))
                                                        .bg(rgb(theme.thinking_surface.value()))
                                                        .px_token(DesignSpace::Md)
                                                        .py_token(DesignSpace::Sm)
                                                        .flex()
                                                        .items_center()
                                                        .justify_between()
                                                        .text_token(DesignText::Metadata)
                                                        .text_color(rgb(theme.reasoning.value()))
                                                        .child(
                                                            div()
                                                                .flex_1()
                                                                .min_w_0()
                                                                .whitespace_normal()
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
                                                                }),
                                                        )
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
                                                    .rounded_token(DesignRadius::Md)
                                                    .border_l_3()
                                                    .border_color(rgb(theme.reasoning.value()))
                                                    .bg(rgb(theme.thinking_surface.value()))
                                                    .px_token(DesignSpace::Md)
                                                    .py_token(DesignSpace::Sm)
                                                    .flex()
                                                    .flex_col()
                                                    .gap_token(DesignSpace::Xs)
                                                    .child(
                                                        div()
                                                            .text_token(DesignText::Metadata)
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
                                                            cx.entity().downgrade(),
                                                        )
                                                        .into_any_element(window, cx),
                                                    ),
                                            )
                                        },
                                        )
                                        .when(is_tool && detail_expanded, |card| {
                                            card.child(
                                                div()
                                                    .flex()
                                                    .flex_col()
                                                    .gap_token(DesignSpace::Xs)
                                                    .when_some(tool_command.clone(), |section, command| {
                                                        section
                                                            .child(
                                                                div()
                                                                    .text_token(DesignText::Metadata)
                                                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                                                    .text_color(rgb(theme.subtle_text.value()))
                                                                    .child("COMMAND"),
                                                            )
                                                            .child(
                                                                div()
                                                                    .font_family(MONOSPACE_FONT_FAMILY)
                                                                    .text_token(DesignText::Metadata)
                                                                    .text_color(rgb(theme.text.value()))
                                                                    .whitespace_normal()
                                                                    .child(command),
                                                            )
                                                    })
                                                    .child(
                                                        div()
                                                            .flex()
                                                            .flex_wrap()
                                                            .items_center()
                                                            .gap_token(DesignSpace::Xs)
                                                            .when(tool_command.is_some(), |actions| {
                                                                actions.child(
                                                                    Button::new(("copy-tool-command", index))
                                                                        .debug_selector(|| {
                                                                            "desktop-copy-tool-command".into()
                                                                        })
                                                                        .compact()
                                                                        .label("Copy command")
                                                                        .tooltip("Copy the complete bounded tool command")
                                                                        .on_click(cx.listener(move |_, _, _, cx| {
                                                                            cx.stop_propagation();
                                                                            cx.emit(ConversationPaneEvent::CopyToolCommand {
                                                                                block_id: copy_tool_command_block_id.clone(),
                                                                            });
                                                                        })),
                                                                )
                                                            })
                                                            .when(!text.is_empty(), |actions| {
                                                                actions
                                                                    .child(
                                                                        Button::new(("copy-tool-output", index))
                                                                            .debug_selector(|| {
                                                                                "desktop-copy-tool-output".into()
                                                                            })
                                                                            .compact()
                                                                            .label("Copy output")
                                                                            .tooltip("Copy the complete bounded tool output")
                                                                            .on_click(cx.listener(move |_, _, _, cx| {
                                                                                cx.stop_propagation();
                                                                                cx.emit(ConversationPaneEvent::CopyToolOutput {
                                                                                    block_id: copy_tool_output_block_id.clone(),
                                                                                });
                                                                            })),
                                                                    )
                                                                    .child(
                                                                        Button::new(("open-tool-output", index))
                                                                            .debug_selector(|| {
                                                                                "desktop-open-tool-output".into()
                                                                            })
                                                                            .compact()
                                                                            .label("Open full output")
                                                                            .tooltip("Open the complete bounded tool output")
                                                                            .on_click(cx.listener(move |_, _, _, cx| {
                                                                                cx.stop_propagation();
                                                                                cx.emit(ConversationPaneEvent::OpenToolOutput {
                                                                                    block_id: open_tool_output_block_id.clone(),
                                                                                });
                                                                            })),
                                                                    )
                                                            }),
                                                    ),
                                            )
                                        })
                                        .when(!text.is_empty() && (!is_tool || detail_expanded), |card| {
                                            let content = StreamingText::new(
                                                markdown_id,
                                                text,
                                                text_phase,
                                                cx.entity().downgrade(),
                                            )
                                            .into_any_element(window, cx);
                                            if is_tool {
                                                card.child(
                                                    div()
                                                        .flex()
                                                        .flex_col()
                                                        .gap_token(DesignSpace::Xs)
                                                        .child(
                                                            div()
                                                                .text_token(DesignText::Metadata)
                                                                .font_weight(
                                                                    gpui::FontWeight::SEMIBOLD,
                                                                )
                                                                .text_color(rgb(
                                                                    theme.subtle_text.value(),
                                                                ))
                                                                .child("OUTPUT"),
                                                        )
                                                        .child(content),
                                                )
                                            } else {
                                                card.child(content)
                                            }
                                        })
                                        .when(
                                            !is_assistant
                                                && !detail_text.is_empty()
                                                && (!is_tool || detail_expanded),
                                            |card| {
                                            card.child(
                                                div()
                                                    .mt_token(DesignSpace::Xs)
                                                    .rounded_token(DesignRadius::Md)
                                                    .border_1()
                                                    .border_color(rgb(theme.border.value()))
                                                    .bg(rgb(theme.canvas.value()))
                                                    .px_token(DesignSpace::Md)
                                                    .py_token(DesignSpace::Sm)
                                                    .when(is_tool, |detail| {
                                                        detail
                                                            .font_family(MONOSPACE_FONT_FAMILY)
                                                            .text_token(DesignText::Metadata)
                                                    })
                                                    .text_color(rgb(theme.muted_text.value()))
                                                    .when(is_tool, |detail| {
                                                        detail.child(
                                                            div()
                                                                .mb_token(DesignSpace::Xs)
                                                                .font_family(
                                                                    desktop::shell::UI_FONT_FAMILY,
                                                                )
                                                                .text_token(DesignText::Metadata)
                                                                .font_weight(
                                                                    gpui::FontWeight::SEMIBOLD,
                                                                )
                                                                .text_color(rgb(
                                                                    theme.subtle_text.value(),
                                                                ))
                                                                .child("ARGUMENTS"),
                                                        )
                                                    })
                                                    .child(
                                                        StreamingText::new(
                                                            detail_markdown_id,
                                                            detail_text,
                                                            text_phase,
                                                            cx.entity().downgrade(),
                                                        )
                                                        .into_any_element(window, cx),
                                                    ),
                                            )
                                        },
                                        )
                                        .when_some(
                                            (is_tool && detail_expanded)
                                                .then_some(tool_exit_code)
                                                .flatten(),
                                            |card, exit_code| {
                                                card.child(
                                                    div()
                                                        .flex()
                                                        .items_center()
                                                        .gap_token(DesignSpace::Sm)
                                                        .text_token(DesignText::Metadata)
                                                        .child(
                                                            div()
                                                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                                                .text_color(rgb(theme.subtle_text.value()))
                                                                .child("EXIT CODE"),
                                                        )
                                                        .child(
                                                            div()
                                                                .font_family(MONOSPACE_FONT_FAMILY)
                                                                .text_color(rgb(if block.is_error {
                                                                    theme.danger.value()
                                                                } else {
                                                                    theme.muted_text.value()
                                                                }))
                                                                .child(exit_code),
                                                        ),
                                                )
                                            },
                                        )
                                        .when_some(diagnostic_recovery, |card, identity| {
                                            let retry_identity = identity.clone();
                                            let failed_identity = identity.clone();
                                            card.child(
                                                div()
                                                    .flex()
                                                    .flex_wrap()
                                                    .items_center()
                                                    .gap_token(DesignSpace::Xs)
                                                    .child(
                                                        Button::new(("retry-diagnostic", index))
                                                            .debug_selector(|| {
                                                                "desktop-retry-diagnostic".into()
                                                            })
                                                            .compact()
                                                            .label("Retry")
                                                            .tooltip("Retry the pending recovery")
                                                            .on_click(cx.listener(move |_, _, _, cx| {
                                                                cx.stop_propagation();
                                                                cx.emit(ConversationPaneEvent::Recovery {
                                                                    identity: retry_identity.clone(),
                                                                    action: DesktopRecoveryAction::Retry,
                                                                });
                                                            })),
                                                    )
                                                    .child(
                                                        Button::new(("mark-diagnostic-failed", index))
                                                            .compact()
                                                            .label("Mark failed")
                                                            .tooltip("Resolve the pending recovery as failed")
                                                            .on_click(cx.listener(move |_, _, _, cx| {
                                                                cx.stop_propagation();
                                                                cx.emit(ConversationPaneEvent::Recovery {
                                                                    identity: failed_identity.clone(),
                                                                    action: DesktopRecoveryAction::MarkFailed,
                                                                });
                                                            })),
                                                    )
                                                    .child(
                                                        Button::new(("abort-diagnostic", index))
                                                            .compact()
                                                            .label("Abort")
                                                            .tooltip("Resolve the pending recovery as aborted")
                                                            .on_click(cx.listener(move |_, _, _, cx| {
                                                                cx.stop_propagation();
                                                                cx.emit(ConversationPaneEvent::Recovery {
                                                                    identity: identity.clone(),
                                                                    action: DesktopRecoveryAction::Abort,
                                                                });
                                                            })),
                                                    ),
                                            )
                                        })
                                        .when(block.preview_truncated, |card| {
                                            card.child(
                                                div()
                                                    .absolute()
                                                    .left_4()
                                                    .right_4()
                                                    .bottom_2()
                                                    .flex()
                                                    .items_center()
                                                    .justify_between()
                                                    .gap_token(DesignSpace::Sm)
                                                    .rounded_token(DesignRadius::Md)
                                                    .border_1()
                                                    .border_color(rgb(theme.warning.value()))
                                                    .bg(rgb(theme.elevated.value()))
                                                    .px_token(DesignSpace::Md)
                                                    .py_token(DesignSpace::Sm)
                                                    .text_color(rgb(theme.warning.value()))
                                                    .child(
                                                        "! preview truncated at desktop safety limit",
                                                    )
                                                    .child(
                                                        Button::new(("open-full-message", index))
                                                            .debug_selector(|| {
                                                                "desktop-open-full-message".into()
                                                            })
                                                            .compact()
                                                            .label("Open full message")
                                                            .tooltip(
                                                                "Open the complete bounded message source",
                                                            )
                                                                .on_click(cx.listener(
                                                                    move |_, _, _, cx| {
                                                                        cx.emit(
                                                                            ConversationPaneEvent::OpenFull {
                                                                            block_id: full_block_id
                                                                                .clone(),
                                                                        },
                                                                    );
                                                                },
                                                            )),
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
                                        .when(index + 1 == row_count, |card| {
                                            card.child(
                                                div()
                                                    .id("conversation-tail-marker")
                                                    .debug_selector(|| {
                                                        "conversation-tail-marker".to_owned()
                                                    })
                                                    .w_full()
                                                    .h(px(1.)),
                                            )
                                        })
                                        .child(
                                            div()
                                                .absolute()
                                                .top_2()
                                                .right_2()
                                                .flex()
                                                .gap_token(DesignSpace::Xs)
                                                .invisible()
                                                .group_hover(hover_group, |style| style.visible())
                                                .when(selected, |actions| actions.visible())
                                                .when(has_collapsible_detail && !is_tool, |actions| {
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
            .id("conversation-log")
            .role(Role::Log)
            .aria_label("Conversation messages")
            .aria_description(if follow_latest {
                "Following the latest conversation message."
            } else {
                "Conversation history paused away from the latest message."
            })
            .relative()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .when(visible_count == 0, |content| {
                content.child(
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
                        ),
                )
            })
            .when(visible_count > 0, |content| {
                content
                    .when(omitted_count > 0, |content| {
                        content.child(
                            div()
                                .px_token(DesignSpace::Lg)
                                .py_token(DesignSpace::Sm)
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
                                .rounded_token(DesignRadius::Lg)
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

fn tool_exit_code_label(title: &str, output: &str, done: bool, is_error: bool) -> String {
    if !done {
        return "running".to_owned();
    }
    if title.split(" · ").nth(1) != Some("bash") {
        return "not reported".to_owned();
    }
    if !is_error {
        return "0".to_owned();
    }
    output
        .rsplit_once("Command exited with code ")
        .and_then(|(_, code)| code.lines().next())
        .filter(|code| code.parse::<i32>().is_ok())
        .unwrap_or("not reported")
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::tool_exit_code_label;

    #[test]
    fn tool_metadata_uses_only_structured_or_reported_values() {
        assert_eq!(tool_exit_code_label("Tool · bash", "ok", true, false), "0");
        assert_eq!(
            tool_exit_code_label(
                "Tool · bash",
                "failed\n\nCommand exited with code 101",
                true,
                true
            ),
            "101"
        );
        assert_eq!(
            tool_exit_code_label("Tool · read", "ok", true, false),
            "not reported"
        );
    }
}
