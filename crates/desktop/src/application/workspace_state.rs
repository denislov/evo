use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use coding_agent::api::{
    embedding::{
        CodingAgentEmbeddingSnapshot, CodingAgentModelChoice, CodingAgentThinkingLevel,
        CodingAgentWorkspaceScope, CodingAgentWorkspaceSelection,
    },
    review::CodingAgentFileReviewRequest,
};
use desktop::preferences::DesktopThinkingLevel;
use desktop::projection::DesktopProjection;
use desktop::runtime::{DesktopPromptTarget, DesktopRuntimeOwnerTarget};
use desktop::ui::conversation::{ComposerAdmission, ComposerState};
use desktop::ui::inspector::review::DesktopFileReviewDocument;

pub(crate) const MAX_SESSION_WORKSPACES: usize = 4;

#[derive(Clone)]
pub(crate) struct RuntimeWorkspaceDefaults {
    pub(crate) home_project: CodingAgentEmbeddingSnapshot,
    pub(crate) projectless_selection: CodingAgentWorkspaceSelection,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) enum DesktopFileReviewState {
    #[default]
    Empty,
    Loading(CodingAgentFileReviewRequest),
    Ready(DesktopFileReviewDocument),
    Failed {
        request: CodingAgentFileReviewRequest,
        code: String,
    },
}

/// Application-owned workspace facts plus a feature-owned presentation extension.
pub(crate) struct WorkspaceState<Presentation> {
    pub(crate) project: CodingAgentEmbeddingSnapshot,
    pub(crate) projection: Option<DesktopProjection>,
    pub(crate) draft_workspace_selection: CodingAgentWorkspaceSelection,
    pub(crate) preference_notice: Option<String>,
    pub(crate) preference_notice_revision: u64,
    pub(crate) composer: ComposerState,
    pub(crate) composer_needs_sync: bool,
    pub(crate) composer_attachments: Vec<PathBuf>,
    pub(crate) thinking_selection: DesktopThinkingLevel,
    pub(crate) thinking_hint: Option<Arc<str>>,
    pub(crate) file_review: Arc<DesktopFileReviewState>,
    pub(crate) presentation: Presentation,
}

impl<Presentation> WorkspaceState<Presentation> {
    pub(crate) fn new(
        project: CodingAgentEmbeddingSnapshot,
        projection: Option<DesktopProjection>,
        draft_workspace_selection: CodingAgentWorkspaceSelection,
        preference_notice: Option<String>,
        thinking_selection: DesktopThinkingLevel,
        thinking_fallback: bool,
        presentation: Presentation,
    ) -> Self {
        let preference_notice_revision = u64::from(preference_notice.is_some());
        Self {
            project,
            projection,
            draft_workspace_selection,
            preference_notice,
            preference_notice_revision,
            composer: ComposerState::default(),
            composer_needs_sync: false,
            composer_attachments: Vec::new(),
            thinking_selection,
            thinking_hint: thinking_fallback
                .then(|| Arc::from("Thinking reset to Auto for the selected model.")),
            file_review: Arc::new(DesktopFileReviewState::default()),
            presentation,
        }
    }

    pub(crate) fn prompt_target(&self) -> DesktopPromptTarget {
        if let Some(projection) = self.projection.as_ref() {
            return DesktopPromptTarget::existing(projection.snapshot().session.session_id.clone());
        }
        DesktopPromptTarget::new(
            self.draft_workspace_selection.clone(),
            self.project.selected_model_id.clone(),
            self.project.default_agent_profile_id.as_str(),
        )
    }

    pub(crate) fn project_directory(&self) -> Option<&Path> {
        if self.projection.is_none() {
            return match &self.draft_workspace_selection {
                CodingAgentWorkspaceSelection::Project { cwd } => Some(cwd.as_path()),
                CodingAgentWorkspaceSelection::Projectless { .. } => None,
            };
        }
        match self
            .project
            .workspace
            .as_ref()
            .map(|workspace| &workspace.scope)
        {
            Some(CodingAgentWorkspaceScope::Project { cwd })
            | Some(CodingAgentWorkspaceScope::Legacy { cwd: Some(cwd) }) => Some(cwd.as_path()),
            Some(CodingAgentWorkspaceScope::Projectless { .. })
            | Some(CodingAgentWorkspaceScope::Legacy { cwd: None }) => None,
            None => Some(self.project.cwd.as_path()),
        }
    }

    pub(crate) fn project_directory_editable(&self) -> bool {
        self.projection.is_none()
            && matches!(self.composer.admission(), ComposerAdmission::Idle)
            && self.composer.submitted().is_none()
    }

    pub(crate) fn runtime_owner_target(&self) -> DesktopRuntimeOwnerTarget {
        self.projection
            .as_ref()
            .map_or_else(DesktopRuntimeOwnerTarget::home, |projection| {
                DesktopRuntimeOwnerTarget::session(projection.snapshot().session.session_id.clone())
            })
    }

    pub(crate) fn set_preference_notice(&mut self, message: String) {
        self.preference_notice = Some(message);
        self.preference_notice_revision = self.preference_notice_revision.wrapping_add(1).max(1);
    }
}

pub(crate) fn workspace_selection_from_embedding(
    project: &CodingAgentEmbeddingSnapshot,
) -> CodingAgentWorkspaceSelection {
    match project.workspace.as_ref().map(|workspace| &workspace.scope) {
        Some(CodingAgentWorkspaceScope::Project { cwd })
        | Some(CodingAgentWorkspaceScope::Legacy { cwd: Some(cwd) }) => {
            CodingAgentWorkspaceSelection::project(cwd.clone())
        }
        Some(CodingAgentWorkspaceScope::Projectless { workspace_id }) => {
            CodingAgentWorkspaceSelection::projectless(workspace_id.clone())
        }
        Some(CodingAgentWorkspaceScope::Legacy { cwd: None }) | None => {
            CodingAgentWorkspaceSelection::project(project.cwd.clone())
        }
    }
}

pub(crate) fn admitted_thinking_selection(
    project: &CodingAgentEmbeddingSnapshot,
    requested: DesktopThinkingLevel,
) -> (DesktopThinkingLevel, bool) {
    if requested == DesktopThinkingLevel::Default {
        return (requested, false);
    }
    let model = project
        .models
        .iter()
        .find(|model| model.id == project.selected_model_id);
    if thinking_selection_supported(model, requested) {
        (requested, false)
    } else {
        (DesktopThinkingLevel::Default, true)
    }
}

pub(crate) fn thinking_selection_supported(
    model: Option<&CodingAgentModelChoice>,
    requested: DesktopThinkingLevel,
) -> bool {
    if requested == DesktopThinkingLevel::Default {
        return true;
    }
    let Some(capability) = model.map(|model| &model.thinking_capability) else {
        return false;
    };
    if !capability.supported {
        return false;
    }
    match requested.explicit() {
        None => true,
        Some(CodingAgentThinkingLevel::Off) => capability.can_disable,
        Some(level) => capability.explicit_levels.contains(&level),
    }
}
