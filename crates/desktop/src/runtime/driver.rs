use std::collections::VecDeque;
use std::sync::mpsc as std_mpsc;
use std::time::Duration;

use coding_agent::api::client::{
    CodingAgentClientConnection, CodingAgentClientId, CodingAgentControlId, CodingAgentDraftId,
    CodingAgentFreshSnapshotRecovery, CodingAgentReconnect, CodingAgentReconnectDelivery,
    CodingAgentReconnectReceiver, CodingAgentRecoveryReason, CodingAgentSnapshot,
    CodingAgentSubmissionDraft,
};
use coding_agent::api::embedding::{
    CodingAgentEmbeddingContext, CodingAgentEmbeddingOptions, CodingAgentEmbeddingSnapshot,
    CodingAgentThinkingLevel,
};
use coding_agent::api::event::{
    CodingAgentProductEvent, CodingAgentProductEventDeliveryClass, CodingAgentProductEventFamily,
    CodingAgentRecoveryResolution,
};
use coding_agent::api::operation::{
    CodingAgentOperation, CodingAgentOperationOutcome, PromptInvocation,
};
use coding_agent::api::review::{
    CodingAgentExternalEditorTarget, CodingAgentFileReview, CodingAgentFileReviewRequest,
};
use coding_agent::api::runtime::{
    CodingAgentRecoveryResolutionRequest, CodingAgentRecoveryRetryRequest, CodingAgentSession,
};
use coding_agent::api::view::ProfileId;
use tokio::sync::{mpsc, watch};
use tokio::task;

use crate::file_review::{DesktopExternalEditorConfig, launch_external_editor};

use super::dispatch::{dispatch_active_command, dispatch_idle_command};
use super::protocol::{
    DESKTOP_UPDATE_QUEUE_CAPACITY, DesktopBridgeError, DesktopRecoveryIdentity,
    DesktopRuntimeCommand, DesktopRuntimeError, DesktopRuntimeErrorSource,
    DesktopRuntimeHydratedSnapshot, DesktopRuntimeMetadataSnapshot, DesktopRuntimeReadySnapshot,
    DesktopRuntimeRecoverySnapshot, DesktopRuntimeUpdate, DesktopSessionCatalogEntry,
    MAX_DESKTOP_SESSION_CATALOG, MAX_SESSION_ID_BYTES, bounded_utf8_prefix, local_runtime_error,
    runtime_error,
};

const RUNTIME_SHUTDOWN_DEADLINE: Duration = Duration::from_secs(10);
pub(super) const DESKTOP_CLIENT_ID: &str = "evo-desktop";

pub(super) struct RuntimeState {
    pub(super) context: CodingAgentEmbeddingContext,
    pub(super) session: Option<CodingAgentSession>,
    #[cfg(test)]
    pub(super) fail_next_prompt_start: bool,
}

impl RuntimeState {
    pub(super) fn metadata_snapshot(&self) -> DesktopRuntimeMetadataSnapshot {
        DesktopRuntimeMetadataSnapshot {
            project: self.context.snapshot().clone(),
            session: self.session.as_ref().map(CodingAgentSession::snapshot),
        }
    }

    pub(super) fn snapshot(&self) -> Result<DesktopRuntimeHydratedSnapshot, DesktopBridgeError> {
        let session = self
            .session
            .as_ref()
            .ok_or_else(|| DesktopBridgeError::Session {
                message: "desktop runtime has no idle session owner".into(),
            })?;
        Ok(DesktopRuntimeHydratedSnapshot {
            project: self.context.snapshot().clone(),
            session: session.snapshot(),
            transcript: session.transcript_snapshot()?,
            pending_recoveries: session.recovery_pending()?,
        })
    }

    pub(super) fn session_catalog(
        &self,
    ) -> Result<(Vec<DesktopSessionCatalogEntry>, usize), DesktopBridgeError> {
        let catalog = self.context.session_query()?.overviews()?;
        let omitted = catalog
            .overviews
            .len()
            .saturating_sub(MAX_DESKTOP_SESSION_CATALOG)
            + usize::from(catalog.truncated);
        let sessions = catalog
            .overviews
            .into_iter()
            .take(MAX_DESKTOP_SESSION_CATALOG)
            .map(|overview| DesktopSessionCatalogEntry {
                session_id: bounded_utf8_prefix(&overview.session_id, MAX_SESSION_ID_BYTES),
                name: overview.name.map(|name| bounded_utf8_prefix(&name, 256)),
                cwd: overview.cwd.map(|cwd| bounded_utf8_prefix(&cwd, 1024)),
                created_at: bounded_utf8_prefix(&overview.created_at, 128),
                updated_at: bounded_utf8_prefix(&overview.updated_at, 128),
                active_leaf_id: overview
                    .active_leaf_id
                    .map(|id| bounded_utf8_prefix(&id, 256)),
            })
            .collect();
        Ok((sessions, omitted))
    }

