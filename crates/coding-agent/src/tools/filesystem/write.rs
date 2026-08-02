use crate::mutex::MutexExt;
use crate::platform::fs::capability::FilesystemCapability;
use crate::platform::fs::mutation::{FileMutation, MutationGuard};
use crate::tools::FilesystemTarget;
use crate::tools::filesystem_target_for_execution;
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
        mutation: MutationGuard,
    ) -> BoxFuture<'a, Result<(), String>>;
}

#[derive(Debug, Default)]
pub struct RealWriteOperations;

impl WriteOperations for RealWriteOperations {
    fn write_file<'a>(
        &'a self,
        target: &'a FilesystemTarget,
        content: &'a [u8],
        mutation: MutationGuard,
    ) -> BoxFuture<'a, Result<(), String>> {
        let target = target.clone();
        let content = content.to_vec();
        async move {
            tokio::task::spawn_blocking(move || {
                let _mutation = mutation;
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
                    .lock_resource("write opened file")
                    .map_err(|error| error.to_string())?;
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
    let mutation = FileMutation::begin(target).await?;
    let target = target.clone();
    ops.write_file(&target, content.as_bytes(), mutation)
        .await?;
    let n = content.len();
    Ok(vec![ContentBlock::Text {
        text: format!("Successfully wrote {n} bytes to {path}"),
        text_signature: None,
    }])
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

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::time::Duration;

    use futures::FutureExt;

    use super::{WriteOperations, write_target_with_operations};
    use crate::platform::fs::capability::FilesystemCapability;
    use crate::platform::fs::mutation::MutationGuard;
    use crate::tools::FilesystemTarget;

    #[derive(Default)]
    struct BlockingWriteOperations {
        active: Arc<AtomicUsize>,
        max_active: Arc<AtomicUsize>,
        started: Arc<AtomicBool>,
        release: Arc<AtomicBool>,
    }

    impl WriteOperations for BlockingWriteOperations {
        fn write_file<'a>(
            &'a self,
            _target: &'a FilesystemTarget,
            _content: &'a [u8],
            mutation: MutationGuard,
        ) -> futures::future::BoxFuture<'a, Result<(), String>> {
            let active = self.active.clone();
            let max_active = self.max_active.clone();
            let started = self.started.clone();
            let release = self.release.clone();
            async move {
                tokio::task::spawn_blocking(move || {
                    let _mutation = mutation;
                    let active_count = active.fetch_add(1, Ordering::AcqRel) + 1;
                    max_active.fetch_max(active_count, Ordering::AcqRel);
                    started.store(true, Ordering::Release);
                    while !release.load(Ordering::Acquire) {
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    active.fetch_sub(1, Ordering::AcqRel);
                })
                .await
                .map_err(|error| format!("test blocking write failed: {error}"))
            }
            .boxed()
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cancelled_write_keeps_same_path_mutations_serial_until_blocking_owner_finishes() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("target.txt"), "initial").unwrap();
        let filesystem = FilesystemCapability::new(temp.path().to_path_buf()).unwrap();
        let target = filesystem
            .prepare_target_for_tool("write", "target.txt")
            .await
            .unwrap();
        let operations = Arc::new(BlockingWriteOperations::default());
        let first_target = target.clone();
        let first_operations = operations.clone();
        let first = tokio::spawn(async move {
            write_target_with_operations(
                &first_target,
                serde_json::json!({"path": "target.txt", "content": "first"}),
                first_operations,
            )
            .await
        });
        tokio::time::timeout(Duration::from_secs(1), async {
            while !operations.started.load(Ordering::Acquire) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("first blocking write should start");
        first.abort();

        let second_target = target.clone();
        let second_operations = operations.clone();
        let second = tokio::spawn(async move {
            write_target_with_operations(
                &second_target,
                serde_json::json!({"path": "target.txt", "content": "second"}),
                second_operations,
            )
            .await
        });
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(operations.active.load(Ordering::Acquire), 1);
        assert_eq!(operations.max_active.load(Ordering::Acquire), 1);
        operations.release.store(true, Ordering::Release);
        tokio::time::timeout(Duration::from_secs(1), second)
            .await
            .expect("second write should run after the detached first write")
            .expect("second write task should join")
            .expect("second write should succeed");
        assert_eq!(operations.max_active.load(Ordering::Acquire), 1);
    }
}
