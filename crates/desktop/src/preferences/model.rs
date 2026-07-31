//! Serializable, normalized desktop preference model.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use coding_agent::api::embedding::CodingAgentThinkingLevel;

use crate::file_review::DesktopExternalEditorConfig;
use crate::shell::{
    CONTEXT_PANEL_MAX_WIDTH, CONTEXT_PANEL_MIN_WIDTH, CONTEXT_PANEL_WIDTH, SESSION_PANEL_MAX_WIDTH,
    SESSION_PANEL_MIN_WIDTH, SESSION_PANEL_WIDTH,
};

pub(crate) const PREFERENCES_SCHEMA_VERSION: u16 = 1;
const MAX_WORKSPACE_ID_BYTES: usize = 64;
pub(crate) const MAX_PERSISTED_SESSION_ID_BYTES: usize = 256;
pub(crate) const MAX_PERSISTED_SESSION_THINKING_LEVELS: usize = 128;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum DesktopThinkingLevel {
    Off,
    Minimal,
    Low,
    Medium,
    High,
    XHigh,
    #[default]
    #[serde(other)]
    Default,
}

impl DesktopThinkingLevel {
    pub(crate) const fn explicit(self) -> Option<CodingAgentThinkingLevel> {
        match self {
            Self::Default => None,
            Self::Off => Some(CodingAgentThinkingLevel::Off),
            Self::Minimal => Some(CodingAgentThinkingLevel::Minimal),
            Self::Low => Some(CodingAgentThinkingLevel::Low),
            Self::Medium => Some(CodingAgentThinkingLevel::Medium),
            Self::High => Some(CodingAgentThinkingLevel::High),
            Self::XHigh => Some(CodingAgentThinkingLevel::XHigh),
        }
    }

    pub(crate) const fn from_explicit(level: Option<CodingAgentThinkingLevel>) -> Self {
        match level {
            None => Self::Default,
            Some(CodingAgentThinkingLevel::Off) => Self::Off,
            Some(CodingAgentThinkingLevel::Minimal) => Self::Minimal,
            Some(CodingAgentThinkingLevel::Low) => Self::Low,
            Some(CodingAgentThinkingLevel::Medium) => Self::Medium,
            Some(CodingAgentThinkingLevel::High) => Self::High,
            Some(CodingAgentThinkingLevel::XHigh) => Self::XHigh,
        }
    }

