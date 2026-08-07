use super::*;

impl PromptTask {
    pub(in crate::interactive) fn spawn_prompt(
        operation: CodingAgentOperation,
        draft_text: String,
        bootstrap: CodingAgentSessionBootstrap,
        existing_session: Option<CodingAgentSession>,
    ) -> Result<Self, CliError> {
        Ok(Self::spawn_coding(
            operation,
            draft_text,
            bootstrap,
            existing_session,
        ))
    }

    pub(in crate::interactive) fn spawn_compact(
        operation: CodingAgentOperation,
        bootstrap: CodingAgentSessionBootstrap,
        existing_session: Option<CodingAgentSession>,
    ) -> Result<Self, CliError> {
        Ok(Self::spawn_coding_compact(
            operation,
            bootstrap,
            existing_session,
        ))
    }

    pub(in crate::interactive) fn spawn_agent_invocation(
        operation: CodingAgentOperation,
        bootstrap: CodingAgentSessionBootstrap,
        existing_session: Option<CodingAgentSession>,
    ) -> Result<Self, CliError> {
        Ok(Self::spawn_coding_agent_invocation(
            operation,
            bootstrap,
            existing_session,
        ))
    }

    pub(in crate::interactive) fn spawn_agent_team(
        operation: CodingAgentOperation,
        bootstrap: CodingAgentSessionBootstrap,
        existing_session: Option<CodingAgentSession>,
    ) -> Result<Self, CliError> {
        Ok(Self::spawn_coding_agent_team(
            operation,
            bootstrap,
            existing_session,
        ))
    }

    pub(in crate::interactive) fn spawn_delegation_approval(
        existing_session: CodingAgentSession,
        operation_id: String,
        tool_call_id: String,
    ) -> Result<Self, CliError> {
        Ok(Self::spawn_coding_delegation_approval(
            existing_session,
            operation_id,
            tool_call_id,
        ))
    }

    pub(in crate::interactive) fn spawn_session_tree_label(
        existing_session: CodingAgentSession,
        entry_id: String,
        label: Option<String>,
    ) -> Result<Self, CliError> {
        Ok(Self::spawn_coding_session_tree_label(
            existing_session,
            entry_id,
            label,
        ))
    }

    pub(in crate::interactive) fn spawn_merge_review(
        existing_session: CodingAgentSession,
        operation: CodingAgentOperation,
    ) -> Result<Self, CliError> {
        Ok(Self::spawn_coding_merge_review(existing_session, operation))
    }

    pub(in crate::interactive) fn spawn_delegation_rejection(
        existing_session: CodingAgentSession,
        operation_id: String,
        tool_call_id: String,
        reason: String,
    ) -> Result<Self, CliError> {
        Ok(Self::spawn_coding_delegation_rejection(
            existing_session,
            operation_id,
            tool_call_id,
            reason,
        ))
    }

    pub(in crate::interactive) fn spawn_self_healing_edit(
        operation: CodingAgentOperation,
        bootstrap: CodingAgentSessionBootstrap,
        existing_session: Option<CodingAgentSession>,
    ) -> Result<Self, CliError> {
        Ok(Self::spawn_coding_self_healing_edit(
            operation,
            bootstrap,
            existing_session,
        ))
    }

    pub(in crate::interactive) fn spawn_branch_summary(
        operation: CodingAgentOperation,
        bootstrap: CodingAgentSessionBootstrap,
        existing_session: Option<CodingAgentSession>,
    ) -> Result<Self, CliError> {
        Ok(Self::spawn_coding_branch_summary(
            operation,
            bootstrap,
            existing_session,
        ))
    }

    pub(in crate::interactive) fn spawn_branch_summary_navigation(
        operation: CodingAgentOperation,
        bootstrap: CodingAgentSessionBootstrap,
        existing_session: Option<CodingAgentSession>,
        target_leaf_id: String,
    ) -> Result<Self, CliError> {
        Ok(Self::spawn_coding_branch_summary_navigation(
            operation,
            bootstrap,
            existing_session,
            target_leaf_id,
        ))
    }

