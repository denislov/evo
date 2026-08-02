use std::fmt;
use std::path::PathBuf;

use crate::app::invocation::CodingAgentInvocationOptions;
use crate::app::prompt_runtime::PromptRuntimeOptions;
use crate::app::session::open_headless_prompt_session;
use crate::events::CodingAgentProductEvent;
use crate::runtime::facade::{
    CodingAgentOperation, CodingAgentPublicDiagnostic, CodingAgentPublicError, PromptTurnOptions,
    PromptTurnOutcome,
};
use crate::services::event::ProductEventReceiver;

/// Safe model identity needed by application-owned event presentation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodingAgentPromptExecutionMetadata {
    pub api: String,
    pub provider: String,
    pub model: String,
}

/// One ordered item from a running prompt execution.
#[derive(Debug)]
pub enum CodingAgentPromptExecutionUpdate {
    Event(CodingAgentProductEvent),
    Completed(PromptTurnOutcome),
}

/// Opaque running prompt and its product-event subscription.
pub struct CodingAgentPromptExecutionStream {
    receiver: ProductEventReceiver,
    task: Option<
        tokio::task::JoinHandle<
            Result<PromptTurnOutcome, crate::runtime::facade::CodingSessionError>,
        >,
    >,
    completed: Option<Result<PromptTurnOutcome, crate::runtime::facade::CodingSessionError>>,
    receiver_closed: bool,
    finished: bool,
}

impl fmt::Debug for CodingAgentPromptExecutionStream {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CodingAgentPromptExecutionStream")
            .field("receiver_closed", &self.receiver_closed)
            .field("finished", &self.finished)
            .finish_non_exhaustive()
    }
}

impl Drop for CodingAgentPromptExecutionStream {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

/// Opaque, one-shot product prompt execution prepared for an application adapter.
///
/// Provider credentials, executable tools, resource content, runtime models,
/// configuration, and durable session targets remain product-owned. Adapters
/// receive only the typed prompt outcome or a categorized safe startup error.
pub struct CodingAgentPromptExecution {
    options: PromptRuntimeOptions,
    metadata: CodingAgentPromptExecutionMetadata,
}

/// Product-resolved prompt execution plus safe startup diagnostics.
///
/// Process I/O, exit status, and print/JSON presentation remain application-owned.
pub struct CodingAgentPromptExecutionPreparation {
    pub execution: CodingAgentPromptExecution,
    pub diagnostics: Vec<CodingAgentPublicDiagnostic>,
}

impl fmt::Debug for CodingAgentPromptExecutionPreparation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CodingAgentPromptExecutionPreparation")
            .field("execution", &self.execution)
            .field("diagnostic_count", &self.diagnostics.len())
            .finish()
    }
}

impl fmt::Debug for CodingAgentPromptExecution {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CodingAgentPromptExecution")
            .field("has_session_options", &self.options.session.is_some())
            .field("has_session_target", &self.options.session_target.is_some())
            .finish_non_exhaustive()
    }
}

impl CodingAgentPromptExecution {
    /// Resolves one headless product invocation without taking ownership of
    /// application mode dispatch, stdio, rendering, or exit status.
    pub fn prepare(
        cwd: PathBuf,
        invocation: CodingAgentInvocationOptions,
        stdin: Option<String>,
    ) -> Result<CodingAgentPromptExecutionPreparation, CodingAgentPublicError> {
        crate::app::application::prepare_prompt_execution(cwd, invocation, stdin)
            .map_err(CodingAgentPublicError::from)
    }

    pub(crate) fn from_internal(options: PromptRuntimeOptions) -> Self {
        let metadata = CodingAgentPromptExecutionMetadata {
            api: options.model.api.clone(),
            provider: options.model.provider.clone(),
            model: options.model.id.clone(),
        };
        Self { options, metadata }
    }

    pub fn metadata(&self) -> &CodingAgentPromptExecutionMetadata {
        &self.metadata
    }

