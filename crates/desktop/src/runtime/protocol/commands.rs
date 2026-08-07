//! Typed desktop runtime commands, targets, and kind labels.

use std::fmt;
use std::path::PathBuf;

use coding_agent::api::authorization::{ToolAuthorizationDecision, ToolAuthorizationIdentity};
use coding_agent::api::embedding::{CodingAgentThinkingLevel, CodingAgentWorkspaceSelection};
use coding_agent::api::event::CodingAgentRecoveryResolution;
use coding_agent::api::review::{CodingAgentExternalEditorTarget, CodingAgentFileReviewRequest};

use super::DesktopRecoveryIdentity;

pub(in crate::runtime) enum DesktopRuntimeCommand {
    Reload {
        command_id: u64,
        target: DesktopRuntimeOwnerTarget,
    },
    Resync {
        command_id: u64,
        session_id: Option<String>,
    },
    CreateSession {
        command_id: u64,
    },
    OpenSession {
        command_id: u64,
        session_id: String,
    },
    CloseSession {
        command_id: u64,
        session_id: String,
    },
    DeleteSession {
        command_id: u64,
        session_id: String,
    },
    ListSessions {
        command_id: u64,
    },
    RenameSession {
        command_id: u64,
        session_id: String,
        name: Option<String>,
    },
    SelectModel {
        command_id: u64,
        target: DesktopRuntimeOwnerTarget,
        model_id: String,
        thinking_level: Option<CodingAgentThinkingLevel>,
    },
    SelectSessionProfile {
        command_id: u64,
        target: DesktopRuntimeOwnerTarget,
        profile_id: String,
    },
    SubmitPrompt {
        command_id: u64,
        target: DesktopPromptTarget,
        prompt: String,
        attachments: Vec<PathBuf>,
        thinking_level: Option<CodingAgentThinkingLevel>,
    },
    Abort {
        command_id: u64,
        session_id: Option<String>,
    },
    Steer {
        command_id: u64,
        session_id: Option<String>,
        text: String,
    },
    FollowUp {
        command_id: u64,
        session_id: Option<String>,
        text: String,
    },
    DecideToolAuthorization {
        command_id: u64,
        session_id: Option<String>,
        identity: ToolAuthorizationIdentity,
        decision: ToolAuthorizationDecision,
    },
    RetryRecovery {
        command_id: u64,
        session_id: Option<String>,
        identity: DesktopRecoveryIdentity,
    },
    ResolveRecovery {
        command_id: u64,
        session_id: Option<String>,
        identity: DesktopRecoveryIdentity,
        resolution: CodingAgentRecoveryResolution,
    },
    OpenChange {
        command_id: u64,
        session_id: String,
        request: CodingAgentFileReviewRequest,
    },
    OpenExternalEditor {
        command_id: u64,
        session_id: String,
        target: CodingAgentExternalEditorTarget,
    },
    ListMergeProposals {
        command_id: u64,
        session_id: String,
    },
    MergeChildWorktree {
        command_id: u64,
        session_id: String,
        worktree_id: String,
    },
    DiscardChildWorktree {
        command_id: u64,
        session_id: String,
        worktree_id: String,
    },
}

/// Explicit owner for context-level desktop commands.
#[derive(Clone, PartialEq, Eq)]
pub enum DesktopRuntimeOwnerTarget {
    Home,
    Session { session_id: String },
}

impl DesktopRuntimeOwnerTarget {
    pub const fn home() -> Self {
        Self::Home
    }

    pub fn session(session_id: impl Into<String>) -> Self {
        Self::Session {
            session_id: session_id.into(),
        }
    }

    pub fn session_id(&self) -> Option<&str> {
        match self {
            Self::Home => None,
            Self::Session { session_id } => Some(session_id),
        }
    }
}

impl fmt::Debug for DesktopRuntimeOwnerTarget {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DesktopRuntimeOwnerTarget")
            .field(
                "kind",
                &match self {
                    Self::Home => "home",
                    Self::Session { .. } => "session",
                },
            )
            .finish_non_exhaustive()
    }
}

/// Explicit target for a desktop prompt submission.
///
/// New sessions must carry every Home selection needed to construct their
/// future per-workspace runtime owner. Existing sessions carry only durable
/// identity, so a cwd or workspace override cannot be represented.
#[derive(Clone, PartialEq, Eq)]
pub enum DesktopPromptTarget {
    New {
        workspace: CodingAgentWorkspaceSelection,
        model_id: String,
        profile_id: String,
    },
    Existing {
        session_id: String,
    },
}

impl DesktopPromptTarget {
    pub fn new(
        workspace: CodingAgentWorkspaceSelection,
        model_id: impl Into<String>,
        profile_id: impl Into<String>,
    ) -> Self {
        Self::New {
            workspace,
            model_id: model_id.into(),
            profile_id: profile_id.into(),
        }
    }

    pub fn existing(session_id: impl Into<String>) -> Self {
        Self::Existing {
            session_id: session_id.into(),
        }
    }

    pub fn existing_session_id(&self) -> Option<&str> {
        match self {
            Self::New { .. } => None,
            Self::Existing { session_id } => Some(session_id),
        }
    }

    const fn kind_label(&self) -> &'static str {
        match self {
            Self::New { .. } => "new",
            Self::Existing { .. } => "existing",
        }
    }
}

impl fmt::Debug for DesktopPromptTarget {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DesktopPromptTarget")
            .field("kind", &self.kind_label())
            .finish_non_exhaustive()
    }
}

