use super::*;
use crate::ui::conversation::model::DelegationMeta;
use crate::ui::shell::SemanticColor;

pub(super) struct IdentityHeaderArgs {
    pub(super) show_identity_header: bool,
    pub(super) index: usize,
    pub(super) row_count: usize,
    pub(super) is_tool: bool,
    pub(super) tool_expandable: bool,
    pub(super) is_delegation: bool,
    pub(super) detail_expanded: bool,
    pub(super) select_block_id: String,
    pub(super) durable: bool,
    pub(super) tool_header_toggle_block_id: String,
    pub(super) tool_label: Option<&'static str>,
    pub(super) tool_summary_text: String,
    pub(super) hover_group: SharedString,
    pub(super) delegation: Option<DelegationMeta>,
    pub(super) visual_glyph: &'static str,
    pub(super) visual_accent: SemanticColor,
    pub(super) delegation_summary_text: String,
    pub(super) delegation_expandable: bool,
    pub(super) terminal_label: Option<&'static str>,
    pub(super) block_kind: ConversationBlockKind,
    pub(super) block_title: Arc<str>,
    pub(super) theme: SemanticTheme,
}

pub(super) struct ReasoningArgs {
    pub(super) index: usize,
    pub(super) is_assistant: bool,
    pub(super) detail_text: Arc<str>,
    pub(super) detail_expanded: bool,
    pub(super) reasoning_duration_label: Option<String>,
    pub(super) block_done: bool,
    pub(super) reasoning_collapsed_toggle_block_id: String,
    pub(super) reasoning_collapsed_chevron_block_id: String,
    pub(super) reasoning_expanded_toggle_block_id: String,
    pub(super) reasoning_expanded_chevron_block_id: String,
    pub(super) detail_markdown_key: Arc<str>,
    pub(super) theme: SemanticTheme,
}

pub(super) trait ConversationCardExt {
    fn with_identity_header(
        self,
        args: IdentityHeaderArgs,
        cx: &gpui::Context<ConversationPane>,
    ) -> Self;

    fn with_reasoning(
        self,
        args: ReasoningArgs,
        pane: &mut ConversationPane,
        cx: &mut gpui::Context<ConversationPane>,
    ) -> Self;
}

