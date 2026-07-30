use gpui::{
    ElementId, Entity, EventEmitter, IntoElement, ParentElement as _, Render, Role, SharedString,
    Styled as _, Window, div, prelude::*, px, rgb,
};
use gpui_component::{
    ElementExt as _, Icon, Sizable as _, VirtualListScrollHandle, button::Button,
    text::TextViewState, v_virtual_list,
};
use std::{
    collections::{HashMap, HashSet},
    rc::Rc,
    sync::Arc,
    time::Instant,
};
use unicode_width::UnicodeWidthChar as _;

use super::{
    ConversationBlockKind, conversation_block_visual,
    conversation_controller::ConversationRenderReader,
    desktop_controls::{
        DesktopControlSize, DesktopCriticalButton, DesktopCriticalTone, DesktopIcon,
        DesktopIconButton,
    },
    desktop_style::{DesignRadius, DesignSpace, DesignText, DesktopStyledExt as _},
    streaming_text::{StreamingText, markdown_completion_trace_enabled, trace_markdown_parse},
};
use desktop::conversation::{
    ConversationRowMeasurement, TRANSCRIPT_COLLAPSED_PREVIEW_MAX_HEIGHT, compact_duration,
    conversation_copy_text,
};
use desktop::runtime::{DesktopRecoveryAction, DesktopRecoveryIdentity};
use desktop::shell::{
    ASSISTANT_MESSAGE_MAX_WIDTH, CONVERSATION_CONTENT_MAX_WIDTH,
    CONVERSATION_ROW_VERTICAL_PADDING_PX, MONOSPACE_FONT_FAMILY, SemanticTheme,
    USER_MESSAGE_MAX_WIDTH,
};

/// Width of the leading rail that carries conversation selection now that
/// blocks no longer paint a card background.
pub(super) const CONVERSATION_RAIL_WIDTH: f32 = 2.;
// The background excludes only the gap and copy control. The card's bottom
// padding remains inside the bubble, matching its top padding and vertically
// centering the message body.
const USER_MESSAGE_COPY_FOOTER_INSET: f32 =
    DesignSpace::Sm.pixels() + DesktopControlSize::Compact.pixels();
const CONVERSATION_TAIL_MARKER_HEIGHT: f32 = 1.;
const USER_MESSAGE_HORIZONTAL_CHROME: f32 = 48.;
const USER_MESSAGE_COLUMN_WIDTH: f32 = 8.;
const USER_MESSAGE_MIN_WIDTH: f32 = 64.;