    fn recovery_snapshot(&self) -> Result<DesktopRuntimeRecoverySnapshot, DesktopBridgeError> {
        let session = self
            .session
            .as_ref()
            .ok_or_else(|| DesktopBridgeError::Session {
                message: "desktop runtime has no idle session owner".into(),
            })?;
        Ok(DesktopRuntimeRecoverySnapshot {
            project: self.context.snapshot().clone(),
            session: session.snapshot(),
            pending_recoveries: session.recovery_pending()?,
        })
    }

    pub(super) async fn review_changed_file(
        &self,
        request: CodingAgentFileReviewRequest,
    ) -> Result<CodingAgentFileReview, DesktopBridgeError> {
        let session = self
            .session
            .as_ref()
            .ok_or_else(|| DesktopBridgeError::Session {
                message: "desktop runtime has no idle session owner".into(),
            })?;
        session
            .review_changed_file(request)
            .await
            .map_err(DesktopBridgeError::from)
    }

    pub(super) async fn open_external_editor(
        &self,
        target: CodingAgentExternalEditorTarget,
        editor: DesktopExternalEditorConfig,
    ) -> Result<String, DesktopBridgeError> {
        let session = self
            .session
            .as_ref()
            .ok_or_else(|| DesktopBridgeError::Session {
                message: "desktop runtime has no idle session owner".into(),
            })?;
        session.revalidate_external_editor_target(&target).await?;
        let project_relative_path = target.project_relative_path().to_owned();
        task::spawn_blocking(move || launch_external_editor(&editor, &target))
            .await
            .map_err(|_| DesktopBridgeError::ExternalEditor)??;
        Ok(project_relative_path)
    }

    pub(super) fn retry_recovery(
        &mut self,
        identity: DesktopRecoveryIdentity,
    ) -> Result<(String, DesktopRuntimeRecoverySnapshot), DesktopBridgeError> {
        let session = self
            .session
            .as_mut()
            .ok_or_else(|| DesktopBridgeError::Session {
                message: "desktop runtime has no idle session owner".into(),
            })?;
        let result = session.retry_recovery(CodingAgentRecoveryRetryRequest {
            operation_id: identity.operation_id,
            recovery_id: identity.recovery_id,
            expected_record_version: identity.record_version,
            expected_descriptor_revision: identity.descriptor_revision,
            expected_capability_generation: identity.capability_generation,
            expected_attempt_count: identity.attempt_count,
            schedule_with_backoff: false,
        })?;
        let recovery_id = result.recovery_id;
        Ok((recovery_id, self.recovery_snapshot()?))
    }

    pub(super) fn resolve_recovery(
        &mut self,
        identity: DesktopRecoveryIdentity,
        resolution: CodingAgentRecoveryResolution,
    ) -> Result<(String, DesktopRuntimeRecoverySnapshot), DesktopBridgeError> {
        let action = match resolution {
            CodingAgentRecoveryResolution::Failed => "marked failed",
            CodingAgentRecoveryResolution::Aborted => "aborted",
        };
        let session = self
            .session
            .as_mut()
            .ok_or_else(|| DesktopBridgeError::Session {
                message: "desktop runtime has no idle session owner".into(),
            })?;
        let result = session.resolve_recovery(CodingAgentRecoveryResolutionRequest {
            operation_id: identity.operation_id,
            recovery_id: identity.recovery_id,
            expected_record_version: identity.record_version,
            expected_descriptor_revision: identity.descriptor_revision,
            expected_capability_generation: identity.capability_generation,
            expected_attempt_count: identity.attempt_count,
            resolution,
            reason: format!("native desktop operator {action} uncertain operation"),
        })?;
        let recovery_id = result.recovery_id;
        Ok((recovery_id, self.recovery_snapshot()?))
    }

