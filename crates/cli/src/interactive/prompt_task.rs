use crate::interactive::error::CliError;
use coding_agent::api::authorization::{ToolAuthorizationDecision, ToolAuthorizationIdentity};
use coding_agent::api::client::{
    CodingAgentClientConnection, CodingAgentClientId, CodingAgentControlId,
    CodingAgentControlRejection, CodingAgentControlRejectionReason, CodingAgentDraftId,
    CodingAgentOperationControl, CodingAgentPreparedSubmission, CodingAgentPromptControl,
    CodingAgentSubmissionDraft, CodingAgentSubmittedOperationStatus,
    CodingAgentSubmittedTerminalAnchor,
};
use coding_agent::api::operation::{
    AgentTeamOutcome, CodingAgentOperation, CodingAgentOperationOutcome, PromptTurnOutcome,
    SelfHealingEditOutcome,
};
use coding_agent::api::runtime::{CodingAgentSession, CodingAgentSessionBootstrap};
use tokio::sync::{mpsc, oneshot};

mod control;
mod spawn;

use control::*;

const PROMPT_TASK_CONTROL_CAPACITY: usize = 32;

#[allow(
    clippy::large_enum_variant,
    reason = "single-owner task completion preserves exhaustive typed operation outcomes"
)]
pub(super) enum PromptTaskResult {
    Coding(CodingPromptTaskResult),
    AgentInvocation(AgentInvocationTaskResult),
    AgentTeam(AgentTeamTaskResult),
    DelegationApproval(DelegationApprovalTaskResult),
    SelfHealingEdit(SelfHealingEditTaskResult),
    SessionTreeLabel(SessionTreeLabelTaskResult),
    DelegationRejection(DelegationRejectionTaskResult),
    ForkSession(ForkSessionTaskResult),
    MergeReview(MergeReviewTaskResult),
}

#[allow(
    clippy::large_enum_variant,
    reason = "task completion is moved once and retains typed failure recovery state"
)]
pub(super) enum PromptTaskCompletion {
    Completed(PromptTaskResult),
    Failed(PromptTaskFailure),
    SetupFailed(CliError),
}

pub(super) struct PromptTaskFailure {
    pub(super) session: CodingAgentSession,
    pub(super) error: CliError,
}

pub(super) struct CodingPromptTaskResult {
    pub(super) session: CodingAgentSession,
    pub(super) outcome: PromptTurnOutcome,
    pub(super) replacement_session_id: Option<String>,
    pub(super) completion_notice: Option<String>,
    pub(super) hydrate_transcript: bool,
}

pub(super) struct ForkSessionTaskResult {
    pub(super) session: CodingAgentSession,
    pub(super) replacement_session_id: String,
    pub(super) completion_notice: Option<String>,
    pub(super) hydrate_transcript: bool,
}

pub(super) struct AgentInvocationTaskResult {
    pub(super) session: CodingAgentSession,
}

pub(super) struct AgentTeamTaskResult {
    pub(super) session: CodingAgentSession,
    pub(super) outcome: AgentTeamOutcome,
}

pub(super) struct DelegationApprovalTaskResult {
    pub(super) session: CodingAgentSession,
}

pub(super) struct SessionTreeLabelTaskResult {
    pub(super) session: CodingAgentSession,
    pub(super) entry_id: String,
    pub(super) label: Option<String>,
    pub(super) updated_at: String,
}

pub(super) struct DelegationRejectionTaskResult {
    pub(super) session: CodingAgentSession,
}

pub(super) struct SelfHealingEditTaskResult {
    pub(super) session: CodingAgentSession,
    pub(super) outcome: SelfHealingEditOutcome,
}

pub(super) struct MergeReviewTaskResult {
    pub(super) session: CodingAgentSession,
    pub(super) message: String,
}

enum PromptTaskControlHandle {
    Prompt(mpsc::Sender<PromptTaskControl>),
    Operation(mpsc::Sender<PromptTaskControl>),
    AbortOnly(Option<oneshot::Sender<()>>),
}

#[derive(Debug)]
enum PromptTaskControl {
    Abort,
    Steer(String),
    FollowUp(String),
    DecideToolAuthorization {
        identity: ToolAuthorizationIdentity,
        decision: ToolAuthorizationDecision,
    },
}

