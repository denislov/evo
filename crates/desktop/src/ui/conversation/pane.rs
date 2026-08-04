use gpui::{
    ElementId, Entity, EventEmitter, IntoElement, ListState, ParentElement as _, Render, Role,
    SharedString, Styled as _, Subscription, Window, div, list, prelude::*, px, rgb,
};
use gpui_component::{Icon, Sizable as _, button::Button, text::TextViewState};
use std::{
    collections::{HashMap, HashSet},
    rc::Rc,
    sync::Arc,
    time::Instant,
};
use unicode_width::UnicodeWidthChar as _;

use super::{ConversationBlockKind, DELEGATION_TITLE_PREFIX, controller::ConversationRenderReader};
use crate::app::native_shell::{
    SessionWorkspace, conversation_block_visual, delegation_status_color,
};
use crate::ui::components::streaming_text::{
    StreamingText, markdown_completion_trace_enabled, trace_markdown_parse,
};
use crate::ui::components::{
    controls::{
        DesktopControlSize, DesktopCriticalButton, DesktopCriticalTone, DesktopIcon,
        DesktopIconButton,
    },
    style::{DesignRadius, DesignSpace, DesignText, DesktopStyledExt as _},
};
use crate::ui::shell::ShellUiState;
use desktop::projection::DesktopRecoveryStatus;
use desktop::runtime::{DesktopRecoveryAction, DesktopRecoveryIdentity};
use desktop::ui::conversation::{
    TRANSCRIPT_COLLAPSED_PREVIEW_MAX_HEIGHT, compact_duration, conversation_copy_text,
};
use desktop::ui::shell::{
    ASSISTANT_MESSAGE_MAX_WIDTH, CONVERSATION_CONTENT_MAX_WIDTH, MONOSPACE_FONT_FAMILY,
    SemanticTheme, USER_MESSAGE_MAX_WIDTH,
};

/// Width of the leading rail that carries conversation selection now that
/// blocks no longer paint a card background.
pub(crate) const CONVERSATION_RAIL_WIDTH: f32 = 2.;
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
pub(crate) enum ConversationPaneEvent {
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
    Scrolled,
    FollowLatest,
}

#[derive(Clone)]
pub(crate) struct ConversationPaneViewModel {
    pub(crate) render: ConversationRenderReader,
    pub(crate) scroll: ListState,
    pub(crate) visible_count: usize,
    pub(crate) event_count: usize,
    pub(crate) message_count: usize,
    pub(crate) tool_count: usize,
    pub(crate) omitted_count: usize,
    pub(crate) follow_latest: bool,
    pub(crate) unseen_updates: usize,
    pub(crate) selected_block_id: Option<String>,
    pub(crate) expanded_details: Rc<HashSet<String>>,
    pub(crate) full_view_block_id: Option<String>,
    pub(crate) diagnostic_recovery: Option<DesktopRecoveryIdentity>,
}

#[cfg(test)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ConversationPaneSnapshot {
    visible_count: usize,
    event_count: usize,
    message_count: usize,
    tool_count: usize,
    omitted_count: usize,
    follow_latest: bool,
    unseen_updates: usize,
    selected_block_id: Option<String>,
    expanded_details: HashSet<String>,
    full_view_block_id: Option<String>,
    diagnostic_recovery: Option<DesktopRecoveryIdentity>,
}

#[cfg(test)]
impl ConversationPaneViewModel {
    pub(crate) fn snapshot(&self) -> ConversationPaneSnapshot {
        ConversationPaneSnapshot {
            visible_count: self.visible_count,
            event_count: self.event_count,
            message_count: self.message_count,
            tool_count: self.tool_count,
            omitted_count: self.omitted_count,
            follow_latest: self.follow_latest,
            unseen_updates: self.unseen_updates,
            selected_block_id: self.selected_block_id.clone(),
            expanded_details: self.expanded_details.as_ref().clone(),
            full_view_block_id: self.full_view_block_id.clone(),
            diagnostic_recovery: self.diagnostic_recovery.clone(),
        }
    }
}