    pub(super) async fn select_session_profile(
        &mut self,
        profile_id: String,
    ) -> Result<DesktopRuntimeMetadataSnapshot, DesktopBridgeError> {
        if !self
            .context
            .snapshot()
            .profiles
            .iter()
            .any(|profile| profile.id.as_str() == profile_id)
        {
            return Err(DesktopBridgeError::Input {
                message: format!("unknown desktop session profile {profile_id}"),
            });
        }
        let profile_id =
            ProfileId::new(profile_id).map_err(|message| DesktopBridgeError::Input {
                message: format!("invalid desktop session profile: {message}"),
            })?;
        let session = self
            .session
            .as_mut()
            .ok_or_else(|| DesktopBridgeError::Busy {
                operation: "desktop_profile_selection".into(),
            })?;
        let outcome = session
            .run(CodingAgentOperation::SetDefaultAgentProfile { profile_id })
            .await?;
        if !matches!(
            outcome,
            CodingAgentOperationOutcome::DefaultAgentProfileChanged
        ) {
            return Err(DesktopBridgeError::Session {
                message: "desktop profile selection returned an unexpected outcome".into(),
            });
        }
        Ok(self.metadata_snapshot())
    }

    pub(super) async fn replace_with_new_session(&mut self) -> Result<(), DesktopBridgeError> {
        let replacement = self.context.create_session().await?;
        self.shutdown_idle_session().await?;
        self.session = Some(replacement);
        Ok(())
    }

    pub(super) async fn replace_with_open_session(
        &mut self,
        session_id: String,
    ) -> Result<(), DesktopBridgeError> {
        if self
            .session
            .as_ref()
            .is_some_and(|session| session.view().session_id == session_id)
        {
            return Ok(());
        }
        let replacement = self.context.open_session(session_id).await?;
        self.shutdown_idle_session().await?;
        self.session = Some(replacement);
        Ok(())
    }

    pub(super) fn start_prompt(
        &mut self,
        command_id: u64,
        prompt: String,
        thinking_level: Option<CodingAgentThinkingLevel>,
    ) -> Result<ActivePrompt, DesktopBridgeError> {
        #[cfg(test)]
        if std::mem::take(&mut self.fail_next_prompt_start) {
            return Err(DesktopBridgeError::Session {
                message: "injected desktop prompt start failure".into(),
            });
        }
        let mut session = self
            .session
            .take()
            .ok_or_else(|| DesktopBridgeError::Busy {
                operation: "desktop_prompt".into(),
            })?;
        let connection = match session.connect(CodingAgentClientId::new(DESKTOP_CLIENT_ID)) {
            Ok(connection) => connection,
            Err(error) => {
                self.session = Some(session);
                return Err(error.into());
            }
        };
        let operation = self
            .context
            .prompt_operation(PromptInvocation::Text(prompt.clone()), thinking_level);
        let draft_id = CodingAgentDraftId(format!("desktop-prompt-{command_id}"));
        let submission = match connection.prepare_client_submission(
            &mut session,
            Some(CodingAgentSubmissionDraft::new(draft_id, prompt)),
            operation,
        ) {
            Ok(submission) => submission,
            Err(error) => {
                let _ = connection.detach();
                self.session = Some(session);
                return Err(error.into());
            }
        };
        let requested_after = match connection.state() {
            Ok(snapshot) => snapshot.cursor.last_event_sequence,
            Err(error) => {
                let cleanup = submission.discard(&mut session);
                let _ = connection.detach();
                self.session = Some(session);
                if let Err(cleanup) = cleanup {
                    return Err(cleanup.into());
                }
                return Err(error.into());
            }
        };
        let (events, pending_recovery) = match reconnect_event_source(&connection, requested_after)
        {
            Ok(reconnect) => reconnect,
            Err(error) => {
                let cleanup = submission.discard(&mut session);
                let _ = connection.detach();
                self.session = Some(session);
                if let Err(cleanup) = cleanup {
                    return Err(cleanup.into());
                }
                return Err(error);
            }
        };
        let project = self.context.snapshot().clone();
        let task = task::spawn(async move {
            let result = submission
                .run(&mut session)
                .await
                .map_err(DesktopBridgeError::from);
            (session, result)
        });
        Ok(ActivePrompt {
            command_id,
            operation_id: None,
            project,
            connection,
            events,
            pending_recovery,
            last_forwarded_sequence: requested_after,
            task,
        })
    }

    async fn shutdown_idle_session(&mut self) -> Result<(), DesktopBridgeError> {
        if let Some(mut session) = self.session.take() {
            session.shutdown().await?;
        }
        Ok(())
    }
}

pub(super) type PromptTaskOutput = (
    CodingAgentSession,
    Result<CodingAgentOperationOutcome, DesktopBridgeError>,
);

