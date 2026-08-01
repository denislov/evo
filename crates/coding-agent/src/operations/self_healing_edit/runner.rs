use std::path::{Path, PathBuf};
use std::sync::Arc;

use ai::api::conversation::{AssistantMessage, ContentBlock, Context, Message, StopReason};
use ai::api::stream::{AssistantMessageEvent, StreamOptions};
use futures::{
    StreamExt,
    future::{BoxFuture, FutureExt},
};
use serde::Deserialize;
use tokio::process::Command;
use tokio_util::sync::CancellationToken;

use crate::tools::filesystem::edit::{
    EditOperations, RealEditOperations, edit_execute_with_target,
};

use crate::operations::prompt::context::{PromptTurnOptions, RuntimeSnapshot};
use crate::runtime::capability::{FilesystemCapability, FilesystemTarget, ModelCapability};
use crate::runtime::facade::CodingSessionError;
use crate::runtime::operation::control::OperationCancellationHandle;
use crate::services::runtime::stream_model_for_scoped_runtime;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelfHealingEditReplacement {
    pub old_text: String,
    pub new_text: String,
}

impl SelfHealingEditReplacement {
    pub fn new(old_text: impl Into<String>, new_text: impl Into<String>) -> Self {
        Self {
            old_text: old_text.into(),
            new_text: new_text.into(),
        }
    }

    fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "oldText": self.old_text,
            "newText": self.new_text,
        })
    }
}

#[derive(Debug, Clone)]
pub struct SelfHealingEditModelRepairOptions {
    prompt_options: PromptTurnOptions,
    max_attempts: usize,
}

impl SelfHealingEditModelRepairOptions {
    pub fn new(prompt_options: PromptTurnOptions) -> Self {
        Self {
            prompt_options,
            max_attempts: 1,
        }
    }

    pub fn with_max_attempts(mut self, attempts: usize) -> Self {
        self.max_attempts = attempts.max(1);
        self
    }

    pub fn prompt_options(&self) -> &PromptTurnOptions {
        &self.prompt_options
    }

    pub(crate) fn prompt_options_mut(&mut self) -> &mut PromptTurnOptions {
        &mut self.prompt_options
    }

    pub fn max_attempts(&self) -> usize {
        self.max_attempts
    }

    pub(crate) fn into_parts(self) -> (PromptTurnOptions, usize) {
        (self.prompt_options, self.max_attempts)
    }
}

#[derive(Debug, Clone)]
pub struct SelfHealingEditRequest {
    path: String,
    replacements: Vec<SelfHealingEditReplacement>,
    check_command: Option<String>,
    repair_attempts: Vec<Vec<SelfHealingEditReplacement>>,
    model_repair: Option<SelfHealingEditModelRepairOptions>,
}

impl SelfHealingEditRequest {
    pub fn new(path: impl Into<String>, replacements: Vec<SelfHealingEditReplacement>) -> Self {
        Self {
            path: path.into(),
            replacements,
            check_command: None,
            repair_attempts: Vec::new(),
            model_repair: None,
        }
    }

    pub fn with_check_command(mut self, command: impl Into<String>) -> Self {
        self.check_command = Some(command.into());
        self
    }

    pub fn with_repair_attempts(mut self, attempts: Vec<Vec<SelfHealingEditReplacement>>) -> Self {
        self.repair_attempts = attempts;
        self
    }

    pub fn with_model_repair(mut self, options: SelfHealingEditModelRepairOptions) -> Self {
        self.model_repair = Some(options);
        self
    }

    pub fn path(&self) -> &str {
        &self.path
    }

    pub fn replacements(&self) -> &[SelfHealingEditReplacement] {
        &self.replacements
    }

    pub fn check_command(&self) -> Option<&str> {
        self.check_command.as_deref()
    }

    pub fn repair_attempts(&self) -> &[Vec<SelfHealingEditReplacement>] {
        &self.repair_attempts
    }

    pub fn model_repair(&self) -> Option<&SelfHealingEditModelRepairOptions> {
        self.model_repair.as_ref()
    }