    /// Opens the product-owned session and runs the prepared prompt exactly once.
    pub async fn run(self) -> Result<PromptTurnOutcome, CodingAgentPublicError> {
        let mut session = open_headless_prompt_session(&self.options)
            .await
            .map_err(CodingAgentPublicError::from)?;
        let prompt_options = PromptTurnOptions::from_prompt_runtime_options(self.options);
        let outcome = session
            .run_internal(CodingAgentOperation::Prompt(prompt_options))
            .await
            .map_err(CodingAgentPublicError::from)?
            .into_prompt()
            .expect("prompt operation returned a different public outcome");
        Ok(outcome)
    }

    /// Starts one prompt and yields its typed ProductEvents before completion.
    pub async fn start(self) -> Result<CodingAgentPromptExecutionStream, CodingAgentPublicError> {
        let mut session = open_headless_prompt_session(&self.options)
            .await
            .map_err(CodingAgentPublicError::from)?;
        let receiver = session
            .subscribe_product_events()
            .map_err(CodingAgentPublicError::from)?;
        let prompt_options = PromptTurnOptions::from_prompt_runtime_options(self.options);
        let task = tokio::spawn(async move {
            session
                .run_internal(CodingAgentOperation::Prompt(prompt_options))
                .await
                .map(|outcome| {
                    outcome
                        .into_prompt()
                        .expect("prompt operation returned a different public outcome")
                })
        });
        Ok(CodingAgentPromptExecutionStream {
            receiver,
            task: Some(task),
            completed: None,
            receiver_closed: false,
            finished: false,
        })
    }
}

impl CodingAgentPromptExecutionPreparation {
    pub(crate) fn from_internal(
        execution: CodingAgentPromptExecution,
        diagnostics: Vec<CodingAgentPublicDiagnostic>,
    ) -> Self {
        Self {
            execution,
            diagnostics,
        }
    }
}

impl CodingAgentPromptExecutionStream {
    pub async fn next(
        &mut self,
    ) -> Result<Option<CodingAgentPromptExecutionUpdate>, CodingAgentPublicError> {
        loop {
            if self.finished {
                return Ok(None);
            }

            if !self.receiver_closed {
                match self.receiver.try_recv() {
                    Ok(Some(event)) => {
                        return Ok(Some(CodingAgentPromptExecutionUpdate::Event(event)));
                    }
                    Ok(None) => {}
                    Err(crate::runtime::facade::CodingSessionError::Cancelled) => {
                        self.receiver_closed = true;
                    }
                    Err(error) => return Err(CodingAgentPublicError::from(error)),
                }
            }

            if let Some(result) = self.completed.take() {
                self.finished = true;
                return result
                    .map(CodingAgentPromptExecutionUpdate::Completed)
                    .map(Some)
                    .map_err(CodingAgentPublicError::from);
            }

            let Some(task) = self.task.as_mut() else {
                self.finished = true;
                return Ok(None);
            };

            if self.receiver_closed {
                self.completed = Some(prompt_task_result(task.await));
                self.task = None;
                continue;
            }

            tokio::select! {
                event = self.receiver.recv() => match event {
                    Ok(event) => return Ok(Some(CodingAgentPromptExecutionUpdate::Event(event))),
                    Err(crate::runtime::facade::CodingSessionError::Cancelled) => {
                        self.receiver_closed = true;
                    }
                    Err(error) => return Err(CodingAgentPublicError::from(error)),
                },
                result = task => {
                    self.completed = Some(prompt_task_result(result));
                    self.task = None;
                }
            }
        }
    }
}

fn prompt_task_result(
    result: Result<
        Result<PromptTurnOutcome, crate::runtime::facade::CodingSessionError>,
        tokio::task::JoinError,
    >,
) -> Result<PromptTurnOutcome, crate::runtime::facade::CodingSessionError> {
    result.unwrap_or_else(|error| {
        Err(crate::runtime::facade::CodingSessionError::Workflow {
            message: format!("prompt execution task failed: {error}"),
        })
    })
}
