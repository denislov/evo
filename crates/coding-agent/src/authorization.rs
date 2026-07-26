use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ToolAuthorizationMode {
    #[default]
    Deny,
    Interactive,
    AllowAll,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolAuthorizationRisk {
    ExternalRead,
    FilesystemMutation,
    ShellExecution,
    DeclaredSideEffect,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum ToolAuthorizationScope {
    Path {
        path: String,
    },
    FilesystemTarget {
        path: String,
        target_fingerprint: String,
    },
    Shell {
        cwd: String,
        command_fingerprint: String,
    },
    ToolArguments {
        fingerprint: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolAuthorizationPreview {
    pub summary: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_preview: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolAuthorizationRequest {
    pub authorization_id: String,
    pub operation_id: String,
    pub turn_id: String,
    pub tool_call_id: String,
    pub tool_name: String,
    pub risk: ToolAuthorizationRisk,
    pub scope: ToolAuthorizationScope,
    pub preview: ToolAuthorizationPreview,
    pub capability_generation: u64,
    pub requested_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolAuthorizationIdentity {
    pub authorization_id: String,
    pub operation_id: String,
    pub turn_id: String,
    pub tool_call_id: String,
    pub capability_generation: u64,
}

impl ToolAuthorizationRequest {
    pub fn identity(&self) -> ToolAuthorizationIdentity {
        ToolAuthorizationIdentity {
            authorization_id: self.authorization_id.clone(),
            operation_id: self.operation_id.clone(),
            turn_id: self.turn_id.clone(),
            tool_call_id: self.tool_call_id.clone(),
            capability_generation: self.capability_generation,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum ToolAuthorizationDecision {
    AllowOnce,
    AllowForOperation,
    Deny {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
}
