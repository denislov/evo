use std::future::Future;
use std::sync::Arc;

use tool_contract::api::definition::{
    AuthorizationRisk, ToolBehaviorVersion, ToolCapabilities, ToolDefinition, ToolExecutionMode,
    ToolId, ToolKind,
};
use tool_contract::api::output::{ToolContent, ToolError, ToolErrorKind, ToolOutput};
use tool_runtime::api::{DynamicTool, FunctionTool, ToolFuture};

use super::{
    DelegationConfirmationMode, DelegationPolicy, DelegationTarget, DelegationTargetInventory,
    DelegationToolExecutor, ProfileId, handle_delegation_request,
};

pub(crate) fn delegation_tools(
    profile_id: Option<&ProfileId>,
    policy: Option<&DelegationPolicy>,
    inventory: &DelegationTargetInventory,
    executor: Option<DelegationToolExecutor>,
) -> Vec<Arc<dyn DynamicTool>> {
    let Some(policy) = policy else {
        return Vec::new();
    };
    let profile_id = profile_id
        .cloned()
        .unwrap_or_else(|| ProfileId::from("default"));
    let mut tools = Vec::new();
    if policy.allow_delegate_agent && !inventory.agents.is_empty() {
        tools.push(delegate_agent_tool(
            profile_id.clone(),
            policy.clone(),
            &inventory.agents,
            executor.clone(),
        ));
    }
    if policy.allow_delegate_team && !inventory.teams.is_empty() {
        tools.push(delegate_team_tool(
            profile_id,
            policy.clone(),
            &inventory.teams,
            executor,
        ));
    }
    tools
}

fn delegate_agent_tool(
    profile_id: ProfileId,
    policy: DelegationPolicy,
    targets: &[DelegationTarget],
    executor: Option<DelegationToolExecutor>,
) -> Arc<dyn DynamicTool> {
    let requires_confirmation = matches!(
        policy.require_confirmation,
        DelegationConfirmationMode::Always
    );
    delegation_tool(
        "delegate_agent",
        delegation_description(
            "Request bounded help from another configured agent profile. The session owner validates policy before any child work is allowed.",
            "agent",
            targets,
        ),
        delegation_parameters("agent_id", "agent", targets),
        move |context, args| {
            let profile_id = profile_id.clone();
            let policy = policy.clone();
            let executor = executor.clone();
            async move {
                match executor {
                    Some(executor) => executor(context, args).await,
                    None => {
                        handle_delegation_request("agent", "agent_id", &profile_id, &policy, args)
                    }
                }
            }
        },
        if requires_confirmation {
            AuthorizationRisk::SideEffect
        } else {
            AuthorizationRisk::None
        },
    )
}

fn delegate_team_tool(
    profile_id: ProfileId,
    policy: DelegationPolicy,
    targets: &[DelegationTarget],
    executor: Option<DelegationToolExecutor>,
) -> Arc<dyn DynamicTool> {
    let requires_confirmation = !matches!(
        policy.require_confirmation,
        DelegationConfirmationMode::Never
    );
    delegation_tool(
        "delegate_team",
        delegation_description(
            "Request bounded help from a configured team profile. The session owner validates policy before any child work is allowed.",
            "team",
            targets,
        ),
        delegation_parameters("team_id", "team", targets),
        move |context, args| {
            let profile_id = profile_id.clone();
            let policy = policy.clone();
            let executor = executor.clone();
            async move {
                match executor {
                    Some(executor) => executor(context, args).await,
                    None => {
                        handle_delegation_request("team", "team_id", &profile_id, &policy, args)
                    }
                }
            }
        },
        if requires_confirmation {
            AuthorizationRisk::SideEffect
        } else {
            AuthorizationRisk::None
        },
    )
}

fn delegation_tool<F, Fut>(
    id: &str,
    description: String,
    parameters: serde_json::Value,
    executor: F,
    authorization_risk: AuthorizationRisk,
) -> Arc<dyn DynamicTool>
where
    F: Fn(tool_runtime::api::ToolCallContext, serde_json::Value) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<String, String>> + Send + 'static,
{
    let definition = ToolDefinition {
        id: ToolId::new(id).expect("static tool id is valid"),
        kind: ToolKind::Function,
        description,
        parameters,
        capabilities: ToolCapabilities {
            read_only: false,
            execution: ToolExecutionMode::Parallel,
            cancel: true,
            timeout: true,
            streaming: false,
            provider_executed: false,
        },
        behavior: ToolBehaviorVersion::V1,
        authorization_risk,
        requirements: Vec::new(),
    };
    FunctionTool::new(definition, move |context, arguments| {
        let future = executor(context, arguments);
        Box::pin(async move {
            future
                .await
                .map(|text| ToolOutput {
                    content: vec![ToolContent::Text { text }],
                    details: None,
                    terminate: false,
                })
                .map_err(|message| ToolError::new(ToolErrorKind::Execution, message))
        }) as ToolFuture
    })
}

fn delegation_description(base: &str, kind: &str, targets: &[DelegationTarget]) -> String {
    let inventory = targets
        .iter()
        .map(|target| {
            let display_name = single_line(&target.display_name);
            match target.description.as_deref().map(single_line) {
                Some(description) if !description.is_empty() => {
                    format!("- {}: {} - {}", target.id, display_name, description)
                }
                _ => format!("- {}: {}", target.id, display_name),
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    format!("{base}\n\nAvailable {kind} profiles:\n{inventory}")
}

fn single_line(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn delegation_parameters(
    target_field: &str,
    target_kind: &str,
    targets: &[DelegationTarget],
) -> serde_json::Value {
    let target_ids = targets
        .iter()
        .map(|target| target.id.as_str())
        .collect::<Vec<_>>();
    let mut properties = serde_json::Map::new();
    properties.insert(
        target_field.to_string(),
        serde_json::json!({
            "type": "string",
            "description": format!("Configured {target_kind} profile id"),
            "enum": target_ids
        }),
    );
    properties.insert(
        "task".to_string(),
        serde_json::json!({
            "type": "string",
            "description": "Focused task for the delegated child operation"
        }),
    );
    serde_json::json!({
        "type": "object",
        "properties": properties,
        "required": [target_field, "task"],
        "additionalProperties": false
    })
}
