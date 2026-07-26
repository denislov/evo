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

impl RpcState {
    pub(super) async fn handle_command<W>(
        &mut self,
        command: RpcCommand,
        writer: &mut W,
    ) -> Result<(), CliError>
    where
        W: AsyncWrite + Unpin,
    {
        match command {
            RpcCommand::Hello { id, protocol } => {
                if self.negotiated_protocol.rpc.is_some() {
                    return write_rpc_response(
                        writer,
                        RpcResponse::error_with_data(
                            id,
                            "hello",
                            "RPC protocol is already negotiated",
                            serde_json::json!({
                                "code": "protocol_already_negotiated",
                            }),
                        ),
                    )
                    .await;
                }
                if !is_compatible_with(RPC_PROTOCOL_VERSION, &protocol) {
                    write_rpc_response(
                        writer,
                        RpcResponse::error_with_data(
                            id,
                            "hello",
                            format!(
                                "unsupported protocol version for rpc: requested {protocol}, supported {RPC_PROTOCOL_VERSION}"
                            ),
                            serde_json::json!({
                                "code": "unsupported_protocol_version",
                                "requested": {
                                    "family": protocol.family,
                                    "major": protocol.major,
                                    "minor": protocol.minor
                                },
                                "supported": {
                                    "family": RPC_PROTOCOL_VERSION.family,
                                    "major": RPC_PROTOCOL_VERSION.major,
                                    "minor": RPC_PROTOCOL_VERSION.minor
                                }
                            }),
                        ),
                    )
                    .await?;
                    return Ok(());
                }
                self.negotiated_protocol.rpc = Some(RPC_PROTOCOL_VERSION);
                write_rpc_response(
                    writer,
                    RpcResponse::success(
                        id,
                        "hello",
                        Some(
                            serde_json::to_value(RpcHelloResponse {
                                protocol: RPC_PROTOCOL_VERSION,
                                product_events: PRODUCT_EVENT_PROTOCOL_VERSION,
                                ui_snapshot: UI_SNAPSHOT_PROTOCOL_VERSION,
                            })
                            .expect("hello response serializes"),
                        ),
                    ),
                )
                .await
            }
            RpcCommand::Detach { id } => match self.detach_client().await {
                Ok(status) => {
                    if status == RpcDetachStatus::Detached {
                        write_json_line(writer, &RpcDetachLifecycleEvent { status }).await?;
                    }
                    write_rpc_response(
                        writer,
                        RpcResponse::success(
                            id,
                            "detach",
                            Some(
                                serde_json::to_value(RpcDetachResponse { status })
                                    .expect("detach response serializes"),
                            ),
                        ),
                    )
                    .await
                }
                Err(error) => {
                    write_rpc_response(writer, rpc_public_error(id, "detach", error)).await
                }
            },
            RpcCommand::Shutdown { id } => {
                if self.has_active_operations() {
                    if self.pending_shutdown_response.is_some() {
                        write_rpc_response(
                            writer,
                            RpcResponse::error_with_data(
                                id,
                                "shutdown",
                                "runtime shutdown is already pending",
                                serde_json::json!({"code": "shutdown_in_progress"}),
                            ),
                        )
                        .await?;
                        return Ok(());
                    }
                    let shutdown_handle =
                        self.active_shutdown_handle.as_ref().ok_or_else(|| {
                            CliError::AgentFailure(
                                "active RPC operation has no runtime shutdown authority".into(),
                            )
                        })?;
                    shutdown_handle.request_shutdown();
                    self.pending_shutdown_response = Some(id);
                    return Ok(());
                }

                let mut session = match self.coding_session.take() {
                    Some(session) => session,
                    None => self.open_runtime_session().await?,
                };
                let outcome = session.shutdown().await?;
                let status = match outcome {
                    CodingAgentShutdownOutcome::ShutDown => RpcShutdownStatus::ShutDown,
                    CodingAgentShutdownOutcome::AlreadyShutDown => {
                        RpcShutdownStatus::AlreadyShutDown
                    }
                };
                if status == RpcShutdownStatus::ShutDown {
                    write_json_line(writer, &RpcShutdownLifecycleEvent { status }).await?;
                }
                let response = write_rpc_response(
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
                .await;
                self.coding_session = Some(session);
                response
            }
            RpcCommand::Prompt {
                id,
                message,
                images,
                streaming_behavior,
                after_snapshot_cursor,
                idempotency_key,
            } => {
                self.handle_prompt(
                    id,
                    message,
                    images,
                    streaming_behavior,
                    after_snapshot_cursor,
                    idempotency_key,
                    writer,
                )
                .await
            }
            RpcCommand::Steer {
                id,
                message,
                images,
            } => {
                if let Some(foreground) = self.foreground.as_ref() {
                    if foreground.operation_kind != OperationKind::Prompt {
                        write_rpc_response(
                            writer,
                            RpcResponse::error(
                                id,
                                "steer",
                                format!(
                                    "cannot steer while {} is running",
                                    foreground.operation_kind.as_str()
                                ),
                            ),
                        )
                        .await?;
                        return Ok(());
                    }
                    let Some(control) = self.active_prompt_control()? else {
                        write_rpc_response(
                            writer,
                            RpcResponse::error(id, "steer", "agent is not streaming"),
                        )
                        .await?;
                        return Ok(());
                    };
                    let control_id = CodingAgentControlId(
                        id.clone().unwrap_or_else(|| format!("rpc-steer-{message}")),
                    );
                    let result = match self
                        .application
                        .prepare_prompt_with_images(message, images.unwrap_or_default())
                    {
                        Ok(prompt) => control.steer_prepared(control_id, prompt),
                        Err(error) => {
                            write_rpc_response(writer, rpc_public_error(id, "steer", error))
                                .await?;
                            return Ok(());
                        }
                    };
                    match result {
                        Ok(_) => {
                            write_rpc_response(writer, RpcResponse::success(id, "steer", None))
                                .await?
                        }
                        Err(error) => {
                            write_rpc_response(
                                writer,
                                RpcResponse::error(id, "steer", format!("{:?}", error.reason)),
                            )
                            .await?
                        }
                    }
                    return Ok(());
                }
                let input = match self
                    .application
                    .prepare_prompt_with_images(message, images.unwrap_or_default())
                {
                    Ok(prompt) => prompt,
                    Err(error) => {
                        write_rpc_response(writer, rpc_public_error(id, "steer", error)).await?;
                        return Ok(());
                    }
                };
                if let Err(limit) = self.enqueue_steer(input) {
                    write_rpc_response(
                        writer,
                        RpcResponse::error_with_data(
                            id,
                            "steer",
                            "RPC queued controls exceed an input limit",
                            serde_json::json!({
                                "code": "request_too_large",
                                "limit": limit,
                            }),
                        ),
                    )
                    .await?;
                    return Ok(());
                }
                write_rpc_response(writer, RpcResponse::success(id, "steer", None)).await?;
                self.emit_queue_update(writer).await
            }
            RpcCommand::FollowUp {
                id,
                message,
                images,
            } => {
                if let Some(foreground) = self.foreground.as_ref() {
                    if foreground.operation_kind != OperationKind::Prompt {
                        write_rpc_response(
                            writer,
                            RpcResponse::error(
                                id,
                                "follow_up",
                                format!(
                                    "cannot follow up while {} is running",
                                    foreground.operation_kind.as_str()
                                ),
                            ),
                        )
                        .await?;
                        return Ok(());
                    }
                    let Some(control) = self.active_prompt_control()? else {
                        write_rpc_response(
                            writer,
                            RpcResponse::error(id, "follow_up", "agent is not streaming"),
                        )
                        .await?;
                        return Ok(());
                    };
                    let control_id = CodingAgentControlId(
                        id.clone()
                            .unwrap_or_else(|| format!("rpc-follow-up-{message}")),
                    );
                    let result = match self
                        .application
                        .prepare_prompt_with_images(message, images.unwrap_or_default())
                    {
                        Ok(prompt) => control.follow_up_prepared(control_id, prompt),
                        Err(error) => {
                            write_rpc_response(writer, rpc_public_error(id, "follow_up", error))
                                .await?;
                            return Ok(());
                        }
                    };
                    match result {
                        Ok(_) => {
                            write_rpc_response(writer, RpcResponse::success(id, "follow_up", None))
                                .await?
                        }
                        Err(error) => {
                            write_rpc_response(
                                writer,
                                RpcResponse::error(id, "follow_up", format!("{:?}", error.reason)),
                            )
                            .await?
                        }
                    }
                    return Ok(());
                }
                let input = match self
                    .application
                    .prepare_prompt_with_images(message, images.unwrap_or_default())
                {
                    Ok(prompt) => prompt,
                    Err(error) => {
                        write_rpc_response(writer, rpc_public_error(id, "follow_up", error))
                            .await?;
                        return Ok(());
                    }
                };
                if let Err(limit) = self.enqueue_follow_up(input) {
                    write_rpc_response(
                        writer,
                        RpcResponse::error_with_data(
                            id,
                            "follow_up",
                            "RPC queued controls exceed an input limit",
                            serde_json::json!({
                                "code": "request_too_large",
                                "limit": limit,
                            }),
                        ),
                    )
                    .await?;
                    return Ok(());
                }
                write_rpc_response(writer, RpcResponse::success(id, "follow_up", None)).await?;
                self.emit_queue_update(writer).await
            }
            RpcCommand::Abort { id, operation_id } => {
                let target_operation_id = match operation_id {
                    Some(operation_id) => {
                        if !self.background_operations.contains_key(&operation_id)
                            && self.active_foreground_operation_id()?.as_deref()
                                != Some(operation_id.as_str())
                        {
                            write_rpc_response(
                                writer,
                                RpcResponse::error(id, "abort", "operation is not running"),
                            )
                            .await?;
                            return Ok(());
                        }
                        Some(operation_id)
                    }
                    None if self.foreground.is_some() => self.active_foreground_operation_id()?,
                    None => None,
                };
                let cancelled = if let Some(operation_id) = target_operation_id {
                    let Some(control) = self.operation_control(&operation_id) else {
                        write_rpc_response(
                            writer,
                            RpcResponse::error(id, "abort", "operation has no control owner"),
                        )
                        .await?;
                        return Ok(());
                    };
                    match control.abort(
                        CodingAgentControlId(id.clone().unwrap_or_else(|| "rpc-abort".into())),
                        "rpc abort requested",
                    ) {
                        Ok(_) => true,
                        Err(error) => {
                            write_rpc_response(
                                writer,
                                RpcResponse::error(id, "abort", format!("{:?}", error.reason)),
                            )
                            .await?;
                            return Ok(());
                        }
                    }
                } else {
                    false
                };
                write_rpc_response(
                    writer,
                    RpcResponse::success(
                        id,
                        "abort",
                        Some(serde_json::json!({ "cancelled": cancelled })),
                    ),
                )
                .await
            }
            RpcCommand::NewSession { id, parent_session } => {
                self.handle_new_session(id, parent_session, writer).await
            }
            RpcCommand::GetState { id } => {
                write_rpc_response(
                    writer,
                    RpcResponse::success(
                        id,
                        "get_state",
                        Some(
                            serde_json::to_value(self.session_state())
                                .expect("rpc state serializes"),
                        ),
                    ),
                )
                .await
            }
            RpcCommand::SelfHealingEdit {
                id,
                path,
                edits,
                check_command,
                repair_attempts,
                model_repair,
                idempotency_key,
            } => {
                self.handle_self_healing_edit(
                    id,
                    path,
                    edits,
                    check_command,
                    repair_attempts,
                    model_repair,
                    idempotency_key,
                    writer,
                )
                .await
            }
            RpcCommand::ListAgentProfiles { id } => {
                self.handle_list_agent_profiles(id, writer).await
            }
            RpcCommand::ListTeamProfiles { id } => self.handle_list_team_profiles(id, writer).await,
            RpcCommand::SetDefaultAgentProfile {
                id,
                profile_id,
                idempotency_key,
            } => {
                self.handle_set_default_agent_profile(id, profile_id, idempotency_key, writer)
                    .await
            }
            RpcCommand::InvokeAgent {
                id,
                profile_id,
                task,
                idempotency_key,
            } => {
                self.handle_invoke_agent(id, profile_id, task, idempotency_key, writer)
                    .await
            }
            RpcCommand::InvokeTeam {
                id,
                team_id,
                task,
                idempotency_key,
            } => {
                self.handle_invoke_team(id, team_id, task, idempotency_key, writer)
                    .await
            }
            RpcCommand::ListDelegationConfirmations { id } => {
                self.handle_list_delegation_confirmations(id, writer).await
            }
            RpcCommand::ListToolAuthorizations { id } => {
                self.handle_list_tool_authorizations(id, writer).await
            }
            RpcCommand::ApproveToolAuthorization {
                id,
                identity,
                scope,
            } => {
                self.handle_approve_tool_authorization(id, identity, scope, writer)
                    .await
            }
            RpcCommand::DenyToolAuthorization {
                id,
                identity,
                reason,
            } => {
                self.handle_deny_tool_authorization(id, identity, reason, writer)
                    .await
            }
            RpcCommand::ApproveDelegation {
                id,
                operation_id,
                tool_call_id,
                idempotency_key,
            } => {
                self.handle_approve_delegation(
                    id,
                    operation_id,
                    tool_call_id,
                    idempotency_key,
                    writer,
                )
                .await
            }
            RpcCommand::RejectDelegation {
                id,
                operation_id,
                tool_call_id,
                reason,
                idempotency_key,
            } => {
                self.handle_reject_delegation(
                    id,
                    operation_id,
                    tool_call_id,
                    reason,
                    idempotency_key,
                    writer,
                )
                .await
            }
            RpcCommand::SetThinkingLevel { id, level } => {
                self.thinking_level = level;
                self.sync_application_runtime_preferences();
                write_rpc_response(writer, RpcResponse::success(id, "set_thinking_level", None))
                    .await
            }
            RpcCommand::SetSteeringMode { id, mode } => {
                self.steering_mode = mode;
                self.sync_application_runtime_preferences();
                write_rpc_response(writer, RpcResponse::success(id, "set_steering_mode", None))
                    .await
            }
            RpcCommand::SetFollowUpMode { id, mode } => {
                self.follow_up_mode = mode;
                self.sync_application_runtime_preferences();
                write_rpc_response(writer, RpcResponse::success(id, "set_follow_up_mode", None))
                    .await
            }
            RpcCommand::Compact {
                id,
                custom_instructions,
            } => self.handle_compact(id, custom_instructions, writer).await,
            RpcCommand::SetAutoCompaction { id, enabled } => {
                if enabled && self.application.session_bootstrap.is_persistent() {
                    return write_rpc_response(
                        writer,
                        RpcResponse::error(
                            id,
                            "set_auto_compaction",
                            "automatic compaction is unavailable for persistent sessions; use compact",
                        ),
                    )
                    .await;
                }
                self.auto_compaction_enabled = enabled;
                self.sync_application_runtime_preferences();
                write_rpc_response(
                    writer,
                    RpcResponse::success(id, "set_auto_compaction", None),
                )
                .await
            }
            RpcCommand::GetSessionStats { id } => match self.session_stats() {
                Ok(stats) => {
                    write_rpc_response(
                        writer,
                        RpcResponse::success(
                            id,
                            "get_session_stats",
                            Some(serde_json::to_value(stats).expect("rpc session stats serialize")),
                        ),
                    )
                    .await
                }
                Err(error) => {
                    write_rpc_response(writer, rpc_public_error(id, "get_session_stats", error))
                        .await
                }
            },
            RpcCommand::GetLastAssistantText { id } => match self.last_assistant_text() {
                Ok(text) => {
                    write_rpc_response(
                        writer,
                        RpcResponse::success(
                            id,
                            "get_last_assistant_text",
                            Some(serde_json::json!({ "text": text })),
                        ),
                    )
                    .await
                }
                Err(error) => {
                    write_rpc_response(
                        writer,
                        rpc_public_error(id, "get_last_assistant_text", error),
                    )
                    .await
                }
            },
            RpcCommand::SetSessionName { id, name } => {
                let name = name.trim().to_string();
                if name.is_empty() {
                    write_rpc_response(
                        writer,
                        RpcResponse::error(id, "set_session_name", "Session name cannot be empty"),
                    )
                    .await?;
                    return Ok(());
                }
                self.session_name = Some(name.clone());
                write_rpc_response(
                    writer,
                    RpcResponse::success(
                        id,
                        "set_session_name",
                        Some(
                            serde_json::to_value(RpcSetSessionNameResponse {
                                name,
                                persistence: RpcSessionNamePersistence::AdapterLocal,
                            })
                            .expect("set-session-name response serializes"),
                        ),
                    ),
                )
                .await
            }
            RpcCommand::GetMessages { id } => match self.transcript_items() {
                Ok(messages) => {
                    let messages = messages
                        .into_iter()
                        .map(rpc_transcript_item)
                        .collect::<Vec<_>>();
                    write_rpc_response(
                        writer,
                        RpcResponse::success(
                            id,
                            "get_messages",
                            Some(serde_json::json!({ "messages": messages })),
                        ),
                    )
                    .await
                }
                Err(error) => {
                    write_rpc_response(writer, rpc_public_error(id, "get_messages", error)).await
                }
            },
            RpcCommand::RecoveryInspect {
                id,
                authorization_token,
            } => {
                self.authorize_recovery(&authorization_token)?;
                let pending = self
                    .coding_session
                    .as_ref()
                    .ok_or_else(|| CliError::SessionFailure("no active session".into()))?
                    .recovery_pending()?;
                write_rpc_response(
                    writer,
                    RpcResponse::success(
                        id,
                        "recovery_inspect",
                        Some(serde_json::json!({"pending": pending})),
                    ),
                )
                .await
            }
            RpcCommand::RecoveryRetry {
                id,
                authorization_token,
                operation_id,
                recovery_id,
                record_version,
                descriptor_revision,
                capability_generation,
                attempt_count,
                schedule_with_backoff,
                idempotency_key,
            } => {
                self.authorize_recovery(&authorization_token)?;
                let key = self.parse_idempotency_key(idempotency_key)?;
                if let Some(response) =
                    self.idempotent_retry_response(key.as_ref(), "recovery_retry")?
                {
                    write_rpc_response(
                        writer,
                        RpcResponse::success(id, "recovery_retry", Some(response)),
                    )
                    .await?;
                    return Ok(());
                }
                let request = CodingAgentRecoveryRetryRequest {
                    operation_id,
                    recovery_id,
                    expected_record_version: record_version,
                    expected_descriptor_revision: descriptor_revision,
                    expected_capability_generation: capability_generation,
                    expected_attempt_count: attempt_count,
                    schedule_with_backoff,
                };
                let result = self
                    .coding_session
                    .as_mut()
                    .ok_or_else(|| CliError::SessionFailure("no active session".into()))?
                    .retry_recovery(request)?;
                self.remember_idempotency_key(key, "recovery_retry", OperationKind::Prompt);
                write_rpc_response(writer, RpcResponse::success(id, "recovery_retry", Some(serde_json::json!({"operationId": result.operation_id, "recoveryId": result.recovery_id, "attemptCount": result.attempt_count, "lastAttemptAt": result.last_attempt_at, "nextAttemptAt": result.next_attempt_at})))) .await
            }
            RpcCommand::RecoveryResolve {
                id,
                authorization_token,
                operation_id,
                recovery_id,
                record_version,
                descriptor_revision,
                capability_generation,
                attempt_count,
                resolution,
                reason,
                idempotency_key,
            } => {
                self.authorize_recovery(&authorization_token)?;
                let key = self.parse_idempotency_key(idempotency_key)?;
                if let Some(response) =
                    self.idempotent_retry_response(key.as_ref(), "recovery_resolve")?
                {
                    write_rpc_response(
                        writer,
                        RpcResponse::success(id, "recovery_resolve", Some(response)),
                    )
                    .await?;
                    return Ok(());
                }
                let request = CodingAgentRecoveryResolutionRequest {
                    operation_id,
                    recovery_id,
                    expected_record_version: record_version,
                    expected_descriptor_revision: descriptor_revision,
                    expected_capability_generation: capability_generation,
                    expected_attempt_count: attempt_count,
                    resolution,
                    reason,
                };
                let result = self
                    .coding_session
                    .as_mut()
                    .ok_or_else(|| CliError::SessionFailure("no active session".into()))?
                    .resolve_recovery(request)?;
                self.remember_idempotency_key(key, "recovery_resolve", OperationKind::Prompt);
                write_rpc_response(
                    writer,
                    RpcResponse::success(
                        id,
                        "recovery_resolve",
                        Some(serde_json::json!({
                            "operationId": result.operation_id,
                            "recoveryId": result.recovery_id,
                            "resolution": result.resolution,
                        })),
                    ),
                )
                .await
            }
        }
    }

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
            let session_dir = session
                .session_storage_path()?
                .expect("forked runtime sessions have a storage path");
            Ok::<_, CodingAgentPublicError>((
                snapshot.choice.id,
                session_dir,
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

        let response_data = if let Some((session_id, session_dir, active_leaf_id)) = forked_state {
            self.application.session_bootstrap = self
                .application
                .session_bootstrap
                .clone()
                .with_session_id(session_id.clone());
            self.active_session_path = Some(session_dir);
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
            self.active_session_path = None;
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
        self.ensure_session_event_pump(&session);
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
            rpc_agent_profiles_data(session)
        } else {
            match self.open_profile_listing_session().await {
                Ok(session) => rpc_agent_profiles_data(&session),
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

    async fn handle_set_default_agent_profile<W>(
        &mut self,
        id: Option<String>,
        profile_id: String,
        idempotency_key: Option<String>,
        writer: &mut W,
    ) -> Result<(), CliError>
    where
        W: AsyncWrite + Unpin,
    {
        let idempotency_key = match self.parse_idempotency_key(idempotency_key) {
            Ok(key) => key,
            Err(error) => {
                write_rpc_response(
                    writer,
                    rpc_cli_error(id, "set_default_agent_profile", &error),
                )
                .await?;
                return Ok(());
            }
        };
        match self.idempotent_retry_response(idempotency_key.as_ref(), "set_default_agent_profile")
        {
            Ok(Some(data)) => {
                write_rpc_response(
                    writer,
                    RpcResponse::success(id, "set_default_agent_profile", Some(data)),
                )
                .await?;
                return Ok(());
            }
            Ok(None) => {}
            Err(error) => {
                write_rpc_response(
                    writer,
                    rpc_cli_error(id, "set_default_agent_profile", &error),
                )
                .await?;
                return Ok(());
            }
        }

        if self.is_streaming() {
            write_rpc_response(
                writer,
                RpcResponse::error(
                    id,
                    "set_default_agent_profile",
                    "cannot set default agent profile while agent is streaming",
                ),
            )
            .await?;
            return Ok(());
        }

        let profile_id = match ProfileId::new(profile_id) {
            Ok(profile_id) => profile_id,
            Err(message) => {
                write_rpc_response(
                    writer,
                    RpcResponse::error(id, "set_default_agent_profile", message),
                )
                .await?;
                return Ok(());
            }
        };

        let mut session = match self.coding_session.take() {
            Some(session) => session,
            None => match self.open_runtime_session().await {
                Ok(session) => session,
                Err(error) => {
                    write_rpc_response(
                        writer,
                        rpc_cli_error(id, "set_default_agent_profile", &error),
                    )
                    .await?;
                    return Ok(());
                }
            },
        };

        if !session
            .agent_profiles()
            .iter()
            .any(|profile| profile.id.as_str() == profile_id.as_str())
        {
            self.coding_session = Some(session);
            write_rpc_response(
                writer,
                RpcResponse::error(
                    id,
                    "set_default_agent_profile",
                    format!("Unknown agent profile: {profile_id}"),
                ),
            )
            .await?;
            return Ok(());
        }

        let complete_key = idempotency_key.clone();
        self.remember_idempotency_key(
            idempotency_key,
            "set_default_agent_profile",
            OperationKind::SetDefaultAgentProfile,
        );

        self.ensure_session_event_pump(&session);
        let event_flush = self
            .session_event_flush
            .as_ref()
            .expect("session event pump installed")
            .clone();

        let result = session
            .run(CodingAgentOperation::SetDefaultAgentProfile {
                profile_id: profile_id.clone(),
            })
            .await;
        flush_session_product_events(event_flush).await;
        match result {
            Ok(operation_outcome) => {
                operation_outcome
                    .into_default_agent_profile_changed()
                    .expect(
                        "set default agent profile operation returned a different public outcome",
                    );
                let data = serde_json::json!({ "defaultAgentProfileId": profile_id.as_str() });
                self.coding_session = Some(session);
                write_rpc_response(
                    writer,
                    RpcResponse::success(id, "set_default_agent_profile", Some(data)),
                )
                .await?;
                self.drain_session_product_events(writer).await?;
                self.mark_idempotency_complete(complete_key.as_ref());
                Ok(())
            }
            Err(error) => {
                self.coding_session = Some(session);
                write_rpc_response(
                    writer,
                    rpc_public_error(id, "set_default_agent_profile", error),
                )
                .await?;
                self.drain_session_product_events(writer).await?;
                self.mark_idempotency_complete(complete_key.as_ref());
                Ok(())
            }
        }
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
        match self.decide_tool_authorization(&identity, decision) {
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
        match self.decide_tool_authorization(&identity, ToolAuthorizationDecision::Deny { reason })
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
            .map(CodingAgentSession::pending_tool_authorizations)
            .ok_or_else(|| CliError::SessionFailure("no active coding session".into()))
    }

    fn decide_tool_authorization(
        &self,
        identity: &ToolAuthorizationIdentity,
        decision: ToolAuthorizationDecision,
    ) -> Result<(), CliError> {
        if let Some(connection) = self.client_connection.as_ref() {
            return connection
                .decide_tool_authorization(identity, decision)
                .map_err(CliError::from);
        }
        self.coding_session
            .as_ref()
            .ok_or_else(|| CliError::SessionFailure("no active coding session".into()))?
            .decide_tool_authorization(identity, decision)
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
        self.ensure_session_event_pump(&session);
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

fn rpc_self_healing_edit_replacement(
    edit: RpcSelfHealingEditReplacement,
) -> SelfHealingEditReplacement {
    SelfHealingEditReplacement::new(edit.old_text, edit.new_text)
}

fn rpc_self_healing_edit_data(outcome: &SelfHealingEditOutcome) -> serde_json::Value {
    serde_json::json!({
        "path": outcome.path,
        "message": outcome.message,
        "diff": outcome.diff,
        "patch": outcome.patch,
        "firstChangedLine": outcome.first_changed_line,
        "attempts": outcome.attempts,
        "diagnostics": outcome
            .diagnostics
            .iter()
            .map(|diagnostic| serde_json::json!({ "message": diagnostic.message }))
            .collect::<Vec<_>>(),
        "checkOutput": outcome
            .check_output
            .as_ref()
            .map(rpc_self_healing_check_output_data),
        "repairAttempts": outcome
            .repair_attempts
            .iter()
            .map(rpc_self_healing_repair_attempt_data)
            .collect::<Vec<_>>(),
    })
}

fn rpc_self_healing_repair_attempt_data(
    repair: &SelfHealingEditRepairAttempt,
) -> serde_json::Value {
    serde_json::json!({
        "attempt": repair.attempt,
        "edits": repair
            .replacements
            .iter()
            .map(|replacement| serde_json::json!({
                "oldText": replacement.old_text,
                "newText": replacement.new_text,
            }))
            .collect::<Vec<_>>(),
        "diagnostics": repair
            .diagnostics
            .iter()
            .map(|diagnostic| serde_json::json!({ "message": diagnostic.message }))
            .collect::<Vec<_>>(),
        "checkOutput": repair
            .check_output
            .as_ref()
            .map(rpc_self_healing_check_output_data),
    })
}

fn rpc_self_healing_check_output_data(output: &SelfHealingEditCheckOutput) -> serde_json::Value {
    serde_json::json!({
        "command": output.command,
        "stdout": output.stdout,
        "stderr": output.stderr,
        "exitCode": output.exit_code,
    })
}

fn rpc_agent_profiles_data(session: &CodingAgentSession) -> serde_json::Value {
    let view = session.view();
    let default_profile_id = view.default_agent_profile_id;
    let agents = session
        .agent_profiles()
        .into_iter()
        .map(|profile| rpc_agent_profile(&profile))
        .collect::<Vec<_>>();

    serde_json::json!({
        "defaultAgentProfileId": default_profile_id.as_str(),
        "agents": agents,
        "diagnostics": rpc_profile_diagnostics(session),
    })
}

fn rpc_team_profiles_data(session: &CodingAgentSession) -> serde_json::Value {
    let teams = session
        .team_profiles()
        .into_iter()
        .map(|profile| rpc_team_profile(&profile))
        .collect::<Vec<_>>();

    serde_json::json!({
        "teams": teams,
        "diagnostics": rpc_profile_diagnostics(session),
    })
}

fn rpc_agent_profile(profile: &CodingAgentAgentProfileSummary) -> serde_json::Value {
    serde_json::json!({
        "id": profile.id.as_str(),
        "displayName": profile.display_name,
        "description": profile.description.as_deref(),
        "source": rpc_profile_source(profile.source),
        "isDefault": profile.is_default,
        "model": profile.model_id.as_deref(),
        "tools": profile.tools,
        "skills": profile.skills,
        "supervision": rpc_supervision_policy(&profile.supervision),
        "delegation": rpc_delegation_policy(&profile.delegation),
    })
}

fn rpc_team_profile(profile: &CodingAgentTeamProfileSummary) -> serde_json::Value {
    serde_json::json!({
        "id": profile.id.as_str(),
        "displayName": profile.display_name,
        "description": profile.description.as_deref(),
        "source": rpc_profile_source(profile.source),
        "supervisor": rpc_team_supervisor(&profile.supervisor),
        "strategy": rpc_team_strategy(&profile.strategy),
        "members": rpc_profile_id_list(&profile.members),
        "delegation": rpc_delegation_policy(&profile.delegation),
    })
}

pub(super) fn rpc_pending_delegation_confirmation(
    pending: &PendingDelegationConfirmation,
) -> serde_json::Value {
    serde_json::json!({
        "operationId": pending.operation_id,
        "turnId": pending.turn_id,
        "toolCallId": pending.tool_call_id,
        "requestingProfileId": pending.requesting_profile_id.as_str(),
        "targetKind": rpc_profile_kind(pending.target_kind),
        "targetId": pending.target_id.as_str(),
        "task": pending.task,
        "reason": pending.reason,
    })
}

fn rpc_profile_diagnostics(session: &CodingAgentSession) -> Vec<serde_json::Value> {
    session
        .profile_diagnostics()
        .into_iter()
        .map(|diagnostic| rpc_profile_diagnostic(&diagnostic))
        .collect()
}

fn rpc_profile_diagnostic(diagnostic: &CodingAgentPublicDiagnostic) -> serde_json::Value {
    serde_json::json!({
        "severity": diagnostic.severity,
        "code": diagnostic.code,
        "summary": diagnostic.summary,
        "origin": diagnostic.origin,
        "operationId": diagnostic.operation_id,
    })
}

fn rpc_delegation_policy(policy: &DelegationPolicy) -> serde_json::Value {
    serde_json::json!({
        "allowDelegateAgent": policy.allow_delegate_agent,
        "allowDelegateTeam": policy.allow_delegate_team,
        "maxDepth": policy.max_depth,
        "maxParallelChildren": policy.max_parallel_children,
        "requireConfirmation": rpc_delegation_confirmation_mode(&policy.require_confirmation),
        "allowedAgents": rpc_profile_id_list(&policy.allowed_agents),
        "allowedTeams": rpc_profile_id_list(&policy.allowed_teams),
    })
}

fn rpc_profile_id_list(ids: &[ProfileId]) -> Vec<&str> {
    ids.iter().map(ProfileId::as_str).collect()
}

fn rpc_team_supervisor(supervisor: &TeamSupervisor) -> serde_json::Value {
    match supervisor {
        TeamSupervisor::Deterministic => serde_json::json!({ "mode": "deterministic" }),
        TeamSupervisor::Agent(profile_id) => serde_json::json!({
            "mode": "agent",
            "profileId": profile_id.as_str(),
        }),
    }
}

fn rpc_profile_source(source: ProfileSource) -> &'static str {
    match source {
        ProfileSource::BuiltIn => "built_in",
        ProfileSource::User => "user",
        ProfileSource::Project => "project",
    }
}

fn rpc_profile_kind(kind: ProfileKind) -> &'static str {
    match kind {
        ProfileKind::Agent => "agent",
        ProfileKind::Team => "team",
    }
}

fn rpc_supervision_policy(policy: &SupervisionPolicy) -> &'static str {
    match policy {
        SupervisionPolicy::Session => "session",
        SupervisionPolicy::SelfReview => "self_review",
        SupervisionPolicy::LlmSupervisor => "llm_supervisor",
    }
}

fn rpc_delegation_confirmation_mode(mode: &DelegationConfirmationMode) -> &'static str {
    match mode {
        DelegationConfirmationMode::Never => "never",
        DelegationConfirmationMode::Writes => "writes",
        DelegationConfirmationMode::Always => "always",
    }
}

fn rpc_team_strategy(strategy: &TeamStrategy) -> &'static str {
    match strategy {
        TeamStrategy::PlanExecuteReview => "plan_execute_review",
    }
}

fn rpc_transcript_item(item: CodingAgentSessionTranscriptItem) -> serde_json::Value {
    match item {
        CodingAgentSessionTranscriptItem::User { text } => {
            serde_json::json!({"role": "user", "content": text})
        }
        CodingAgentSessionTranscriptItem::Assistant {
            id,
            text,
            thinking,
            images,
            done,
            reasoning_duration_millis,
        } => serde_json::json!({
            "role": "assistant",
            "id": id,
            "content": text,
            "thinking": thinking,
            "images": images,
            "done": done,
            "reasoningDurationMillis": reasoning_duration_millis,
        }),
        CodingAgentSessionTranscriptItem::Tool {
            call_id,
            name,
            args,
            result,
            is_error,
            duration_millis,
        } => serde_json::json!({
            "role": "tool",
            "callId": call_id,
            "name": name,
            "arguments": args,
            "result": result,
            "isError": is_error,
            "durationMillis": duration_millis,
        }),
        CodingAgentSessionTranscriptItem::Delegation {
            tool_call_id,
            requesting_profile_id,
            target_kind,
            target_id,
            task,
            status,
            child_operation_id,
            summary,
        } => serde_json::json!({
            "role": "delegation",
            "toolCallId": tool_call_id,
            "requestingProfileId": requesting_profile_id.as_str(),
            "targetKind": rpc_profile_kind(target_kind),
            "targetId": target_id.as_str(),
            "task": task,
            "status": status,
            "childOperationId": child_operation_id,
            "summary": summary,
        }),
        CodingAgentSessionTranscriptItem::CompactionSummary { summary } => {
            serde_json::json!({"role": "compactionSummary", "summary": summary})
        }
        CodingAgentSessionTranscriptItem::BranchSummary { summary } => {
            serde_json::json!({"role": "branchSummary", "summary": summary})
        }
        CodingAgentSessionTranscriptItem::Diagnostic { message } => {
            serde_json::json!({"role": "diagnostic", "message": message})
        }
    }
}

pub(super) fn has_images(images: &Option<Vec<CodingAgentPromptImage>>) -> bool {
    images.as_ref().is_some_and(|images| !images.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rpc_tool_transcript_projects_optional_authoritative_duration() {
        let item = CodingAgentSessionTranscriptItem::Tool {
            call_id: "tool-1".into(),
            name: "read".into(),
            args: serde_json::json!({"path": "src/lib.rs"}),
            result: Some("ok".into()),
            is_error: false,
            duration_millis: Some(1_250),
        };

        assert_eq!(rpc_transcript_item(item)["durationMillis"], 1_250);
    }

    #[test]
    fn rpc_assistant_transcript_projects_optional_reasoning_duration() {
        let item = CodingAgentSessionTranscriptItem::Assistant {
            id: "message-1".into(),
            text: "answer".into(),
            thinking: "reasoning".into(),
            images: Vec::new(),
            done: true,
            reasoning_duration_millis: Some(2_430),
        };

        assert_eq!(rpc_transcript_item(item)["reasoningDurationMillis"], 2_430);
    }
}
