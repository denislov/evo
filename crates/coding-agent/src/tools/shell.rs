use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tool_contract::api::definition::{
    AuthorizationRisk, ToolBehaviorVersion, ToolCapabilities, ToolDefinition, ToolExecutionMode,
    ToolId, ToolKind,
};
use tool_contract::api::output::{ToolContent, ToolError, ToolErrorKind, ToolOutput, ToolProgress};
use tool_contract::api::schema::schema_for;
use tool_runtime::api::{DynamicTool, ToolCallContext, ToolFuture, TypedTool};

use crate::platform::io::output::{DEFAULT_MAX_BYTES, DEFAULT_MAX_LINES};
use crate::services::background::BackgroundTaskService;
use workspace_runtime::api::{
    EnvPolicy, OutputBudget, ProcessOutcome, ProcessOutput, ProcessSpec, ProcessUpdateCallback,
    ProgramKind, TaskOwner, WorkspaceAccessHandle, path_exists, resolve_shell_path,
    run as run_process,
};

const DESCRIPTION: &str = "Execute a bash command in the working directory. Returns merged stdout and stderr. Output and progress are bounded to the last 2000 lines or 50KB. Commands time out after 120 seconds by default; the maximum is 600 seconds. Set background: true to run the command as a background task: it returns immediately with a task id, is not subject to the 600 second tool timeout, and is queried and cancelled through the session background-task API.";
const DEFAULT_TIMEOUT_SECS: f64 = 120.0;
const MAX_TIMEOUT_SECS: f64 = 600.0;

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct BashArgs {
    /// Bash command to execute.
    command: String,
    /// Timeout in seconds. Defaults to 120 and must not exceed 600.
    #[serde(default)]
    timeout: Option<f64>,
    /// Run the command as a background task: the tool returns immediately
    /// with a task id and the command keeps running past the tool call.
    /// Background tasks are not bound by the 600 second tool timeout; an
    /// explicit timeout (any finite positive number of seconds) becomes the
    /// task budget, and the task ends when it completes, is cancelled, or its
    /// session closes.
    #[serde(default)]
    background: Option<bool>,
}

impl BashArgs {
    fn validate(&self) -> Result<Duration, ToolError> {
        if self.command.len() > crate::limits::MAX_SHELL_COMMAND_BYTES {
            return Err(ToolError::new(
                ToolErrorKind::InvalidArguments,
                format!(
                    "bash: command exceeds the {} byte safety limit",
                    crate::platform::io::output::format_size(
                        crate::limits::MAX_SHELL_COMMAND_BYTES
                    )
                ),
            ));
        }
        if self.command.contains('\0') {
            return Err(ToolError::new(
                ToolErrorKind::InvalidArguments,
                "bash: command must not contain NUL bytes",
            ));
        }
        let seconds = self.timeout.unwrap_or(DEFAULT_TIMEOUT_SECS);
        if !seconds.is_finite() || seconds <= 0.0 || seconds > MAX_TIMEOUT_SECS {
            return Err(ToolError::new(
                ToolErrorKind::InvalidArguments,
                format!(
                    "bash: timeout must be a finite positive number of seconds no greater than {MAX_TIMEOUT_SECS}"
                ),
            ));
        }
        Ok(Duration::from_secs_f64(seconds))
    }

    fn validate_background(&self) -> Result<Option<Duration>, ToolError> {
        if self.command.len() > crate::limits::MAX_SHELL_COMMAND_BYTES {
            return Err(ToolError::new(
                ToolErrorKind::InvalidArguments,
                format!(
                    "bash: command exceeds the {} byte safety limit",
                    crate::platform::io::output::format_size(
                        crate::limits::MAX_SHELL_COMMAND_BYTES
                    )
                ),
            ));
        }
        if self.command.contains('\0') {
            return Err(ToolError::new(
                ToolErrorKind::InvalidArguments,
                "bash: command must not contain NUL bytes",
            ));
        }
        let Some(seconds) = self.timeout else {
            return Ok(None);
        };
        if !seconds.is_finite() || seconds <= 0.0 {
            return Err(ToolError::new(
                ToolErrorKind::InvalidArguments,
                "bash: timeout must be a finite positive number of seconds",
            ));
        }
        Ok(Some(Duration::from_secs_f64(seconds)))
    }
}

