use crate::platform::io::output::{DEFAULT_MAX_BYTES, TruncationLimit, format_size, truncate_head};
use crate::tools::FilesystemTarget;
use crate::tools::filesystem_target_for_runtime_execution;
use globset::{GlobBuilder, GlobMatcher};
use schemars::JsonSchema;
use serde::{Deserialize, Deserializer};
use std::path::Path;
use std::sync::Arc;
use tool_contract::api::definition::{
    AuthorizationRisk, ToolBehaviorVersion, ToolCapabilities, ToolDefinition, ToolExecutionMode,
    ToolId, ToolKind,
};
use tool_contract::api::output::{ToolContent, ToolError, ToolErrorKind, ToolOutput};
use tool_contract::api::schema::schema_for;
use tool_runtime::api::{DynamicTool, ToolFuture, TypedTool};
use workspace_runtime::api::WorkspaceAccessHandle;
use workspace_runtime::api::{CapWalkEntryKind, CapWalkRoot, walk_target};

const DESCRIPTION: &str = "Search for files by glob pattern. Returns matching file paths relative to the search directory. Respects .gitignore. Output is truncated to 1000 results or 50KB (whichever is hit first).";
const DEFAULT_LIMIT: usize = 1000;
const MAX_LIMIT: usize = 10_000;

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct FindArgs {
    /// Glob pattern to match files, e.g. `*.rs` or `src/**/*.spec.ts`.
    pattern: String,
    /// Directory to search in (default: current directory).
    #[serde(default = "default_path")]
    path: String,
    /// Maximum number of results to return (default: 1000).
    #[schemars(range(min = 1, max = 10_000))]
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

impl FindArgs {
    fn limit(&self) -> Result<usize, ToolError> {
        let limit = self.limit.unwrap_or(DEFAULT_LIMIT as u64);
        if limit == 0 || limit > MAX_LIMIT as u64 {
            return Err(ToolError::new(
                ToolErrorKind::InvalidArguments,
                format!("find: limit must be between 1 and {MAX_LIMIT}"),
            ));
        }
        Ok(limit as usize)
    }
}

fn text_block(text: String) -> Vec<ToolContent> {
    vec![ToolContent::Text { text }]
}

fn compile_glob(pattern: &str) -> Result<GlobMatcher, String> {
    GlobBuilder::new(pattern)
        .literal_separator(true)
        .build()
        .map(|glob| glob.compile_matcher())
        .map_err(|e| format!("find: invalid glob: {e}"))
}

fn relative_posix(relative: &Path) -> Option<String> {
    if relative.as_os_str().is_empty() {
        return None;
    }
    Some(
        relative
            .components()
            .map(|component| component.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/"),
    )
}

fn basename(path: &Path) -> Option<String> {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
}

fn sort_paths(paths: &mut [String]) {
    paths.sort_by(|a, b| {
        a.to_lowercase()
            .cmp(&b.to_lowercase())
            .then_with(|| a.cmp(b))
    });
}

async fn find_target(target: &FilesystemTarget, args: FindArgs) -> Result<ToolOutput, ToolError> {
    let limit = args.limit()?;
    let pattern = args.pattern;
    let path = args.path;
    let matcher = compile_glob(&pattern)
        .map_err(|error| ToolError::new(ToolErrorKind::InvalidArguments, error))?;
    let match_path = pattern.contains('/');
    let target_fingerprint = target.target_fingerprint();
    let walked = {
        let target = target.clone();
        tokio::task::spawn_blocking(move || walk_target(&target))
            .await
            .map_err(|error| {
                ToolError::new(
                    ToolErrorKind::Execution,
                    format!("find: blocking filesystem task failed: {error}"),
                )
            })?
            .map_err(|error| ToolError::new(ToolErrorKind::Execution, error.to_string()))?
    };
    let entries = match walked {
        CapWalkRoot::Directory(entries) => entries,
        CapWalkRoot::File(_) => {
            return Err(ToolError::new(
                ToolErrorKind::InvalidArguments,
                format!("find: not a directory: {}", target.display_path().display()),
            ));
        }
    };

    let mut matches = Vec::new();
    for entry in entries {
        let Some(relative) = relative_posix(&entry.relative) else {
            continue;
        };
        let target = if match_path {
            relative.clone()
        } else {
            basename(&entry.relative).unwrap_or_default()
        };
        if !matcher.is_match(&target) {
            continue;
        }
        matches.push(if entry.kind == CapWalkEntryKind::Directory {
            format!("{relative}/")
        } else {
            relative
        });
    }

    sort_paths(&mut matches);
    let total_matches = matches.len();
    if matches.is_empty() {
        return Ok(ToolOutput {
            content: text_block("No files found matching pattern".to_string()),
            details: Some(serde_json::json!({
                "path": path,
                "pattern": pattern,
                "target_fingerprint": target_fingerprint,
                "total_matches": 0,
                "listed_matches": 0,
                "truncated": false,
            })),
            terminate: false,
        });
    }

    let listed_matches = total_matches.min(limit);
    let result_limit_reached = total_matches > limit;
    let output = matches
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
    if result_limit_reached {
        let suggested = limit.saturating_mul(2).min(MAX_LIMIT);
        notices.push(if suggested > limit {
            format!(
                "{limit} results limit reached. Use limit={suggested} for more, or refine pattern"
            )
        } else {
            format!("Maximum {MAX_LIMIT} results reached. Refine pattern for a narrower result")
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
            "pattern": pattern,
            "target_fingerprint": target_fingerprint,
            "total_matches": total_matches,
            "listed_matches": listed_matches,
            "truncated": result_limit_reached || truncation.truncated,
        })),
        terminate: false,
    })
}

