use super::*;

impl RpcState {
    pub(in crate::rpc) async fn handle_command<W>(
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
                    shutdown_handle.request_shutdown()?;
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
                            serde_json::to_value(self.session_state()?)
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
                    .retry_recovery(request)
                    .await?;
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
                    .resolve_recovery(request)
                    .await?;
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
}