pub(crate) fn safe_process_env() -> HashMap<String, String> {
    std::env::vars()
        .filter(|(key, _)| is_safe_env_key(key))
        .collect()
}

fn is_safe_env_key(key: &str) -> bool {
    let normalized = key.to_ascii_uppercase();
    matches!(
        normalized.as_str(),
        "PATH"
            | "HOME"
            | "USER"
            | "USERNAME"
            | "SHELL"
            | "TMPDIR"
            | "TEMP"
            | "TMP"
            | "LANG"
            | "LC_ALL"
            | "LC_CTYPE"
            | "TERM"
            | "SYSTEMROOT"
            | "COMSPEC"
            | "PATHEXT"
            | "WINDIR"
            | "PROGRAMFILES"
            | "USERPROFILE"
            | "APPDATA"
            | "LOCALAPPDATA"
    ) || normalized.starts_with("LC_")
}

pub(crate) fn bash_runtime_tool(
    shell: WorkspaceAccessHandle,
    background_tasks: Option<BackgroundTaskService>,
) -> Result<Arc<dyn DynamicTool>, tool_runtime::api::ToolRegistryError> {
    let definition = ToolDefinition {
        id: ToolId::new("bash").expect("static tool id is valid"),
        kind: ToolKind::Function,
        description: DESCRIPTION.into(),
        parameters: schema_for::<BashArgs>().expect("BashArgs schema is valid"),
        capabilities: ToolCapabilities {
            read_only: false,
            execution: ToolExecutionMode::Sequential,
            cancel: true,
            timeout: true,
            streaming: true,
            provider_executed: false,
        },
        behavior: ToolBehaviorVersion::V1,
        authorization_risk: AuthorizationRisk::SideEffect,
        requirements: Vec::new(),
    };
    Ok(Arc::new(TypedTool::<BashArgs>::new(
        definition,
        move |context, args| {
            let shell = shell.clone();
            let background_tasks = background_tasks.clone();
            Box::pin(async move {
                execute_bash(&shell, background_tasks.as_ref(), &context, args).await
            }) as ToolFuture
        },
    )?))
}

async fn execute_bash(
    shell: &WorkspaceAccessHandle,
    background_tasks: Option<&BackgroundTaskService>,
    context: &ToolCallContext,
    args: BashArgs,
) -> Result<ToolOutput, ToolError> {
    let background = args.background.unwrap_or(false);
    let (timeout, task_budget) = if background {
        (None, args.validate_background()?)
    } else {
        (Some(args.validate()?), None)
    };
    if !path_exists(shell.cwd()).await {
        return Err(ToolError::new(
            ToolErrorKind::Unavailable,
            format!(
                "bash: working directory does not exist: {}",
                shell.cwd().display()
            ),
        ));
    }
    let shell_path = resolve_shell_path(shell.shell_path())
        .await
        .map_err(|error| ToolError::new(ToolErrorKind::Unavailable, error))?;
    let command = match shell.command_prefix() {
        Some(prefix) if !prefix.is_empty() => format!("{prefix}\n{}", args.command),
        _ => args.command,
    };
    if command.len() > crate::limits::MAX_SHELL_COMMAND_BYTES {
        return Err(ToolError::new(
            ToolErrorKind::InvalidArguments,
            "bash: configured command prefix pushes the command over the safety limit",
        ));
    }
    let spec = ProcessSpec {
        program: ProgramKind::Shell {
            path: shell_path,
            command_arg: "-c".into(),
        },
        command,
        cwd: shell.cwd().to_path_buf(),
        env: EnvPolicy::AllowList(safe_process_env()),
        timeout: timeout.unwrap_or_else(|| Duration::from_secs_f64(MAX_TIMEOUT_SECS)),
        output_budget: OutputBudget::new(DEFAULT_MAX_BYTES, DEFAULT_MAX_LINES),
    };
    if background {
        return execute_background(background_tasks, context, spec, task_budget).await;
    }
    let progress = process_progress(context);
    let outcome = run_process(spec, &context.cancel, progress.as_ref()).await;
    outcome_to_terminal(outcome, timeout.expect("foreground timeout"))
}