pub fn find_runtime_tool(
    filesystem: WorkspaceAccessHandle,
) -> Result<Arc<dyn DynamicTool>, tool_runtime::api::ToolRegistryError> {
    let definition = ToolDefinition {
        id: ToolId::new("find").expect("static tool id is valid"),
        kind: ToolKind::Function,
        description: DESCRIPTION.into(),
        parameters: schema_for::<FindArgs>().expect("FindArgs schema is valid"),
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
    Ok(Arc::new(TypedTool::<FindArgs>::new(
        definition,
        move |context, args| {
            let filesystem = filesystem.clone();
            Box::pin(async move {
                let target = filesystem_target_for_runtime_execution(
                    &filesystem,
                    &context,
                    "find",
                    &args.path,
                )
                .await
                .map_err(|error| ToolError::new(ToolErrorKind::Execution, error))?;
                find_target(&target, args).await
            }) as ToolFuture
        },
    )?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio_util::sync::CancellationToken;
    use tool_runtime::api::{ToolCallContext, ToolRegistry, ToolRuntime};

    fn runtime(filesystem: WorkspaceAccessHandle) -> ToolRuntime {
        let mut registry = ToolRegistry::default();
        registry
            .register(find_runtime_tool(filesystem).unwrap())
            .unwrap();
        ToolRuntime::new(registry).unwrap()
    }

    fn context() -> ToolCallContext {
        ToolCallContext::new(
            ToolId::new("find").unwrap(),
            "find-call",
            CancellationToken::new(),
        )
    }

    #[test]
    fn typed_find_definition_requires_pattern_and_bounds_limit() {
        let temp = tempfile::tempdir().expect("tempdir");
        let filesystem = WorkspaceAccessHandle::open_source(temp.path().to_path_buf()).unwrap();
        let definition = find_runtime_tool(filesystem).unwrap().definition().clone();
        assert_eq!(definition.id.as_str(), "find");
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
            definition.parameters["required"],
            serde_json::json!(["pattern"])
        );
        assert_eq!(
            definition.parameters["properties"]["limit"]["anyOf"][0]["maximum"],
            MAX_LIMIT
        );
    }

    #[tokio::test]
    async fn typed_find_walks_sorts_and_returns_revision_details() {
        let temp = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(temp.path().join("src/nested")).expect("create fixture dirs");
        std::fs::write(temp.path().join("src/z.rs"), "z").expect("write fixture");
        std::fs::write(temp.path().join("src/A.rs"), "a").expect("write fixture");
        std::fs::write(temp.path().join("src/nested/deep.rs"), "d").expect("write fixture");
        let filesystem = WorkspaceAccessHandle::open_source(temp.path().to_path_buf()).unwrap();
        let output = runtime(filesystem)
            .execute(
                context(),
                serde_json::json!({"pattern": "**/*.rs", "path": "src"}),
            )
            .await
            .expect("typed find succeeds");
        assert!(matches!(
            output.content.as_slice(),
            [ToolContent::Text { text }] if text == "A.rs\nnested/deep.rs\nz.rs"
        ));
        let details = output.details.expect("find details");
        assert_eq!(details["path"], "src");
        assert_eq!(details["pattern"], "**/*.rs");
        assert_eq!(details["total_matches"], 3);
        assert_eq!(details["listed_matches"], 3);
        assert_eq!(details["truncated"], false);
        assert!(
            details["target_fingerprint"]
                .as_str()
                .is_some_and(|value| value.len() == 64)
        );
    }

    #[tokio::test]
    async fn typed_find_rejects_invalid_pattern_and_arguments_structurally() {
        let temp = tempfile::tempdir().expect("tempdir");
        let filesystem = WorkspaceAccessHandle::open_source(temp.path().to_path_buf()).unwrap();
        let runtime = runtime(filesystem);
        let missing = runtime
            .execute(context(), serde_json::json!({}))
            .await
            .expect_err("pattern is required");
        assert_eq!(missing.kind, ToolErrorKind::InvalidArguments);
        let invalid = runtime
            .execute(context(), serde_json::json!({"pattern": "["}))
            .await
            .expect_err("invalid glob is rejected");
        assert_eq!(invalid.kind, ToolErrorKind::InvalidArguments);
    }
}
