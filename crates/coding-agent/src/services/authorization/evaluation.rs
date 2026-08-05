use super::*;
use tool_contract::api::definition::ToolId;

pub(super) enum Evaluation {
    Allow,
    Ask {
        risk: ToolAuthorizationRisk,
        scope: ToolAuthorizationScope,
        preview: ToolAuthorizationPreview,
    },
}

impl Evaluation {
    pub(super) fn bind_filesystem_descriptor(&mut self, descriptor: &FilesystemBindingDescriptor) {
        let Self::Ask { scope, preview, .. } = self else {
            return;
        };
        let path = descriptor.display.to_string_lossy().into_owned();
        *scope = ToolAuthorizationScope::FilesystemTarget {
            path: path.clone(),
            target_fingerprint: descriptor.target_fingerprint.clone(),
        };
        preview.path = Some(path);
    }
}

pub(super) async fn bind_filesystem_target(
    context: &BeforeToolCallContext,
    snapshot: &OperationCapabilitySnapshot,
) -> Result<Option<FilesystemBindingDescriptor>, String> {
    let path = match context.tool_name.as_str() {
        "read" | "grep" | "find" | "ls" => context
            .arguments
            .get("path")
            .and_then(Value::as_str)
            .unwrap_or("."),
        "write" | "edit" | "hashline_edit" => context
            .arguments
            .get("path")
            .and_then(Value::as_str)
            .ok_or_else(|| "filesystem mutation is missing `path`".to_owned())?,
        _ => return Ok(None),
    };
    let filesystem = snapshot
        .workspace
        .as_ref()
        .ok_or_else(|| "filesystem capability is not granted".to_owned())?;
    filesystem
        .bind_tool_target(
            &snapshot.operation_id,
            &context.tool_call_id,
            &context.tool_name,
            path,
        )
        .await
        .map(Some)
        .map_err(|error| error.to_string())
}

pub(super) fn discard_filesystem_binding(
    context: &BeforeToolCallContext,
    snapshot: &OperationCapabilitySnapshot,
) {
    if let Some(filesystem) = snapshot.workspace.as_ref() {
        filesystem.discard_bound_tool_target(&snapshot.operation_id, &context.tool_call_id);
    }
}

