use super::*;

impl Render for ConversationPane {
    fn render(&mut self, _window: &mut Window, cx: &mut gpui::Context<Self>) -> impl IntoElement {
        let Some(view_model) = self.view_model.clone() else {
            return div()
                .id("conversation-log")
                .role(Role::Log)
                .aria_label("Conversation messages")
                .flex_1();
        };
        let scroll_handle = view_model.scroll;
        let visible_count = view_model.visible_count;
        let event_count = view_model.event_count;
        let message_count = view_model.message_count;
        let tool_count = view_model.tool_count;
        let omitted_count = view_model.omitted_count;
        let follow_latest = view_model.follow_latest;
        let unseen_updates = view_model.unseen_updates;
        let selected_block_id = view_model.selected_block_id;
        let expanded_details = view_model.expanded_details;
        let full_view_block_id = view_model.full_view_block_id;
        let diagnostic_recovery = view_model.diagnostic_recovery;
        let render = view_model.render;
        let theme = SemanticTheme::current(cx);
        let pane_entity = cx.entity();
        let transcript_list = list(scroll_handle.clone(), move |index, _window, cx| {
            pane_entity.update(cx, |pane, cx| {
                pane.markdown_generation = pane.markdown_generation.wrapping_add(1);
                let Some(block) = render.row(index) else {
                    return div().into_any_element();
                };
                let row = {
                        let selected =
                            selected_block_id.as_deref() == Some(block.item_key.row_id());
                        let detail_expanded =
                            expanded_details.contains(block.item_key.row_id());
                        let full_view_open =
                            full_view_block_id.as_deref() == Some(block.item_key.row_id());
                        let diagnostic_recovery =
                            (block.kind == ConversationBlockKind::Diagnostic)
                                .then(|| diagnostic_recovery.clone())
                                .flatten();
                        let row_count = render.len();
                        let is_last_row = index + 1 == row_count;
                        let user_message_background_inset = USER_MESSAGE_COPY_FOOTER_INSET
                            + if is_last_row {
                                DesignSpace::Sm.pixels() + CONVERSATION_TAIL_MARKER_HEIGHT
                            } else {
                                0.
                            };
                        let block_id = block.item_key.row_id().to_owned();
                        let select_block_id = block_id.clone();
                        let copy_block_id = block_id.clone();
                        let copy_tool_details_block_id = block_id.clone();
                        let delegation_copy_block_id = copy_tool_details_block_id.clone();
                        let tool_header_toggle_block_id = block_id.clone();
                        let reasoning_collapsed_toggle_block_id = block_id.clone();
                        let reasoning_collapsed_chevron_block_id = block_id.clone();
                        let reasoning_expanded_toggle_block_id = block_id.clone();
                        let reasoning_expanded_chevron_block_id = block_id.clone();
                        let full_block_id = block_id.clone();
                        let hover_group = SharedString::new(format!(
                            "conversation-card:{}",
                            block.item_key.stable_id()
                        ));
                        let tool_output_hover_group = SharedString::new(format!(
                            "tool-output:{}",
                            block.item_key.stable_id()
                        ));
                        let delegation_output_hover_group = tool_output_hover_group.clone();
                        let durable = block.durable;
                        let markdown_key = block.markdown_state_key.clone();
                        let detail_markdown_key = block.detail_markdown_state_key.clone();
                        let text = block.text.clone();
                        let detail_text = block.detail.clone();
                        let user_card_width = (block.kind == ConversationBlockKind::User)
                            .then(|| user_message_width(&text));
                        let visual = conversation_block_visual(block.kind, block.is_error, theme);
                        let is_assistant = block.kind == ConversationBlockKind::Assistant;
                        let previous_kind = index.checked_sub(1).and_then(|previous| {
                            render.row(previous).map(|row| row.kind)
                        });
                        let next_kind = render.row(index + 1).map(|row| row.kind);
                        let show_identity_header =
                            conversation_identity_header_visible(block.kind, previous_kind);
                        let reasoning_duration_label = block
                            .reasoning_duration_millis
                            .map(compact_duration);
                        let is_tool = block.kind == ConversationBlockKind::Tool;
                        let is_delegation = block.kind == ConversationBlockKind::Delegation;
                        let turn_label = block.turn.as_ref().map(|turn| {
                            let mut label = format!("Model · {}", turn.model);
                            if let Some(duration_millis) = turn.duration_millis {
                                label.push_str(" · ");
                                label.push_str(&compact_duration(duration_millis));
                            }
                            SharedString::from(label)
                        });
                        let delegation = block.delegation.as_ref();
                        let delegation_expandable = is_delegation;
                        let delegation_summary_text = if is_delegation {
                            delegation_task_summary(&block.text)
                        } else {
                            String::new()
                        };
                        let show_generic_copy =
                            conversation_copy_footer_visible(block.kind, next_kind);
                        let tool_command = is_tool
                            .then(|| structured_tool_command(&block.detail, &block.text))
                            .flatten();
                        let tool_name = is_tool.then(|| tool_name_from_title(&block.title));
                        let tool_label = tool_name.map(tool_display_label);
                        let tool_expandable = tool_name.is_some_and(tool_is_expandable);
                        let tool_summary_text = tool_name
                            .map(|name| tool_summary(name, &block.detail, &block.text))
                            .unwrap_or_default();
                        let terminal_label = if block.is_error {
                            Some("failed")
                        } else if is_tool && block.done {
                            Some("completed")
                        } else if !block.done {
                            Some(if is_tool { "running" } else { "streaming" })
                        } else {
                            None
                        };
                        let accessible_label = if let Some(meta) = delegation {
                            format!(
                                "{}, {}, {}",
                                block.title,
                                meta.target_id,
                                meta.status.label()
                            )
                        } else {
                            terminal_label.map_or_else(
                                || block.title.to_string(),
                                |state| format!("{}, {state}", block.title),
                            )
                        };
                        // Selection paints a full-height leading rail; hover no
                        // longer paints a stub. Tool-group rows (tool calls and
                        // delegations) stay rail-free like before. The rail is
                        // absolutely positioned and never affects row height.
                        let selection_rail = (selected && !is_tool_group(block.kind)).then(|| {
                            div()
                                .debug_selector(|| "conversation-selected-rail".to_owned())
                                .absolute()
                                .left_0()
                                .top_0()
                                .bottom_0()
                                .w(px(CONVERSATION_RAIL_WIDTH))
                                .bg(rgb(theme.focus_ring.value()))
                        });
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
                                .w_full()
                                .min_w_0()
                                .py_token(DesignSpace::Md)
                                .flex()
                                .items_start()
                                .child(
                                    div()
                                        .when(index + 1 == row_count, |track| {
                                            track.debug_selector(|| {
                                                "conversation-last-track".to_owned()
                                            })
                                        })
                                        .w_full()
                                        .max_w(px(CONVERSATION_CONTENT_MAX_WIDTH as f32))
                                        .mx_auto()
                                        .min_w_0()
                                        .px_token(DesignSpace::Lg)
                                        .flex()
                                        .items_start()
                                        .when(visual.align_right, |track| track.justify_end())
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
                                        .min_w_0()
                                        .when(block.preview_truncated, |card| {
                                            card.max_h(px(TRANSCRIPT_COLLAPSED_PREVIEW_MAX_HEIGHT))
                                                .overflow_hidden()
                                        })
                                        .when_some(user_card_width, |card, width| {
                                            card.w(px(width))
                                                .max_w(px(USER_MESSAGE_MAX_WIDTH as f32))
                                        })
                                        .when(!visual.align_right, |card| {
                                            card.w_full()
                                                .max_w(px(ASSISTANT_MESSAGE_MAX_WIDTH as f32))
                                        })
                                        .flex()
                                        .flex_col()
                                        .when(is_tool, |card| {
                                            card.px_token(DesignSpace::Lg)
                                                .py_token(DesignSpace::Xs)
                                                .gap_token(DesignSpace::Xs)
                                                .child(
                                                    div()
                                                        .absolute()
                                                        .left_0()
                                                        .top_0()
                                                        .bottom_0()
                                                        .w(px(1.))
                                                        .bg(rgb(theme.divider.value())),
                                                )
                                        })
                                        .when(!is_tool, |card| {
                                            card.px_token(DesignSpace::Lg)
                                                .py_token(DesignSpace::Md)
                                                .gap_token(DesignSpace::Sm)
                                        })
                                        .when(visual.align_right, |card| {
                                            card.child(
                                                div()
                                                    .debug_selector(|| {
                                                        "desktop-user-message-bubble".into()
                                                    })
                                                    .absolute()
                                                    .top_0()
                                                    .left_0()
                                                    .right_0()
                                                    .bottom(px(user_message_background_inset))
                                                    .rounded_token(DesignRadius::Lg)
                                                    .bg(rgb(theme.elevated.value())),
                                            )
                                        })
                                        .when_some(selection_rail, |card, rail| card.child(rail))
                                        .with_identity_header(
                                            IdentityHeaderArgs {
                                                show_identity_header,
                                                index,
                                                row_count,
                                                is_tool,
                                                tool_expandable,
                                                is_delegation,
                                                detail_expanded,
                                                select_block_id,
                                                durable,
                                                tool_header_toggle_block_id,
                                                tool_label,
                                                tool_summary_text,
                                                hover_group: hover_group.clone(),
                                                delegation: delegation.cloned(),
                                                visual_glyph: visual.glyph,
                                                visual_accent: visual.accent,
                                                delegation_summary_text,
                                                delegation_expandable,
                                                terminal_label,
                                                block_kind: block.kind,
                                                block_title: block.title.clone(),
                                                theme,
                                            },
                                            cx,
                                        )
                                        .with_reasoning(
                                            ReasoningArgs {
                                                index,
                                                is_assistant,
                                                detail_text: detail_text.clone(),
                                                detail_expanded,
                                                reasoning_duration_label,
                                                block_done: block.done,
                                                reasoning_collapsed_toggle_block_id,
                                                reasoning_collapsed_chevron_block_id,
                                                reasoning_expanded_toggle_block_id,
                                                reasoning_expanded_chevron_block_id,
                                                detail_markdown_key: detail_markdown_key.clone(),
                                                theme,
                                            },
                                            pane,
                                            cx,
                                        )
                                        .when(is_tool && tool_expandable && detail_expanded, |card| {
                                            let tool_name_str = tool_name_from_title(&block.title);
                                            let is_shell = matches!(tool_name_str, "bash" | "shell");
                                            let is_edit = tool_name_str == "edit";
                                            let is_write = tool_name_str == "write";
                                            let is_ls = tool_name_str == "ls";
                                            let is_find = tool_name_str == "find";
                                            let is_grep = tool_name_str == "grep";
                                            let is_web_search = tool_name_str == "web_search";
                                            card.child(
                                                div()
                                                    .id(("tool-output-region", index))
                                                    .debug_selector(|| {
                                                        "desktop-tool-output-region".into()
                                                    })
                                                    .relative()
                                                    .group(tool_output_hover_group.clone())
                                                    .rounded_token(DesignRadius::Md)
                                                    .border_1()
                                                    .border_color(rgb(theme.divider.value()))
                                                    .bg(rgb(theme.surface.value()))
                                                    .child(
                                                        div()
                                                            .id(("tool-output-scroll", index))
                                                            .max_h(px(400.))
                                                            .overflow_y_scroll()
                                                            .p_token(DesignSpace::Sm)
                                                            .pr_12()
                                                            .font_family(MONOSPACE_FONT_FAMILY)
                                                            .text_token(DesignText::Metadata)
                                                            .when(is_shell, |region| {
                                                                region
                                                                    .child(
                                                                        div()
                                                                            .text_color(rgb(
                                                                                theme
                                                                                    .subtle_text
                                                                                    .value(),
                                                                            ))
                                                                            .child(
                                                                                SharedString::new(
                                                                                    format!(
                                                                                        "$ {}",
                                                                                        tool_command
                                                                                            .clone()
                                                                                            .unwrap_or_default()
                                                                                    ),
                                                                                ),
                                                                            ),
                                                                    )
                                                                    .when(
                                                                        !text.is_empty(),
                                                                        |region| {
                                                                            region.child(
                                                                                div()
                                                                                    .text_color(rgb(
                                                                                        theme
                                                                                            .text
                                                                                            .value(),
                                                                                    ))
                                                                                    .child(
                                                                                        SharedString::new(
                                                                                            &text,
                                                                                        ),
                                                                                    ),
                                                                            )
                                                                        },
                                                                    )
                                                            })
                                                            .when(is_edit, |region| {
                                                                region.child(edit_diff_view(
                                                                    &block.detail,
                                                                    &block.text,
                                                                    &theme,
                                                                ))
                                                            })
                                                            .when(is_write, |region| {
                                                                let diff = write_diff_text(
                                                                    &block.detail,
                                                                    &block.text,
                                                                );
                                                                if diff.is_empty() {
                                                                    region.when(
                                                                        !text.is_empty(),
                                                                        |region| {
                                                                            region.child(
                                                                                div()
                                                                                    .text_color(rgb(
                                                                                        theme
                                                                                            .text
                                                                                            .value(),
                                                                                    ))
                                                                                    .whitespace_normal()
                                                                                    .child(
                                                                                        SharedString::new(
                                                                                            &text,
                                                                                        ),
                                                                                    ),
                                                                            )
                                                                        },
                                                                    )
                                                                } else {
                                                                    region.child(write_diff_view(
                                                                        &block.detail,
                                                                        &block.text,
                                                                        &theme,
                                                                    ))
                                                                }
                                                            })
                                                            .when(is_ls || is_find, |region| {
                                                                region.child(ls_find_view(
                                                                    &text,
                                                                    &theme,
                                                                ))
                                                            })
                                                            .when(is_grep, |region| {
                                                                region.child(grep_view(
                                                                    &text,
                                                                    &theme,
                                                                ))
                                                            })
                                                            .when(is_web_search, |region| {
                                                                region.child(web_search_view(
                                                                    &text,
                                                                    &theme,
                                                                ))
                                                            })
                                                            .when(
                                                                !is_shell
                                                                    && !is_edit
                                                                    && !is_write
                                                                    && !is_ls
                                                                    && !is_find
                                                                    && !is_grep
                                                                    && !is_web_search,
                                                                |region| {
                                                                    region.child(
                                                                        div()
                                                                            .text_color(rgb(
                                                                                theme.text.value(),
                                                                            ))
                                                                            .whitespace_normal()
                                                                            .child(
                                                                                SharedString::new(
                                                                                    &text,
                                                                                ),
                                                                            ),
                                                                    )
                                                                },
                                                            ),
                                                    )
                                                    .child(
                                                        div()
                                                            .absolute()
                                                            .top_2()
                                                            .right_2()
                                                            .child(conversation_hover_tool(
                                                                DesktopIconButton::new(
                                                                    ("copy-tool-details", index),
                                                                    DesktopIcon::Copy,
                                                                    "Copy the displayed tool details",
                                                                )
                                                                .build()
                                                                .debug_selector(|| {
                                                                    "desktop-copy-tool-details".into()
                                                                })
                                                                .on_click(cx.listener(
                                                                    move |_, _, _, cx| {
                                                                        cx.stop_propagation();
                                                                        cx.emit(
                                                                            ConversationPaneEvent::CopyToolDetails {
                                                                                block_id: copy_tool_details_block_id.clone(),
                                                                            },
                                                                        );
                                                                    },
                                                                )),
                                                                tool_output_hover_group,
                                                                false,
                                                            )),
                                                    ),
                                            )
                                        })
                                        .when(is_delegation && detail_expanded, |card| {
                                            card.child(
                                                div()
                                                    .id(("delegation-output-region", index))
                                                    .debug_selector(|| {
                                                        "desktop-delegation-detail".into()
                                                    })
                                                    .relative()
                                                    .group(delegation_output_hover_group.clone())
                                                    .rounded_token(DesignRadius::Md)
                                                    .border_1()
                                                    .border_color(rgb(
                                                        theme.divider.value(),
                                                    ))
                                                    .bg(rgb(theme.surface.value()))
                                                    .child(
                                                        div()
                                                            .id((
                                                                "delegation-output-scroll",
                                                                index,
                                                            ))
                                                            .p_token(DesignSpace::Sm)
                                                            .pr_12()
                                                            .flex()
                                                            .flex_col()
                                                            .gap_token(DesignSpace::Md)
                                                            .child(
                                                                div()
                                                                    .text_color(rgb(
                                                                        theme.text.value(),
                                                                    ))
                                                                    .child(
                                                                        StreamingText::new(
                                                                            pane.markdown_state(
                                                                                &markdown_key,
                                                                                &text,
                                                                                cx,
                                                                            ),
                                                                            cx.entity()
                                                                                .downgrade(),
                                                                        )
                                                                        .into_any_element(),
                                                                    ),
                                                            )
                                                            .when(
                                                                !detail_text.is_empty(),
                                                                |region| {
                                                                    region
                                                                        .child(
                                                                            div()
                                                                                .h(px(1.))
                                                                                .bg(rgb(
                                                                                    theme
                                                                                        .divider
                                                                                        .value(),
                                                                                )),
                                                                        )
                                                                        .child(
                                                                            div()
                                                                                .text_color(rgb(
                                                                                    theme
                                                                                        .muted_text
                                                                                        .value(),
                                                                                ))
                                                                                .child(
                                                                                    StreamingText::new(
                                                                                        pane.markdown_state(
                                                                                            &detail_markdown_key,
                                                                                            &detail_text,
                                                                                            cx,
                                                                                        ),
                                                                                        cx.entity()
                                                                                            .downgrade(),
                                                                                    )
                                                                                    .into_any_element(),
                                                                                ),
                                                                        )
                                                                },
                                                            ),
                                                    )
                                                    .child(
                                                        div()
                                                            .absolute()
                                                            .top_2()
                                                            .right_2()
                                                            .child(conversation_hover_tool(
                                                                DesktopIconButton::new(
                                                                    (
                                                                        "copy-delegation-details",
                                                                        index,
                                                                    ),
                                                                    DesktopIcon::Copy,
                                                                    "Copy the delegation task and result",
                                                                )
                                                                .build()
                                                                .debug_selector(|| {
                                                                    "desktop-copy-delegation-details"
                                                                        .into()
                                                                })
                                                                .on_click(cx.listener(
                                                                    move |_, _, _, cx| {
                                                                        cx.stop_propagation();
                                                                        cx.emit(
                                                                            ConversationPaneEvent::CopyToolDetails {
                                                                                block_id: delegation_copy_block_id.clone(),
                                                                            },
                                                                        );
                                                                    },
                                                                )),
                                                                delegation_output_hover_group,
                                                                false,
                                                            )),
                                                    ),
                                            )
                                        })
                                        .when(!text.is_empty() && !is_tool && !is_delegation, |card| {
                                            let content = StreamingText::new(
                                                pane.markdown_state(&markdown_key, &text, cx),
                                                cx.entity().downgrade(),
                                            )
                                            .into_any_element();
                                            card.child(content)
                                        })
                                        .when(
                                            !is_assistant
                                                && !detail_text.is_empty()
                                                && !is_tool
                                                && !is_delegation,
                                            |card| {
                                                card.child(
                                                    div()
                                                        .mt_token(DesignSpace::Xs)
                                                        .pl_token(DesignSpace::Md)
                                                        .py_token(DesignSpace::Sm)
                                                        .text_color(rgb(theme.muted_text.value()))
                                                        .child(
                                                            StreamingText::new(
                                                                pane.markdown_state(
                                                                    &detail_markdown_key,
                                                                    &detail_text,
                                                                    cx,
                                                                ),
                                                                cx.entity().downgrade(),
                                                            )
                                                            .into_any_element(),
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
                                                        conversation_recovery_button(
                                                            ("retry-diagnostic", index),
                                                            "Retry",
                                                            "Retry the pending recovery",
                                                            retry_identity,
                                                            DesktopRecoveryAction::Retry,
                                                            cx,
                                                        )
                                                            .debug_selector(|| {
                                                                "desktop-retry-diagnostic".into()
                                                            }),
                                                    )
                                                    .child(
                                                        conversation_recovery_button(
                                                            ("mark-diagnostic-failed", index),
                                                            "Mark failed",
                                                            "Resolve the pending recovery as failed",
                                                            failed_identity,
                                                            DesktopRecoveryAction::MarkFailed,
                                                            cx,
                                                        )
                                                        .debug_selector(|| {
                                                            "desktop-mark-failed-diagnostic".into()
                                                        }),
                                                    )
                                                    .child(
                                                        conversation_recovery_button(
                                                            ("abort-diagnostic", index),
                                                            "Abort",
                                                            "Resolve the pending recovery as aborted",
                                                            identity,
                                                            DesktopRecoveryAction::Abort,
                                                            cx,
                                                        )
                                                        .debug_selector(|| {
                                                            "desktop-abort-diagnostic".into()
                                                        }),
                                                    ),
                                            )
                                        })
                                        .when(block.preview_truncated && !is_tool, |card| {
                                            card.child(
                                                div()
                                                    .absolute()
                                                    .left_0()
                                                    .right_0()
                                                    .bottom_0()
                                                    .flex()
                                                    .items_center()
                                                    .gap_token(DesignSpace::Sm)
                                                    .border_t_1()
                                                    .border_color(rgb(theme.warning.value()))
                                                    .bg(rgb(theme.elevated.value()))
                                                    .px_token(DesignSpace::Lg)
                                                    .py_token(DesignSpace::Sm)
                                                    .text_color(rgb(theme.warning.value()))
                                                    .when(!visual.align_right && show_generic_copy, |actions| {
                                                        actions.child(conversation_copy_button(
                                                            ("copy-conversation-row", index),
                                                            copy_block_id.clone(),
                                                            hover_group.clone(),
                                                            selected,
                                                            cx,
                                                        ))
                                                    })
                                                    .child(
                                                        div()
                                                            .flex_1()
                                                            .child("! preview truncated at desktop safety limit"),
                                                    )
                                                    .child(
                                                        div()
                                                            .flex()
                                                            .items_center()
                                                            .gap_token(DesignSpace::Xs)
                                                            .when(visual.align_right && show_generic_copy, |actions| {
                                                                actions.child(
                                                                    conversation_copy_button(
                                                                        (
                                                                            "copy-conversation-row",
                                                                            index,
                                                                        ),
                                                                        copy_block_id.clone(),
                                                                        hover_group.clone(),
                                                                        selected,
                                                                        cx,
                                                                    ),
                                                                )
                                                            })
                                                            .child(
                                                                DesktopIconButton::new(
                                                                    ("open-full-message", index),
                                                                    DesktopIcon::Expand,
                                                                    "Open the complete bounded message source",
                                                                )
                                                                .build()
                                                                .debug_selector(|| {
                                                                    "desktop-open-full-message".into()
                                                                })
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
                                                    .h(px(CONVERSATION_TAIL_MARKER_HEIGHT)),
                                            )
                                        })
                                        .when(!block.preview_truncated && show_generic_copy, |card| {
                                            card.child(
                                                div()
                                                    .w_full()
                                                    .flex()
                                                    .items_center()
                                                    .gap_token(DesignSpace::Sm)
                                                    .when(visual.align_right, |actions| {
                                                        actions.justify_end()
                                                    })
                                                    .child(conversation_copy_button(
                                                        ("copy-conversation-row", index),
                                                        copy_block_id.clone(),
                                                        hover_group.clone(),
                                                        selected,
                                                        cx,
                                                    ))
                                                    .when_some(turn_label, |actions, label| {
                                                        actions.child(
                                                            div()
                                                                .id(("turn-metadata", index))
                                                                .debug_selector(|| {
                                                                    "desktop-turn-metadata".into()
                                                                })
                                                                .flex_shrink_0()
                                                                .text_token(DesignText::Metadata)
                                                                .text_color(rgb(
                                                                    theme.muted_text.value(),
                                                                ))
                                                                .opacity(0.)
                                                                .group_hover(
                                                                    hover_group.clone(),
                                                                    |style| style.opacity(1.),
                                                                )
                                                                .when(selected, |metadata| {
                                                                    metadata.opacity(1.)
                                                                })
                                                                .child(label),
                                                        )
                                                    }),
                                            )
                                        }),
                                        ),
                                )
                };
                pane.evict_markdown_states();
                row.into_any_element()
            })
        })
        .w_full()
        .h_full();

        let follow_latest_label = follow_latest_label(unseen_updates);
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
                content.child(empty_conversation(
                    event_count,
                    message_count,
                    tool_count,
                    theme,
                ))
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
