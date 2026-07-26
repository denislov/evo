use std::collections::VecDeque;
use std::fmt;

use coding_agent::api::review::CodingAgentFileReviewRequest;
use desktop::runtime::{
    DesktopRecoveryAction, DesktopRuntimeCommandKind, DesktopRuntimeSelectionKind,
};

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
    ListSessions,
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
            Self::ListSessions => DesktopRuntimeCommandKind::ListSessions,
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
struct PendingDesktopCommand {
    command_id: u64,
    intent: DesktopCommandIntent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DesktopCommandLedgerError {
    Full,
    IdExhausted,
}

impl fmt::Display for DesktopCommandLedgerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Full => formatter.write_str("desktop command ledger is full"),
            Self::IdExhausted => formatter.write_str("desktop command IDs are exhausted"),
        }
    }
}

pub(crate) struct DesktopCommandLedger {
    next_command_id: u64,
    pending: VecDeque<PendingDesktopCommand>,
    capacity: usize,
}

impl Default for DesktopCommandLedger {
    fn default() -> Self {
        Self {
            next_command_id: 1,
            pending: VecDeque::with_capacity(MAX_PENDING_DESKTOP_COMMANDS),
            capacity: MAX_PENDING_DESKTOP_COMMANDS,
        }
    }
}

impl DesktopCommandLedger {
    pub(crate) fn reserve(
        &mut self,
        intent: DesktopCommandIntent,
    ) -> Result<u64, DesktopCommandLedgerError> {
        if self.pending.len() >= self.capacity {
            return Err(DesktopCommandLedgerError::Full);
        }
        let command_id = self.next_command_id;
        self.next_command_id = command_id
            .checked_add(1)
            .ok_or(DesktopCommandLedgerError::IdExhausted)?;
        self.pending
            .push_back(PendingDesktopCommand { command_id, intent });
        Ok(command_id)
    }

    pub(crate) fn contains(&self, intent: &DesktopCommandIntent) -> bool {
        self.pending.iter().any(|pending| &pending.intent == intent)
    }

    pub(crate) fn contains_where(&self, predicate: impl Fn(&DesktopCommandIntent) -> bool) -> bool {
        self.pending
            .iter()
            .any(|pending| predicate(&pending.intent))
    }

    pub(crate) fn matches(&self, command_id: u64, intent: &DesktopCommandIntent) -> bool {
        self.pending
            .iter()
            .any(|pending| pending.command_id == command_id && &pending.intent == intent)
    }

    pub(crate) fn intent(&self, command_id: u64) -> Option<&DesktopCommandIntent> {
        self.pending
            .iter()
            .find(|pending| pending.command_id == command_id)
            .map(|pending| &pending.intent)
    }

    pub(crate) fn complete(&mut self, command_id: u64, intent: &DesktopCommandIntent) -> bool {
        self.remove_where(|pending| pending.command_id == command_id && &pending.intent == intent)
            .is_some()
    }

    pub(crate) fn complete_rejection(
        &mut self,
        command_id: u64,
        command: DesktopRuntimeCommandKind,
    ) -> Option<DesktopCommandIntent> {
        self.remove_where(|pending| {
            pending.command_id == command_id && pending.intent.command_kind() == command
        })
        .map(|pending| pending.intent)
    }

    pub(crate) fn complete_authorization(
        &mut self,
        command_id: u64,
        authorization_id: &str,
    ) -> bool {
        self.remove_where(|pending| {
            pending.command_id == command_id
                && matches!(
                    &pending.intent,
                    DesktopCommandIntent::Authorization {
                        authorization_id: pending_authorization_id,
                        ..
                    } if pending_authorization_id == authorization_id
                )
        })
        .is_some()
    }

    pub(crate) fn complete_where(
        &mut self,
        predicate: impl Fn(&DesktopCommandIntent) -> bool,
    ) -> Option<DesktopCommandIntent> {
        self.remove_where(|pending| predicate(&pending.intent))
            .map(|pending| pending.intent)
    }

    pub(crate) fn authorization(&self) -> Option<(u64, &str, &str)> {
        self.pending
            .iter()
            .find_map(|pending| match &pending.intent {
                DesktopCommandIntent::Authorization {
                    authorization_id,
                    operation_id,
                } => Some((
                    pending.command_id,
                    authorization_id.as_str(),
                    operation_id.as_str(),
                )),
                _ => None,
            })
    }

    pub(crate) fn clear(&mut self) {
        self.pending.clear();
    }

