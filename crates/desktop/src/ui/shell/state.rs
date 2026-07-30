use std::time::Instant;

use gpui::FocusHandle;

use crate::actions::DesktopCommandPalette;
use crate::app::native_shell::{
    ConversationFullMessageView, DesktopModalKind, FocusInputModality, PanelResizeState,
    center_drawer_host::CenterDrawerKind, center_navigation::CenterSurface,
};
use crate::application::workspace::WorkspaceKey;
use crate::shell::{FocusState, FocusTarget};

/// Window-local interaction state. Product and runtime facts do not belong here.
pub(crate) struct ShellUiState {
    pub(crate) focus: FocusState,
    pub(crate) center_header_focus: FocusHandle,
    pub(crate) sidebar_focus: FocusHandle,
    pub(crate) center_body_focus: FocusHandle,
    pub(crate) inspector_focus: FocusHandle,
    pub(crate) authorization_focus: FocusHandle,
    pub(crate) command_palette_focus: FocusHandle,
    pub(crate) full_message_focus: FocusHandle,
    pub(crate) command_palette: DesktopCommandPalette,
    pub(crate) active_modal: Option<DesktopModalKind>,
    pub(crate) active_drawer: Option<CenterDrawerKind>,
    pub(crate) center_surface: CenterSurface,
    pub(crate) drawer_restore_focus: Option<FocusTarget>,
    pub(crate) conversation_full_message: Option<ConversationFullMessageView>,
    pub(crate) conversation_announcement: Option<(WorkspaceKey, u64, String)>,
    conversation_announcement_sequence: u64,
    pub(crate) panel_resize: Option<PanelResizeState>,
    pub(crate) focus_input_modality: FocusInputModality,
    pub(crate) inspector_telemetry_last_refresh: Option<Instant>,
    pub(crate) inspector_telemetry_refresh_deadline: Option<Instant>,
    #[cfg(test)]
    pub(crate) runtime_ui_notification_count: usize,
}

impl ShellUiState {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        center_header_focus: FocusHandle,
        sidebar_focus: FocusHandle,
        center_body_focus: FocusHandle,
        inspector_focus: FocusHandle,
        authorization_focus: FocusHandle,
        command_palette_focus: FocusHandle,
        full_message_focus: FocusHandle,
    ) -> Self {
        Self {
            focus: FocusState::default(),
            center_header_focus,
            sidebar_focus,
            center_body_focus,
            inspector_focus,
            authorization_focus,
            command_palette_focus,
            full_message_focus,
            command_palette: DesktopCommandPalette::default(),
            active_modal: None,
            active_drawer: None,
            center_surface: CenterSurface::Primary,
            drawer_restore_focus: None,
            conversation_full_message: None,
            conversation_announcement: None,
            conversation_announcement_sequence: 0,
            panel_resize: None,
            focus_input_modality: FocusInputModality::default(),
            inspector_telemetry_last_refresh: None,
            inspector_telemetry_refresh_deadline: None,
            #[cfg(test)]
            runtime_ui_notification_count: 0,
        }
    }

    pub(crate) fn announce_conversation(
        &mut self,
        owner: WorkspaceKey,
        message: impl Into<String>,
    ) -> u64 {
        self.conversation_announcement_sequence = self
            .conversation_announcement_sequence
            .wrapping_add(1)
            .max(1);
        let sequence = self.conversation_announcement_sequence;
        self.conversation_announcement = Some((owner, sequence, message.into()));
        sequence
    }

    pub(crate) fn clear_conversation_announcement(&mut self, owner: &WorkspaceKey) -> bool {
        if !self
            .conversation_announcement
            .as_ref()
            .is_some_and(|(announcement_owner, _, _)| announcement_owner == owner)
        {
            return false;
        }
        self.conversation_announcement = None;
        true
    }
}
