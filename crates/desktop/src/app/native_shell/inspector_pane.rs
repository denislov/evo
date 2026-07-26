use coding_agent::api::review::CodingAgentFileReviewRequest;
use desktop::file_review::{DesktopReviewLineKind, MAX_VISIBLE_FILE_CHANGES};
use desktop::runtime::{DesktopRecoveryAction, DesktopRecoveryIdentity};
use desktop::shell::{
    CONTEXT_PANEL_WIDTH, MONOSPACE_FONT_FAMILY, SemanticColor, SemanticTheme, truncate_label,
};
use gpui::{
    EventEmitter, IntoElement, ParentElement as _, Render, Styled as _, WeakEntity, Window, div,
    prelude::*, px, rgb,
};
use gpui_component::{Disableable as _, button::Button};

use super::{
    DesktopCommandIntent, DesktopFileReviewState, DesktopRecoveryStatus, NativeShell, actions,
    recovery_status_label, runtime_state_label, usage_cost_label,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum InspectorPaneEvent {
    RequestFileReview(CodingAgentFileReviewRequest),
    CopyReviewPath,
    CopyFileReview,
    OpenExternalEditor,
    Recovery {
        identity: DesktopRecoveryIdentity,
        action: DesktopRecoveryAction,
    },
}

pub(super) struct InspectorPane {
    owner: WeakEntity<NativeShell>,
}

impl InspectorPane {
    pub(super) fn new(owner: WeakEntity<NativeShell>) -> Self {
        Self { owner }
    }
}

impl EventEmitter<InspectorPaneEvent> for InspectorPane {}

impl Render for InspectorPane {
    fn render(&mut self, window: &mut Window, cx: &mut gpui::Context<Self>) -> impl IntoElement {
        let Some(owner) = self.owner.upgrade() else {
            return div()
                .w(px(CONTEXT_PANEL_WIDTH as f32))
                .h_full()
                .into_any_element();
        };
        let owner = owner.read(cx);
        let theme = SemanticTheme::GEEK_DARK;
        let snapshot = owner.projection.snapshot();
        let project = owner.projection.project();
        let composer_running = snapshot.active_operation.is_some();
        let awaiting_prompt_start = owner.composer.submitted().is_some() && !composer_running;
        let recovery_pending = owner
            .command_ledger
            .contains_where(|intent| matches!(intent, DesktopCommandIntent::Recovery { .. }));
        let file_review_pending = owner
            .command_ledger
            .contains_where(|intent| matches!(intent, DesktopCommandIntent::FileReview { .. }));
        let external_editor_pending = owner
            .command_ledger
            .contains_where(|intent| matches!(intent, DesktopCommandIntent::ExternalEditor { .. }));
        let change_count = snapshot.context.changes.len();
        let changed_file_rows = snapshot
            .context
            .changes
            .iter()
            .take(MAX_VISIBLE_FILE_CHANGES)
            .enumerate()
            .map(|(index, change)| {
                let request = CodingAgentFileReviewRequest::from(change);
                Button::new(("changed-file-review", index))
                    .compact()
                    .label(format!(
                        "{}  {}",
                        truncate_label(&change.mutation_kind, 10),
                        truncate_label(&change.path, 38)
                    ))
                    .tooltip("Load this product-authorized changed-file review")
                    .disabled(composer_running || awaiting_prompt_start || file_review_pending)
                    .on_click(cx.listener(move |_, _, _, cx| {
                        cx.emit(InspectorPaneEvent::RequestFileReview(request.clone()));
                    }))
            })
            .collect::<Vec<_>>();
        let omitted_changed_files = change_count.saturating_sub(changed_file_rows.len());
        let file_review_panel = match &owner.file_review {
            DesktopFileReviewState::Empty => div()
                .text_sm()
                .text_color(rgb(theme.muted_text.value()))
                .child("Select a changed file to load a product-authorized preview."),
            DesktopFileReviewState::Loading(request) => div()
                .text_sm()
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
                    .gap_2()
                    .text_sm()
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
                            .gap_2()
                            .text_sm()
                            .text_color(rgb(color.value()))
                            .child(marker)
                            .child(row.text.clone())
                    })
                    .collect::<Vec<_>>();
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .text_sm()
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
                            .flex_wrap()
                            .gap_2()
                            .child(
                                Button::new("copy-review-path")
                                    .compact()
                                    .label("Copy path")
                                    .tooltip("Copy the reviewed project-relative path")
                                    .on_click(cx.listener(|_, _, _, cx| {
                                        cx.emit(InspectorPaneEvent::CopyReviewPath);
                                    })),
                            )
                            .child(
                                Button::new("copy-file-review")
                                    .compact()
                                    .label("Copy review")
                                    .tooltip("Copy the bounded read-only file review")
                                    .on_click(cx.listener(|_, _, _, cx| {
                                        cx.emit(InspectorPaneEvent::CopyFileReview);
                                    })),
                            )
                            .child(
                                Button::new("open-external-editor")
                                    .compact()
                                    .label("Open editor")
                                    .tooltip(
                                        "Revalidate and open this file in the configured editor",
                                    )
                                    .disabled(
                                        owner.preferences.external_editor.is_none()
                                            || external_editor_pending
                                            || composer_running
                                            || awaiting_prompt_start,
                                    )
                                    .on_click(cx.listener(|_, _, _, cx| {
                                        cx.emit(InspectorPaneEvent::OpenExternalEditor);
                                    })),
                            ),
                    )
                    .child(
                        div()
                            .mt_1()
                            .pl_2()
                            .border_l_1()
                            .border_color(rgb(theme.border.value()))
                            .flex()
                            .flex_col()
                            .gap_1()
                            .children(rows),
                    )
            }
        };
        let usage = &snapshot.context.usage;
        let latest_recovery = owner.projection.recoveries().front().map(|recovery| {
            (
                recovery_status_label(recovery.status),
                truncate_label(&recovery.recovery_id, 22),
                truncate_label(&recovery.operation_id, 22),
                truncate_label(&recovery.reason, 120),
                recovery.attempt_count,
                recovery.identity.clone().filter(|_| {
                    recovery.status == DesktopRecoveryStatus::Pending && recovery.authoritative
                }),
            )
        });
        let latest_diagnostic = owner.projection.diagnostics().back().map(|diagnostic| {
            (
                diagnostic.sequence,
                diagnostic
                    .operation_id
                    .as_deref()
                    .map(|id| truncate_label(id, 22))
                    .unwrap_or_else(|| "global".into()),
                truncate_label(&diagnostic.message, 120),
                diagnostic.truncated,
            )
        });
        let latest_config_diagnostic = project.diagnostics.last().map(|diagnostic| {
            (
                truncate_label(&diagnostic.code, 28),
                truncate_label(&diagnostic.summary, 120),
            )
        });
        let latest_issue = owner
            .projection
            .issues()
            .back()
            .map(|issue| truncate_label(&issue.code, 28));
        let active_operation = snapshot
            .active_operation
            .as_deref()
            .map(|id| truncate_label(id, 24))
            .unwrap_or_else(|| "—".into());
        let context_is_overlay = owner.narrow_context_open;
        let focused = owner.context_focus.is_focused(window);
        let thinking = owner
            .thinking_selection
            .label(project.settings.default_thinking_level.as_deref());

        div()
            .id("context-panel")
            .when(context_is_overlay, |panel| {
                panel
                    .key_context(actions::NARROW_CONTEXT_KEY_CONTEXT)
                    .absolute()
                    .top_0()
                    .right_0()
                    .bottom_0()
                    .occlude()
            })
            .track_focus(&owner.context_focus)
            .w(px(CONTEXT_PANEL_WIDTH as f32))
            .h_full()
            .flex()
            .flex_col()
            .border_l_1()
            .border_color(rgb(if focused {
                theme.focus_ring.value()
            } else {
                theme.border.value()
            }))
            .bg(rgb(theme.surface.value()))
            .child(
                div()
                    .h_12()
                    .px_4()
                    .flex()
                    .items_center()
                    .justify_between()
                    .border_b_1()
                    .border_color(rgb(theme.border.value()))
                    .child("CONTEXT")
                    .child("Tab focus"),
            )
            .child(
                div()
                    .id("context-details")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .p_4()
                    .font_family(MONOSPACE_FONT_FAMILY)
                    .flex()
                    .flex_col()
                    .gap_3()
                    .child(section("RUNTIME", theme))
                    .child(format!(
                        "state       {}",
                        runtime_state_label(owner.projection.lifecycle(), composer_running)
                    ))
                    .child(format!(
                        "stream      {}",
                        truncate_label(&snapshot.cursor.stream_id, 18)
                    ))
                    .child(format!(
                        "sequence    {}",
                        snapshot.cursor.last_event_sequence
                    ))
                    .child(format!(
                        "generation  {}",
                        snapshot.cursor.capability_generation
                    ))
                    .child(format!("active op   {active_operation}"))
                    .child(section("WORK", theme))
                    .child(format!(
                        "operations  {:>4}",
                        snapshot.context.operations.len()
                    ))
                    .child(format!("changes     {change_count:>4}"))
                    .child(format!(
                        "delegations {:>4}",
                        snapshot.context.delegations.len()
                    ))
                    .child(section("CHANGED FILES", theme))
                    .children(changed_file_rows)
                    .when(omitted_changed_files > 0, |panel| {
                        panel.child(
                            div()
                                .text_color(rgb(theme.warning.value()))
                                .child(format!("+ {omitted_changed_files} more change(s) omitted")),
                        )
                    })
                    .child(section("FILE REVIEW", theme))
                    .child(file_review_panel)
                    .child(format!(
                        "diagnostics {:>4}",
                        owner.projection.diagnostics().len()
                    ))
                    .child(format!(
                        "recoveries  {:>4}",
                        owner.projection.recoveries().len()
                    ))
                    .child(section("USAGE", theme))
                    .child(format!("input       {}", usage.input))
                    .child(format!("output      {}", usage.output))
                    .child(format!("cache read  {}", usage.cache_read))
                    .child(format!("cache write {}", usage.cache_write))
                    .child(format!(
                        "tokens      {}",
                        usage.input.saturating_add(usage.output)
                    ))
                    .child(format!(
                        "context     {}",
                        usage
                            .context_window
                            .map(|v| v.to_string())
                            .unwrap_or_else(|| "—".into())
                    ))
                    .child(format!("cost        {}", usage_cost_label(usage.cost)))
                    .child(section("LOCAL RESOURCES", theme))
                    .child(format!(
                        "model       {}",
                        truncate_label(&project.selected_model_id, 28)
                    ))
                    .child(format!(
                        "profile     {}",
                        truncate_label(snapshot.session.default_agent_profile_id.as_str(), 28)
                    ))
                    .child(format!("thinking    {thinking}"))
                    .child(format!("models      {}", project.models.len()))
                    .child(format!("profiles    {}", project.profiles.len()))
                    .child(format!(
                        "skills      {}",
                        project.resources.skill_names.len()
                    ))
                    .child(format!(
                        "prompts     {}",
                        project.resources.prompt_template_names.len()
                    ))
                    .child(format!(
                        "context     {}",
                        project.resources.context_files.len()
                    ))
                    .child(format!("config diag {}", project.diagnostics.len()))
                    .when_some(latest_recovery, |panel, recovery| {
                        panel
                            .child(colored_section("LATEST RECOVERY", theme.warning))
                            .child(format!("status      {}", recovery.0))
                            .child(format!("recovery    {}", recovery.1))
                            .child(format!("operation   {}", recovery.2))
                            .child(format!("attempts    {}", recovery.4))
                            .child(format!("detail      {}", recovery.3))
                            .when_some(recovery.5, |panel, identity| {
                                let retry = identity.clone();
                                let failed = identity.clone();
                                panel.child(
                                    div()
                                        .flex()
                                        .flex_wrap()
                                        .gap_2()
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
                            .child(format!("sequence    {}", diagnostic.0))
                            .child(format!("operation   {}", diagnostic.1))
                            .child(format!("detail      {}", diagnostic.2))
                            .when(diagnostic.3, |panel| panel.child("detail      [truncated]"))
                    })
                    .when_some(latest_config_diagnostic, |panel, diagnostic| {
                        panel
                            .child(colored_section("LATEST CONFIG DIAGNOSTIC", theme.warning))
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
                            .mt_3()
                            .text_sm()
                            .text_color(rgb(theme.muted_text.value()))
                            .child(truncate_label(&project.cwd.display().to_string(), 54)),
                    ),
            )
            .into_any_element()
    }
}

fn section(label: &'static str, theme: SemanticTheme) -> gpui::Div {
    colored_section(label, theme.accent)
}

fn colored_section(label: &'static str, color: SemanticColor) -> gpui::Div {
    div().mt_2().text_color(rgb(color.value())).child(label)
}

fn recovery_button(
    id: &'static str,
    label: &'static str,
    identity: DesktopRecoveryIdentity,
    action: DesktopRecoveryAction,
    disabled: bool,
    cx: &gpui::Context<InspectorPane>,
) -> Button {
    Button::new(id)
        .compact()
        .label(label)
        .tooltip(match action {
            DesktopRecoveryAction::Retry => "Retry this authoritative recovery",
            DesktopRecoveryAction::MarkFailed => "Resolve this recovery as failed",
            DesktopRecoveryAction::Abort => "Resolve this recovery as aborted",
        })
        .disabled(disabled)
        .on_click(cx.listener(move |_, _, _, cx| {
            cx.emit(InspectorPaneEvent::Recovery {
                identity: identity.clone(),
                action,
            });
        }))
}