pub(crate) fn view_model(
    workspace: &SessionWorkspace,
    ui: &ShellUiState,
) -> ConversationPaneViewModel {
    let projection = workspace.projection.as_ref();
    let diagnostic_recovery = projection.and_then(|projection| {
        projection.recoveries().iter().find_map(|recovery| {
            (recovery.status == DesktopRecoveryStatus::Pending && recovery.authoritative)
                .then(|| recovery.identity.clone())
                .flatten()
        })
    });
    let visible_count = visible_count(workspace);
    ConversationPaneViewModel {
        render: workspace
            .presentation
            .conversation_controller
            .render_reader(),
        scroll: workspace
            .presentation
            .conversation_controller
            .scroll
            .clone(),
        visible_count,
        event_count: projection
            .map(|projection| projection.recent_events().len())
            .unwrap_or_default(),
        message_count: projection
            .map(|projection| projection.messages().len())
            .unwrap_or_default(),
        tool_count: projection
            .map(|projection| projection.tools().len())
            .unwrap_or_default(),
        omitted_count: projection
            .map(|projection| projection.conversation().omitted_blocks())
            .unwrap_or_default(),
        follow_latest: workspace
            .presentation
            .conversation_controller
            .follow_latest_enabled(),
        unseen_updates: workspace
            .presentation
            .conversation_controller
            .unseen_updates(),
        selected_block_id: workspace
            .presentation
            .conversation_controller
            .selected_block_id()
            .map(str::to_owned),
        expanded_details: Rc::new(
            workspace
                .presentation
                .conversation_controller
                .expanded_details()
                .clone(),
        ),
        full_view_block_id: ui
            .conversation_full_message
            .as_ref()
            .map(|message| message.block_id.clone()),
        diagnostic_recovery,
    }
}

pub(crate) fn visible_count(workspace: &SessionWorkspace) -> usize {
    workspace.projection.as_ref().map_or(0, |projection| {
        projection.conversation().blocks().len()
            + usize::from(workspace.composer.submitted().is_some())
            + projection.messages().len()
            + projection.tools().len()
    })
}

/// Markdown parse states outlive the frame that rendered them so a streaming row
/// can extend its document instead of re-parsing it.
///
/// Only rows the dynamic list actually renders get one, so the live set is
/// bounded by the viewport; this cap is the backstop for scrolling churn.
const MAX_MARKDOWN_PARSE_STATES: usize = 64;

/// One row body's parsed Markdown, plus exactly what has been fed to it.
struct MarkdownParseState {
    state: Entity<TextViewState>,
    /// The text `state` currently holds. Kept as the same `Arc` the render cache
    /// hands out, so an unchanged row costs one pointer comparison.
    fed: Arc<str>,
    touched: u64,
    _subscription: Subscription,
}

pub(crate) struct ConversationPane {
    view_model: Option<ConversationPaneViewModel>,
    markdown_states: HashMap<Arc<str>, MarkdownParseState>,
    markdown_generation: u64,
}

impl ConversationPane {
    pub(crate) fn new() -> Self {
        Self {
            view_model: None,
            markdown_states: HashMap::new(),
            markdown_generation: 0,
        }
    }

    /// Invalidate the outer dynamic-list item after an asynchronous Markdown
    /// parse publishes new block geometry.
    ///
    /// A delta invalidates the item before `push_str` starts its background
    /// parse, so that layout pass intentionally measures the previous parsed
    /// document. `TextViewState` notifies when the new document lands; this
    /// observation closes that timing gap and makes the next list layout adopt
    /// the new natural height while preserving the native scroll anchor.
    fn remeasure_markdown_row(&self, key: &Arc<str>) {
        let Some(view_model) = &self.view_model else {
            return;
        };
        let render = &view_model.render;
        let row_index = (0..render.len()).find(|index| {
            render.row(*index).is_some_and(|row| {
                row.markdown_state_key == *key || row.detail_markdown_state_key == *key
            })
        });
        if let Some(index) = row_index {
            view_model.scroll.remeasure_items(index..index + 1);
        }
    }

