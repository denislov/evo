use super::*;

impl RpcState {
    pub(in crate::rpc) async fn handle_compact<W>(
        &mut self,
        id: Option<String>,
        custom_instructions: Option<String>,
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
                    "compact",
                    "cannot compact while another operation is running",
                ),
            )
            .await?;
            return Ok(());
        }
        if !self.application.session_bootstrap.is_persistent() {
            write_rpc_response(
                writer,
                RpcResponse::error(
                    id,
                    "compact",
                    "manual compaction requires a persistent Rust-native session",
                ),
            )
            .await?;
            return Ok(());
        }

        let (mut session, session_storage) = self.take_or_open_coding_session().await?;
        self.ensure_client_connection(&session)?;
        self.ensure_session_event_pump(&session)?;
        let event_flush = self
            .session_event_flush
            .as_ref()
            .expect("session event pump installed")
            .clone();
        let operation = self.application.compact_operation(custom_instructions);
        let (done_tx, done_rx) = oneshot::channel();

        write_rpc_response(writer, RpcResponse::success(id, "compact", None)).await?;

        let shutdown_handle = session.runtime_shutdown_handle();
        self.active_shutdown_handle.get_or_insert(shutdown_handle);
        tokio::spawn(async move {
            let outcome =
                session
                    .run(operation)
                    .await
                    .map_err(CliError::from)
                    .map(|operation_outcome| {
                        operation_outcome.into_compact().expect(
                            "manual compaction operation returned a different public outcome",
                        )
                    });
            flush_session_product_events(event_flush).await;
            let _ = done_tx.send(CodingOperationTaskResult {
                session: Some(session),
                session_storage,
                outcome: CodingOperationOutcome::Compact(outcome),
            });
        });

        self.is_compacting = true;
        self.foreground = Some(RpcForegroundOperation {
            done: done_rx,
            operation_kind: OperationKind::Compact,
            idempotency_key: None,
        });
        Ok(())
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "RPC prompt fields and stream cursor remain explicit protocol inputs"
    )]
    pub(in crate::rpc) async fn handle_prompt<W>(
        &mut self,
        id: Option<String>,
        message: String,
        images: Option<Vec<CodingAgentPromptImage>>,
        streaming_behavior: Option<StreamingBehavior>,
        after_snapshot_cursor: Option<CodingAgentSnapshotCursor>,
        idempotency_key: Option<String>,
        writer: &mut W,
    ) -> Result<(), CliError>
    where
        W: AsyncWrite + Unpin,
    {
        let idempotency_key = match self.parse_idempotency_key(idempotency_key) {
            Ok(key) => key,
            Err(error) => {
                write_rpc_response(writer, rpc_cli_error(id, "prompt", &error)).await?;
                return Ok(());
            }
        };
        if self.has_active_operations() && after_snapshot_cursor.is_some() {
            self.handle_streaming_prompt(
                id,
                message,
                images,
                streaming_behavior,
                after_snapshot_cursor,
                writer,
            )
            .await?;
            return Ok(());
        }
        match self.idempotent_retry_response(idempotency_key.as_ref(), "prompt") {
            Ok(Some(data)) => {
                write_rpc_response(writer, RpcResponse::success(id, "prompt", Some(data))).await?;
                return Ok(());
            }
            Ok(None) => {}
            Err(error) => {
                write_rpc_response(writer, rpc_cli_error(id, "prompt", &error)).await?;
                return Ok(());
            }
        }

        if self.is_streaming() {
            self.handle_streaming_prompt(
                id,
                message,
                images,
                streaming_behavior,
                after_snapshot_cursor,
                writer,
            )
            .await?;
            return Ok(());
        }

        let prompt = match self
            .application
            .prepare_prompt_with_images(message.clone(), images.unwrap_or_default())
        {
            Ok(prompt) => prompt,
            Err(error) => {
                write_rpc_response(writer, rpc_public_error(id, "prompt", error)).await?;
                return Ok(());
            }
        };
        self.start_coding_session_prompt(id, prompt, idempotency_key, writer)
            .await
    }

    async fn handle_streaming_prompt<W>(
        &mut self,
        id: Option<String>,
        message: String,
        images: Option<Vec<CodingAgentPromptImage>>,
        streaming_behavior: Option<StreamingBehavior>,
        after_snapshot_cursor: Option<CodingAgentSnapshotCursor>,
        writer: &mut W,
    ) -> Result<(), CliError>
    where
        W: AsyncWrite + Unpin,
    {
        if let Some(cursor) = after_snapshot_cursor {
            let replayed = match reconnect_running_prompt_after(self, &cursor).await {
                Ok(replayed) => replayed,
                Err(error) if error.code() == "event_stream_gap" => {
                    write_rpc_response(writer, rpc_public_error(id, "prompt", error)).await?;
                    return Ok(());
                }
                Err(error) => return Err(CliError::from(error)),
            };

            if streaming_behavior.is_none() {
                if has_images(&images) {
                    write_rpc_response(
                        writer,
                        RpcResponse::error(
                            id,
                            "prompt",
                            "reconnect-only prompt requests cannot include image content",
                        ),
                    )
                    .await?;
                    return Ok(());
                }
                write_rpc_response(writer, RpcResponse::success(id, "prompt", None)).await?;
                for event in replayed {
                    write_json_line(writer, &event).await?;
                }
                return Ok(());
            }

            self.handle_streaming_prompt_control(id, message, images, streaming_behavior, writer)
                .await?;
            for event in replayed {
                write_json_line(writer, &event).await?;
            }
            return Ok(());
        }

        self.handle_streaming_prompt_control(id, message, images, streaming_behavior, writer)
            .await
    }

    async fn handle_streaming_prompt_control<W>(
        &mut self,
        id: Option<String>,
        message: String,
        images: Option<Vec<CodingAgentPromptImage>>,
        streaming_behavior: Option<StreamingBehavior>,
        writer: &mut W,
    ) -> Result<(), CliError>
    where
        W: AsyncWrite + Unpin,
    {
        let Some(foreground) = self.foreground.as_ref() else {
            write_rpc_response(
                writer,
                RpcResponse::error(id, "prompt", "agent is not streaming"),
            )
            .await?;
            return Ok(());
        };

        if foreground.operation_kind != OperationKind::Prompt {
            write_rpc_response(
                writer,
                RpcResponse::error(
                    id,
                    "prompt",
                    format!(
                        "cannot send prompt control while {} is running",
                        foreground.operation_kind.as_str()
                    ),
                ),
            )
            .await?;
            return Ok(());
        }
        let Some(connection) = self.client_connection.as_ref() else {
            write_rpc_response(
                writer,
                RpcResponse::error(id, "prompt", "agent is not streaming"),
            )
            .await?;
            return Ok(());
        };
        let Some(submitted) = connection.state()?.submitted_operation else {
            write_rpc_response(
                writer,
                RpcResponse::error(id, "prompt", "agent is not streaming"),
            )
            .await?;
            return Ok(());
        };
        let control = connection.prompt_control(submitted.operation_id);
        let control_id = CodingAgentControlId(
            id.clone()
                .unwrap_or_else(|| format!("rpc-prompt-control-{message}")),
        );

        let prompt = match self
            .application
            .prepare_prompt_with_images(message.clone(), images.unwrap_or_default())
        {
            Ok(prompt) => prompt,
            Err(error) => {
                write_rpc_response(writer, rpc_public_error(id, "prompt", error)).await?;
                return Ok(());
            }
        };
        let result = match streaming_behavior {
            Some(StreamingBehavior::Steer) => control.steer_prepared(control_id, prompt),
            Some(StreamingBehavior::FollowUp) => control.follow_up_prepared(control_id, prompt),
            None => {
                write_rpc_response(
                    writer,
                    RpcResponse::error(
                        id,
                        "prompt",
                        "agent is streaming; prompt requires streamingBehavior steer or followUp",
                    ),
                )
                .await?;
                return Ok(());
            }
        };

        match result {
            Ok(_) => write_rpc_response(writer, RpcResponse::success(id, "prompt", None)).await,
            Err(rejection) => {
                write_rpc_response(
                    writer,
                    RpcResponse::error(id, "prompt", format!("{:?}", rejection.reason)),
                )
                .await
            }
        }
    }

    pub(in crate::rpc) async fn handle_invoke_agent<W>(
        &mut self,
        id: Option<String>,
        profile_id: String,
        task: String,
        idempotency_key: Option<String>,
        writer: &mut W,
    ) -> Result<(), CliError>
    where
        W: AsyncWrite + Unpin,
    {
        let idempotency_key = match self.parse_idempotency_key(idempotency_key) {
            Ok(key) => key,
            Err(error) => {
                write_rpc_response(writer, rpc_cli_error(id, "invoke_agent", &error)).await?;
                return Ok(());
            }
        };
        match self.idempotent_retry_response(idempotency_key.as_ref(), "invoke_agent") {
            Ok(Some(data)) => {
                write_rpc_response(writer, RpcResponse::success(id, "invoke_agent", Some(data)))
                    .await?;
                return Ok(());
            }
            Ok(None) => {}
            Err(error) => {
                write_rpc_response(writer, rpc_cli_error(id, "invoke_agent", &error)).await?;
                return Ok(());
            }
        }

        if task.trim().is_empty() {
            write_rpc_response(
                writer,
                RpcResponse::error(
                    id,
                    "invoke_agent",
                    "agent invocation requires a non-empty task",
                ),
            )
            .await?;
            return Ok(());
        }

        let profile_id = match ProfileId::new(profile_id) {
            Ok(profile_id) => profile_id,
            Err(message) => {
                write_rpc_response(writer, RpcResponse::error(id, "invoke_agent", message)).await?;
                return Ok(());
            }
        };

        let (mut session, session_storage) = self.take_or_open_coding_session().await?;

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
                    "invoke_agent",
                    format!("Unknown agent profile: {profile_id}"),
                ),
            )
            .await?;
            return Ok(());
        }

        let operation = self
            .application
            .agent_invocation_operation(profile_id.clone(), task.clone());
        let connection = self.ensure_client_connection(&session)?;
        self.ensure_session_event_pump(&session)?;
        let event_flush = self
            .session_event_flush
            .as_ref()
            .expect("session event pump installed")
            .clone();
        let invocation = match session.submit(operation) {
            Ok(invocation) => invocation,
            Err(error) => {
                self.coding_session = Some(session);
                write_rpc_response(writer, rpc_public_error(id, "invoke_agent", error)).await?;
                return Ok(());
            }
        };
        let operation_id = invocation.operation_id().to_owned();
        invocation.bind_control_owner(&connection)?;

        write_rpc_response(
            writer,
            RpcResponse::success(
                id,
                "invoke_agent",
                Some(serde_json::json!({
                    "operationId": operation_id,
                    "profileId": profile_id.as_str(),
                    "task": task,
                })),
            ),
        )
        .await?;
        write_json_line(writer, &ProtocolEvent::agent_start()).await?;

        let running_idempotency_key = idempotency_key.clone();
        self.remember_idempotency_key(
            idempotency_key,
            "invoke_agent",
            OperationKind::AgentInvocation,
        );

        let shutdown_handle = session.runtime_shutdown_handle();
        self.active_shutdown_handle.get_or_insert(shutdown_handle);
        self.coding_session = Some(session);
        let completion_tx = self.background_completion_tx.clone();
        let completion_operation_id = operation_id.clone();
        tokio::spawn(async move {
            let outcome =
                invocation
                    .join()
                    .await
                    .map_err(CliError::from)
                    .map(|operation_outcome| {
                        operation_outcome.into_agent_invocation().expect(
                            "agent invocation operation returned a different public outcome",
                        )
                    });
            flush_session_product_events(event_flush).await;

            let _ = completion_tx
                .send(RpcBackgroundCompletion {
                    operation_id: completion_operation_id,
                    result: CodingOperationTaskResult {
                        session: None,
                        session_storage,
                        outcome: CodingOperationOutcome::AgentInvocation(outcome),
                    },
                })
                .await;
        });

        self.background_operations.insert(
            operation_id,
            RpcBackgroundOperation {
                operation_kind: OperationKind::AgentInvocation,
                idempotency_key: running_idempotency_key,
            },
        );

        Ok(())
    }

    pub(in crate::rpc) async fn handle_invoke_team<W>(
        &mut self,
        id: Option<String>,
        team_id: String,
        task: String,
        idempotency_key: Option<String>,
        writer: &mut W,
    ) -> Result<(), CliError>
    where
        W: AsyncWrite + Unpin,
    {
        let idempotency_key = match self.parse_idempotency_key(idempotency_key) {
            Ok(key) => key,
            Err(error) => {
                write_rpc_response(writer, rpc_cli_error(id, "invoke_team", &error)).await?;
                return Ok(());
            }
        };
        match self.idempotent_retry_response(idempotency_key.as_ref(), "invoke_team") {
            Ok(Some(data)) => {
                write_rpc_response(writer, RpcResponse::success(id, "invoke_team", Some(data)))
                    .await?;
                return Ok(());
            }
            Ok(None) => {}
            Err(error) => {
                write_rpc_response(writer, rpc_cli_error(id, "invoke_team", &error)).await?;
                return Ok(());
            }
        }

        if task.trim().is_empty() {
            write_rpc_response(
                writer,
                RpcResponse::error(
                    id,
                    "invoke_team",
                    "agent team invocation requires a non-empty task",
                ),
            )
            .await?;
            return Ok(());
        }

        let team_id = match ProfileId::new(team_id) {
            Ok(team_id) => team_id,
            Err(message) => {
                write_rpc_response(writer, RpcResponse::error(id, "invoke_team", message)).await?;
                return Ok(());
            }
        };

        let (mut session, session_storage) = self.take_or_open_coding_session().await?;

        if !session
            .team_profiles()
            .iter()
            .any(|team| team.id.as_str() == team_id.as_str())
        {
            self.coding_session = Some(session);
            write_rpc_response(
                writer,
                RpcResponse::error(
                    id,
                    "invoke_team",
                    format!("Unknown team profile: {team_id}"),
                ),
            )
            .await?;
            return Ok(());
        }

        let operation = self
            .application
            .team_invocation_operation(team_id.clone(), task.clone());
        let connection = self.ensure_client_connection(&session)?;
        self.ensure_session_event_pump(&session)?;
        let event_flush = self
            .session_event_flush
            .as_ref()
            .expect("session event pump installed")
            .clone();
        let invocation = match session.submit(operation) {
            Ok(invocation) => invocation,
            Err(error) => {
                self.coding_session = Some(session);
                write_rpc_response(writer, rpc_public_error(id, "invoke_team", error)).await?;
                return Ok(());
            }
        };
        let operation_id = invocation.operation_id().to_owned();
        invocation.bind_control_owner(&connection)?;

        write_rpc_response(
            writer,
            RpcResponse::success(
                id,
                "invoke_team",
                Some(serde_json::json!({
                    "operationId": operation_id,
                    "teamId": team_id.as_str(),
                    "task": task,
                })),
            ),
        )
        .await?;
        write_json_line(writer, &ProtocolEvent::agent_start()).await?;

        let running_idempotency_key = idempotency_key.clone();
        self.remember_idempotency_key(idempotency_key, "invoke_team", OperationKind::AgentTeam);

        let shutdown_handle = session.runtime_shutdown_handle();
        self.active_shutdown_handle.get_or_insert(shutdown_handle);
        self.coding_session = Some(session);
        let completion_tx = self.background_completion_tx.clone();
        let completion_operation_id = operation_id.clone();
        tokio::spawn(async move {
            let outcome =
                invocation
                    .join()
                    .await
                    .map_err(CliError::from)
                    .map(|operation_outcome| {
                        operation_outcome
                            .into_agent_team()
                            .expect("agent team operation returned a different public outcome")
                    });
            flush_session_product_events(event_flush).await;

            let _ = completion_tx
                .send(RpcBackgroundCompletion {
                    operation_id: completion_operation_id,
                    result: CodingOperationTaskResult {
                        session: None,
                        session_storage,
                        outcome: CodingOperationOutcome::AgentTeam(outcome),
                    },
                })
                .await;
        });

        self.background_operations.insert(
            operation_id,
            RpcBackgroundOperation {
                operation_kind: OperationKind::AgentTeam,
                idempotency_key: running_idempotency_key,
            },
        );

        Ok(())
    }

    pub(in crate::rpc) async fn handle_approve_delegation<W>(
        &mut self,
        id: Option<String>,
        operation_id: String,
        tool_call_id: String,
        idempotency_key: Option<String>,
        writer: &mut W,
    ) -> Result<(), CliError>
    where
        W: AsyncWrite + Unpin,
    {
        let idempotency_key = match self.parse_idempotency_key(idempotency_key) {
            Ok(key) => key,
            Err(error) => {
                write_rpc_response(writer, rpc_cli_error(id, "approve_delegation", &error)).await?;
                return Ok(());
            }
        };
        match self.idempotent_retry_response(idempotency_key.as_ref(), "approve_delegation") {
            Ok(Some(data)) => {
                write_rpc_response(
                    writer,
                    RpcResponse::success(id, "approve_delegation", Some(data)),
                )
                .await?;
                return Ok(());
            }
            Ok(None) => {}
            Err(error) => {
                write_rpc_response(writer, rpc_cli_error(id, "approve_delegation", &error)).await?;
                return Ok(());
            }
        }

        if self.is_streaming() {
            write_rpc_response(
                writer,
                RpcResponse::error(
                    id,
                    "approve_delegation",
                    "cannot approve delegation while agent is streaming",
                ),
            )
            .await?;
            return Ok(());
        }

        let Some(mut session) = self.coding_session.take() else {
            write_rpc_response(
                writer,
                RpcResponse::error(id, "approve_delegation", "no active coding session"),
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
                        "approve_delegation",
                        format!(
                            "pending delegation confirmation not found: operation_id={operation_id}, tool_call_id={tool_call_id}"
                        ),
                    ),
                )
                .await?;
                return Ok(());
            }
        };
        let operation_kind = match pending.target_kind {
            ProfileKind::Agent => OperationKind::AgentInvocation,
            ProfileKind::Team => OperationKind::AgentTeam,
        };
        let session_storage = session_runtime_storage(&session)?;
        self.ensure_session_event_pump(&session)?;
        let event_flush = self
            .session_event_flush
            .as_ref()
            .expect("session event pump installed")
            .clone();
        let (done_tx, done_rx) = oneshot::channel();

        write_rpc_response(
            writer,
            RpcResponse::success(
                id,
                "approve_delegation",
                Some(serde_json::json!({
                    "delegation": rpc_pending_delegation_confirmation(&pending),
                })),
            ),
        )
        .await?;
        write_json_line(writer, &ProtocolEvent::agent_start()).await?;

        let running_idempotency_key = idempotency_key.clone();
        self.remember_idempotency_key(idempotency_key, "approve_delegation", operation_kind);

        let shutdown_handle = session.runtime_shutdown_handle();
        self.active_shutdown_handle.get_or_insert(shutdown_handle);
        tokio::spawn(async move {
            let outcome = session
                .run(CodingAgentOperation::ApproveDelegation {
                    operation_id,
                    tool_call_id,
                })
                .await
                .map_err(CliError::from)
                .map(|operation_outcome| {
                    operation_outcome
                        .into_delegation_approved()
                        .expect("delegation approval operation returned a different public outcome")
                });
            flush_session_product_events(event_flush).await;

            let _ = done_tx.send(CodingOperationTaskResult {
                session: Some(session),
                session_storage,
                outcome: CodingOperationOutcome::DelegationApproval(outcome),
            });
        });

        self.foreground = Some(RpcForegroundOperation {
            done: done_rx,
            operation_kind,
            idempotency_key: running_idempotency_key,
        });

        Ok(())
    }

    async fn start_coding_session_prompt<W>(
        &mut self,
        id: Option<String>,
        prompt: CodingAgentPreparedPrompt,
        idempotency_key: Option<RpcIdempotencyKey>,
        writer: &mut W,
    ) -> Result<(), CliError>
    where
        W: AsyncWrite + Unpin,
    {
        let (mut session, session_storage) = self.take_or_open_coding_session().await?;
        let draft_display = prompt.display_text().to_owned();
        let connection = self.ensure_client_connection(&session)?;
        let draft_id = CodingAgentDraftId("rpc-prompt".into());
        let operation = self.application.prompt_operation_with_queued_controls(
            prompt,
            self.steering.clone(),
            self.follow_up.clone(),
        );
        self.ensure_session_event_pump(&session)?;
        let event_flush = self
            .session_event_flush
            .as_ref()
            .expect("session event pump installed")
            .clone();
        let submission = connection.prepare_client_submission(
            &mut session,
            Some(CodingAgentSubmissionDraft::new(draft_id, draft_display)),
            operation,
        )?;
        let (done_tx, done_rx) = oneshot::channel();

        if let Err(error) =
            write_rpc_response(writer, RpcResponse::success(id, "prompt", None)).await
        {
            let cleanup = submission.discard(&mut session);
            self.coding_session = Some(session);
            cleanup.map_err(CliError::from)?;
            return Err(error);
        }
        if let Err(error) = write_json_line(writer, &ProtocolEvent::agent_start()).await {
            let cleanup = submission.discard(&mut session);
            self.coding_session = Some(session);
            cleanup.map_err(CliError::from)?;
            return Err(error);
        }

        let running_idempotency_key = idempotency_key.clone();
        self.remember_idempotency_key(idempotency_key, "prompt", OperationKind::Prompt);

        let shutdown_handle = session.runtime_shutdown_handle();
        self.active_shutdown_handle.get_or_insert(shutdown_handle);
        tokio::spawn(async move {
            let outcome = submission
                .run(&mut session)
                .await
                .map_err(CliError::from)
                .map(|operation_outcome| {
                    operation_outcome
                        .into_prompt()
                        .expect("prompt operation returned a different public outcome")
                });
            flush_session_product_events(event_flush).await;

            let _ = done_tx.send(CodingOperationTaskResult {
                session: Some(session),
                session_storage,
                outcome: CodingOperationOutcome::Prompt(outcome),
            });
        });

        self.foreground = Some(RpcForegroundOperation {
            done: done_rx,
            operation_kind: OperationKind::Prompt,
            idempotency_key: running_idempotency_key,
        });

        Ok(())
    }

    pub(in crate::rpc) fn enqueue_steer(
        &mut self,
        input: CodingAgentPreparedPrompt,
    ) -> Result<(), &'static str> {
        self.admit_queued_control(&input)?;
        if let Some(connection) = &self.client_connection {
            let _ = connection.enqueue_control_draft(CodingAgentDraft {
                id: CodingAgentDraftId(format!("rpc-steer-{}", self.steering.len())),
                kind: CodingAgentDraftKind::Steer,
                text: input.display_text().to_owned(),
            });
        }
        self.steering.push(input);
        Ok(())
    }

    pub(in crate::rpc) fn enqueue_follow_up(
        &mut self,
        input: CodingAgentPreparedPrompt,
    ) -> Result<(), &'static str> {
        self.admit_queued_control(&input)?;
        if let Some(connection) = &self.client_connection {
            let _ = connection.enqueue_control_draft(CodingAgentDraft {
                id: CodingAgentDraftId(format!("rpc-follow-up-{}", self.follow_up.len())),
                kind: CodingAgentDraftKind::FollowUp,
                text: input.display_text().to_owned(),
            });
        }
        self.follow_up.push(input);
        Ok(())
    }

    fn admit_queued_control(&self, input: &CodingAgentPreparedPrompt) -> Result<(), &'static str> {
        admit_queued_control(&self.steering, &self.follow_up, input)
    }
}

fn admit_queued_control(
    steering: &[CodingAgentPreparedPrompt],
    follow_up: &[CodingAgentPreparedPrompt],
    input: &CodingAgentPreparedPrompt,
) -> Result<(), &'static str> {
    let item_count = steering
        .len()
        .checked_add(follow_up.len())
        .and_then(|count| count.checked_add(1))
        .ok_or("queued_control_count")?;
    if item_count > RPC_QUEUED_CONTROL_ITEM_LIMIT {
        return Err("queued_control_count");
    }

    let retained_bytes = steering
        .iter()
        .chain(follow_up)
        .chain(std::iter::once(input))
        .try_fold(0usize, |total, input| {
            total.checked_add(queued_control_retained_bytes(input)?)
        })
        .ok_or("queued_control_bytes")?;
    if retained_bytes > RPC_QUEUED_CONTROL_BYTE_LIMIT {
        return Err("queued_control_bytes");
    }
    Ok(())
}
