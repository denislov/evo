use super::*;

impl RuntimeCommandClient {
    pub fn try_reload(
        &self,
        command_id: u64,
        target: DesktopRuntimeOwnerTarget,
    ) -> Result<(), DesktopCommandAdmissionError> {
        validate_runtime_owner_target(&target)?;
        self.try_send(DesktopRuntimeCommand::Reload { command_id, target })
    }

    pub fn try_resync(&self, command_id: u64) -> Result<(), DesktopCommandAdmissionError> {
        self.try_send(DesktopRuntimeCommand::Resync {
            command_id,
            session_id: None,
        })
    }

    pub fn try_create_session(&self, command_id: u64) -> Result<(), DesktopCommandAdmissionError> {
        self.try_send(DesktopRuntimeCommand::CreateSession { command_id })
    }

    pub fn try_open_session(
        &self,
        command_id: u64,
        session_id: &str,
    ) -> Result<(), DesktopCommandAdmissionError> {
        validate_session_id(session_id)?;
        self.try_send(DesktopRuntimeCommand::OpenSession {
            command_id,
            session_id: session_id.to_owned(),
        })
    }

    pub fn try_close_session(
        &self,
        command_id: u64,
        session_id: &str,
    ) -> Result<(), DesktopCommandAdmissionError> {
        validate_session_id(session_id)?;
        self.try_send(DesktopRuntimeCommand::CloseSession {
            command_id,
            session_id: session_id.to_owned(),
        })
    }

    pub fn try_delete_session(
        &self,
        command_id: u64,
        session_id: &str,
    ) -> Result<(), DesktopCommandAdmissionError> {
        validate_session_id(session_id)?;
        self.try_send(DesktopRuntimeCommand::DeleteSession {
            command_id,
            session_id: session_id.to_owned(),
        })
    }

    pub fn try_list_sessions(&self, command_id: u64) -> Result<(), DesktopCommandAdmissionError> {
        self.try_send(DesktopRuntimeCommand::ListSessions { command_id })
    }

    pub fn try_rename_session(
        &self,
        command_id: u64,
        session_id: &str,
        name: Option<&str>,
    ) -> Result<(), DesktopCommandAdmissionError> {
        validate_session_id(session_id)?;
        validate_session_name(name)?;
        self.try_send(DesktopRuntimeCommand::RenameSession {
            command_id,
            session_id: session_id.to_owned(),
            name: name
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .map(str::to_owned),
        })
    }

    pub fn try_select_model(
        &self,
        command_id: u64,
        target: DesktopRuntimeOwnerTarget,
        model_id: &str,
        thinking_level: Option<CodingAgentThinkingLevel>,
    ) -> Result<(), DesktopCommandAdmissionError> {
        validate_runtime_owner_target(&target)?;
        validate_selection_id("model", model_id)?;
        self.try_send(DesktopRuntimeCommand::SelectModel {
            command_id,
            target,
            model_id: model_id.to_owned(),
            thinking_level,
        })
    }

    pub fn try_select_session_profile(
        &self,
        command_id: u64,
        target: DesktopRuntimeOwnerTarget,
        profile_id: &str,
    ) -> Result<(), DesktopCommandAdmissionError> {
        validate_runtime_owner_target(&target)?;
        validate_selection_id("profile", profile_id)?;
        self.try_send(DesktopRuntimeCommand::SelectSessionProfile {
            command_id,
            target,
            profile_id: profile_id.to_owned(),
        })
    }

    pub fn try_submit_prompt_with_attachments(
        &self,
        command_id: u64,
        target: DesktopPromptTarget,
        prompt: &str,
        attachments: &[PathBuf],
        thinking_level: Option<CodingAgentThinkingLevel>,
    ) -> Result<(), DesktopCommandAdmissionError> {
        self.try_send(admitted_prompt_command(
            command_id,
            target,
            prompt,
            attachments,
            thinking_level,
        )?)
    }

    pub fn try_abort_for_session(
        &self,
        command_id: u64,
        session_id: &str,
    ) -> Result<(), DesktopCommandAdmissionError> {
        validate_session_id(session_id)?;
        self.try_send(DesktopRuntimeCommand::Abort {
            command_id,
            session_id: Some(session_id.to_owned()),
        })
    }

    pub fn try_steer_for_session(
        &self,
        command_id: u64,
        session_id: &str,
        text: &str,
    ) -> Result<(), DesktopCommandAdmissionError> {
        validate_session_id(session_id)?;
        validate_control_text(text)?;
        self.try_send(DesktopRuntimeCommand::Steer {
            command_id,
            session_id: Some(session_id.to_owned()),
            text: text.to_owned(),
        })
    }

