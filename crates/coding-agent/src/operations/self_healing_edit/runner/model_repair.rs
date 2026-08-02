use super::*;

pub(super) fn model_repair_prompt(
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

pub(super) async fn stream_model_repair(
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

pub(super) fn model_repair_stream_options(runtime: &RuntimeSnapshot) -> Option<StreamOptions> {
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

pub(super) fn assistant_message_text(message: &AssistantMessage) -> String {
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

pub(super) fn parse_model_repair_response(
    text: &str,
) -> Result<Vec<SelfHealingEditReplacement>, String> {
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

pub(super) fn session_error(message: impl Into<String>) -> CodingSessionError {
    CodingSessionError::Session {
        message: message.into(),
    }
}

pub(super) fn check_failure_message(output: &SelfHealingEditCheckOutput) -> String {
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

pub(super) fn compact_check_text(text: &str) -> String {
    const MAX_LEN: usize = 240;
    let compact = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if compact.chars().count() <= MAX_LEN {
        compact
    } else {
        format!("{}...", compact.chars().take(MAX_LEN).collect::<String>())
    }
}
