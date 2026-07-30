use gpui::{
    ElementId, Entity, EventEmitter, IntoElement, ParentElement as _, Render, Role, SharedString,
    Styled as _, Window, div, prelude::*, px, relative, rgb,
};
use gpui_component::{
    ElementExt as _, VirtualListScrollHandle, button::Button, text::TextViewState, v_virtual_list,
};
use std::{
    collections::{HashMap, HashSet},
    rc::Rc,
    sync::Arc,
    time::Instant,
};

use super::{
    ConversationBlockKind, conversation_block_visual,
    conversation_controller::ConversationRenderReader,
    desktop_controls::{
        DesktopCriticalButton, DesktopCriticalTone, DesktopIcon, DesktopIconButton,
    },
    desktop_style::{DesignRadius, DesignSpace, DesignText, DesktopStyledExt as _},
    streaming_text::{StreamingText, markdown_completion_trace_enabled, trace_markdown_parse},
};
use desktop::conversation::{
    ConversationRowMeasurement, TRANSCRIPT_COLLAPSED_PREVIEW_MAX_HEIGHT, compact_duration,
};
use desktop::runtime::{DesktopRecoveryAction, DesktopRecoveryIdentity};
use desktop::shell::{
    ASSISTANT_MESSAGE_MAX_WIDTH, CONVERSATION_ROW_VERTICAL_PADDING_PX, MONOSPACE_FONT_FAMILY,
    SemanticTheme, USER_MESSAGE_MAX_WIDTH, USER_MESSAGE_WIDTH_PERCENT,
};

/// Width of the leading rail that carries conversation selection now that
/// blocks no longer paint a card background.
pub(super) const CONVERSATION_RAIL_WIDTH: f32 = 2.;

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

#[derive(Clone)]
pub(super) struct ConversationPaneViewModel {
    pub(super) render: ConversationRenderReader,
    pub(super) scroll: VirtualListScrollHandle,
    pub(super) visible_count: usize,
    pub(super) event_count: usize,
    pub(super) message_count: usize,
    pub(super) tool_count: usize,
    pub(super) omitted_count: usize,
    pub(super) follow_latest: bool,
    pub(super) unseen_updates: usize,
    pub(super) selected_block_id: Option<String>,
    pub(super) expanded_details: Rc<HashSet<String>>,
    pub(super) full_view_block_id: Option<String>,
    pub(super) diagnostic_recovery: Option<DesktopRecoveryIdentity>,
}

/// Markdown parse states outlive the frame that rendered them so a streaming row
/// can extend its document instead of re-parsing it.
///
/// Only rows the virtual list actually renders get one, so the live set is
/// bounded by the viewport; this cap is the backstop for scrolling churn.
const MAX_MARKDOWN_PARSE_STATES: usize = 64;

/// One row body's parsed Markdown, plus exactly what has been fed to it.
struct MarkdownParseState {
    state: Entity<TextViewState>,
    /// The text `state` currently holds. Kept as the same `Arc` the render cache
    /// hands out, so an unchanged row costs one pointer comparison.
    fed: Arc<str>,
    touched: u64,
}

pub(super) struct ConversationPane {
    view_model: Option<ConversationPaneViewModel>,
    markdown_states: HashMap<Arc<str>, MarkdownParseState>,
    markdown_generation: u64,
}

impl ConversationPane {
    pub(super) fn new() -> Self {
        Self {
            view_model: None,
            markdown_states: HashMap::new(),
            markdown_generation: 0,
        }
    }

    pub(super) fn set_view_model(&mut self, view_model: ConversationPaneViewModel) {
        self.view_model = Some(view_model);
    }