    pub(crate) fn model_repair_mut(&mut self) -> Option<&mut SelfHealingEditModelRepairOptions> {
        self.model_repair.as_mut()
    }

    #[allow(clippy::type_complexity)]
    pub(crate) fn into_parts(
        self,
    ) -> (
        String,
        Vec<SelfHealingEditReplacement>,
        Option<String>,
        Vec<Vec<SelfHealingEditReplacement>>,
        Option<SelfHealingEditModelRepairOptions>,
    ) {
        (
            self.path,
            self.replacements,
            self.check_command,
            self.repair_attempts,
            self.model_repair,
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelfHealingEditDiagnostic {
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelfHealingEditCheckOutput {
    pub command: String,
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelfHealingEditRepairAttempt {
    pub attempt: usize,
    pub replacements: Vec<SelfHealingEditReplacement>,
    pub diagnostics: Vec<SelfHealingEditDiagnostic>,
    pub check_output: Option<SelfHealingEditCheckOutput>,
}

pub(crate) trait SelfHealingEditCheckRunner: Send + Sync {
    fn run_check<'a>(
        &'a self,
        cwd: &'a Path,
        command: &'a str,
    ) -> BoxFuture<'a, Result<SelfHealingEditCheckOutput, String>>;
}

pub(crate) trait SelfHealingEditRepairStrategy: Send + Sync {
    fn repair<'a>(
        &'a self,
        attempt: usize,
        path: &'a str,
        replacements: &'a [SelfHealingEditReplacement],
        diagnostics: &'a [SelfHealingEditDiagnostic],
    ) -> BoxFuture<'a, Result<Vec<SelfHealingEditReplacement>, String>>;
}

pub(crate) trait SelfHealingEditObserver: Send + Sync {
    fn repair_attempted<'a>(
        &'a self,
        path: &'a str,
        repair: &'a SelfHealingEditRepairAttempt,
    ) -> BoxFuture<'a, ()>;
}

pub(crate) struct PlannedSelfHealingEditRepairStrategy {
    attempts: Vec<Vec<SelfHealingEditReplacement>>,
}

impl PlannedSelfHealingEditRepairStrategy {
    pub(crate) fn new(attempts: Vec<Vec<SelfHealingEditReplacement>>) -> Self {
        Self { attempts }
    }
}

impl SelfHealingEditRepairStrategy for PlannedSelfHealingEditRepairStrategy {
    fn repair<'a>(
        &'a self,
        attempt: usize,
        _path: &'a str,
        _replacements: &'a [SelfHealingEditReplacement],
        _diagnostics: &'a [SelfHealingEditDiagnostic],
    ) -> BoxFuture<'a, Result<Vec<SelfHealingEditReplacement>, String>> {
        async move {
            let index = attempt.saturating_sub(1);
            self.attempts.get(index).cloned().ok_or_else(|| {
                format!("self-healing edit repair attempt {attempt} was not configured")
            })
        }
        .boxed()
    }
}

pub(crate) struct ModelSelfHealingEditRepairStrategy {
    runtime: RuntimeSnapshot,
    model_capability: ModelCapability,
}

impl ModelSelfHealingEditRepairStrategy {
    pub(crate) fn new(runtime: RuntimeSnapshot, model_capability: ModelCapability) -> Self {
        Self {
            runtime,
            model_capability,
        }
    }
}

impl SelfHealingEditRepairStrategy for ModelSelfHealingEditRepairStrategy {
    fn repair<'a>(
        &'a self,
        attempt: usize,
        path: &'a str,
        replacements: &'a [SelfHealingEditReplacement],
        diagnostics: &'a [SelfHealingEditDiagnostic],
    ) -> BoxFuture<'a, Result<Vec<SelfHealingEditReplacement>, String>> {
        async move {
            let prompt = model_repair_prompt(attempt, path, replacements, diagnostics);
            let response =
                stream_model_repair(&self.runtime, &self.model_capability, prompt).await?;
            parse_model_repair_response(&response)
        }
        .boxed()
    }
}

