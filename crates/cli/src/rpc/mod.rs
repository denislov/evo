mod commands;
mod event_queue;
pub(crate) mod events;
mod limits;
mod prompt;
mod state;
mod stats;
mod wire;

use crate::error::CliError;
use crate::protocol::json::parse_strict_json;
use crate::protocol::jsonl::{JsonlFrame, JsonlLineReader};
use crate::protocol::types::{RpcCommand, RpcResponse};
use coding_agent::api::embedding::CodingAgentApplicationStartup;
use coding_agent::api::error::{
    CodingAgentErrorCategory, CodingAgentErrorContext, CodingAgentPublicError,
};
use event_queue::RpcQueuedProductEvent;
use limits::{
    MAX_RPC_ARRAY_ITEMS, MAX_RPC_AUTHORIZATION_TOKEN_BYTES, MAX_RPC_CONTAINER_ITEMS,
    MAX_RPC_IDENTIFIER_BYTES, MAX_RPC_IMAGE_ENCODED_BYTES, MAX_RPC_IMAGE_ENCODED_TOTAL_BYTES,
    MAX_RPC_IMAGES, MAX_RPC_JSON_DEPTH, MAX_RPC_OBJECT_FIELDS, MAX_RPC_OBJECT_KEY_BYTES,
    MAX_RPC_REPAIR_ATTEMPTS, MAX_RPC_TEXT_BYTES, RPC_JSONL_FRAME_BYTES,
};
use serde_json::Value;
use state::{CodingOperationTaskResult, RpcState};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::oneshot;
pub use wire::write_rpc_response;
use wire::{command_id, command_type, is_supported_m5_command};

fn rpc_cli_error(id: Option<String>, command: impl Into<String>, error: &CliError) -> RpcResponse {
    if let CliError::Product(error) = error {
        return rpc_public_error(id, command, error.clone());
    }
    let (category, code, summary) = match error {
        CliError::AgentFailure(_) => (
            CodingAgentErrorCategory::Workflow,
            "agent_failure",
            "The coding-agent operation failed.",
        ),
        CliError::SessionFailure(_) => (
            CodingAgentErrorCategory::Session,
            "session_failure",
            "The session request failed.",
        ),
        CliError::MissingValue(_)
        | CliError::UnknownFlag(_)
        | CliError::InvalidMaxTurns(_)
        | CliError::InvalidInput(_)
        | CliError::InvalidSessionFlags(_) => (
            CodingAgentErrorCategory::Input,
            "invalid_input",
            "The RPC request is invalid.",
        ),
        CliError::Product(_) => unreachable!("product errors return above"),
    };
    rpc_public_error(
        id,
        command,
        CodingAgentPublicError {
            category,
            code: code.into(),
            retryable: false,
            summary: summary.into(),
            context: CodingAgentErrorContext::None,
        },
    )
}

fn rpc_public_error(
    id: Option<String>,
    command: impl Into<String>,
    public: CodingAgentPublicError,
) -> RpcResponse {
    let summary = public.summary.clone();
    let data = serde_json::json!({
        "category": public.category,
        "code": public.code,
        "retryable": public.retryable,
        "summary": public.summary,
        "context": public.context,
    });
    RpcResponse::error_with_data(id, command, summary, data)
}

pub(crate) async fn run_rpc_mode_for_io<R, W>(
    reader: R,
    writer: &mut W,
    application: CodingAgentApplicationStartup,
) -> Result<(), CliError>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    for diagnostic in &application.diagnostics {
        eprintln!(
            "{} {:?}: {}",
            diagnostic.code, diagnostic.severity, diagnostic.summary
        );
    }
    let mut state = RpcState::new(application)?;
    let mut lines = JsonlLineReader::with_max_frame_bytes(reader, RPC_JSONL_FRAME_BYTES);
    let result = run_rpc_loop(&mut state, &mut lines, writer).await;
    let _ = state.detach_client().await;
    result
}

