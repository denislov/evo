use crate::error::CliError;
use crate::protocol::types::{
    ProtocolEvent, RpcResponse, RpcShutdownLifecycleEvent, RpcShutdownResponse, RpcShutdownStatus,
    StreamingBehavior,
};
use crate::rpc::commands::{has_images, rpc_pending_delegation_confirmation};
use crate::rpc::event_queue::RpcQueuedProductEvent;
use crate::rpc::events::RpcCodingEventAdapter;
use crate::rpc::state::{
    CodingOperationOutcome, CodingOperationTaskResult, OperationKind, ProductEventSequence,
    RpcBackgroundCompletion, RpcBackgroundOperation, RpcForegroundOperation, RpcIdempotencyKey,
    RpcState,
};
use crate::rpc::wire::{write_json_line, write_rpc_response};
use coding_agent::api::client::{
    CodingAgentControlId, CodingAgentDraft, CodingAgentDraftId, CodingAgentDraftKind,
    CodingAgentReconnect, CodingAgentSnapshotCursor, CodingAgentSubmissionDraft,
};
use coding_agent::api::embedding::{CodingAgentPreparedPrompt, CodingAgentPromptImage};
use coding_agent::api::error::CodingAgentPublicError;
use coding_agent::api::event::CodingAgentProductEvent;
use coding_agent::api::operation::{CodingAgentOperation, PromptTurnOutcome};
use coding_agent::api::runtime::{CodingAgentSession, CodingAgentShutdownOutcome};
use coding_agent::api::view::{ProfileId, ProfileKind, SessionStorageHandle};
use tokio::io::AsyncWrite;
use tokio::sync::oneshot;

use super::limits::{RPC_QUEUED_CONTROL_BYTE_LIMIT, RPC_QUEUED_CONTROL_ITEM_LIMIT};
use super::{rpc_cli_error, rpc_public_error};

mod start;

impl RpcState {
    pub(super) async fn write_product_event<W>(
        &mut self,
        event: CodingAgentProductEvent,
        writer: &mut W,
    ) -> Result<(), CliError>
    where
        W: AsyncWrite + Unpin,
    {
        let pushed = push_live_product_event(
            &mut self.event_adapter,
            &mut self.adapter_applied_sequence,
            &event,
        );
        for protocol_event in pushed.protocol_events {
            write_json_line(writer, &protocol_event).await?;
        }
        self.acknowledge_delivered_product_event(&event)?;
        Ok(())
    }

    fn acknowledge_delivered_product_event(
        &self,
        event: &CodingAgentProductEvent,
    ) -> Result<(), CliError> {
        let Some(connection) = &self.client_connection else {
            return Ok(());
        };
        match connection.acknowledge(event.sequence()) {
            Ok(_) => Ok(()),
            Err(error) if error.code() == "runtime_shut_down" => Ok(()),
            Err(error) => Err(error.into()),
        }
    }

    pub(super) async fn finish_coding_running_prompt<W>(
        &mut self,
        result: Result<CodingOperationTaskResult, oneshot::error::RecvError>,
        writer: &mut W,
    ) -> Result<(), CliError>
    where
        W: AsyncWrite + Unpin,
    {
        let Some(running) = self.foreground.take() else {
            return Ok(());
        };
        if running.operation_kind == OperationKind::Compact {
            self.is_compacting = false;
        }
        self.mark_idempotency_complete(running.idempotency_key.as_ref());
        self.drain_session_product_events(writer).await?;

        let result = result.map_err(|error| {
            CliError::AgentFailure(format!(
                "coding agent task ended before reporting completion: {error}"
            ))
        })?;

        let consumed_prompt_queue = matches!(&result.outcome, CodingOperationOutcome::Prompt(_));
        let outcome = match &result.outcome {
            CodingOperationOutcome::Prompt(outcome) => {
                if let Ok(outcome) = outcome {
                    self.active_leaf_id = prompt_outcome_leaf_id(outcome).map(ToString::to_string);
                    self.active_session_storage = result.session_storage.clone();
                }
                outcome.as_ref().map(|_| ()).map_err(Clone::clone)
            }
            CodingOperationOutcome::Compact(outcome) => {
                if let Ok(outcome) = outcome {
                    self.active_leaf_id = prompt_outcome_leaf_id(outcome).map(ToString::to_string);
                    self.active_session_storage = result.session_storage.clone();
                }
                outcome.as_ref().map(|_| ()).map_err(Clone::clone)
            }
            CodingOperationOutcome::AgentInvocation(outcome) => {
                outcome.as_ref().map(|_| ()).map_err(Clone::clone)
            }
            CodingOperationOutcome::AgentTeam(outcome) => {
                outcome.as_ref().map(|_| ()).map_err(Clone::clone)
            }
            CodingOperationOutcome::DelegationApproval(outcome) => {
                outcome.as_ref().map(|_| ()).map_err(Clone::clone)
            }
        };

        let session = match result.session {
            Some(session) => session,
            None => self.coding_session.take().ok_or_else(|| {
                CliError::AgentFailure(
                    "runtime-owned operation completed without a retained session".into(),
                )
            })?,
        };
        self.coding_session = Some(session);
        if consumed_prompt_queue {
            self.steering.clear();
            self.follow_up.clear();
            if let Some(connection) = &self.client_connection {
                let _ = connection.clear_control_drafts();
            }
        }
        self.finish_pending_shutdown_if_idle(writer).await?;
        match outcome {
            Err(CliError::SessionFailure(message)) if message == "cancelled" => Ok(()),
            outcome => outcome,
        }
    }

