use std::collections::HashMap;
use std::fmt;

use coding_agent::api::review::CodingAgentFileReviewRequest;
use desktop::runtime::{
    DesktopRecoveryAction, DesktopRuntimeCommandKind, DesktopRuntimeSelectionKind,
};

use super::workspace::WorkspaceKey;

pub(crate) const MAX_PENDING_DESKTOP_COMMANDS: usize = 32;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DesktopCommandIntent {
    Prompt,
    Steer,
    FollowUp,
    Resync,
    CreateSession,
    OpenSession {
        session_id: String,
    },
    CloseSession {
        session_id: String,
    },
    ListSessions,
    RenameSession {
        session_id: String,
    },
    Abort {
        operation_id: String,
    },
    Reload,
    Selection(DesktopRuntimeSelectionKind),
    Authorization {
        authorization_id: String,
        operation_id: String,
    },
    Recovery {
        recovery_id: String,
        action: DesktopRecoveryAction,
    },
    FileReview {
        request: CodingAgentFileReviewRequest,
    },
    ExternalEditor {
        project_relative_path: String,
    },
}

impl DesktopCommandIntent {
    pub(crate) const fn command_kind(&self) -> DesktopRuntimeCommandKind {
        match self {
            Self::Prompt => DesktopRuntimeCommandKind::SubmitPrompt,
            Self::Steer => DesktopRuntimeCommandKind::Steer,
            Self::FollowUp => DesktopRuntimeCommandKind::FollowUp,
            Self::Resync => DesktopRuntimeCommandKind::Resync,
            Self::CreateSession => DesktopRuntimeCommandKind::CreateSession,
            Self::OpenSession { .. } => DesktopRuntimeCommandKind::OpenSession,
            Self::CloseSession { .. } => DesktopRuntimeCommandKind::CloseSession,
            Self::ListSessions => DesktopRuntimeCommandKind::ListSessions,
            Self::RenameSession { .. } => DesktopRuntimeCommandKind::RenameSession,
            Self::Abort { .. } => DesktopRuntimeCommandKind::Abort,
            Self::Reload => DesktopRuntimeCommandKind::Reload,
            Self::Selection(DesktopRuntimeSelectionKind::Model) => {
                DesktopRuntimeCommandKind::SelectModel
            }
            Self::Selection(DesktopRuntimeSelectionKind::SessionProfile) => {
                DesktopRuntimeCommandKind::SelectSessionProfile
            }
            Self::Authorization { .. } => DesktopRuntimeCommandKind::DecideToolAuthorization,
            Self::Recovery {
                action: DesktopRecoveryAction::Retry,
                ..
            } => DesktopRuntimeCommandKind::RetryRecovery,
            Self::Recovery {
                action: DesktopRecoveryAction::MarkFailed | DesktopRecoveryAction::Abort,
                ..
            } => DesktopRuntimeCommandKind::ResolveRecovery,
            Self::FileReview { .. } => DesktopRuntimeCommandKind::ReviewChangedFile,
            Self::ExternalEditor { .. } => DesktopRuntimeCommandKind::OpenExternalEditor,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PendingCommand {
    owner: WorkspaceKey,
    intent: DesktopCommandIntent,
}

impl PendingCommand {
    pub(crate) const fn owner(&self) -> &WorkspaceKey {
        &self.owner
    }

    pub(crate) const fn intent(&self) -> &DesktopCommandIntent {
        &self.intent
    }

    pub(crate) fn into_intent(self) -> DesktopCommandIntent {
        self.intent
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CommandAdmissionError {
    Full,
    IdExhausted,
}

impl fmt::Display for CommandAdmissionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Full => formatter.write_str("desktop command tracker is full"),
            Self::IdExhausted => formatter.write_str("desktop command IDs are exhausted"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CommandCompletionError {
    UnknownCommand,
    IntentMismatch,
    OwnerMismatch,
}

pub(crate) struct CommandTracker {
    next_id: u64,
    pending: HashMap<u64, PendingCommand>,
    capacity: usize,
}

impl Default for CommandTracker {
    fn default() -> Self {
        Self {
            next_id: 1,
            pending: HashMap::with_capacity(MAX_PENDING_DESKTOP_COMMANDS),
            capacity: MAX_PENDING_DESKTOP_COMMANDS,
        }
    }
}

impl CommandTracker {
    pub(crate) fn reserve(
        &mut self,
        owner: WorkspaceKey,
        intent: DesktopCommandIntent,
    ) -> Result<u64, CommandAdmissionError> {
        if self.pending.len() >= self.capacity {
            return Err(CommandAdmissionError::Full);
        }
        let command_id = self.next_id;
        self.next_id = command_id
            .checked_add(1)
            .ok_or(CommandAdmissionError::IdExhausted)?;
        let replaced = self
            .pending
            .insert(command_id, PendingCommand { owner, intent });
        debug_assert!(replaced.is_none(), "command IDs must be globally unique");
        Ok(command_id)
    }

    pub(crate) fn pending(&self, command_id: u64) -> Option<&PendingCommand> {
        self.pending.get(&command_id)
    }

    pub(crate) fn owner(&self, command_id: u64) -> Option<&WorkspaceKey> {
        self.pending(command_id).map(PendingCommand::owner)
    }

    pub(crate) fn intent(&self, command_id: u64) -> Option<&DesktopCommandIntent> {
        self.pending(command_id).map(PendingCommand::intent)
    }

    pub(crate) fn contains(&self, owner: &WorkspaceKey, intent: &DesktopCommandIntent) -> bool {
        self.pending
            .values()
            .any(|pending| pending.owner() == owner && pending.intent() == intent)
    }

    pub(crate) fn contains_where(
        &self,
        owner: &WorkspaceKey,
        predicate: impl Fn(&DesktopCommandIntent) -> bool,
    ) -> bool {
        self.pending
            .values()
            .any(|pending| pending.owner() == owner && predicate(pending.intent()))
    }

    pub(crate) fn contains_anywhere(
        &self,
        predicate: impl Fn(&DesktopCommandIntent) -> bool,
    ) -> bool {
        self.pending
            .values()
            .any(|pending| predicate(pending.intent()))
    }

    pub(crate) fn matches(
        &self,
        command_id: u64,
        owner: &WorkspaceKey,
        intent: &DesktopCommandIntent,
    ) -> bool {
        self.pending(command_id)
            .is_some_and(|pending| pending.owner() == owner && pending.intent() == intent)
    }

    pub(crate) fn complete(
        &mut self,
        command_id: u64,
        observed_owner: &WorkspaceKey,
        expected_intent: &DesktopCommandIntent,
    ) -> Result<PendingCommand, CommandCompletionError> {
        let pending = self
            .pending(command_id)
            .ok_or(CommandCompletionError::UnknownCommand)?;
        if pending.intent() != expected_intent {
            return Err(CommandCompletionError::IntentMismatch);
        }
        if pending.owner() != observed_owner {
            return Err(CommandCompletionError::OwnerMismatch);
        }
        Ok(self
            .pending
            .remove(&command_id)
            .expect("validated pending command must still exist"))
    }

    pub(crate) fn reject(
        &mut self,
        command_id: u64,
        observed_owner: &WorkspaceKey,
        command: DesktopRuntimeCommandKind,
    ) -> Result<PendingCommand, CommandCompletionError> {
        let intent = self
            .intent(command_id)
            .filter(|intent| intent.command_kind() == command)
            .cloned()
            .ok_or_else(|| {
                if self.pending(command_id).is_some() {
                    CommandCompletionError::IntentMismatch
                } else {
                    CommandCompletionError::UnknownCommand
                }
            })?;
        self.complete(command_id, observed_owner, &intent)
    }

    pub(crate) fn find(
        &self,
        owner: &WorkspaceKey,
        predicate: impl Fn(&DesktopCommandIntent) -> bool,
    ) -> Option<(u64, DesktopCommandIntent)> {
        self.pending.iter().find_map(|(command_id, pending)| {
            (pending.owner() == owner && predicate(pending.intent()))
                .then(|| (*command_id, pending.intent().clone()))
        })
    }

    pub(crate) fn authorization(&self, owner: &WorkspaceKey) -> Option<(u64, &str, &str)> {
        self.pending.iter().find_map(|(command_id, pending)| {
            if pending.owner() != owner {
                return None;
            }
            match pending.intent() {
                DesktopCommandIntent::Authorization {
                    authorization_id,
                    operation_id,
                } => Some((
                    *command_id,
                    authorization_id.as_str(),
                    operation_id.as_str(),
                )),
                _ => None,
            }
        })
    }

    pub(crate) fn cancel_owner(&mut self, owner: &WorkspaceKey) -> Vec<PendingCommand> {
        let command_ids = self
            .pending
            .iter()
            .filter_map(|(command_id, pending)| (pending.owner() == owner).then_some(*command_id))
            .collect::<Vec<_>>();
        command_ids
            .into_iter()
            .filter_map(|command_id| self.pending.remove(&command_id))
            .collect()
    }

    pub(crate) fn transfer_owner(&mut self, from: &WorkspaceKey, to: &WorkspaceKey) -> usize {
        let mut transferred = 0;
        for pending in self.pending.values_mut() {
            if pending.owner() == from {
                pending.owner = to.clone();
                transferred += 1;
            }
        }
        transferred
    }

    pub(crate) fn transfer_command(
        &mut self,
        command_id: u64,
        to: WorkspaceKey,
    ) -> Result<(), CommandCompletionError> {
        let pending = self
            .pending
            .get_mut(&command_id)
            .ok_or(CommandCompletionError::UnknownCommand)?;
        pending.owner = to;
        Ok(())
    }

    pub(crate) fn cancel_all(&mut self) -> Vec<PendingCommand> {
        self.pending.drain().map(|(_, pending)| pending).collect()
    }

    #[cfg(test)]
    fn with_limits(next_id: u64, capacity: usize) -> Self {
        Self {
            next_id,
            pending: HashMap::with_capacity(capacity),
            capacity,
        }
    }

    #[cfg(test)]
    pub(crate) fn command_id_for(
        &self,
        owner: &WorkspaceKey,
        intent: &DesktopCommandIntent,
    ) -> Option<u64> {
        self.find(owner, |pending| pending == intent)
            .map(|(command_id, _)| command_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn home() -> WorkspaceKey {
        WorkspaceKey::Home
    }

    fn session() -> WorkspaceKey {
        WorkspaceKey::session("session-a")
    }

    #[test]
    fn pressure_is_globally_bounded_and_full_reservations_do_not_consume_ids() {
        let mut tracker = CommandTracker::with_limits(7, 2);
        assert_eq!(tracker.reserve(home(), DesktopCommandIntent::Reload), Ok(7));
        assert_eq!(
            tracker.reserve(
                session(),
                DesktopCommandIntent::Abort {
                    operation_id: "operation-8".into(),
                },
            ),
            Ok(8)
        );
        assert_eq!(
            tracker.reserve(home(), DesktopCommandIntent::Prompt),
            Err(CommandAdmissionError::Full)
        );
        assert!(
            tracker
                .complete(7, &home(), &DesktopCommandIntent::Reload)
                .is_ok()
        );
        assert_eq!(tracker.reserve(home(), DesktopCommandIntent::Prompt), Ok(9));
    }

    #[test]
    fn checked_id_exhaustion_fails_closed_without_inserting_an_intent() {
        let mut tracker = CommandTracker::with_limits(u64::MAX, 2);
        assert_eq!(
            tracker.reserve(home(), DesktopCommandIntent::Reload),
            Err(CommandAdmissionError::IdExhausted)
        );
        assert!(!tracker.contains(&home(), &DesktopCommandIntent::Reload));
    }

    #[test]
    fn stale_id_and_intent_mismatch_cannot_remove_a_pending_command() {
        let mut tracker = CommandTracker::with_limits(11, 4);
        let intent = DesktopCommandIntent::Authorization {
            authorization_id: "authorization-11".into(),
            operation_id: "operation-11".into(),
        };
        let command_id = tracker.reserve(home(), intent.clone()).unwrap();

        assert_eq!(
            tracker.complete(99, &home(), &intent),
            Err(CommandCompletionError::UnknownCommand)
        );
        assert_eq!(
            tracker.complete(command_id, &home(), &DesktopCommandIntent::Reload),
            Err(CommandCompletionError::IntentMismatch)
        );
        assert!(tracker.matches(command_id, &home(), &intent));
    }

    #[test]
    fn owner_mismatch_cannot_complete_or_reject_another_workspace_command() {
        let mut tracker = CommandTracker::with_limits(20, 4);
        let intent = DesktopCommandIntent::Recovery {
            recovery_id: "recovery-20".into(),
            action: DesktopRecoveryAction::MarkFailed,
        };
        let command_id = tracker.reserve(home(), intent.clone()).unwrap();

        assert_eq!(
            tracker.complete(command_id, &session(), &intent),
            Err(CommandCompletionError::OwnerMismatch)
        );
        assert_eq!(
            tracker.reject(
                command_id,
                &session(),
                DesktopRuntimeCommandKind::ResolveRecovery,
            ),
            Err(CommandCompletionError::OwnerMismatch)
        );
        assert!(tracker.matches(command_id, &home(), &intent));
    }

    #[test]
    fn rejection_is_identity_kind_and_owner_bound() {
        let mut tracker = CommandTracker::with_limits(30, 4);
        let intent = DesktopCommandIntent::Recovery {
            recovery_id: "recovery-30".into(),
            action: DesktopRecoveryAction::MarkFailed,
        };
        let command_id = tracker.reserve(session(), intent.clone()).unwrap();

        assert_eq!(
            tracker.reject(
                command_id,
                &session(),
                DesktopRuntimeCommandKind::RetryRecovery,
            ),
            Err(CommandCompletionError::IntentMismatch)
        );
        assert_eq!(
            tracker
                .reject(
                    command_id,
                    &session(),
                    DesktopRuntimeCommandKind::ResolveRecovery,
                )
                .map(PendingCommand::into_intent),
            Ok(intent)
        );
    }

    #[test]
    fn cancelling_an_owner_leaves_other_workspaces_pending() {
        let mut tracker = CommandTracker::with_limits(40, 4);
        tracker
            .reserve(home(), DesktopCommandIntent::Prompt)
            .unwrap();
        let session_id = tracker
            .reserve(session(), DesktopCommandIntent::Reload)
            .unwrap();

        let cancelled = tracker.cancel_owner(&home());

        assert_eq!(cancelled.len(), 1);
        assert_eq!(cancelled[0].owner(), &WorkspaceKey::Home);
        assert!(tracker.matches(session_id, &session(), &DesktopCommandIntent::Reload));
    }

    #[test]
    fn transferring_home_commands_preserves_ids_and_intents() {
        let mut tracker = CommandTracker::with_limits(50, 4);
        let command_id = tracker
            .reserve(home(), DesktopCommandIntent::CreateSession)
            .unwrap();

        assert_eq!(tracker.transfer_owner(&home(), &session()), 1);
        assert!(tracker.matches(command_id, &session(), &DesktopCommandIntent::CreateSession));
        assert!(!tracker.contains(&home(), &DesktopCommandIntent::CreateSession));
    }

    #[test]
    fn transferring_one_command_does_not_move_its_siblings() {
        let mut tracker = CommandTracker::with_limits(60, 4);
        let create_id = tracker
            .reserve(home(), DesktopCommandIntent::CreateSession)
            .unwrap();
        let reload_id = tracker
            .reserve(home(), DesktopCommandIntent::Reload)
            .unwrap();

        tracker.transfer_command(create_id, session()).unwrap();

        assert!(tracker.matches(create_id, &session(), &DesktopCommandIntent::CreateSession));
        assert!(tracker.matches(reload_id, &home(), &DesktopCommandIntent::Reload));
    }
}
