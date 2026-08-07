//! Inspector view model derivation from workspace state and shell UI state.

use std::sync::Arc;

use crate::app::native_shell::NativeDesktopState;
use crate::application::commands::DesktopCommandIntent;
use crate::ui::inspector::review::MAX_VISIBLE_FILE_CHANGES;
use crate::ui::shell::drawer::CenterDrawerKind;
use crate::ui::shell::presentation::{
    recovery_status_label, runtime_state_label, usage_cost_label,
};
use crate::ui::shell::{ShellUiState, truncate_label};
use coding_agent::api::review::CodingAgentFileReviewRequest;
use desktop::projection::DesktopRecoveryStatus;

use super::{
    InspectorChangedFileView, InspectorDiagnosticView, InspectorPaneViewModel,
    InspectorRecoveryView,
};

pub(crate) fn view_model(
    app: &NativeDesktopState,
    ui: &ShellUiState,
    global_skill_count: usize,
) -> InspectorPaneViewModel {
    let workspace = app.workspaces.active();
    let project = &workspace.project;
    let pending = |predicate: fn(&DesktopCommandIntent) -> bool| {
        app.commands
            .contains_where(app.workspaces.active_key(), predicate)
    };
    let Some(projection) = workspace.projection.as_ref() else {
        return InspectorPaneViewModel {
            panel_width: app.preferences.context_panel_width,
            presented_as_drawer: ui.active_drawer == Some(CenterDrawerKind::Inspector),
            keyboard_focus_visible: ui.keyboard_focus_visible(),
            selected_section: workspace.presentation.inspector_section,
            composer_running: false,
            awaiting_prompt_start: workspace.composer.submitted().is_some(),
            recovery_pending: false,
            file_review_pending: false,
            external_editor_pending: false,
            external_editor_configured: app.preferences.external_editor.is_some(),
            changed_files: Vec::new(),
            change_count: 0,
            file_review: Arc::clone(&workspace.file_review),
            merge_proposals: Arc::clone(&workspace.merge_proposals),
            merge_proposal_pending: false,
            runtime_attention_count: project.diagnostics.len(),
            task_state: "ready".into(),
            active_operation: "—".into(),
            operation_count: 0,
            delegation_count: 0,
            selected_model: truncate_label(&project.selected_model_id, 28),
            profile: truncate_label(project.default_agent_profile_id.as_str(), 28),
            thinking: workspace
                .thinking_selection
                .label(project.settings.default_thinking_level.as_deref()),
            usage_input: "0".into(),
            usage_output: "0".into(),
            usage_cache_read: "0".into(),
            usage_cache_write: "0".into(),
            usage_tokens: "0".into(),
            usage_context: "—".into(),
            usage_cost: "—".into(),
            reduced_motion: app.preferences.reduced_motion,
            stream_id: "—".into(),
            sequence: "0".into(),
            generation: "0".into(),
            model_count: project.models.len(),
            profile_count: project.profiles.len(),
            skill_count: global_skill_count,
            prompt_count: 0,
            context_count: 0,
            latest_recovery: None,
            latest_diagnostic: None,
            latest_config_diagnostic: project.diagnostics.last().map(|diagnostic| {
                (
                    truncate_label(&diagnostic.code, 28),
                    truncate_label(&diagnostic.summary, 120),
                )
            }),
            latest_issue: None,
            cwd: truncate_label(&project.cwd.display().to_string(), 54),
        };
    };

    let snapshot = projection.snapshot();
    let composer_running = snapshot.active_operation.is_some();
    let awaiting_prompt_start = workspace.composer.submitted().is_some() && !composer_running;
    let changed_files = snapshot
        .context
        .changes
        .iter()
        .take(MAX_VISIBLE_FILE_CHANGES)
        .map(|change| InspectorChangedFileView {
            request: CodingAgentFileReviewRequest::from(change),
            mutation_kind: truncate_label(&change.mutation_kind, 10),
            file_name: truncate_label(
                change
                    .path
                    .rsplit(['/', '\\'])
                    .next()
                    .unwrap_or(change.path.as_str()),
                22,
            ),
            path: truncate_label(&change.path, 34),
        })
        .collect();
    let latest_recovery = projection
        .recoveries()
        .front()
        .map(|recovery| InspectorRecoveryView {
            status: recovery_status_label(recovery.status).to_owned(),
            recovery_id: truncate_label(&recovery.recovery_id, 22),
            operation_id: truncate_label(&recovery.operation_id, 22),
            detail: truncate_label(&recovery.reason, 120),
            attempt_count: recovery.attempt_count.to_string(),
            identity: recovery.identity.clone().filter(|_| {
                recovery.status == DesktopRecoveryStatus::Pending && recovery.authoritative
            }),
        });
    let latest_diagnostic =
        projection
            .diagnostics()
            .back()
            .map(|diagnostic| InspectorDiagnosticView {
                sequence: diagnostic.sequence.to_string(),
                operation: diagnostic
                    .operation_id
                    .as_deref()
                    .map(|id| truncate_label(id, 22))
                    .unwrap_or_else(|| "global".into()),
                detail: truncate_label(&diagnostic.message, 120),
                truncated: diagnostic.truncated,
            });
    let latest_config_diagnostic = project.diagnostics.last().map(|diagnostic| {
        (
            truncate_label(&diagnostic.code, 28),
            truncate_label(&diagnostic.summary, 120),
        )
    });
    let latest_issue = projection
        .issues()
        .back()
        .map(|issue| truncate_label(&issue.code, 28));
    let runtime_attention_count = projection
        .diagnostics()
        .len()
        .saturating_add(projection.recoveries().len())
        .saturating_add(project.diagnostics.len())
        .saturating_add(projection.issues().len());
    let usage = &snapshot.context.usage;
    InspectorPaneViewModel {
        panel_width: app.preferences.context_panel_width,
        presented_as_drawer: ui.active_drawer == Some(CenterDrawerKind::Inspector),
        keyboard_focus_visible: ui.keyboard_focus_visible(),
        selected_section: workspace.presentation.inspector_section,
        composer_running,
        awaiting_prompt_start,
        recovery_pending: pending(|intent| matches!(intent, DesktopCommandIntent::Recovery { .. })),
        file_review_pending: pending(|intent| {
            matches!(intent, DesktopCommandIntent::FileReview { .. })
        }),
        external_editor_pending: pending(|intent| {
            matches!(intent, DesktopCommandIntent::ExternalEditor { .. })
        }),
        external_editor_configured: app.preferences.external_editor.is_some(),
        changed_files,
        change_count: snapshot.context.changes.len(),
        file_review: Arc::clone(&workspace.file_review),
        merge_proposals: Arc::clone(&workspace.merge_proposals),
        merge_proposal_pending: pending(|intent| {
            matches!(
                intent,
                DesktopCommandIntent::ListMergeProposals
                    | DesktopCommandIntent::MergeProposal { .. }
                    | DesktopCommandIntent::DiscardProposal { .. }
            )
        }),
        runtime_attention_count,
        task_state: runtime_state_label(projection.lifecycle(), composer_running).to_owned(),
        active_operation: snapshot
            .active_operation
            .as_deref()
            .map(|id| truncate_label(id, 24))
            .unwrap_or_else(|| "—".into()),
        operation_count: snapshot.context.operations.len(),
        delegation_count: snapshot.context.delegations.len(),
        selected_model: truncate_label(&project.selected_model_id, 28),
        profile: truncate_label(snapshot.session.default_agent_profile_id.as_str(), 28),
        thinking: workspace
            .thinking_selection
            .label(project.settings.default_thinking_level.as_deref()),
        usage_input: usage.input.to_string(),
        usage_output: usage.output.to_string(),
        usage_cache_read: usage.cache_read.to_string(),
        usage_cache_write: usage.cache_write.to_string(),
        usage_tokens: usage.input.saturating_add(usage.output).to_string(),
        usage_context: usage
            .context_window
            .map(|value| value.to_string())
            .unwrap_or_else(|| "—".into()),
        usage_cost: usage_cost_label(usage.cost),
        reduced_motion: app.preferences.reduced_motion,
        stream_id: truncate_label(&snapshot.cursor.stream_id, 18),
        sequence: snapshot.cursor.last_event_sequence.to_string(),
        generation: snapshot.cursor.capability_generation.to_string(),
        model_count: project.models.len(),
        profile_count: project.profiles.len(),
        skill_count: project.resources.skill_names.len(),
        prompt_count: project.resources.prompt_template_names.len(),
        context_count: project.resources.context_files.len(),
        latest_recovery,
        latest_diagnostic,
        latest_config_diagnostic,
        latest_issue,
        cwd: truncate_label(&project.cwd.display().to_string(), 54),
    }
}