pub(super) struct ActivePrompt {
    pub(super) command_id: u64,
    pub(super) operation_id: Option<String>,
    pub(super) project: CodingAgentEmbeddingSnapshot,
    pub(super) connection: CodingAgentClientConnection,
    pub(super) events: DesktopProductEventSource,
    pub(super) pending_recovery: Option<CodingAgentFreshSnapshotRecovery>,
    pub(super) last_forwarded_sequence: u64,
    pub(super) task: task::JoinHandle<PromptTaskOutput>,
}

enum ActiveSignal {
    Command(Option<DesktopRuntimeCommand>),
    Event(Box<Result<CodingAgentReconnectDelivery, DesktopBridgeError>>),
    Finished(Box<Result<PromptTaskOutput, task::JoinError>>),
    Shutdown,
}

pub(super) async fn run_runtime(
    options: CodingAgentEmbeddingOptions,
    mut commands: mpsc::Receiver<DesktopRuntimeCommand>,
    mut shutdown: watch::Receiver<bool>,
    priority_updates: mpsc::Sender<DesktopRuntimeUpdate>,
    data_updates: mpsc::Sender<DesktopRuntimeUpdate>,
    ready: std_mpsc::SyncSender<Result<DesktopRuntimeReadySnapshot, DesktopRuntimeError>>,
) {
    let context = match CodingAgentEmbeddingContext::load(options) {
        Ok(context) => context,
        Err(error) => {
            let _ = ready.send(Err(runtime_error(&error)));
            return;
        }
    };
    let mut state = RuntimeState {
        context,
        session: None,
        #[cfg(test)]
        fail_next_prompt_start: false,
    };
    let initial = DesktopRuntimeReadySnapshot {
        project: state.context.snapshot().clone(),
    };
    if ready.send(Ok(initial)).is_err() {
        let _ = state.shutdown_idle_session().await;
        return;
    }

    let mut active: Option<ActivePrompt> = None;
    loop {
        if let Some(active_prompt) = active.as_mut() {
            if let Some(recovery) = active_prompt.pending_recovery.take() {
                active_prompt.last_forwarded_sequence = recovery.fresh_cursor.last_event_sequence;
                if priority_updates
                    .send(recovery_update(recovery))
                    .await
                    .is_err()
                {
                    shutdown_active_prompt(active.take(), &priority_updates).await;
                    return;
                }
            }
            let signal = tokio::select! {
                biased;
                changed = shutdown.changed() => {
                    let _ = changed;
                    ActiveSignal::Shutdown
                }
                command = commands.recv() => ActiveSignal::Command(command),
                result = &mut active_prompt.task => ActiveSignal::Finished(Box::new(result)),
                event = recv_product_event(&mut active_prompt.events) => {
                    ActiveSignal::Event(Box::new(event))
                },
            };
            match signal {
                ActiveSignal::Shutdown | ActiveSignal::Command(None) => {
                    shutdown_active_prompt(active.take(), &priority_updates).await;
                    break;
                }
                ActiveSignal::Command(Some(command)) => {
                    let update = dispatch_active_command(
                        active.as_ref().expect("active prompt exists"),
                        command,
                    );
                    if priority_updates.send(update).await.is_err() {
                        shutdown_active_prompt(active.take(), &priority_updates).await;
                        return;
                    }
                }
                ActiveSignal::Event(event) => match *event {
                    Ok(CodingAgentReconnectDelivery::Event(event)) => {
                        let active_prompt = active.as_mut().expect("active prompt exists");
                        let sequence = event.sequence();
                        let candidate_operation_id = event.operation_id().map(str::to_owned);
                        if !ensure_operation_started(
                            active_prompt,
                            candidate_operation_id.as_deref(),
                            &priority_updates,
                        )
                        .await
                        {
                            shutdown_active_prompt(active.take(), &priority_updates).await;
                            return;
                        }
                        if !publish_product_event(
                            event,
                            active_prompt,
                            &priority_updates,
                            &data_updates,
                        )
                        .await
                        {
                            shutdown_active_prompt(active.take(), &priority_updates).await;
                            return;
                        }
                        if !acknowledge_product_event(active_prompt, sequence, &priority_updates)
                            .await
                        {
                            shutdown_active_prompt(active.take(), &priority_updates).await;
                            return;
                        }
                        active_prompt.last_forwarded_sequence = sequence;
                    }
                    Ok(CodingAgentReconnectDelivery::FreshSnapshotRequired(recovery)) => {
                        let active_prompt = active.as_mut().expect("active prompt exists");
                        active_prompt.last_forwarded_sequence =
                            recovery.fresh_cursor.last_event_sequence;
                        if priority_updates
                            .send(recovery_update(recovery))
                            .await
                            .is_err()
                        {
                            shutdown_active_prompt(active.take(), &priority_updates).await;
                            return;
                        }
                    }
                    Err(error) => {
                        let active_prompt = active.as_mut().expect("active prompt exists");
                        if !recover_product_event_source(active_prompt, error, &priority_updates)
                            .await
                        {
                            shutdown_active_prompt(active.take(), &priority_updates).await;
                            return;
                        }
                    }
                },
                ActiveSignal::Finished(result) => {
                    let result = *result;
                    let mut completed = active.take().expect("active prompt exists");
                    if !drain_product_events(&mut completed, &priority_updates, &data_updates).await
                    {
                        shutdown_active_prompt(Some(completed), &priority_updates).await;
                        return;
                    }
                    let _ = completed.connection.detach();
                    match result {
                        Ok((session, operation_result)) => {
                            state.session = Some(session);
                            if !ensure_operation_started(&mut completed, None, &priority_updates)
                                .await
                            {
                                break;
                            }
                            let Some(operation_id) = completed.operation_id.take() else {
                                let _ = priority_updates
                                    .send(DesktopRuntimeUpdate::RuntimeFailed {
                                        error: DesktopRuntimeError {
                                            code: "operation_association_missing".into(),
                                            message: "completed desktop prompt has no product operation id"
                                                .into(),
                                        },
                                    })
                                    .await;
                                break;
                            };
                            let snapshot = match state.snapshot() {
                                Ok(snapshot) => snapshot,
                                Err(error) => {
                                    let _ = priority_updates
                                        .send(DesktopRuntimeUpdate::RuntimeFailed {
                                            error: runtime_error(&error),
                                        })
                                        .await;
                                    break;
                                }
                            };
                            let error = operation_result.err().map(|error| runtime_error(&error));
                            if priority_updates
                                .send(DesktopRuntimeUpdate::PromptFinished {
                                    command_id: completed.command_id,
                                    operation_id,
                                    snapshot,
                                    error,
                                })
                                .await
                                .is_err()
                            {
                                break;
                            }
                        }
                        Err(_) => {
                            let _ = priority_updates
                                .send(DesktopRuntimeUpdate::RuntimeFailed {
                                    error: local_runtime_error(
                                        "runtime_task_panicked",
                                        "A desktop runtime task stopped unexpectedly.",
                                    ),
                                })
                                .await;
                            break;
                        }
                    }
                }
            }
            continue;
        }

        let command = tokio::select! {
            biased;
            changed = shutdown.changed() => {
                let _ = changed;
                None
            }
            command = commands.recv() => command,
        };
        let Some(command) = command else {
            break;
        };
        let update = dispatch_idle_command(&mut state, &mut active, command).await;
        if priority_updates.send(update).await.is_err() {
            break;
        }
    }

    let _ = state.shutdown_idle_session().await;
    let _ = priority_updates.send(DesktopRuntimeUpdate::Stopped).await;
}

