use std::path::Path;

use crate::output::CliOutput;
use crate::protocol::events::CodingProtocolEventAdapter;
use crate::protocol::types::ProtocolEvent;
use coding_agent::api::error::{
    CodingAgentErrorCategory, CodingAgentPublicDiagnostic, CodingAgentPublicDiagnosticOrigin,
    CodingAgentPublicDiagnosticSeverity, CodingAgentPublicError,
};
use coding_agent::api::operation::{
    CodingAgentPromptExecution, CodingAgentPromptExecutionUpdate, PromptTurnMode, PromptTurnOutcome,
};
use serde::Serialize;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use uuid::Uuid;

#[derive(Debug, Serialize)]
struct JsonSessionHeader {
    #[serde(rename = "type")]
    entry_type: &'static str,
    version: u32,
    id: String,
    timestamp: String,
    cwd: String,
    #[serde(rename = "parentSession", skip_serializing_if = "Option::is_none")]
    parent_session: Option<String>,
}

pub(crate) async fn run(
    mode: PromptTurnMode,
    execution: CodingAgentPromptExecution,
    diagnostics: Vec<CodingAgentPublicDiagnostic>,
    cwd: &Path,
) -> CliOutput {
    let output = match mode {
        PromptTurnMode::Print => run_print(execution).await,
        PromptTurnMode::Json => run_json(execution, cwd).await,
        PromptTurnMode::Rpc => {
            failure("unsupported mode: rpc requires the streaming binary entry point")
        }
    };
    with_diagnostics(output, &diagnostics)
}

async fn run_print(execution: CodingAgentPromptExecution) -> CliOutput {
    match execution.run().await {
        Ok(PromptTurnOutcome::Success { final_text, .. }) => {
            success(with_trailing_newline(final_text))
        }
        Ok(PromptTurnOutcome::Aborted { reason, .. }) => failure(reason),
        Ok(PromptTurnOutcome::Failed { error, .. }) | Err(error) => print_error(error),
    }
}

async fn run_json(execution: CodingAgentPromptExecution, cwd: &Path) -> CliOutput {
    let metadata = execution.metadata().clone();
    let header = JsonSessionHeader {
        entry_type: "session",
        version: 3,
        id: Uuid::now_v7().to_string(),
        timestamp: OffsetDateTime::now_utc()
            .format(&Rfc3339)
            .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string())
            .replace("+00:00", "Z"),
        cwd: cwd.display().to_string(),
        parent_session: None,
    };
    let mut stdout = match serialize_json_line(&header) {
        Ok(line) => line,
        Err(error) => return agent_failure(error),
    };
    if let Err(error) = push_protocol_events(&mut stdout, [ProtocolEvent::agent_start()]) {
        return CliOutput {
            exit_code: 1,
            stdout,
            stderr: format!("agent failure: {error}\n"),
        };
    }

    let mut adapter = CodingProtocolEventAdapter::new_with_provider(
        metadata.api,
        metadata.provider,
        metadata.model,
    );
    let mut stream = match execution.start().await {
        Ok(stream) => stream,
        Err(error) => {
            let _ = push_protocol_events(&mut stdout, adapter.push_prompt_failure(&error.summary));
            return json_error(stdout, error.summary);
        }
    };

    loop {
        match stream.next().await {
            Ok(Some(CodingAgentPromptExecutionUpdate::Event(event))) => {
                if let Err(error) =
                    push_protocol_events(&mut stdout, adapter.push_product_event(&event))
                {
                    return CliOutput {
                        exit_code: 1,
                        stdout,
                        stderr: format!("agent failure: {error}\n"),
                    };
                }
            }
            Ok(Some(CodingAgentPromptExecutionUpdate::Completed(outcome))) => {
                return match outcome {
                    PromptTurnOutcome::Success { .. } => CliOutput {
                        exit_code: 0,
                        stdout,
                        stderr: String::new(),
                    },
                    PromptTurnOutcome::Aborted { reason, .. } => json_error(stdout, reason),
                    PromptTurnOutcome::Failed { error, .. } => json_error(stdout, error.summary),
                };
            }
            Ok(None) => return json_error(stdout, "prompt execution ended without an outcome"),
            Err(error) => {
                let _ =
                    push_protocol_events(&mut stdout, adapter.push_prompt_failure(&error.summary));
                return json_error(stdout, error.summary);
            }
        }
    }
}

fn push_protocol_events(
    stdout: &mut String,
    events: impl IntoIterator<Item = ProtocolEvent>,
) -> Result<(), serde_json::Error> {
    for event in events {
        stdout.push_str(&serialize_json_line(&event)?);
    }
    Ok(())
}

fn serialize_json_line<T: Serialize>(value: &T) -> Result<String, serde_json::Error> {
    serde_json::to_string(value).map(|line| format!("{line}\n"))
}

fn print_error(error: CodingAgentPublicError) -> CliOutput {
    match error.category {
        CodingAgentErrorCategory::Cancellation => agent_failure("cancelled"),
        CodingAgentErrorCategory::Provider => agent_failure(error.summary),
        _ => failure(error.summary),
    }
}

fn agent_failure(error: impl std::fmt::Display) -> CliOutput {
    failure(format!("agent failure: {error}"))
}

fn json_error(stdout: String, error: impl std::fmt::Display) -> CliOutput {
    CliOutput {
        exit_code: 1,
        stdout,
        stderr: format!("{error}\n"),
    }
}

fn success(stdout: String) -> CliOutput {
    CliOutput {
        exit_code: 0,
        stdout,
        stderr: String::new(),
    }
}

fn failure(error: impl std::fmt::Display) -> CliOutput {
    CliOutput {
        exit_code: 1,
        stdout: String::new(),
        stderr: format!("{error}\n"),
    }
}

fn with_diagnostics(
    mut output: CliOutput,
    diagnostics: &[CodingAgentPublicDiagnostic],
) -> CliOutput {
    let diagnostics = render_diagnostics(diagnostics);
    if !diagnostics.is_empty() {
        output.stderr = format!("{diagnostics}{}", output.stderr);
    }
    output
}

fn render_diagnostics(diagnostics: &[CodingAgentPublicDiagnostic]) -> String {
    let mut output = String::new();
    for diagnostic in diagnostics {
        let severity = match diagnostic.severity {
            CodingAgentPublicDiagnosticSeverity::Info => "info",
            CodingAgentPublicDiagnosticSeverity::Warning => "warning",
            CodingAgentPublicDiagnosticSeverity::Error => "error",
        };
        let origin = match diagnostic.origin {
            CodingAgentPublicDiagnosticOrigin::Configuration => "config",
            CodingAgentPublicDiagnosticOrigin::Profile => "profile",
            CodingAgentPublicDiagnosticOrigin::Runtime => "runtime",
            CodingAgentPublicDiagnosticOrigin::Persistence => "persistence",
            CodingAgentPublicDiagnosticOrigin::Provider => "provider",
            CodingAgentPublicDiagnosticOrigin::Tool => "tool",
        };
        if diagnostic.code == "config" {
            output.push_str(&format!("{origin} {severity}: {}\n", diagnostic.summary));
        } else {
            output.push_str(&format!(
                "{origin} {severity}: {} (code: {})\n",
                diagnostic.summary, diagnostic.code
            ));
        }
    }
    output
}

fn with_trailing_newline(text: String) -> String {
    if text.is_empty() || text.ends_with('\n') {
        text
    } else {
        format!("{text}\n")
    }
}
