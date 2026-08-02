use crate::error::CliError;
use crate::protocol::types::{RpcDetachStatus, RpcNegotiatedProtocolState};
use crate::rpc::event_queue::{RpcProductEventQueue, RpcProductEventReceiver};
use crate::rpc::events::RpcCodingEventAdapter;
use crate::rpc::limits::{
    RPC_BACKGROUND_OPERATION_LIMIT, RPC_EVENT_FLUSH_QUEUE_CAPACITY, RPC_IDEMPOTENCY_RECORD_LIMIT,
};
use coding_agent::api::client::{
    CodingAgentClientConnection, CodingAgentClientId, CodingAgentDetachOutcome, CodingAgentDraft,
    CodingAgentDraftId, CodingAgentDraftKind, CodingAgentOperationControl,
    CodingAgentPromptControl, UI_SNAPSHOT_PROTOCOL_VERSION,
};
use coding_agent::api::embedding::{
    CodingAgentApplicationStartup, CodingAgentModelCatalogEntry, CodingAgentPreparedPrompt,
    CodingAgentThinkingLevel,
};
use coding_agent::api::error::{CodingAgentErrorContext, CodingAgentPublicError};
use coding_agent::api::event::PRODUCT_EVENT_PROTOCOL_VERSION;
use coding_agent::api::operation::{AgentInvocationOutcome, AgentTeamOutcome, PromptTurnOutcome};
use coding_agent::api::runtime::{CodingAgentRuntimeShutdownHandle, CodingAgentSession};
use coding_agent::api::settings::CodingAgentQueueMode;
use coding_agent::api::view::SessionStorageHandle;
use std::collections::{HashMap, VecDeque};
use tokio::sync::{mpsc, oneshot};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum OperationKind {
    Prompt,
    Compact,
    AgentInvocation,
    AgentTeam,
    SelfHealingEdit,
    DelegationConfirmation,
}