    pub(super) async fn finish_background_operation<W>(
        &mut self,
        completion: RpcBackgroundCompletion,
        writer: &mut W,
    ) -> Result<(), CliError>
    where
        W: AsyncWrite + Unpin,
    {
        self.drain_session_product_events(writer).await?;
        let Some(operation) = self.background_operations.remove(&completion.operation_id) else {
            return Err(CliError::AgentFailure(format!(
                "background operation completed without registry ownership: {}",
                completion.operation_id
            )));
        };
        self.mark_idempotency_complete(operation.idempotency_key.as_ref());

        let outcome = match &completion.result.outcome {
            CodingOperationOutcome::AgentInvocation(outcome)
                if operation.operation_kind == OperationKind::AgentInvocation =>
            {
                outcome.as_ref().map(|_| ()).map_err(Clone::clone)
            }
            CodingOperationOutcome::AgentTeam(outcome)
                if operation.operation_kind == OperationKind::AgentTeam =>
            {
                outcome.as_ref().map(|_| ()).map_err(Clone::clone)
            }
            _ => Err(CliError::AgentFailure(format!(
                "background operation {} completed with a mismatched outcome",
                completion.operation_id
            ))),
        };

        self.finish_pending_shutdown_if_idle(writer).await?;
        match outcome {
            Err(CliError::SessionFailure(message)) if message == "cancelled" => Ok(()),
            outcome => outcome,
        }
    }

    pub(super) async fn drain_session_product_events<W>(
        &mut self,
        writer: &mut W,
    ) -> Result<(), CliError>
    where
        W: AsyncWrite + Unpin,
    {
        while let Some(events) = self.session_events.as_mut() {
            let Ok(item) = events.try_recv() else {
                break;
            };
            match item {
                RpcQueuedProductEvent::Event(event) => {
                    let pushed = push_live_product_event(
                        &mut self.event_adapter,
                        &mut self.adapter_applied_sequence,
                        &event,
                    );
                    for protocol_event in pushed.protocol_events {
                        write_json_line(writer, &protocol_event).await?;
                    }
                    self.acknowledge_delivered_product_event(&event)?;
                }
                RpcQueuedProductEvent::Overflow { skipped } => {
                    write_rpc_response(
                        writer,
                        RpcResponse::error_with_data(
                            None,
                            "event_stream",
                            format!(
                                "event stream lagged by {skipped} events; client must request a fresh UI snapshot"
                            ),
                            serde_json::json!({
                                "code": "event_stream_lag",
                                "skipped": skipped,
                                "recovery": "fresh_snapshot"
                            }),
                        ),
                    )
                    .await?;
                    self.session_events_closed = true;
                    break;
                }
            }
        }
        Ok(())
    }