pub(super) struct PromptTask {
    control: PromptTaskControlHandle,
    pub(super) connection_handoff:
        Option<oneshot::Receiver<Result<Option<CodingAgentClientConnection>, CliError>>>,
    pub(super) done: oneshot::Receiver<PromptTaskCompletion>,
    abort_requested: bool,
}

async fn run_coding_prompt_task(
    operation: CodingAgentOperation,
    draft_text: String,
    bootstrap: CodingAgentSessionBootstrap,
    existing_session: Option<CodingAgentSession>,
    connection_tx: oneshot::Sender<Result<Option<CodingAgentClientConnection>, CliError>>,
    mut control_rx: mpsc::Receiver<PromptTaskControl>,
) -> PromptTaskCompletion {
    let (mut session, _) = match open_task_session(existing_session, &bootstrap).await {
        Ok(opened) => opened,
        Err(error) => return PromptTaskCompletion::SetupFailed(error),
    };
    let result = async {
        let (submission, connection) = prepare_interactive_submission(
            connection_tx,
            &mut session,
            Some(CodingAgentSubmissionDraft::new(
                CodingAgentDraftId("interactive-prompt".into()),
                draft_text,
            )),
            operation,
        )?;
        let operation_id = prepared_operation_id(&submission);
        let prompt_control = connection.prompt_control(operation_id.clone());

        let outcome = {
            let mut prompt = Box::pin(run_interactive_submission(
                &mut session,
                submission,
                &connection,
            ));
            let mut abort_requested = false;
            let mut controls_open = true;
            let mut control_sequence = 1;
            loop {
                tokio::select! {
                    control = control_rx.recv(), if controls_open => {
                        match control {
                            Some(PromptTaskControl::Abort) if !abort_requested => {
                                abort_requested = true;
                                abort_prompt_control(
                                    &prompt_control,
                                    &operation_id,
                                    &mut control_sequence,
                                )?;
                            }
                            Some(PromptTaskControl::Steer(text)) => {
                                prompt_control
                                    .steer(
                                        next_control_id(&operation_id, &mut control_sequence),
                                        text,
                                    )
                                    .map_err(control_rejection)?;
                            }
                            Some(PromptTaskControl::FollowUp(text)) => {
                                prompt_control
                                    .follow_up(
                                        next_control_id(&operation_id, &mut control_sequence),
                                        text,
                                    )
                                    .map_err(control_rejection)?;
                            }
                            Some(PromptTaskControl::DecideToolAuthorization {
                                identity,
                                decision,
                            }) => {
                                connection
                                    .decide_tool_authorization(&identity, decision)
                                    .await?;
                            }
                            Some(PromptTaskControl::Abort) => {}
                            None => {
                                controls_open = false;
                            }
                        }
                    }
                    outcome = &mut prompt => {
                        break outcome
                            .map(|operation_outcome| operation_outcome
                                .into_prompt()
                                .expect("prompt operation returned a different public outcome"));
                    }
                }
            }
        }?;
        Ok(outcome)
    }
    .await;

    complete_owned_task(session, result, |session, outcome| {
        PromptTaskResult::Coding(CodingPromptTaskResult {
            session,
            outcome,
            replacement_session_id: None,
            completion_notice: None,
            hydrate_transcript: false,
        })
    })
}