#[derive(Clone)]
pub(crate) struct SelfHealingEditOptions {
    filesystem: FilesystemCapability,
    target: Option<FilesystemTarget>,
    path: String,
    replacements: Vec<SelfHealingEditReplacement>,
    operations: Arc<dyn EditOperations>,
    check_command: Option<String>,
    check_runner: Option<Arc<dyn SelfHealingEditCheckRunner>>,
    repair_strategy: Option<Arc<dyn SelfHealingEditRepairStrategy>>,
    max_repair_attempts: usize,
    repair_observer: Option<Arc<dyn SelfHealingEditObserver>>,
}

impl SelfHealingEditOptions {
    pub(crate) fn new(
        cwd: impl Into<PathBuf>,
        path: impl Into<String>,
        replacements: Vec<SelfHealingEditReplacement>,
    ) -> Result<Self, CodingSessionError> {
        let filesystem = FilesystemCapability::new(cwd.into())?;
        Ok(Self::from_filesystem(filesystem, path, replacements))
    }

    pub(crate) fn from_filesystem(
        filesystem: FilesystemCapability,
        path: impl Into<String>,
        replacements: Vec<SelfHealingEditReplacement>,
    ) -> Self {
        Self {
            filesystem,
            target: None,
            path: path.into(),
            replacements,
            operations: Arc::new(RealEditOperations),
            check_command: None,
            check_runner: None,
            repair_strategy: None,
            max_repair_attempts: 0,
            repair_observer: None,
        }
    }

    pub(crate) fn from_bound_target(
        filesystem: FilesystemCapability,
        target: FilesystemTarget,
        path: impl Into<String>,
        replacements: Vec<SelfHealingEditReplacement>,
    ) -> Self {
        let mut options = Self::from_filesystem(filesystem, path, replacements);
        options.target = Some(target);
        options
    }

    pub(crate) fn with_operations(mut self, operations: Arc<dyn EditOperations>) -> Self {
        self.operations = operations;
        self
    }

    pub(crate) fn with_check_command(mut self, command: impl Into<String>) -> Self {
        self.check_command = Some(command.into());
        self
    }

    pub(crate) fn with_real_check_runner(mut self) -> Self {
        self.check_runner = Some(Arc::new(RealSelfHealingEditCheckRunner));
        self
    }

    pub(crate) fn with_repair_strategy(
        mut self,
        strategy: Arc<dyn SelfHealingEditRepairStrategy>,
    ) -> Self {
        self.repair_strategy = Some(strategy);
        self
    }

    pub(crate) fn with_max_repair_attempts(mut self, attempts: usize) -> Self {
        self.max_repair_attempts = attempts;
        self
    }