async fn execute_background(
    background_tasks: Option<&BackgroundTaskService>,
    context: &ToolCallContext,
    spec: ProcessSpec,
    task_budget: Option<Duration>,
) -> Result<ToolOutput, ToolError> {
    let Some(service) = background_tasks else {
        return Err(ToolError::new(
            ToolErrorKind::Unavailable,
            "bash: background tasks are not available in this runtime",
        ));
    };
    let owner = TaskOwner::Operation(
        context
            .operation_id
            .clone()
            .unwrap_or_else(|| "shell".into()),
    );
    let task_id = service
        .start(spec, owner, task_budget)
        .await
        .map_err(|error| ToolError::new(ToolErrorKind::Execution, error.to_string()))?;
    let snapshot = service
        .snapshot(task_id)
        .expect("the spawned task is registered");
    let initial_cursor = service
        .output(task_id, 0)
        .map(|chunk| chunk.next_cursor)
        .unwrap_or(0);
    Ok(ToolOutput {
        content: vec![ToolContent::Text {
            text: format!(
                "Background task started: {task_id}\nUse the session background-task API to query output, wait, or cancel."
            ),
        }],
        details: Some(serde_json::json!({
            "status": "background_started",
            "taskId": task_id.to_string(),
            "owner": snapshot.owner,
            "initialCursor": initial_cursor,
            "state": "running",
        })),
        terminate: false,
    })
}

fn process_progress(context: &ToolCallContext) -> Option<ProcessUpdateCallback> {
    context.progress.clone().map(|progress| {
        Arc::new(move |text| {
            let _ = progress.emit(ToolProgress {
                content: vec![ToolContent::Text { text }],
                details: Some(serde_json::json!({
                    "stream": "merged",
                    "cumulative": true,
                })),
            });
        }) as ProcessUpdateCallback
    })
}

fn outcome_to_terminal(
    outcome: ProcessOutcome,
    configured_timeout: Duration,
) -> Result<ToolOutput, ToolError> {
    match outcome {
        ProcessOutcome::Completed {
            exit_code: Some(0),
            output,
        } => Ok(success_output(output)),
        ProcessOutcome::Completed {
            exit_code: Some(exit_code),
            output,
        } => Err(outcome_error(
            ToolErrorKind::Execution,
            &output,
            Some(exit_code),
            append_status(
                output.merged.clone(),
                format!("Command exited with code {exit_code}"),
            ),
            "completed",
        )),
        ProcessOutcome::Completed {
            exit_code: None,
            output,
        } => Err(outcome_error(
            ToolErrorKind::Execution,
            &output,
            None,
            append_status(output.merged.clone(), "Command terminated by signal"),
            "signalled",
        )),
        ProcessOutcome::TimedOut { output } => Err(outcome_error(
            ToolErrorKind::Timeout,
            &output,
            None,
            append_status(
                output.merged.clone(),
                format!(
                    "Command timed out after {} seconds",
                    configured_timeout.as_secs_f64()
                ),
            ),
            "timed_out",
        )),
        ProcessOutcome::Cancelled { output } => Err(outcome_error(
            ToolErrorKind::Cancelled,
            &output,
            None,
            append_status(output.merged.clone(), "Command cancelled"),
            "cancelled",
        )),
        ProcessOutcome::Failed { message, output } => Err(outcome_error(
            ToolErrorKind::Execution,
            &output,
            None,
            append_status(output.merged.clone(), format!("bash: {message}")),
            "failed",
        )),
    }
}