pub(super) fn evaluate(
    context: &BeforeToolCallContext,
    snapshot: &OperationCapabilitySnapshot,
    inventory: &ToolAuthorizationInventory,
) -> Result<Evaluation, String> {
    let tool_id = ToolId::new(context.tool_name.clone()).ok();
    match context.tool_name.as_str() {
        "read" | "grep" | "find" | "ls" => {
            let Some(filesystem) = snapshot.workspace.as_ref() else {
                return Err("filesystem capability is not granted".into());
            };
            let path = context
                .arguments
                .get("path")
                .and_then(Value::as_str)
                .unwrap_or(".");
            let preview = filesystem
                .preview_path(path)
                .map_err(|error| error.to_string())?;
            if preview.workspace_local {
                Ok(Evaluation::Allow)
            } else {
                Ok(path_request(
                    ToolAuthorizationRisk::ExternalRead,
                    preview.display,
                    "Read outside the workspace",
                ))
            }
        }
        "write" | "edit" | "hashline_edit" => {
            let Some(filesystem) = snapshot.workspace.as_ref() else {
                return Err("filesystem capability is not granted".into());
            };
            let path = context
                .arguments
                .get("path")
                .and_then(Value::as_str)
                .ok_or_else(|| "filesystem mutation is missing `path`".to_owned())?;
            let target = filesystem
                .preview_path(path)
                .map_err(|error| error.to_string())?;
            Ok(path_request_with_content(
                ToolAuthorizationRisk::FilesystemMutation,
                target.display,
                "Modify a file",
                mutation_content_preview(context),
            ))
        }
        "apply_patch" => {
            let Some(filesystem) = snapshot.workspace.as_ref() else {
                return Err("filesystem capability is not granted".into());
            };
            let patch = context
                .arguments
                .get("patch")
                .and_then(Value::as_str)
                .ok_or_else(|| "apply_patch is missing `patch`".to_owned())?;
            let parsed = crate::tools::filesystem::patch::parse_patch(patch)
                .map_err(|error| format!("invalid apply_patch input: {error}"))?;
            let mut previews = parsed
                .files
                .iter()
                .map(|file| filesystem.preview_path(&file.path))
                .collect::<Result<Vec<_>, _>>()
                .map_err(|error| error.to_string())?;
            if previews.iter().any(|preview| !preview.workspace_local) {
                return Err("apply_patch only accepts workspace-local paths; use an explicit write operation for external targets".into());
            }
            let preview = previews
                .pop()
                .ok_or_else(|| "apply_patch must contain at least one file".to_owned())?;
            Ok(path_request_with_content(
                ToolAuthorizationRisk::FilesystemMutation,
                preview.display,
                "Apply a workspace patch",
                mutation_content_preview(context),
            ))
        }
        "bash" => {
            let Some(shell) = snapshot.workspace.as_ref() else {
                return Err("shell capability is not granted".into());
            };
            let command = context
                .arguments
                .get("command")
                .and_then(Value::as_str)
                .ok_or_else(|| "shell invocation is missing `command`".to_owned())?;
            let redacted = redact_command(command);
            Ok(Evaluation::Ask {
                risk: ToolAuthorizationRisk::ShellExecution,
                scope: ToolAuthorizationScope::Shell {
                    cwd: shell.cwd().to_string_lossy().into_owned(),
                    command_fingerprint: fingerprint(command.as_bytes()),
                },
                preview: ToolAuthorizationPreview {
                    summary: "Execute a shell command".into(),
                    path: None,
                    command: Some(redacted),
                    cwd: Some(shell.cwd().to_string_lossy().into_owned()),
                    content_preview: None,
                },
            })
        }
        "delegate_agent" | "delegate_team" => {
            match tool_id
                .as_ref()
                .and_then(|id| inventory.explicit_tools.get(id))
                .copied()
                .flatten()
            {
                Some(DeclaredToolAuthorizationRisk::SideEffect) => Ok(argument_request(
                    context,
                    ToolAuthorizationRisk::DeclaredSideEffect,
                    "Delegate work to a child agent",
                )),
                _ => Ok(Evaluation::Allow),
            }
        }
        _ if tool_id
            .as_ref()
            .is_some_and(|id| inventory.explicit_tools.contains_key(id)) =>
        {
            match tool_id
                .as_ref()
                .and_then(|id| inventory.explicit_tools.get(id))
                .copied()
                .flatten()
            {
                Some(DeclaredToolAuthorizationRisk::WorkspaceLocalReadOnly) => {
                    Ok(Evaluation::Allow)
                }
                Some(DeclaredToolAuthorizationRisk::SideEffect) | None => Ok(argument_request(
                    context,
                    ToolAuthorizationRisk::DeclaredSideEffect,
                    "Run a custom tool",
                )),
            }
        }
        _ => Ok(argument_request(
            context,
            ToolAuthorizationRisk::Unknown,
            "Run a tool without risk metadata",
        )),
    }
}

fn path_request(risk: ToolAuthorizationRisk, path: PathBuf, summary: &str) -> Evaluation {
    path_request_with_content(risk, path, summary, None)
}

fn path_request_with_content(
    risk: ToolAuthorizationRisk,
    path: PathBuf,
    summary: &str,
    content_preview: Option<String>,
) -> Evaluation {
    let path = path.to_string_lossy().into_owned();
    Evaluation::Ask {
        risk,
        scope: ToolAuthorizationScope::Path { path: path.clone() },
        preview: ToolAuthorizationPreview {
            summary: summary.into(),
            path: Some(path),
            command: None,
            cwd: None,
            content_preview,
        },
    }
}

