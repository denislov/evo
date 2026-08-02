use crate::platform::io::output::{DEFAULT_MAX_BYTES, DEFAULT_MAX_LINES};
use crate::platform::process::ShellCapability;
use crate::platform::process::{
    EnvPolicy, OutputBudget, ProcessOutcome, ProcessSpec, ProcessUpdateCallback, ProgramKind,
    path_exists, resolve_shell_path, run as run_process,
};
use agent_core::api::tool::{AgentTool, AgentToolOutput, ToolFn, ToolUpdateCallback};
use ai::api::conversation::ContentBlock;
use futures::future::{BoxFuture, FutureExt};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

const DESCRIPTION: &str = "Execute a bash command in the working directory. Returns merged stdout and stderr. Output is truncated to the last 2000 lines or 50KB (whichever is hit first). Commands time out after 120 seconds by default; timeout is capped at 600 seconds.";
const DEFAULT_TIMEOUT_SECS: f64 = 120.0;
const MAX_TIMEOUT_SECS: f64 = 600.0;

#[derive(Clone)]
pub struct BashSpawnContext {
    pub command: String,
    pub cwd: PathBuf,
    pub env: HashMap<String, String>,
}

pub type BashSpawnHook = Arc<dyn Fn(BashSpawnContext) -> BashSpawnContext + Send + Sync>;

#[derive(Clone, Default)]
pub struct BashOptions {
    pub shell_path: Option<String>,
    pub command_prefix: Option<String>,
    pub spawn_hook: Option<BashSpawnHook>,
}

pub trait BashOperations: Send + Sync {
    fn execute<'a>(
        &'a self,
        cwd: &'a Path,
        args: serde_json::Value,
        options: &'a BashOptions,
        on_update: Option<ToolUpdateCallback>,
        cancellation: &'a CancellationToken,
    ) -> BoxFuture<'a, Result<Vec<ContentBlock>, String>>;
}

#[derive(Debug, Default)]
pub struct RealBashOperations;

impl BashOperations for RealBashOperations {
    fn execute<'a>(
        &'a self,
        cwd: &'a Path,
        args: serde_json::Value,
        options: &'a BashOptions,
        on_update: Option<ToolUpdateCallback>,
        cancellation: &'a CancellationToken,
    ) -> BoxFuture<'a, Result<Vec<ContentBlock>, String>> {
        async move { bash_execute_real(cwd, args, options, on_update, cancellation).await }.boxed()
    }
}