    pub(crate) fn set_view_model(&mut self, view_model: ConversationPaneViewModel) {
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
        let observed_key = Arc::clone(key);
        let subscription = cx.observe(&state, move |pane, _, cx| {
            pane.remeasure_markdown_row(&observed_key);
            cx.notify();
        });
        if let Some(started_at) = started_at {
            trace_markdown_parse(key, initial.len(), started_at);
        }
        self.markdown_states.insert(
            Arc::clone(key),
            MarkdownParseState {
                state: state.clone(),
                fed: initial,
                touched: generation,
                _subscription: subscription,
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
                                                .when(is_delegation, |header| {
                                                    header.debug_selector(|| {
                                                        "desktop-delegation-toggle-header".into()
                                                    })
                                                })
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
                                                        .when(is_delegation, |surface| {
                                                            surface
                                                                .role(Role::Button)
                                                                .aria_label(
                                                                    "Show or hide delegation details",
                                                                )
                                                                .aria_expanded(detail_expanded)
                                                        })
                                                        .on_click(cx.listener(
                                                            move |_, _, _, cx| {
                                                                cx.emit(
                                                                    ConversationPaneEvent::Select {
                                                                        block_id: select_block_id
                                                                            .clone(),
                                                                        durable,
                                                                    },
                                                                );
                                                                if (is_tool && tool_expandable)
                                                                    || is_delegation
                                                                {
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
                                                        .when(is_delegation, |main| {
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
                                                            .when_some(delegation, |main, meta| {
                                                                main.child(
                                                                    div()
                                                                        .text_token(
                                                                            DesignText::Body,
                                                                        )
                                                                        .font_weight(
                                                                            gpui::FontWeight::MEDIUM,
                                                                        )
                                                                        .text_color(rgb(
                                                                            theme.text.value(),
                                                                        ))
                                                                        .min_w_0()
                                                                        .truncate()
                                                                        .child(SharedString::new(
                                                                            &meta.target_id,
                                                                        )),
                                                                )
                                                                .child(
                                                                    div()
                                                                        .flex_shrink_0()
                                                                        .text_token(
                                                                            DesignText::Metadata,
                                                                        )
                                                                        .text_color(
                                                                            delegation_status_color(
                                                                                meta.status, theme,
                                                                            ),
                                                                        )
                                                                        .child(meta.status.label()),
                                                                )
                                                            })
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
                                                                        delegation_summary_text
                                                                            .clone(),
                                                                    )),
                                                            )
                                                            .when(delegation_expandable, |main| {
                                                                main.child(
                                                                    div()
                                                                        .id((
                                                                            "delegation-toggle-details",
                                                                            index,
                                                                        ))
                                                                        .debug_selector(|| {
                                                                            "desktop-toggle-delegation-details"
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
                                                        .when(!is_tool && !is_delegation, |main| {
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

/// Whether the row is an interior part of an assistant turn: tool calls and
/// delegations neither start a new identity segment nor carry the turn's
/// trailing copy affordance, so adjacent assistant rows must merge across
/// them exactly like they merge across plain tool calls.
fn is_tool_group(kind: ConversationBlockKind) -> bool {
    matches!(
        kind,
        ConversationBlockKind::Tool | ConversationBlockKind::Delegation
    )
}

fn conversation_identity_header_visible(
    kind: ConversationBlockKind,
    previous_kind: Option<ConversationBlockKind>,
) -> bool {
    kind != ConversationBlockKind::User
        && !(kind == ConversationBlockKind::Assistant && previous_kind.is_some_and(is_tool_group))
}

fn conversation_copy_footer_visible(
    kind: ConversationBlockKind,
    next_kind: Option<ConversationBlockKind>,
) -> bool {
    !(is_tool_group(kind)
        || kind == ConversationBlockKind::Assistant && next_kind.is_some_and(is_tool_group))
}

/// Collapsed-header summary for a delegation: the first line of the task.
fn delegation_task_summary(text: &str) -> String {
    text.lines().next().unwrap_or_default().to_owned()
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
        "write" => "Write",
        "read" => "Read",
        "ls" => "Files",
        "find" => "Find",
        "grep" => "Search",
        "web_search" => "Web search",
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
        "write" => {
            let path = args.get("path").and_then(|v| v.as_str()).unwrap_or("");
            let added = args
                .get("content")
                .and_then(|v| v.as_str())
                .map_or(0, |content| content.lines().count());
            format!("{path} +{added}")
        }
        "ls" => {
            let path = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");
            let entries = count_entries(text);
            if entries == 0 {
                format!("{path} · empty")
            } else {
                format!("{path} · {}", pluralized(entries, "entry", "entries"))
            }
        }
        "find" => {
            let pattern = args
                .get("pattern")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            let matches = count_entries(text);
            if matches == 0 {
                format!("{pattern} · no matches")
            } else {
                format!("{pattern} · {}", pluralized(matches, "match", "matches"))
            }
        }
        "grep" => {
            let pattern = args
                .get("pattern")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            let matches = count_grep_matches(text);
            if matches == 0 {
                format!("{pattern} · no matches")
            } else {
                format!("{pattern} · {}", pluralized(matches, "match", "matches"))
            }
        }
        "web_search" => web_search_summary(text),
        _ => String::new(),
    }
}

/// Summary line for a completed provider web-search item. The `summary`
/// carries the terminal item JSON (`{"status": ..., "action": {...}}`) so the
/// action type, search queries and opened-page URL survive into the
/// transcript; legacy items fall back to an empty summary.
fn web_search_summary(summary: &str) -> String {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(summary) else {
        return String::new();
    };
    let Some(action) = value.get("action") else {
        return String::new();
    };
    match action.get("type").and_then(serde_json::Value::as_str) {
        Some("search") => {
            let queries = action
                .get("queries")
                .and_then(serde_json::Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(serde_json::Value::as_str)
                .filter(|query| !query.starts_with("ws_call_id="))
                .collect::<Vec<_>>();
            match queries.len() {
                0 => "搜索完成".into(),
                1 => format!("搜索：{}", queries[0]),
                n => format!("搜索 {} 个查询：{}", n, queries[0]),
            }
        }
        Some("open_page") => action
            .get("url")
            .and_then(serde_json::Value::as_str)
            .map(|url| format!("打开页面：{}", strip_ws_call_id(url)))
            .unwrap_or_default(),
        _ => String::new(),
    }
}

/// Removes the `#ws_call_id=...` marker DeepSeek appends to opened-page URLs.
fn strip_ws_call_id(url: &str) -> &str {
    url.split('#').next().unwrap_or(url)
}

/// Non-empty content lines of an ls/find/grep result, excluding the trailing
/// `[notice]` block those tools append after a blank line. Content lines that
/// merely start with '[' (e.g. a grep match of `[foo]`) are kept.
fn tool_result_lines(text: &str) -> Vec<&str> {
    let mut lines = Vec::new();
    let mut in_notice = false;
    for line in text.lines().map(str::trim_end) {
        if line.is_empty() {
            in_notice = true;
        } else if !in_notice {
            lines.push(line);
        }
    }
    lines
}

/// The empty-state messages ls/find/grep emit when nothing matched.
fn is_empty_state_line(line: &str) -> bool {
    matches!(
        line,
        "(empty directory)" | "No files found matching pattern" | "No matches found"
    )
}

/// Entry count of an ls/find result, treating empty-state messages as zero
/// entries.
fn count_entries(text: &str) -> usize {
    let lines = tool_result_lines(text);
    if lines
        .first()
        .is_some_and(|first| is_empty_state_line(first))
    {
        0
    } else {
        lines.len()
    }
}

/// Number of `path:line: content` match lines in a grep result, so context
/// lines around a match do not inflate the count.
fn count_grep_matches(text: &str) -> usize {
    tool_result_lines(text)
        .iter()
        .filter(|line| parse_grep_match(line).is_some())
        .count()
}

fn pluralized(count: usize, singular: &str, plural: &str) -> String {
    if count == 1 {
        format!("1 {singular}")
    } else {
        format!("{count} {plural}")
    }
}

/// Split a grep match line `path:line: content`. The path and content may
/// themselves contain `: `, `:` or digits, so the split anchors on the *last*
/// `: <digits>: ` segment — the emitters format every match that way.
fn parse_grep_match(line: &str) -> Option<(&str, &str, &str)> {
    let mut anchor: Option<(usize, usize)> = None; // (colon index, digit count)
    for (index, _) in line.match_indices(':') {
        let after = &line[index + 1..];
        let digits = after
            .bytes()
            .take_while(|byte| byte.is_ascii_digit())
            .count();
        if digits == 0 {
            continue;
        }
        if after.as_bytes().get(digits) == Some(&b':')
            && after.as_bytes().get(digits + 1) == Some(&b' ')
        {
            anchor = Some((index, digits));
        }
    }
    let (colon, digits) = anchor?;
    let after = &line[colon + 1..];
    let (line_no, content) = after.split_at(digits);
    Some((&line[..colon], line_no, &content[2..]))
}

/// Split a grep context line `path-line- content` shown around a match.
fn parse_grep_context(line: &str) -> Option<(&str, &str, &str)> {
    let mut anchor: Option<(usize, usize)> = None; // (dash index, digit count)
    for (index, _) in line.match_indices('-') {
        let after = &line[index + 1..];
        let digits = after
            .bytes()
            .take_while(|byte| byte.is_ascii_digit())
            .count();
        if digits == 0 {
            continue;
        }
        if after.as_bytes().get(digits) == Some(&b'-')
            && after.as_bytes().get(digits + 1) == Some(&b' ')
        {
            anchor = Some((index, digits));
        }
    }
    let (dash, digits) = anchor?;
    let after = &line[dash + 1..];
    let (line_no, content) = after.split_at(digits);
    Some((&line[..dash], line_no, &content[2..]))
}

/// Directory listings (`ls`, `find`) paint directory entries with the accent
/// color, keep files neutral and dim the notice and empty-state lines.
fn ls_find_view(text: &str, theme: &SemanticTheme) -> gpui::AnyElement {
    let mut container = div().flex().flex_col();
    for line in tool_result_lines(text) {
        let directory = line.ends_with('/');
        let muted = is_empty_state_line(line);
        container = container.child(
            div()
                .text_color(rgb(if directory {
                    theme.accent.value()
                } else if muted {
                    theme.subtle_text.value()
                } else {
                    theme.text.value()
                }))
                .child(SharedString::new(line)),
        );
    }
    container.into_any_element()
}

/// Grep results keep the path neutral, highlight the line number on match
/// lines and dim context, notice and empty-state lines.
fn grep_view(text: &str, theme: &SemanticTheme) -> gpui::AnyElement {
    let mut container = div().flex().flex_col();
    for line in text.lines() {
        let line = line.trim_end();
        if line.is_empty() {
            continue;
        }
        if let Some((path, line_no, content)) = parse_grep_match(line) {
            container = container.child(
                div()
                    .flex()
                    .child(
                        div()
                            .text_color(rgb(theme.subtle_text.value()))
                            .child(SharedString::new(format!("{path}:"))),
                    )
                    .child(
                        div()
                            .text_color(rgb(theme.accent.value()))
                            .child(SharedString::new(format!("{line_no}: "))),
                    )
                    .child(
                        div()
                            .text_color(rgb(theme.text.value()))
                            .child(SharedString::new(content)),
                    ),
            );
        } else {
            let muted = line.starts_with('[')
                || is_empty_state_line(line)
                || parse_grep_context(line).is_some();
            container = container.child(
                div()
                    .text_color(rgb(if muted {
                        theme.subtle_text.value()
                    } else {
                        theme.text.value()
                    }))
                    .child(SharedString::new(line)),
            );
        }
    }
    container.into_any_element()
}

/// Renders a completed provider web-search item: one line per search query,
/// or the opened-page URL. The terminal `summary` carries the item JSON
/// (`{"status": ..., "action": {...}}`); legacy items render their raw text.
fn web_search_view(summary: &str, theme: &SemanticTheme) -> gpui::AnyElement {
    let mut container = div().flex().flex_col();
    let Ok(value) = serde_json::from_str::<serde_json::Value>(summary) else {
        container = container.child(
            div()
                .text_color(rgb(theme.text.value()))
                .child(SharedString::new(summary)),
        );
        return container.into_any_element();
    };
    let Some(action) = value.get("action") else {
        container = container.child(
            div()
                .text_color(rgb(theme.text.value()))
                .child(SharedString::new(summary)),
        );
        return container.into_any_element();
    };
    match action.get("type").and_then(serde_json::Value::as_str) {
        Some("search") => {
            let queries = action
                .get("queries")
                .and_then(serde_json::Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(serde_json::Value::as_str)
                .filter(|query| !query.starts_with("ws_call_id="))
                .collect::<Vec<_>>();
            if queries.is_empty() {
                container = container.child(
                    div()
                        .text_color(rgb(theme.subtle_text.value()))
                        .child(SharedString::new("搜索完成，无查询记录")),
                );
            } else {
                for query in queries {
                    container = container.child(
                        div()
                            .text_color(rgb(theme.text.value()))
                            .child(SharedString::new(format!("• {query}"))),
                    );
                }
            }
        }
        Some("open_page") => {
            if let Some(url) = action
                .get("url")
                .and_then(serde_json::Value::as_str)
                .map(strip_ws_call_id)
            {
                container = container.child(
                    div()
                        .text_color(rgb(theme.accent.value()))
                        .child(SharedString::new(url)),
                );
            }
        }
        _ => {
            container = container.child(
                div()
                    .text_color(rgb(theme.text.value()))
                    .child(SharedString::new(summary)),
            );
        }
    }
    container.into_any_element()
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

fn write_diff_view(detail: &str, text: &str, theme: &SemanticTheme) -> gpui::AnyElement {
    let args = tool_arguments_json(detail, text);
    let mut container = div().flex().flex_col();
    if let Some(args) = args.as_ref()
        && let Some(content) = args.get("content").and_then(|v| v.as_str())
    {
        for line in content.lines() {
            container = container.child(
                div()
                    .text_color(rgb(theme.accent.value()))
                    .child(SharedString::new(format!("+ {line}"))),
            );
        }
    }
    container.into_any_element()
}

fn write_diff_text(detail: &str, text: &str) -> String {
    let Some(args) = tool_arguments_json(detail, text) else {
        return String::new();
    };
    args.get("content")
        .and_then(|v| v.as_str())
        .map(|content| {
            content
                .lines()
                .map(|line| format!("+ {line}"))
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default()
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

pub(crate) fn tool_detail_copy_text(title: &str, detail: &str, text: &str) -> String {
    match tool_name_from_title(title) {
        "bash" | "shell" => {
            let command = structured_tool_command(detail, text).unwrap_or_default();
            conversation_copy_text(&format!("$ {command}"), text)
        }
        "edit" => conversation_copy_text(&edit_diff_text(detail, text), ""),
        "write" => {
            let diff = write_diff_text(detail, text);
            if diff.is_empty() {
                conversation_copy_text(text, "")
            } else {
                conversation_copy_text(&diff, "")
            }
        }
        _ if title.starts_with(DELEGATION_TITLE_PREFIX) => conversation_copy_text(text, detail),
        _ => conversation_copy_text(text, ""),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ConversationBlockKind, ConversationPane, MAX_MARKDOWN_PARSE_STATES,
        conversation_copy_footer_visible, conversation_identity_header_visible,
        delegation_task_summary, edit_diff_text, parse_grep_context, parse_grep_match,
        strip_ws_call_id, tool_detail_copy_text, tool_disclosure_icon, tool_display_label,
        tool_name_from_title, tool_summary, user_message_width, write_diff_text,
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
            desktop::ui::shell::USER_MESSAGE_MAX_WIDTH as f32
        );
    }

    #[test]
    fn web_search_tools_render_action_specific_summaries() {
        assert_eq!(tool_display_label("web_search"), "Web search");
        // Search action: queries carry the internal `ws_call_id` marker that
        // must be hidden before counting.
        assert_eq!(
            tool_summary(
                "web_search",
                r#"{"type":"web_search_call","id":"call_1","status":"in_progress"}"#,
                r#"{"status":"completed","action":{"type":"search","queries":["2025年诺贝尔物理学奖 获奖者","ws_call_id=call_1"]}}"#,
            ),
            "搜索：2025年诺贝尔物理学奖 获奖者"
        );
        assert_eq!(
            tool_summary(
                "web_search",
                "{}",
                r#"{"status":"completed","action":{"type":"search","queries":["2025年诺贝尔物理学奖 获奖者","Nobel Prize Physics 2025"]}}"#,
            ),
            "搜索 2 个查询：2025年诺贝尔物理学奖 获奖者"
        );
        // Open-page action strips the trailing `#ws_call_id` fragment.
        assert_eq!(
            tool_summary(
                "web_search",
                "{}",
                r#"{"status":"completed","action":{"type":"open_page","url":"https://nobelprize.org/prizes/physics/2025/summary/#ws_call_id=call_2"}}"#,
            ),
            "打开页面：https://nobelprize.org/prizes/physics/2025/summary/"
        );
        // Legacy items without an action fall back to no summary.
        assert_eq!(tool_summary("web_search", "{}", "completed"), "");
        assert_eq!(
            strip_ws_call_id("https://x/y#ws_call_id=abc"),
            "https://x/y"
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
        assert_eq!(
            tool_summary(
                "write",
                r#"{"path":"src/lib.rs","content":"line one\nline two\n"}"#,
                ""
            ),
            "src/lib.rs +2"
        );
        assert_eq!(
            tool_summary("ls", r#"{"path":"src"}"#, "a.rs\nb.rs\nlib/\n"),
            "src · 3 entries"
        );
        assert_eq!(
            tool_summary("ls", r#"{"path":"."}"#, "(empty directory)"),
            ". · empty"
        );
        assert_eq!(
            tool_summary("find", r#"{"pattern":"*.rs"}"#, "a.rs\nb.rs"),
            "*.rs · 2 matches"
        );
        assert_eq!(
            tool_summary(
                "find",
                r#"{"pattern":"*.rs"}"#,
                "No files found matching pattern"
            ),
            "*.rs · no matches"
        );
        assert_eq!(
            tool_summary(
                "grep",
                r#"{"pattern":"foo"}"#,
                "src/a.rs:3: foo\nsrc/b.rs:1: foo\n\n[2 matches limit reached]"
            ),
            "foo · 2 matches"
        );
        // grep context lines around a match do not inflate the count.
        assert_eq!(
            tool_summary(
                "grep",
                r#"{"pattern":"fn","context":1}"#,
                "src/lib.rs-3- use std::io;\nsrc/lib.rs:4: fn main() {}"
            ),
            "fn · 1 match"
        );
        // A match whose content starts with '[' or contains ': ' still counts
        // and is not mistaken for the trailing notice block.
        assert_eq!(
            tool_summary(
                "grep",
                r#"{"pattern":"a"}"#,
                "src/a.rs:2: [foo]\nsrc/b.rs:5: let m = {a: 1}\n\n[3 matches limit reached]"
            ),
            "a · 2 matches"
        );
        assert_eq!(
            tool_summary("grep", r#"{"pattern":"nope"}"#, "No matches found"),
            "nope · no matches"
        );
    }

    #[test]
    fn grep_lines_parse_paths_line_numbers_and_context() {
        assert_eq!(
            parse_grep_match("src/a.rs:12: let x = 1"),
            Some(("src/a.rs", "12", "let x = 1"))
        );
        // A path containing ':' or content containing ': ' must not confuse
        // the final `: <digits>: ` anchor.
        assert_eq!(
            parse_grep_match("src/a.rs:3: url = \"http://x:8080\""),
            Some(("src/a.rs", "3", "url = \"http://x:8080\""))
        );
        assert_eq!(
            parse_grep_match("src/a.rs:3: let m = {a: 1, b: 2}"),
            Some(("src/a.rs", "3", "let m = {a: 1, b: 2}"))
        );
        assert_eq!(
            parse_grep_context("src/lib.rs-3- use std::io;"),
            Some(("src/lib.rs", "3", "use std::io;"))
        );
        // A hyphenated basename still splits at the final `- <digits>- `.
        assert_eq!(
            parse_grep_context("my-file.rs-5- let y = 2"),
            Some(("my-file.rs", "5", "let y = 2"))
        );
        // Content containing '- ' must not confuse the context anchor.
        assert_eq!(
            parse_grep_context("src/lib.rs-2- let y = a - b"),
            Some(("src/lib.rs", "2", "let y = a - b"))
        );
        assert_eq!(parse_grep_match("not a match line"), None);
        assert_eq!(parse_grep_match("src/a.rs:12"), None);
        assert_eq!(parse_grep_context("src/a.rs:12: content"), None);
    }

    #[test]
    fn tool_detail_copy_matches_the_expanded_shell_edit_and_write_views() {
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
        let write = r#"{"path":"src/lib.rs","content":"line one\nline two\n"}"#;
        assert_eq!(write_diff_text(write, ""), "+ line one\n+ line two");
        assert_eq!(
            tool_detail_copy_text("Tool · write · 15 ms", write, "Wrote 18 bytes"),
            "+ line one\n+ line two"
        );
        // A write whose args were truncated mid-JSON (no parseable content)
        // falls back to copying the tool result text.
        let truncated = r#"{"path":"src/lib.rs","content":"trunc"#;
        assert_eq!(write_diff_text(truncated, ""), "");
        assert_eq!(
            tool_detail_copy_text("Tool · write · 15 ms", truncated, "Wrote 18 bytes"),
            "Wrote 18 bytes"
        );
        // Delegation copy joins the task and the result summary.
        assert_eq!(
            tool_detail_copy_text("Delegation · Agent", "summary text", "task text"),
            "task text\nsummary text"
        );
    }

    #[test]
    fn identity_headers_hide_for_user_and_continue_across_tool_group_rows() {
        assert!(!conversation_identity_header_visible(
            ConversationBlockKind::User,
            Some(ConversationBlockKind::Assistant)
        ));
        assert!(!conversation_identity_header_visible(
            ConversationBlockKind::Assistant,
            Some(ConversationBlockKind::Tool)
        ));
        assert!(!conversation_identity_header_visible(
            ConversationBlockKind::Assistant,
            Some(ConversationBlockKind::Delegation)
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
        assert!(!conversation_copy_footer_visible(
            ConversationBlockKind::Assistant,
            Some(ConversationBlockKind::Delegation)
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
    fn delegation_summary_uses_the_first_task_line() {
        assert_eq!(
            delegation_task_summary("Implement the auth flow\nsecond line"),
            "Implement the auth flow"
        );
        assert_eq!(delegation_task_summary("single line"), "single line");
        assert_eq!(delegation_task_summary(""), "");
    }

    #[test]
    fn delegation_rows_hide_the_generic_copy_footer_like_tools() {
        assert!(!conversation_copy_footer_visible(
            ConversationBlockKind::Delegation,
            None
        ));
        assert!(!conversation_copy_footer_visible(
            ConversationBlockKind::Delegation,
            Some(ConversationBlockKind::Assistant)
        ));
        // An assistant row followed by a delegation is still mid-turn: the
        // copy affordance waits for the delegation like it does for a tool.
        assert!(!conversation_copy_footer_visible(
            ConversationBlockKind::Assistant,
            Some(ConversationBlockKind::Delegation)
        ));
        assert!(conversation_copy_footer_visible(
            ConversationBlockKind::Assistant,
            None
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

        let streamed_key: Arc<str> = Arc::from("transcript-markdown:row:streaming");
        let oneshot_key: Arc<str> = Arc::from("transcript-markdown:other:streaming");

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
        let key: Arc<str> = Arc::from("transcript-markdown:rewound:streaming");

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
                let key: Arc<str> = Arc::from(format!("transcript-markdown:row-{index}:streaming"));
                pane.markdown_state(&key, &text, cx);
            }
            pane.evict_markdown_states();
        });

        let remaining = pane.read_with(visual_cx, |pane, _| pane.markdown_states.len());
        assert_eq!(remaining, MAX_MARKDOWN_PARSE_STATES);
    }
}
