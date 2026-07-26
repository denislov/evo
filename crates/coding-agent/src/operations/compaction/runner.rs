use agent_core::api::agent::AgentMessage;
use agent_core::api::compaction::{estimate_tokens, summarize_with_provider_streamer};
use ai::api::conversation::{AssistantMessage, ContentBlock};
use ai::api::stream::StreamOptions;
use tokio_util::sync::CancellationToken;

use crate::app::bootstrap::PromptInvocation;
use crate::operations::prompt::context::{
    InternalPromptTurnOutcome, PromptTurnOptions, PromptTurnTransaction, RuntimeSnapshot,
};
use crate::runtime::capability::ModelCapability;
use crate::runtime::capability::OperationCapabilitySnapshot;
use crate::runtime::facade::CodingSessionError;
use crate::services::runtime::{RuntimeService, scoped_provider_streamer_for_runtime};
#[cfg(test)]
use crate::session::event::SessionEventEnvelope;
use crate::session::replay::{SessionReplay, transcript_item_id};

#[derive(Debug, Clone)]
pub(crate) struct ManualCompactionOptions {
    runtime: RuntimeSnapshot,
    custom_instructions: Option<String>,
    cancellation: Option<CancellationToken>,
}

impl ManualCompactionOptions {
    #[cfg(test)]
    pub(crate) fn new(runtime: RuntimeSnapshot) -> Self {
        Self {
            runtime,
            custom_instructions: None,
            cancellation: None,
        }
    }

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