    /// Resolve the parse state for a row body, feeding it the smallest update
    /// that gets it to `text`.
    ///
    /// A streaming revision only appends, so the common case is a suffix push,
    /// which `TextViewState` parses incrementally on a background task while the
    /// previous document stays laid out. Anything that is not an extension —
    /// completion replacing the text with its sanitised form, a rewind, a
    /// branch — falls back to a full replace, which parses synchronously so the
    /// very next layout already has correct geometry.
    fn markdown_state(
        &mut self,
        key: &Arc<str>,
        text: &Arc<str>,
        cx: &mut gpui::Context<Self>,
    ) -> Entity<TextViewState> {
        let generation = self.markdown_generation;
        if let Some(existing) = self.markdown_states.get_mut(key) {
            existing.touched = generation;
            if !Arc::ptr_eq(&existing.fed, text) && existing.fed != *text {
                let appended = (text.len() > existing.fed.len()
                    && text.starts_with(existing.fed.as_ref()))
                .then(|| &text[existing.fed.len()..]);
                match appended {
                    Some(suffix) => {
                        let suffix = suffix.to_owned();
                        existing
                            .state
                            .update(cx, |state, cx| state.push_str(&suffix, cx));
                    }
                    None => {
                        let replacement = Arc::clone(text);
                        let traced = markdown_completion_trace_enabled();
                        let started_at = traced.then(Instant::now);
                        existing
                            .state
                            .update(cx, |state, cx| state.set_text(&replacement, cx));
                        if let Some(started_at) = started_at {
                            trace_markdown_parse(key, replacement.len(), started_at);
                        }
                    }
                }
                existing.fed = Arc::clone(text);
            }
            return existing.state.clone();
        }

        let initial = Arc::clone(text);
        let started_at = markdown_completion_trace_enabled().then(Instant::now);
        let state = cx.new(|cx| TextViewState::markdown(&initial, cx));
        if let Some(started_at) = started_at {
            trace_markdown_parse(key, initial.len(), started_at);
        }
        self.markdown_states.insert(
            Arc::clone(key),
            MarkdownParseState {
                state: state.clone(),
                fed: initial,
                touched: generation,
            },
        );
        state
    }

    /// Drop parse states no recent frame rendered, newest generations first.
    fn evict_markdown_states(&mut self) {
        while self.markdown_states.len() > MAX_MARKDOWN_PARSE_STATES {
            let Some(stale) = self
                .markdown_states
                .iter()
                .min_by_key(|(_, entry)| entry.touched)
                .map(|(key, _)| Arc::clone(key))
            else {
                break;
            };
            self.markdown_states.remove(&stale);
        }
    }
}

impl EventEmitter<ConversationPaneEvent> for ConversationPane {}

