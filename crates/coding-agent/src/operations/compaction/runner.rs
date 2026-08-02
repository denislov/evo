use agent_core::api::agent::AgentMessage;
use agent_core::api::compaction::{estimate_tokens, summarize_with_provider_streamer};
use ai::api::conversation::{AssistantMessage, ContentBlock};
use ai::api::stream::StreamOptions;
use tokio_util::sync::CancellationToken;

use crate::app::bootstrap::PromptInvocation;
use crate::application::capability::OperationCapabilitySnapshot;
use crate::kernel::capability::ModelCapability;
use crate::kernel::error::CodingSessionError;
use crate::operations::prompt::context::{
    InternalPromptTurnOutcome, PromptTurnOptions, PromptTurnTransaction, RuntimeSnapshot,
};
use crate::services::runtime::{RuntimeService, scoped_provider_streamer_for_runtime};

use crate::session::replay::{SessionReplay, transcript_item_id};

#[derive(Debug, Clone)]
pub(crate) struct ManualCompactionOptions {
    runtime: RuntimeSnapshot,
    custom_instructions: Option<String>,
    cancellation: Option<CancellationToken>,
}

impl ManualCompactionOptions {
    pub(crate) fn from_prompt_turn_options(
        options: &PromptTurnOptions,
    ) -> Result<Self, CodingSessionError> {
        let custom_instructions = match options.invocation() {
            PromptInvocation::Compact {
                custom_instructions,
            } => custom_instructions.clone(),
            _ => {
                return Err(CodingSessionError::Input {
                    message: "compact operation requires a compaction invocation".into(),
                });
            }
        };
        let runtime = options
            .runtime()
            .cloned()
            .ok_or_else(|| CodingSessionError::Config {
                message: "compact operation options do not include a runtime snapshot".into(),
            })?;
        Ok(Self {
            runtime,
            custom_instructions,
            cancellation: None,
        })
    }

    pub(crate) fn with_cancellation(mut self, cancellation: CancellationToken) -> Self {
        self.cancellation = Some(cancellation);
        self
    }

    fn runtime(&self) -> &RuntimeSnapshot {
        &self.runtime
    }

    fn custom_instructions(&self) -> Option<&str> {
        self.custom_instructions.as_deref()
    }