async fn run_coding_agent_invocation_task(
    operation: CodingAgentOperation,
    bootstrap: CodingAgentSessionBootstrap,
    existing_session: Option<CodingAgentSession>,
    connection_tx: oneshot::Sender<Result<Option<CodingAgentClientConnection>, CliError>>,
    mut control_rx: mpsc::Receiver<PromptTaskControl>,
) -> PromptTaskCompletion {
    let (mut session, _) = match open_task_session(existing_session, &bootstrap).await {
        Ok(opened) => opened,
        Err(error) => return PromptTaskCompletion::SetupFailed(error),
    };
    let result = async {
        let (submission, connection) =
            prepare_interactive_submission(connection_tx, &mut session, None, operation)?;
        let operation_id = prepared_operation_id(&submission);
        let prompt_control = connection.prompt_control(operation_id.clone());
        let mut invocation = Box::pin(run_interactive_submission(
            &mut session,
            submission,
            &connection,
        ));
        let mut abort_requested = false;
        let mut controls_open = true;
        let mut control_sequence = 1;
        loop {
            tokio::select! {
                control = control_rx.recv(), if controls_open => {
                    match control {
                        Some(PromptTaskControl::Abort) if !abort_requested => {
                            abort_requested = true;
                            abort_prompt_control(
                                &prompt_control,
                                &operation_id,
                                &mut control_sequence,
                            )?;
                        }
                        Some(PromptTaskControl::Steer(text)) => {
                            prompt_control
                                .steer(
                                    next_control_id(&operation_id, &mut control_sequence),
                                    text,
                                )
                                .map_err(control_rejection)?;
                        }
                        Some(PromptTaskControl::FollowUp(text)) => {
                            prompt_control
                                .follow_up(
                                    next_control_id(&operation_id, &mut control_sequence),
                                    text,
                                )
                                .map_err(control_rejection)?;
                        }
                        Some(PromptTaskControl::DecideToolAuthorization {
                            identity,
                            decision,
                        }) => {
                            connection
                                .decide_tool_authorization(&identity, decision)
                                .await?;
                        }
                        Some(PromptTaskControl::Abort) => {}
                        None => {
                            controls_open = false;
                        }
                    }
                }
                outcome = &mut invocation => {
                    break outcome
                        .map(|operation_outcome| {
                            operation_outcome
                                .into_agent_invocation()
                                .expect("agent invocation operation returned a different public outcome");
                        });
                }
            }
        }?;
        Ok(())
    }
    .await;

    complete_owned_task(session, result, |session, ()| {
        PromptTaskResult::AgentInvocation(AgentInvocationTaskResult { session })
    })
}

async fn run_coding_agent_team_task(
    operation: CodingAgentOperation,
    bootstrap: CodingAgentSessionBootstrap,
    existing_session: Option<CodingAgentSession>,
    connection_tx: oneshot::Sender<Result<Option<CodingAgentClientConnection>, CliError>>,
    mut control_rx: mpsc::Receiver<PromptTaskControl>,
) -> PromptTaskCompletion {
    let (mut session, _) = match open_task_session(existing_session, &bootstrap).await {
        Ok(opened) => opened,
        Err(error) => return PromptTaskCompletion::SetupFailed(error),
    };
    let result = async {
        let (submission, connection) =
            prepare_interactive_submission(connection_tx, &mut session, None, operation)?;
        let operation_id = prepared_operation_id(&submission);
        let operation_control = connection.operation_control(operation_id.clone());
        let outcome = {
            let mut invocation = Box::pin(run_interactive_submission(
                &mut session,
                submission,
                &connection,
            ));
            let mut abort_requested = false;
            let mut control_sequence = 1;
            loop {
            tokio::select! {
                control = control_rx.recv() => {
                    match control {
                        Some(PromptTaskControl::Abort) if !abort_requested => {
                            abort_requested = true;
                            abort_operation_control(
                                &operation_control,
                                &operation_id,
                                &mut control_sequence,
                            )?;
                        }
                        Some(PromptTaskControl::DecideToolAuthorization {
                            identity,
                            decision,
                        }) => {
                            connection
                                .decide_tool_authorization(&identity, decision)
                                .await?;
                        }
                        Some(PromptTaskControl::Abort)
                        | Some(PromptTaskControl::Steer(_))
                        | Some(PromptTaskControl::FollowUp(_))
                        | None => {}
                    }
                }
                outcome = &mut invocation => {
                    break outcome
                        .map(|operation_outcome| operation_outcome
                            .into_agent_team()
                            .expect("agent team operation returned a different public outcome"));
                }
            }
            }
        }?;

        Ok(outcome)
    }
    .await;

    complete_owned_task(session, result, |session, outcome| {
        PromptTaskResult::AgentTeam(AgentTeamTaskResult { session, outcome })
    })
}