    pub fn try_follow_up_for_session(
        &self,
        command_id: u64,
        session_id: &str,
        text: &str,
    ) -> Result<(), DesktopCommandAdmissionError> {
        validate_session_id(session_id)?;
        validate_control_text(text)?;
        self.try_send(DesktopRuntimeCommand::FollowUp {
            command_id,
            session_id: Some(session_id.to_owned()),
            text: text.to_owned(),
        })
    }

    pub fn try_decide_tool_authorization(
        &self,
        command_id: u64,
        identity: &ToolAuthorizationIdentity,
        decision: ToolAuthorizationDecision,
    ) -> Result<(), DesktopCommandAdmissionError> {
        validate_authorization_identity(identity)?;
        self.try_send(DesktopRuntimeCommand::DecideToolAuthorization {
            command_id,
            session_id: None,
            identity: identity.clone(),
            decision,
        })
    }

    pub fn try_retry_recovery(
        &self,
        command_id: u64,
        identity: &DesktopRecoveryIdentity,
    ) -> Result<(), DesktopCommandAdmissionError> {
        validate_recovery_identity(identity)?;
        self.try_send(DesktopRuntimeCommand::RetryRecovery {
            command_id,
            session_id: None,
            identity: identity.clone(),
        })
    }

    pub fn try_resolve_recovery(
        &self,
        command_id: u64,
        identity: &DesktopRecoveryIdentity,
        resolution: CodingAgentRecoveryResolution,
    ) -> Result<(), DesktopCommandAdmissionError> {
        validate_recovery_identity(identity)?;
        self.try_send(DesktopRuntimeCommand::ResolveRecovery {
            command_id,
            session_id: None,
            identity: identity.clone(),
            resolution,
        })
    }

    pub fn try_open_change(
        &self,
        command_id: u64,
        session_id: &str,
        request: &CodingAgentFileReviewRequest,
    ) -> Result<(), DesktopCommandAdmissionError> {
        validate_session_id(session_id)?;
        validate_file_review_request(request)?;
        self.try_send(DesktopRuntimeCommand::OpenChange {
            command_id,
            session_id: session_id.to_owned(),
            request: request.clone(),
        })
    }

    pub fn try_open_external_editor(
        &self,
        command_id: u64,
        session_id: &str,
        target: &CodingAgentExternalEditorTarget,
    ) -> Result<(), DesktopCommandAdmissionError> {
        validate_session_id(session_id)?;
        self.try_send(DesktopRuntimeCommand::OpenExternalEditor {
            command_id,
            session_id: session_id.to_owned(),
            target: target.clone(),
        })
    }

    pub fn try_list_merge_proposals(
        &self,
        command_id: u64,
        session_id: &str,
    ) -> Result<(), DesktopCommandAdmissionError> {
        validate_session_id(session_id)?;
        self.try_send(DesktopRuntimeCommand::ListMergeProposals {
            command_id,
            session_id: session_id.to_owned(),
        })
    }

    pub fn try_merge_child_worktree(
        &self,
        command_id: u64,
        session_id: &str,
        worktree_id: &str,
    ) -> Result<(), DesktopCommandAdmissionError> {
        validate_session_id(session_id)?;
        validate_selection_id("worktree", worktree_id)?;
        self.try_send(DesktopRuntimeCommand::MergeChildWorktree {
            command_id,
            session_id: session_id.to_owned(),
            worktree_id: worktree_id.to_owned(),
        })
    }

    pub fn try_discard_child_worktree(
        &self,
        command_id: u64,
        session_id: &str,
        worktree_id: &str,
    ) -> Result<(), DesktopCommandAdmissionError> {
        validate_session_id(session_id)?;
        validate_selection_id("worktree", worktree_id)?;
        self.try_send(DesktopRuntimeCommand::DiscardChildWorktree {
            command_id,
            session_id: session_id.to_owned(),
            worktree_id: worktree_id.to_owned(),
        })
    }

    fn try_send(&self, command: DesktopRuntimeCommand) -> Result<(), DesktopCommandAdmissionError> {
        self.commands
            .try_send(command)
            .map_err(|error| match error {
                mpsc::error::TrySendError::Full(_) => DesktopCommandAdmissionError::QueueFull,
                mpsc::error::TrySendError::Closed(_) => DesktopCommandAdmissionError::RuntimeClosed,
            })
    }
}
