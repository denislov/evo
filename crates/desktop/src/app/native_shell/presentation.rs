//! Session workspace presentation: visual styling helpers, projection
//! reconciliation, and workspace construction.

use std::sync::Arc;

use coding_agent::api::embedding::{
    CodingAgentEmbeddingSnapshot, CodingAgentResourceCommand, CodingAgentWorkspaceSelection,
};
use desktop::preferences::DesktopThinkingLevel;
use desktop::projection::DesktopProjection;

use gpui::rgb;

use super::InspectorSection;
use crate::application::{
    catalog::ProjectCatalogController,
    state::DesktopState,
    workspace_state::{RuntimeWorkspaceDefaults, WorkspaceState},
};
use crate::application::{
    runtime_state::{RuntimeProjectionPresentation, RuntimeWorkspacePresentation},
    workspace_state::{admitted_thinking_selection, workspace_selection_from_embedding},
};
use crate::ui::conversation::controller::{ConversationController, ConversationSource};
use crate::ui::conversation::model::{ConversationBlockKind, DelegationStatus};
use crate::ui::shell::{SemanticColor, SemanticTheme};
use crate::ui::shell::{ShellConnection, ShellUiState, ShellViews};
use desktop::ui::conversation::ComposerState;
use desktop::ui::shell::SemanticStatus;

pub(crate) struct ConversationBlockVisual {
    pub(crate) glyph: &'static str,
    pub(crate) accent: SemanticColor,
    pub(crate) align_right: bool,
}

pub(crate) fn conversation_focus_accent(focused: bool, theme: SemanticTheme) -> SemanticColor {
    if focused { theme.accent } else { theme.divider }
}

pub(crate) fn semantic_status_color(status: SemanticStatus, theme: SemanticTheme) -> gpui::Rgba {
    rgb(match status {
        SemanticStatus::Idle => theme.muted_text.value(),
        SemanticStatus::Running => theme.accent.value(),
        SemanticStatus::Warning | SemanticStatus::Authorization => theme.warning.value(),
        SemanticStatus::Error => theme.danger.value(),
    })
}

/// Accent colour for a delegation's lifecycle state, mirroring
/// [`semantic_status_color`] for the delegation status vocabulary.
pub(crate) fn delegation_status_color(
    status: DelegationStatus,
    theme: SemanticTheme,
) -> gpui::Rgba {
    rgb(match status {
        DelegationStatus::Requested | DelegationStatus::Completed | DelegationStatus::Unknown => {
            theme.muted_text.value()
        }
        DelegationStatus::Running => theme.accent.value(),
        DelegationStatus::Failed => theme.danger.value(),
        DelegationStatus::Rejected
        | DelegationStatus::Cancelled
        | DelegationStatus::ConfirmationRequired => theme.warning.value(),
    })
}

pub(crate) fn conversation_block_visual(
    kind: ConversationBlockKind,
    is_error: bool,
    theme: SemanticTheme,
) -> ConversationBlockVisual {
    match kind {
        ConversationBlockKind::User => ConversationBlockVisual {
            glyph: "",
            accent: theme.accent,
            align_right: true,
        },
        ConversationBlockKind::Assistant => ConversationBlockVisual {
            glyph: "AI",
            accent: theme.text,
            align_right: false,
        },
        ConversationBlockKind::Tool => ConversationBlockVisual {
            glyph: "TOOL",
            accent: if is_error {
                theme.danger
            } else {
                theme.muted_text
            },
            align_right: false,
        },
        ConversationBlockKind::Delegation => ConversationBlockVisual {
            glyph: "AGENT",
            accent: theme.accent,
            align_right: false,
        },
        ConversationBlockKind::CompactionSummary | ConversationBlockKind::BranchSummary => {
            ConversationBlockVisual {
                glyph: "SUMMARY",
                accent: theme.muted_text,
                align_right: false,
            }
        }
        ConversationBlockKind::Diagnostic => ConversationBlockVisual {
            glyph: "ISSUE",
            accent: theme.danger,
            align_right: false,
        },
    }
}

#[derive(Default)]
pub(crate) struct SessionWorkspacePresentation {
    pub(crate) conversation_controller: ConversationController,
    pub(crate) inspector_section: InspectorSection,
}

impl RuntimeWorkspacePresentation for SessionWorkspacePresentation {
    fn mark_composer_accepted(&mut self) {
        self.conversation_controller.mark_live_dirty();
    }

    fn reconcile_projection(
        &mut self,
        composer: &mut ComposerState,
        update: RuntimeProjectionPresentation<'_>,
    ) -> bool {
        let RuntimeProjectionPresentation {
            projection,
            replaced,
            delta,
            sequence,
            completes_submitted_prompt,
            active_operation_after,
        } = update;
        self.conversation_controller
            .apply_projection_delta(replaced, delta, sequence);
        let mut composer_needs_sync = false;
        if replaced {
            if completes_submitted_prompt
                && !active_operation_after
                && composer.submitted().is_some()
            {
                if let Some((live_id, durable_id)) =
                    composer.reconcile_completed_submission(projection.conversation())
                {
                    self.conversation_controller
                        .reconcile_live_selection(&live_id, &durable_id);
                }
                composer_needs_sync = true;
            }
            let source = ConversationSource::new(projection, composer.submitted());
            self.conversation_controller
                .reconcile_hydration(&source, sequence);
        } else if delta.is_some_and(|delta| delta.conversation || delta.tools) {
            let source = ConversationSource::new(projection, composer.submitted());
            self.conversation_controller
                .reconcile_content(&source, sequence);
        }
        composer_needs_sync
    }
}

pub(crate) type SessionWorkspace = WorkspaceState<SessionWorkspacePresentation>;
pub(crate) type NativeDesktopState =
    DesktopState<SessionWorkspace, ProjectCatalogController, RuntimeWorkspaceDefaults>;

pub(crate) fn build_session_workspace(
    project: CodingAgentEmbeddingSnapshot,
    projection: Option<DesktopProjection>,
    preference_notice: Option<String>,
    thinking_selection: DesktopThinkingLevel,
    draft_workspace_selection: CodingAgentWorkspaceSelection,
) -> SessionWorkspace {
    let (thinking_selection, thinking_fallback) =
        admitted_thinking_selection(&project, thinking_selection);
    WorkspaceState::new(
        project,
        projection,
        draft_workspace_selection,
        preference_notice,
        thinking_selection,
        thinking_fallback,
        SessionWorkspacePresentation::default(),
    )
}

pub(crate) fn session_workspace_with_thinking(
    project: CodingAgentEmbeddingSnapshot,
    projection: Option<DesktopProjection>,
    preference_notice: Option<String>,
    thinking_selection: DesktopThinkingLevel,
) -> SessionWorkspace {
    let selection = workspace_selection_from_embedding(&project);
    build_session_workspace(
        project,
        projection,
        preference_notice,
        thinking_selection,
        selection,
    )
}

pub(crate) struct NativeShell {
    pub(in crate::app) connection: ShellConnection,
    pub(in crate::app) app: NativeDesktopState,
    pub(in crate::app) global_skills: Arc<[CodingAgentResourceCommand]>,
    pub(in crate::app) views: ShellViews,
    pub(in crate::app) ui: ShellUiState,
}
