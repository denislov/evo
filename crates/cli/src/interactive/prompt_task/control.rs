use super::*;

pub(super) fn prepare_interactive_submission(
    connection_tx: oneshot::Sender<Result<Option<CodingAgentClientConnection>, CliError>>,
    session: &mut CodingAgentSession,
    draft: Option<CodingAgentSubmissionDraft>,
    operation: CodingAgentOperation,
) -> Result<(CodingAgentPreparedSubmission, CodingAgentClientConnection), CliError> {
    let connection = match session.connect(CodingAgentClientId::new("interactive")) {
        Ok(connection) => connection,
        Err(error) => {
            let public = CliError::from(error.clone());
            let _ = connection_tx.send(Err(public));
            return Err(CliError::from(error));
        }
    };
    let submission = match connection.prepare_client_submission(session, draft, operation) {
        Ok(submission) => submission,
        Err(error) => {
            let public = CliError::from(error.clone());
            let _ = connection_tx.send(Err(public));
            return Err(CliError::from(error));
        }
    };
    if connection_tx.send(Ok(Some(connection.clone()))).is_err() {
        submission.discard(session)?;
        return Err(CliError::AgentFailure(
            "interactive connection handoff receiver closed".to_string(),
        ));
    }
    Ok((submission, connection))
}

pub(super) fn prepared_operation_id(submission: &CodingAgentPreparedSubmission) -> String {
    submission.operation_id().to_owned()
}

pub(super) fn acknowledge_outcome_only(
    connection: &CodingAgentClientConnection,
) -> Result<(), CliError> {
    let Some(submitted) = connection.state()?.submitted_operation else {
        return Ok(());
    };
    if let CodingAgentSubmittedOperationStatus::Terminal {
        anchor: CodingAgentSubmittedTerminalAnchor::OutcomeOnly { acknowledgement },
        ..
    } = submitted.status
    {
        connection.acknowledge_outcome(acknowledgement)?;
    }
    Ok(())
}

pub(super) async fn run_interactive_submission(
    session: &mut CodingAgentSession,
    submission: CodingAgentPreparedSubmission,
    connection: &CodingAgentClientConnection,
) -> Result<CodingAgentOperationOutcome, CliError> {
    let outcome = submission.run(session).await.map_err(CliError::from);
    let acknowledgement = acknowledge_outcome_only(connection);
    match outcome {
        Ok(outcome) => acknowledgement.map(|()| outcome),
        Err(error) => Err(error),
    }
}

pub(super) fn next_control_id(operation_id: &str, sequence: &mut u64) -> CodingAgentControlId {
    let control_id = CodingAgentControlId(format!("interactive:{operation_id}:{}", *sequence));
    *sequence = sequence.saturating_add(1);
    control_id
}

pub(super) fn control_rejection(error: CodingAgentControlRejection) -> CliError {
    CliError::SessionFailure(format!(
        "interactive {:?} control rejected: {:?}",
        error.kind, error.reason
    ))
}

pub(super) fn abort_prompt_control(
    control: &CodingAgentPromptControl,
    operation_id: &str,
    sequence: &mut u64,
) -> Result<(), CliError> {
    match control.abort(next_control_id(operation_id, sequence), "user cancelled") {
        Ok(_) => Ok(()),
        Err(error)
            if matches!(
                error.reason,
                CodingAgentControlRejectionReason::NoLongerCancellable
                    | CodingAgentControlRejectionReason::TargetNotRunning
            ) =>
        {
            Ok(())
        }
        Err(error) => Err(control_rejection(error)),
    }
}

pub(super) fn abort_operation_control(
    control: &CodingAgentOperationControl,
    operation_id: &str,
    sequence: &mut u64,
) -> Result<(), CliError> {
    match control.abort(next_control_id(operation_id, sequence), "user cancelled") {
        Ok(_) => Ok(()),
        Err(error)
            if matches!(
                error.reason,
                CodingAgentControlRejectionReason::NoLongerCancellable
                    | CodingAgentControlRejectionReason::TargetNotRunning
            ) =>
        {
            Ok(())
        }
        Err(error) => Err(control_rejection(error)),
    }
}

pub(super) async fn run_abortable_submission(
    session: &mut CodingAgentSession,
    submission: CodingAgentPreparedSubmission,
    connection: &CodingAgentClientConnection,
    abort_rx: &mut oneshot::Receiver<()>,
) -> Result<CodingAgentOperationOutcome, CliError> {
    let operation_id = prepared_operation_id(&submission);
    let operation_control = connection.operation_control(operation_id.clone());
    let mut operation = Box::pin(run_interactive_submission(session, submission, connection));
    let mut abort_requested = false;
    let mut control_sequence = 1;
    loop {
        tokio::select! {
            _ = &mut *abort_rx, if !abort_requested => {
                abort_requested = true;
                abort_operation_control(
                    &operation_control,
                    &operation_id,
                    &mut control_sequence,
                )?;
            }
            outcome = &mut operation => break outcome,
        }
    }
}

pub(super) fn complete_owned_task<T>(
    session: CodingAgentSession,
    result: Result<T, CliError>,
    completed: impl FnOnce(CodingAgentSession, T) -> PromptTaskResult,
) -> PromptTaskCompletion {
    match result {
        Ok(value) => PromptTaskCompletion::Completed(completed(session, value)),
        Err(error) => PromptTaskCompletion::Failed(PromptTaskFailure { session, error }),
    }
}

pub(super) async fn open_task_session(
    existing_session: Option<CodingAgentSession>,
    bootstrap: &CodingAgentSessionBootstrap,
) -> Result<(CodingAgentSession, bool), CliError> {
    match existing_session {
        Some(session) => Ok((session, false)),
        None => bootstrap
            .open()
            .await
            .map(|session| (session, true))
            .map_err(CliError::from),
    }
}