impl Render for ConversationPane {
    fn render(&mut self, _window: &mut Window, cx: &mut gpui::Context<Self>) -> impl IntoElement {
        let Some(view_model) = self.view_model.clone() else {
            return div()
                .id("conversation-log")
                .role(Role::Log)
                .aria_label("Conversation messages")
                .flex_1();
        };
        let transcript_rows = view_model.render.row_sizes();
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
        let transcript_list = v_virtual_list(
            cx.entity(),
            "conversation-transcript",
            transcript_rows,
            move |pane, visible_range, _window, cx| {
                pane.markdown_generation = pane.markdown_generation.wrapping_add(1);
                let rows = visible_range
                    .filter_map(|index| {
                        let (block, row_height) = render.row(index)?;
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
                        let block_id = block.item_key.row_id().to_owned();
                        let select_block_id = block_id.clone();
                        let copy_block_id = block_id.clone();
                        let copy_tool_command_block_id = block_id.clone();
                        let copy_tool_output_block_id = block_id.clone();
                        let tool_header_toggle_block_id = block_id.clone();
                        let tool_chevron_select_block_id = block_id.clone();
                        let tool_chevron_toggle_block_id = block_id.clone();
                        let reasoning_collapsed_toggle_block_id = block_id.clone();
                        let reasoning_collapsed_chevron_block_id = block_id.clone();
                        let reasoning_expanded_toggle_block_id = block_id.clone();
                        let reasoning_expanded_chevron_block_id = block_id.clone();
                        let full_block_id = block_id.clone();
                        let open_tool_output_block_id = block_id.clone();
                        let hover_group = SharedString::new(format!(
                            "conversation-card:{}",
                            block.item_key.stable_id()
                        ));
                        let durable = block.durable;
                        let markdown_key = block.markdown_state_key.clone();
                        let detail_markdown_key = block.detail_markdown_state_key.clone();
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
                        let is_assistant = block.kind == ConversationBlockKind::Assistant;
                        let reasoning_duration_label = block
                            .reasoning_duration_millis
                            .map(compact_duration);
                        let is_tool = block.kind == ConversationBlockKind::Tool;
                        let tool_command = is_tool
                            .then(|| structured_tool_command(&block.detail, &block.text))
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
                        // Selection paints a full-height leading rail; hover no
                        // longer paints a stub. The rail is absolutely
                        // positioned and never affects row height.
                        let selection_rail = selected.then(|| {
                            div()
                                .debug_selector(|| "conversation-selected-rail".to_owned())
                                .absolute()
                                .left_0()
                                .top_0()
                                .bottom_0()
                                .w(px(CONVERSATION_RAIL_WIDTH))
                                .bg(rgb(theme.focus_ring.value()))
                        });
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
                                        .px_token(DesignSpace::Lg)
                                        .py_token(DesignSpace::Md)
                                        .flex()
                                        .flex_col()
                                        .gap_token(DesignSpace::Sm)
                                        .when_some(selection_rail, |card, rail| card.child(rail))
                                        .child(
                                            div()
                                                .id(("conversation-row-header", index))
                                                .debug_selector(|| {
                                                    "desktop-conversation-row-header".into()
                                                })
                                                .flex()
                                                .items_center()
                                                .justify_between()
                                                .gap_token(DesignSpace::Md)
                                                .pr_12()
                                                .when(
                                                    is_tool && has_collapsible_detail,
                                                    |header| {
                                                        header.debug_selector(|| {
                                                            "desktop-tool-toggle-header".into()
                                                        })
                                                    },
                                                )
                                                .child(
                                                    div()
                                                        .id(("conversation-row-main", index))
                                                        .flex()
                                                        .flex_1()
                                                        .min_w_0()
                                                        .items_center()
                                                        .gap_token(DesignSpace::Sm)
                                                        .when(
                                                            is_tool && has_collapsible_detail,
                                                            |surface| {
                                                                surface
                                                                    .role(Role::Button)
                                                                    .aria_label(
                                                                        "Show or hide tool output and arguments",
                                                                    )
                                                                    .aria_expanded(detail_expanded)
                                                            },
                                                        )
                                                        .on_click(cx.listener(
                                                            move |_, _, _, cx| {
                                                                cx.emit(
                                                                    ConversationPaneEvent::Select {
                                                                        block_id: select_block_id
                                                                            .clone(),
                                                                        durable,
                                                                    },
                                                                );
                                                                if is_tool
                                                                    && has_collapsible_detail
                                                                {
                                                                    cx.emit(
                                                                        ConversationPaneEvent::ToggleDetails {
                                                                            block_id: tool_header_toggle_block_id.clone(),
                                                                        },
                                                                    );
                                                                }
                                                            },
                                                        ))
                                                        .child(
                                                            div()
                                                                // Keep the former label padding so
                                                                // removing its filled chip does not
                                                                // perturb measured row height. Role
                                                                // identity now comes from the leading
                                                                // glyph and text tone alone.
                                                                .px_token(DesignSpace::Sm)
                                                                .py_token(DesignSpace::Xs)
                                                                .text_token(DesignText::Metadata)
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
                                                                .text_token(DesignText::Body)
                                                                .font_weight(
                                                                    gpui::FontWeight::MEDIUM,
                                                                )
                                                                .text_color(rgb(theme.text.value()))
                                                                .child(SharedString::new(
                                                                    block.title.clone(),
                                                                )),
                                                        )
                                                        .when_some(
                                                            terminal_label.filter(|_| is_tool),
                                                            |surface, label| {
                                                                surface.child(
                                                                    div()
                                                                        .flex_shrink_0()
                                                                        .text_token(
                                                                            DesignText::Metadata,
                                                                        )
                                                                        .text_color(rgb(
                                                                            visual.accent.value(),
                                                                        ))
                                                                        .child(label),
                                                                )
                                                            },
                                                        ),
                                                )
                                                .when_some(
                                                    terminal_label.filter(|_| !is_tool),
                                                    |header, label| {
                                                        header.child(
                                                            div()
                                                                .flex_shrink_0()
                                                                .text_token(DesignText::Metadata)
                                                                .text_color(rgb(
                                                                    visual.accent.value(),
                                                                ))
                                                                .child(label),
                                                        )
                                                    },
                                                )
                                                .when(
                                                    is_tool && has_collapsible_detail,
                                                    |header| {
                                                        header.child(
                                                            DesktopIconButton::new(
                                                                ("toggle-tool-details", index),
                                                                if detail_expanded {
                                                                    DesktopIcon::ChevronUp
                                                                } else {
                                                                    DesktopIcon::ChevronDown
                                                                },
                                                                if detail_expanded {
                                                                    "Hide tool output and arguments"
                                                                } else {
                                                                    "Show tool output and arguments"
                                                                },
                                                            )
                                                            .build()
                                                            .debug_selector(|| {
                                                                "desktop-toggle-tool-details".into()
                                                            })
                                                            .on_click(cx.listener(
                                                                move |_, _, _, cx| {
                                                                    cx.emit(
                                                                        ConversationPaneEvent::Select {
                                                                            block_id: tool_chevron_select_block_id.clone(),
                                                                            durable,
                                                                        },
                                                                    );
                                                                    cx.emit(
                                                                        ConversationPaneEvent::ToggleDetails {
                                                                            block_id: tool_chevron_toggle_block_id.clone(),
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
                                                        .id(("reasoning-toggle", index))
                                                        .debug_selector(|| {
                                                            "desktop-reasoning-toggle-header".into()
                                                        })
                                                        .rounded_token(DesignRadius::Md)
                                                        .border_l_3()
                                                        .border_color(rgb(theme.reasoning.value()))
                                                        .bg(rgb(theme.elevated.value()))
                                                        .px_token(DesignSpace::Md)
                                                        .py_token(DesignSpace::Sm)
                                                        .flex()
                                                        .items_center()
                                                        .justify_between()
                                                        .text_token(DesignText::Metadata)
                                                        .text_color(rgb(theme.reasoning.value()))
                                                        .child(
                                                            div()
                                                                .id(("reasoning-toggle-main", index))
                                                                .flex_1()
                                                                .min_w_0()
                                                                .whitespace_normal()
                                                                .role(Role::Button)
                                                                .aria_label(
                                                                    "Show reasoning details",
                                                                )
                                                                .aria_expanded(false)
                                                                .on_click(cx.listener(
                                                                    move |_, _, _, cx| {
                                                                        cx.emit(
                                                                            ConversationPaneEvent::ToggleDetails {
                                                                                block_id: reasoning_collapsed_toggle_block_id.clone(),
                                                                            },
                                                                        );
                                                                    },
                                                                ))
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
                                                            DesktopIconButton::new(
                                                                ("show-reasoning", index),
                                                                DesktopIcon::ChevronDown,
                                                                "Show reasoning details",
                                                            )
                                                            .build()
                                                            .debug_selector(|| {
                                                                "desktop-toggle-reasoning-details"
                                                                    .into()
                                                            })
                                                            .on_click(cx.listener(
                                                                move |_, _, _, cx| {
                                                                    cx.emit(
                                                                        ConversationPaneEvent::ToggleDetails {
                                                                            block_id: reasoning_collapsed_chevron_block_id.clone(),
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
                                                    .bg(rgb(theme.elevated.value()))
                                                    .px_token(DesignSpace::Md)
                                                    .py_token(DesignSpace::Sm)
                                                    .flex()
                                                    .flex_col()
                                                    .gap_token(DesignSpace::Xs)
                                                    .child(
                                                        div()
                                                            .id(("reasoning-toggle-expanded", index))
                                                            .debug_selector(|| {
                                                                "desktop-reasoning-toggle-header"
                                                                    .into()
                                                            })
                                                            .flex()
                                                            .items_center()
                                                            .justify_between()
                                                            .text_token(DesignText::Metadata)
                                                            .font_weight(
                                                                gpui::FontWeight::SEMIBOLD,
                                                            )
                                                            .text_color(rgb(
                                                                theme.reasoning.value(),
                                                            ))
                                                            .child(
                                                                div()
                                                                    .id((
                                                                        "reasoning-toggle-expanded-main",
                                                                        index,
                                                                    ))
                                                                    .flex_1()
                                                                    .min_w_0()
                                                                    .role(Role::Button)
                                                                    .aria_label(
                                                                        "Hide reasoning details",
                                                                    )
                                                                    .aria_expanded(true)
                                                                    .on_click(cx.listener(
                                                                        move |_, _, _, cx| {
                                                                            cx.emit(
                                                                                ConversationPaneEvent::ToggleDetails {
                                                                                    block_id: reasoning_expanded_toggle_block_id.clone(),
                                                                                },
                                                                            );
                                                                        },
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
                                                                DesktopIconButton::new(
                                                                    ("hide-reasoning", index),
                                                                    DesktopIcon::ChevronUp,
                                                                    "Hide reasoning details",
                                                                )
                                                                .build()
                                                                .debug_selector(|| {
                                                                    "desktop-toggle-reasoning-details"
                                                                        .into()
                                                                })
                                                                .on_click(cx.listener(
                                                                    move |_, _, _, cx| {
                                                                        cx.emit(
                                                                            ConversationPaneEvent::ToggleDetails {
                                                                                block_id: reasoning_expanded_chevron_block_id.clone(),
                                                                            },
                                                                        );
                                                                    },
                                                                )),
                                                            ),
                                                    )
                                                    .child(
                                                        StreamingText::new(
                                                            detail_text.clone(),
                                                            text_phase,
                                                            text_phase.renders_markdown().then(|| {
                                                                pane.markdown_state(
                                                                    &detail_markdown_key,
                                                                    &detail_text,
                                                                    cx,
                                                                )
                                                            }),
                                                            cx.entity().downgrade(),
                                                        )
                                                        .into_any_element(),
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
                                                                    DesktopIconButton::new(
                                                                        ("copy-tool-command", index),
                                                                        DesktopIcon::Copy,
                                                                        "Copy the complete bounded tool command",
                                                                    )
                                                                        .build()
                                                                        .debug_selector(|| {
                                                                            "desktop-copy-tool-command".into()
                                                                        })
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
                                                                        DesktopIconButton::new(
                                                                            ("copy-tool-output", index),
                                                                            DesktopIcon::Copy,
                                                                            "Copy the complete bounded tool output",
                                                                        )
                                                                            .build()
                                                                            .debug_selector(|| {
                                                                                "desktop-copy-tool-output".into()
                                                                            })
                                                                            .on_click(cx.listener(move |_, _, _, cx| {
                                                                                cx.stop_propagation();
                                                                                cx.emit(ConversationPaneEvent::CopyToolOutput {
                                                                                    block_id: copy_tool_output_block_id.clone(),
                                                                                });
                                                                            })),
                                                                    )
                                                                    .child(
                                                                        DesktopIconButton::new(
                                                                            ("open-tool-output", index),
                                                                            DesktopIcon::Expand,
                                                                            "Open the complete bounded tool output",
                                                                        )
                                                                            .build()
                                                                            .debug_selector(|| {
                                                                                "desktop-open-tool-output".into()
                                                                            })
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
                                                text.clone(),
                                                text_phase,
                                                text_phase.renders_markdown().then(|| {
                                                    pane.markdown_state(&markdown_key, &text, cx)
                                                }),
                                                cx.entity().downgrade(),
                                            )
                                            .into_any_element();
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
                                                    .border_l_1()
                                                    .border_color(rgb(theme.divider.value()))
                                                    .pl_token(DesignSpace::Md)
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
                                                            detail_text.clone(),
                                                            text_phase,
                                                            text_phase.renders_markdown().then(|| {
                                                                pane.markdown_state(
                                                                    &detail_markdown_key,
                                                                    &detail_text,
                                                                    cx,
                                                                )
                                                            }),
                                                            cx.entity().downgrade(),
                                                        )
                                                        .into_any_element(),
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
                                        .when(block.preview_truncated, |card| {
                                            card.child(
                                                div()
                                                    .absolute()
                                                    .left_0()
                                                    .right_0()
                                                    .bottom_0()
                                                    .flex()
                                                    .items_center()
                                                    .justify_between()
                                                    .gap_token(DesignSpace::Sm)
                                                    .border_t_1()
                                                    .border_color(rgb(theme.warning.value()))
                                                    .bg(rgb(theme.elevated.value()))
                                                    .px_token(DesignSpace::Lg)
                                                    .py_token(DesignSpace::Sm)
                                                    .text_color(rgb(theme.warning.value()))
                                                    .child(
                                                        "! preview truncated at desktop safety limit",
                                                    )
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
                                                .child(
                                                    conversation_hover_tool(
                                                        DesktopIconButton::new(
                                                            ("copy-conversation-row", index),
                                                            DesktopIcon::Copy,
                                                            "Copy this bounded message",
                                                        )
                                                        .build()
                                                        .debug_selector(|| {
                                                            "desktop-copy-conversation-row".into()
                                                        })
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
                                                        hover_group,
                                                        selected,
                                                    ),
                                                ),
                                        ),
                                ),
                        )
                    })
                    .collect::<Vec<_>>();
                pane.evict_markdown_states();
                rows
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

fn conversation_hover_tool(button: Button, hover_group: SharedString, selected: bool) -> Button {
    // Keep the button paint-visible with zero opacity instead of using
    // `visibility: hidden`: GPUI registers tab stops during paint after its
    // hidden-visibility early return, so an invisible button could never
    // receive keyboard focus to reveal itself.
    button
        .opacity(0.)
        .group_hover(hover_group, |style| style.opacity(1.))
        .focus(|style| style.opacity(1.))
        .when(selected, |button| button.opacity(1.))
}

fn conversation_recovery_button(
    id: impl Into<ElementId>,
    label: &'static str,
    tooltip: &'static str,
    identity: DesktopRecoveryIdentity,
    action: DesktopRecoveryAction,
    cx: &gpui::Context<ConversationPane>,
) -> Button {
    let tone = match action {
        DesktopRecoveryAction::Retry => DesktopCriticalTone::Neutral,
        DesktopRecoveryAction::MarkFailed | DesktopRecoveryAction::Abort => {
            DesktopCriticalTone::Dangerous
        }
    };
    DesktopCriticalButton::new(id, label, tooltip, tone)
        .build()
        .on_click(cx.listener(move |_, _, _, cx| {
            cx.stop_propagation();
            cx.emit(ConversationPaneEvent::Recovery {
                identity: identity.clone(),
                action,
            });
        }))
}

fn structured_tool_command(detail: &str, text: &str) -> Option<String> {
    [detail, text].into_iter().find_map(|arguments| {
        serde_json::from_str::<serde_json::Value>(arguments)
            .ok()?
            .get("command")?
            .as_str()
            .map(str::to_owned)
    })
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
    use super::{ConversationPane, MAX_MARKDOWN_PARSE_STATES, tool_exit_code_label};
    use gpui::{AppContext as _, Entity, TestAppContext};
    use gpui_component::{Theme, ThemeMode, text::TextViewState};
    use std::sync::Arc;

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

    fn measure(cx: &mut gpui::VisualTestContext, state: &Entity<TextViewState>) -> f32 {
        use gpui::{ParentElement as _, Styled as _, px, size};
        use gpui_component::{ElementExt as _, text::TextView};
        use std::cell::RefCell;
        use std::rc::Rc;

        let observed = Rc::new(RefCell::new(0.0f32));
        let sink = Rc::clone(&observed);
        let state = state.clone();
        cx.draw(
            gpui::point(px(0.), px(0.)),
            size(px(900.), px(4_000.)),
            move |_, _| {
                gpui::div().w(px(900.)).child(
                    gpui::div()
                        .w_full()
                        .on_prepaint(move |bounds, _, _| {
                            *sink.borrow_mut() = f32::from(bounds.size.height);
                        })
                        .child(TextView::new(&state)),
                )
            },
        );
        *observed.borrow()
    }

    struct PaneRoot;
    impl gpui::Render for PaneRoot {
        fn render(
            &mut self,
            _: &mut gpui::Window,
            _: &mut gpui::Context<Self>,
        ) -> impl gpui::IntoElement {
            gpui::div()
        }
    }

    /// Streaming a row in chunks must land on the same document as parsing it in
    /// one shot.
    ///
    /// The pane feeds a reused `TextViewState` the smallest update that gets it
    /// to the current text, so a delta becomes an incremental background append.
    /// If the suffix arithmetic or the append path were wrong the row would
    /// silently render a truncated or duplicated document, which the rendered
    /// height catches.
    #[gpui::test]
    fn streamed_chunks_and_a_single_parse_reach_the_same_document(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            Theme::change(ThemeMode::Dark, None, cx);
        });
        let (_, visual_cx) = cx.add_window_view(|_, _| PaneRoot);
        visual_cx.run_until_parked();

        let pane = visual_cx.update(|_, cx| cx.new(|_| ConversationPane::new()));
        let chunks = [
            "# Heading\n\nfirst paragraph with **bold**\n\n",
            "- alpha\n- beta\n\n",
            "```rust\nfn main() {}\n```\n\n",
            "closing paragraph\n",
        ];
        let full: String = chunks.concat();

        let streamed_key: Arc<str> = Arc::from("transcript-markdown:row:settling");
        let oneshot_key: Arc<str> = Arc::from("transcript-markdown:other:settling");

        let mut accumulated = String::new();
        let mut streamed = None;
        for chunk in chunks {
            accumulated.push_str(chunk);
            let text: Arc<str> = Arc::from(accumulated.as_str());
            streamed = Some(pane.update(visual_cx, |pane, cx| {
                pane.markdown_state(&streamed_key, &text, cx)
            }));
            visual_cx.run_until_parked();
        }
        let streamed = streamed.expect("the streamed row resolved a parse state");

        let oneshot_text: Arc<str> = Arc::from(full.as_str());
        let oneshot = pane.update(visual_cx, |pane, cx| {
            pane.markdown_state(&oneshot_key, &oneshot_text, cx)
        });
        visual_cx.run_until_parked();

        let streamed_height = measure(visual_cx, &streamed);
        let oneshot_height = measure(visual_cx, &oneshot);
        assert!(oneshot_height > 100., "the fixture must be substantial");
        assert_eq!(
            streamed_height, oneshot_height,
            "incrementally appended chunks must render the same document as one parse"
        );

        // One state per row body, reused across every delta rather than rebuilt.
        let (state_count, reused) = pane.read_with(visual_cx, |pane, _| {
            (
                pane.markdown_states.len(),
                pane.markdown_states
                    .get(&streamed_key)
                    .map(|entry| entry.state.entity_id()),
            )
        });
        assert_eq!(state_count, 2);
        assert_eq!(reused, Some(streamed.entity_id()));
    }

    /// A revision that is not an extension has to replace, not append.
    #[gpui::test]
    fn a_rewritten_row_replaces_its_document_in_place(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            Theme::change(ThemeMode::Dark, None, cx);
        });
        let (_, visual_cx) = cx.add_window_view(|_, _| PaneRoot);
        visual_cx.run_until_parked();

        let pane = visual_cx.update(|_, cx| cx.new(|_| ConversationPane::new()));
        let key: Arc<str> = Arc::from("transcript-markdown:rewound:settling");

        let long: Arc<str> = Arc::from("paragraph\n\n".repeat(12).as_str());
        let long_state = pane.update(visual_cx, |pane, cx| pane.markdown_state(&key, &long, cx));
        visual_cx.run_until_parked();
        let long_height = measure(visual_cx, &long_state);

        // Completion swaps in sanitised text, and a rewind or branch can shorten
        // a row outright; neither is a suffix of what came before.
        let short: Arc<str> = Arc::from("paragraph\n");
        let short_state = pane.update(visual_cx, |pane, cx| pane.markdown_state(&key, &short, cx));
        visual_cx.run_until_parked();
        let short_height = measure(visual_cx, &short_state);

        assert_eq!(
            long_state.entity_id(),
            short_state.entity_id(),
            "the row keeps one parse state across a rewrite"
        );
        assert!(
            short_height < long_height,
            "a rewrite must replace the document, not append to it: \
             {long_height} -> {short_height}"
        );
    }

    #[gpui::test]
    fn the_parse_state_pool_stays_bounded(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            Theme::change(ThemeMode::Dark, None, cx);
        });
        let (_, visual_cx) = cx.add_window_view(|_, _| PaneRoot);
        visual_cx.run_until_parked();

        let pane = visual_cx.update(|_, cx| cx.new(|_| ConversationPane::new()));
        let text: Arc<str> = Arc::from("body");
        pane.update(visual_cx, |pane, cx| {
            for index in 0..(MAX_MARKDOWN_PARSE_STATES * 3) {
                pane.markdown_generation = pane.markdown_generation.wrapping_add(1);
                let key: Arc<str> = Arc::from(format!("transcript-markdown:row-{index}:settling"));
                pane.markdown_state(&key, &text, cx);
            }
            pane.evict_markdown_states();
        });

        let remaining = pane.read_with(visual_cx, |pane, _| pane.markdown_states.len());
        assert_eq!(remaining, MAX_MARKDOWN_PARSE_STATES);
    }
}