    #[cfg(test)]
    pub(crate) fn with_custom_instructions(mut self, instructions: impl Into<String>) -> Self {
        self.custom_instructions = Some(instructions.into());
        self
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

    #[cfg(test)]
    pub(crate) fn summary(&self) -> Option<&str> {
        self.summary.as_deref()
    }

    #[cfg(test)]
    pub(crate) fn first_kept_message_id(&self) -> Option<&str> {
        self.first_kept_message_id.as_deref()
    }

    #[cfg(test)]
    pub(crate) fn tokens_before(&self) -> Option<u32> {
        self.tokens_before
    }

    #[cfg(test)]
    pub(crate) fn pending_session_events(&self) -> &[SessionEventEnvelope] {
        self.transaction
            .as_ref()
            .map(PromptTurnTransaction::pending_events)
            .unwrap_or_default()
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

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use agent_core::api::agent::AgentResources;
    use ai::api::model::{Model, ModelCost, ModelInput};
    use ai::api::testing::FauxProvider;

    use super::*;
    use crate::session::event::{PersistedContentBlock, SessionEventData, SessionEventEnvelope};
    use crate::session::service::SessionService;

    #[tokio::test]
    async fn compact_cancellation_maps_flow_cancelled_to_typed_error() {
        let api = "manual-compaction-flow-cancelled";
        let _provider_guard = crate::test_support::ProviderGuard::register(
            api,
            Arc::new(FauxProvider::simple_text("unused summary")),
        );
        let temp = tempfile::tempdir().unwrap();
        let mut service = SessionService::create(
            &CodingAgentSessionOptions::new()
                .with_session_id("sess_manual_compaction_cancelled")
                .with_session_log_root(temp.path()),
        )
        .unwrap();
        record_prompt(&mut service);
        let replay = service.replay().unwrap();
        let snapshot = OperationCapabilitySnapshot::permissive("op_cancelled");
        let transaction = service.begin_manual_compaction_transaction(&snapshot);
        let cancellation = CancellationToken::new();
        cancellation.cancel();
        let mut context = ManualCompactionContext::new(
            ManualCompactionOptions::new(compact_runtime(api, _provider_guard.ai_client()))
                .with_cancellation(cancellation),
            replay,
            transaction,
            snapshot,
        );

        let error = ManualCompactionRunner::new()
            .unwrap()
            .run_typed(&mut context)
            .await
            .unwrap_err();
        assert_eq!(error, CodingSessionError::Cancelled);
    }
    use crate::app::bootstrap::{PromptInvocation, SessionRunOptions};
    use crate::app::prompt_runtime::PromptRuntimeOptions;
    use crate::runtime::facade::{CodingAgentSessionOptions, PromptTurnOptions};

    fn model(api: &str) -> Model {
        Model {
            id: "test-model".into(),
            name: "Test Model".into(),
            api: api.into(),
            provider: "test".into(),
            base_url: String::new(),
            reasoning: false,
            thinking_level_map: None,
            input: vec![ModelInput::Text],
            cost: ModelCost::default(),
            context_window: 0,
            max_tokens: 0,
            headers: None,
            compat: None,
        }
    }

    fn compact_runtime(
        api: &str,
        ai_client: ai::api::client::AiClient,
    ) -> crate::operations::prompt::context::RuntimeSnapshot {
        PromptTurnOptions::from_prompt_runtime_options(PromptRuntimeOptions {
            model: model(api),
            api_key: None,
            auth_diagnostics: Vec::new(),
            system_prompt: Some("system".into()),
            max_turns: Some(2),
            tools: Vec::new(),
            register_builtins: false,
            ai_client: Some(ai_client),
            session: Some(SessionRunOptions::disabled(".".into())),
            session_target: None,
            session_name: None,
            thinking_level: None,
            tool_execution: None,
            resources: AgentResources::default(),
            settings: None,
            invocation: PromptInvocation::Compact {
                custom_instructions: Some("keep decisions".into()),
            },
        })
        .runtime()
        .cloned()
        .unwrap()
    }

    fn record_prompt(service: &mut SessionService) -> String {
        let mut transaction = service.begin_prompt_transaction();
        let operation_id = transaction.operation_id().to_owned();
        transaction
            .record_user_input(vec![PersistedContentBlock::Text {
                text: "first question".into(),
            }])
            .unwrap();
        let message_id = transaction.start_assistant_message().unwrap();
        transaction
            .complete_assistant_message(
                message_id,
                vec![PersistedContentBlock::Text {
                    text: "first answer".into(),
                }],
                Some("stop".into()),
                Default::default(),
            )
            .unwrap();
        service
            .commit_prompt_transaction(Some(transaction), operation_id)
            .unwrap()
            .leaf_id
            .unwrap()
    }

    fn event_kinds(events: &[SessionEventEnvelope]) -> Vec<&'static str> {
        events
            .iter()
            .map(|event| match event.data {
                SessionEventData::OperationStarted { .. } => "operation.started",
                SessionEventData::TurnStarted {} => "turn.started",
                SessionEventData::SessionCompactionStarted { .. } => "session.compaction.started",
                SessionEventData::SessionCompactionCompleted { .. } => {
                    "session.compaction.completed"
                }
                SessionEventData::OperationCommitted { .. } => "operation.committed",
                SessionEventData::OperationFailed { .. } => "operation.failed",
                SessionEventData::OperationAborted { .. } => "operation.aborted",
                SessionEventData::DiagnosticEmitted { .. } => "diagnostic.emitted",
                _ => "other",
            })
            .collect()
    }

    #[tokio::test]
    async fn manual_compaction_flow_records_summary_events_without_flushing() {
        let api = "manual-compaction-flow-records-summary";
        let _provider_guard = crate::test_support::ProviderGuard::register(
            api,
            Arc::new(FauxProvider::simple_text("summary from flow")),
        );
        let temp = tempfile::tempdir().unwrap();
        let mut service = SessionService::create(
            &CodingAgentSessionOptions::new()
                .with_session_id("sess_manual_compaction_flow")
                .with_session_log_root(temp.path()),
        )
        .unwrap();
        let active_leaf = record_prompt(&mut service);
        let replay = service.replay().unwrap();
        let snapshot = OperationCapabilitySnapshot::permissive("op_test");
        let transaction = service.begin_manual_compaction_transaction(&snapshot);
        let mut context = ManualCompactionContext::new(
            ManualCompactionOptions::new(compact_runtime(api, _provider_guard.ai_client()))
                .with_custom_instructions("keep decisions"),
            replay,
            transaction,
            snapshot,
        );
        let flow = ManualCompactionRunner::new().unwrap();

        flow.run_typed(&mut context).await.unwrap();
        assert_eq!(context.summary(), Some("summary from flow"));
        assert!(
            context
                .first_kept_message_id()
                .is_some_and(|id| id.starts_with("msg_"))
        );
        assert!(context.tokens_before().is_some_and(|tokens| tokens > 0));
        assert_eq!(
            event_kinds(context.pending_session_events()),
            vec![
                "operation.started",
                "turn.started",
                "session.compaction.started",
                "session.compaction.completed",
            ]
        );
        assert_eq!(
            service.replay().unwrap().active_leaf_id.as_deref(),
            Some(active_leaf.as_str())
        );
        assert!(matches!(
            service.replay().unwrap().transcript.as_slice(),
            [
                crate::session::replay::TranscriptItem::UserInput { .. },
                crate::session::replay::TranscriptItem::AssistantMessage { .. }
            ]
        ));
    }
}