#[derive(Debug, Clone, PartialEq)]
pub(super) enum ConversationPaneEvent {
    Select {
        block_id: String,
        durable: bool,
    },
    Copy {
        block_id: String,
    },
    CopyToolDetails {
        block_id: String,
    },
    CopyCodeCompleted,
    ToggleDetails {
        block_id: String,
    },
    OpenFull {
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
                        let user_card_width = (block.kind == ConversationBlockKind::User)
                            .then(|| user_message_width(&text));
                        let theme = SemanticTheme::GEEK_DARK;
                        let visual = conversation_block_visual(block.kind, block.is_error, theme);
                        let is_assistant = block.kind == ConversationBlockKind::Assistant;
                        let previous_kind = index.checked_sub(1).and_then(|previous| {
                            render.row(previous).map(|(row, _)| row.kind)
                        });
                        let next_kind = render.row(index + 1).map(|(row, _)| row.kind);
                        let show_identity_header =
                            conversation_identity_header_visible(block.kind, previous_kind);
                        let reasoning_duration_label = block
                            .reasoning_duration_millis
                            .map(compact_duration);
                        let is_tool = block.kind == ConversationBlockKind::Tool;
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
                        let accessible_label = terminal_label.map_or_else(
                            || block.title.to_string(),
                            |state| format!("{}, {state}", block.title),
                        );
                        // Selection paints a full-height leading rail; hover no
                        // longer paints a stub. The rail is absolutely
                        // positioned and never affects row height.
                        let selection_rail = (selected && !is_tool).then(|| {
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
                                .py_token(DesignSpace::Xs)
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
                                        .when(show_identity_header, |card| {
                                            card.child(
                                                div()
                                                .id(("conversation-row-header", index))
                                                .debug_selector(|| {
                                                    "desktop-conversation-row-header".into()
                                                })
                                                .when(index + 1 == row_count, |header| {
                                                    header.debug_selector(|| {
                                                        "desktop-last-conversation-row-header"
                                                            .into()
                                                    })
                                                })
                                                .flex()
                                                .items_center()
                                                .justify_between()
                                                .gap_token(DesignSpace::Md)
                                                .when(
                                                    is_tool && tool_expandable,
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
                                                            is_tool && tool_expandable,
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
                                                                if is_tool && tool_expandable {
                                                                    cx.emit(
                                                                        ConversationPaneEvent::ToggleDetails {
                                                                            block_id: tool_header_toggle_block_id.clone(),
                                                                        },
                                                                    );
                                                                }
                                                            },
                                                        ))
                                                        .when(is_tool, |main| {
                                                            main.child(
                                                                div()
                                                                    .px_token(DesignSpace::Sm)
                                                                    .py_token(DesignSpace::Xs)
                                                                    .text_token(DesignText::Metadata)
                                                                    .font_weight(
                                                                        gpui::FontWeight::SEMIBOLD,
                                                                    )
                                                                    .text_color(rgb(
                                                                        theme.text.value(),
                                                                    ))
                                                                    .child(tool_label.unwrap_or("Tool")),
                                                            )
                                                            .child(
                                                                div()
                                                                    .text_token(DesignText::Body)
                                                                    .font_weight(
                                                                        gpui::FontWeight::MEDIUM,
                                                                    )
                                                                    .text_color(rgb(
                                                                        theme.muted_text.value(),
                                                                    ))
                                                                    .min_w_0()
                                                                    .truncate()
                                                                    .child(SharedString::new(
                                                                        tool_summary_text.clone(),
                                                                    )),
                                                            )
                                                            .when(tool_expandable, |main| {
                                                                main.child(
                                                                    div()
                                                                        .id((
                                                                            "tool-toggle-details",
                                                                            index,
                                                                        ))
                                                                        .debug_selector(|| {
                                                                            "desktop-toggle-tool-details"
                                                                                .into()
                                                                        })
                                                                        .flex_shrink_0()
                                                                        .w(px(32.))
                                                                        .h(px(32.))
                                                                        .flex()
                                                                        .items_center()
                                                                        .justify_center()
                                                                        .text_token(
                                                                            DesignText::Metadata,
                                                                        )
                                                                        .text_color(rgb(
                                                                            theme.muted_text.value(),
                                                                        ))
                                                                        .opacity(0.)
                                                                        .group_hover(
                                                                            hover_group.clone(),
                                                                            |style| {
                                                                                style.opacity(1.)
                                                                            },
                                                                        )
                                                                        .child(
                                                                            Icon::new(
                                                                                tool_disclosure_icon(
                                                                                    detail_expanded,
                                                                                )
                                                                                .name(),
                                                                            )
                                                                            .small(),
                                                                        ),
                                                                )
                                                            })
                                                        })
                                                        .when(!is_tool, |main| {
                                                            main.child(
                                                                div()
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
                                                            .when(block.kind != ConversationBlockKind::User, |main| {
                                                                main.child(
                                                                    div()
                                                                        .text_token(DesignText::Body)
                                                                        .font_weight(
                                                                            gpui::FontWeight::MEDIUM,
                                                                        )
                                                                        .text_color(rgb(
                                                                            theme.text.value(),
                                                                        ))
                                                                        .child(SharedString::new(
                                                                            block.title.clone(),
                                                                        )),
                                                                )
                                                            })
                                                        }),
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
                                                ),
                                            )
                                        })
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
                                                        .bg(rgb(theme.elevated.value()))
                                                        .px_token(DesignSpace::Md)
                                                        .py_token(DesignSpace::Sm)
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
                                                                .aria_label(
                                                                    "Show thoughts",
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
                                                                .child(if block.done {
                                                                    match &reasoning_duration_label {
                                                                        Some(duration) => SharedString::new(
                                                                            format!("Thought for {duration}"),
                                                                        ),
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
                                                                theme.muted_text.value(),
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
                                                                        "Hide thoughts",
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
                                                                    .child(if block.done {
                                                                        match &reasoning_duration_label {
                                                                            Some(duration) => SharedString::new(
                                                                                format!("Thought for {duration}"),
                                                                            ),
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
                                        .when(is_tool && tool_expandable && detail_expanded, |card| {
                                            let tool_name_str = tool_name_from_title(&block.title);
                                            let is_shell = matches!(tool_name_str, "bash" | "shell");
                                            let is_edit = tool_name_str == "edit";
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
                                                            .when(
                                                                !is_shell && !is_edit,
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
                                        .when(!text.is_empty() && !is_tool, |card| {
                                            let content = StreamingText::new(
                                                text.clone(),
                                                text_phase,
                                                text_phase.renders_markdown().then(|| {
                                                    pane.markdown_state(&markdown_key, &text, cx)
                                                }),
                                                cx.entity().downgrade(),
                                            )
                                            .into_any_element();
                                            card.child(content)
                                        })
                                        .when(
                                            !is_assistant
                                                && !detail_text.is_empty()
                                                && !is_tool,
                                            |card| {
                                                card.child(
                                                    div()
                                                        .mt_token(DesignSpace::Xs)
                                                        .pl_token(DesignSpace::Md)
                                                        .py_token(DesignSpace::Sm)
                                                        .text_color(rgb(theme.muted_text.value()))
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
                                                    .when(visual.align_right, |actions| {
                                                        actions.justify_end()
                                                    })
                                                    .child(conversation_copy_button(
                                                        ("copy-conversation-row", index),
                                                        copy_block_id.clone(),
                                                        hover_group.clone(),
                                                        selected,
                                                        cx,
                                                    )),
                                            )
                                        }),
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

fn conversation_copy_button(
    id: impl Into<ElementId>,
    block_id: String,
    hover_group: SharedString,
    selected: bool,
    cx: &gpui::Context<ConversationPane>,
) -> Button {
    conversation_hover_tool(
        DesktopIconButton::new(id, DesktopIcon::Copy, "Copy this bounded message")
            .build()
            .debug_selector(|| "desktop-copy-conversation-row".into())
            .on_click(cx.listener(move |_, _, _, cx| {
                cx.stop_propagation();
                cx.emit(ConversationPaneEvent::Copy {
                    block_id: block_id.clone(),
                });
            })),
        hover_group,
        selected,
    )
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

fn user_message_width(text: &str) -> f32 {
    let maximum = USER_MESSAGE_MAX_WIDTH as f32;
    let maximum_content = maximum - USER_MESSAGE_HORIZONTAL_CHROME;
    let maximum_columns = (maximum_content / USER_MESSAGE_COLUMN_WIDTH).ceil() as usize;
    let mut line_columns = 0usize;
    let mut widest_line = 0usize;

    for character in text.chars() {
        if character == '\n' {
            widest_line = widest_line.max(line_columns);
            line_columns = 0;
            continue;
        }
        let character_columns = if character == '\t' {
            4
        } else {
            character.width().unwrap_or_default()
        };
        line_columns = line_columns.saturating_add(character_columns);
        if line_columns >= maximum_columns {
            return maximum;
        }
    }
    widest_line = widest_line.max(line_columns);

    (widest_line as f32 * USER_MESSAGE_COLUMN_WIDTH + USER_MESSAGE_HORIZONTAL_CHROME)
        .clamp(USER_MESSAGE_MIN_WIDTH, maximum)
}

fn conversation_identity_header_visible(
    kind: ConversationBlockKind,
    previous_kind: Option<ConversationBlockKind>,
) -> bool {
    kind != ConversationBlockKind::User
        && (kind != ConversationBlockKind::Assistant
            || previous_kind != Some(ConversationBlockKind::Tool))
}

fn conversation_copy_footer_visible(
    kind: ConversationBlockKind,
    next_kind: Option<ConversationBlockKind>,
) -> bool {
    kind != ConversationBlockKind::Tool
        && !(kind == ConversationBlockKind::Assistant
            && next_kind == Some(ConversationBlockKind::Tool))
}

fn tool_name_from_title(title: &str) -> &str {
    title
        .strip_prefix("Tool · ")
        .and_then(|title| title.split(" · ").next())
        .unwrap_or(title)
}

fn tool_display_label(name: &str) -> &'static str {
    match name {
        "bash" | "shell" => "Shell",
        "edit" => "Edit",
        "read" => "Read",
        _ => "Tool",
    }
}

fn tool_is_expandable(name: &str) -> bool {
    !matches!(name, "read")
}

fn tool_disclosure_icon(expanded: bool) -> DesktopIcon {
    if expanded {
        DesktopIcon::ChevronDown
    } else {
        DesktopIcon::ChevronRight
    }
}

fn tool_arguments_json(detail: &str, text: &str) -> Option<serde_json::Value> {
    [detail, text]
        .into_iter()
        .find_map(|s| serde_json::from_str(s).ok())
}

fn edit_replacements(args: &serde_json::Value) -> Vec<(&str, &str)> {
    let mut replacements = args
        .get("edits")
        .and_then(|value| value.as_array())
        .into_iter()
        .flatten()
        .filter_map(|edit| {
            Some((
                edit.get("oldText")?.as_str()?,
                edit.get("newText")?.as_str()?,
            ))
        })
        .collect::<Vec<_>>();
    if let (Some(old), Some(new)) = (
        args.get("oldText").and_then(|value| value.as_str()),
        args.get("newText").and_then(|value| value.as_str()),
    ) {
        replacements.push((old, new));
    }
    replacements
}

fn tool_summary(name: &str, detail: &str, text: &str) -> String {
    let Some(args) = tool_arguments_json(detail, text) else {
        return String::new();
    };
    match name {
        "bash" | "shell" => args
            .get("command")
            .and_then(|v| v.as_str())
            .map(str::to_owned)
            .unwrap_or_default(),
        "read" => {
            let path = args.get("path").and_then(|v| v.as_str()).unwrap_or("");
            let offset = args.get("offset").and_then(|v| v.as_u64());
            let limit = args.get("limit").and_then(|v| v.as_u64());
            match (offset, limit) {
                (Some(o), Some(l)) => format!("{path} [{o},{l}]"),
                (Some(o), None) => format!("{path} [{o},]"),
                (None, Some(l)) => format!("{path} [,{l}]"),
                (None, None) => path.to_owned(),
            }
        }
        "edit" => {
            let path = args.get("path").and_then(|v| v.as_str()).unwrap_or("");
            let (added, removed) = edit_replacements(&args).into_iter().fold(
                (0usize, 0usize),
                |(added, removed), (old, new)| {
                    (added + new.lines().count(), removed + old.lines().count())
                },
            );
            format!("{path} +{added} -{removed}")
        }
        _ => String::new(),
    }
}

fn edit_diff_view(detail: &str, text: &str, theme: &SemanticTheme) -> gpui::AnyElement {
    let args = tool_arguments_json(detail, text);
    let mut container = div().flex().flex_col();
    if let Some(args) = args.as_ref() {
        for (old, new) in edit_replacements(args) {
            for line in old.lines() {
                container = container.child(
                    div()
                        .text_color(rgb(theme.danger.value()))
                        .child(SharedString::new(format!("- {line}"))),
                );
            }
            for line in new.lines() {
                container = container.child(
                    div()
                        .text_color(rgb(theme.accent.value()))
                        .child(SharedString::new(format!("+ {line}"))),
                );
            }
        }
    }
    container.into_any_element()
}

fn edit_diff_text(detail: &str, text: &str) -> String {
    let Some(args) = tool_arguments_json(detail, text) else {
        return String::new();
    };
    edit_replacements(&args)
        .into_iter()
        .flat_map(|(old, new)| {
            old.lines()
                .map(|line| format!("- {line}"))
                .chain(new.lines().map(|line| format!("+ {line}")))
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>()
        .join("\n")
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

pub(super) fn tool_detail_copy_text(title: &str, detail: &str, text: &str) -> String {
    match tool_name_from_title(title) {
        "bash" | "shell" => {
            let command = structured_tool_command(detail, text).unwrap_or_default();
            conversation_copy_text(&format!("$ {command}"), text)
        }
        "edit" => conversation_copy_text(&edit_diff_text(detail, text), ""),
        _ => conversation_copy_text(text, ""),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ConversationBlockKind, ConversationPane, MAX_MARKDOWN_PARSE_STATES,
        conversation_copy_footer_visible, conversation_identity_header_visible, edit_diff_text,
        tool_detail_copy_text, tool_disclosure_icon, tool_name_from_title, tool_summary,
        user_message_width,
    };
    use gpui::{AppContext as _, Entity, TestAppContext};
    use gpui_component::{Theme, ThemeMode, text::TextViewState};
    use std::sync::Arc;

    #[test]
    fn user_message_width_wraps_content_and_caps_long_lines() {
        assert!(user_message_width("Short prompt") < 320.);
        assert!(user_message_width("中文提示") >= user_message_width("test"));
        assert_eq!(
            user_message_width(&"long wrapping prompt ".repeat(200)),
            desktop::shell::USER_MESSAGE_MAX_WIDTH as f32
        );
    }

    #[test]
    fn tool_titles_and_summaries_use_structured_arguments() {
        assert_eq!(tool_name_from_title("Tool · bash · 320 ms"), "bash");
        assert_eq!(
            tool_summary("bash", r#"{"command":"git status --short"}"#, ""),
            "git status --short"
        );
        assert_eq!(
            tool_summary(
                "read",
                r#"{"path":"src/main.rs","offset":40,"limit":80}"#,
                ""
            ),
            "src/main.rs [40,80]"
        );
        assert_eq!(
            tool_summary(
                "edit",
                r#"{"path":"src/main.rs","oldText":"one\ntwo","newText":"three\nfour\nfive"}"#,
                ""
            ),
            "src/main.rs +3 -2"
        );
    }

    #[test]
    fn tool_detail_copy_matches_the_expanded_shell_and_edit_views() {
        assert_eq!(
            tool_detail_copy_text(
                "Tool · shell · 1.2 s",
                r#"{"command":"git status"}"#,
                "M src/main.rs\n"
            ),
            "$ git status\nM src/main.rs\n"
        );
        let edit = r#"{"path":"src/main.rs","oldText":"old one\nold two","newText":"new one"}"#;
        assert_eq!(edit_diff_text(edit, ""), "- old one\n- old two\n+ new one");
        assert_eq!(
            tool_detail_copy_text("Tool · edit · 90 ms", edit, "done"),
            "- old one\n- old two\n+ new one"
        );
    }

    #[test]
    fn identity_headers_hide_for_user_and_continue_across_a_tool_row() {
        assert!(!conversation_identity_header_visible(
            ConversationBlockKind::User,
            Some(ConversationBlockKind::Assistant)
        ));
        assert!(!conversation_identity_header_visible(
            ConversationBlockKind::Assistant,
            Some(ConversationBlockKind::Tool)
        ));
        assert!(conversation_identity_header_visible(
            ConversationBlockKind::Assistant,
            Some(ConversationBlockKind::User)
        ));
        assert!(conversation_identity_header_visible(
            ConversationBlockKind::Tool,
            Some(ConversationBlockKind::Assistant)
        ));
    }

    #[test]
    fn assistant_copy_waits_until_the_tool_group_finishes() {
        assert!(!conversation_copy_footer_visible(
            ConversationBlockKind::Assistant,
            Some(ConversationBlockKind::Tool)
        ));
        assert!(conversation_copy_footer_visible(
            ConversationBlockKind::Assistant,
            None
        ));
        assert!(!conversation_copy_footer_visible(
            ConversationBlockKind::Tool,
            Some(ConversationBlockKind::Assistant)
        ));
    }

    #[test]
    fn tool_disclosure_rotates_down_when_expanded() {
        assert_eq!(
            tool_disclosure_icon(false),
            super::DesktopIcon::ChevronRight
        );
        assert_eq!(tool_disclosure_icon(true), super::DesktopIcon::ChevronDown);
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
