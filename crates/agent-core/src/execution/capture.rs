use crate::execution::truncate::{DEFAULT_MAX_BYTES, TruncationLimit, truncate_tail};
use crate::execution::{ExecOptions, ExecutionEnv, ExecutionEvent, MAX_SHELL_OUTPUT_CHUNK_BYTES};
use crate::execution::{ExecutionError, ExecutionErrorCode};
use futures::StreamExt;

pub const MAX_SHELL_RETAINED_BYTES: usize = 512 * 1024;
pub const MAX_SHELL_RETAINED_LINES: usize = 10_000;
pub const MAX_SHELL_SPOOL_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_SHELL_OUTPUT_EVENTS: usize = 131_072;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShellCaptureOptions {
    pub max_lines: usize,
    pub max_bytes: usize,
}

impl Default for ShellCaptureOptions {
    fn default() -> Self {
        Self {
            max_lines: crate::execution::truncate::DEFAULT_MAX_LINES,
            max_bytes: DEFAULT_MAX_BYTES,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellCaptureResult {
    pub output: String,
    pub exit_code: Option<i32>,
    pub cancelled: bool,
    pub truncated: bool,
    pub full_output_path: Option<String>,
}

pub fn sanitize_binary_output(text: &str) -> String {
    text.chars()
        .filter(|ch| {
            let code = *ch as u32;
            code == 0x09
                || code == 0x0a
                || code == 0x0d
                || (code > 0x1f && !(0xfff9..=0xfffb).contains(&code))
        })
        .collect()
}

pub async fn execute_shell_with_capture<E: ExecutionEnv>(
    env: &E,
    command: &str,
    options: ShellCaptureOptions,
) -> Result<ShellCaptureResult, ExecutionError> {
    validate_capture_options(options)?;
    let spool_path = env
        .create_temp_file("bash-", ".log")
        .await
        .map_err(file_error_to_execution_error)?;
    let spool_path = spool_path.to_string_lossy().to_string();
    let mut stream = env.exec_stream(command, Some(ExecOptions::default()));
    let mut retained = String::new();
    let mut retained_was_truncated = false;
    let mut spooled_bytes = 0usize;
    let mut event_count = 0usize;
    let mut exit_code = None;

    while let Some(event) = stream.next().await {
        event_count = event_count.saturating_add(1);
        if event_count > MAX_SHELL_OUTPUT_EVENTS {
            remove_spool(env, &spool_path).await;
            return Err(ExecutionError::OutputLimit {
                message: format!(
                    "shell output exceeded the event limit of {MAX_SHELL_OUTPUT_EVENTS}"
                ),
            });
        }

        let event = match event {
            Ok(event) => event,
            Err(error) if error.code() == ExecutionErrorCode::Aborted => {
                remove_spool(env, &spool_path).await;
                return Ok(ShellCaptureResult {
                    output: String::new(),
                    exit_code: None,
                    cancelled: true,
                    truncated: false,
                    full_output_path: None,
                });
            }
            Err(error) => {
                remove_spool(env, &spool_path).await;
                return Err(error);
            }
        };

        match event {
            ExecutionEvent::Stdout(chunk) | ExecutionEvent::Stderr(chunk) => {
                if exit_code.is_some() {
                    remove_spool(env, &spool_path).await;
                    return Err(shell_protocol_error("output arrived after the exit event"));
                }
                if chunk.len() > MAX_SHELL_OUTPUT_CHUNK_BYTES {
                    remove_spool(env, &spool_path).await;
                    return Err(ExecutionError::OutputLimit {
                        message: format!(
                            "shell output chunk exceeded {MAX_SHELL_OUTPUT_CHUNK_BYTES} bytes"
                        ),
                    });
                }
                let chunk = sanitize_binary_output(&chunk).replace('\r', "");
                let Some(next_spooled_bytes) = spooled_bytes.checked_add(chunk.len()) else {
                    remove_spool(env, &spool_path).await;
                    return Err(ExecutionError::OutputLimit {
                        message: "shell output byte accounting overflowed".into(),
                    });
                };
                if next_spooled_bytes > MAX_SHELL_SPOOL_BYTES {
                    remove_spool(env, &spool_path).await;
                    return Err(ExecutionError::OutputLimit {
                        message: format!(
                            "shell output exceeded the spool limit of {MAX_SHELL_SPOOL_BYTES} bytes"
                        ),
                    });
                }
                if let Err(error) = env.append_file(&spool_path, chunk.as_bytes()).await {
                    remove_spool(env, &spool_path).await;
                    return Err(file_error_to_execution_error(error));
                }
                spooled_bytes = next_spooled_bytes;

                retained.push_str(&chunk);
                let truncation = truncate_tail(
                    &retained,
                    TruncationLimit {
                        max_lines: options.max_lines,
                        max_bytes: options.max_bytes,
                    },
                );
                if truncation.truncated {
                    retained_was_truncated = true;
                    retained = truncation.content;
                }
            }
            ExecutionEvent::Exit(code) => {
                if exit_code.replace(code).is_some() {
                    remove_spool(env, &spool_path).await;
                    return Err(shell_protocol_error(
                        "shell emitted more than one exit event",
                    ));
                }
            }
        }
    }

    let Some(exit_code) = exit_code else {
        remove_spool(env, &spool_path).await;
        return Err(shell_protocol_error(
            "shell output stream ended without an exit event",
        ));
    };

    let full_output_path = if retained_was_truncated {
        Some(spool_path)
    } else {
        remove_spool(env, &spool_path).await;
        None
    };

    Ok(ShellCaptureResult {
        output: retained,
        exit_code: Some(exit_code),
        cancelled: false,
        truncated: retained_was_truncated,
        full_output_path,
    })
}

fn validate_capture_options(options: ShellCaptureOptions) -> Result<(), ExecutionError> {
    if options.max_lines > MAX_SHELL_RETAINED_LINES {
        return Err(ExecutionError::OutputLimit {
            message: format!("shell retained-line limit cannot exceed {MAX_SHELL_RETAINED_LINES}"),
        });
    }
    if options.max_bytes > MAX_SHELL_RETAINED_BYTES {
        return Err(ExecutionError::OutputLimit {
            message: format!("shell retained-byte limit cannot exceed {MAX_SHELL_RETAINED_BYTES}"),
        });
    }
    Ok(())
}

fn shell_protocol_error(message: &str) -> ExecutionError {
    ExecutionError::Protocol {
        message: message.into(),
    }
}

async fn remove_spool<E: ExecutionEnv>(env: &E, path: &str) {
    let _ = env.remove(path, false, true).await;
}

fn file_error_to_execution_error(error: crate::execution::FileError) -> ExecutionError {
    ExecutionError::CallbackError {
        message: error.to_string(),
    }
}

pub fn bash_execution_to_text(
    command: &str,
    output: &str,
    exit_code: Option<i32>,
    cancelled: bool,
    truncated: bool,
    full_output_path: Option<&str>,
) -> String {
    let mut text = format!("Ran `{}`\n", command);
    if output.is_empty() {
        text.push_str("(no output)");
    } else {
        text.push_str("```\n");
        text.push_str(output);
        text.push_str("\n```");
    }
    if cancelled {
        text.push_str("\n\n(command cancelled)");
    } else if let Some(code) = exit_code
        && code != 0
    {
        text.push_str(&format!("\n\nCommand exited with code {}", code));
    }
    if truncated && let Some(path) = full_output_path {
        text.push_str(&format!("\n\n[Output truncated. Full output: {}]", path));
    }
    text
}
