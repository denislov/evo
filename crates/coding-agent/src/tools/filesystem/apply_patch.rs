use crate::mutex::MutexExt;
use crate::platform::fs::capability::FilesystemCapability;
use crate::platform::fs::mutation::{FileMutation, MutationGuard};
use crate::tools::FilesystemTarget;
use crate::tools::filesystem::bounded::read_target_bytes;
use crate::tools::filesystem::mutation_receipt::{content_revision, receipt, validate_fence};
use crate::tools::filesystem::patch::{FilePatch, PatchOperation, apply_file, parse_patch};
use crate::tools::filesystem_target_for_runtime_execution;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::io::{Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::sync::Arc;
use tool_contract::api::definition::{
    AuthorizationRisk, ToolBehaviorVersion, ToolCapabilities, ToolDefinition, ToolExecutionMode,
    ToolId, ToolKind, ToolRequirement,
};
use tool_contract::api::output::{
    ChangeReceipt, ToolContent, ToolError, ToolErrorKind, ToolOutput,
};
use tool_contract::api::schema::schema_for;
use tool_runtime::api::{DynamicTool, ToolCallContext, ToolFuture, TypedTool};

const DESCRIPTION: &str = "Apply a bounded Codex-style patch. Update and add operations share the filesystem mutation fence and return one ChangeReceipt per file.";

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ApplyPatchArgs {
    patch: String,
    #[serde(default, rename = "expectedRevisions")]
    #[schemars(rename = "expectedRevisions")]
    expected_revisions: Vec<ExpectedRevision>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ExpectedRevision {
    path: String,
    revision: String,
}

struct BoundPatch {
    file: FilePatch,
    target: FilesystemTarget,
    expected_revision: Option<String>,
    mutation: Option<MutationGuard>,
}

struct PlannedPatch {
    path: String,
    target: FilesystemTarget,
    created: bool,
    after: Option<Vec<u8>>,
    mutation: MutationGuard,
    receipt: ChangeReceipt,
}

pub(crate) fn apply_patch_runtime_tool(
    filesystem: FilesystemCapability,
) -> Result<Arc<dyn DynamicTool>, tool_runtime::api::ToolRegistryError> {
    let definition = ToolDefinition {
        id: ToolId::new("apply_patch").expect("static tool id is valid"),
        kind: ToolKind::Function,
        description: DESCRIPTION.into(),
        parameters: schema_for::<ApplyPatchArgs>().expect("ApplyPatchArgs schema is valid"),
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
        requirements: vec![ToolRequirement {
            tool: ToolId::new("read").expect("static tool id is valid"),
            minimum_behavior: ToolBehaviorVersion::V1,
        }],
    };
    Ok(Arc::new(TypedTool::<ApplyPatchArgs>::new(
        definition,
        move |context, args| {
            let filesystem = filesystem.clone();
            Box::pin(async move { execute_patch(&filesystem, &context, args).await }) as ToolFuture
        },
    )?))
}

async fn execute_patch(
    filesystem: &FilesystemCapability,
    context: &ToolCallContext,
    args: ApplyPatchArgs,
) -> Result<ToolOutput, ToolError> {
    let parsed = parse_patch(&args.patch)
        .map_err(|error| ToolError::new(ToolErrorKind::InvalidArguments, error.to_string()))?;
    let expected = validate_batch(&parsed.files, args.expected_revisions)?;
    let mut bound = Vec::with_capacity(parsed.files.len());
    let mut bound_paths = HashSet::<PathBuf>::new();
    for file in parsed.files {
        let target =
            filesystem_target_for_runtime_execution(filesystem, context, "apply_patch", &file.path)
                .await
                .map_err(|error| ToolError::new(ToolErrorKind::Execution, error))?;
        if !bound_paths.insert(target.display_path().to_path_buf()) {
            return Err(ToolError::new(
                ToolErrorKind::InvalidArguments,
                format!(
                    "apply_patch: multiple directives resolve to {}",
                    target.display_path().display()
                ),
            ));
        }
        let expected_revision = expected.get(&file.path).cloned();
        bound.push(BoundPatch {
            file,
            target,
            expected_revision,
            mutation: None,
        });
    }

    let mut lock_order = (0..bound.len()).collect::<Vec<_>>();
    lock_order.sort_by(|left, right| {
        bound[*left]
            .target
            .display_path()
            .cmp(bound[*right].target.display_path())
    });
    for index in lock_order {
        bound[index].mutation = Some(
            FileMutation::begin(&bound[index].target)
                .await
                .map_err(|error| ToolError::new(ToolErrorKind::Execution, error))?,
        );
    }

    let mut plans = Vec::with_capacity(bound.len());
    for entry in bound {
        plans.push(
            preflight(entry)
                .await
                .map_err(|error| ToolError::new(ToolErrorKind::Execution, error))?,
        );
    }

    let mut receipts = Vec::with_capacity(plans.len());
    for plan in plans {
        let failed_path = plan.path.clone();
        match commit(plan).await {
            Ok(receipt) => receipts.push(receipt),
            Err(cause) => {
                let mut error = ToolError::new(
                    ToolErrorKind::Execution,
                    format!("apply_patch: partial commit uncertainty at {failed_path}: {cause}"),
                );
                error.details = Some(serde_json::json!({
                    "code": "partial_commit",
                    "failedPath": failed_path,
                    "committedReceipts": receipts,
                    "stateUncertain": true,
                }));
                return Err(error);
            }
        }
    }
    Ok(ToolOutput {
        content: vec![ToolContent::Text {
            text: format!("Applied {} patch file(s).", receipts.len()),
        }],
        details: Some(serde_json::json!({"changeReceipts": receipts})),
        terminate: false,
    })
}

fn validate_batch(
    files: &[FilePatch],
    expected_revisions: Vec<ExpectedRevision>,
) -> Result<HashMap<String, String>, ToolError> {
    let mut patch_paths = HashSet::new();
    for file in files {
        if !patch_paths.insert(file.path.clone()) {
            return Err(ToolError::new(
                ToolErrorKind::InvalidArguments,
                format!("apply_patch: duplicate file directive for {}", file.path),
            ));
        }
    }
    let mut expected = HashMap::new();
    for entry in expected_revisions {
        if !patch_paths.contains(&entry.path) {
            return Err(ToolError::new(
                ToolErrorKind::InvalidArguments,
                format!(
                    "apply_patch: expected revision names a path absent from the patch: {}",
                    entry.path
                ),
            ));
        }
        if entry.revision.len() != 64
            || !entry.revision.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(ToolError::new(
                ToolErrorKind::InvalidArguments,
                format!("apply_patch: invalid SHA-256 revision for {}", entry.path),
            ));
        }
        if expected
            .insert(entry.path.clone(), entry.revision)
            .is_some()
        {
            return Err(ToolError::new(
                ToolErrorKind::InvalidArguments,
                format!(
                    "apply_patch: duplicate expected revision for {}",
                    entry.path
                ),
            ));
        }
    }
    Ok(expected)
}

async fn preflight(mut entry: BoundPatch) -> Result<PlannedPatch, String> {
    let path = entry.file.path.clone();
    let before = if entry.target.is_vacant() {
        None
    } else {
        Some(
            read_target_bytes(
                &entry.target,
                "apply_patch",
                crate::limits::MAX_TOOL_EDIT_FILE_BYTES,
            )
            .await?,
        )
    };
    validate_fence(
        entry.expected_revision.as_deref(),
        None,
        before.as_deref().map(content_revision).as_deref(),
        entry.target.target_fingerprint(),
        &path,
    )?;
    if entry.file.operation == PatchOperation::Add && before.is_some() {
        return Err(format!("cannot add {path} because it already exists"));
    }
    let existing = match entry.file.operation {
        PatchOperation::Update => Some(
            std::str::from_utf8(
                before
                    .as_deref()
                    .ok_or_else(|| format!("cannot update missing file {path}"))?,
            )
            .map_err(|error| format!("apply_patch: {path} is not valid UTF-8: {error}"))?,
        ),
        PatchOperation::Delete if before.is_none() => {
            return Err(format!("cannot delete missing file {path}"));
        }
        PatchOperation::Delete => Some(""),
        _ => None,
    };
    let updated = apply_file(existing, &entry.file).map_err(|error| error.to_string())?;
    let after = updated.map(String::into_bytes);
    if after
        .as_ref()
        .is_some_and(|bytes| bytes.len() > crate::limits::MAX_EDIT_RESULT_BYTES)
    {
        return Err(format!(
            "apply_patch: result for {path} exceeds the {} safety limit",
            crate::platform::io::output::format_size(crate::limits::MAX_EDIT_RESULT_BYTES)
        ));
    }
    let change_receipt = receipt(
        path.clone(),
        entry.target.target_fingerprint().to_owned(),
        before.as_deref(),
        after.as_deref().unwrap_or_default(),
        "apply_patch",
        None,
    );
    Ok(PlannedPatch {
        path,
        target: entry.target,
        created: before.is_none(),
        after,
        mutation: entry
            .mutation
            .take()
            .expect("batch mutation guards are acquired before preflight"),
        receipt: change_receipt,
    })
}

async fn commit(plan: PlannedPatch) -> Result<ChangeReceipt, String> {
    let PlannedPatch {
        path,
        target,
        created,
        after,
        mutation,
        receipt,
    } = plan;
    tokio::task::spawn_blocking(move || {
        let _mutation = mutation;
        let result = match after {
            None => target.remove_file(),
            Some(bytes) if created => {
                let mut file = target.create_vacant_file()?;
                file.write_all(&bytes).map_err(|error| error.to_string())?;
                file.sync_all().map_err(|error| error.to_string())
            }
            Some(bytes) => {
                let file = target.opened_file()?;
                let mut file = file
                    .lock_resource("apply patch write")
                    .map_err(|error| error.to_string())?;
                file.seek(SeekFrom::Start(0))
                    .map_err(|error| error.to_string())?;
                file.set_len(0).map_err(|error| error.to_string())?;
                file.write_all(&bytes).map_err(|error| error.to_string())?;
                file.sync_all().map_err(|error| error.to_string())
            }
        };
        result.map(|()| receipt)
    })
    .await
    .map_err(|error| format!("blocking commit for {path} failed: {error}"))?
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::fs::capability::FilesystemCapability;
    use tokio_util::sync::CancellationToken;
    use tool_runtime::api::{ToolCallContext, ToolRegistry, ToolRuntime};

    #[tokio::test]
    async fn typed_apply_patch_updates_and_adds_files() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("target.txt"), "one\ntwo\n").unwrap();
        std::fs::write(temp.path().join("deleted.txt"), "obsolete\n").unwrap();
        let filesystem = FilesystemCapability::new(temp.path().to_path_buf()).unwrap();
        let mut registry = ToolRegistry::default();
        registry
            .register(
                crate::tools::filesystem::read::read_runtime_tool(filesystem.clone()).unwrap(),
            )
            .unwrap();
        registry
            .register(apply_patch_runtime_tool(filesystem).unwrap())
            .unwrap();
        let runtime = ToolRuntime::new(registry).unwrap();
        let output = runtime
            .execute(
                ToolCallContext::new(
                    ToolId::new("apply_patch").unwrap(),
                    "patch-call",
                    CancellationToken::new(),
                ),
                serde_json::json!({
                    "patch": "*** Begin Patch\n*** Update File: target.txt\n@@\n one\n-two\n+TWO\n*** Add File: added.txt\n+created\n*** Delete File: deleted.txt\n*** End Patch\n"
                }),
            )
            .await
            .expect("typed patch succeeds");
        assert_eq!(
            std::fs::read_to_string(temp.path().join("target.txt")).unwrap(),
            "one\nTWO\n"
        );
        assert_eq!(
            std::fs::read_to_string(temp.path().join("added.txt")).unwrap(),
            "created\n"
        );
        assert!(!temp.path().join("deleted.txt").exists());
        assert_eq!(
            output.details.unwrap()["changeReceipts"]
                .as_array()
                .unwrap()
                .len(),
            3
        );
    }

    #[test]
    fn typed_apply_patch_requires_read_behavior() {
        let temp = tempfile::tempdir().unwrap();
        let filesystem = FilesystemCapability::new(temp.path().to_path_buf()).unwrap();
        let mut registry = ToolRegistry::default();
        registry
            .register(apply_patch_runtime_tool(filesystem).unwrap())
            .unwrap();
        let error = match ToolRuntime::new(registry) {
            Ok(_) => panic!("read requirement must be enforced"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("requires missing tool read"));
    }

    #[tokio::test]
    async fn typed_apply_patch_rejects_a_stale_batch_before_writing() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("first.txt"), "first-before\n").unwrap();
        std::fs::write(temp.path().join("second.txt"), "second-before\n").unwrap();
        let filesystem = FilesystemCapability::new(temp.path().to_path_buf()).unwrap();
        let mut registry = ToolRegistry::default();
        registry
            .register(
                crate::tools::filesystem::read::read_runtime_tool(filesystem.clone()).unwrap(),
            )
            .unwrap();
        registry
            .register(apply_patch_runtime_tool(filesystem).unwrap())
            .unwrap();
        let runtime = ToolRuntime::new(registry).unwrap();
        let error = runtime
            .execute(
                ToolCallContext::new(
                    ToolId::new("apply_patch").unwrap(),
                    "stale-patch-call",
                    CancellationToken::new(),
                ),
                serde_json::json!({
                    "patch": "*** Begin Patch\n*** Update File: first.txt\n@@\n-first-before\n+first-after\n*** Update File: second.txt\n@@\n-second-before\n+second-after\n*** End Patch\n",
                    "expectedRevisions": [{
                        "path": "second.txt",
                        "revision": "0000000000000000000000000000000000000000000000000000000000000000"
                    }]
                }),
            )
            .await
            .expect_err("stale patch must fail");
        assert!(error.message.contains("mutation fence rejected"));
        assert_eq!(
            std::fs::read_to_string(temp.path().join("first.txt")).unwrap(),
            "first-before\n"
        );
        assert_eq!(
            std::fs::read_to_string(temp.path().join("second.txt")).unwrap(),
            "second-before\n"
        );
    }

    #[tokio::test]
    async fn typed_apply_patch_rejects_oversized_source_before_writing() {
        let temp = tempfile::tempdir().unwrap();
        let oversized = vec![b'x'; crate::limits::MAX_TOOL_EDIT_FILE_BYTES + 1];
        std::fs::write(temp.path().join("large.txt"), &oversized).unwrap();
        let filesystem = FilesystemCapability::new(temp.path().to_path_buf()).unwrap();
        let mut registry = ToolRegistry::default();
        registry
            .register(
                crate::tools::filesystem::read::read_runtime_tool(filesystem.clone()).unwrap(),
            )
            .unwrap();
        registry
            .register(apply_patch_runtime_tool(filesystem).unwrap())
            .unwrap();
        let runtime = ToolRuntime::new(registry).unwrap();
        let error = runtime
            .execute(
                ToolCallContext::new(
                    ToolId::new("apply_patch").unwrap(),
                    "large-patch-call",
                    CancellationToken::new(),
                ),
                serde_json::json!({
                    "patch": "*** Begin Patch\n*** Update File: large.txt\n@@\n-x\n+y\n*** End Patch\n"
                }),
            )
            .await
            .expect_err("oversized patch source must fail");
        assert!(error.message.contains("safety limit"), "{error:?}");
        assert_eq!(
            std::fs::read(temp.path().join("large.txt")).unwrap(),
            oversized
        );
    }
}