    pub(in crate::interactive) fn spawn_fork_session(
        operation: CodingAgentOperation,
        bootstrap: CodingAgentSessionBootstrap,
        existing_session: Option<CodingAgentSession>,
        completion_notice: Option<String>,
    ) -> Result<Self, CliError> {
        Ok(Self::spawn_coding_fork_session(
            operation,
            bootstrap,
            existing_session,
            completion_notice,
        ))
    }

    pub(in crate::interactive) async fn abort_once(&mut self) {
        if self.abort_requested {
            return;
        }
        match &mut self.control {
            PromptTaskControlHandle::Prompt(control) => {
                let _ = control.send(PromptTaskControl::Abort).await;
            }
            PromptTaskControlHandle::Operation(control) => {
                let _ = control.send(PromptTaskControl::Abort).await;
            }
            PromptTaskControlHandle::AbortOnly(abort) => {
                if let Some(abort) = abort.take() {
                    let _ = abort.send(());
                }
            }
        }
        self.abort_requested = true;
    }

    pub(in crate::interactive) async fn steer(&self, text: String) -> bool {
        match &self.control {
            PromptTaskControlHandle::Prompt(control) => {
                control.send(PromptTaskControl::Steer(text)).await.is_ok()
            }
            PromptTaskControlHandle::Operation(_) | PromptTaskControlHandle::AbortOnly(_) => false,
        }
    }

    pub(in crate::interactive) async fn follow_up(&self, text: String) -> bool {
        match &self.control {
            PromptTaskControlHandle::Prompt(control) => control
                .send(PromptTaskControl::FollowUp(text))
                .await
                .is_ok(),
            PromptTaskControlHandle::Operation(_) | PromptTaskControlHandle::AbortOnly(_) => false,
        }
    }

    pub(in crate::interactive) async fn decide_tool_authorization(
        &self,
        identity: ToolAuthorizationIdentity,
        decision: ToolAuthorizationDecision,
    ) -> bool {
        match &self.control {
            PromptTaskControlHandle::Prompt(control)
            | PromptTaskControlHandle::Operation(control) => control
                .send(PromptTaskControl::DecideToolAuthorization { identity, decision })
                .await
                .is_ok(),
            PromptTaskControlHandle::AbortOnly(_) => false,
        }
    }

    fn spawn_coding(
        operation: CodingAgentOperation,
        draft_text: String,
        bootstrap: CodingAgentSessionBootstrap,
        existing_session: Option<CodingAgentSession>,
    ) -> Self {
        let (connection_tx, connection_rx) = oneshot::channel();
        let (done_tx, done_rx) = oneshot::channel();
        let (control_tx, control_rx) = mpsc::channel(PROMPT_TASK_CONTROL_CAPACITY);

        tokio::spawn(async move {
            let result = run_coding_prompt_task(
                operation,
                draft_text,
                bootstrap,
                existing_session,
                connection_tx,
                control_rx,
            )
            .await;
            let _ = done_tx.send(result);
        });

        Self {
            control: PromptTaskControlHandle::Prompt(control_tx),
            connection_handoff: Some(connection_rx),
            done: done_rx,
            abort_requested: false,
        }
    }

    fn spawn_coding_compact(
        operation: CodingAgentOperation,
        bootstrap: CodingAgentSessionBootstrap,
        existing_session: Option<CodingAgentSession>,
    ) -> Self {
        let (connection_tx, connection_rx) = oneshot::channel();
        let (done_tx, done_rx) = oneshot::channel();
        let (abort_tx, abort_rx) = oneshot::channel();

        tokio::spawn(async move {
            let result = run_coding_compact_task(
                operation,
                bootstrap,
                existing_session,
                connection_tx,
                abort_rx,
            )
            .await;
            let _ = done_tx.send(result);
        });

        Self {
            control: PromptTaskControlHandle::AbortOnly(Some(abort_tx)),
            connection_handoff: Some(connection_rx),
            done: done_rx,
            abort_requested: false,
        }
    }

