use super::*;

impl PromptTurnContext {
    pub(crate) fn finish_success(
        &self,
        session_id: Option<String>,
        leaf_id: Option<String>,
    ) -> Result<InternalPromptTurnOutcome, CodingSessionError> {
        let final_message =
            self.final_message
                .clone()
                .ok_or_else(|| CodingSessionError::Session {
                    message: "prompt turn cannot finish successfully without a final message"
                        .into(),
                })?;
        Ok(InternalPromptTurnOutcome::Success {
            operation_id: self.operation_id().to_owned(),
            turn_id: self.turn_id().to_owned(),
            session_id,
            leaf_id,
            final_text: assistant_text(&final_message),
            final_message,
            diagnostics: self.diagnostics.clone(),
        })
    }

    pub(crate) fn finish_abort(
        &self,
        reason: impl Into<String>,
        session_id: Option<String>,
    ) -> InternalPromptTurnOutcome {
        InternalPromptTurnOutcome::Aborted {
            operation_id: self.operation_id().to_owned(),
            turn_id: Some(self.turn_id().to_owned()),
            reason: reason.into(),
            session_id,
        }
    }

    pub(crate) fn finish_failure(&self, error: CodingSessionError) -> InternalPromptTurnOutcome {
        let mut diagnostics = self.diagnostics.clone();
        if matches!(
            error,
            CodingSessionError::SessionWriteFailure {
                reason: crate::kernel::error::SessionWriteFailureReason::QueueSaturated,
                ..
            }
        ) {
            diagnostics.push(CodingDiagnostic::warning(
                "Session persistence is lagging: the bounded writer queue stayed saturated; retry after pending writes drain.",
            ));
        }
        InternalPromptTurnOutcome::Failed {
            operation_id: self.operation_id().to_owned(),
            turn_id: Some(self.turn_id().to_owned()),
            error,
            diagnostics,
        }
    }
}