async fn run_coding_delegation_approval_task(
    mut session: CodingAgentSession,
    operation_id: String,
    tool_call_id: String,
    connection_tx: oneshot::Sender<Result<Option<CodingAgentClientConnection>, CliError>>,
    mut control_rx: mpsc::Receiver<PromptTaskControl>,
) -> PromptTaskCompletion {
    let result = async {
        let operation = CodingAgentOperation::ApproveDelegation {
            operation_id,
            tool_call_id,
        };
        let (submission, connection) =
            prepare_interactive_submission(connection_tx, &mut session, None, operation)?;
        let active_operation_id = prepared_operation_id(&submission);
        let operation_control = connection.operation_control(active_operation_id.clone());
        let mut approval = Box::pin(run_interactive_submission(
            &mut session,
            submission,
            &connection,
        ));
        let mut abort_requested = false;
        let mut control_sequence = 1;
        loop {
            tokio::select! {
                control = control_rx.recv() => {
                    match control {
                        Some(PromptTaskControl::Abort) if !abort_requested => {
                            abort_requested = true;
                            abort_operation_control(
                                &operation_control,
                                &active_operation_id,
                                &mut control_sequence,
                            )?;
                        }
                        Some(PromptTaskControl::DecideToolAuthorization {
                            identity,
                            decision,
                        }) => {
                            connection
                                .decide_tool_authorization(&identity, decision)
                                .await?;
                        }
                        Some(PromptTaskControl::Abort)
                        | Some(PromptTaskControl::Steer(_))
                        | Some(PromptTaskControl::FollowUp(_))
                        | None => {}
                    }
                }
                outcome = &mut approval => {
                    break outcome
                        .map(|operation_outcome| operation_outcome
                            .into_delegation_approved()
                            .expect("delegation approval operation returned a different public outcome"));
                }
            }
        }?;

        Ok(())
    }
    .await;

    complete_owned_task(session, result, |session, ()| {
        PromptTaskResult::DelegationApproval(DelegationApprovalTaskResult { session })
    })
}

async fn run_coding_session_tree_label_task(
    mut session: CodingAgentSession,
    entry_id: String,
    label: Option<String>,
    connection_tx: oneshot::Sender<Result<Option<CodingAgentClientConnection>, CliError>>,
    mut abort_rx: oneshot::Receiver<()>,
) -> PromptTaskCompletion {
    let result = async {
        let operation = CodingAgentOperation::SetSessionTreeLabel { entry_id, label };
        let (submission, connection) =
            prepare_interactive_submission(connection_tx, &mut session, None, operation)?;
        let update = run_abortable_submission(&mut session, submission, &connection, &mut abort_rx)
            .await?
            .into_session_tree_label_changed()
            .expect("session tree label operation returned a different public outcome");

        Ok(update)
    }
    .await;

    complete_owned_task(session, result, |session, (entry_id, label, updated_at)| {
        PromptTaskResult::SessionTreeLabel(SessionTreeLabelTaskResult {
            session,
            entry_id,
            label,
            updated_at,
        })
    })
}

async fn run_coding_merge_review_task(
    mut session: CodingAgentSession,
    operation: CodingAgentOperation,
    connection_tx: oneshot::Sender<Result<Option<CodingAgentClientConnection>, CliError>>,
    mut abort_rx: oneshot::Receiver<()>,
) -> PromptTaskCompletion {
    let result = async {
        let (submission, connection) =
            prepare_interactive_submission(connection_tx, &mut session, None, operation)?;
        let outcome =
            run_abortable_submission(&mut session, submission, &connection, &mut abort_rx).await?;
        let message = match outcome {
            CodingAgentOperationOutcome::MergeProposals(proposals) => {
                if proposals.is_empty() {
                    "No pending merge proposals.".to_string()
                } else {
                    let mut lines = Vec::new();
                    for proposal in proposals {
                        lines.push(format!(
                            "Proposal {} from {} ({} changes)",
                            proposal.worktree_id,
                            proposal.child_operation_id,
                            proposal.changes.len()
                        ));
                        for change in proposal.changes {
                            let marker = match change.kind {
                                coding_agent::api::event::CodingAgentMergeChangeKind::Added => "+",
                                coding_agent::api::event::CodingAgentMergeChangeKind::Modified => {
                                    "~"
                                }
                                coding_agent::api::event::CodingAgentMergeChangeKind::Deleted => {
                                    "-"
                                }
                            };
                            lines.push(format!(
                                "  {marker} {} (+{}/-{})",
                                change.path, change.additions, change.deletions
                            ));
                        }
                    }
                    lines.join("\n")
                }
            }
            CodingAgentOperationOutcome::MergeApplied {
                worktree_id,
                applied,
            } => format!("Merged {worktree_id}: {applied} changes applied."),
            CodingAgentOperationOutcome::WorktreeDiscarded { worktree_id } => {
                format!("Discarded merge proposal {worktree_id}.")
            }
            _ => unreachable!("merge review operation returned a different public outcome"),
        };
        Ok(message)
    }
    .await;

    complete_owned_task(session, result, |session, message| {
        PromptTaskResult::MergeReview(MergeReviewTaskResult { session, message })
    })
}

