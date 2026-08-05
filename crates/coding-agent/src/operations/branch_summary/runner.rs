use std::collections::{HashMap, HashSet};

use agent_core::api::agent::AgentMessage;
use agent_core::api::compaction::summarize_with_provider_streamer;
use ai_protocol::api::conversation::{AssistantMessage, ContentBlock};
use ai_protocol::api::stream::StreamOptions;
use tokio_util::sync::CancellationToken;

use crate::application::capability::OperationCapabilitySnapshot;
use crate::kernel::capability::ModelCapability;
use crate::kernel::error::CodingSessionError;
use crate::operations::prompt::context::{
    InternalPromptTurnOutcome, PromptTurnTransaction, RuntimeSnapshot,
};
use crate::services::runtime::{RuntimeService, scoped_provider_streamer_for_runtime};

use crate::session::event::{DiagnosticLevel, PersistedContentBlock};
use crate::session::replay::{ReplayLeaf, SessionReplay, ToolCallStatus, TranscriptItem};

const NO_ABANDONED_BRANCH_REASON: &str =
    "No abandoned branch is available in the current Rust-native session view";
const BRANCH_SUMMARY_PREAMBLE: &str = "The user explored a different conversation branch before returning here.\nSummary of that exploration:\n";
const BRANCH_SUMMARY_INSTRUCTIONS: &str = r#"Create a structured summary of this conversation branch for context when returning later.

Use this EXACT format:

## Goal
[What was the user trying to accomplish in this branch?]

## Constraints & Preferences
- [Any constraints, preferences, or requirements mentioned]
- [Or "(none)" if none were mentioned]

## Progress
### Done
- [x] [Completed tasks/changes]

### In Progress
- [ ] [Work that was started but not finished]

### Blocked
- [Issues preventing progress, if any]

## Key Decisions
- **[Decision]**: [Brief rationale]

## Next Steps
1. [What should happen next to continue this work]

Keep each section concise. Preserve exact file paths, function names, and error messages."#;

#[derive(Debug, Clone, Default)]
pub(crate) struct BranchSummaryOptions {
    source_leaf_id: Option<String>,
    target_leaf_id: Option<String>,
    custom_instructions: Option<String>,
    runtime: Option<RuntimeSnapshot>,
}

impl BranchSummaryOptions {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn with_source_leaf_id(mut self, source_leaf_id: impl Into<String>) -> Self {
        self.source_leaf_id = Some(source_leaf_id.into());
        self
    }

    pub(crate) fn with_target_leaf_id(mut self, target_leaf_id: impl Into<String>) -> Self {
        self.target_leaf_id = Some(target_leaf_id.into());
        self
    }

    pub(crate) fn with_custom_instructions(mut self, instructions: impl Into<String>) -> Self {
        self.custom_instructions = Some(instructions.into());
        self
    }

    pub(crate) fn with_runtime(mut self, runtime: RuntimeSnapshot) -> Self {
        self.runtime = Some(runtime);
        self
    }

    fn custom_instructions(&self) -> Option<&str> {
        self.custom_instructions.as_deref()
    }

