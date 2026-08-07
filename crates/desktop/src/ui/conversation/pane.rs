mod card;
mod render;
mod shell;
#[cfg(test)]
mod tests;
mod tools;

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

use card::{ConversationCardExt as _, IdentityHeaderArgs, ReasoningArgs};
use shell::{empty_conversation, follow_latest_label};
use tools::*;

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

pub(crate) fn tool_detail_copy_text(title: &str, detail: &str, text: &str) -> String {
    tools::tool_detail_copy_text(title, detail, text)
}