    fn spawn_coding_agent_invocation(
        operation: CodingAgentOperation,
        bootstrap: CodingAgentSessionBootstrap,
        existing_session: Option<CodingAgentSession>,
    ) -> Self {
        let (connection_tx, connection_rx) = oneshot::channel();
        let (done_tx, done_rx) = oneshot::channel();
        let (control_tx, control_rx) = mpsc::channel(PROMPT_TASK_CONTROL_CAPACITY);

        tokio::spawn(async move {
            let result = run_coding_agent_invocation_task(
                operation,
                bootstrap,
                existing_session,
                connection_tx,
                control_rx,
            )
            .await;
            let _ = done_tx.send(result);
        });

        Self {
            control: PromptTaskControlHandle::Prompt(control_tx),
            connection_handoff: Some(connection_rx),
            done: done_rx,
            abort_requested: false,
        }
    }

    fn spawn_coding_agent_team(
        operation: CodingAgentOperation,
        bootstrap: CodingAgentSessionBootstrap,
        existing_session: Option<CodingAgentSession>,
    ) -> Self {
        let (connection_tx, connection_rx) = oneshot::channel();
        let (done_tx, done_rx) = oneshot::channel();
        let (control_tx, control_rx) = mpsc::channel(PROMPT_TASK_CONTROL_CAPACITY);

        tokio::spawn(async move {
            let result = run_coding_agent_team_task(
                operation,
                bootstrap,
                existing_session,
                connection_tx,
                control_rx,
            )
            .await;
            let _ = done_tx.send(result);
        });

        Self {
            control: PromptTaskControlHandle::Operation(control_tx),
            connection_handoff: Some(connection_rx),
            done: done_rx,
            abort_requested: false,
        }
    }

    fn spawn_coding_delegation_approval(
        existing_session: CodingAgentSession,
        operation_id: String,
        tool_call_id: String,
    ) -> Self {
        let (connection_tx, connection_rx) = oneshot::channel();
        let (done_tx, done_rx) = oneshot::channel();
        let (control_tx, control_rx) = mpsc::channel(PROMPT_TASK_CONTROL_CAPACITY);

        tokio::spawn(async move {
            let result = run_coding_delegation_approval_task(
                existing_session,
                operation_id,
                tool_call_id,
                connection_tx,
                control_rx,
            )
            .await;
            let _ = done_tx.send(result);
        });

        Self {
            control: PromptTaskControlHandle::Operation(control_tx),
            connection_handoff: Some(connection_rx),
            done: done_rx,
            abort_requested: false,
        }
    }

    fn spawn_coding_session_tree_label(
        existing_session: CodingAgentSession,
        entry_id: String,
        label: Option<String>,
    ) -> Self {
        let (connection_tx, connection_rx) = oneshot::channel();
        let (done_tx, done_rx) = oneshot::channel();
        let (abort_tx, abort_rx) = oneshot::channel();

        tokio::spawn(async move {
            let result = run_coding_session_tree_label_task(
                existing_session,
                entry_id,
                label,
                connection_tx,
                abort_rx,
            )
            .await;
            let _ = done_tx.send(result);
        });

        Self {
            control: PromptTaskControlHandle::AbortOnly(Some(abort_tx)),
            connection_handoff: Some(connection_rx),
            done: done_rx,
            abort_requested: false,
        }
    }

    fn spawn_coding_merge_review(
        existing_session: CodingAgentSession,
        operation: CodingAgentOperation,
    ) -> Self {
        let (connection_tx, connection_rx) = oneshot::channel();
        let (done_tx, done_rx) = oneshot::channel();
        let (abort_tx, abort_rx) = oneshot::channel();

        tokio::spawn(async move {
            let result =
                run_coding_merge_review_task(existing_session, operation, connection_tx, abort_rx)
                    .await;
            let _ = done_tx.send(result);
        });

        Self {
            control: PromptTaskControlHandle::AbortOnly(Some(abort_tx)),
            connection_handoff: Some(connection_rx),
            done: done_rx,
            abort_requested: false,
        }
    }

