use coding_agent::api::authorization::{ToolAuthorizationDecision, ToolAuthorizationIdentity};
use coding_agent::api::review::CodingAgentFileReviewRequest;
use desktop::preferences::DesktopThinkingLevel;
use desktop::runtime::{DesktopRecoveryAction, DesktopRecoveryIdentity};
use desktop::ui::conversation::ConversationRowMeasurement;
use std::sync::Arc;

use super::{
    ComposerRunningMode, DesktopPaletteCommand, InspectorSection,
    center_drawer_host::CenterDrawerHostEvent, composer_pane::ComposerPaneEvent,
    conversation_header::ConversationHeaderEvent, conversation_pane::ConversationPaneEvent,
    inspector_pane::InspectorPaneEvent, root_modal_host::RootModalHostEvent,
    sessions_pane::SessionsPaneEvent,
};
use crate::ui::shell::CenterNavigationTarget;

/// One typed adapter boundary for every child-view event.
///
/// Child subscriptions only normalize their feature event into this enum.
/// Mutation, command admission, focus, and platform effects are dispatched by
/// the shell's single intent handler.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum UiIntent {
    Navigate(CenterNavigationTarget),
    RefreshSessions,
    SetProjectCollapsed {
        group_id: String,
        collapsed: bool,
    },
    RenameSession {
        session_id: String,
        name: String,
    },
    CloseSession(String),
    DismissDrawer,
    ToggleSessions,
    ToggleInspector,
    Reload,
    SelectModel(Arc<str>),
    SelectSessionProfile(Arc<str>),
    SelectThinking(DesktopThinkingLevel),
    Abort,
    ComposerInputChanged(String),
    ComposerFocused,
    AddAttachments,
    RemoveAttachment(usize),
    ChooseProjectDirectory,
    ClearProjectDirectory,
    SubmitPrimary,
    Submit,
    SubmitRunning,
    SetRunningMode(ComposerRunningMode),
    SelectConversation {
        block_id: String,
        durable: bool,
    },
    ConversationScrolled,
    CopyConversation(String),
    CopyToolDetails(String),
    CopyCodeCompleted,
    ToggleConversationDetails(String),
    OpenFullConversation(String),
    Recovery {
        identity: DesktopRecoveryIdentity,
        action: DesktopRecoveryAction,
    },
    ConversationMeasured(ConversationRowMeasurement),
    FollowLatest,
    RequestFileReview(CodingAgentFileReviewRequest),
    CopyReviewPath,
    CopyFileReview,
    OpenExternalEditor,
    SelectInspectorSection(InspectorSection),
    ExecutePalette(DesktopPaletteCommand),
    DecideAuthorization {
        identity: ToolAuthorizationIdentity,
        decision: ToolAuthorizationDecision,
    },
    CopyFullMessage,
    CloseFullMessage,
}

impl From<&ConversationPaneEvent> for UiIntent {
    fn from(event: &ConversationPaneEvent) -> Self {
        match event {
            ConversationPaneEvent::Select { block_id, durable } => Self::SelectConversation {
                block_id: block_id.clone(),
                durable: *durable,
            },
            ConversationPaneEvent::Scrolled => Self::ConversationScrolled,
            ConversationPaneEvent::Copy { block_id } => Self::CopyConversation(block_id.clone()),
            ConversationPaneEvent::CopyToolDetails { block_id } => {
                Self::CopyToolDetails(block_id.clone())
            }
            ConversationPaneEvent::CopyCodeCompleted => Self::CopyCodeCompleted,
            ConversationPaneEvent::ToggleDetails { block_id } => {
                Self::ToggleConversationDetails(block_id.clone())
            }
            ConversationPaneEvent::OpenFull { block_id } => {
                Self::OpenFullConversation(block_id.clone())
            }
            ConversationPaneEvent::Recovery { identity, action } => Self::Recovery {
                identity: identity.clone(),
                action: *action,
            },
            ConversationPaneEvent::Measured(measurement) => {
                Self::ConversationMeasured(measurement.clone())
            }
            ConversationPaneEvent::FollowLatest => Self::FollowLatest,
        }
    }
}

impl From<&ConversationHeaderEvent> for UiIntent {
    fn from(event: &ConversationHeaderEvent) -> Self {
        match event {
            ConversationHeaderEvent::ToggleSessions => Self::ToggleSessions,
            ConversationHeaderEvent::ToggleInspector => Self::ToggleInspector,
            ConversationHeaderEvent::Reload => Self::Reload,
            ConversationHeaderEvent::SelectModel(model_id) => {
                Self::SelectModel(Arc::clone(model_id))
            }
            ConversationHeaderEvent::SelectSessionProfile(profile_id) => {
                Self::SelectSessionProfile(Arc::clone(profile_id))
            }
            ConversationHeaderEvent::SelectThinking(level) => Self::SelectThinking(*level),
            ConversationHeaderEvent::Abort => Self::Abort,
        }
    }
}

