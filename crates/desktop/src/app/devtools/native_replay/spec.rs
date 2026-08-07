//! Visual replay layout/state spec parsing and viewport resolution.

use super::VISUAL_REPLAY_ENV;
use crate::app::native_shell::EvoBrandMode;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::app::devtools) enum NativeReplayRequest {
    Performance,
    Visual(VisualReplaySpec),
    Brand(EvoBrandMode),
    ClickToPhoton,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::app::devtools) enum VisualReplayLayout {
    Wide,
    Medium,
    Narrow,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::app::devtools) enum VisualReplayState {
    Standard,
    Idle,
    Authorization,
    ReducedMotion,
    KeyboardFocus,
    InspectorDrawer,
    ModelMenu,
    ThinkingMenu,
    ThinkingNonReasoning,
    HomeProject,
    HomeLongProject,
    CatalogLoading,
    CatalogError,
    CatalogEmpty,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::app::devtools) struct VisualReplaySpec {
    pub(in crate::app::devtools) layout: VisualReplayLayout,
    pub(in crate::app::devtools) state: VisualReplayState,
}

impl VisualReplaySpec {
    pub(in crate::app::devtools) fn parse(value: &str) -> Result<Self, String> {
        let (layout, state) = if let Some(layout) = value.strip_suffix("-idle") {
            (layout, VisualReplayState::Idle)
        } else if let Some(layout) = value.strip_suffix("-authorization") {
            (layout, VisualReplayState::Authorization)
        } else if let Some(layout) = value.strip_suffix("-reduced-motion") {
            (layout, VisualReplayState::ReducedMotion)
        } else if let Some(layout) = value.strip_suffix("-keyboard-focus") {
            (layout, VisualReplayState::KeyboardFocus)
        } else if let Some(layout) = value.strip_suffix("-inspector") {
            (layout, VisualReplayState::InspectorDrawer)
        } else if let Some(layout) = value.strip_suffix("-model-menu") {
            (layout, VisualReplayState::ModelMenu)
        } else if let Some(layout) = value.strip_suffix("-thinking-menu") {
            (layout, VisualReplayState::ThinkingMenu)
        } else if let Some(layout) = value.strip_suffix("-thinking-non-reasoning") {
            (layout, VisualReplayState::ThinkingNonReasoning)
        } else if let Some(layout) = value.strip_suffix("-home-project") {
            (layout, VisualReplayState::HomeProject)
        } else if let Some(layout) = value.strip_suffix("-home-long-project") {
            (layout, VisualReplayState::HomeLongProject)
        } else if let Some(layout) = value.strip_suffix("-catalog-loading") {
            (layout, VisualReplayState::CatalogLoading)
        } else if let Some(layout) = value.strip_suffix("-catalog-error") {
            (layout, VisualReplayState::CatalogError)
        } else if let Some(layout) = value.strip_suffix("-catalog-empty") {
            (layout, VisualReplayState::CatalogEmpty)
        } else {
            (value, VisualReplayState::Standard)
        };
        Ok(Self {
            layout: VisualReplayLayout::parse(layout)?,
            state,
        })
    }

    pub(in crate::app::devtools) fn key(self) -> String {
        let state = match self.state {
            VisualReplayState::Standard => return self.layout.key().into(),
            VisualReplayState::Idle => "idle",
            VisualReplayState::Authorization => "authorization",
            VisualReplayState::ReducedMotion => "reduced-motion",
            VisualReplayState::KeyboardFocus => "keyboard-focus",
            VisualReplayState::InspectorDrawer => "inspector",
            VisualReplayState::ModelMenu => "model-menu",
            VisualReplayState::ThinkingMenu => "thinking-menu",
            VisualReplayState::ThinkingNonReasoning => "thinking-non-reasoning",
            VisualReplayState::HomeProject => "home-project",
            VisualReplayState::HomeLongProject => "home-long-project",
            VisualReplayState::CatalogLoading => "catalog-loading",
            VisualReplayState::CatalogError => "catalog-error",
            VisualReplayState::CatalogEmpty => "catalog-empty",
        };
        format!("{}-{state}", self.layout.key())
    }
}

impl VisualReplayState {
    pub(in crate::app::devtools) const fn uses_home(self) -> bool {
        matches!(
            self,
            Self::Idle
                | Self::ModelMenu
                | Self::ThinkingMenu
                | Self::ThinkingNonReasoning
                | Self::HomeProject
                | Self::HomeLongProject
                | Self::CatalogLoading
                | Self::CatalogError
                | Self::CatalogEmpty
        )
    }
}

impl VisualReplayLayout {
    pub(in crate::app::devtools) fn parse(value: &str) -> Result<Self, String> {
        match value {
            "wide" => Ok(Self::Wide),
            "medium" => Ok(Self::Medium),
            "narrow" => Ok(Self::Narrow),
            other => Err(format!(
                "{VISUAL_REPLAY_ENV} must be wide, medium, or narrow; got {other}"
            )),
        }
    }

    pub(in crate::app::devtools) fn key(self) -> &'static str {
        match self {
            Self::Wide => "wide",
            Self::Medium => "medium",
            Self::Narrow => "narrow",
        }
    }

    pub(in crate::app::devtools) fn viewport(self) -> (f32, f32) {
        match self {
            Self::Wide => (1_300., 900.),
            Self::Medium => (900., 800.),
            Self::Narrow => (700., 800.),
        }
    }
}