async fn recv_product_event(
    receiver: &mut DesktopProductEventSource,
) -> Result<CodingAgentReconnectDelivery, DesktopBridgeError> {
    receiver.recv().await
}

pub(super) struct DesktopProductEventSource {
    pub(super) replay: VecDeque<CodingAgentProductEvent>,
    pub(super) receiver: DesktopProductEventReceiver,
}

pub(super) enum DesktopProductEventReceiver {
    Product(CodingAgentReconnectReceiver),
    #[cfg(test)]
    Injected(mpsc::Receiver<Result<CodingAgentReconnectDelivery, DesktopBridgeError>>),
}

impl DesktopProductEventReceiver {
    async fn recv(&mut self) -> Result<CodingAgentReconnectDelivery, DesktopBridgeError> {
        match self {
            Self::Product(receiver) => receiver.recv().await.map_err(DesktopBridgeError::from),
            #[cfg(test)]
            Self::Injected(receiver) => receiver
                .recv()
                .await
                .unwrap_or_else(|| Err(DesktopBridgeError::cancelled_for_tests())),
        }
    }

    fn try_recv(&mut self) -> Result<Option<CodingAgentReconnectDelivery>, DesktopBridgeError> {
        match self {
            Self::Product(receiver) => receiver.try_recv().map_err(DesktopBridgeError::from),
            #[cfg(test)]
            Self::Injected(receiver) => match receiver.try_recv() {
                Ok(delivery) => delivery.map(Some),
                Err(mpsc::error::TryRecvError::Empty) => Ok(None),
                Err(mpsc::error::TryRecvError::Disconnected) => {
                    Err(DesktopBridgeError::cancelled_for_tests())
                }
            },
        }
    }
}

