use std::io::{self, Write};

use ai_protocol::api::conversation::{
    ContentBlock, Tool as ProviderTool, ToolKind as ProviderToolKind,
};
use tool_contract::api::definition::{ToolDefinition, ToolKind};
use tool_contract::api::output::{ToolContent, ToolError, ToolOutput, ToolProgress};

use crate::agent::types::{AgentToolOutput, AgentToolResult};

pub(crate) fn output_to_agent_result(output: ToolOutput) -> AgentToolResult {
    AgentToolResult {
        content: content_to_blocks(output.content),
        is_error: false,
        terminate: output.terminate,
        details: output.details,
    }
}

pub(crate) fn progress_to_agent_output(progress: ToolProgress) -> AgentToolOutput {
    AgentToolOutput {
        content: content_to_blocks(progress.content),
        details: progress.details,
    }
}

pub(crate) fn error_to_agent_result(error: ToolError) -> AgentToolResult {
    let kind = serde_json::to_value(error.kind).expect("ToolErrorKind always serializes");
    let mut structured = serde_json::Map::new();
    structured.insert("kind".into(), kind);
    if let Some(details) = error.details {
        structured.insert("details".into(), details);
    }

    AgentToolResult {
        content: vec![ContentBlock::Text {
            text: error.message,
            text_signature: None,
        }],
        is_error: true,
        terminate: false,
        details: Some(serde_json::json!({ "tool_error": structured })),
    }
}

pub(crate) fn error_result(
    kind: tool_contract::api::output::ToolErrorKind,
    message: impl Into<String>,
) -> AgentToolResult {
    error_to_agent_result(ToolError::new(kind, message))
}

pub(crate) fn contract_tool_declaration(definition: &ToolDefinition) -> ProviderTool {
    ProviderTool {
        kind: match definition.kind {
            ToolKind::Function => ProviderToolKind::Function,
            ToolKind::Custom => ProviderToolKind::Custom,
            ToolKind::WebSearch => ProviderToolKind::WebSearch,
        },
        name: definition.id.as_str().to_owned(),
        description: Some(definition.description.clone()),
        parameters: definition.parameters.clone(),
    }
}

pub(crate) fn serialized_output_bytes(output: &AgentToolOutput) -> Option<usize> {
    serialized_bytes(&output.content, &output.details)
}

pub(crate) fn serialized_result_bytes(result: &AgentToolResult) -> Option<usize> {
    serialized_bytes(&result.content, &result.details)
}

fn serialized_bytes(
    content: &[ContentBlock],
    details: &Option<serde_json::Value>,
) -> Option<usize> {
    let mut writer = ByteCounter::default();
    serde_json::to_writer(&mut writer, content).ok()?;
    serde_json::to_writer(&mut writer, details).ok()?;
    Some(writer.bytes)
}

#[derive(Default)]
struct ByteCounter {
    bytes: usize,
}