fn schema() -> serde_json::Value {
    serde_json::json!({
        "type":"object",
        "properties":{
            "command":{"type":"string","description":"Bash command to execute"},
            "timeout":{"type":"number","description":"Timeout in seconds (optional, default 120, max 600)"}
        },
        "required":["command"]
    })
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

fn resolve_spawn_context(
    command: String,
    cwd: PathBuf,
    spawn_hook: Option<&BashSpawnHook>,
) -> BashSpawnContext {
    let context = BashSpawnContext {
        command,
        cwd,
        env: safe_process_env(),
    };
    match spawn_hook {
        Some(hook) => hook(context),
        None => context,
    }
}

fn timeout_secs_from_args(args: &serde_json::Value) -> Result<f64, String> {
    let raw = args
        .get("timeout")
        .and_then(|v| v.as_f64())
        .unwrap_or(DEFAULT_TIMEOUT_SECS);
    if !raw.is_finite() || raw <= 0.0 {
        return Err(format!(
            "bash: timeout must be a finite positive number of seconds (max {MAX_TIMEOUT_SECS})"
        ));
    }
    Ok(raw.min(MAX_TIMEOUT_SECS))
}

pub async fn bash_execute_with_operations(
    cwd: &Path,
    args: serde_json::Value,
    options: &BashOptions,
    on_update: Option<ToolUpdateCallback>,
    cancellation: &CancellationToken,
    ops: Arc<dyn BashOperations>,
) -> Result<Vec<ContentBlock>, String> {
    ops.execute(cwd, args, options, on_update, cancellation)
        .await
}

async fn bash_execute_real(
    cwd: &Path,
    args: serde_json::Value,
    options: &BashOptions,
    on_update: Option<ToolUpdateCallback>,
    cancellation: &CancellationToken,
) -> Result<Vec<ContentBlock>, String> {
    let command = args
        .get("command")
        .and_then(|v| v.as_str())
        .ok_or("bash: missing or non-string 'command' argument")?
        .to_string();
    let timeout_secs = timeout_secs_from_args(&args)?;
    let workdir = cwd.to_path_buf();
    let resolved_command = match options.command_prefix.as_deref() {
        Some(prefix) if !prefix.is_empty() => format!("{prefix}\n{command}"),
        _ => command,
    };
    let spawn_context =
        resolve_spawn_context(resolved_command, workdir, options.spawn_hook.as_ref());
    if !path_exists(&spawn_context.cwd).await {
        return Err(format!(
            "Working directory does not exist: {}\nCannot execute bash commands.",
            spawn_context.cwd.display()
        ));
    }
    let shell_path = resolve_shell_path(options.shell_path.as_deref()).await?;

    let process_update = on_update.map(|on_update| -> ProcessUpdateCallback {
        Arc::new(move |text| {
            on_update(AgentToolOutput::new(vec![ContentBlock::Text {
                text,
                text_signature: None,
            }]));
        })
    });
    let outcome = run_process(
        ProcessSpec {
            program: ProgramKind::Shell {
                path: shell_path,
                command_arg: "-c".into(),
            },
            command: spawn_context.command,
            cwd: spawn_context.cwd,
            env: EnvPolicy::AllowList(spawn_context.env),
            timeout: Duration::from_secs_f64(timeout_secs),
            output_budget: OutputBudget::new(DEFAULT_MAX_BYTES, DEFAULT_MAX_LINES),
        },
        cancellation,
        process_update.as_ref(),
    )
    .await;
    let text = outcome.output().merged.clone();
    match outcome {
        ProcessOutcome::Completed {
            exit_code: Some(0), ..
        } => Ok(vec![ContentBlock::Text {
            text: if text.is_empty() {
                "(no output)".into()
            } else {
                text
            },
            text_signature: None,
        }]),
        ProcessOutcome::Completed {
            exit_code: Some(code),
            ..
        } => Err(append_status(
            text,
            format!("Command exited with code {code}"),
        )),
        ProcessOutcome::Completed {
            exit_code: None, ..
        } => Err(append_status(text, "Command terminated by signal")),
        ProcessOutcome::TimedOut { .. } => Err(append_status(
            text,
            format!("Command timed out after {timeout_secs} seconds"),
        )),
        ProcessOutcome::Cancelled { .. } => Err("tool execution cancelled".into()),
        ProcessOutcome::Failed { message, .. } => Err(format!("bash: {message}")),
    }
}

fn append_status(output: String, status: impl std::fmt::Display) -> String {
    if output.is_empty() {
        status.to_string()
    } else {
        format!("{output}\n\n{status}")
    }
}

pub fn bash_tool(shell: ShellCapability) -> AgentTool {
    bash_tool_with_operations(shell, Arc::new(RealBashOperations))
}

pub fn bash_tool_with_operations(
    shell: ShellCapability,
    ops: Arc<dyn BashOperations>,
) -> AgentTool {
    let execute: ToolFn = Arc::new(move |context, args, on_update| {
        let shell = shell.clone();
        let ops = ops.clone();
        let cancel_token = context.cancel_token().clone();
        Box::pin(async move {
            let options = BashOptions {
                shell_path: shell.shell_path.clone(),
                command_prefix: shell.command_prefix.clone(),
                spawn_hook: None,
            };
            bash_execute_with_operations(&shell.cwd, args, &options, on_update, &cancel_token, ops)
                .await
                .map(AgentToolOutput::new)
        })
    });
    AgentTool {
        kind: Default::default(),
        name: "bash".into(),
        description: DESCRIPTION.into(),
        parameters: schema(),
        execution_mode: Some(agent_core::api::tool::ToolExecutionMode::Sequential),
        execute,
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use ai::api::conversation::ContentBlock;
    use tokio_util::sync::CancellationToken;

    use super::{BashOptions, bash_execute_real, is_safe_env_key};

    fn text(blocks: Vec<ContentBlock>) -> String {
        match blocks.as_slice() {
            [ContentBlock::Text { text, .. }] => text.clone(),
            _ => panic!("expected exactly one text block"),
        }
    }

    #[tokio::test]
    async fn bash_preserves_success_and_exit_error_contracts() {
        let cwd = std::env::current_dir().expect("current directory");
        let cancellation = CancellationToken::new();
        let success = bash_execute_real(
            &cwd,
            serde_json::json!({"command": "printf hello"}),
            &BashOptions::default(),
            None,
            &cancellation,
        )
        .await
        .expect("bash success");
        assert_eq!(text(success), "hello");

        let no_output = bash_execute_real(
            &cwd,
            serde_json::json!({"command": ":"}),
            &BashOptions::default(),
            None,
            &cancellation,
        )
        .await
        .expect("bash no-output success");
        assert_eq!(text(no_output), "(no output)");

        let error = bash_execute_real(
            &cwd,
            serde_json::json!({"command": "printf bad; exit 7"}),
            &BashOptions::default(),
            None,
            &cancellation,
        )
        .await
        .expect_err("non-zero exit should fail");
        assert_eq!(error, "bad\n\nCommand exited with code 7");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn bash_timeout_and_cancellation_wait_for_teardown() {
        let cwd = std::env::current_dir().expect("current directory");
        let timeout_error = bash_execute_real(
            &cwd,
            serde_json::json!({"command": "printf started; sleep 300", "timeout": 0.05}),
            &BashOptions::default(),
            None,
            &CancellationToken::new(),
        )
        .await
        .expect_err("command should time out");
        assert_eq!(
            timeout_error,
            "started\n\nCommand timed out after 0.05 seconds"
        );

        let cancellation = CancellationToken::new();
        let task_token = cancellation.clone();
        let task = tokio::spawn(async move {
            bash_execute_real(
                &cwd,
                serde_json::json!({"command": "sleep 300"}),
                &BashOptions::default(),
                None,
                &task_token,
            )
            .await
        });
        tokio::time::sleep(Duration::from_millis(50)).await;
        cancellation.cancel();
        let result = tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .expect("cancelled bash should return")
            .expect("bash task should join");
        assert_eq!(result, Err("tool execution cancelled".into()));
    }

    #[test]
    fn safe_environment_includes_windows_process_bootstrap_variables() {
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
}
