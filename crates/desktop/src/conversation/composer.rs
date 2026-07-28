//! Composer admission and optimistic submitted-prompt state.
//!
//! This reducer owns exact draft preservation across runtime admission. It is
//! independent of GPUI and only reads the durable conversation projection when
//! reconciling an accepted prompt.

use super::{ConversationBlockKind, ConversationProjection, MAX_BLOCK_TEXT_BYTES, truncate_bytes};

pub const MAX_COMPOSER_BYTES: usize = 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ComposerAdmission {
    Idle,
    Pending {
        command_id: u64,
        kind: ComposerSubmissionKind,
        payload: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComposerSubmissionKind {
    Prompt,
    Steer,
    FollowUp,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubmittedPromptPreview {
    pub command_id: u64,
    pub payload: String,
}

impl SubmittedPromptPreview {
    pub fn block_id(&self) -> String {
        format!("submitted-user:{}", self.command_id)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ComposerSubmitError {
    #[error("composer draft is empty")]
    Empty,
    #[error("composer draft exceeds {MAX_COMPOSER_BYTES} bytes")]
    TooLarge,
    #[error("composer submission is already awaiting admission")]
    AdmissionPending,
    #[error("composer completion does not match pending command")]
    StaleCompletion,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComposerState {
    draft: String,
    admission: ComposerAdmission,
    submitted: Option<SubmittedPromptPreview>,
    rejection: Option<String>,
}

impl Default for ComposerState {
    fn default() -> Self {
        Self {
            draft: String::new(),
            admission: ComposerAdmission::Idle,
            submitted: None,
            rejection: None,
        }
    }
}

impl ComposerState {
    pub fn draft(&self) -> &str {
        &self.draft
    }

    pub fn admission(&self) -> &ComposerAdmission {
        &self.admission
    }

    pub fn rejection(&self) -> Option<&str> {
        self.rejection.as_deref()
    }

    pub fn submitted(&self) -> Option<&SubmittedPromptPreview> {
        self.submitted.as_ref()
    }

    pub fn edit(&mut self, draft: impl Into<String>) {
        self.draft = draft.into();
        self.rejection = None;
    }

    pub fn begin_submit(
        &mut self,
        command_id: u64,
        kind: ComposerSubmissionKind,
    ) -> Result<&str, ComposerSubmitError> {
        if matches!(self.admission, ComposerAdmission::Pending { .. }) {
            return Err(ComposerSubmitError::AdmissionPending);
        }
        if self.draft.trim().is_empty() {
            return Err(ComposerSubmitError::Empty);
        }
        if self.draft.len() > MAX_COMPOSER_BYTES {
            return Err(ComposerSubmitError::TooLarge);
        }
        self.admission = ComposerAdmission::Pending {
            command_id,
            kind,
            payload: self.draft.clone(),
        };
        let ComposerAdmission::Pending { payload, .. } = &self.admission else {
            unreachable!("composer admission was just installed");
        };
        Ok(payload)
    }

    pub fn accepted(&mut self, command_id: u64) -> Result<(), ComposerSubmitError> {
        let ComposerAdmission::Pending {
            command_id: pending,
            kind,
            payload,
        } = &self.admission
        else {
            return Err(ComposerSubmitError::StaleCompletion);
        };
        if *pending != command_id {
            return Err(ComposerSubmitError::StaleCompletion);
        }
        if *kind == ComposerSubmissionKind::Prompt {
            self.submitted = Some(SubmittedPromptPreview {
                command_id,
                payload: payload.clone(),
            });
        }
        if self.draft == *payload {
            self.draft.clear();
        }
        self.admission = ComposerAdmission::Idle;
        self.rejection = None;
        Ok(())
    }

    /// Reconcile an accepted client-local prompt with completed durable truth.
    ///
    /// Returns the live and durable block identities when the prompt was
    /// retained. If completed hydration does not contain it, the exact payload
    /// is restored to the draft instead of being silently lost.
    pub fn reconcile_completed_submission(
        &mut self,
        projection: &ConversationProjection,
    ) -> Option<(String, String)> {
        let submitted = self.submitted.take()?;
        if let Some(block) = projection.blocks().iter().rev().find(|block| {
            block.kind == ConversationBlockKind::User && block.text == submitted.payload
        }) {
            self.rejection = None;
            return Some((submitted.block_id(), block.id.clone()));
        }
        if self.draft.is_empty() {
            self.draft = submitted.payload;
        }
        self.rejection =
            Some("Accepted prompt was not retained; the exact draft was restored.".into());
        None
    }

    pub fn rejected(
        &mut self,
        command_id: u64,
        message: impl Into<String>,
    ) -> Result<(), ComposerSubmitError> {
        let ComposerAdmission::Pending {
            command_id: pending,
            ..
        } = &self.admission
        else {
            return Err(ComposerSubmitError::StaleCompletion);
        };
        if *pending != command_id {
            return Err(ComposerSubmitError::StaleCompletion);
        }
        self.admission = ComposerAdmission::Idle;
        self.rejection = Some(truncate_bytes(message.into(), MAX_BLOCK_TEXT_BYTES).0);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conversation::ConversationViewport;
    use coding_agent::api::view::{
        CodingAgentSessionTranscriptItem, CodingAgentTranscriptSnapshot,
    };

    fn transcript(items: Vec<CodingAgentSessionTranscriptItem>) -> CodingAgentTranscriptSnapshot {
        CodingAgentTranscriptSnapshot {
            session_id: "session-1".into(),
            active_leaf_id: Some("leaf-1".into()),
            items,
        }
    }

    #[test]
    fn composer_submits_exactly_once_and_retains_rejected_draft() {
        let mut composer = ComposerState::default();
        composer.edit("  exact payload\n");
        assert_eq!(
            composer
                .begin_submit(7, ComposerSubmissionKind::Prompt)
                .unwrap(),
            "  exact payload\n"
        );
        assert_eq!(
            composer.begin_submit(8, ComposerSubmissionKind::Prompt),
            Err(ComposerSubmitError::AdmissionPending)
        );
        composer.rejected(7, "queue full").unwrap();
        assert_eq!(composer.draft(), "  exact payload\n");
        assert_eq!(composer.rejection(), Some("queue full"));

        assert_eq!(
            composer
                .begin_submit(9, ComposerSubmissionKind::Prompt)
                .unwrap(),
            "  exact payload\n"
        );
        composer.accepted(9).unwrap();
        assert_eq!(composer.draft(), "");
        assert_eq!(composer.admission(), &ComposerAdmission::Idle);
        assert_eq!(
            composer.submitted(),
            Some(&SubmittedPromptPreview {
                command_id: 9,
                payload: "  exact payload\n".into(),
            })
        );
    }

    #[test]
    fn submitted_prompt_reconciles_selection_to_durable_user_block() {
        let projection = ConversationProjection::hydrate(transcript(vec![
            CodingAgentSessionTranscriptItem::User {
                text: "exact payload".into(),
            },
        ]));
        let mut composer = ComposerState::default();
        composer.edit("exact payload");
        composer
            .begin_submit(7, ComposerSubmissionKind::Prompt)
            .unwrap();
        composer.accepted(7).unwrap();
        let live_id = composer.submitted().unwrap().block_id();
        let mut viewport = ConversationViewport::new(5);
        viewport.select_live(live_id.clone());

        let (reconciled_live, durable_id) = composer
            .reconcile_completed_submission(&projection)
            .unwrap();
        assert_eq!(reconciled_live, live_id);
        viewport.reconcile_live_selection(&reconciled_live, &durable_id);
        viewport.reconcile_hydration(&projection, projection.blocks().len(), 1);
        assert_eq!(viewport.selected_block_id(), Some(durable_id.as_str()));
        assert!(composer.submitted().is_none());
        assert!(composer.rejection().is_none());
    }

    #[test]
    fn completed_hydration_without_submitted_prompt_restores_exact_draft() {
        let projection = ConversationProjection::hydrate(transcript(Vec::new()));
        let mut composer = ComposerState::default();
        composer.edit("  exact payload\n");
        composer
            .begin_submit(7, ComposerSubmissionKind::Prompt)
            .unwrap();
        composer.accepted(7).unwrap();

        assert!(
            composer
                .reconcile_completed_submission(&projection)
                .is_none()
        );
        assert_eq!(composer.draft(), "  exact payload\n");
        assert!(composer.rejection().unwrap().contains("not retained"));
    }

    #[test]
    fn accepted_steer_clears_exact_draft_without_creating_user_overlay() {
        let mut composer = ComposerState::default();
        composer.edit("steer exactly");
        assert_eq!(
            composer
                .begin_submit(8, ComposerSubmissionKind::Steer)
                .unwrap(),
            "steer exactly"
        );
        composer.accepted(8).unwrap();
        assert!(composer.draft().is_empty());
        assert!(composer.submitted().is_none());
        assert_eq!(composer.admission(), &ComposerAdmission::Idle);
    }

    #[test]
    fn rejected_follow_up_retains_exact_draft() {
        let mut composer = ComposerState::default();
        composer.edit("  follow up exactly\n");
        composer
            .begin_submit(9, ComposerSubmissionKind::FollowUp)
            .unwrap();
        composer.rejected(9, "operation completed").unwrap();

        assert_eq!(composer.draft(), "  follow up exactly\n");
        assert_eq!(composer.rejection(), Some("operation completed"));
        assert!(composer.submitted().is_none());
        assert_eq!(composer.admission(), &ComposerAdmission::Idle);
    }

    #[test]
    fn composer_rejects_empty_oversized_and_stale_completion() {
        let mut composer = ComposerState::default();
        assert_eq!(
            composer.begin_submit(1, ComposerSubmissionKind::Prompt),
            Err(ComposerSubmitError::Empty)
        );
        composer.edit("x".repeat(MAX_COMPOSER_BYTES + 1));
        assert_eq!(
            composer.begin_submit(2, ComposerSubmissionKind::Prompt),
            Err(ComposerSubmitError::TooLarge)
        );
        composer.edit("valid");
        composer
            .begin_submit(3, ComposerSubmissionKind::Prompt)
            .unwrap();
        assert_eq!(
            composer.accepted(4),
            Err(ComposerSubmitError::StaleCompletion)
        );
        assert_eq!(composer.draft(), "valid");
    }
}
