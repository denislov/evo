use std::path::PathBuf;

use crate::operations::prompt::context::InternalPromptTurnOutcome;
use crate::runtime::facade::{
    CodingSessionError, PendingDelegationConfirmationQueue, pending_state_from_replay,
};
use crate::session::service::FinalizedSessionWrite;
use crate::session::service::{SessionService, StartupRecoveryMarker};

pub(crate) fn apply_finalized_session_write(
    outcome: &mut InternalPromptTurnOutcome,
    finalized: &FinalizedSessionWrite,
) {
    outcome.apply_success_session_write_metadata(
        finalized.session_id.clone(),
        finalized.leaf_id.clone(),
    );
}

pub(crate) struct ReplayDerivedOwnerState {
    pub(crate) pending_delegation_confirmations: PendingDelegationConfirmationQueue,
    pub(crate) startup_recovery_markers: Vec<StartupRecoveryMarker>,
}

pub(crate) fn replay_derived_owner_state(
    session_service: &mut SessionService,
) -> Result<ReplayDerivedOwnerState, CodingSessionError> {
    let startup_recovery_markers = session_service.take_startup_recovery_markers();
    let replay = session_service.replay()?;
    let cwd = replay
        .cwd
        .as_deref()
        .map(PathBuf::from)
        .unwrap_or_else(default_cwd);
    let pending_delegation_confirmations = PendingDelegationConfirmationQueue::from_pending(
        replay
            .pending_delegation_confirmations
            .into_iter()
            .map(|pending| pending_state_from_replay(pending, &cwd))
            .collect::<Result<Vec<_>, _>>()?,
    );
    Ok(ReplayDerivedOwnerState {
        pending_delegation_confirmations,
        startup_recovery_markers,
    })
}

pub(crate) fn session_cwd(session_service: &SessionService) -> Option<PathBuf> {
    session_service
        .replay()
        .ok()
        .and_then(|replay| replay.cwd.map(PathBuf::from))
}

pub(crate) fn default_cwd() -> PathBuf {
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}