impl DesktopProductEventSource {
    pub(super) async fn recv(
        &mut self,
    ) -> Result<CodingAgentReconnectDelivery, DesktopBridgeError> {
        if let Some(event) = self.replay.pop_front() {
            return Ok(CodingAgentReconnectDelivery::Event(event));
        }
        self.receiver.recv().await
    }

    fn try_recv(&mut self) -> Result<Option<CodingAgentReconnectDelivery>, DesktopBridgeError> {
        if let Some(event) = self.replay.pop_front() {
            return Ok(Some(CodingAgentReconnectDelivery::Event(event)));
        }
        self.receiver.try_recv()
    }
}

pub(super) enum DesktopReconnectAttempt<R> {
    Replayed {
        events: Vec<CodingAgentProductEvent>,
        receiver: R,
    },
    FreshSnapshotRequired(CodingAgentFreshSnapshotRecovery),
}

pub(super) fn establish_reconnect<R>(
    requested_after: u64,
    mut reconnect: impl FnMut(u64) -> Result<DesktopReconnectAttempt<R>, DesktopBridgeError>,
) -> Result<
    (
        Vec<CodingAgentProductEvent>,
        R,
        Option<CodingAgentFreshSnapshotRecovery>,
    ),
    DesktopBridgeError,
> {
    match reconnect(requested_after)? {
        DesktopReconnectAttempt::Replayed { events, receiver } => Ok((events, receiver, None)),
        DesktopReconnectAttempt::FreshSnapshotRequired(recovery) => {
            let fresh_sequence = recovery.fresh_cursor.last_event_sequence;
            match reconnect(fresh_sequence)? {
                DesktopReconnectAttempt::Replayed { events, receiver } => {
                    Ok((events, receiver, Some(recovery)))
                }
                DesktopReconnectAttempt::FreshSnapshotRequired(second) => {
                    Err(DesktopBridgeError::Input {
                        message: format!(
                            "desktop ProductEvent reconnect exhausted after fresh cursor {} \
                             (oldest retained sequence {})",
                            second.requested_sequence, second.oldest_available_sequence
                        ),
                    })
                }
            }
        }
    }
}

pub(super) fn reconnect_event_source(
    connection: &CodingAgentClientConnection,
    requested_after: u64,
) -> Result<
    (
        DesktopProductEventSource,
        Option<CodingAgentFreshSnapshotRecovery>,
    ),
    DesktopBridgeError,
> {
    let (events, receiver, recovery) = establish_reconnect(requested_after, |sequence| {
        connection
            .reconnect(sequence)
            .map(|reconnect| match reconnect {
                CodingAgentReconnect::Replayed {
                    events, receiver, ..
                } => DesktopReconnectAttempt::Replayed { events, receiver },
                CodingAgentReconnect::FreshSnapshotRequired(recovery) => {
                    DesktopReconnectAttempt::FreshSnapshotRequired(recovery)
                }
            })
            .map_err(DesktopBridgeError::from)
    })?;
    Ok((
        DesktopProductEventSource {
            replay: events.into(),
            receiver: DesktopProductEventReceiver::Product(receiver),
        },
        recovery,
    ))
}

async fn recover_product_event_source(
    active: &mut ActivePrompt,
    receiver_error: DesktopBridgeError,
    priority_updates: &mpsc::Sender<DesktopRuntimeUpdate>,
) -> bool {
    match reconnect_event_source(&active.connection, active.last_forwarded_sequence) {
        Ok((events, recovery)) => {
            active.events = events;
            if let Some(recovery) = recovery {
                active.last_forwarded_sequence = recovery.fresh_cursor.last_event_sequence;
                priority_updates
                    .send(recovery_update(recovery))
                    .await
                    .is_ok()
            } else {
                true
            }
        }
        Err(reconnect_error) => priority_updates
            .send(DesktopRuntimeUpdate::RuntimeFailed {
                error: DesktopRuntimeError {
                    code: "product_event_reconnect_failed".into(),
                    message: format!(
                        "ProductEvent receiver failed ({}); reconnect from sequence {} failed: {}",
                        receiver_error, active.last_forwarded_sequence, reconnect_error
                    ),
                },
            })
            .await
            .is_ok(),
    }
}

