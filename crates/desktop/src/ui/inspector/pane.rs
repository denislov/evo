use coding_agent::api::{
    event::{CodingAgentMergeChangeKind, CodingAgentMergeProposal},
    review::CodingAgentFileReviewRequest,
};
use desktop::runtime::{DesktopRecoveryAction, DesktopRecoveryIdentity};
use desktop::ui::inspector::review::{DesktopReviewLineKind, MAX_VISIBLE_FILE_CHANGES};
use desktop::ui::shell::{
    CONTEXT_PANEL_WIDTH, MONOSPACE_FONT_FAMILY, SemanticTheme, truncate_label,
};
use gpui::{
    EventEmitter, FocusHandle, IntoElement, ParentElement as _, Render, Role, ScrollHandle,
    Styled as _, Window, div, prelude::*, px, rgb,
};
use gpui_component::{Disableable as _, badge::Badge, button::Button};
use std::sync::Arc;

use crate::actions;
use crate::app::native_shell::InspectorSection;
use crate::application::workspace_state::DesktopFileReviewState;
use crate::ui::components::{
    controls::{
        DesktopActionRow, DesktopControlSize, DesktopCriticalButton, DesktopCriticalTone,
        DesktopIcon, DesktopIconButton, DesktopRowState,
    },
    style::{DesignRadius, DesignSpace, DesignText, DesktopStyledExt as _},
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum InspectorPaneEvent {
    Close,
    RequestFileReview(CodingAgentFileReviewRequest),
    CopyReviewPath,
    CopyFileReview,
    OpenExternalEditor,
    RefreshMergeProposals,
    MergeProposal(String),
    DiscardProposal(String),
    SelectSection(InspectorSection),
    Recovery {
        identity: DesktopRecoveryIdentity,
        action: DesktopRecoveryAction,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct InspectorChangedFileView {
    pub(crate) request: CodingAgentFileReviewRequest,
    pub(crate) mutation_kind: String,
    pub(crate) file_name: String,
    pub(crate) path: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct InspectorRecoveryView {
    pub(crate) status: String,
    pub(crate) recovery_id: String,
    pub(crate) operation_id: String,
    pub(crate) detail: String,
    pub(crate) attempt_count: String,
    pub(crate) identity: Option<DesktopRecoveryIdentity>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct InspectorDiagnosticView {
    pub(crate) sequence: String,
    pub(crate) operation: String,
    pub(crate) detail: String,
    pub(crate) truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct InspectorPaneViewModel {
    pub(crate) panel_width: u32,
    pub(crate) presented_as_drawer: bool,
    pub(crate) keyboard_focus_visible: bool,
    pub(crate) selected_section: InspectorSection,
    pub(crate) composer_running: bool,
    pub(crate) awaiting_prompt_start: bool,
    pub(crate) recovery_pending: bool,
    pub(crate) file_review_pending: bool,
    pub(crate) external_editor_pending: bool,
    pub(crate) external_editor_configured: bool,
    pub(crate) changed_files: Vec<InspectorChangedFileView>,
    pub(crate) change_count: usize,
    pub(crate) file_review: Arc<DesktopFileReviewState>,
    pub(crate) merge_proposals: Arc<Vec<CodingAgentMergeProposal>>,
    pub(crate) merge_proposal_pending: bool,
    pub(crate) runtime_attention_count: usize,
    pub(crate) task_state: String,
    pub(crate) active_operation: String,
    pub(crate) operation_count: usize,
    pub(crate) delegation_count: usize,
    pub(crate) selected_model: String,
    pub(crate) profile: String,
    pub(crate) thinking: String,
    pub(crate) usage_input: String,
    pub(crate) usage_output: String,
    pub(crate) usage_cache_read: String,
    pub(crate) usage_cache_write: String,
    pub(crate) usage_tokens: String,
    pub(crate) usage_context: String,
    pub(crate) usage_cost: String,
    pub(crate) reduced_motion: bool,
    pub(crate) stream_id: String,
    pub(crate) sequence: String,
    pub(crate) generation: String,
    pub(crate) model_count: usize,
    pub(crate) profile_count: usize,
    pub(crate) skill_count: usize,
    pub(crate) prompt_count: usize,
    pub(crate) context_count: usize,
    pub(crate) latest_recovery: Option<InspectorRecoveryView>,
    pub(crate) latest_diagnostic: Option<InspectorDiagnosticView>,
    pub(crate) latest_config_diagnostic: Option<(String, String)>,
    pub(crate) latest_issue: Option<String>,
    pub(crate) cwd: String,
}

mod sections;
mod view_model;

use self::sections::{
    colored_section, inspector_section_index, inspector_section_tab, recovery_button, section,
};
pub(crate) use self::view_model::view_model;

pub(crate) struct InspectorPane {
    focus: FocusHandle,
    tab_focus: [FocusHandle; 4],
    tab_scroll: ScrollHandle,
    view_model: Option<InspectorPaneViewModel>,
}

impl InspectorPane {
    pub(crate) fn new(focus: FocusHandle, cx: &mut gpui::Context<Self>) -> Self {
        Self {
            focus,
            tab_focus: std::array::from_fn(|index| cx.focus_handle().tab_index(index as isize)),
            tab_scroll: ScrollHandle::new(),
            view_model: None,
        }
    }

    pub(crate) fn set_view_model(&mut self, view_model: InspectorPaneViewModel) {
        self.view_model = Some(view_model);
    }

    #[cfg(test)]
    pub(crate) fn focus_tab(
        &self,
        section: InspectorSection,
        window: &mut Window,
        cx: &mut gpui::Context<Self>,
    ) {
        self.tab_focus[inspector_section_index(section)].focus(window, cx);
    }

    #[cfg(test)]
    pub(crate) fn tab_scroll_offset(&self) -> gpui::Point<gpui::Pixels> {
        self.tab_scroll.offset()
    }
}

impl EventEmitter<InspectorPaneEvent> for InspectorPane {}

impl Render for InspectorPane {
    fn render(&mut self, window: &mut Window, cx: &mut gpui::Context<Self>) -> impl IntoElement {
        let Some(view_model) = self.view_model.clone() else {
            return div()
                .w(px(CONTEXT_PANEL_WIDTH as f32))
                .h_full()
                .into_any_element();
        };
        let panel_width = view_model.panel_width;
        let theme = SemanticTheme::current(cx);
        let composer_running = view_model.composer_running;
        let awaiting_prompt_start = view_model.awaiting_prompt_start;
        let recovery_pending = view_model.recovery_pending;
        let file_review_pending = view_model.file_review_pending;
        let external_editor_pending = view_model.external_editor_pending;
        let merge_proposal_pending = view_model.merge_proposal_pending;
        let change_count = view_model.change_count;
        let selected_review_request = match view_model.file_review.as_ref() {
            DesktopFileReviewState::Empty => None,
            DesktopFileReviewState::Loading(request)
            | DesktopFileReviewState::Failed { request, .. } => Some(request),
            DesktopFileReviewState::Ready(document) => Some(&document.request),
        };
        let changed_file_rows = view_model
            .changed_files
            .iter()
            .enumerate()
            .map(|(index, change)| {
                let request = change.request.clone();
                let selected = selected_review_request == Some(&change.request);
                DesktopActionRow::new(
                    ("changed-file-review", index),
                    change.file_name.clone(),
                    format!("{} changed file {}", change.mutation_kind, change.path),
                )
                .state(DesktopRowState {
                    selected,
                    disabled: composer_running || awaiting_prompt_start || file_review_pending,
                    focus_visible: false,
                })
                .size(DesktopControlSize::Critical)
                .leading(
                    div()
                        .rounded_token(DesignRadius::Sm)
                        .bg(rgb(theme.canvas.value()))
                        .px_token(DesignSpace::Xs)
                        .text_token(DesignText::Metadata)
                        .text_color(rgb(theme.accent.value()))
                        .child(change.mutation_kind.clone()),
                )
                .detail(change.path.clone())
                .build(theme)
                .debug_selector(move || format!("desktop-changed-file-row-{index}"))
                .on_click(cx.listener(move |_, _, _, cx| {
                    cx.emit(InspectorPaneEvent::RequestFileReview(request.clone()));
                }))
            })
            .collect::<Vec<_>>();
        let omitted_changed_files = change_count.saturating_sub(changed_file_rows.len());
        let file_review_panel = match view_model.file_review.as_ref() {
            DesktopFileReviewState::Empty => div()
                .text_token(DesignText::Body)
                .text_color(rgb(theme.muted_text.value()))
                .child("Select a changed file to load a product-authorized preview."),
            DesktopFileReviewState::Loading(request) => div()
                .text_token(DesignText::Body)
                .text_color(rgb(theme.warning.value()))
                .child(format!(
                    "Loading {}…",
                    truncate_label(&request.change.path, 44)
                )),
            DesktopFileReviewState::Failed { request, code } => {
                let retry = request.clone();
                div()
                    .flex()
                    .flex_col()
                    .gap_token(DesignSpace::Sm)
                    .text_token(DesignText::Body)
                    .text_color(rgb(theme.danger.value()))
                    .child(format!(
                        "{} unavailable ({})",
                        truncate_label(&request.change.path, 36),
                        truncate_label(code, 28)
                    ))
                    .child(
                        Button::new("retry-file-review")
                            .compact()
                            .label("Retry review")
                            .tooltip("Retry the current changed-file review")
                            .disabled(
                                composer_running || awaiting_prompt_start || file_review_pending,
                            )
                            .on_click(cx.listener(move |_, _, _, cx| {
                                cx.emit(InspectorPaneEvent::RequestFileReview(retry.clone()));
                            })),
                    )
            }
            DesktopFileReviewState::Ready(document) => {
                let rows = document
                    .rows
                    .iter()
                    .enumerate()
                    .map(|(index, row)| {
                        let color = match row.kind {
                            DesktopReviewLineKind::Added => theme.success,
                            DesktopReviewLineKind::Removed => theme.danger,
                            DesktopReviewLineKind::FileHeader
                            | DesktopReviewLineKind::HunkHeader => theme.accent,
                            DesktopReviewLineKind::Fold => theme.warning,
                            DesktopReviewLineKind::Context => theme.muted_text,
                        };
                        let marker = match row.kind {
                            DesktopReviewLineKind::Added => "+",
                            DesktopReviewLineKind::Removed => "-",
                            DesktopReviewLineKind::Fold => "…",
                            _ => " ",
                        };
                        div()
                            .id(("file-review-line", index))
                            .flex()
                            .gap_token(DesignSpace::Sm)
                            .text_token(DesignText::Body)
                            .text_color(rgb(color.value()))
                            .child(marker)
                            .child(row.text.clone())
                    })
                    .collect::<Vec<_>>();
                div()
                    .flex()
                    .flex_col()
                    .gap_token(DesignSpace::Sm)
                    .text_token(DesignText::Body)
                    .child(
                        div()
                            .text_color(rgb(theme.text.value()))
                            .child(document.display_path.clone()),
                    )
                    .child(format!(
                        "{} · {} bytes · {} lines · {}",
                        document.mutation_kind,
                        document.total_bytes,
                        document.total_lines,
                        if document.using_diff {
                            "unified diff"
                        } else {
                            "file preview"
                        }
                    ))
                    .when(
                        document.source_truncated || document.rows_truncated,
                        |panel| {
                            panel.child(
                                div()
                                    .text_color(rgb(theme.warning.value()))
                                    .child("Preview bounded at desktop safety limits."),
                            )
                        },
                    )
                    .child(
                        div()
                            .flex()
                            .gap_token(DesignSpace::Sm)
                            .child(
                                DesktopIconButton::new(
                                    "copy-review-path",
                                    DesktopIcon::Copy,
                                    "Copy the reviewed project-relative path",
                                )
                                .build()
                                .debug_selector(|| "desktop-hit-copy-review-path".into())
                                .on_click(cx.listener(
                                    |_, _, _, cx| {
                                        cx.emit(InspectorPaneEvent::CopyReviewPath);
                                    },
                                )),
                            )
                            .child(
                                DesktopIconButton::new(
                                    "copy-file-review",
                                    DesktopIcon::Copy,
                                    "Copy the bounded read-only file review",
                                )
                                .build()
                                .debug_selector(|| "desktop-hit-copy-file-review".into())
                                .on_click(cx.listener(
                                    |_, _, _, cx| {
                                        cx.emit(InspectorPaneEvent::CopyFileReview);
                                    },
                                )),
                            )
                            .child(
                                DesktopIconButton::new(
                                    "open-external-editor",
                                    DesktopIcon::OpenExternal,
                                    "Revalidate and open this file in the configured editor",
                                )
                                .busy(external_editor_pending)
                                .build()
                                .debug_selector(|| "desktop-hit-open-external-editor".into())
                                .disabled(
                                    !view_model.external_editor_configured
                                        || composer_running
                                        || awaiting_prompt_start,
                                )
                                .on_click(cx.listener(
                                    |_, _, _, cx| {
                                        cx.emit(InspectorPaneEvent::OpenExternalEditor);
                                    },
                                )),
                            ),
                    )
                    .child(
                        div()
                            .mt_token(DesignSpace::Xs)
                            .pl_2()
                            .border_l_1()
                            .border_color(rgb(theme.border.value()))
                            .flex()
                            .flex_col()
                            .gap_token(DesignSpace::Xs)
                            .children(rows),
                    )
            }
        };
        let proposal_rows = view_model
            .merge_proposals
            .iter()
            .enumerate()
            .map(|(index, proposal)| {
                let merge_id = proposal.worktree_id.clone();
                let discard_id = proposal.worktree_id.clone();
                let changes = proposal
                    .changes
                    .iter()
                    .take(MAX_VISIBLE_FILE_CHANGES)
                    .map(|change| {
                        let marker = match change.kind {
                            CodingAgentMergeChangeKind::Added => "+",
                            CodingAgentMergeChangeKind::Modified => "~",
                            CodingAgentMergeChangeKind::Deleted => "-",
                        };
                        div()
                            .text_color(rgb(theme.muted_text.value()))
                            .child(format!(
                                "{marker} {}  +{} -{}",
                                truncate_label(&change.path, 42),
                                change.additions,
                                change.deletions
                            ))
                    })
                    .collect::<Vec<_>>();
                let omitted = proposal.changes.len().saturating_sub(changes.len());
                div()
                    .id(("merge-proposal-row", index))
                    .py_token(DesignSpace::Sm)
                    .border_b_1()
                    .border_color(rgb(theme.divider.value()))
                    .flex()
                    .flex_col()
                    .gap_token(DesignSpace::Xs)
                    .child(format!(
                        "{} · {} change(s)",
                        truncate_label(&proposal.worktree_id, 30),
                        proposal.changes.len()
                    ))
                    .child(format!(
                        "child {}",
                        truncate_label(&proposal.child_operation_id, 36)
                    ))
                    .children(changes)
                    .when(omitted > 0, |row| {
                        row.child(
                            div()
                                .text_color(rgb(theme.warning.value()))
                                .child(format!("+ {omitted} more change(s) omitted")),
                        )
                    })
                    .child(
                        div()
                            .flex()
                            .gap_token(DesignSpace::Sm)
                            .child(
                                Button::new(("merge-child-worktree", index))
                                    .compact()
                                    .label("Merge")
                                    .tooltip("Apply this reviewed proposal to the parent workspace")
                                    .disabled(
                                        composer_running
                                            || awaiting_prompt_start
                                            || merge_proposal_pending,
                                    )
                                    .on_click(cx.listener(move |_, _, _, cx| {
                                        cx.emit(InspectorPaneEvent::MergeProposal(
                                            merge_id.clone(),
                                        ));
                                    })),
                            )
                            .child(
                                DesktopCriticalButton::new(
                                    ("discard-child-worktree", index),
                                    "Discard",
                                    "Permanently discard this child proposal",
                                    DesktopCriticalTone::Dangerous,
                                )
                                .disabled(
                                    composer_running
                                        || awaiting_prompt_start
                                        || merge_proposal_pending,
                                )
                                .build()
                                .on_click(cx.listener(
                                    move |_, _, _, cx| {
                                        cx.emit(InspectorPaneEvent::DiscardProposal(
                                            discard_id.clone(),
                                        ));
                                    },
                                )),
                            ),
                    )
            })
            .collect::<Vec<_>>();
        let latest_recovery = view_model.latest_recovery;
        let latest_diagnostic = view_model.latest_diagnostic;
        let latest_config_diagnostic = view_model.latest_config_diagnostic;
        let latest_issue = view_model.latest_issue;
        let runtime_attention_count = view_model.runtime_attention_count;
        let active_operation = view_model.active_operation;
        let presented_as_drawer = view_model.presented_as_drawer;
        let focused = self.focus.is_focused(window) && view_model.keyboard_focus_visible;
        let selected_section = view_model.selected_section;
        let selected_section_index = inspector_section_index(selected_section);
        self.tab_scroll.scroll_to_item(selected_section_index);
        let selected_section_label = match selected_section {
            InspectorSection::Changes => "Changes",
            InspectorSection::Task => "Task",
            InspectorSection::Usage => "Usage",
            InspectorSection::Runtime => "Runtime",
        };
        let thinking = view_model.thinking;

        div()
            .id("inspector-panel")
            .role(Role::Complementary)
            .aria_label("Task Inspector")
            .debug_selector(|| "desktop-inspector-panel".into())
            .when(presented_as_drawer, |panel| {
                panel
                    .role(Role::Dialog)
                    .aria_label("Task Inspector dialog")
                    .aria_description("Review task details. Escape closes this dialog.")
                    .key_context(actions::INSPECTOR_DRAWER_KEY_CONTEXT)
                    .absolute()
                    .top_0()
                    .right_0()
                    .bottom_0()
                    .occlude()
            })
            .track_focus(&self.focus)
            .w(px(panel_width as f32))
            .h_full()
            .flex()
            .flex_col()
            .border_l_1()
            .border_color(rgb(if focused {
                theme.focus_ring.value()
            } else {
                theme.divider.value()
            }))
            .bg(rgb(theme.surface.value()))
            .child(
                div()
                    .h_12()
                    .px_token(DesignSpace::Lg)
                    .flex()
                    .items_center()
                    .justify_between()
                    .border_b_1()
                    .border_color(rgb(theme.divider.value()))
                    .child("INSPECTOR")
                    .when(presented_as_drawer, |header| {
                        header.child(
                            DesktopIconButton::new(
                                "close-inspector",
                                DesktopIcon::Close,
                                "Close Inspector",
                            )
                            .build()
                            .debug_selector(|| "desktop-hit-close-inspector".into())
                            .on_click(cx.listener(|_, _, _, cx| {
                                cx.emit(InspectorPaneEvent::Close);
                            })),
                        )
                    }),
            )
            .child(
                div()
                    .id("inspector-tabs")
                    .debug_selector(|| "desktop-inspector-tabs".into())
                    .role(Role::TabList)
                    .aria_label("Inspector sections")
                    .w_full()
                    .px_token(DesignSpace::Sm)
                    .py_token(DesignSpace::Sm)
                    .flex()
                    .min_w_0()
                    .overflow_x_scroll()
                    .track_scroll(&self.tab_scroll)
                    .gap_token(DesignSpace::Xs)
                    .border_b_1()
                    .border_color(rgb(theme.divider.value()))
                    .child(inspector_section_tab(
                        "inspector-changes",
                        "Changes",
                        InspectorSection::Changes,
                        selected_section,
                        self.tab_focus.clone(),
                        self.tab_scroll.clone(),
                        cx,
                    ))
                    .child(inspector_section_tab(
                        "inspector-task",
                        "Task",
                        InspectorSection::Task,
                        selected_section,
                        self.tab_focus.clone(),
                        self.tab_scroll.clone(),
                        cx,
                    ))
                    .child(inspector_section_tab(
                        "inspector-usage",
                        "Usage",
                        InspectorSection::Usage,
                        selected_section,
                        self.tab_focus.clone(),
                        self.tab_scroll.clone(),
                        cx,
                    ))
                    .child(
                        Badge::new()
                            .count(runtime_attention_count)
                            .max(99)
                            .color(rgb(theme.warning.value()))
                            .child(inspector_section_tab(
                                "inspector-runtime",
                                "Runtime",
                                InspectorSection::Runtime,
                                selected_section,
                                self.tab_focus.clone(),
                                self.tab_scroll.clone(),
                                cx,
                            )),
                    ),
            )
            .child(
                div()
                    .id("inspector-details")
                    .role(Role::TabPanel)
                    .aria_label(format!("{selected_section_label} Inspector details"))
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .p_token(DesignSpace::Lg)
                    .font_family(MONOSPACE_FONT_FAMILY)
                    .flex()
                    .flex_col()
                    .gap_token(DesignSpace::Md)
                    .when(selected_section == InspectorSection::Changes, |panel| {
                        panel
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .justify_between()
                                    .child(section("CHILD PROPOSALS", theme))
                                    .child(
                                        DesktopIconButton::new(
                                            "refresh-merge-proposals",
                                            DesktopIcon::Refresh,
                                            "Refresh pending child merge proposals",
                                        )
                                        .busy(merge_proposal_pending)
                                        .build()
                                        .disabled(
                                            composer_running
                                                || awaiting_prompt_start
                                                || merge_proposal_pending,
                                        )
                                        .on_click(
                                            cx.listener(|_, _, _, cx| {
                                                cx.emit(InspectorPaneEvent::RefreshMergeProposals);
                                            }),
                                        ),
                                    ),
                            )
                            .when(view_model.merge_proposals.is_empty(), |panel| {
                                panel.child(
                                    div()
                                        .text_color(rgb(theme.muted_text.value()))
                                        .child("No child proposals awaiting review."),
                                )
                            })
                            .children(proposal_rows)
                            .child(section("CHANGES", theme))
                            .child(format!("changed files {change_count}"))
                            .when(change_count == 0, |panel| {
                                panel.child(
                                    div()
                                        .text_color(rgb(theme.muted_text.value()))
                                        .child("No changed files in the current task."),
                                )
                            })
                            .children(changed_file_rows)
                            .when(omitted_changed_files > 0, |panel| {
                                panel.child(div().text_color(rgb(theme.warning.value())).child(
                                    format!("+ {omitted_changed_files} more change(s) omitted"),
                                ))
                            })
                            .child(section("REVIEW", theme))
                            .child(file_review_panel)
                    })
                    .when(selected_section == InspectorSection::Task, |panel| {
                        panel
                            .child(section("TASK", theme))
                            .child(format!("state       {}", view_model.task_state))
                            .child(format!("active op   {active_operation}"))
                            .child(format!("operations  {:>4}", view_model.operation_count))
                            .child(format!("delegations {:>4}", view_model.delegation_count))
                            .child(section("CONFIGURATION", theme))
                            .child(format!("model       {}", view_model.selected_model))
                            .child(format!("profile     {}", view_model.profile))
                            .child(format!("thinking    {thinking}"))
                    })
                    .when(selected_section == InspectorSection::Usage, |panel| {
                        panel
                            .child(section("USAGE", theme))
                            .child(format!("input       {}", view_model.usage_input))
                            .child(format!("output      {}", view_model.usage_output))
                            .child(format!("cache read  {}", view_model.usage_cache_read))
                            .child(format!("cache write {}", view_model.usage_cache_write))
                            .child(format!("tokens      {}", view_model.usage_tokens))
                            .child(format!("context     {}", view_model.usage_context))
                            .child(format!("cost        {}", view_model.usage_cost))
                    })
                    .when(selected_section == InspectorSection::Runtime, |panel| {
                        panel
                            .child(section("RUNTIME", theme))
                            .child(format!("state       {}", view_model.task_state))
                            .child(if view_model.reduced_motion {
                                "motion reduced"
                            } else {
                                "motion static"
                            })
                            .child(format!("stream      {}", view_model.stream_id))
                            .child(format!("sequence    {}", view_model.sequence))
                            .child(format!("generation  {}", view_model.generation))
                            .child(section("LOCAL RESOURCES", theme))
                            .child(format!("models      {}", view_model.model_count))
                            .child(format!("profiles    {}", view_model.profile_count))
                            .child(format!("skills      {}", view_model.skill_count))
                            .child(format!("prompts     {}", view_model.prompt_count))
                            .child(format!("context     {}", view_model.context_count))
                            .when_some(latest_recovery, |panel, recovery| {
                                panel
                                    .child(colored_section("LATEST RECOVERY", theme.warning))
                                    .child(format!("status      {}", recovery.status))
                                    .child(format!("recovery    {}", recovery.recovery_id))
                                    .child(format!("operation   {}", recovery.operation_id))
                                    .child(format!("attempts    {}", recovery.attempt_count))
                                    .child(format!("detail      {}", recovery.detail))
                                    .when_some(recovery.identity, |panel, identity| {
                                        let retry = identity.clone();
                                        let failed = identity.clone();
                                        panel.child(
                                            div()
                                                .flex()
                                                .flex_wrap()
                                                .gap_token(DesignSpace::Sm)
                                                .child(recovery_button(
                                                    "retry-recovery",
                                                    "Retry",
                                                    retry,
                                                    DesktopRecoveryAction::Retry,
                                                    recovery_pending,
                                                    cx,
                                                ))
                                                .child(recovery_button(
                                                    "fail-recovery",
                                                    "Mark failed",
                                                    failed,
                                                    DesktopRecoveryAction::MarkFailed,
                                                    recovery_pending,
                                                    cx,
                                                ))
                                                .child(recovery_button(
                                                    "abort-recovery",
                                                    "Abort",
                                                    identity,
                                                    DesktopRecoveryAction::Abort,
                                                    recovery_pending,
                                                    cx,
                                                )),
                                        )
                                    })
                            })
                            .when_some(latest_diagnostic, |panel, diagnostic| {
                                panel
                                    .child(colored_section("LATEST DIAGNOSTIC", theme.warning))
                                    .child(format!("sequence    {}", diagnostic.sequence))
                                    .child(format!("operation   {}", diagnostic.operation))
                                    .child(format!("detail      {}", diagnostic.detail))
                                    .when(diagnostic.truncated, |panel| {
                                        panel.child("detail      [truncated]")
                                    })
                            })
                            .when_some(latest_config_diagnostic, |panel, diagnostic| {
                                panel
                                    .child(colored_section(
                                        "LATEST CONFIG DIAGNOSTIC",
                                        theme.warning,
                                    ))
                                    .child(format!("code        {}", diagnostic.0))
                                    .child(format!("detail      {}", diagnostic.1))
                            })
                            .when_some(latest_issue, |panel, code| {
                                panel
                                    .child(colored_section("LATEST ISSUE", theme.danger))
                                    .child(format!("code        {code}"))
                            })
                            .child(
                                div()
                                    .mt_token(DesignSpace::Md)
                                    .text_token(DesignText::Body)
                                    .text_color(rgb(theme.muted_text.value()))
                                    .child(view_model.cwd),
                            )
                    }),
            )
            .into_any_element()
    }
}