impl fmt::Debug for DesktopRuntimeCommand {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut debug = formatter.debug_struct("DesktopRuntimeCommand");
        debug
            .field("kind", &self.kind())
            .field("command_id", &self.command_id());
        if let Self::SubmitPrompt { target, .. } = self {
            debug.field("prompt_target", &target.kind_label());
        }
        debug.finish_non_exhaustive()
    }
}

impl DesktopRuntimeCommand {
    pub(in crate::runtime) const fn command_id(&self) -> u64 {
        match self {
            Self::Reload { command_id, .. }
            | Self::Resync { command_id, .. }
            | Self::CreateSession { command_id }
            | Self::OpenSession { command_id, .. }
            | Self::CloseSession { command_id, .. }
            | Self::DeleteSession { command_id, .. }
            | Self::ListSessions { command_id }
            | Self::RenameSession { command_id, .. }
            | Self::SelectModel { command_id, .. }
            | Self::SelectSessionProfile { command_id, .. }
            | Self::SubmitPrompt { command_id, .. }
            | Self::Abort { command_id, .. }
            | Self::Steer { command_id, .. }
            | Self::FollowUp { command_id, .. }
            | Self::DecideToolAuthorization { command_id, .. }
            | Self::RetryRecovery { command_id, .. }
            | Self::ResolveRecovery { command_id, .. }
            | Self::OpenChange { command_id, .. }
            | Self::OpenExternalEditor { command_id, .. }
            | Self::ListMergeProposals { command_id, .. }
            | Self::MergeChildWorktree { command_id, .. }
            | Self::DiscardChildWorktree { command_id, .. } => *command_id,
        }
    }

    pub(in crate::runtime) const fn kind(&self) -> DesktopRuntimeCommandKind {
        match self {
            Self::Reload { .. } => DesktopRuntimeCommandKind::Reload,
            Self::Resync { .. } => DesktopRuntimeCommandKind::Resync,
            Self::CreateSession { .. } => DesktopRuntimeCommandKind::CreateSession,
            Self::OpenSession { .. } => DesktopRuntimeCommandKind::OpenSession,
            Self::CloseSession { .. } => DesktopRuntimeCommandKind::CloseSession,
            Self::DeleteSession { .. } => DesktopRuntimeCommandKind::DeleteSession,
            Self::ListSessions { .. } => DesktopRuntimeCommandKind::ListSessions,
            Self::RenameSession { .. } => DesktopRuntimeCommandKind::RenameSession,
            Self::SelectModel { .. } => DesktopRuntimeCommandKind::SelectModel,
            Self::SelectSessionProfile { .. } => DesktopRuntimeCommandKind::SelectSessionProfile,
            Self::SubmitPrompt { .. } => DesktopRuntimeCommandKind::SubmitPrompt,
            Self::Abort { .. } => DesktopRuntimeCommandKind::Abort,
            Self::Steer { .. } => DesktopRuntimeCommandKind::Steer,
            Self::FollowUp { .. } => DesktopRuntimeCommandKind::FollowUp,
            Self::DecideToolAuthorization { .. } => {
                DesktopRuntimeCommandKind::DecideToolAuthorization
            }
            Self::RetryRecovery { .. } => DesktopRuntimeCommandKind::RetryRecovery,
            Self::ResolveRecovery { .. } => DesktopRuntimeCommandKind::ResolveRecovery,
            Self::OpenChange { .. } => DesktopRuntimeCommandKind::OpenChange,
            Self::OpenExternalEditor { .. } => DesktopRuntimeCommandKind::OpenExternalEditor,
            Self::ListMergeProposals { .. } => DesktopRuntimeCommandKind::ListMergeProposals,
            Self::MergeChildWorktree { .. } => DesktopRuntimeCommandKind::MergeChildWorktree,
            Self::DiscardChildWorktree { .. } => DesktopRuntimeCommandKind::DiscardChildWorktree,
        }
    }

    pub(in crate::runtime) fn target_session_id(&self) -> Option<&str> {
        match self {
            Self::OpenSession { session_id, .. }
            | Self::CloseSession { session_id, .. }
            | Self::DeleteSession { session_id, .. }
            | Self::RenameSession { session_id, .. } => Some(session_id),
            Self::Reload { target, .. }
            | Self::SelectModel { target, .. }
            | Self::SelectSessionProfile { target, .. } => target.session_id(),
            Self::SubmitPrompt { target, .. } => target.existing_session_id(),
            Self::OpenChange { session_id, .. }
            | Self::OpenExternalEditor { session_id, .. }
            | Self::ListMergeProposals { session_id, .. }
            | Self::MergeChildWorktree { session_id, .. }
            | Self::DiscardChildWorktree { session_id, .. } => Some(session_id),
            Self::Resync { session_id, .. }
            | Self::Abort { session_id, .. }
            | Self::Steer { session_id, .. }
            | Self::FollowUp { session_id, .. }
            | Self::DecideToolAuthorization { session_id, .. }
            | Self::RetryRecovery { session_id, .. }
            | Self::ResolveRecovery { session_id, .. } => session_id.as_deref(),
            Self::CreateSession { .. } | Self::ListSessions { .. } => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DesktopRuntimeCommandKind {
    Reload,
    Resync,
    CreateSession,
    OpenSession,
    CloseSession,
    DeleteSession,
    ListSessions,
    RenameSession,
    SelectModel,
    SelectSessionProfile,
    SubmitPrompt,
    Abort,
    Steer,
    FollowUp,
    DecideToolAuthorization,
    RetryRecovery,
    ResolveRecovery,
    OpenChange,
    OpenExternalEditor,
    ListMergeProposals,
    MergeChildWorktree,
    DiscardChildWorktree,
}