pub(super) fn recovery_update(recovery: CodingAgentFreshSnapshotRecovery) -> DesktopRuntimeUpdate {
    let reason = match recovery.reason {
        CodingAgentRecoveryReason::RetainedHistoryGap => DesktopRuntimeError {
            code: "product_event_retained_history_gap".into(),
            message: format!(
                "ProductEvent replay after sequence {} is unavailable; oldest retained sequence is {}",
                recovery.requested_sequence, recovery.oldest_available_sequence
            ),
        },
        CodingAgentRecoveryReason::LiveReceiverLag => DesktopRuntimeError {
            code: "product_event_live_receiver_lag".into(),
            message: format!(
                "ProductEvent receiver lagged after sequence {}; recovered at fresh sequence {}",
                recovery.requested_sequence, recovery.fresh_cursor.last_event_sequence
            ),
        },
    };
    DesktopRuntimeUpdate::ResyncRequired {
        reason,
        snapshot: *recovery.snapshot,
    }
}

async fn publish_product_event(
    event: CodingAgentProductEvent,
    active: &ActivePrompt,
    priority_updates: &mpsc::Sender<DesktopRuntimeUpdate>,
    data_updates: &mpsc::Sender<DesktopRuntimeUpdate>,
) -> bool {
    if event.family() == CodingAgentProductEventFamily::Capability {
        let snapshot = match active.connection.state() {
            Ok(snapshot) => snapshot,
            Err(error) => {
                return priority_updates
                    .send(DesktopRuntimeUpdate::RuntimeFailed {
                        error: runtime_error(&error),
                    })
                    .await
                    .is_ok();
            }
        };
        return priority_updates
            .send(DesktopRuntimeUpdate::ResyncRequired {
                reason: DesktopRuntimeError {
                    code: "capability_generation_changed".into(),
                    message: format!(
                        "capability generation changed at ProductEvent sequence {}; replacing the desktop projection atomically",
                        event.sequence()
                    ),
                },
                snapshot,
            })
            .await
            .is_ok();
    }
    if is_priority_event(&event) {
        return priority_updates
            .send(DesktopRuntimeUpdate::ProductEvent { event })
            .await
            .is_ok();
    }
    publish_data_update(
        DesktopRuntimeUpdate::ProductEvent { event },
        || active.connection.state(),
        priority_updates,
        data_updates,
    )
    .await
}

async fn acknowledge_product_event(
    active: &ActivePrompt,
    sequence: u64,
    priority_updates: &mpsc::Sender<DesktopRuntimeUpdate>,
) -> bool {
    match active.connection.acknowledge(sequence) {
        Ok(_) => true,
        Err(error) => priority_updates
            .send(DesktopRuntimeUpdate::RuntimeFailed {
                error: runtime_error(&error),
            })
            .await
            .is_ok(),
    }
}

pub(super) async fn publish_data_update<E>(
    update: DesktopRuntimeUpdate,
    snapshot: impl FnOnce() -> Result<CodingAgentSnapshot, E>,
    priority_updates: &mpsc::Sender<DesktopRuntimeUpdate>,
    data_updates: &mpsc::Sender<DesktopRuntimeUpdate>,
) -> bool
where
    E: DesktopRuntimeErrorSource,
{
    match data_updates.try_send(update) {
        Ok(()) => true,
        Err(mpsc::error::TrySendError::Closed(_)) => false,
        Err(mpsc::error::TrySendError::Full(_)) => {
            let snapshot = match snapshot() {
                Ok(snapshot) => snapshot,
                Err(error) => {
                    return priority_updates
                        .send(DesktopRuntimeUpdate::RuntimeFailed {
                            error: runtime_error(&error),
                        })
                        .await
                        .is_ok();
                }
            };
            priority_updates
                .send(DesktopRuntimeUpdate::ResyncRequired {
                    reason: DesktopRuntimeError {
                        code: "desktop_data_queue_full".into(),
                        message: format!(
                            "desktop message update queue reached its {}-event bound",
                            DESKTOP_UPDATE_QUEUE_CAPACITY
                        ),
                    },
                    snapshot,
                })
                .await
                .is_ok()
        }
    }
}