    pub(crate) fn label(self, default: Option<&str>) -> String {
        match self {
            Self::Default => default
                .map(|level| format!("default:{}", crate::shell::truncate_label(level, 10)))
                .unwrap_or_else(|| "default".into()),
            Self::Off => "off".into(),
            Self::Minimal => "minimal".into(),
            Self::Low => "low".into(),
            Self::Medium => "medium".into(),
            Self::High => "high".into(),
            Self::XHigh => "xhigh".into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WindowGeometry {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub maximized: bool,
}

impl Default for WindowGeometry {
    fn default() -> Self {
        Self {
            x: 80,
            y: 60,
            width: 1_280,
            height: 840,
            maximized: false,
        }
    }
}

impl WindowGeometry {
    fn normalize(&mut self) {
        self.x = self.x.clamp(-32_768, 32_767);
        self.y = self.y.clamp(-32_768, 32_767);
        self.width = self.width.clamp(640, 7_680);
        self.height = self.height.clamp(480, 4_320);
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DesktopPreferences {
    pub schema_version: u16,
    pub window: WindowGeometry,
    pub sessions_panel_visible: bool,
    pub context_panel_visible: bool,
    #[serde(default = "default_sessions_panel_width")]
    pub sessions_panel_width: u32,
    #[serde(default = "default_context_panel_width")]
    pub context_panel_width: u32,
    pub reduced_motion: bool,
    pub ui_scale: f32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) external_editor: Option<DesktopExternalEditorConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) scratch_workspace_id: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub(crate) session_thinking_levels: BTreeMap<String, DesktopThinkingLevel>,
}

impl Default for DesktopPreferences {
    fn default() -> Self {
        Self {
            schema_version: PREFERENCES_SCHEMA_VERSION,
            window: WindowGeometry::default(),
            sessions_panel_visible: true,
            context_panel_visible: false,
            sessions_panel_width: SESSION_PANEL_WIDTH,
            context_panel_width: CONTEXT_PANEL_WIDTH,
            reduced_motion: false,
            ui_scale: 1.0,
            external_editor: None,
            scratch_workspace_id: None,
            session_thinking_levels: BTreeMap::new(),
        }
    }
}

impl DesktopPreferences {
    pub fn normalized(mut self) -> Self {
        self.schema_version = PREFERENCES_SCHEMA_VERSION;
        self.window.normalize();
        self.sessions_panel_width = self
            .sessions_panel_width
            .clamp(SESSION_PANEL_MIN_WIDTH, SESSION_PANEL_MAX_WIDTH);
        self.context_panel_width = self
            .context_panel_width
            .clamp(CONTEXT_PANEL_MIN_WIDTH, CONTEXT_PANEL_MAX_WIDTH);
        if !self.ui_scale.is_finite() {
            self.ui_scale = 1.0;
        }
        self.ui_scale = self.ui_scale.clamp(0.75, 2.0);
        if self
            .external_editor
            .as_ref()
            .is_some_and(|editor| editor.validate().is_err())
        {
            self.external_editor = None;
        }
        if self
            .scratch_workspace_id
            .as_deref()
            .is_some_and(|id| !valid_scratch_workspace_id(id))
        {
            self.scratch_workspace_id = None;
        }
        self.session_thinking_levels.retain(|session_id, level| {
            valid_persisted_session_id(session_id) && *level != DesktopThinkingLevel::Default
        });
        while self.session_thinking_levels.len() > MAX_PERSISTED_SESSION_THINKING_LEVELS {
            self.session_thinking_levels.pop_last();
        }
        self
    }

    pub(crate) fn thinking_level_for_session(&self, session_id: &str) -> DesktopThinkingLevel {
        self.session_thinking_levels
            .get(session_id)
            .copied()
            .unwrap_or_default()
    }

    pub(crate) fn set_thinking_level_for_session(
        &mut self,
        session_id: &str,
        level: DesktopThinkingLevel,
    ) -> bool {
        if !valid_persisted_session_id(session_id) {
            return false;
        }
        if level == DesktopThinkingLevel::Default {
            return self.session_thinking_levels.remove(session_id).is_some();
        }
        if self.session_thinking_levels.get(session_id) == Some(&level) {
            return false;
        }
        if !self.session_thinking_levels.contains_key(session_id)
            && self.session_thinking_levels.len() >= MAX_PERSISTED_SESSION_THINKING_LEVELS
        {
            self.session_thinking_levels.pop_first();
        }
        self.session_thinking_levels
            .insert(session_id.to_owned(), level);
        true
    }
}

fn valid_persisted_session_id(session_id: &str) -> bool {
    !session_id.is_empty() && session_id.len() <= MAX_PERSISTED_SESSION_ID_BYTES
}

const fn default_sessions_panel_width() -> u32 {
    SESSION_PANEL_WIDTH
}

const fn default_context_panel_width() -> u32 {
    CONTEXT_PANEL_WIDTH
}

pub(crate) fn valid_scratch_workspace_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_WORKSPACE_ID_BYTES
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_preferences_open_sidebar_and_close_inspector() {
        let preferences = DesktopPreferences::default();
        assert!(preferences.sessions_panel_visible);
        assert!(!preferences.context_panel_visible);
    }

    #[test]
    fn untrusted_scratch_workspace_ids_are_discarded_before_path_resolution() {
        for invalid in ["", "../escape", "nested/path", "x\\y", &"x".repeat(65)] {
            let preferences = DesktopPreferences {
                scratch_workspace_id: Some(invalid.to_owned()),
                ..DesktopPreferences::default()
            }
            .normalized();
            assert!(preferences.scratch_workspace_id.is_none());
        }
    }

    #[test]
    fn legacy_preferences_gain_default_panel_widths() {
        let legacy = serde_json::json!({
            "schema_version": PREFERENCES_SCHEMA_VERSION,
            "window": {
                "x": 0, "y": 0, "width": 1200, "height": 800, "maximized": false
            },
            "sessions_panel_visible": true,
            "context_panel_visible": true,
            "reduced_motion": false,
            "ui_scale": 1.0
        });
        let preferences: DesktopPreferences = serde_json::from_value(legacy).unwrap();
        assert_eq!(preferences.sessions_panel_width, SESSION_PANEL_WIDTH);
        assert_eq!(preferences.context_panel_width, CONTEXT_PANEL_WIDTH);
        assert!(preferences.scratch_workspace_id.is_none());
        assert!(preferences.session_thinking_levels.is_empty());
    }
}
