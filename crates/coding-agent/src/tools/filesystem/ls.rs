use crate::platform::fs::capability::FilesystemCapability;
use crate::platform::io::output::{DEFAULT_MAX_BYTES, TruncationLimit, format_size, truncate_head};
use crate::tools::FilesystemTarget;
use crate::tools::filesystem_target_for_runtime_execution;
use schemars::JsonSchema;
use serde::{Deserialize, Deserializer};
use std::sync::Arc;
use tool_contract::api::definition::{
    AuthorizationRisk, ToolBehaviorVersion, ToolCapabilities, ToolDefinition, ToolExecutionMode,
    ToolId, ToolKind,
};
use tool_contract::api::output::{ToolContent, ToolError, ToolErrorKind, ToolOutput};
use tool_contract::api::schema::schema_for;
use tool_runtime::api::{DynamicTool, ToolFuture, TypedTool};

const DESCRIPTION: &str = "List directory contents. Returns entries sorted alphabetically, with '/' suffix for directories. Includes dotfiles. Output is truncated to 500 entries or 50KB (whichever is hit first).";
const DEFAULT_LIMIT: usize = 500;
const MAX_LIMIT: usize = 5_000;

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct LsArgs {
    /// Directory to list (default: current directory).
    #[serde(default = "default_path")]
    path: String,
    /// Maximum number of entries to return (default: 500).
    #[schemars(range(min = 1, max = 5_000))]
    #[serde(default, deserialize_with = "deserialize_optional_limit")]
    limit: Option<u64>,
}

fn default_path() -> String {
    ".".into()
}

fn deserialize_optional_limit<'de, D>(deserializer: D) -> Result<Option<u64>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Option::<u64>::deserialize(deserializer)?;
    if value.is_some_and(|value| value == 0 || value > MAX_LIMIT as u64) {
        return Err(serde::de::Error::custom(format!(
            "limit must be between 1 and {MAX_LIMIT}"
        )));
    }
    Ok(value)
}

impl LsArgs {
    fn limit(&self) -> Result<usize, ToolError> {
        let limit = self.limit.unwrap_or(DEFAULT_LIMIT as u64);
        if limit == 0 || limit > MAX_LIMIT as u64 {
            return Err(ToolError::new(
                ToolErrorKind::InvalidArguments,
                format!("ls: limit must be between 1 and {MAX_LIMIT}"),
            ));
        }
        Ok(limit as usize)
    }
}

fn text_block(text: String) -> Vec<ToolContent> {
    vec![ToolContent::Text { text }]
}

async fn ls_target(target: &FilesystemTarget, args: LsArgs) -> Result<ToolOutput, ToolError> {
    let limit = args.limit()?;
    let path = args.path;
    let target_fingerprint = target.target_fingerprint();
    let target = target.clone();
    let mut entries = tokio::task::spawn_blocking(move || {
        let directory = target
            .opened_directory()
            .map_err(|error| ToolError::new(ToolErrorKind::Execution, error))?;
        let read_dir = directory.entries().map_err(|error| {
            ToolError::new(
                ToolErrorKind::Execution,
                format!(
                    "ls: cannot read directory {}: {error}",
                    target.display_path().display()
                ),
            )
        })?;
        let mut entries = Vec::new();
        for result in read_dir {
            let entry = match result {
                Ok(entry) => entry,
                Err(_) => continue,
            };
            let name = entry.file_name().to_string_lossy().into_owned();
            let file_type = match entry.file_type() {
                Ok(file_type) => file_type,
                Err(_) => continue,
            };
            entries.push(if file_type.is_dir() {
                format!("{name}/")
            } else {
                name
            });
        }
        Ok::<_, ToolError>(entries)
    })
    .await
    .map_err(|error| {
        ToolError::new(
            ToolErrorKind::Execution,
            format!("ls: blocking filesystem task failed: {error}"),
        )
    })??;

    entries.sort_by(|a, b| {
        a.to_lowercase()
            .cmp(&b.to_lowercase())
            .then_with(|| a.cmp(b))
    });

    let total_entries = entries.len();
    if entries.is_empty() {
        return Ok(ToolOutput {
            content: text_block("(empty directory)".to_string()),
            details: Some(serde_json::json!({
                "path": path,
                "target_fingerprint": target_fingerprint,
                "total_entries": 0,
                "listed_entries": 0,
                "truncated": false,
            })),
            terminate: false,
        });
    }

    let listed_entries = total_entries.min(limit);
    let entry_limit_reached = total_entries > limit;
    let output = entries
        .into_iter()
        .take(limit)
        .collect::<Vec<_>>()
        .join("\n");
    let truncation = truncate_head(
        &output,
        TruncationLimit {
            max_lines: usize::MAX,
            max_bytes: DEFAULT_MAX_BYTES,
        },
    );
    let mut output = truncation.content;

    let mut notices = Vec::new();
    if entry_limit_reached {
        let suggested = limit.saturating_mul(2).min(MAX_LIMIT);
        notices.push(if suggested > limit {
            format!("{limit} entries limit reached. Use limit={suggested} for more")
        } else {
            format!("Maximum {MAX_LIMIT} entries reached")
        });
    }
    if truncation.truncated {
        notices.push(format!("{} limit reached", format_size(DEFAULT_MAX_BYTES)));
    }
    if !notices.is_empty() {
        output.push_str(&format!("\n\n[{}]", notices.join(". ")));
    }

    Ok(ToolOutput {
        content: text_block(output),
        details: Some(serde_json::json!({
            "path": path,
            "target_fingerprint": target_fingerprint,
            "total_entries": total_entries,
            "listed_entries": listed_entries,
            "truncated": entry_limit_reached || truncation.truncated,
        })),
        terminate: false,
    })
}