    async fn finish_pending_shutdown_if_idle<W>(&mut self, writer: &mut W) -> Result<(), CliError>
    where
        W: AsyncWrite + Unpin,
    {
        if self.has_active_operations() {
            return Ok(());
        }
        self.active_shutdown_handle = None;
        let Some(id) = self.pending_shutdown_response.take() else {
            return Ok(());
        };
        let mut session = self.coding_session.take().ok_or_else(|| {
            CliError::AgentFailure(
                "runtime operations drained without returning the owned session".into(),
            )
        })?;
        let status = match session.shutdown().await? {
            CodingAgentShutdownOutcome::ShutDown => RpcShutdownStatus::ShutDown,
            CodingAgentShutdownOutcome::AlreadyShutDown => RpcShutdownStatus::AlreadyShutDown,
        };
        if status == RpcShutdownStatus::ShutDown {
            write_json_line(writer, &RpcShutdownLifecycleEvent { status }).await?;
        }
        write_rpc_response(
            writer,
            RpcResponse::success(
                id,
                "shutdown",
                Some(
                    serde_json::to_value(RpcShutdownResponse { status })
                        .expect("shutdown response serializes"),
                ),
            ),
        )
        .await?;
        self.coding_session = Some(session);
        Ok(())
    }

    pub(super) async fn take_or_open_coding_session(
        &mut self,
    ) -> Result<(CodingAgentSession, Option<SessionStorageHandle>), CliError> {
        let session = match self.coding_session.take() {
            Some(session) => session,
            None => self.application.session_bootstrap.open().await?,
        };
        let storage = session_runtime_storage(&session)?;
        Ok((session, storage))
    }

    pub(super) async fn emit_queue_update<W>(&self, writer: &mut W) -> Result<(), CliError>
    where
        W: AsyncWrite + Unpin,
    {
        write_json_line(
            writer,
            &ProtocolEvent::queue_update(
                self.steering
                    .iter()
                    .map(CodingAgentPreparedPrompt::display_text)
                    .map(str::to_owned)
                    .collect(),
                self.follow_up
                    .iter()
                    .map(CodingAgentPreparedPrompt::display_text)
                    .map(str::to_owned)
                    .collect(),
            ),
        )
        .await
    }
}

fn queued_control_retained_bytes(input: &CodingAgentPreparedPrompt) -> Option<usize> {
    Some(input.retained_bytes())
}

pub(super) async fn flush_session_product_events(
    flush: tokio::sync::mpsc::Sender<oneshot::Sender<()>>,
) {
    let (acknowledge, acknowledged) = oneshot::channel();
    if flush.send(acknowledge).await.is_ok() {
        let _ = acknowledged.await;
    }
}

pub(super) async fn reconnect_running_prompt_after(
    state: &mut RpcState,
    cursor: &CodingAgentSnapshotCursor,
) -> Result<Vec<ProtocolEvent>, CodingAgentPublicError> {
    let Some(connection) = state.client_connection.as_ref() else {
        return Ok(Vec::new());
    };
    let recovery = connection.reconnect_from_cursor(cursor)?;
    let (retained_events, through) = match recovery {
        CodingAgentReconnect::Replayed { events, cursor, .. } => {
            (events, cursor.last_event_sequence)
        }
        CodingAgentReconnect::FreshSnapshotRequired(recovery) => {
            return Err(recovery.into_public_error());
        }
    };

    let mut protocol_events = Vec::new();
    for event in retained_events {
        if event.sequence() <= state.adapter_applied_sequence.get() {
            continue;
        }
        let sequence = ProductEventSequence::new(event.sequence());
        protocol_events.extend(state.event_adapter.push_product_event(&event));
        state.adapter_applied_sequence = state.adapter_applied_sequence.max(sequence);
    }
    connection.acknowledge(through)?;
    state.session_events_closed = false;
    Ok(protocol_events)
}

struct LiveProductEventPush {
    protocol_events: Vec<ProtocolEvent>,
}

fn push_live_product_event(
    adapter: &mut RpcCodingEventAdapter,
    applied_sequence: &mut ProductEventSequence,
    event: &CodingAgentProductEvent,
) -> LiveProductEventPush {
    let sequence = ProductEventSequence::new(event.sequence());
    if sequence <= *applied_sequence {
        return LiveProductEventPush {
            protocol_events: Vec::new(),
        };
    }
    let protocol_events = adapter.push_product_event(event);
    *applied_sequence = (*applied_sequence).max(sequence);
    LiveProductEventPush { protocol_events }
}

fn session_runtime_storage(
    session: &CodingAgentSession,
) -> Result<Option<SessionStorageHandle>, CliError> {
    session.session_storage().map_err(CliError::from)
}

fn prompt_outcome_leaf_id(outcome: &PromptTurnOutcome) -> Option<&str> {
    match outcome {
        PromptTurnOutcome::Success { leaf_id, .. } => leaf_id.as_deref(),
        PromptTurnOutcome::Aborted { .. } | PromptTurnOutcome::Failed { .. } => None,
    }
}