    pub(crate) fn with_repair_observer(
        mut self,
        observer: Arc<dyn SelfHealingEditObserver>,
    ) -> Self {
        self.repair_observer = Some(observer);
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelfHealingEditOutcome {
    pub path: String,
    pub message: String,
    pub diff: String,
    pub patch: String,
    pub first_changed_line: Option<usize>,
    pub attempts: usize,
    pub diagnostics: Vec<SelfHealingEditDiagnostic>,
    pub check_output: Option<SelfHealingEditCheckOutput>,
    pub repair_attempts: Vec<SelfHealingEditRepairAttempt>,
}

pub(crate) struct SelfHealingEditContext {
    options: SelfHealingEditOptions,
    target_was_read: bool,
    proposal_ready: bool,
    apply_output: Option<agent_core::api::tool::AgentToolOutput>,
    outcome: Option<SelfHealingEditOutcome>,
    diagnostics: Vec<SelfHealingEditDiagnostic>,
    attempts: usize,
    repair_attempts: usize,
    repair_attempt_records: Vec<SelfHealingEditRepairAttempt>,
    check_output: Option<SelfHealingEditCheckOutput>,
    check_failed: bool,
    cancellation_handle: Option<OperationCancellationHandle>,
}

impl SelfHealingEditContext {
    pub(crate) fn new(options: SelfHealingEditOptions) -> Self {
        Self {
            options,
            target_was_read: false,
            proposal_ready: false,
            apply_output: None,
            outcome: None,
            diagnostics: Vec::new(),
            attempts: 0,
            repair_attempts: 0,
            repair_attempt_records: Vec::new(),
            check_output: None,
            check_failed: false,
            cancellation_handle: None,
        }
    }

    pub(crate) fn repair_attempts(&self) -> &[SelfHealingEditRepairAttempt] {
        &self.repair_attempt_records
    }

    pub(crate) fn set_cancellation_handle(
        &mut self,
        cancellation_handle: OperationCancellationHandle,
    ) {
        self.cancellation_handle = Some(cancellation_handle);
    }

    pub(crate) fn finish_success(&self) -> Result<SelfHealingEditOutcome, CodingSessionError> {
        self.outcome
            .clone()
            .ok_or_else(|| CodingSessionError::Session {
                message: "self-healing edit cannot finish without a recorded result".into(),
            })
    }

    fn start_edit_workflow(&mut self) -> Result<(), CodingSessionError> {
        if self.options.path.trim().is_empty() {
            return Err(session_error("self-healing edit path must not be empty"));
        }
        if self.options.replacements.is_empty() {
            return Err(session_error(
                "self-healing edit requires at least one replacement",
            ));
        }
        self.target_was_read = false;
        self.proposal_ready = false;
        self.apply_output = None;
        self.outcome = None;
        self.attempts = 0;
        self.repair_attempts = 0;
        self.repair_attempt_records.clear();
        self.check_output = None;
        self.check_failed = false;
        Ok(())
    }

    async fn read_target(&mut self) -> Result<(), CodingSessionError> {
        if self.options.target.is_none() {
            self.options.target = Some(
                self.options
                    .filesystem
                    .prepare_target_for_tool("edit", &self.options.path)
                    .await?,
            );
        }
        let target = self
            .options
            .target
            .as_ref()
            .ok_or_else(|| session_error("self-healing edit target is not bound"))?;
        let opened = self
            .options
            .operations
            .open_file(target)
            .await
            .map_err(session_error)?;
        opened.read_file().await.map_err(session_error)?;
        self.target_was_read = true;
        Ok(())
    }

    fn propose_patch(&mut self) -> Result<(), CodingSessionError> {
        if !self.target_was_read {
            return Err(session_error(
                "self-healing edit cannot propose before reading target",
            ));
        }
        self.proposal_ready = true;
        Ok(())
    }

    fn validate_patch(&mut self) -> Result<(), CodingSessionError> {
        if !self.proposal_ready {
            return Err(session_error(
                "self-healing edit cannot validate before proposal",
            ));
        }
        for replacement in &self.options.replacements {
            if replacement.old_text.is_empty() {
                return Err(session_error(format!(
                    "oldText must not be empty in {}.",
                    self.options.path
                )));
            }
        }
        Ok(())
    }

    async fn apply_patch(&mut self) -> Result<(), CodingSessionError> {
        if let Some(cancellation_handle) = &self.cancellation_handle {
            cancellation_handle.close()?;
        }
        self.attempts += 1;
        let args = serde_json::json!({
            "path": self.options.path,
            "edits": self
                .options
                .replacements
                .iter()
                .map(SelfHealingEditReplacement::to_json)
                .collect::<Vec<_>>(),
        });
        let target = self
            .options
            .target
            .as_ref()
            .ok_or_else(|| session_error("self-healing edit target is not bound"))?;
        let output = edit_execute_with_target(target, args, self.options.operations.clone())
            .await
            .map_err(session_error)?;
        self.apply_output = Some(output);
        Ok(())
    }

    async fn run_check(&mut self) -> Result<(), CodingSessionError> {
        self.check_failed = false;
        let Some(command) = self.options.check_command.as_deref() else {
            return Ok(());
        };
        if command.trim().is_empty() {
            return Err(session_error(
                "self-healing edit check command must not be empty",
            ));
        }
        let runner = self.options.check_runner.clone().ok_or_else(|| {
            session_error("self-healing edit check command requires a check runner")
        })?;
        let output = runner
            .run_check(&self.options.filesystem.cwd, command)
            .await
            .map_err(|error| {
                session_error(format!("self-healing edit check failed to run: {error}"))
            })?;
        self.check_failed = output.exit_code != 0;
        if self.check_failed {
            self.diagnostics.push(SelfHealingEditDiagnostic {
                message: check_failure_message(&output),
            });
        }
        self.check_output = Some(output);
        Ok(())
    }

    async fn repair_patch(&mut self) -> Result<(), CodingSessionError> {
        if !self.check_failed {
            return Ok(());
        }
        let Some(strategy) = self.options.repair_strategy.clone() else {
            return Err(self.check_failure_error());
        };
        if self.options.max_repair_attempts == 0 {
            return Err(self.check_failure_error());
        }

        while self.check_failed && self.repair_attempts < self.options.max_repair_attempts {
            self.repair_attempts += 1;
            let replacements = match strategy
                .repair(
                    self.repair_attempts,
                    &self.options.path,
                    &self.options.replacements,
                    &self.diagnostics,
                )
                .await
            {
                Ok(replacements) => replacements,
                Err(error) => return Err(self.repair_failure_error(error)),
            };
            if replacements.is_empty() {
                return Err(session_error(
                    "self-healing edit repair produced no replacements",
                ));
            }
            let applied_replacements = replacements.clone();
            self.options.replacements = replacements;
            self.proposal_ready = true;
            self.validate_patch()?;
            self.apply_patch().await?;
            self.run_check().await?;
            let repair = SelfHealingEditRepairAttempt {
                attempt: self.repair_attempts,
                replacements: applied_replacements,
                diagnostics: self.diagnostics.clone(),
                check_output: self.check_output.clone(),
            };
            self.notify_repair_attempted(&repair).await;
            self.repair_attempt_records.push(repair);
        }

        if self.check_failed {
            return Err(self.check_failure_error());
        }
        Ok(())
    }

    async fn notify_repair_attempted(&self, repair: &SelfHealingEditRepairAttempt) {
        if let Some(observer) = self.options.repair_observer.as_ref() {
            observer.repair_attempted(&self.options.path, repair).await;
        }
    }

    fn check_failure_error(&self) -> CodingSessionError {
        CodingSessionError::SelfHealingEditFailed {
            message: self.latest_check_failure_message(),
            diagnostics: self.diagnostics.clone(),
            check_output: self.check_output.clone().map(Box::new),
            repair_attempts: self.repair_attempt_records.clone(),
        }
    }

    fn repair_failure_error(&self, error: impl std::fmt::Display) -> CodingSessionError {
        let message = format!("self-healing edit repair failed: {error}");
        let mut diagnostics = self.diagnostics.clone();
        diagnostics.push(SelfHealingEditDiagnostic {
            message: message.clone(),
        });
        CodingSessionError::SelfHealingEditFailed {
            message,
            diagnostics,
            check_output: self.check_output.clone().map(Box::new),
            repair_attempts: self.repair_attempt_records.clone(),
        }
    }

    fn latest_check_failure_message(&self) -> String {
        self.check_output
            .as_ref()
            .map(check_failure_message)
            .unwrap_or_else(|| "self-healing edit check failed".to_owned())
    }

    fn record_result(&mut self) -> Result<(), CodingSessionError> {
        let output = self
            .apply_output
            .as_ref()
            .ok_or_else(|| session_error("self-healing edit cannot record result before apply"))?;
        let message = output
            .content
            .iter()
            .filter_map(|block| match block {
                ContentBlock::Text { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n");
        let details = output.details.as_ref().ok_or_else(|| {
            session_error("self-healing edit output did not include edit details")
        })?;
        self.outcome = Some(SelfHealingEditOutcome {
            path: self.options.path.clone(),
            message,
            diff: details
                .get("diff")
                .and_then(|value| value.as_str())
                .unwrap_or_default()
                .to_owned(),
            patch: details
                .get("patch")
                .and_then(|value| value.as_str())
                .unwrap_or_default()
                .to_owned(),
            first_changed_line: details
                .get("firstChangedLine")
                .and_then(|value| value.as_u64())
                .map(|value| value as usize),
            attempts: self.attempts,
            diagnostics: self.diagnostics.clone(),
            check_output: self.check_output.clone(),
            repair_attempts: self.repair_attempt_records.clone(),
        });
        Ok(())
    }
}

pub(crate) struct SelfHealingEditRunner;

impl SelfHealingEditRunner {
    pub(crate) fn new() -> Result<Self, CodingSessionError> {
        Ok(Self)
    }

    pub(crate) async fn run_typed(
        &self,
        ctx: &mut SelfHealingEditContext,
        cancellation: Option<CancellationToken>,
    ) -> Result<(), CodingSessionError> {
        Self::check_cancellation(&cancellation)?;
        ctx.start_edit_workflow()?;
        Self::check_cancellation(&cancellation)?;
        ctx.read_target().await?;
        Self::check_cancellation(&cancellation)?;
        ctx.propose_patch()?;
        Self::check_cancellation(&cancellation)?;
        ctx.validate_patch()?;
        Self::check_cancellation(&cancellation)?;
        ctx.apply_patch().await?;
        Self::check_cancellation(&cancellation)?;
        ctx.run_check().await?;
        Self::check_cancellation(&cancellation)?;
        ctx.repair_patch().await?;
        Self::check_cancellation(&cancellation)?;
        ctx.record_result()?;
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

struct RealSelfHealingEditCheckRunner;

impl SelfHealingEditCheckRunner for RealSelfHealingEditCheckRunner {
    fn run_check<'a>(
        &'a self,
        cwd: &'a Path,
        command: &'a str,
    ) -> BoxFuture<'a, Result<SelfHealingEditCheckOutput, String>> {
        async move {
            let output = shell_check_command(command)
                .current_dir(cwd)
                .kill_on_drop(true)
                .output()
                .await
                .map_err(|error| error.to_string())?;
            Ok(SelfHealingEditCheckOutput {
                command: command.to_owned(),
                stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
                stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
                exit_code: output.status.code().unwrap_or(-1),
            })
        }
        .boxed()
    }
}

fn shell_check_command(command: &str) -> Command {
    #[cfg(windows)]
    {
        let mut shell = Command::new("cmd");
        shell.arg("/C").arg(command);
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x08000000;
        shell.as_std_mut().creation_flags(CREATE_NO_WINDOW);
        shell
    }
    #[cfg(not(windows))]
    {
        let mut shell = Command::new("sh");
        shell.arg("-c").arg(command);
        shell
    }
}

#[derive(Deserialize)]
struct ModelRepairResponse {
    edits: Vec<ModelRepairEdit>,
}

#[derive(Deserialize)]
struct ModelRepairEdit {
    #[serde(rename = "oldText")]
    old_text: String,
    #[serde(rename = "newText")]
    new_text: String,
}

fn model_repair_prompt(
    attempt: usize,
    path: &str,
    replacements: &[SelfHealingEditReplacement],
    diagnostics: &[SelfHealingEditDiagnostic],
) -> String {
    let replacement_values = replacements
        .iter()
        .map(SelfHealingEditReplacement::to_json)
        .collect::<Vec<_>>();
    let replacements_json =
        serde_json::to_string(&replacement_values).unwrap_or_else(|_| "[]".to_string());
    let diagnostic_messages = diagnostics
        .iter()
        .map(|diagnostic| diagnostic.message.as_str())
        .collect::<Vec<_>>();
    let diagnostics_json =
        serde_json::to_string(&diagnostic_messages).unwrap_or_else(|_| "[]".to_string());
    format!(
        "A self-healing edit check failed. Return only JSON shaped as {{\"edits\":[{{\"oldText\":\"...\",\"newText\":\"...\"}}]}} with replacements to apply to the current file.\nPath: {path}\nRepair attempt: {attempt}\nCurrent edits: {replacements_json}\nDiagnostics: {diagnostics_json}"
    )
}

async fn stream_model_repair(
    runtime: &RuntimeSnapshot,
    model_capability: &ModelCapability,
    prompt: String,
) -> Result<String, String> {
    let context = Context {
        system_prompt: runtime.system_prompt().map(str::to_owned),
        messages: vec![Message::User {
            content: vec![ContentBlock::Text {
                text: prompt,
                text_signature: None,
            }],
        }],
        tools: None,
    };
    let mut stream = stream_model_for_scoped_runtime(
        runtime,
        model_capability,
        context,
        model_repair_stream_options(runtime),
    )
    .map_err(|error| error.to_string())?;
    let mut final_text = None;
    while let Some(event) = stream.next().await {
        match event {
            AssistantMessageEvent::Done { message, .. } => {
                if matches!(message.stop_reason, StopReason::Error) {
                    return Err(message.error_message.unwrap_or_else(|| {
                        "self-healing edit model repair returned an error".into()
                    }));
                }
                final_text = Some(assistant_message_text(&message));
            }
            AssistantMessageEvent::Error { message, .. } => {
                return Err(message
                    .error_message
                    .unwrap_or_else(|| "self-healing edit model repair stream failed".into()));
            }
            AssistantMessageEvent::Start { .. }
            | AssistantMessageEvent::TextStart { .. }
            | AssistantMessageEvent::TextDelta { .. }
            | AssistantMessageEvent::TextEnd { .. }
            | AssistantMessageEvent::ThinkingStart { .. }
            | AssistantMessageEvent::ThinkingDelta { .. }
            | AssistantMessageEvent::ThinkingEnd { .. }
            | AssistantMessageEvent::ToolcallStart { .. }
            | AssistantMessageEvent::ToolcallDelta { .. }
            | AssistantMessageEvent::ToolcallEnd { .. }
            | AssistantMessageEvent::ProviderItemStart { .. }
            | AssistantMessageEvent::ProviderItemDelta { .. }
            | AssistantMessageEvent::ProviderItemEnd { .. } => {}
        }
    }
    let text = final_text.ok_or_else(|| {
        "self-healing edit model repair did not return a final message".to_string()
    })?;
    if text.trim().is_empty() {
        return Err("self-healing edit model repair returned empty text".into());
    }
    Ok(text)
}

fn model_repair_stream_options(runtime: &RuntimeSnapshot) -> Option<StreamOptions> {
    crate::app::bootstrap::build_agent_config_with_auth_diagnostics(
        runtime.model().clone(),
        runtime.system_prompt().map(str::to_owned),
        runtime.max_turns(),
        runtime.api_key().map(str::to_owned),
        runtime.auth_diagnostics().to_vec(),
        runtime.thinking_level(),
        runtime.tool_execution(),
        runtime.resources().clone(),
        runtime.settings(),
    )
    .stream_options
}

fn assistant_message_text(message: &AssistantMessage) -> String {
    message
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn parse_model_repair_response(text: &str) -> Result<Vec<SelfHealingEditReplacement>, String> {
    let response: ModelRepairResponse = serde_json::from_str(text.trim()).map_err(|error| {
        format!("self-healing edit model repair response was not valid JSON edits: {error}")
    })?;
    if response.edits.is_empty() {
        return Err("self-healing edit model repair response contained no edits".into());
    }
    Ok(response
        .edits
        .into_iter()
        .map(|edit| SelfHealingEditReplacement::new(edit.old_text, edit.new_text))
        .collect())
}

fn session_error(message: impl Into<String>) -> CodingSessionError {
    CodingSessionError::Session {
        message: message.into(),
    }
}

fn check_failure_message(output: &SelfHealingEditCheckOutput) -> String {
    let mut message = format!(
        "self-healing edit check failed: `{}` exited with {}",
        output.command, output.exit_code
    );
    let stderr = output.stderr.trim();
    let stdout = output.stdout.trim();
    if !stderr.is_empty() {
        message.push_str(&format!("; stderr: {}", compact_check_text(stderr)));
    } else if !stdout.is_empty() {
        message.push_str(&format!("; stdout: {}", compact_check_text(stdout)));
    }
    message
}

fn compact_check_text(text: &str) -> String {
    const MAX_LEN: usize = 240;
    let compact = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if compact.chars().count() <= MAX_LEN {
        compact
    } else {
        format!("{}...", compact.chars().take(MAX_LEN).collect::<String>())
    }
}