async fn run_coding_delegation_rejection_task(
    mut session: CodingAgentSession,
    operation_id: String,
    tool_call_id: String,
    reason: String,
    connection_tx: oneshot::Sender<Result<Option<CodingAgentClientConnection>, CliError>>,
    mut abort_rx: oneshot::Receiver<()>,
) -> PromptTaskCompletion {
    let result = async {
        let operation = CodingAgentOperation::RejectDelegation {
            operation_id,
            tool_call_id,
            reason,
        };
        let (submission, connection) =
            prepare_interactive_submission(connection_tx, &mut session, None, operation)?;
        run_abortable_submission(&mut session, submission, &connection, &mut abort_rx)
            .await?
            .into_delegation_rejected()
            .expect("delegation rejection operation returned a different public outcome");
        Ok(())
    }
    .await;

    complete_owned_task(session, result, |session, ()| {
        PromptTaskResult::DelegationRejection(DelegationRejectionTaskResult { session })
    })
}

async fn run_coding_compact_task(
    operation: CodingAgentOperation,
    bootstrap: CodingAgentSessionBootstrap,
    existing_session: Option<CodingAgentSession>,
    connection_tx: oneshot::Sender<Result<Option<CodingAgentClientConnection>, CliError>>,
    mut abort_rx: oneshot::Receiver<()>,
) -> PromptTaskCompletion {
    let (mut session, _) = match open_task_session(existing_session, &bootstrap).await {
        Ok(opened) => opened,
        Err(error) => return PromptTaskCompletion::SetupFailed(error),
    };
    let result = async {
        let (submission, connection) =
            prepare_interactive_submission(connection_tx, &mut session, None, operation)?;
        Ok(
            run_abortable_submission(&mut session, submission, &connection, &mut abort_rx)
                .await?
                .into_compact()
                .expect("manual compaction operation returned a different public outcome"),
        )
    }
    .await;

    complete_owned_task(session, result, |session, outcome| {
        PromptTaskResult::Coding(CodingPromptTaskResult {
            session,
            outcome,
            replacement_session_id: None,
            completion_notice: None,
            hydrate_transcript: false,
        })
    })
}

async fn run_coding_self_healing_edit_task(
    operation: CodingAgentOperation,
    bootstrap: CodingAgentSessionBootstrap,
    existing_session: Option<CodingAgentSession>,
    connection_tx: oneshot::Sender<Result<Option<CodingAgentClientConnection>, CliError>>,
    mut abort_rx: oneshot::Receiver<()>,
) -> PromptTaskCompletion {
    let (mut session, _) = match open_task_session(existing_session, &bootstrap).await {
        Ok(opened) => opened,
        Err(error) => return PromptTaskCompletion::SetupFailed(error),
    };
    let result = async {
        let (submission, connection) =
            prepare_interactive_submission(connection_tx, &mut session, None, operation)?;
        Ok(
            run_abortable_submission(&mut session, submission, &connection, &mut abort_rx)
                .await?
                .into_self_healing_edit()
                .expect("self-healing edit operation returned a different public outcome"),
        )
    }
    .await;

    complete_owned_task(session, result, |session, outcome| {
        PromptTaskResult::SelfHealingEdit(SelfHealingEditTaskResult { session, outcome })
    })
}