async fn run_rpc_loop<R, W>(
    state: &mut RpcState,
    lines: &mut JsonlLineReader<R>,
    writer: &mut W,
) -> Result<(), CliError>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut input_closed = false;

    loop {
        if input_closed && !state.has_active_operations() {
            break;
        }

        let background_completion_rx = &mut state.background_completion_rx;
        let event = match (
            input_closed,
            state.foreground.as_mut(),
            state.session_events.as_mut(),
            state.session_events_closed,
        ) {
            (false, Some(foreground), Some(events), false) => {
                tokio::select! {
                    line = lines.read_next_frame() => RpcLoopEvent::Input(line),
                    event = events.recv() => RpcLoopEvent::CodingEvent(event),
                    done = &mut foreground.done => RpcLoopEvent::CodingPromptDone(done),
                    completion = background_completion_rx.recv() => RpcLoopEvent::BackgroundOperationDone(completion),
                }
            }
            (false, Some(foreground), _, _) => {
                tokio::select! {
                    line = lines.read_next_frame() => RpcLoopEvent::Input(line),
                    done = &mut foreground.done => RpcLoopEvent::CodingPromptDone(done),
                    completion = background_completion_rx.recv() => RpcLoopEvent::BackgroundOperationDone(completion),
                }
            }
            (true, Some(foreground), Some(events), false) => {
                tokio::select! {
                    event = events.recv() => RpcLoopEvent::CodingEvent(event),
                    done = &mut foreground.done => RpcLoopEvent::CodingPromptDone(done),
                    completion = background_completion_rx.recv() => RpcLoopEvent::BackgroundOperationDone(completion),
                }
            }
            (true, Some(foreground), _, _) => {
                tokio::select! {
                    done = &mut foreground.done => RpcLoopEvent::CodingPromptDone(done),
                    completion = background_completion_rx.recv() => RpcLoopEvent::BackgroundOperationDone(completion),
                }
            }
            (false, None, Some(events), false) => {
                tokio::select! {
                    line = lines.read_next_frame() => RpcLoopEvent::Input(line),
                    event = events.recv() => RpcLoopEvent::CodingEvent(event),
                    completion = background_completion_rx.recv() => RpcLoopEvent::BackgroundOperationDone(completion),
                }
            }
            (false, None, _, _) => {
                tokio::select! {
                    line = lines.read_next_frame() => RpcLoopEvent::Input(line),
                    completion = background_completion_rx.recv() => RpcLoopEvent::BackgroundOperationDone(completion),
                }
            }
            (true, None, Some(events), false) => {
                tokio::select! {
                    event = events.recv() => RpcLoopEvent::CodingEvent(event),
                    completion = background_completion_rx.recv() => RpcLoopEvent::BackgroundOperationDone(completion),
                }
            }
            (true, None, _, _) => {
                RpcLoopEvent::BackgroundOperationDone(background_completion_rx.recv().await)
            }
        };

        match event {
            RpcLoopEvent::Input(line) => {
                let Some(frame) = line.map_err(|e| CliError::AgentFailure(e.to_string()))? else {
                    input_closed = true;
                    continue;
                };
                match frame {
                    JsonlFrame::Line(line) => handle_input_line(state, &line, writer).await?,
                    JsonlFrame::TooLarge { max_bytes } => {
                        write_rpc_response(
                            writer,
                            RpcResponse::error_with_data(
                                None,
                                "parse",
                                "RPC request exceeds the frame-size limit",
                                serde_json::json!({
                                    "code": "request_too_large",
                                    "maxBytes": max_bytes,
                                }),
                            ),
                        )
                        .await?;
                    }
                }
            }
            RpcLoopEvent::CodingEvent(Some(RpcQueuedProductEvent::Overflow { skipped })) => {
                write_rpc_response(
                    writer,
                    RpcResponse::error_with_data(
                        None,
                        "event_stream",
                        format!(
                            "event stream lagged by {skipped} events; client must request a fresh UI snapshot"
                        ),
                        serde_json::json!({
                            "code": "event_stream_lag",
                            "skipped": skipped,
                            "recovery": "fresh_snapshot"
                        }),
                    ),
                )
                .await?;
                state.session_events_closed = true;
            }
            RpcLoopEvent::CodingEvent(Some(RpcQueuedProductEvent::Event(event))) => {
                state.write_product_event(event, writer).await?;
            }
            RpcLoopEvent::CodingEvent(None) => {
                state.session_events_closed = true;
            }
            RpcLoopEvent::CodingPromptDone(result) => {
                state.finish_coding_running_prompt(result, writer).await?;
            }
            RpcLoopEvent::BackgroundOperationDone(Some(completion)) => {
                state
                    .finish_background_operation(completion, writer)
                    .await?;
            }
            RpcLoopEvent::BackgroundOperationDone(None) => {
                return Err(CliError::AgentFailure(
                    "RPC background completion channel closed while operations were active".into(),
                ));
            }
        }
    }

    Ok(())
}

enum RpcLoopEvent {
    Input(Result<Option<JsonlFrame>, std::io::Error>),
    CodingEvent(Option<RpcQueuedProductEvent>),
    CodingPromptDone(Result<CodingOperationTaskResult, oneshot::error::RecvError>),
    BackgroundOperationDone(Option<state::RpcBackgroundCompletion>),
}