impl From<&SessionsPaneEvent> for UiIntent {
    fn from(event: &SessionsPaneEvent) -> Self {
        match event {
            SessionsPaneEvent::Navigate(target) => Self::Navigate(target.clone()),
            SessionsPaneEvent::Refresh => Self::RefreshSessions,
            SessionsPaneEvent::SetProjectCollapsed {
                group_id,
                collapsed,
            } => Self::SetProjectCollapsed {
                group_id: group_id.clone(),
                collapsed: *collapsed,
            },
            SessionsPaneEvent::Rename(session_id, name) => Self::RenameSession {
                session_id: session_id.clone(),
                name: name.clone(),
            },
            SessionsPaneEvent::CloseSession(session_id) => Self::CloseSession(session_id.clone()),
            SessionsPaneEvent::Dismiss => Self::DismissDrawer,
        }
    }
}

impl From<&ComposerPaneEvent> for UiIntent {
    fn from(event: &ComposerPaneEvent) -> Self {
        match event {
            ComposerPaneEvent::InputChanged(value) => Self::ComposerInputChanged(value.clone()),
            ComposerPaneEvent::Focused => Self::ComposerFocused,
            ComposerPaneEvent::AddAttachments => Self::AddAttachments,
            ComposerPaneEvent::RemoveAttachment(index) => Self::RemoveAttachment(*index),
            ComposerPaneEvent::ChooseProjectDirectory => Self::ChooseProjectDirectory,
            ComposerPaneEvent::ClearProjectDirectory => Self::ClearProjectDirectory,
            ComposerPaneEvent::SubmitPrimary => Self::SubmitPrimary,
            ComposerPaneEvent::Submit => Self::Submit,
            ComposerPaneEvent::SubmitRunning => Self::SubmitRunning,
            ComposerPaneEvent::SetRunningMode(mode) => Self::SetRunningMode(*mode),
        }
    }
}

impl From<&InspectorPaneEvent> for UiIntent {
    fn from(event: &InspectorPaneEvent) -> Self {
        match event {
            InspectorPaneEvent::Close => Self::DismissDrawer,
            InspectorPaneEvent::RequestFileReview(request) => {
                Self::RequestFileReview(request.clone())
            }
            InspectorPaneEvent::CopyReviewPath => Self::CopyReviewPath,
            InspectorPaneEvent::CopyFileReview => Self::CopyFileReview,
            InspectorPaneEvent::OpenExternalEditor => Self::OpenExternalEditor,
            InspectorPaneEvent::Recovery { identity, action } => Self::Recovery {
                identity: identity.clone(),
                action: *action,
            },
            InspectorPaneEvent::SelectSection(section) => Self::SelectInspectorSection(*section),
        }
    }
}

impl From<&RootModalHostEvent> for UiIntent {
    fn from(event: &RootModalHostEvent) -> Self {
        match event {
            RootModalHostEvent::ExecutePalette(command) => Self::ExecutePalette(*command),
            RootModalHostEvent::DecideAuthorization { identity, decision } => {
                Self::DecideAuthorization {
                    identity: identity.clone(),
                    decision: decision.clone(),
                }
            }
            RootModalHostEvent::CopyFullMessage => Self::CopyFullMessage,
            RootModalHostEvent::CloseFullMessage => Self::CloseFullMessage,
        }
    }
}

impl From<&CenterDrawerHostEvent> for UiIntent {
    fn from(_: &CenterDrawerHostEvent) -> Self {
        Self::DismissDrawer
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn feature_events_preserve_their_typed_payloads() {
        assert_eq!(
            UiIntent::from(&SessionsPaneEvent::Rename(
                "session-a".into(),
                "Readable name".into(),
            )),
            UiIntent::RenameSession {
                session_id: "session-a".into(),
                name: "Readable name".into(),
            }
        );
        assert_eq!(
            UiIntent::from(&ConversationHeaderEvent::SelectModel(Arc::from("model-a"))),
            UiIntent::SelectModel(Arc::from("model-a"))
        );
        assert_eq!(
            UiIntent::from(&ComposerPaneEvent::SetRunningMode(
                ComposerRunningMode::QueueNext,
            )),
            UiIntent::SetRunningMode(ComposerRunningMode::QueueNext)
        );
        assert_eq!(
            UiIntent::from(&ConversationPaneEvent::Select {
                block_id: "message-a".into(),
                durable: true,
            }),
            UiIntent::SelectConversation {
                block_id: "message-a".into(),
                durable: true,
            }
        );
        assert_eq!(
            UiIntent::from(&InspectorPaneEvent::SelectSection(
                InspectorSection::Runtime
            )),
            UiIntent::SelectInspectorSection(InspectorSection::Runtime)
        );
    }

    #[test]
    fn overlay_events_normalize_to_shared_intents() {
        assert_eq!(
            UiIntent::from(&SessionsPaneEvent::Dismiss),
            UiIntent::DismissDrawer
        );
        assert_eq!(
            UiIntent::from(&InspectorPaneEvent::Close),
            UiIntent::DismissDrawer
        );
        assert_eq!(
            UiIntent::from(&CenterDrawerHostEvent::Dismiss),
            UiIntent::DismissDrawer
        );
        assert_eq!(
            UiIntent::from(&RootModalHostEvent::CopyFullMessage),
            UiIntent::CopyFullMessage
        );
    }
}