impl ConversationCardExt for gpui::Stateful<gpui::Div> {
    fn with_identity_header(
        self,
        args: IdentityHeaderArgs,
        cx: &gpui::Context<ConversationPane>,
    ) -> Self {
        let IdentityHeaderArgs {
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
            hover_group,
            delegation,
            visual_glyph,
            visual_accent,
            delegation_summary_text,
            delegation_expandable,
            terminal_label,
            block_kind,
            block_title,
            theme,
        } = args;
        self.when(show_identity_header, |card| {
            card.child(
                div()
                    .id(("conversation-row-header", index))
                    .debug_selector(|| "desktop-conversation-row-header".into())
                    .when(index + 1 == row_count, |header| {
                        header.debug_selector(|| "desktop-last-conversation-row-header".into())
                    })
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_token(DesignSpace::Md)
                    .when(is_tool && tool_expandable, |header| {
                        header.debug_selector(|| "desktop-tool-toggle-header".into())
                    })
                    .when(is_delegation, |header| {
                        header.debug_selector(|| "desktop-delegation-toggle-header".into())
                    })
                    .child(
                        div()
                            .id(("conversation-row-main", index))
                            .flex()
                            .flex_1()
                            .min_w_0()
                            .items_center()
                            .gap_token(DesignSpace::Sm)
                            .when(is_tool && tool_expandable, |surface| {
                                surface
                                    .role(Role::Button)
                                    .aria_label("Show or hide tool output and arguments")
                                    .aria_expanded(detail_expanded)
                            })
                            .when(is_delegation, |surface| {
                                surface
                                    .role(Role::Button)
                                    .aria_label("Show or hide delegation details")
                                    .aria_expanded(detail_expanded)
                            })
                            .on_click(cx.listener(move |_, _, _, cx| {
                                cx.emit(ConversationPaneEvent::Select {
                                    block_id: select_block_id.clone(),
                                    durable,
                                });
                                if (is_tool && tool_expandable) || is_delegation {
                                    cx.emit(ConversationPaneEvent::ToggleDetails {
                                        block_id: tool_header_toggle_block_id.clone(),
                                    });
                                }
                            }))
                            .when(is_tool, |main| {
                                main.child(
                                    div()
                                        .px_token(DesignSpace::Sm)
                                        .py_token(DesignSpace::Xs)
                                        .text_token(DesignText::Metadata)
                                        .font_weight(gpui::FontWeight::SEMIBOLD)
                                        .text_color(rgb(theme.text.value()))
                                        .child(tool_label.unwrap_or("Tool")),
                                )
                                .child(
                                    div()
                                        .text_token(DesignText::Body)
                                        .font_weight(gpui::FontWeight::MEDIUM)
                                        .text_color(rgb(theme.muted_text.value()))
                                        .min_w_0()
                                        .truncate()
                                        .child(SharedString::new(tool_summary_text.clone())),
                                )
                                .when(tool_expandable, |main| {
                                    main.child(
                                        div()
                                            .id(("tool-toggle-details", index))
                                            .debug_selector(|| "desktop-toggle-tool-details".into())
                                            .flex_shrink_0()
                                            .w(px(32.))
                                            .h(px(32.))
                                            .flex()
                                            .items_center()
                                            .justify_center()
                                            .text_token(DesignText::Metadata)
                                            .text_color(rgb(theme.muted_text.value()))
                                            .opacity(0.)
                                            .group_hover(hover_group.clone(), |style| {
                                                style.opacity(1.)
                                            })
                                            .child(
                                                Icon::new(
                                                    tool_disclosure_icon(detail_expanded).name(),
                                                )
                                                .small(),
                                            ),
                                    )
                                })
                            })
                            .when(is_delegation, |main| {
                                main.child(
                                    div()
                                        .px_token(DesignSpace::Sm)
                                        .py_token(DesignSpace::Xs)
                                        .text_token(DesignText::Metadata)
                                        .font_weight(gpui::FontWeight::SEMIBOLD)
                                        .text_color(rgb(visual_accent.value()))
                                        .child(visual_glyph),
                                )
                                .when_some(delegation, |main, meta| {
                                    main.child(
                                        div()
                                            .text_token(DesignText::Body)
                                            .font_weight(gpui::FontWeight::MEDIUM)
                                            .text_color(rgb(theme.text.value()))
                                            .min_w_0()
                                            .truncate()
                                            .child(SharedString::new(&meta.target_id)),
                                    )
                                    .child(
                                        div()
                                            .flex_shrink_0()
                                            .text_token(DesignText::Metadata)
                                            .text_color(delegation_status_color(meta.status, theme))
                                            .child(meta.status.label()),
                                    )
                                })
                                .child(
                                    div()
                                        .text_token(DesignText::Body)
                                        .font_weight(gpui::FontWeight::MEDIUM)
                                        .text_color(rgb(theme.muted_text.value()))
                                        .min_w_0()
                                        .truncate()
                                        .child(SharedString::new(delegation_summary_text.clone())),
                                )
                                .when(
                                    delegation_expandable,
                                    |main| {
                                        main.child(
                                            div()
                                                .id(("delegation-toggle-details", index))
                                                .debug_selector(|| {
                                                    "desktop-toggle-delegation-details".into()
                                                })
                                                .flex_shrink_0()
                                                .w(px(32.))
                                                .h(px(32.))
                                                .flex()
                                                .items_center()
                                                .justify_center()
                                                .text_token(DesignText::Metadata)
                                                .text_color(rgb(theme.muted_text.value()))
                                                .opacity(0.)
                                                .group_hover(hover_group.clone(), |style| {
                                                    style.opacity(1.)
                                                })
                                                .child(
                                                    Icon::new(
                                                        tool_disclosure_icon(detail_expanded)
                                                            .name(),
                                                    )
                                                    .small(),
                                                ),
                                        )
                                    },
                                )
                            })
                            .when(!is_tool && !is_delegation, |main| {
                                main.child(
                                    div()
                                        .px_token(DesignSpace::Sm)
                                        .py_token(DesignSpace::Xs)
                                        .text_token(DesignText::Metadata)
                                        .font_weight(gpui::FontWeight::SEMIBOLD)
                                        .text_color(rgb(visual_accent.value()))
                                        .child(visual_glyph),
                                )
                                .when(
                                    block_kind != ConversationBlockKind::User,
                                    |main| {
                                        main.child(
                                            div()
                                                .text_token(DesignText::Body)
                                                .font_weight(gpui::FontWeight::MEDIUM)
                                                .text_color(rgb(theme.text.value()))
                                                .child(SharedString::new(block_title.clone())),
                                        )
                                    },
                                )
                            }),
                    )
                    .when_some(terminal_label.filter(|_| !is_tool), |header, label| {
                        header.child(
                            div()
                                .flex_shrink_0()
                                .text_token(DesignText::Metadata)
                                .text_color(rgb(visual_accent.value()))
                                .child(label),
                        )
                    }),
            )
        })
    }