fn argument_request(
    context: &BeforeToolCallContext,
    risk: ToolAuthorizationRisk,
    summary: &str,
) -> Evaluation {
    Evaluation::Ask {
        risk,
        scope: ToolAuthorizationScope::ToolArguments {
            fingerprint: argument_fingerprint(&context.arguments),
        },
        preview: ToolAuthorizationPreview {
            summary: format!("{summary}: {}", context.tool_name),
            path: None,
            command: None,
            cwd: None,
            content_preview: None,
        },
    }
}

pub(super) fn blocked(reason: impl Into<String>) -> BeforeToolCallResult {
    BeforeToolCallResult {
        block: true,
        reason: Some(reason.into()),
    }
}

pub(super) fn delegation_request(
    context: &BeforeToolCallContext,
    turn_id: &str,
    snapshot: &OperationCapabilitySnapshot,
) -> Option<DelegationRequest> {
    let (target_kind, target_field) = match context.tool_name.as_str() {
        "delegate_agent" => (ProfileKind::Agent, "agent_id"),
        "delegate_team" => (ProfileKind::Team, "team_id"),
        _ => return None,
    };
    let operation_id = context.execution_context.scope_id()?.to_owned();
    let requesting_profile_id = snapshot.model.as_ref()?.profile_id.clone()?;
    let target_id =
        ProfileId::new(context.arguments.get(target_field)?.as_str()?.to_owned()).ok()?;
    let task = context.arguments.get("task")?.as_str()?.trim().to_owned();
    if task.is_empty() {
        return None;
    }
    Some(DelegationRequest {
        operation_id,
        turn_id: turn_id.to_owned(),
        tool_call_id: context.tool_call_id.clone(),
        requesting_profile_id,
        target_kind,
        target_id,
        task,
    })
}

pub(super) fn delegation_rejected_result(request: &DelegationRequest, reason: &str) -> String {
    let mut result =
        DelegationToolResult::from_request(request, DelegationToolResultStatus::Rejected);
    result.error = Some(reason.to_owned());
    result.to_json()
}

fn argument_fingerprint(arguments: &Value) -> String {
    fingerprint(canonical_json(arguments).as_bytes())
}

fn fingerprint(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn canonical_json(value: &Value) -> String {
    match value {
        Value::Object(values) => {
            let mut fields = values.iter().collect::<Vec<_>>();
            fields.sort_by_key(|(name, _)| *name);
            let fields = fields
                .into_iter()
                .map(|(key, value)| {
                    format!(
                        "{}:{}",
                        serde_json::to_string(key).unwrap(),
                        canonical_json(value)
                    )
                })
                .collect::<Vec<_>>()
                .join(",");
            format!("{{{fields}}}")
        }
        Value::Array(values) => format!(
            "[{}]",
            values
                .iter()
                .map(canonical_json)
                .collect::<Vec<_>>()
                .join(",")
        ),
        _ => serde_json::to_string(value).unwrap(),
    }
}

fn redact_command(command: &str) -> String {
    crate::platform::io::redaction::redact_sensitive_text(command)
}

fn mutation_content_preview(context: &BeforeToolCallContext) -> Option<String> {
    let raw = if context.tool_name == "write" {
        context.arguments.get("content")?.as_str()?.to_owned()
    } else {
        context
            .arguments
            .get("edits")?
            .as_array()?
            .iter()
            .take(4)
            .flat_map(|edit| {
                let old = edit.get("oldText").and_then(Value::as_str).unwrap_or("");
                let new = edit.get("newText").and_then(Value::as_str).unwrap_or("");
                old.lines()
                    .take(3)
                    .map(|line| format!("- {line}"))
                    .chain(new.lines().take(3).map(|line| format!("+ {line}")))
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    let bounded = raw.lines().take(12).collect::<Vec<_>>().join("\n");
    let bounded = bounded.chars().take(1_200).collect::<String>();
    (!bounded.is_empty()).then(|| crate::platform::io::redaction::redact_sensitive_text(&bounded))
}