    fn remove_where(
        &mut self,
        predicate: impl Fn(&PendingDesktopCommand) -> bool,
    ) -> Option<PendingDesktopCommand> {
        let index = self.pending.iter().position(predicate)?;
        self.pending.remove(index)
    }

    #[cfg(test)]
    fn with_limits(next_command_id: u64, capacity: usize) -> Self {
        Self {
            next_command_id,
            pending: VecDeque::with_capacity(capacity),
            capacity,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pressure_is_bounded_and_failed_reservations_do_not_consume_ids() {
        let mut ledger = DesktopCommandLedger::with_limits(7, 2);
        assert_eq!(ledger.reserve(DesktopCommandIntent::Reload), Ok(7));
        assert_eq!(
            ledger.reserve(DesktopCommandIntent::Abort {
                operation_id: "operation-8".into(),
            }),
            Ok(8)
        );
        assert_eq!(
            ledger.reserve(DesktopCommandIntent::Prompt),
            Err(DesktopCommandLedgerError::Full)
        );
        assert!(ledger.complete(7, &DesktopCommandIntent::Reload));
        assert_eq!(ledger.reserve(DesktopCommandIntent::Prompt), Ok(9));
    }

    #[test]
    fn checked_id_exhaustion_fails_closed_without_inserting_an_intent() {
        let mut ledger = DesktopCommandLedger::with_limits(u64::MAX, 2);
        assert_eq!(
            ledger.reserve(DesktopCommandIntent::Reload),
            Err(DesktopCommandLedgerError::IdExhausted)
        );
        assert!(!ledger.contains(&DesktopCommandIntent::Reload));
    }

    #[test]
    fn stale_or_mismatched_completion_cannot_remove_another_intent() {
        let mut ledger = DesktopCommandLedger::with_limits(11, 4);
        let authorization = DesktopCommandIntent::Authorization {
            authorization_id: "authorization-11".into(),
            operation_id: "operation-11".into(),
        };
        let command_id = ledger.reserve(authorization.clone()).unwrap();

        assert!(!ledger.complete(
            command_id,
            &DesktopCommandIntent::Authorization {
                authorization_id: "stale-authorization".into(),
                operation_id: "operation-11".into(),
            }
        ));
        assert_eq!(
            ledger.complete_rejection(command_id, DesktopRuntimeCommandKind::Abort),
            None
        );
        assert!(ledger.matches(command_id, &authorization));
        assert!(ledger.complete(command_id, &authorization));
    }

    #[test]
    fn rejection_matches_command_id_and_typed_runtime_kind() {
        let mut ledger = DesktopCommandLedger::with_limits(20, 4);
        let recovery = DesktopCommandIntent::Recovery {
            recovery_id: "recovery-20".into(),
            action: DesktopRecoveryAction::MarkFailed,
        };
        let command_id = ledger.reserve(recovery.clone()).unwrap();

        assert_eq!(
            ledger.complete_rejection(command_id, DesktopRuntimeCommandKind::RetryRecovery),
            None
        );
        assert_eq!(
            ledger.complete_rejection(command_id, DesktopRuntimeCommandKind::ResolveRecovery),
            Some(recovery)
        );
    }

    #[test]
    fn terminal_and_projection_completion_are_identity_bound() {
        let mut ledger = DesktopCommandLedger::with_limits(30, 6);
        let abort_a = DesktopCommandIntent::Abort {
            operation_id: "operation-a".into(),
        };
        let abort_b = DesktopCommandIntent::Abort {
            operation_id: "operation-b".into(),
        };
        let authorization = DesktopCommandIntent::Authorization {
            authorization_id: "authorization-a".into(),
            operation_id: "operation-a".into(),
        };
        let abort_a_id = ledger.reserve(abort_a.clone()).unwrap();
        let abort_b_id = ledger.reserve(abort_b.clone()).unwrap();
        let authorization_id = ledger.reserve(authorization.clone()).unwrap();

        assert!(!ledger.complete_authorization(authorization_id, "authorization-stale"));
        assert!(ledger.matches(authorization_id, &authorization));
        assert_eq!(
            ledger.complete_where(|intent| {
                matches!(
                    intent,
                    DesktopCommandIntent::Abort { operation_id }
                        if operation_id == "operation-a"
                )
            }),
            Some(abort_a)
        );
        assert!(!ledger.matches(
            abort_a_id,
            &DesktopCommandIntent::Abort {
                operation_id: "operation-a".into(),
            }
        ));
        assert!(ledger.matches(abort_b_id, &abort_b));
        assert!(ledger.complete_authorization(authorization_id, "authorization-a"));
    }
}