    fn runtime(&self) -> Option<&RuntimeSnapshot> {
        self.runtime.as_ref()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum BranchSummaryOutcome {
    Created {
        summary: String,
        source_leaf_id: String,
        target_leaf_id: String,
    },
    NoOp {
        reason: String,
    },
}

pub(crate) fn branch_summary_outcome_text(outcome: &BranchSummaryOutcome) -> String {
    match outcome {
        BranchSummaryOutcome::Created { summary, .. } => summary.clone(),
        BranchSummaryOutcome::NoOp { reason } => reason.clone(),
    }
}

pub(crate) fn branch_summary_success_outcome(
    operation_id: impl Into<String>,
    turn_id: impl Into<String>,
    session_id: impl Into<String>,
    leaf_id: Option<String>,
    runtime: &RuntimeSnapshot,
    final_text: impl Into<String>,
) -> InternalPromptTurnOutcome {
    let final_text = final_text.into();
    InternalPromptTurnOutcome::Success {
        operation_id: operation_id.into(),
        turn_id: turn_id.into(),
        session_id: Some(session_id.into()),
        leaf_id,
        final_message: branch_summary_final_message(runtime, &final_text),
        final_text,
        diagnostics: Vec::new(),
    }
}

pub(crate) fn branch_summary_failed_outcome(
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

fn branch_summary_final_message(runtime: &RuntimeSnapshot, summary: &str) -> AssistantMessage {
    let mut message = AssistantMessage::empty(&runtime.model().api, &runtime.model().id);
    message.provider = Some(runtime.model().provider.clone());
    message.content.push(ContentBlock::Text {
        text: summary.to_owned(),
        text_signature: None,
    });
    message
}

pub(crate) struct BranchSummaryContext {
    options: BranchSummaryOptions,
    operation_id: String,
    turn_id: String,
    replay: SessionReplay,
    transaction: Option<PromptTurnTransaction>,
    capability_snapshot: OperationCapabilitySnapshot,
    selected_source_leaf_id: Option<String>,
    selected_target_leaf_id: Option<String>,
    selected_transcript: Vec<TranscriptItem>,
    summary_messages: Vec<AgentMessage>,
    stream_options: Option<StreamOptions>,
    outcome: Option<BranchSummaryOutcome>,
    cancellation: Option<CancellationToken>,
}

impl BranchSummaryContext {
    pub(crate) fn new(
        options: BranchSummaryOptions,
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
            selected_source_leaf_id: None,
            selected_target_leaf_id: None,
            selected_transcript: Vec::new(),
            summary_messages: Vec::new(),
            stream_options: None,
            outcome: None,
            cancellation: None,
        }
    }

    pub(crate) fn operation_id(&self) -> &str {
        &self.operation_id
    }

    pub(crate) fn turn_id(&self) -> &str {
        &self.turn_id
    }

    pub(crate) fn take_transaction(&mut self) -> Option<PromptTurnTransaction> {
        self.transaction.take()
    }

    pub(crate) fn set_cancellation(&mut self, cancellation: CancellationToken) {
        self.cancellation = Some(cancellation);
    }

    pub(crate) fn finish_success(&self) -> Result<BranchSummaryOutcome, CodingSessionError> {
        self.outcome
            .clone()
            .ok_or_else(|| CodingSessionError::Session {
                message: "branch summary cannot finish without an outcome".into(),
            })
    }

    fn start_branch_summary(&mut self) -> Result<(), CodingSessionError> {
        if self.transaction.is_none() {
            return Err(CodingSessionError::Session {
                message: "branch summary cannot start without a transaction".into(),
            });
        }
        Ok(())
    }

    fn load_branch_events(&mut self) -> Result<(), CodingSessionError> {
        if self.replay.session_id.is_empty() {
            return Err(CodingSessionError::Session {
                message: "branch summary cannot load an unnamed session replay".into(),
            });
        }
        Ok(())
    }

    fn select_abandoned_range(&mut self) -> Result<(), CodingSessionError> {
        if self.outcome.is_some() || !self.selected_transcript.is_empty() {
            return Ok(());
        }
        let Some(source_leaf_id) = self
            .options
            .source_leaf_id
            .as_deref()
            .or(self.replay.active_leaf_id.as_deref())
        else {
            self.outcome = Some(BranchSummaryOutcome::NoOp {
                reason: NO_ABANDONED_BRANCH_REASON.into(),
            });
            return Ok(());
        };
        let Some(target_leaf_id) = self.options.target_leaf_id.as_deref() else {
            self.outcome = Some(BranchSummaryOutcome::NoOp {
                reason: NO_ABANDONED_BRANCH_REASON.into(),
            });
            return Ok(());
        };
        let abandoned = abandoned_leaf_path(&self.replay.leaves, source_leaf_id, target_leaf_id)?;
        if abandoned.is_empty() {
            self.outcome = Some(BranchSummaryOutcome::NoOp {
                reason: NO_ABANDONED_BRANCH_REASON.into(),
            });
            return Ok(());
        }
        self.selected_transcript = transcript_for_leaves(&self.replay.transcript, &abandoned);
        if self.selected_transcript.is_empty() {
            self.outcome = Some(BranchSummaryOutcome::NoOp {
                reason: NO_ABANDONED_BRANCH_REASON.into(),
            });
            return Ok(());
        }
        self.selected_source_leaf_id = Some(source_leaf_id.to_owned());
        self.selected_target_leaf_id = Some(target_leaf_id.to_owned());
        Ok(())
    }

    fn prepare_summary_prompt(&mut self) -> Result<(), CodingSessionError> {
        if self.outcome.is_some() || !self.summary_messages.is_empty() {
            return Ok(());
        }
        let Some(runtime) = self.options.runtime() else {
            return Ok(());
        };
        let selected_replay = SessionReplay {
            session_id: self.replay.session_id.clone(),
            committed_through_session_sequence: self.replay.committed_through_session_sequence,
            cwd: self.replay.cwd.clone(),
            active_leaf_id: self.selected_source_leaf_id.clone(),
            leaves: Vec::new(),
            tree_labels: Default::default(),
            transcript: self.selected_transcript.clone(),
            diagnostics: Vec::new(),
            pending_delegation_confirmations: Vec::new(),
            pending_tool_authorizations: Vec::new(),
            usage: Default::default(),
            operation_statuses: Default::default(),
        };
        let service = RuntimeService::new();
        let build =
            service.build_agent_runtime_with_capabilities(runtime, &self.capability_snapshot)?;
        let agent = build.agent;
        service.hydrate_agent_runtime(&agent, runtime, &selected_replay);
        let messages = agent.messages();
        if messages.is_empty() {
            self.outcome = Some(BranchSummaryOutcome::NoOp {
                reason: NO_ABANDONED_BRANCH_REASON.into(),
            });
            return Ok(());
        }
        self.stream_options = agent.provider_request_snapshot().1;
        self.summary_messages = messages;
        Ok(())
    }

    async fn run_summary_model(&mut self) -> Result<(), CodingSessionError> {
        if self.outcome.is_some() {
            return Ok(());
        }
        let source_leaf_id =
            self.selected_source_leaf_id
                .clone()
                .ok_or_else(|| CodingSessionError::Session {
                    message: "branch summary range was not selected".into(),
                })?;
        let target_leaf_id =
            self.selected_target_leaf_id
                .clone()
                .ok_or_else(|| CodingSessionError::Session {
                    message: "branch summary target was not selected".into(),
                })?;
        let summary = if let Some(runtime) = self.options.runtime() {
            let model_capability = ModelCapability::require(
                self.capability_snapshot.model.as_ref(),
                runtime.profile_id(),
            )?;
            let instructions = branch_summary_instructions(self.options.custom_instructions());
            let cancellation = self.cancellation.clone();
            let summary = summarize_with_provider_streamer(
                runtime.model(),
                &self.summary_messages,
                Some(instructions.as_str()),
                self.stream_options.clone(),
                cancellation.clone(),
                Some(scoped_provider_streamer_for_runtime(
                    runtime,
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
            format!("{BRANCH_SUMMARY_PREAMBLE}\n{}", summary.trim())
        } else {
            render_transcript_summary(&self.selected_transcript)
        };
        self.outcome = Some(BranchSummaryOutcome::Created {
            summary,
            source_leaf_id,
            target_leaf_id,
        });
        Ok(())
    }

    fn record_branch_summary(&mut self) -> Result<(), CodingSessionError> {
        let outcome = self
            .outcome
            .clone()
            .ok_or_else(|| CodingSessionError::Session {
                message: "branch summary cannot record events without an outcome".into(),
            })?;
        match outcome {
            BranchSummaryOutcome::Created {
                summary,
                source_leaf_id,
                target_leaf_id,
            } => self
                .transaction_mut_required()?
                .record_branch_summary_created(summary, source_leaf_id, target_leaf_id),
            BranchSummaryOutcome::NoOp { reason } => self
                .transaction_mut_required()?
                .emit_diagnostic(DiagnosticLevel::Info, reason),
        }
    }

    fn transaction_mut_required(
        &mut self,
    ) -> Result<&mut PromptTurnTransaction, CodingSessionError> {
        self.transaction
            .as_mut()
            .ok_or_else(|| CodingSessionError::Session {
                message: "branch summary has no active transaction".into(),
            })
    }

    fn finalize_branch_summary(&mut self) -> Result<(), CodingSessionError> {
        self.finish_success().map(|_| ())
    }
}

pub(crate) struct BranchSummaryRunner;

impl BranchSummaryRunner {
    pub(crate) fn new() -> Result<Self, CodingSessionError> {
        Ok(Self)
    }

    pub(crate) async fn run_typed(
        &self,
        ctx: &mut BranchSummaryContext,
        cancellation: Option<CancellationToken>,
    ) -> Result<(), CodingSessionError> {
        if let Some(token) = cancellation.clone() {
            ctx.set_cancellation(token);
        }
        Self::check_cancellation(&cancellation)?;
        ctx.start_branch_summary()?;
        Self::check_cancellation(&cancellation)?;
        ctx.load_branch_events()?;
        Self::check_cancellation(&cancellation)?;
        ctx.select_abandoned_range()?;
        Self::check_cancellation(&cancellation)?;
        ctx.prepare_summary_prompt()?;
        Self::check_cancellation(&cancellation)?;
        ctx.run_summary_model().await?;
        Self::check_cancellation(&cancellation)?;
        ctx.record_branch_summary()?;
        Self::check_cancellation(&cancellation)?;
        ctx.finalize_branch_summary()?;
        Ok(())
    }

    fn check_cancellation(
        cancellation: &Option<CancellationToken>,
    ) -> Result<(), CodingSessionError> {
        if cancellation
            .as_ref()
            .is_some_and(|token| token.is_cancelled())
        {
            return Err(CodingSessionError::Cancelled);
        }
        Ok(())
    }
}

fn abandoned_leaf_path(
    leaves: &[ReplayLeaf],
    source_leaf_id: &str,
    target_leaf_id: &str,
) -> Result<Vec<ReplayLeaf>, CodingSessionError> {
    let by_id = leaves
        .iter()
        .map(|leaf| (leaf.leaf_id.as_str(), leaf))
        .collect::<HashMap<_, _>>();
    let source_path = leaf_path_to_root(source_leaf_id, &by_id)?;
    let target_path = leaf_path_to_root(target_leaf_id, &by_id)?;
    let target_ids = target_path
        .iter()
        .map(|leaf| leaf.leaf_id.as_str())
        .collect::<HashSet<_>>();
    let mut abandoned = Vec::new();
    for leaf in source_path {
        if target_ids.contains(leaf.leaf_id.as_str()) {
            break;
        }
        abandoned.push(leaf.clone());
    }
    abandoned.reverse();
    Ok(abandoned)
}

fn leaf_path_to_root<'a>(
    leaf_id: &str,
    by_id: &HashMap<&str, &'a ReplayLeaf>,
) -> Result<Vec<&'a ReplayLeaf>, CodingSessionError> {
    let mut current = Some(leaf_id);
    let mut path = Vec::new();
    let mut visited = HashSet::new();
    while let Some(leaf_id) = current {
        if !visited.insert(leaf_id.to_owned()) {
            return Err(CodingSessionError::Session {
                message: format!("cycle detected in replay leaf ancestry at id: {leaf_id}"),
            });
        }
        let leaf = by_id
            .get(leaf_id)
            .copied()
            .ok_or_else(|| CodingSessionError::Session {
                message: format!("leaf id not found in replay: {leaf_id}"),
            })?;
        path.push(leaf);
        current = leaf.parent_leaf_id.as_deref();
    }
    Ok(path)
}

fn transcript_for_leaves(
    transcript: &[TranscriptItem],
    leaves: &[ReplayLeaf],
) -> Vec<TranscriptItem> {
    let mut selected = Vec::new();
    for leaf in leaves {
        let start = leaf.transcript_start.min(transcript.len());
        let end = leaf.transcript_end.min(transcript.len());
        if start >= end {
            continue;
        }
        selected.extend(transcript[start..end].iter().cloned());
    }
    selected
}

fn branch_summary_instructions(custom_instructions: Option<&str>) -> String {
    match custom_instructions {
        Some(custom) if !custom.trim().is_empty() => {
            format!(
                "{BRANCH_SUMMARY_INSTRUCTIONS}\n\nAdditional focus: {}",
                custom.trim()
            )
        }
        _ => BRANCH_SUMMARY_INSTRUCTIONS.to_owned(),
    }
}

fn render_transcript_summary(items: &[TranscriptItem]) -> String {
    let mut summary = String::from(BRANCH_SUMMARY_PREAMBLE);
    for item in items {
        match item {
            TranscriptItem::UserInput { text, .. } if !text.trim().is_empty() => {
                summary.push_str("\nUser:\n");
                summary.push_str(text.trim());
                summary.push('\n');
            }
            TranscriptItem::AssistantMessage { content, .. } => {
                let text = persisted_content_blocks_text(content);
                if !text.trim().is_empty() {
                    summary.push_str("\nAssistant:\n");
                    summary.push_str(text.trim());
                    summary.push('\n');
                }
            }
            TranscriptItem::ToolCall {
                name,
                status,
                summary: tool_summary,
                ..
            } if !tool_summary.trim().is_empty() => {
                summary.push_str("\nTool ");
                summary.push_str(name);
                if matches!(status, ToolCallStatus::Failed) {
                    summary.push_str(" failed");
                }
                summary.push_str(":\n");
                summary.push_str(tool_summary.trim());
                summary.push('\n');
            }
            TranscriptItem::CompactionSummary { summary: text, .. }
            | TranscriptItem::BranchSummary { summary: text, .. }
                if !text.trim().is_empty() =>
            {
                summary.push_str("\nPrior summary:\n");
                summary.push_str(text.trim());
                summary.push('\n');
            }
            TranscriptItem::Diagnostic { message, .. } if !message.trim().is_empty() => {
                summary.push_str("\nDiagnostic:\n");
                summary.push_str(message.trim());
                summary.push('\n');
            }
            _ => {}
        }
    }
    summary
}

fn persisted_content_blocks_text(content: &[PersistedContentBlock]) -> String {
    content
        .iter()
        .map(|block| match block {
            PersistedContentBlock::Text { text } => text.clone(),
            PersistedContentBlock::Thinking { thinking, .. } => thinking.clone(),
            PersistedContentBlock::Image { mime_type, .. } => format!("[image:{mime_type}]"),
            PersistedContentBlock::ProviderItem { api, .. } => format!("[provider_item:{api}]"),
        })
        .collect::<Vec<_>>()
        .join("\n")
}
