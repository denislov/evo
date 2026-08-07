use crate::error::CliError;
use crate::protocol::types::{
    RpcCommand, RpcDetachLifecycleEvent, RpcDetachResponse, RpcDetachStatus, RpcHelloResponse,
    RpcResponse, RpcSelfHealingEditModelRepair, RpcSelfHealingEditReplacement,
    RpcSessionNamePersistence, RpcSetSessionNameResponse, RpcShutdownLifecycleEvent,
    RpcShutdownResponse, RpcShutdownStatus, RpcToolAuthorizationApprovalScope,
};
use crate::protocol::version::{
    PRODUCT_EVENT_PROTOCOL_VERSION, RPC_PROTOCOL_VERSION, UI_SNAPSHOT_PROTOCOL_VERSION,
    is_compatible_with,
};
use crate::rpc::prompt::flush_session_product_events;
use crate::rpc::state::{OperationKind, ProductEventSequence, RpcState};
use crate::rpc::wire::{write_json_line, write_rpc_response};
use coding_agent::api::authorization::{
    ToolAuthorizationDecision, ToolAuthorizationIdentity, ToolAuthorizationRequest,
};
use coding_agent::api::client::{
    CodingAgentControlId, CodingAgentRecoveryResolutionRequest, CodingAgentRecoveryRetryRequest,
};
use coding_agent::api::embedding::CodingAgentPromptImage;
use coding_agent::api::error::{CodingAgentPublicDiagnostic, CodingAgentPublicError};
use coding_agent::api::operation::{
    CodingAgentOperation, DelegationConfirmationMode, DelegationPolicy,
    PendingDelegationConfirmation, SelfHealingEditCheckOutput, SelfHealingEditModelRepairOptions,
    SelfHealingEditOutcome, SelfHealingEditRepairAttempt, SelfHealingEditReplacement,
    SelfHealingEditRequest, SupervisionPolicy,
};
use coding_agent::api::runtime::{CodingAgentSession, CodingAgentShutdownOutcome};
use coding_agent::api::view::{
    CodingAgentAgentProfileSummary, CodingAgentSessionTranscriptItem,
    CodingAgentTeamProfileSummary, ProfileId, ProfileKind, ProfileSource, TeamStrategy,
    TeamSupervisor,
};
use tokio::io::AsyncWrite;

use super::{rpc_cli_error, rpc_public_error};

mod dispatch;
mod presentation;

pub(super) use presentation::*;

impl RpcState {
    async fn handle_new_session<W>(
        &mut self,
        id: Option<String>,
        parent_session: Option<String>,
        writer: &mut W,
    ) -> Result<(), CliError>
    where
        W: AsyncWrite + Unpin,
    {
        if self.has_active_operations() {
            write_rpc_response(
                writer,
                RpcResponse::error(
                    id,
                    "new_session",
                    "cannot start new session while agent is streaming",
                ),
            )
            .await?;
            return Ok(());
        }

        let parent_session = match parent_session {
            Some(parent) if parent.is_empty() || parent.trim() != parent => {
                write_rpc_response(
                    writer,
                    RpcResponse::error_with_data(
                        id,
                        "new_session",
                        "parentSession must be a non-empty session ID without surrounding whitespace",
                        serde_json::json!({ "code": "input" }),
                    ),
                )
                .await?;
                return Ok(());
            }
            parent => parent,
        };

        let forked = if let Some(parent) = parent_session.as_deref() {
            let bootstrap = self
                .application
                .session_bootstrap
                .clone()
                .with_forked_session(parent);
            match bootstrap.open().await {
                Ok(session) => Some(session),
                Err(error) => {
                    write_rpc_response(writer, rpc_public_error(id, "new_session", error)).await?;
                    return Ok(());
                }
            }
        } else {
            None
        };
        let forked_state = match forked.as_ref().map(|session| {
            let snapshot = session
                .current_session_snapshot()?
                .expect("forked runtime sessions are persistent");
            let storage = session
                .session_storage()?
                .expect("forked runtime sessions have durable storage");
            Ok::<_, CodingAgentPublicError>((
                snapshot.choice.id,
                storage,
                snapshot.choice.active_leaf_id,
            ))
        }) {
            Some(Ok(state)) => Some(state),
            Some(Err(error)) => {
                write_rpc_response(writer, rpc_public_error(id, "new_session", error)).await?;
                return Ok(());
            }
            None => None,
        };

        if let Err(error) = self.detach_client().await {
            write_rpc_response(writer, rpc_public_error(id, "new_session", error)).await?;
            return Ok(());
        }
        self.steering.clear();
        self.follow_up.clear();
        self.session_name = None;
        self.session_event_stream_id = None;
        self.session_events = None;
        self.session_event_flush = None;
        self.session_events_closed = false;
        self.adapter_applied_sequence = ProductEventSequence::default();
        self.active_shutdown_handle = None;

        let response_data = if let Some((session_id, storage, active_leaf_id)) = forked_state {
            self.application.session_bootstrap = self
                .application
                .session_bootstrap
                .clone()
                .with_session_id(session_id.clone());
            self.active_session_storage = Some(storage);
            self.active_leaf_id = active_leaf_id;
            self.coding_session = forked;
            serde_json::json!({
                "cancelled": false,
                "sessionId": session_id,
                "parentSession": parent_session,
            })
        } else {
            self.application.session_bootstrap = self
                .application
                .session_bootstrap
                .clone()
                .with_fresh_session();
            self.active_session_storage = None;
            self.active_leaf_id = None;
            self.coding_session = None;
            serde_json::json!({"cancelled": false})
        };

        write_rpc_response(
            writer,
            RpcResponse::success(id, "new_session", Some(response_data)),
        )
        .await
    }