async fn ensure_operation_started(
    active: &mut ActivePrompt,
    candidate_operation_id: Option<&str>,
    priority_updates: &mpsc::Sender<DesktopRuntimeUpdate>,
) -> bool {
    if active.operation_id.is_some() {
        return true;
    }
    let snapshot = match active.connection.state() {
        Ok(snapshot) => snapshot,
        Err(error) => {
            let _ = priority_updates
                .send(DesktopRuntimeUpdate::RuntimeFailed {
                    error: runtime_error(&error),
                })
                .await;
            return false;
        }
    };
    let operation_id = snapshot
        .submitted_operation
        .as_ref()
        .map(|operation| operation.operation_id.clone())
        .or_else(|| candidate_operation_id.map(str::to_owned));
    let Some(operation_id) = operation_id else {
        return true;
    };
    active.operation_id = Some(operation_id.clone());
    priority_updates
        .send(DesktopRuntimeUpdate::PromptStarted {
            command_id: active.command_id,
            operation_id,
            metadata: DesktopRuntimeMetadataSnapshot {
                project: active.project.clone(),
                session: Some(snapshot),
            },
        })
        .await
        .is_ok()
}

async fn drain_product_events(
    active: &mut ActivePrompt,
    priority_updates: &mpsc::Sender<DesktopRuntimeUpdate>,
    data_updates: &mpsc::Sender<DesktopRuntimeUpdate>,
) -> bool {
    loop {
        let received = active.events.try_recv();
        match received {
            Ok(Some(CodingAgentReconnectDelivery::Event(event))) => {
                let sequence = event.sequence();
                let candidate_operation_id = event.operation_id().map(str::to_owned);
                if !ensure_operation_started(
                    active,
                    candidate_operation_id.as_deref(),
                    priority_updates,
                )
                .await
                {
                    return false;
                }
                if !publish_product_event(event, active, priority_updates, data_updates).await {
                    return false;
                }
                if !acknowledge_product_event(active, sequence, priority_updates).await {
                    return false;
                }
                active.last_forwarded_sequence = sequence;
            }
            Ok(Some(CodingAgentReconnectDelivery::FreshSnapshotRequired(recovery))) => {
                active.last_forwarded_sequence = recovery.fresh_cursor.last_event_sequence;
                if priority_updates
                    .send(recovery_update(recovery))
                    .await
                    .is_err()
                {
                    return false;
                }
            }
            Ok(None) => return true,
            Err(error) => {
                return recover_product_event_source(active, error, priority_updates).await;
            }
        }
    }
}

fn is_priority_event(event: &CodingAgentProductEvent) -> bool {
    !matches!(
        (event.delivery_class(), event.family()),
        (
            CodingAgentProductEventDeliveryClass::Data,
            CodingAgentProductEventFamily::Message
        )
    )
}

async fn shutdown_active_prompt(
    active: Option<ActivePrompt>,
    priority_updates: &mpsc::Sender<DesktopRuntimeUpdate>,
) {
    shutdown_active_prompt_with_deadline(active, priority_updates, RUNTIME_SHUTDOWN_DEADLINE).await;
}

pub(super) async fn shutdown_active_prompt_with_deadline(
    active: Option<ActivePrompt>,
    priority_updates: &mpsc::Sender<DesktopRuntimeUpdate>,
    shutdown_deadline: Duration,
) {
    let Some(mut active) = active else {
        return;
    };
    let operation_id = active.operation_id.clone().or_else(|| {
        active
            .connection
            .state()
            .ok()
            .and_then(|snapshot| snapshot.submitted_operation)
            .map(|operation| operation.operation_id)
    });
    if let Some(operation_id) = operation_id.as_deref() {
        let control = active.connection.prompt_control(operation_id);
        let _ = control.abort(
            CodingAgentControlId("desktop-runtime-shutdown".into()),
            "desktop runtime shutdown",
        );
    }
    match tokio::time::timeout(shutdown_deadline, &mut active.task).await {
        Ok(Ok((mut session, _))) => {
            let _ = session.shutdown().await;
        }
        Ok(Err(_)) => {
            let _ = priority_updates
                .send(DesktopRuntimeUpdate::RuntimeFailed {
                    error: local_runtime_error(
                        "runtime_task_panicked",
                        "A desktop runtime task stopped unexpectedly.",
                    ),
                })
                .await;
        }
        Err(_) => {
            active.task.abort();
            let _ = active.task.await;
            let _ = priority_updates
                .send(DesktopRuntimeUpdate::RuntimeFailed {
                    error: DesktopRuntimeError {
                        code: "shutdown_deadline_exceeded".into(),
                        message: format!(
                            "prompt operation {} did not stop within {} seconds",
                            operation_id.as_deref().unwrap_or("<starting>"),
                            shutdown_deadline.as_secs_f64()
                        ),
                    },
                })
                .await;
        }
    }
    let _ = active.connection.detach();
}