    pub(crate) fn cancellation(&self) -> Option<CancellationToken> {
        self.cancellation.clone()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ManualCompactionOutcome {
    pub(crate) summary: String,
    pub(crate) first_kept_message_id: String,
    pub(crate) tokens_before: u32,
    pub(crate) final_message: AssistantMessage,
}

pub(crate) fn manual_compaction_success_outcome(
    operation_id: impl Into<String>,
    turn_id: impl Into<String>,
    session_id: impl Into<String>,
    leaf_id: Option<String>,
    outcome: &ManualCompactionOutcome,
) -> InternalPromptTurnOutcome {
    InternalPromptTurnOutcome::Success {
        operation_id: operation_id.into(),
        turn_id: turn_id.into(),
        session_id: Some(session_id.into()),
        leaf_id,
        final_text: outcome.summary.clone(),
        final_message: outcome.final_message.clone(),
        diagnostics: Vec::new(),
    }
}

pub(crate) fn manual_compaction_failed_outcome(
    operation_id: impl Into<String>,
    turn_id: impl Into<String>,
    error: CodingSessionError,
) -> InternalPromptTurnOutcome {
    InternalPromptTurnOutcome::Failed {
        operation_id: operation_id.into(),
        turn_id: Some(turn_id.into()),
        error,
        diagnostics: Vec::new(),
    }
}

pub(crate) fn manual_compaction_operation_id(outcome: &InternalPromptTurnOutcome) -> &str {
    match outcome {
        InternalPromptTurnOutcome::Success { operation_id, .. }
        | InternalPromptTurnOutcome::Failed { operation_id, .. }
        | InternalPromptTurnOutcome::Aborted { operation_id, .. } => operation_id,
    }
}

pub(crate) struct ManualCompactionContext {
    options: ManualCompactionOptions,
    operation_id: String,
    turn_id: String,
    replay: SessionReplay,
    transaction: Option<PromptTurnTransaction>,
    capability_snapshot: OperationCapabilitySnapshot,
    first_kept_message_id: Option<String>,
    tokens_before: Option<u32>,
    summary_messages: Vec<AgentMessage>,
    stream_options: Option<StreamOptions>,
    summary: Option<String>,
    final_message: Option<AssistantMessage>,
}

impl ManualCompactionContext {
    pub(crate) fn new(
        options: ManualCompactionOptions,
        replay: SessionReplay,
        transaction: PromptTurnTransaction,
        capability_snapshot: OperationCapabilitySnapshot,
    ) -> Self {
        let operation_id = transaction.operation_id().to_owned();
        let turn_id = transaction.turn_id().to_owned();
        Self {
            options,
            operation_id,
            turn_id,
            replay,
            transaction: Some(transaction),
            capability_snapshot,
            first_kept_message_id: None,
            tokens_before: None,
            summary_messages: Vec::new(),
            stream_options: None,
            summary: None,
            final_message: None,
        }
    }

    pub(crate) fn operation_id(&self) -> &str {
        &self.operation_id
    }

    pub(crate) fn options(&self) -> &ManualCompactionOptions {
        &self.options
    }

    pub(crate) fn turn_id(&self) -> &str {
        &self.turn_id
    }

    pub(crate) fn take_transaction(&mut self) -> Option<PromptTurnTransaction> {
        self.transaction.take()
    }

    pub(crate) fn finish_success(&self) -> Result<ManualCompactionOutcome, CodingSessionError> {
        Ok(ManualCompactionOutcome {
            summary: self
                .summary
                .clone()
                .ok_or_else(|| CodingSessionError::Session {
                    message: "manual compaction cannot finish without a summary".into(),
                })?,
            first_kept_message_id: self.first_kept_message_id.clone().ok_or_else(|| {
                CodingSessionError::Session {
                    message: "manual compaction cannot finish without a kept message id".into(),
                }
            })?,
            tokens_before: self
                .tokens_before
                .ok_or_else(|| CodingSessionError::Session {
                    message: "manual compaction cannot finish without token accounting".into(),
                })?,
            final_message: self.final_message.clone().ok_or_else(|| {
                CodingSessionError::Session {
                    message: "manual compaction cannot finish without a final message".into(),
                }
            })?,
        })
    }

    fn transaction_mut_required(
        &mut self,
    ) -> Result<&mut PromptTurnTransaction, CodingSessionError> {
        self.transaction
            .as_mut()
            .ok_or_else(|| CodingSessionError::Session {
                message: "manual compaction has no active transaction".into(),
            })
    }

    fn start_compaction(&mut self) -> Result<(), CodingSessionError> {
        if self.transaction.is_none() {
            return Err(CodingSessionError::Session {
                message: "manual compaction cannot start without a transaction".into(),
            });
        }
        Ok(())
    }

    fn load_session_replay(&mut self) -> Result<(), CodingSessionError> {
        if self.replay.session_id.is_empty() {
            return Err(CodingSessionError::Session {
                message: "manual compaction cannot load an unnamed session replay".into(),
            });
        }
        Ok(())
    }

    fn select_compaction_range(&mut self) -> Result<(), CodingSessionError> {
        if self.first_kept_message_id.is_some() {
            return Ok(());
        }
        let first_kept_message_id = self
            .replay
            .transcript
            .iter()
            .rev()
            .find_map(transcript_item_id)
            .ok_or_else(|| CodingSessionError::Session {
                message: "Nothing to compact (no messages yet)".into(),
            })?;
        self.first_kept_message_id = Some(first_kept_message_id);
        Ok(())
    }

    fn prepare_summary_context(&mut self) -> Result<(), CodingSessionError> {
        if !self.summary_messages.is_empty() {
            return Ok(());
        }
        let service = RuntimeService::new();
        let build = service.build_agent_runtime_with_capabilities(
            self.options.runtime(),
            &self.capability_snapshot,
        )?;
        let agent = build.agent;
        service.hydrate_agent_runtime(&agent, self.options.runtime(), &self.replay);
        let messages = agent.messages();
        if messages.len() < 2 {
            return Err(CodingSessionError::Session {
                message: "Nothing to compact (no messages yet)".into(),
            });
        }
        let first_kept_index = messages.len() - 1;
        let to_summarize = messages[..first_kept_index].to_vec();
        if to_summarize.is_empty() {
            return Err(CodingSessionError::Session {
                message: "Nothing to compact (no compactable history)".into(),
            });
        }
        let tokens_before = estimate_tokens(&messages);
        let first_kept_message_id =
            self.first_kept_message_id
                .clone()
                .ok_or_else(|| CodingSessionError::Session {
                    message: "manual compaction range was not selected".into(),
                })?;
        let stream_options = agent.provider_request_snapshot().1;
        self.transaction_mut_required()?
            .record_session_compaction_started(first_kept_message_id, tokens_before)?;
        self.tokens_before = Some(tokens_before);
        self.summary_messages = to_summarize;
        self.stream_options = stream_options;
        Ok(())
    }

    async fn run_summary_model(&mut self) -> Result<(), CodingSessionError> {
        if self.summary.is_some() {
            return Ok(());
        }
        let model_capability = ModelCapability::require(
            self.capability_snapshot.model.as_ref(),
            self.options.runtime().profile_id(),
        )?;
        let cancellation = self.options.cancellation();
        let summary = summarize_with_provider_streamer(
            self.options.runtime().model(),
            &self.summary_messages,
            self.options.custom_instructions(),
            self.stream_options.clone(),
            cancellation.clone(),
            Some(scoped_provider_streamer_for_runtime(
                self.options.runtime(),
                model_capability,
            )?),
        )
        .await
        .map_err(|error| {
            if cancellation
                .as_ref()
                .is_some_and(CancellationToken::is_cancelled)
            {
                CodingSessionError::Cancelled
            } else {
                CodingSessionError::Provider {
                    message: error.to_string(),
                }
            }
        })?;
        self.summary = Some(summary.clone());
        self.final_message = Some(compaction_final_message(self.options.runtime(), &summary));
        Ok(())
    }

    fn record_compaction_events(&mut self) -> Result<(), CodingSessionError> {
        let summary = self
            .summary
            .clone()
            .ok_or_else(|| CodingSessionError::Session {
                message: "manual compaction cannot record events without a summary".into(),
            })?;
        let first_kept_message_id =
            self.first_kept_message_id
                .clone()
                .ok_or_else(|| CodingSessionError::Session {
                    message: "manual compaction cannot record events without a kept message id"
                        .into(),
                })?;
        let tokens_before = self
            .tokens_before
            .ok_or_else(|| CodingSessionError::Session {
                message: "manual compaction cannot record events without token accounting".into(),
            })?;
        self.transaction_mut_required()?
            .record_session_compaction_completed(summary, first_kept_message_id, tokens_before)
    }

    fn finalize_compaction(&mut self) -> Result<(), CodingSessionError> {
        self.finish_success().map(|_| ())
    }
}

pub(crate) struct ManualCompactionRunner;

impl ManualCompactionRunner {
    pub(crate) fn new() -> Result<Self, CodingSessionError> {
        Ok(Self)
    }

    pub(crate) async fn run_typed(
        &self,
        ctx: &mut ManualCompactionContext,
    ) -> Result<ManualCompactionOutcome, CodingSessionError> {
        ctx.start_compaction()?;
        Self::check_cancellation(ctx)?;
        ctx.load_session_replay()?;
        Self::check_cancellation(ctx)?;
        ctx.select_compaction_range()?;
        Self::check_cancellation(ctx)?;
        ctx.prepare_summary_context()?;
        Self::check_cancellation(ctx)?;
        ctx.run_summary_model().await?;
        Self::check_cancellation(ctx)?;
        ctx.record_compaction_events()?;
        Self::check_cancellation(ctx)?;
        ctx.finalize_compaction()?;
        ctx.finish_success()
    }

    fn check_cancellation(ctx: &ManualCompactionContext) -> Result<(), CodingSessionError> {
        if ctx
            .options()
            .cancellation()
            .is_some_and(|token| token.is_cancelled())
        {
            return Err(CodingSessionError::Cancelled);
        }
        Ok(())
    }
}

fn compaction_final_message(runtime: &RuntimeSnapshot, summary: &str) -> AssistantMessage {
    let mut message = AssistantMessage::empty(&runtime.model().api, &runtime.model().id);
    message.provider = Some(runtime.model().provider.clone());
    message.content.push(ContentBlock::Text {
        text: summary.to_owned(),
        text_signature: None,
    });
    message
}