    fn self_healing_model_repair_options(
        &self,
        policy: RpcSelfHealingEditModelRepair,
    ) -> SelfHealingEditModelRepairOptions {
        self.application
            .model_repair_options(policy.max_attempts.unwrap_or(1))
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "RPC decoding keeps the versioned self-healing-edit fields explicit"
    )]
    async fn handle_self_healing_edit<W>(
        &mut self,
        id: Option<String>,
        path: String,
        edits: Vec<RpcSelfHealingEditReplacement>,
        check_command: Option<String>,
        repair_attempts: Option<Vec<Vec<RpcSelfHealingEditReplacement>>>,
        model_repair: Option<RpcSelfHealingEditModelRepair>,
        idempotency_key: Option<String>,
        writer: &mut W,
    ) -> Result<(), CliError>
    where
        W: AsyncWrite + Unpin,
    {
        let idempotency_key = match self.parse_idempotency_key(idempotency_key) {
            Ok(key) => key,
            Err(error) => {
                write_rpc_response(writer, rpc_cli_error(id, "self_healing_edit", &error)).await?;
                return Ok(());
            }
        };
        match self.idempotent_retry_response(idempotency_key.as_ref(), "self_healing_edit") {
            Ok(Some(data)) => {
                write_rpc_response(
                    writer,
                    RpcResponse::success(id, "self_healing_edit", Some(data)),
                )
                .await?;
                return Ok(());
            }
            Ok(None) => {}
            Err(error) => {
                write_rpc_response(writer, rpc_cli_error(id, "self_healing_edit", &error)).await?;
                return Ok(());
            }
        }

        if self.is_streaming() {
            write_rpc_response(
                writer,
                RpcResponse::error(
                    id,
                    "self_healing_edit",
                    "cannot run self-healing edit while agent is streaming",
                ),
            )
            .await?;
            return Ok(());
        }

        let replacements = edits
            .into_iter()
            .map(rpc_self_healing_edit_replacement)
            .collect::<Vec<_>>();
        let repair_attempts = repair_attempts
            .unwrap_or_default()
            .into_iter()
            .map(|attempt| {
                attempt
                    .into_iter()
                    .map(rpc_self_healing_edit_replacement)
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        let mut session = match self.coding_session.take() {
            Some(session) => session,
            None => match self.open_runtime_session().await {
                Ok(session) => session,
                Err(error) => {
                    write_rpc_response(writer, rpc_cli_error(id, "self_healing_edit", &error))
                        .await?;
                    return Ok(());
                }
            },
        };
        self.ensure_session_event_pump(&session)?;
        let event_flush = self
            .session_event_flush
            .as_ref()
            .expect("session event pump installed")
            .clone();

        let mut request = SelfHealingEditRequest::new(path, replacements);
        if let Some(command) = check_command {
            request = request.with_check_command(command);
        }
        if !repair_attempts.is_empty() {
            request = request.with_repair_attempts(repair_attempts);
        }
        if let Some(model_repair) = model_repair {
            request =
                request.with_model_repair(self.self_healing_model_repair_options(model_repair));
        }

        let complete_key = idempotency_key.clone();
        self.remember_idempotency_key(
            idempotency_key,
            "self_healing_edit",
            OperationKind::SelfHealingEdit,
        );

        let result = session
            .run(self.application.self_healing_edit_operation(request))
            .await;
        flush_session_product_events(event_flush).await;
        match result {
            Ok(operation_outcome) => {
                let outcome = operation_outcome
                    .into_self_healing_edit()
                    .expect("self-healing edit operation returned a different public outcome");
                let data = rpc_self_healing_edit_data(&outcome);
                self.coding_session = Some(session);
                write_rpc_response(
                    writer,
                    RpcResponse::success(id, "self_healing_edit", Some(data)),
                )
                .await?;
                self.drain_session_product_events(writer).await?;
                self.mark_idempotency_complete(complete_key.as_ref());
                Ok(())
            }
            Err(error) => {
                let response = rpc_public_error(id, "self_healing_edit", error);
                self.coding_session = Some(session);
                write_rpc_response(writer, response).await?;
                self.drain_session_product_events(writer).await?;
                self.mark_idempotency_complete(complete_key.as_ref());
                Ok(())
            }
        }
    }

    async fn handle_list_agent_profiles<W>(
        &mut self,
        id: Option<String>,
        writer: &mut W,
    ) -> Result<(), CliError>
    where
        W: AsyncWrite + Unpin,
    {
        if self.is_streaming() {
            write_rpc_response(
                writer,
                RpcResponse::error(
                    id,
                    "list_agent_profiles",
                    "cannot list agent profiles while agent is streaming",
                ),
            )
            .await?;
            return Ok(());
        }

        let data = if let Some(session) = self.coding_session.as_ref() {
            rpc_agent_profiles_data(session)?
        } else {
            match self.open_profile_listing_session().await {
                Ok(session) => rpc_agent_profiles_data(&session)?,
                Err(error) => {
                    write_rpc_response(writer, rpc_cli_error(id, "list_agent_profiles", &error))
                        .await?;
                    return Ok(());
                }
            }
        };
        write_rpc_response(
            writer,
            RpcResponse::success(id, "list_agent_profiles", Some(data)),
        )
        .await
    }

    async fn handle_list_team_profiles<W>(
        &mut self,
        id: Option<String>,
        writer: &mut W,
    ) -> Result<(), CliError>
    where
        W: AsyncWrite + Unpin,
    {
        if self.is_streaming() {
            write_rpc_response(
                writer,
                RpcResponse::error(
                    id,
                    "list_team_profiles",
                    "cannot list team profiles while agent is streaming",
                ),
            )
            .await?;
            return Ok(());
        }

        let data = if let Some(session) = self.coding_session.as_ref() {
            rpc_team_profiles_data(session)
        } else {
            match self.open_profile_listing_session().await {
                Ok(session) => rpc_team_profiles_data(&session),
                Err(error) => {
                    write_rpc_response(writer, rpc_cli_error(id, "list_team_profiles", &error))
                        .await?;
                    return Ok(());
                }
            }
        };
        write_rpc_response(
            writer,
            RpcResponse::success(id, "list_team_profiles", Some(data)),
        )
        .await
    }

    async fn handle_list_delegation_confirmations<W>(
        &mut self,
        id: Option<String>,
        writer: &mut W,
    ) -> Result<(), CliError>
    where
        W: AsyncWrite + Unpin,
    {
        if self.is_streaming() {
            write_rpc_response(
                writer,
                RpcResponse::error(
                    id,
                    "list_delegation_confirmations",
                    "cannot list delegation confirmations while agent is streaming",
                ),
            )
            .await?;
            return Ok(());
        }

        let confirmations = self
            .coding_session
            .as_ref()
            .map(CodingAgentSession::pending_delegation_confirmations)
            .unwrap_or_default()
            .into_iter()
            .map(|pending| rpc_pending_delegation_confirmation(&pending))
            .collect::<Vec<_>>();
        write_rpc_response(
            writer,
            RpcResponse::success(
                id,
                "list_delegation_confirmations",
                Some(serde_json::json!({ "confirmations": confirmations })),
            ),
        )
        .await
    }

    async fn handle_list_tool_authorizations<W>(
        &mut self,
        id: Option<String>,
        writer: &mut W,
    ) -> Result<(), CliError>
    where
        W: AsyncWrite + Unpin,
    {
        let authorizations = match self.pending_tool_authorizations() {
            Ok(authorizations) => authorizations,
            Err(error) => {
                write_rpc_response(
                    writer,
                    rpc_cli_error(id, "list_tool_authorizations", &error),
                )
                .await?;
                return Ok(());
            }
        };
        write_rpc_response(
            writer,
            RpcResponse::success(
                id,
                "list_tool_authorizations",
                Some(serde_json::json!({ "authorizations": authorizations })),
            ),
        )
        .await
    }

    async fn handle_approve_tool_authorization<W>(
        &mut self,
        id: Option<String>,
        identity: ToolAuthorizationIdentity,
        scope: RpcToolAuthorizationApprovalScope,
        writer: &mut W,
    ) -> Result<(), CliError>
    where
        W: AsyncWrite + Unpin,
    {
        let decision = match scope {
            RpcToolAuthorizationApprovalScope::Once => ToolAuthorizationDecision::AllowOnce,
            RpcToolAuthorizationApprovalScope::Operation => {
                ToolAuthorizationDecision::AllowForOperation
            }
        };
        match self.decide_tool_authorization(&identity, decision).await {
            Ok(()) => {
                write_rpc_response(
                    writer,
                    RpcResponse::success(
                        id,
                        "approve_tool_authorization",
                        Some(serde_json::json!({
                            "authorizationId": identity.authorization_id,
                            "scope": match scope {
                                RpcToolAuthorizationApprovalScope::Once => "once",
                                RpcToolAuthorizationApprovalScope::Operation => "operation",
                            },
                        })),
                    ),
                )
                .await
            }
            Err(error) => {
                write_rpc_response(
                    writer,
                    rpc_cli_error(id, "approve_tool_authorization", &error),
                )
                .await
            }
        }
    }

    async fn handle_deny_tool_authorization<W>(
        &mut self,
        id: Option<String>,
        identity: ToolAuthorizationIdentity,
        reason: Option<String>,
        writer: &mut W,
    ) -> Result<(), CliError>
    where
        W: AsyncWrite + Unpin,
    {
        match self
            .decide_tool_authorization(&identity, ToolAuthorizationDecision::Deny { reason })
            .await
        {
            Ok(()) => {
                write_rpc_response(
                    writer,
                    RpcResponse::success(
                        id,
                        "deny_tool_authorization",
                        Some(serde_json::json!({
                            "authorizationId": identity.authorization_id
                        })),
                    ),
                )
                .await
            }
            Err(error) => {
                write_rpc_response(writer, rpc_cli_error(id, "deny_tool_authorization", &error))
                    .await
            }
        }
    }

    fn pending_tool_authorizations(&self) -> Result<Vec<ToolAuthorizationRequest>, CliError> {
        if let Some(connection) = self.client_connection.as_ref() {
            return connection
                .pending_tool_authorizations()
                .map_err(CliError::from);
        }
        self.coding_session
            .as_ref()
            .ok_or_else(|| CliError::SessionFailure("no active coding session".into()))
            .and_then(|session| {
                session
                    .pending_tool_authorizations()
                    .map_err(CliError::from)
            })
    }

    async fn decide_tool_authorization(
        &self,
        identity: &ToolAuthorizationIdentity,
        decision: ToolAuthorizationDecision,
    ) -> Result<(), CliError> {
        if let Some(connection) = self.client_connection.as_ref() {
            return connection
                .decide_tool_authorization(identity, decision)
                .await
                .map_err(CliError::from);
        }
        self.coding_session
            .as_ref()
            .ok_or_else(|| CliError::SessionFailure("no active coding session".into()))?
            .decide_tool_authorization(identity, decision)
            .await
            .map_err(CliError::from)
    }

    async fn handle_reject_delegation<W>(
        &mut self,
        id: Option<String>,
        operation_id: String,
        tool_call_id: String,
        reason: Option<String>,
        idempotency_key: Option<String>,
        writer: &mut W,
    ) -> Result<(), CliError>
    where
        W: AsyncWrite + Unpin,
    {
        let idempotency_key = match self.parse_idempotency_key(idempotency_key) {
            Ok(key) => key,
            Err(error) => {
                write_rpc_response(writer, rpc_cli_error(id, "reject_delegation", &error)).await?;
                return Ok(());
            }
        };
        match self.idempotent_retry_response(idempotency_key.as_ref(), "reject_delegation") {
            Ok(Some(data)) => {
                write_rpc_response(
                    writer,
                    RpcResponse::success(id, "reject_delegation", Some(data)),
                )
                .await?;
                return Ok(());
            }
            Ok(None) => {}
            Err(error) => {
                write_rpc_response(writer, rpc_cli_error(id, "reject_delegation", &error)).await?;
                return Ok(());
            }
        }

        if self.is_streaming() {
            write_rpc_response(
                writer,
                RpcResponse::error(
                    id,
                    "reject_delegation",
                    "cannot reject delegation while agent is streaming",
                ),
            )
            .await?;
            return Ok(());
        }

        let Some(mut session) = self.coding_session.take() else {
            write_rpc_response(
                writer,
                RpcResponse::error(id, "reject_delegation", "no active coding session"),
            )
            .await?;
            return Ok(());
        };

        let pending = match session
            .pending_delegation_confirmations()
            .into_iter()
            .find(|pending| {
                pending.operation_id == operation_id && pending.tool_call_id == tool_call_id
            }) {
            Some(pending) => pending,
            None => {
                self.coding_session = Some(session);
                write_rpc_response(
                    writer,
                    RpcResponse::error(
                        id,
                        "reject_delegation",
                        format!(
                        "pending delegation confirmation not found: operation_id={operation_id}, tool_call_id={tool_call_id}"
                    ),
                    ),
                )
                .await?;
                return Ok(());
            }
        };

        let complete_key = idempotency_key.clone();
        self.remember_idempotency_key(
            idempotency_key,
            "reject_delegation",
            OperationKind::DelegationConfirmation,
        );

        let reason = reason.unwrap_or_default();
        let reason = if reason.trim().is_empty() {
            "delegation rejected by user".to_string()
        } else {
            reason
        };
        self.ensure_session_event_pump(&session)?;
        let event_flush = self
            .session_event_flush
            .as_ref()
            .expect("session event pump installed")
            .clone();

        let result = session
            .run(CodingAgentOperation::RejectDelegation {
                operation_id,
                tool_call_id,
                reason: reason.clone(),
            })
            .await;
        flush_session_product_events(event_flush).await;
        match result {
            Ok(operation_outcome) => {
                operation_outcome
                    .into_delegation_rejected()
                    .expect("delegation rejection operation returned a different public outcome");
                self.coding_session = Some(session);
                write_rpc_response(
                    writer,
                    RpcResponse::success(
                        id,
                        "reject_delegation",
                        Some(serde_json::json!({
                            "delegation": rpc_pending_delegation_confirmation(&pending),
                            "reason": reason,
                        })),
                    ),
                )
                .await?;
                self.drain_session_product_events(writer).await?;
                self.mark_idempotency_complete(complete_key.as_ref());
                Ok(())
            }
            Err(error) => {
                self.coding_session = Some(session);
                write_rpc_response(writer, rpc_public_error(id, "reject_delegation", error))
                    .await?;
                self.drain_session_product_events(writer).await?;
                self.mark_idempotency_complete(complete_key.as_ref());
                Ok(())
            }
        }
    }

    async fn open_profile_listing_session(&self) -> Result<CodingAgentSession, CliError> {
        Ok(self
            .application
            .session_bootstrap
            .clone()
            .without_persistence()
            .open()
            .await?)
    }

    async fn open_runtime_session(&self) -> Result<CodingAgentSession, CliError> {
        Ok(self.application.session_bootstrap.open().await?)
    }
}