impl OperationKind {
    pub(super) const fn as_str(self) -> &'static str {
        match self {
            Self::Prompt => "prompt",
            Self::Compact => "compact",
            Self::AgentInvocation => "agent_invocation",
            Self::AgentTeam => "agent_team",
            Self::SelfHealingEdit => "self_healing_edit",
            Self::DelegationConfirmation => "delegation_confirmation",
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct ProductEventSequence(u64);

impl ProductEventSequence {
    pub(super) const fn new(value: u64) -> Self {
        Self(value)
    }

    pub(super) const fn get(self) -> u64 {
        self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RpcIdempotencyRecord {
    pub(super) command: &'static str,
    pub(super) operation_kind: OperationKind,
    pub(super) completed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) struct RpcIdempotencyKey(String);

impl RpcIdempotencyKey {
    const MAX_LEN: usize = 128;

    fn parse(value: String) -> Result<Self, CliError> {
        let valid = !value.is_empty()
            && value.len() <= Self::MAX_LEN
            && value.bytes().all(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':')
            });
        valid.then_some(Self(value)).ok_or_else(|| {
            CliError::SessionFailure(
                "idempotency key must be 1-128 ASCII letters, digits, '-', '_', '.', or ':'".into(),
            )
        })
    }
}

pub(super) struct RpcState {
    pub(super) recovery_auth_token: Option<String>,
    pub(super) application: CodingAgentApplicationStartup,
    pub(super) model: CodingAgentModelCatalogEntry,
    pub(super) thinking_level: CodingAgentThinkingLevel,
    pub(super) steering_mode: CodingAgentQueueMode,
    pub(super) follow_up_mode: CodingAgentQueueMode,
    pub(super) auto_compaction_enabled: bool,
    pub(super) session_name: Option<String>,
    pub(super) active_session_storage: Option<SessionStorageHandle>,
    pub(super) active_leaf_id: Option<String>,
    pub(super) coding_session: Option<CodingAgentSession>,
    pub(super) client_connection: Option<CodingAgentClientConnection>,
    pub(super) session_event_stream_id: Option<String>,
    pub(super) session_events: Option<RpcProductEventReceiver>,
    pub(super) session_event_flush: Option<mpsc::Sender<oneshot::Sender<()>>>,
    pub(super) session_events_closed: bool,
    pub(super) event_adapter: RpcCodingEventAdapter,
    pub(super) adapter_applied_sequence: ProductEventSequence,
    pub(super) foreground: Option<RpcForegroundOperation>,
    pub(super) background_operations: HashMap<String, RpcBackgroundOperation>,
    pub(super) background_completion_tx: mpsc::Sender<RpcBackgroundCompletion>,
    pub(super) background_completion_rx: mpsc::Receiver<RpcBackgroundCompletion>,
    pub(super) active_shutdown_handle: Option<CodingAgentRuntimeShutdownHandle>,
    pub(super) pending_shutdown_response: Option<Option<String>>,
    pub(super) is_compacting: bool,
    pub(super) steering: Vec<CodingAgentPreparedPrompt>,
    pub(super) follow_up: Vec<CodingAgentPreparedPrompt>,
    pub(super) negotiated_protocol: RpcNegotiatedProtocolState,
    pub(super) idempotency_records: HashMap<RpcIdempotencyKey, RpcIdempotencyRecord>,
    pub(super) idempotency_order: VecDeque<RpcIdempotencyKey>,
}

pub(super) struct RpcForegroundOperation {
    pub(super) done: oneshot::Receiver<CodingOperationTaskResult>,
    pub(super) operation_kind: OperationKind,
    pub(super) idempotency_key: Option<RpcIdempotencyKey>,
}

pub(super) struct RpcBackgroundOperation {
    pub(super) operation_kind: OperationKind,
    pub(super) idempotency_key: Option<RpcIdempotencyKey>,
}

pub(super) struct RpcBackgroundCompletion {
    pub(super) operation_id: String,
    pub(super) result: CodingOperationTaskResult,
}

pub(super) struct CodingOperationTaskResult {
    pub(super) session: Option<CodingAgentSession>,
    pub(super) session_storage: Option<SessionStorageHandle>,
    pub(super) outcome: CodingOperationOutcome,
}

pub(super) enum CodingOperationOutcome {
    Prompt(Result<PromptTurnOutcome, CliError>),
    Compact(Result<PromptTurnOutcome, CliError>),
    AgentInvocation(Result<AgentInvocationOutcome, CliError>),
    AgentTeam(Result<AgentTeamOutcome, CliError>),
    DelegationApproval(Result<(), CliError>),
}

impl RpcState {
    pub(super) fn new(mut application: CodingAgentApplicationStartup) -> Result<Self, CliError> {
        let model = application.model_summary.clone();
        let event_adapter = RpcCodingEventAdapter::new_with_provider(
            model.api.clone(),
            model.provider.clone(),
            model.id.clone(),
        );
        let (background_completion_tx, background_completion_rx) =
            mpsc::channel(RPC_BACKGROUND_OPERATION_LIMIT);
        let auto_compaction_enabled = !application.session_bootstrap.is_persistent();
        application.configure_runtime_preferences(
            CodingAgentThinkingLevel::Off,
            CodingAgentQueueMode::OneAtATime,
            CodingAgentQueueMode::OneAtATime,
            auto_compaction_enabled,
        );
        Ok(Self {
            recovery_auth_token: std::env::var("EVO_RPC_AUTH_TOKEN")
                .ok()
                .filter(|v| !v.is_empty()),
            application,
            model,
            thinking_level: CodingAgentThinkingLevel::Off,
            steering_mode: CodingAgentQueueMode::OneAtATime,
            follow_up_mode: CodingAgentQueueMode::OneAtATime,
            auto_compaction_enabled,
            session_name: None,
            active_session_storage: None,
            active_leaf_id: None,
            coding_session: None,
            client_connection: None,
            session_event_stream_id: None,
            session_events: None,
            session_event_flush: None,
            session_events_closed: false,
            event_adapter,
            adapter_applied_sequence: ProductEventSequence::default(),
            foreground: None,
            background_operations: HashMap::new(),
            background_completion_tx,
            background_completion_rx,
            active_shutdown_handle: None,
            pending_shutdown_response: None,
            is_compacting: false,
            steering: Vec::new(),
            follow_up: Vec::new(),
            negotiated_protocol: RpcNegotiatedProtocolState {
                rpc: None,
                product_events: PRODUCT_EVENT_PROTOCOL_VERSION,
                ui_snapshot: UI_SNAPSHOT_PROTOCOL_VERSION,
            },
            idempotency_records: HashMap::new(),
            idempotency_order: VecDeque::new(),
        })
    }

    pub(super) fn authorize_recovery(&self, token: &str) -> Result<(), CliError> {
        match self.recovery_auth_token.as_deref() {
            Some(expected) if expected == token => Ok(()),
            _ => Err(CliError::SessionFailure(
                "recovery RPC requires a valid authorization token".into(),
            )),
        }
    }

    pub(super) fn is_streaming(&self) -> bool {
        self.foreground.is_some()
    }

    pub(super) fn has_active_operations(&self) -> bool {
        self.foreground.is_some() || !self.background_operations.is_empty()
    }

    pub(super) fn sync_application_runtime_preferences(&mut self) {
        self.application.configure_runtime_preferences(
            self.thinking_level,
            self.steering_mode,
            self.follow_up_mode,
            self.auto_compaction_enabled,
        );
    }

    pub(super) fn ensure_session_event_pump(
        &mut self,
        session: &CodingAgentSession,
    ) -> Result<(), CliError> {
        let stream_id = session.snapshot()?.cursor.stream_id;
        if self.session_event_stream_id.as_deref() == Some(stream_id.as_str())
            && self.session_events.is_some()
        {
            return Ok(());
        }

        let mut source = session.subscribe_product_events_public()?;
        let (sender, receiver) = RpcProductEventQueue::new();
        let (flush_tx, mut flush_rx) =
            mpsc::channel::<oneshot::Sender<()>>(RPC_EVENT_FLUSH_QUEUE_CAPACITY);
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    event = source.recv() => {
                        match event {
                            Ok(event) => {
                                if sender.send_event(event).await.is_err() {
                                    break;
                                }
                            }
                            Err(error) => {
                                if let CodingAgentErrorContext::EventStreamLag { skipped } =
                                    error.context
                                {
                                    let _ = sender.send_overflow(skipped).await;
                                }
                                break;
                            }
                        }
                    }
                    flush = flush_rx.recv() => {
                        let Some(flush) = flush else {
                            break;
                        };
                        loop {
                            match source.try_recv() {
                                Ok(Some(event)) => {
                                    if sender.send_event(event).await.is_err() {
                                        break;
                                    }
                                }
                                Ok(None) => break,
                                Err(error) => {
                                    if let CodingAgentErrorContext::EventStreamLag { skipped } =
                                        error.context
                                    {
                                        let _ = sender.send_overflow(skipped).await;
                                    }
                                    break;
                                }
                            }
                        }
                        let _ = flush.send(());
                    }
                }
            }
        });
        self.session_event_stream_id = Some(stream_id);
        self.session_events = Some(receiver);
        self.session_event_flush = Some(flush_tx);
        self.session_events_closed = false;
        self.event_adapter = RpcCodingEventAdapter::new_with_provider(
            self.model.api.clone(),
            self.model.provider.clone(),
            self.model.id.clone(),
        );
        self.adapter_applied_sequence = ProductEventSequence::default();
        Ok(())
    }

    pub(super) async fn detach_client(
        &mut self,
    ) -> Result<RpcDetachStatus, CodingAgentPublicError> {
        let Some(connection) = self.client_connection.take() else {
            return Ok(RpcDetachStatus::AlreadyDetached);
        };
        match connection.detach() {
            Ok(outcome) => Ok(match outcome {
                CodingAgentDetachOutcome::Detached => RpcDetachStatus::Detached,
                CodingAgentDetachOutcome::AlreadyDetached => RpcDetachStatus::AlreadyDetached,
                CodingAgentDetachOutcome::StaleGeneration => RpcDetachStatus::StaleGeneration,
            }),
            Err(error) => {
                self.client_connection = Some(connection);
                Err(error)
            }
        }
    }

    pub(super) fn ensure_client_connection(
        &mut self,
        session: &CodingAgentSession,
    ) -> Result<CodingAgentClientConnection, CliError> {
        if let Some(connection) = &self.client_connection {
            return Ok(connection.clone());
        }
        let connection = session
            .connect(CodingAgentClientId::new("rpc-primary"))
            .map_err(CliError::from)?;
        for (index, input) in self.steering.iter().enumerate() {
            connection
                .enqueue_control_draft(CodingAgentDraft {
                    id: CodingAgentDraftId(format!("rpc-steer-{index}")),
                    kind: CodingAgentDraftKind::Steer,
                    text: input.display_text().to_owned(),
                })
                .map_err(|reason| CliError::SessionFailure(format!("{reason:?}")))?;
        }
        for (index, input) in self.follow_up.iter().enumerate() {
            connection
                .enqueue_control_draft(CodingAgentDraft {
                    id: CodingAgentDraftId(format!("rpc-follow-up-{index}")),
                    kind: CodingAgentDraftKind::FollowUp,
                    text: input.display_text().to_owned(),
                })
                .map_err(|reason| CliError::SessionFailure(format!("{reason:?}")))?;
        }
        self.client_connection = Some(connection.clone());
        Ok(connection)
    }

    pub(super) fn active_prompt_control(
        &self,
    ) -> Result<Option<CodingAgentPromptControl>, CodingAgentPublicError> {
        let Some(foreground) = self.foreground.as_ref() else {
            return Ok(None);
        };
        if foreground.operation_kind != OperationKind::Prompt {
            return Ok(None);
        }
        let Some(connection) = self.client_connection.as_ref() else {
            return Ok(None);
        };
        Ok(connection
            .state()?
            .submitted_operation
            .map(|submitted| connection.prompt_control(submitted.operation_id)))
    }

    pub(super) fn operation_control(
        &self,
        operation_id: &str,
    ) -> Option<CodingAgentOperationControl> {
        self.client_connection
            .as_ref()
            .map(|connection| connection.operation_control(operation_id.to_owned()))
    }

    pub(super) fn active_foreground_operation_id(
        &self,
    ) -> Result<Option<String>, CodingAgentPublicError> {
        let Some(connection) = self.client_connection.as_ref() else {
            return Ok(None);
        };
        Ok(connection
            .state()?
            .submitted_operation
            .map(|submitted| submitted.operation_id))
    }

    pub(super) fn parse_idempotency_key(
        &self,
        key: Option<String>,
    ) -> Result<Option<RpcIdempotencyKey>, CliError> {
        key.map(RpcIdempotencyKey::parse).transpose()
    }

    pub(super) fn idempotent_retry_response(
        &self,
        key: Option<&RpcIdempotencyKey>,
        command: &'static str,
    ) -> Result<Option<serde_json::Value>, CliError> {
        let Some(key) = key else {
            return Ok(None);
        };
        let Some(record) = self.idempotency_records.get(key) else {
            return Ok(None);
        };
        if record.command == command {
            return Ok(Some(serde_json::json!({
                "deduplicated": true,
                "operation": record.operation_kind.as_str(),
                "completed": record.completed
            })));
        }
        Err(CliError::SessionFailure(format!(
            "idempotency key was already used for {}, not {command}",
            record.command
        )))
    }

    pub(super) fn remember_idempotency_key(
        &mut self,
        key: Option<RpcIdempotencyKey>,
        command: &'static str,
        operation_kind: OperationKind,
    ) {
        let Some(key) = key else {
            return;
        };
        if !self.idempotency_records.contains_key(&key) {
            self.idempotency_order.push_back(key.clone());
        }
        self.idempotency_records.insert(
            key,
            RpcIdempotencyRecord {
                command,
                operation_kind,
                completed: false,
            },
        );
        while self.idempotency_order.len() > RPC_IDEMPOTENCY_RECORD_LIMIT {
            if let Some(expired) = self.idempotency_order.pop_front() {
                self.idempotency_records.remove(&expired);
            }
        }
    }

    pub(super) fn mark_idempotency_complete(&mut self, key: Option<&RpcIdempotencyKey>) {
        let Some(key) = key else {
            return;
        };
        if let Some(record) = self.idempotency_records.get_mut(key) {
            record.completed = true;
        }
    }
}
