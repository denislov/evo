use crate::runtime::facade::FilesystemCapability;
use crate::tools::FilesystemTarget;
use crate::tools::filesystem_target_for_execution;
use crate::tools::mutation_queue::with_file_mutation_queue;
use agent_core::api::tool::{AgentTool, AgentToolOutput, ToolFn};
use ai::api::conversation::ContentBlock;
use futures::future::{BoxFuture, FutureExt};
use std::io::{Seek, SeekFrom, Write};
use std::sync::Arc;

const DESCRIPTION: &str = "Write content to a file. Creates the file if it doesn't exist, overwrites if it does. Automatically creates parent directories.";

fn schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "path": { "type": "string", "description": "Path to the file to write (relative or absolute)" },
            "content": { "type": "string", "description": "Content to write to the file" }
        },
        "required": ["path", "content"],
        "additionalProperties": false
    })
}

fn arg_str(args: &serde_json::Value, key: &str) -> Result<String, String> {
    args.get(key)
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| format!("write: missing or non-string '{key}' argument"))
}

pub trait WriteOperations: Send + Sync {
    fn write_file<'a>(
        &'a self,
        target: &'a FilesystemTarget,
        content: &'a [u8],
    ) -> BoxFuture<'a, Result<(), String>>;
}

#[derive(Debug, Default)]
pub struct RealWriteOperations;

impl WriteOperations for RealWriteOperations {
    fn write_file<'a>(
        &'a self,
        target: &'a FilesystemTarget,
        content: &'a [u8],
    ) -> BoxFuture<'a, Result<(), String>> {
        let target = target.clone();
        let content = content.to_vec();
        async move {
            tokio::task::spawn_blocking(move || {
                if target.is_vacant() {
                    let mut file = target.create_vacant_file()?;
                    file.write_all(&content).map_err(|error| {
                        format!(
                            "write: failed to write created file {}: {error}",
                            target.display_path().display()
                        )
                    })?;
                    return file.sync_all().map_err(|error| {
                        format!(
                            "write: failed to sync created file {}: {error}",
                            target.display_path().display()
                        )
                    });
                }
                let file = target.opened_file()?;
                let mut file = file
                    .lock()
                    .map_err(|_| "write: opened file lock is poisoned".to_owned())?;
                file.seek(SeekFrom::Start(0)).map_err(|error| {
                    format!(
                        "write: failed to seek opened file {}: {error}",
                        target.display_path().display()
                    )
                })?;
                file.set_len(0).map_err(|error| {
                    format!(
                        "write: failed to truncate opened file {}: {error}",
                        target.display_path().display()
                    )
                })?;
                file.write_all(&content).map_err(|error| {
                    format!(
                        "write: failed to write opened file {}: {error}",
                        target.display_path().display()
                    )
                })?;
                // Not crash-atomic (the write goes through the bound handle);
                // force the bytes to disk before reporting success.
                file.sync_all().map_err(|error| {
                    format!(
                        "write: failed to sync opened file {}: {error}",
                        target.display_path().display()
                    )
                })
            })
            .await
            .map_err(|error| format!("write: blocking filesystem task failed: {error}"))?
        }
        .boxed()
    }
}

async fn write_target_with_operations(
    target: &FilesystemTarget,
    args: serde_json::Value,
    ops: Arc<dyn WriteOperations>,
) -> Result<Vec<ContentBlock>, String> {
    let path = arg_str(&args, "path")?;
    let content = arg_str(&args, "content")?;
    let queue_path = target.display_path().to_path_buf();
    let target = target.clone();
    let ops = ops.clone();
    with_file_mutation_queue(&queue_path, move || async move {
        ops.write_file(&target, content.as_bytes()).await?;
        let n = content.len();
        Ok(vec![ContentBlock::Text {
            text: format!("Successfully wrote {n} bytes to {path}"),
            text_signature: None,
        }])
    })
    .await
}

pub fn write_tool(filesystem: FilesystemCapability) -> AgentTool {
    write_tool_with_operations(filesystem, Arc::new(RealWriteOperations))
}

pub fn write_tool_with_operations(
    filesystem: FilesystemCapability,
    ops: Arc<dyn WriteOperations>,
) -> AgentTool {
    let execute: ToolFn = Arc::new(move |context, args, _on_update| {
        let filesystem = filesystem.clone();
        let ops = ops.clone();
        Box::pin(async move {
            let path = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");
            let target =
                filesystem_target_for_execution(&filesystem, &context, "write", path).await?;
            write_target_with_operations(&target, args, ops)
                .await
                .map(AgentToolOutput::new)
        })
    });
    AgentTool {
        kind: Default::default(),
        name: "write".into(),
        description: DESCRIPTION.into(),
        parameters: schema(),
        execution_mode: None,
        execute,
    }
}
