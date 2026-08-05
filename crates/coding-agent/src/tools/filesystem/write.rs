use crate::mutex::MutexExt;
use crate::services::review::MutationTracking;
use crate::tools::FilesystemTarget;
use crate::tools::filesystem::mutation_receipt::{
    bounded_diff, receipt_from_revisions, revision, validate_fence,
};
use futures::future::{BoxFuture, FutureExt};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::io::{Read, Seek, SeekFrom, Write};
use std::sync::Arc;
use tool_contract::api::definition::{
    AuthorizationRisk, ToolBehaviorVersion, ToolCapabilities, ToolDefinition, ToolExecutionMode,
    ToolId, ToolKind,
};
use tool_contract::api::output::{ToolContent, ToolError, ToolErrorKind, ToolOutput};
use tool_contract::api::schema::schema_for;
use tool_runtime::api::{DynamicTool, ToolFuture, TypedTool};
use workspace_runtime::api::WorkspaceAccessHandle;
use workspace_runtime::api::{FileMutation, MutationGuard};

const DESCRIPTION: &str = "Write content to a file. Creates the file if it doesn't exist, overwrites if it does. Automatically creates parent directories.";

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct WriteArgs {
    path: String,
    content: String,
    #[serde(default, rename = "expectedRevision")]
    #[schemars(rename = "expectedRevision")]
    expected_revision: Option<String>,
    #[serde(default, rename = "expectedTargetFingerprint")]
    #[schemars(rename = "expectedTargetFingerprint")]
    expected_target_fingerprint: Option<String>,
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

#[cfg(test)]
async fn write_target_with_operations(
    target: &FilesystemTarget,
    args: serde_json::Value,
    ops: Arc<dyn WriteOperations>,
) -> Result<ToolOutput, String> {
    write_target_with_tracking(target, args, ops, None, None).await
}

async fn write_target_with_tracking(
    target: &FilesystemTarget,
    args: serde_json::Value,
    ops: Arc<dyn WriteOperations>,
    tracking: Option<&MutationTracking>,
    tool_call_id: Option<&str>,
) -> Result<ToolOutput, String> {
    let path = arg_str(&args, "path")?;
    let content = arg_str(&args, "content")?;
    if content.len() > crate::limits::MAX_EDIT_RESULT_BYTES {
        return Err(format!(
            "write: content exceeds the {} safety limit",
            crate::platform::io::output::format_size(crate::limits::MAX_EDIT_RESULT_BYTES)
        ));
    }
    let expected_revision = args
        .get("expectedRevision")
        .and_then(|value| value.as_str());
    let expected_target_fingerprint = args
        .get("expectedTargetFingerprint")
        .and_then(|value| value.as_str());
    let mutation = FileMutation::begin(target).await?;
    let before = if target.is_vacant() {
        None
    } else {
        let file = target.opened_file()?;
        let mut file = file
            .lock_resource("write revision")
            .map_err(|error| error.to_string())?;
        let metadata = file
            .metadata()
            .map_err(|error| format!("write: cannot stat revision source: {error}"))?;
        if metadata.len() > crate::limits::MAX_TOOL_EDIT_FILE_BYTES as u64 {
            return Err(format!(
                "write: refusing to overwrite {path} because its current content exceeds the {} safety limit",
                crate::platform::io::output::format_size(crate::limits::MAX_TOOL_EDIT_FILE_BYTES)
            ));
        }
        file.seek(SeekFrom::Start(0))
            .map_err(|error| format!("write: cannot seek revision source: {error}"))?;
        let mut bytes = Vec::with_capacity(usize::try_from(metadata.len()).unwrap_or(0));
        file.read_to_end(&mut bytes)
            .map_err(|error| format!("write: cannot read current revision for {path}: {error}"))?;
        Some((revision(&bytes), bytes))
    };
    validate_fence(
        expected_revision,
        expected_target_fingerprint,
        before.as_ref().map(|(revision, _)| revision.hash.as_str()),
        target.target_fingerprint(),
        &path,
    )?;
    let target = target.clone();
    ops.write_file(&target, content.as_bytes(), mutation)
        .await?;
    let after = revision(content.as_bytes());
    let unified_diff = before
        .as_ref()
        .map_or(Some(""), |(_, bytes)| std::str::from_utf8(bytes).ok())
        .map(|before| {
            crate::tools::filesystem::diff::generate_unified_patch(&path, before, &content)
        })
        .and_then(bounded_diff);
    let receipt = receipt_from_revisions(
        path.clone(),
        target.target_fingerprint().to_owned(),
        before.as_ref().map(|(revision, _)| revision),
        Some(&after),
        "write",
        unified_diff,
    );
    if let Some(tracking) = tracking {
        tracking
            .record(
                tool_call_id.unwrap_or("write"),
                receipt.clone(),
            )
            .await
            .map_err(|error| {
                format!(
                    "write: mutation committed but change tracking failed; reconcile required: {error}"
                )
            })?;
    }
    Ok(ToolOutput {
        content: vec![ToolContent::Text {
            text: format!("Successfully wrote {} bytes to {path}", content.len()),
        }],
        details: Some(serde_json::json!({"changeReceipt": receipt})),
        terminate: false,
    })
}

#[cfg(test)]
pub fn write_runtime_tool(
    filesystem: WorkspaceAccessHandle,
) -> Result<Arc<dyn DynamicTool>, tool_runtime::api::ToolRegistryError> {
    write_runtime_tool_with_tracking(filesystem, None)
}

pub(crate) fn write_runtime_tool_with_tracking(
    filesystem: WorkspaceAccessHandle,
    tracking: Option<MutationTracking>,
) -> Result<Arc<dyn DynamicTool>, tool_runtime::api::ToolRegistryError> {
    let definition = ToolDefinition {
        id: ToolId::new("write").expect("static tool id is valid"),
        kind: ToolKind::Function,
        description: DESCRIPTION.into(),
        parameters: schema_for::<WriteArgs>().expect("WriteArgs schema is valid"),
        capabilities: ToolCapabilities {
            read_only: false,
            execution: ToolExecutionMode::Parallel,
            cancel: true,
            timeout: true,
            streaming: false,
            provider_executed: false,
        },
        behavior: ToolBehaviorVersion::V1,
        authorization_risk: AuthorizationRisk::SideEffect,
        requirements: Vec::new(),
    };
    Ok(Arc::new(TypedTool::<WriteArgs>::new(
        definition,
        move |context, args| {
            let filesystem = filesystem.clone();
            let tracking = tracking.clone();
            Box::pin(async move {
                let target = crate::tools::filesystem_target_for_runtime_execution(
                    &filesystem,
                    &context,
                    "write",
                    &args.path,
                )
                .await
                .map_err(|error| ToolError::new(ToolErrorKind::Execution, error))?;
                write_target_with_tracking(
                    &target,
                    serde_json::to_value(&args).expect("typed write args serialize"),
                    Arc::new(RealWriteOperations),
                    tracking.as_ref(),
                    Some(&context.call_id),
                )
                .await
                .map_err(|error| ToolError::new(ToolErrorKind::Execution, error))
            }) as ToolFuture
        },
    )?))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::time::Duration;

    use futures::FutureExt;
    use tokio_util::sync::CancellationToken;
    use tool_contract::api::definition::ToolId;
    use tool_runtime::api::{ToolCallContext, ToolRegistry, ToolRuntime};

    use super::{
        RealWriteOperations, WriteOperations, write_runtime_tool, write_target_with_operations,
    };
    use crate::tools::FilesystemTarget;
    use workspace_runtime::api::MutationGuard;
    use workspace_runtime::api::WorkspaceAccessHandle;

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
        let filesystem = WorkspaceAccessHandle::open_source(temp.path().to_path_buf()).unwrap();
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

    #[tokio::test]
    async fn write_returns_change_receipt_and_rejects_a_stale_revision() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("target.txt"), "initial\n").unwrap();
        let filesystem = WorkspaceAccessHandle::open_source(temp.path().to_path_buf()).unwrap();
        let target = filesystem
            .prepare_target_for_tool("write", "target.txt")
            .await
            .unwrap();

        let output = write_target_with_operations(
            &target,
            serde_json::json!({"path": "target.txt", "content": "updated\n"}),
            Arc::new(RealWriteOperations),
        )
        .await
        .expect("write succeeds");
        let receipt = output
            .details
            .expect("write receipt")
            .get("changeReceipt")
            .cloned()
            .expect("receipt field");
        assert_eq!(receipt["origin"], "write");
        assert_eq!(receipt["before_revision"].as_str().unwrap().len(), 64);
        assert_eq!(receipt["after_revision"].as_str().unwrap().len(), 64);
        assert_eq!(receipt["byte_delta"], 0);
        assert!(
            receipt["unified_diff"]
                .as_str()
                .is_some_and(|diff| diff.contains("-initial") && diff.contains("+updated"))
        );

        let stale = write_target_with_operations(
            &target,
            serde_json::json!({
                "path": "target.txt",
                "content": "stale\n",
                "expectedRevision": "0000000000000000000000000000000000000000000000000000000000000000"
            }),
            Arc::new(RealWriteOperations),
        )
        .await
        .expect_err("stale write must be rejected");
        assert!(stale.contains("mutation fence rejected"));
    }

    #[tokio::test]
    async fn typed_write_executes_through_the_runtime_registry() {
        let temp = tempfile::tempdir().unwrap();
        let filesystem = WorkspaceAccessHandle::open_source(temp.path().to_path_buf()).unwrap();
        let mut registry = ToolRegistry::default();
        registry
            .register(write_runtime_tool(filesystem).unwrap())
            .unwrap();
        let runtime = ToolRuntime::new(registry).unwrap();
        let context = ToolCallContext::new(
            ToolId::new("write").unwrap(),
            "write-call",
            CancellationToken::new(),
        );
        let output = runtime
            .execute(
                context,
                serde_json::json!({"path": "new.txt", "content": "hello\n"}),
            )
            .await
            .expect("typed write succeeds");
        assert_eq!(output.details.unwrap()["changeReceipt"]["origin"], "write");
        assert_eq!(
            std::fs::read_to_string(temp.path().join("new.txt")).unwrap(),
            "hello\n"
        );
    }
}
