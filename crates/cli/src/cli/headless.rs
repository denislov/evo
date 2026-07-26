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

#[cfg(test)]
mod tests {
    use super::*;
    use coding_agent::api::event::CodingAgentProductEvent;

    fn json_lines(stdout: &str) -> Vec<serde_json::Value> {
        stdout
            .lines()
            .map(|line| serde_json::from_str(line).expect("headless output must remain JSONL"))
            .collect()
    }

    #[test]
    fn json_session_header_preserves_v3_wire_shape() {
        let header = JsonSessionHeader {
            entry_type: "session",
            version: 3,
            id: "session-1".into(),
            timestamp: "2026-07-26T00:00:00Z".into(),
            cwd: "/workspace".into(),
            parent_session: None,
        };

        let value = serde_json::to_value(header).unwrap();
        assert_eq!(value["type"], "session");
        assert_eq!(value["version"], 3);
        assert_eq!(value["cwd"], "/workspace");
        assert!(value.get("parentSession").is_none());
    }

    #[test]
    fn output_helpers_preserve_cli_newline_and_diagnostic_order() {
        assert_eq!(with_trailing_newline(String::new()), "");
        assert_eq!(with_trailing_newline("hello".into()), "hello\n");
        assert_eq!(with_trailing_newline("hello\n".into()), "hello\n");

        let diagnostics = [CodingAgentPublicDiagnostic {
            severity: CodingAgentPublicDiagnosticSeverity::Warning,
            code: "config".into(),
            summary: "warning".into(),
            origin: CodingAgentPublicDiagnosticOrigin::Configuration,
            operation_id: None,
        }];
        let output = with_diagnostics(failure("failed"), &diagnostics);
        assert_eq!(output.stderr, "config warning: warning\nfailed\n");
    }

    #[test]
    fn product_owned_fixture_preserves_complete_headless_jsonl_order_and_shape() {
        let events: Vec<CodingAgentProductEvent> = serde_json::from_str(include_str!(
            "../../../coding-agent/tests/fixtures/client_projection/headless-wire-events.json"
        ))
        .unwrap();
        let mut adapter = CodingProtocolEventAdapter::new_with_provider(
            "faux".into(),
            "faux-provider".into(),
            "faux-model".into(),
        );
        let mut stdout = String::new();

        push_protocol_events(&mut stdout, [ProtocolEvent::agent_start()]).unwrap();
        for event in &events {
            push_protocol_events(&mut stdout, adapter.push_product_event(event)).unwrap();
        }

        let lines = json_lines(&stdout);
        let types = lines
            .iter()
            .map(|line| line["type"].as_str().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(
            types,
            [
                "agent_start",
                "turn_start",
                "message_start",
                "message_update",
                "tool_authorization_required",
                "tool_authorization_denied",
                "tool_execution_start",
                "tool_execution_update",
                "tool_execution_end",
                "message_start",
                "message_end",
                "message_end",
                "turn_end",
                "agent_end",
            ]
        );
        assert_eq!(lines[3]["message"]["content"][0]["text"], "hello ");
        assert_eq!(lines[4]["request"]["authorizationId"], "authorization-1");
        assert_eq!(lines[5]["authorizationId"], "authorization-1");
        assert_eq!(lines[6]["args"]["path"], "src/lib.rs");
        assert_eq!(lines[8]["isError"], false);
        assert!(stdout.ends_with('\n'));
    }

    #[test]
    fn public_prompt_failure_keeps_stdout_valid_jsonl_and_stderr_safe() {
        let mut adapter = CodingProtocolEventAdapter::new_with_provider(
            "faux".into(),
            "faux-provider".into(),
            "faux-model".into(),
        );
        let mut stdout = String::new();
        push_protocol_events(&mut stdout, [ProtocolEvent::agent_start()]).unwrap();
        push_protocol_events(
            &mut stdout,
            adapter.push_prompt_failure("The model provider request failed."),
        )
        .unwrap();

        let output = json_error(stdout, "The model provider request failed.");
        assert_eq!(output.exit_code, 1);
        assert_eq!(output.stderr, "The model provider request failed.\n");
        assert!(!output.stderr.contains("LLM error"));
        assert_eq!(
            json_lines(&output.stdout)
                .iter()
                .map(|line| line["type"].as_str().unwrap())
                .collect::<Vec<_>>(),
            [
                "agent_start",
                "message_start",
                "message_end",
                "turn_end",
                "agent_end",
            ]
        );
    }
}