    fn spawn_coding_delegation_rejection(
        existing_session: CodingAgentSession,
        operation_id: String,
        tool_call_id: String,
        reason: String,
    ) -> Self {
        let (connection_tx, connection_rx) = oneshot::channel();
        let (done_tx, done_rx) = oneshot::channel();
        let (abort_tx, abort_rx) = oneshot::channel();

        tokio::spawn(async move {
            let result = run_coding_delegation_rejection_task(
                existing_session,
                operation_id,
                tool_call_id,
                reason,
                connection_tx,
                abort_rx,
            )
            .await;
            let _ = done_tx.send(result);
        });

        Self {
            control: PromptTaskControlHandle::AbortOnly(Some(abort_tx)),
            connection_handoff: Some(connection_rx),
            done: done_rx,
            abort_requested: false,
        }
    }

    fn spawn_coding_self_healing_edit(
        operation: CodingAgentOperation,
        bootstrap: CodingAgentSessionBootstrap,
        existing_session: Option<CodingAgentSession>,
    ) -> Self {
        let (connection_tx, connection_rx) = oneshot::channel();
        let (done_tx, done_rx) = oneshot::channel();
        let (abort_tx, abort_rx) = oneshot::channel();

        tokio::spawn(async move {
            let result = run_coding_self_healing_edit_task(
                operation,
                bootstrap,
                existing_session,
                connection_tx,
                abort_rx,
            )
            .await;
            let _ = done_tx.send(result);
        });

        Self {
            control: PromptTaskControlHandle::AbortOnly(Some(abort_tx)),
            connection_handoff: Some(connection_rx),
            done: done_rx,
            abort_requested: false,
        }
    }

    fn spawn_coding_branch_summary(
        operation: CodingAgentOperation,
        bootstrap: CodingAgentSessionBootstrap,
        existing_session: Option<CodingAgentSession>,
    ) -> Self {
        let (connection_tx, connection_rx) = oneshot::channel();
        let (done_tx, done_rx) = oneshot::channel();
        let (abort_tx, abort_rx) = oneshot::channel();

        tokio::spawn(async move {
            let result = run_coding_branch_summary_task(
                operation,
                bootstrap,
                existing_session,
                connection_tx,
                abort_rx,
            )
            .await;
            let _ = done_tx.send(result);
        });

        Self {
            control: PromptTaskControlHandle::AbortOnly(Some(abort_tx)),
            connection_handoff: Some(connection_rx),
            done: done_rx,
            abort_requested: false,
        }
    }

    fn spawn_coding_branch_summary_navigation(
        operation: CodingAgentOperation,
        bootstrap: CodingAgentSessionBootstrap,
        existing_session: Option<CodingAgentSession>,
        target_leaf_id: String,
    ) -> Self {
        let (connection_tx, connection_rx) = oneshot::channel();
        let (done_tx, done_rx) = oneshot::channel();
        let (abort_tx, abort_rx) = oneshot::channel();

        tokio::spawn(async move {
            let result = run_coding_branch_summary_navigation_task(
                operation,
                bootstrap,
                existing_session,
                target_leaf_id,
                connection_tx,
                abort_rx,
            )
            .await;
            let _ = done_tx.send(result);
        });

        Self {
            control: PromptTaskControlHandle::AbortOnly(Some(abort_tx)),
            connection_handoff: Some(connection_rx),
            done: done_rx,
            abort_requested: false,
        }
    }

    fn spawn_coding_fork_session(
        operation: CodingAgentOperation,
        bootstrap: CodingAgentSessionBootstrap,
        existing_session: Option<CodingAgentSession>,
        completion_notice: Option<String>,
    ) -> Self {
        let (connection_tx, connection_rx) = oneshot::channel();
        let (done_tx, done_rx) = oneshot::channel();
        let (abort_tx, abort_rx) = oneshot::channel();

        tokio::spawn(async move {
            let result = run_coding_fork_session_task(
                operation,
                bootstrap,
                existing_session,
                completion_notice,
                connection_tx,
                abort_rx,
            )
            .await;
            let _ = done_tx.send(result);
        });

        Self {
            control: PromptTaskControlHandle::AbortOnly(Some(abort_tx)),
            connection_handoff: Some(connection_rx),
            done: done_rx,
            abort_requested: false,
        }
    }
}
