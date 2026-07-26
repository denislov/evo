//! Typed desktop actions, key bindings, and command-palette state.
//!
//! This module deliberately contains no runtime or presentation callbacks.
//! GPUI dispatches concrete action types, while the native shell maps the
//! closed `DesktopPaletteCommand` set onto existing typed command paths.

use gpui::{App, KeyBinding, actions};

pub(crate) const ROOT_KEY_CONTEXT: &str = "PiDesktop";
pub(crate) const CONVERSATION_KEY_CONTEXT: &str = "PiDesktopConversation";
pub(crate) const PALETTE_KEY_CONTEXT: &str = "PiDesktopPalette";
pub(crate) const AUTHORIZATION_KEY_CONTEXT: &str = "PiDesktopAuthorization";
pub(crate) const NARROW_SESSIONS_KEY_CONTEXT: &str = "PiDesktopNarrowSessions";
pub(crate) const NARROW_CONTEXT_KEY_CONTEXT: &str = "PiDesktopNarrowContext";

actions!(
    desktop,
    [
        OpenCommandPalette,
        OpenFileSurface,
        NewSession,
        FocusComposer,
        SubmitComposer,
        AbortActiveOperation,
        EscapeHierarchy,
        FollowLatestOutput,
        ToggleContextPanel,
        FocusNextRegion,
        FocusPreviousRegion,
        SelectPreviousConversation,
        SelectNextConversation,
        CopySelectedConversation,
        ToggleSelectedConversationDetails,
        PalettePrevious,
        PaletteNext,
        PaletteConfirm,
        AuthorizationDeny,
        AuthorizationAllowOnce,
        AuthorizationAllowForOperation,
        TrapOverlayFocus,
    ]
);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum DesktopPaletteCommand {
    NewSession,
    SwitchNextSession,
    ToggleSessions,
    ToggleContext,
    FocusSessions,
    FocusConversation,
    FocusComposer,
    FocusContext,
    SubmitPrompt,
    SteerOperation,
    FollowUpOperation,
    AbortOperation,
    FollowLatest,
    ReloadResources,
    CopyConversation,
    SelectNextModel,
    SelectNextProfile,
    CycleThinking,
    ReviewNextFile,
    CopyReviewPath,
    CopyFileReview,
    OpenExternalEditor,
    RetryRecovery,
    MarkRecoveryFailed,
    AbortRecovery,
    ToggleReducedMotion,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DesktopPaletteEntry {
    pub command: DesktopPaletteCommand,
    pub label: &'static str,
    pub shortcut: Option<&'static str>,
    pub semantic_label: &'static str,
}

pub(crate) const PALETTE_ENTRIES: &[DesktopPaletteEntry] = &[
    entry(
        DesktopPaletteCommand::NewSession,
        "New session",
        Some("Ctrl/Cmd+N"),
        "Create a new coding-agent session",
    ),
    entry(
        DesktopPaletteCommand::SwitchNextSession,
        "Switch to next session",
        None,
        "Switch to the next product-listed session",
    ),
    entry(
        DesktopPaletteCommand::ToggleSessions,
        "Toggle sessions",
        None,
        "Show or hide the sessions surface",
    ),
    entry(
        DesktopPaletteCommand::ToggleContext,
        "Toggle context",
        Some("Ctrl/Cmd+\\"),
        "Show or hide the context surface",
    ),
    entry(
        DesktopPaletteCommand::FocusSessions,
        "Focus sessions",
        None,
        "Move keyboard focus to the sessions surface",
    ),
    entry(
        DesktopPaletteCommand::FocusConversation,
        "Focus conversation",
        None,
        "Move keyboard focus to the conversation transcript",
    ),
    entry(
        DesktopPaletteCommand::FocusComposer,
        "Focus composer",
        Some("Ctrl/Cmd+L"),
        "Return keyboard focus to the prompt composer",
    ),
    entry(
        DesktopPaletteCommand::FocusContext,
        "Focus context",
        None,
        "Move keyboard focus to the context surface",
    ),
    entry(
        DesktopPaletteCommand::SubmitPrompt,
        "Send prompt",
        Some("Ctrl/Cmd+Enter"),
        "Submit the current composer draft as a new prompt",
    ),
    entry(
        DesktopPaletteCommand::SteerOperation,
        "Steer active operation",
        None,
        "Submit the composer draft as steering input",
    ),
    entry(
        DesktopPaletteCommand::FollowUpOperation,
        "Queue follow-up",
        None,
        "Submit the composer draft as a follow-up",
    ),
    entry(
        DesktopPaletteCommand::AbortOperation,
        "Abort active operation",
        Some("Ctrl/Cmd+Esc"),
        "Request cancellation of the active operation",
    ),
    entry(
        DesktopPaletteCommand::FollowLatest,
        "Jump to latest output",
        Some("End"),
        "Resume follow-latest and jump to the newest conversation block",
    ),
    entry(
        DesktopPaletteCommand::ReloadResources,
        "Reload local resources",
        None,
        "Reload product-owned local models, profiles, skills, and prompts",
    ),
    entry(
        DesktopPaletteCommand::CopyConversation,
        "Copy selected conversation block",
        None,
        "Copy the selected durable conversation block",
    ),
    entry(
        DesktopPaletteCommand::SelectNextModel,
        "Select next model",
        None,
        "Select the next configured text model",
    ),
    entry(
        DesktopPaletteCommand::SelectNextProfile,
        "Select next agent profile",
        None,
        "Select the next available session agent profile",
    ),
    entry(
        DesktopPaletteCommand::CycleThinking,
        "Cycle thinking level",
        None,
        "Cycle the composer thinking override",
    ),
    entry(
        DesktopPaletteCommand::ReviewNextFile,
        "Review next changed file",
        Some("Ctrl/Cmd+P"),
        "Load the next product-authorized changed-file review",
    ),
    entry(
        DesktopPaletteCommand::CopyReviewPath,
        "Copy reviewed path",
        None,
        "Copy the current reviewed project-relative path",
    ),
    entry(
        DesktopPaletteCommand::CopyFileReview,
        "Copy file review",
        None,
        "Copy the current bounded file review",
    ),
    entry(
        DesktopPaletteCommand::OpenExternalEditor,
        "Open review in external editor",
        None,
        "Revalidate and open the current reviewed file in the configured editor",
    ),
    entry(
        DesktopPaletteCommand::RetryRecovery,
        "Retry latest recovery",
        None,
        "Retry the latest authoritative pending recovery",
    ),
    entry(
        DesktopPaletteCommand::MarkRecoveryFailed,
        "Mark latest recovery failed",
        None,
        "Resolve the latest authoritative pending recovery as failed",
    ),
    entry(
        DesktopPaletteCommand::AbortRecovery,
        "Abort latest recovery",
        None,
        "Resolve the latest authoritative pending recovery as aborted",
    ),
    entry(
        DesktopPaletteCommand::ToggleReducedMotion,
        "Toggle reduced motion",
        None,
        "Disable or enable nonessential desktop motion",
    ),
];

const fn entry(
    command: DesktopPaletteCommand,
    label: &'static str,
    shortcut: Option<&'static str>,
    semantic_label: &'static str,
) -> DesktopPaletteEntry {
    DesktopPaletteEntry {
        command,
        label,
        shortcut,
        semantic_label,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct DesktopCommandPalette {
    open: bool,
    selected: usize,
}

impl DesktopCommandPalette {
    pub const fn is_open(self) -> bool {
        self.open
    }

    pub const fn selected(self) -> usize {
        self.selected
    }

    pub fn open(&mut self) {
        self.open = true;
        self.selected = 0;
    }

    pub fn close(&mut self) {
        self.open = false;
    }

    pub fn move_selection(&mut self, reverse: bool) {
        if !self.open || PALETTE_ENTRIES.is_empty() {
            return;
        }
        self.selected = if reverse {
            self.selected
                .checked_sub(1)
                .unwrap_or(PALETTE_ENTRIES.len() - 1)
        } else {
            (self.selected + 1) % PALETTE_ENTRIES.len()
        };
    }

    pub fn selected_command(self) -> Option<DesktopPaletteCommand> {
        self.open
            .then(|| {
                PALETTE_ENTRIES
                    .get(self.selected)
                    .map(|entry| entry.command)
            })
            .flatten()
    }
}

#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct BindingSpec {
    keystroke: &'static str,
    context: &'static str,
}

#[cfg(test)]
const ROOT_BINDINGS: &[BindingSpec] = &[
    binding("ctrl-k", ROOT_KEY_CONTEXT),
    binding("cmd-k", ROOT_KEY_CONTEXT),
    binding("ctrl-p", ROOT_KEY_CONTEXT),
    binding("cmd-p", ROOT_KEY_CONTEXT),
    binding("ctrl-n", ROOT_KEY_CONTEXT),
    binding("cmd-n", ROOT_KEY_CONTEXT),
    binding("ctrl-l", ROOT_KEY_CONTEXT),
    binding("cmd-l", ROOT_KEY_CONTEXT),
    binding("ctrl-enter", ROOT_KEY_CONTEXT),
    binding("cmd-enter", ROOT_KEY_CONTEXT),
    binding("ctrl-escape", ROOT_KEY_CONTEXT),
    binding("cmd-escape", ROOT_KEY_CONTEXT),
    binding("escape", ROOT_KEY_CONTEXT),
    binding("end", ROOT_KEY_CONTEXT),
    binding("ctrl-\\", ROOT_KEY_CONTEXT),
    binding("cmd-\\", ROOT_KEY_CONTEXT),
    binding("ctrl-tab", ROOT_KEY_CONTEXT),
    binding("ctrl-shift-tab", ROOT_KEY_CONTEXT),
    binding("cmd-tab", ROOT_KEY_CONTEXT),
    binding("cmd-shift-tab", ROOT_KEY_CONTEXT),
];

#[cfg(test)]
const fn binding(keystroke: &'static str, context: &'static str) -> BindingSpec {
    BindingSpec { keystroke, context }
}

pub(crate) fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("ctrl-k", OpenCommandPalette, Some(ROOT_KEY_CONTEXT)),
        KeyBinding::new("cmd-k", OpenCommandPalette, Some(ROOT_KEY_CONTEXT)),
        KeyBinding::new("ctrl-p", OpenFileSurface, Some(ROOT_KEY_CONTEXT)),
        KeyBinding::new("cmd-p", OpenFileSurface, Some(ROOT_KEY_CONTEXT)),
        KeyBinding::new("ctrl-n", NewSession, Some(ROOT_KEY_CONTEXT)),
        KeyBinding::new("cmd-n", NewSession, Some(ROOT_KEY_CONTEXT)),
        KeyBinding::new("ctrl-l", FocusComposer, Some(ROOT_KEY_CONTEXT)),
        KeyBinding::new("cmd-l", FocusComposer, Some(ROOT_KEY_CONTEXT)),
        KeyBinding::new("ctrl-enter", SubmitComposer, Some(ROOT_KEY_CONTEXT)),
        KeyBinding::new("cmd-enter", SubmitComposer, Some(ROOT_KEY_CONTEXT)),
        KeyBinding::new("ctrl-escape", AbortActiveOperation, Some(ROOT_KEY_CONTEXT)),
        KeyBinding::new("cmd-escape", AbortActiveOperation, Some(ROOT_KEY_CONTEXT)),
        KeyBinding::new("escape", EscapeHierarchy, Some(ROOT_KEY_CONTEXT)),
        KeyBinding::new("end", FollowLatestOutput, Some(ROOT_KEY_CONTEXT)),
        KeyBinding::new("ctrl-\\", ToggleContextPanel, Some(ROOT_KEY_CONTEXT)),
        KeyBinding::new("cmd-\\", ToggleContextPanel, Some(ROOT_KEY_CONTEXT)),
        KeyBinding::new("ctrl-tab", FocusNextRegion, Some(ROOT_KEY_CONTEXT)),
        KeyBinding::new(
            "ctrl-shift-tab",
            FocusPreviousRegion,
            Some(ROOT_KEY_CONTEXT),
        ),
        KeyBinding::new("cmd-tab", FocusNextRegion, Some(ROOT_KEY_CONTEXT)),
        KeyBinding::new("cmd-shift-tab", FocusPreviousRegion, Some(ROOT_KEY_CONTEXT)),
        KeyBinding::new(
            "up",
            SelectPreviousConversation,
            Some(CONVERSATION_KEY_CONTEXT),
        ),
        KeyBinding::new(
            "down",
            SelectNextConversation,
            Some(CONVERSATION_KEY_CONTEXT),
        ),
        KeyBinding::new(
            "ctrl-c",
            CopySelectedConversation,
            Some(CONVERSATION_KEY_CONTEXT),
        ),
        KeyBinding::new(
            "cmd-c",
            CopySelectedConversation,
            Some(CONVERSATION_KEY_CONTEXT),
        ),
        KeyBinding::new(
            "space",
            ToggleSelectedConversationDetails,
            Some(CONVERSATION_KEY_CONTEXT),
        ),
        KeyBinding::new("up", PalettePrevious, Some(PALETTE_KEY_CONTEXT)),
        KeyBinding::new("shift-tab", PalettePrevious, Some(PALETTE_KEY_CONTEXT)),
        KeyBinding::new("down", PaletteNext, Some(PALETTE_KEY_CONTEXT)),
        KeyBinding::new("tab", PaletteNext, Some(PALETTE_KEY_CONTEXT)),
        KeyBinding::new("enter", PaletteConfirm, Some(PALETTE_KEY_CONTEXT)),
        KeyBinding::new("escape", EscapeHierarchy, Some(PALETTE_KEY_CONTEXT)),
        KeyBinding::new("1", AuthorizationDeny, Some(AUTHORIZATION_KEY_CONTEXT)),
        KeyBinding::new("2", AuthorizationAllowOnce, Some(AUTHORIZATION_KEY_CONTEXT)),
        KeyBinding::new(
            "3",
            AuthorizationAllowForOperation,
            Some(AUTHORIZATION_KEY_CONTEXT),
        ),
        KeyBinding::new("tab", TrapOverlayFocus, Some(AUTHORIZATION_KEY_CONTEXT)),
        KeyBinding::new(
            "shift-tab",
            TrapOverlayFocus,
            Some(AUTHORIZATION_KEY_CONTEXT),
        ),
        KeyBinding::new("escape", EscapeHierarchy, Some(AUTHORIZATION_KEY_CONTEXT)),
        KeyBinding::new("tab", TrapOverlayFocus, Some(NARROW_SESSIONS_KEY_CONTEXT)),
        KeyBinding::new(
            "shift-tab",
            TrapOverlayFocus,
            Some(NARROW_SESSIONS_KEY_CONTEXT),
        ),
        KeyBinding::new("escape", EscapeHierarchy, Some(NARROW_SESSIONS_KEY_CONTEXT)),
        KeyBinding::new("tab", TrapOverlayFocus, Some(NARROW_CONTEXT_KEY_CONTEXT)),
        KeyBinding::new(
            "shift-tab",
            TrapOverlayFocus,
            Some(NARROW_CONTEXT_KEY_CONTEXT),
        ),
        KeyBinding::new("escape", EscapeHierarchy, Some(NARROW_CONTEXT_KEY_CONTEXT)),
    ]);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn palette_routes_unique_typed_commands_with_semantic_labels() {
        let commands = PALETTE_ENTRIES
            .iter()
            .map(|entry| entry.command)
            .collect::<HashSet<_>>();
        assert_eq!(commands.len(), PALETTE_ENTRIES.len());
        assert!(PALETTE_ENTRIES.iter().all(|entry| {
            !entry.label.trim().is_empty() && !entry.semantic_label.trim().is_empty()
        }));
    }

    #[test]
    fn palette_selection_wraps_and_resets_on_open() {
        let mut palette = DesktopCommandPalette::default();
        palette.open();
        assert_eq!(palette.selected(), 0);
        assert_eq!(
            palette.selected_command(),
            Some(DesktopPaletteCommand::NewSession)
        );

        palette.move_selection(true);
        assert_eq!(palette.selected(), PALETTE_ENTRIES.len() - 1);
        palette.move_selection(false);
        assert_eq!(palette.selected(), 0);

        palette.close();
        assert_eq!(palette.selected_command(), None);
        palette.open();
        assert_eq!(palette.selected(), 0);
    }

    #[test]
    fn root_key_bindings_have_no_contextual_conflicts() {
        let unique = ROOT_BINDINGS
            .iter()
            .map(|binding| (binding.keystroke, binding.context))
            .collect::<HashSet<_>>();
        assert_eq!(unique.len(), ROOT_BINDINGS.len());
    }

    #[test]
    fn platform_equivalent_shortcuts_are_paired() {
        let bindings = ROOT_BINDINGS
            .iter()
            .map(|binding| binding.keystroke)
            .collect::<HashSet<_>>();
        for key in ["k", "p", "n", "l", "enter", "escape", "\\"] {
            assert!(bindings.contains(format!("ctrl-{key}").as_str()));
            assert!(bindings.contains(format!("cmd-{key}").as_str()));
        }
        assert!(bindings.contains("ctrl-tab"));
        assert!(bindings.contains("ctrl-shift-tab"));
        assert!(bindings.contains("cmd-tab"));
        assert!(bindings.contains("cmd-shift-tab"));
        assert!(!bindings.contains("tab"));
        assert!(!bindings.contains("shift-tab"));
    }
}