async fn handle_input_line<W>(
    state: &mut RpcState,
    line: &str,
    writer: &mut W,
) -> Result<(), CliError>
where
    W: AsyncWrite + Unpin,
{
    let value: Value = match parse_strict_json(line) {
        Ok(value) => value,
        Err(_) => {
            write_rpc_response(
                writer,
                RpcResponse::error(None, "parse", "Failed to parse command: malformed JSON"),
            )
            .await?;
            return Ok(());
        }
    };
    if let Err(limit) = validate_rpc_value(&value) {
        write_rpc_response(
            writer,
            RpcResponse::error_with_data(
                None,
                "parse",
                "RPC request exceeds an input limit",
                serde_json::json!({
                    "code": "request_too_large",
                    "limit": limit,
                }),
            ),
        )
        .await?;
        return Ok(());
    }

    let command_name = command_type(&value);
    if command_name != "hello" && state.negotiated_protocol.rpc.is_none() {
        write_rpc_response(
            writer,
            RpcResponse::error_with_data(
                command_id(&value),
                command_name,
                "RPC hello negotiation is required before commands",
                serde_json::json!({
                    "code": "protocol_negotiation_required",
                    "recovery": "send_hello",
                }),
            ),
        )
        .await?;
        return Ok(());
    }
    if !is_supported_m5_command(&command_name) {
        write_rpc_response(
            writer,
            RpcResponse::error(
                command_id(&value),
                command_name.clone(),
                format!("unsupported command in Rust M5: {command_name}"),
            ),
        )
        .await?;
        return Ok(());
    }

    let command: RpcCommand = match serde_json::from_value(value) {
        Ok(command) => command,
        Err(error) => {
            write_rpc_response(
                writer,
                RpcResponse::error(None, command_name, format!("Invalid command: {error}")),
            )
            .await?;
            return Ok(());
        }
    };

    state.handle_command(command, writer).await
}

fn validate_rpc_value(value: &Value) -> Result<(), &'static str> {
    let mut stack = vec![(value, 0_usize, None::<&str>)];
    let mut container_items = 0_usize;
    let mut encoded_image_bytes = 0_usize;

    while let Some((value, depth, field)) = stack.pop() {
        if depth > MAX_RPC_JSON_DEPTH {
            return Err("json_depth");
        }
        match value {
            Value::Object(object) => {
                if object.len() > MAX_RPC_OBJECT_FIELDS {
                    return Err("object_fields");
                }
                container_items = container_items
                    .checked_add(object.len())
                    .ok_or("container_items")?;
                if container_items > MAX_RPC_CONTAINER_ITEMS {
                    return Err("container_items");
                }
                for (key, value) in object {
                    if key.len() > MAX_RPC_OBJECT_KEY_BYTES {
                        return Err("object_key_bytes");
                    }
                    stack.push((value, depth + 1, Some(key.as_str())));
                }
            }
            Value::Array(array) => {
                if array.len() > MAX_RPC_ARRAY_ITEMS {
                    return Err("array_items");
                }
                if field == Some("images") && array.len() > MAX_RPC_IMAGES {
                    return Err("image_count");
                }
                if field == Some("repairAttempts") && array.len() > MAX_RPC_REPAIR_ATTEMPTS {
                    return Err("repair_attempt_count");
                }
                container_items = container_items
                    .checked_add(array.len())
                    .ok_or("container_items")?;
                if container_items > MAX_RPC_CONTAINER_ITEMS {
                    return Err("container_items");
                }
                stack.extend(array.iter().map(|value| (value, depth + 1, None::<&str>)));
            }
            Value::String(text) => {
                let maximum = match field {
                    Some(
                        "id" | "type" | "idempotencyKey" | "operationId" | "recoveryId"
                        | "toolCallId" | "authorizationId" | "profileId" | "teamId"
                        | "parentSession",
                    ) => MAX_RPC_IDENTIFIER_BYTES,
                    Some("authorizationToken") => MAX_RPC_AUTHORIZATION_TOKEN_BYTES,
                    Some("data") => MAX_RPC_IMAGE_ENCODED_BYTES,
                    _ => MAX_RPC_TEXT_BYTES,
                };
                if text.len() > maximum {
                    return Err(match field {
                        Some("data") => "image_encoded_bytes",
                        Some("authorizationToken") => "authorization_token_bytes",
                        Some(
                            "id" | "type" | "idempotencyKey" | "operationId" | "recoveryId"
                            | "toolCallId" | "authorizationId" | "profileId" | "teamId"
                            | "parentSession",
                        ) => "identifier_bytes",
                        _ => "text_bytes",
                    });
                }
                if field == Some("data") {
                    encoded_image_bytes = encoded_image_bytes
                        .checked_add(text.len())
                        .ok_or("image_encoded_total_bytes")?;
                    if encoded_image_bytes > MAX_RPC_IMAGE_ENCODED_TOTAL_BYTES {
                        return Err("image_encoded_total_bytes");
                    }
                }
            }
            Value::Null | Value::Bool(_) | Value::Number(_) => {}
        }
    }
    Ok(())
}

pub(crate) async fn run_rpc_mode_stdio(
    application: CodingAgentApplicationStartup,
) -> Result<(), CliError> {
    let stdin = tokio::io::stdin();
    let mut stdout = tokio::io::stdout();
    run_rpc_mode_for_io(stdin, &mut stdout, application).await
}