fn success_output(output: ProcessOutput) -> ToolOutput {
    let text = if output.merged.is_empty() {
        "(no output)".into()
    } else {
        output.merged.clone()
    };
    ToolOutput {
        content: vec![ToolContent::Text { text }],
        details: Some(outcome_details(&output, Some(0), "completed")),
        terminate: false,
    }
}

fn outcome_error(
    kind: ToolErrorKind,
    output: &ProcessOutput,
    exit_code: Option<i32>,
    message: String,
    status: &str,
) -> ToolError {
    ToolError {
        kind,
        message,
        details: Some(outcome_details(output, exit_code, status)),
    }
}

fn outcome_details(
    output: &ProcessOutput,
    exit_code: Option<i32>,
    status: &str,
) -> serde_json::Value {
    serde_json::json!({
        "status": status,
        "exitCode": exit_code,
        "stdoutBytes": output.stdout_bytes,
        "stderrBytes": output.stderr_bytes,
    })
}

fn append_status(output: String, status: impl std::fmt::Display) -> String {
    if output.is_empty() {
        status.to_string()
    } else {
        format!("{output}\n\n{status}")
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    use tokio_util::sync::CancellationToken;
    use tool_contract::api::definition::ToolId;
    use tool_contract::api::output::{ToolContent, ToolErrorKind, ToolOutput, ToolProgress};
    use tool_runtime::api::{ProgressSink, ToolCallContext, ToolRegistry, ToolRuntime};

    use super::{bash_runtime_tool, is_safe_env_key};
    use crate::application::snapshot::SnapshotCoordinator;
    use crate::mutex::MutexExt;
    use crate::services::background::{BackgroundTaskService, CodingAgentBackgroundTaskState};
    use crate::services::event::EventService;
    use workspace_runtime::api::{TaskId, WorkspaceAccessHandle};

    fn runtime(cwd: &Path) -> ToolRuntime {
        runtime_with_background(cwd, None)
    }

    fn runtime_with_background(
        cwd: &Path,
        background_tasks: Option<BackgroundTaskService>,
    ) -> ToolRuntime {
        let mut registry = ToolRegistry::default();
        registry
            .register(
                bash_runtime_tool(
                    WorkspaceAccessHandle::open_source(cwd.to_path_buf()).unwrap(),
                    background_tasks,
                )
                .unwrap(),
            )
            .unwrap();
        ToolRuntime::new(registry).unwrap()
    }

    fn background_service() -> BackgroundTaskService {
        let coordinator = SnapshotCoordinator::new();
        let events = EventService::with_snapshot_coordinator(coordinator);
        BackgroundTaskService::new(events)
    }

    fn context(cancel: CancellationToken) -> ToolCallContext {
        ToolCallContext::new(ToolId::new("bash").unwrap(), "bash-call", cancel)
    }

    fn terminal_text(output: ToolOutput) -> String {
        match output.content.as_slice() {
            [ToolContent::Text { text }] => text.clone(),
            _ => panic!("expected one terminal text block"),
        }
    }

    #[tokio::test]
    async fn typed_bash_preserves_success_and_structures_failures() {
        let cwd = std::env::current_dir().unwrap();
        let runtime = runtime(&cwd);
        let success = runtime
            .execute(
                context(CancellationToken::new()),
                serde_json::json!({"command": "printf hello"}),
            )
            .await
            .unwrap();
        assert_eq!(terminal_text(success.clone()), "hello");
        assert_eq!(success.details.unwrap()["exitCode"], 0);

        let no_output = runtime
            .execute(
                context(CancellationToken::new()),
                serde_json::json!({"command": ":"}),
            )
            .await
            .unwrap();
        assert_eq!(terminal_text(no_output), "(no output)");

        let error = runtime
            .execute(
                context(CancellationToken::new()),
                serde_json::json!({"command": "printf bad; exit 7"}),
            )
            .await
            .unwrap_err();
        assert_eq!(error.kind, ToolErrorKind::Execution);
        assert_eq!(error.message, "bad\n\nCommand exited with code 7");
        assert_eq!(error.details.unwrap()["exitCode"], 7);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn typed_bash_timeout_and_runtime_controls_wait_for_teardown() {
        let cwd = std::env::current_dir().unwrap();
        let runtime = runtime(&cwd);
        let timeout = runtime
            .execute(
                context(CancellationToken::new()),
                serde_json::json!({
                    "command": "printf started; sleep 300",
                    "timeout": 0.05
                }),
            )
            .await
            .unwrap_err();
        assert_eq!(timeout.kind, ToolErrorKind::Timeout);
        assert!(timeout.message.contains("started"));

        let cancel = CancellationToken::new();
        let task_runtime = runtime.clone();
        let task_cancel = cancel.clone();
        let task = tokio::spawn(async move {
            task_runtime
                .execute(
                    context(task_cancel),
                    serde_json::json!({"command": "sleep 300"}),
                )
                .await
        });
        tokio::time::sleep(Duration::from_millis(50)).await;
        cancel.cancel();
        let cancelled = tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .expect("cancelled bash returns after teardown")
            .unwrap()
            .unwrap_err();
        assert_eq!(cancelled.kind, ToolErrorKind::Cancelled);

        let deadline = context(CancellationToken::new())
            .with_deadline(Some(Instant::now() + Duration::from_millis(50)));
        let timed_out = tokio::time::timeout(
            Duration::from_secs(2),
            runtime.execute(deadline, serde_json::json!({"command": "sleep 300"})),
        )
        .await
        .expect("deadline bash returns after teardown")
        .unwrap_err();
        assert_eq!(timed_out.kind, ToolErrorKind::Timeout);
    }

    #[tokio::test]
    async fn typed_bash_progress_is_bounded_and_closed_before_terminal_returns() {
        let cwd = std::env::current_dir().unwrap();
        let runtime = runtime(&cwd);
        let updates = Arc::new(Mutex::new(Vec::<String>::new()));
        let captured = updates.clone();
        let progress = ProgressSink::new(move |update| {
            if let [ToolContent::Text { text }] = update.content.as_slice() {
                captured
                    .lock_or_recover("bash progress test")
                    .push(text.clone());
            }
        });
        let observer = progress.clone();
        let output = runtime
            .execute(
                context(CancellationToken::new()).with_progress(Some(progress)),
                serde_json::json!({"command": "i=0; while [ $i -lt 4000 ]; do echo 012345678901234567890123456789; i=$((i+1)); done"}),
            )
            .await
            .unwrap();
        assert!(terminal_text(output).len() <= 52 * 1024);
        assert!(
            updates
                .lock_or_recover("bash progress test")
                .iter()
                .all(|update| update.len() <= 52 * 1024)
        );
        assert!(
            observer
                .emit(ToolProgress {
                    content: Vec::new(),
                    details: None,
                })
                .is_err()
        );
    }

    #[test]
    fn typed_definition_and_safe_environment_are_fail_closed() {
        let cwd = std::env::current_dir().unwrap();
        let tool =
            bash_runtime_tool(WorkspaceAccessHandle::open_source(cwd).unwrap(), None).unwrap();
        let definition = tool.definition();
        assert_eq!(definition.id.as_str(), "bash");
        assert_eq!(
            definition.capabilities.execution,
            tool_contract::api::definition::ToolExecutionMode::Sequential
        );
        assert!(definition.capabilities.streaming);
        assert!(!definition.capabilities.provider_executed);
        assert_eq!(definition.parameters["additionalProperties"], false);
        for key in [
            "SystemRoot",
            "ComSpec",
            "PATHEXT",
            "windir",
            "ProgramFiles",
            "USERPROFILE",
            "APPDATA",
            "LOCALAPPDATA",
        ] {
            assert!(is_safe_env_key(key), "{key} should be allowed");
        }
        assert!(!is_safe_env_key("OPENAI_API_KEY"));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn background_mode_returns_a_task_id_and_keeps_running() {
        let cwd = std::env::current_dir().unwrap();
        let service = background_service();
        let runtime = runtime_with_background(&cwd, Some(service.clone()));
        let output = runtime
            .execute(
                context(CancellationToken::new()).with_operation_id(Some("op-shell-bg".into())),
                serde_json::json!({
                    "command": "sleep 0.3; printf bg-done",
                    "background": true,
                }),
            )
            .await
            .unwrap();
        assert!(terminal_text(output.clone()).contains("Background task started"));
        let details = output.details.unwrap();
        assert_eq!(details["status"], "background_started");
        assert_eq!(details["owner"], "operation:op-shell-bg");
        let task_id = details["taskId"].as_str().expect("task id string");
        let tasks = service.list();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].task_id, task_id);
        assert_eq!(tasks[0].state, CodingAgentBackgroundTaskState::Running);
        let report = tokio::time::timeout(
            Duration::from_secs(5),
            service.wait(TaskId::from_u64(task_id.parse().unwrap())),
        )
        .await
        .expect("background task finishes");
        assert!(report.output.contains("bg-done"));
        assert_eq!(
            report.state,
            CodingAgentBackgroundTaskState::Completed { exit_code: Some(0) }
        );
        service.shutdown().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn background_mode_without_a_service_is_fail_closed() {
        let cwd = std::env::current_dir().unwrap();
        let runtime = runtime(&cwd);
        let error = runtime
            .execute(
                context(CancellationToken::new()),
                serde_json::json!({
                    "command": "echo nope",
                    "background": true,
                }),
            )
            .await
            .unwrap_err();
        assert_eq!(error.kind, ToolErrorKind::Unavailable);
        assert!(error.message.contains("background tasks are not available"));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn background_timeout_becomes_a_task_budget() {
        let cwd = std::env::current_dir().unwrap();
        let service = background_service();
        let runtime = runtime_with_background(&cwd, Some(service.clone()));
        let output = runtime
            .execute(
                context(CancellationToken::new()),
                serde_json::json!({
                    "command": "sleep 300",
                    "background": true,
                    "timeout": 0.1,
                }),
            )
            .await
            .unwrap();
        let task_id = output.details.unwrap()["taskId"]
            .as_str()
            .expect("task id string")
            .parse::<u64>()
            .unwrap();
        let report = tokio::time::timeout(
            Duration::from_secs(2),
            service.wait(TaskId::from_u64(task_id)),
        )
        .await
        .expect("task budget terminates");
        assert_eq!(report.state, CodingAgentBackgroundTaskState::TimedOut);
        service.shutdown().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn background_supports_large_timeout_budgets_beyond_the_foreground_cap() {
        let cwd = std::env::current_dir().unwrap();
        let service = background_service();
        let runtime = runtime_with_background(&cwd, Some(service.clone()));
        let output = runtime
            .execute(
                context(CancellationToken::new()),
                serde_json::json!({
                    "command": "printf ok",
                    "background": true,
                    "timeout": 3600,
                }),
            )
            .await
            .unwrap();
        let task_id = output.details.unwrap()["taskId"]
            .as_str()
            .expect("task id string")
            .parse::<u64>()
            .unwrap();
        let report = tokio::time::timeout(
            Duration::from_secs(5),
            service.wait(TaskId::from_u64(task_id)),
        )
        .await
        .expect("task completes");
        assert_eq!(report.output, "ok");
        service.shutdown().await;
    }
}