pub fn ls_runtime_tool(
    filesystem: FilesystemCapability,
) -> Result<Arc<dyn DynamicTool>, tool_runtime::api::ToolRegistryError> {
    let definition = ToolDefinition {
        id: ToolId::new("ls").expect("static tool id is valid"),
        kind: ToolKind::Function,
        description: DESCRIPTION.into(),
        parameters: schema_for::<LsArgs>().expect("LsArgs schema is valid"),
        capabilities: ToolCapabilities {
            read_only: true,
            execution: ToolExecutionMode::Parallel,
            cancel: true,
            timeout: true,
            streaming: false,
            provider_executed: false,
        },
        behavior: ToolBehaviorVersion::V1,
        authorization_risk: AuthorizationRisk::WorkspaceLocalReadOnly,
        requirements: Vec::new(),
    };
    Ok(Arc::new(TypedTool::<LsArgs>::new(
        definition,
        move |context, args| {
            let filesystem = filesystem.clone();
            Box::pin(async move {
                let target = filesystem_target_for_runtime_execution(
                    &filesystem,
                    &context,
                    "ls",
                    &args.path,
                )
                .await
                .map_err(|error| ToolError::new(ToolErrorKind::Execution, error))?;
                ls_target(&target, args).await
            }) as ToolFuture
        },
    )?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio_util::sync::CancellationToken;
    use tool_runtime::api::{ToolCallContext, ToolRegistry, ToolRuntime};

    fn runtime(filesystem: FilesystemCapability) -> ToolRuntime {
        let mut registry = ToolRegistry::default();
        registry
            .register(ls_runtime_tool(filesystem).unwrap())
            .unwrap();
        ToolRuntime::new(registry).unwrap()
    }

    fn context() -> ToolCallContext {
        ToolCallContext::new(
            ToolId::new("ls").unwrap(),
            "ls-call",
            CancellationToken::new(),
        )
    }

    #[test]
    fn typed_ls_definition_matches_limits_and_capabilities() {
        let temp = tempfile::tempdir().expect("tempdir");
        let filesystem = FilesystemCapability::new(temp.path().to_path_buf()).unwrap();
        let tool = ls_runtime_tool(filesystem).unwrap();
        let definition = tool.definition();
        assert_eq!(definition.id.as_str(), "ls");
        assert!(definition.capabilities.read_only);
        assert_eq!(
            definition.capabilities.execution,
            ToolExecutionMode::Parallel
        );
        assert!(!definition.capabilities.provider_executed);
        assert_eq!(
            definition.authorization_risk,
            AuthorizationRisk::WorkspaceLocalReadOnly
        );
        assert_eq!(definition.parameters["additionalProperties"], false);
        assert_eq!(
            definition.parameters["properties"]["limit"]["anyOf"][0]["minimum"],
            1
        );
        assert_eq!(
            definition.parameters["properties"]["limit"]["anyOf"][0]["maximum"],
            MAX_LIMIT
        );
    }

    #[tokio::test]
    async fn typed_ls_sorts_entries_and_returns_directory_identity() {
        let temp = tempfile::tempdir().expect("tempdir");
        std::fs::write(temp.path().join("zeta.txt"), "z").expect("write fixture");
        std::fs::write(temp.path().join("Alpha.txt"), "a").expect("write fixture");
        std::fs::create_dir(temp.path().join("middle")).expect("create fixture directory");
        let filesystem = FilesystemCapability::new(temp.path().to_path_buf()).unwrap();

        let output = runtime(filesystem)
            .execute(context(), serde_json::json!({}))
            .await
            .expect("typed ls succeeds");
        assert!(matches!(
            output.content.as_slice(),
            [ToolContent::Text { text }] if text == "Alpha.txt\nmiddle/\nzeta.txt"
        ));
        let details = output.details.expect("ls details");
        assert_eq!(details["path"], ".");
        assert_eq!(details["total_entries"], 3);
        assert_eq!(details["listed_entries"], 3);
        assert_eq!(details["truncated"], false);
        assert!(
            details["target_fingerprint"]
                .as_str()
                .is_some_and(|fingerprint| fingerprint.len() == 64)
        );
    }

    #[tokio::test]
    async fn typed_ls_rejects_invalid_and_unknown_arguments_structurally() {
        let temp = tempfile::tempdir().expect("tempdir");
        let filesystem = FilesystemCapability::new(temp.path().to_path_buf()).unwrap();
        let runtime = runtime(filesystem);

        let invalid_limit = runtime
            .execute(context(), serde_json::json!({"limit": 0}))
            .await
            .expect_err("zero limit is invalid");
        assert_eq!(invalid_limit.kind, ToolErrorKind::InvalidArguments);

        let unknown = runtime
            .execute(context(), serde_json::json!({"depth": 1}))
            .await
            .expect_err("unknown arguments are invalid");
        assert_eq!(unknown.kind, ToolErrorKind::InvalidArguments);
    }
}