impl Write for ByteCounter {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.bytes = self
            .bytes
            .checked_add(buffer.len())
            .ok_or_else(|| io::Error::other("serialized tool output size overflow"))?;
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn content_to_blocks(content: Vec<ToolContent>) -> Vec<ContentBlock> {
    content
        .into_iter()
        .map(|content| match content {
            ToolContent::Text { text } => ContentBlock::Text {
                text,
                text_signature: None,
            },
            ToolContent::Image { data, mime_type } => ContentBlock::Image { data, mime_type },
            ToolContent::Json { value } => ContentBlock::Text {
                text: canonical_json_string(&value),
                text_signature: None,
            },
        })
        .collect()
}

fn canonical_json_string(value: &serde_json::Value) -> String {
    fn canonicalize(value: &serde_json::Value) -> serde_json::Value {
        match value {
            serde_json::Value::Object(object) => {
                let mut keys = object.keys().collect::<Vec<_>>();
                keys.sort_unstable();
                let mut sorted = serde_json::Map::with_capacity(object.len());
                for key in keys {
                    sorted.insert(key.clone(), canonicalize(&object[key]));
                }
                serde_json::Value::Object(sorted)
            }
            serde_json::Value::Array(items) => {
                serde_json::Value::Array(items.iter().map(canonicalize).collect())
            }
            scalar => scalar.clone(),
        }
    }

    serde_json::to_string(&canonicalize(value)).expect("serde_json::Value always serializes")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tool_contract::api::definition::{
        AuthorizationRisk, ToolBehaviorVersion, ToolCapabilities, ToolExecutionMode, ToolId,
    };
    use tool_contract::api::output::ToolErrorKind;

    fn blocks_snapshot(blocks: &[ContentBlock]) -> serde_json::Value {
        serde_json::to_value(blocks).expect("content blocks serialize")
    }

    #[test]
    fn output_mapping_golden() {
        let result = output_to_agent_result(ToolOutput {
            content: vec![
                ToolContent::Text {
                    text: "done".into(),
                },
                ToolContent::Image {
                    data: "aGVsbG8=".into(),
                    mime_type: "image/png".into(),
                },
                ToolContent::Json {
                    value: serde_json::json!({"b": 2, "a": 1}),
                },
            ],
            details: Some(serde_json::json!({"revision": "r2"})),
            terminate: true,
        });

        assert_eq!(
            serde_json::json!({
                "content": blocks_snapshot(&result.content),
                "is_error": result.is_error,
                "terminate": result.terminate,
                "details": result.details,
            }),
            serde_json::json!({
                "content": [
                    {"type": "text", "text": "done"},
                    {"type": "image", "data": "aGVsbG8=", "mimeType": "image/png"},
                    {"type": "text", "text": "{\"a\":1,\"b\":2}"},
                ],
                "is_error": false,
                "terminate": true,
                "details": {"revision": "r2"},
            })
        );
    }

    #[test]
    fn error_mapping_golden() {
        let result = error_to_agent_result(ToolError {
            kind: ToolErrorKind::Timeout,
            message: "deadline exceeded".into(),
            details: Some(serde_json::json!({"deadline_ms": 1500})),
        });

        assert_eq!(
            serde_json::json!({
                "content": blocks_snapshot(&result.content),
                "is_error": result.is_error,
                "terminate": result.terminate,
                "details": result.details,
            }),
            serde_json::json!({
                "content": [{"type": "text", "text": "deadline exceeded"}],
                "is_error": true,
                "terminate": false,
                "details": {
                    "tool_error": {
                        "kind": "timeout",
                        "details": {"deadline_ms": 1500},
                    }
                },
            })
        );
    }

    #[test]
    fn progress_mapping_golden() {
        let update = progress_to_agent_output(ToolProgress {
            content: vec![ToolContent::Text {
                text: "chunk".into(),
            }],
            details: Some(serde_json::json!({"sequence": 3})),
        });

        assert_eq!(
            serde_json::json!({
                "content": blocks_snapshot(&update.content),
                "details": update.details,
            }),
            serde_json::json!({
                "content": [{"type": "text", "text": "chunk"}],
                "details": {"sequence": 3},
            })
        );
    }

    #[test]
    fn provider_declaration_mapping_golden() {
        let definition = ToolDefinition {
            id: ToolId::new("web_search").unwrap(),
            kind: ToolKind::WebSearch,
            description: "Search the web".into(),
            parameters: serde_json::Value::Null,
            capabilities: ToolCapabilities {
                read_only: true,
                execution: ToolExecutionMode::Parallel,
                cancel: false,
                timeout: false,
                streaming: true,
                provider_executed: true,
            },
            behavior: ToolBehaviorVersion::V1,
            authorization_risk: AuthorizationRisk::None,
            requirements: Vec::new(),
        };

        assert_eq!(
            serde_json::to_value(contract_tool_declaration(&definition)).unwrap(),
            serde_json::json!({
                "type": "web_search",
                "name": "web_search",
                "description": "Search the web",
                "parameters": null,
            })
        );
    }
}