    fn with_reasoning(
        self,
        args: ReasoningArgs,
        pane: &mut ConversationPane,
        cx: &mut gpui::Context<ConversationPane>,
    ) -> Self {
        let ReasoningArgs {
            index,
            is_assistant,
            detail_text,
            detail_expanded,
            reasoning_duration_label,
            block_done,
            reasoning_collapsed_toggle_block_id,
            reasoning_collapsed_chevron_block_id,
            reasoning_expanded_toggle_block_id,
            reasoning_expanded_chevron_block_id,
            detail_markdown_key,
            theme,
        } = args;
        self.when(
            is_assistant && !detail_text.is_empty() && !detail_expanded,
            |card| {
                card.child(
                    div()
                        .id(("reasoning-toggle", index))
                        .debug_selector(|| "desktop-reasoning-toggle-header".into())
                        .ml_token(DesignSpace::Sm)
                        .pl_token(DesignSpace::Lg)
                        .pr_token(DesignSpace::Sm)
                        .py_token(DesignSpace::Sm)
                        .border_l_1()
                        .border_color(rgb(theme.divider.value()))
                        .flex()
                        .items_center()
                        .justify_between()
                        .text_token(DesignText::Metadata)
                        .text_color(rgb(theme.muted_text.value()))
                        .child(
                            div()
                                .id(("reasoning-toggle-main", index))
                                .flex_1()
                                .min_w_0()
                                .whitespace_normal()
                                .role(Role::Button)
                                .aria_label("Show thoughts")
                                .aria_expanded(false)
                                .on_click(cx.listener(move |_, _, _, cx| {
                                    cx.emit(ConversationPaneEvent::ToggleDetails {
                                        block_id: reasoning_collapsed_toggle_block_id.clone(),
                                    });
                                }))
                                .child(if block_done {
                                    match &reasoning_duration_label {
                                        Some(duration) => {
                                            SharedString::new(format!("Thought for {duration}"))
                                        }
                                        None => SharedString::new("Thought"),
                                    }
                                } else {
                                    SharedString::new("Thinking")
                                }),
                        )
                        .child(
                            DesktopIconButton::new(
                                ("show-reasoning", index),
                                DesktopIcon::ChevronDown,
                                "Show thoughts",
                            )
                            .build()
                            .debug_selector(|| "desktop-toggle-reasoning-details".into())
                            .on_click(cx.listener(
                                move |_, _, _, cx| {
                                    cx.emit(ConversationPaneEvent::ToggleDetails {
                                        block_id: reasoning_collapsed_chevron_block_id.clone(),
                                    });
                                },
                            )),
                        ),
                )
            },
        )
        .when(
            is_assistant && !detail_text.is_empty() && detail_expanded,
            |card| {
                card.child(
                    div()
                        .ml_token(DesignSpace::Sm)
                        .pl_token(DesignSpace::Lg)
                        .pr_token(DesignSpace::Sm)
                        .py_token(DesignSpace::Sm)
                        .border_l_1()
                        .border_color(rgb(theme.divider.value()))
                        .flex()
                        .flex_col()
                        .gap_token(DesignSpace::Xs)
                        .child(
                            div()
                                .id(("reasoning-toggle-expanded", index))
                                .debug_selector(|| "desktop-reasoning-toggle-header".into())
                                .flex()
                                .items_center()
                                .justify_between()
                                .text_token(DesignText::Metadata)
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .text_color(rgb(theme.muted_text.value()))
                                .child(
                                    div()
                                        .id(("reasoning-toggle-expanded-main", index))
                                        .flex_1()
                                        .min_w_0()
                                        .role(Role::Button)
                                        .aria_label("Hide thoughts")
                                        .aria_expanded(true)
                                        .on_click(cx.listener(move |_, _, _, cx| {
                                            cx.emit(ConversationPaneEvent::ToggleDetails {
                                                block_id: reasoning_expanded_toggle_block_id
                                                    .clone(),
                                            });
                                        }))
                                        .child(if block_done {
                                            match &reasoning_duration_label {
                                                Some(duration) => SharedString::new(format!(
                                                    "Thought for {duration}"
                                                )),
                                                None => SharedString::new("Thought"),
                                            }
                                        } else {
                                            SharedString::new("Thinking")
                                        }),
                                )
                                .child(
                                    DesktopIconButton::new(
                                        ("hide-reasoning", index),
                                        DesktopIcon::ChevronUp,
                                        "Hide thoughts",
                                    )
                                    .build()
                                    .debug_selector(|| "desktop-toggle-reasoning-details".into())
                                    .on_click(cx.listener(
                                        move |_, _, _, cx| {
                                            cx.emit(ConversationPaneEvent::ToggleDetails {
                                                block_id: reasoning_expanded_chevron_block_id
                                                    .clone(),
                                            });
                                        },
                                    )),
                                ),
                        )
                        .child(
                            StreamingText::new(
                                pane.markdown_state(&detail_markdown_key, &detail_text, cx),
                                cx.entity().downgrade(),
                            )
                            .into_any_element(),
                        ),
                )
            },
        )
    }
}