async fn run_coding_branch_summary_task(
    operation: CodingAgentOperation,
    bootstrap: CodingAgentSessionBootstrap,
    existing_session: Option<CodingAgentSession>,
    connection_tx: oneshot::Sender<Result<Option<CodingAgentClientConnection>, CliError>>,
    mut abort_rx: oneshot::Receiver<()>,
) -> PromptTaskCompletion {
    let (mut session, _) = match open_task_session(existing_session, &bootstrap).await {
        Ok(opened) => opened,
        Err(error) => return PromptTaskCompletion::SetupFailed(error),
    };
    let result = async {
        let (submission, connection) =
            prepare_interactive_submission(connection_tx, &mut session, None, operation)?;
        Ok(
            run_abortable_submission(&mut session, submission, &connection, &mut abort_rx)
                .await?
                .into_branch_summary()
                .expect("branch summary operation returned a different public outcome"),
        )
    }
    .await;

    complete_owned_task(session, result, |session, outcome| {
        PromptTaskResult::Coding(CodingPromptTaskResult {
            session,
            outcome,
            replacement_session_id: None,
            completion_notice: None,
            hydrate_transcript: false,
        })
    })
}

async fn run_coding_branch_summary_navigation_task(
    operation: CodingAgentOperation,
    bootstrap: CodingAgentSessionBootstrap,
    existing_session: Option<CodingAgentSession>,
    target_leaf_id: String,
    connection_tx: oneshot::Sender<Result<Option<CodingAgentClientConnection>, CliError>>,
    mut abort_rx: oneshot::Receiver<()>,
) -> PromptTaskCompletion {
    let (mut session, _) = match open_task_session(existing_session, &bootstrap).await {
        Ok(opened) => opened,
        Err(error) => return PromptTaskCompletion::SetupFailed(error),
    };
    let result = async {
        let (submission, connection) =
            prepare_interactive_submission(connection_tx, &mut session, None, operation)?;
        let outcome =
            run_abortable_submission(&mut session, submission, &connection, &mut abort_rx)
                .await?
                .into_branch_summary()
                .expect("branch summary navigation operation returned a different public outcome");

        if !branch_summary_allows_navigation(&outcome) {
            return Ok((outcome, false, None));
        }

        let fork = CodingAgentOperation::ForkSession {
            target_leaf_id: Some(target_leaf_id),
        };
        let submission = connection.prepare_client_submission(&mut session, None, fork)?;
        run_abortable_submission(&mut session, submission, &connection, &mut abort_rx)
            .await?
            .into_session_forked()
            .expect("navigation fork operation returned a different public outcome");

        let replacement_session_id = session.view()?.session_id.clone();
        Ok((outcome, true, Some(replacement_session_id)))
    }
    .await;

    complete_owned_task(
        session,
        result,
        |session, (outcome, navigated, replacement_session_id)| {
            PromptTaskResult::Coding(CodingPromptTaskResult {
                session,
                outcome,
                replacement_session_id,
                completion_notice: navigated.then(|| "Navigated to selected point".to_string()),
                hydrate_transcript: navigated,
            })
        },
    )
}

fn branch_summary_allows_navigation(outcome: &PromptTurnOutcome) -> bool {
    matches!(outcome, PromptTurnOutcome::Success { .. })
}

async fn run_coding_fork_session_task(
    operation: CodingAgentOperation,
    bootstrap: CodingAgentSessionBootstrap,
    existing_session: Option<CodingAgentSession>,
    completion_notice: Option<String>,
    connection_tx: oneshot::Sender<Result<Option<CodingAgentClientConnection>, CliError>>,
    mut abort_rx: oneshot::Receiver<()>,
) -> PromptTaskCompletion {
    let (mut session, _) = match open_task_session(existing_session, &bootstrap).await {
        Ok(opened) => opened,
        Err(error) => return PromptTaskCompletion::SetupFailed(error),
    };
    let result = async {
        let (submission, connection) =
            prepare_interactive_submission(connection_tx, &mut session, None, operation)?;
        run_abortable_submission(&mut session, submission, &connection, &mut abort_rx)
            .await?
            .into_session_forked()
            .expect("fork session operation returned a different public outcome");

        Ok(session.view()?.session_id.clone())
    }
    .await;

    complete_owned_task(session, result, |session, replacement_session_id| {
        PromptTaskResult::ForkSession(ForkSessionTaskResult {
            session,
            replacement_session_id,
            completion_notice,
            hydrate_transcript: true,
        })
    })
}
