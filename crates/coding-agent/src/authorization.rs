use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// Interactive permission policy for risky tool invocations.
///
/// Three product modes, mirroring the Plan / Ask / Yolo permission ladder:
/// `Plan` is read-only (mutating actions are denied without prompting),
/// `Ask` prompts before every risky action, and `Yolo` auto-approves
/// everything.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolAuthorizationMode {
    /// Read-only planning: read-only tools (reads, greps, listings) run
    /// automatically; mutating, shell and side-effecting tools are denied
    /// without prompting.
    Plan,
    /// Prompt before every risky action.
    #[default]
    Ask,
    /// Auto-approve every action; nothing is ever blocked or prompted.
    Yolo,
}

impl fmt::Display for ToolAuthorizationMode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Plan => formatter.write_str("plan"),
            Self::Ask => formatter.write_str("ask"),
            Self::Yolo => formatter.write_str("yolo"),
        }
    }
}

impl FromStr for ToolAuthorizationMode {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "plan" => Ok(Self::Plan),
            "ask" => Ok(Self::Ask),
            "yolo" => Ok(Self::Yolo),
            other => Err(format!(
                "unknown permission mode `{other}` (expected plan, ask, or yolo)"
            )),
        }
    }
}

#[cfg(test)]
mod mode_tests {
    use super::ToolAuthorizationMode;

    #[test]
    fn default_mode_is_ask() {
        assert_eq!(ToolAuthorizationMode::default(), ToolAuthorizationMode::Ask);
    }

    #[test]
    fn modes_round_trip_through_display_and_parse() {
        for mode in [ToolAuthorizationMode::Plan, ToolAuthorizationMode::Ask, ToolAuthorizationMode::Yolo] {
            let text = mode.to_string();
            assert_eq!(text.parse::<ToolAuthorizationMode>().unwrap(), mode, "{text}");
        }
    }

    #[test]
    fn modes_serialize_as_snake_case_and_parse_back() {
        let yolo = serde_json::to_string(&ToolAuthorizationMode::Yolo).unwrap();
        assert_eq!(yolo, r#""yolo""#);
        let parsed: ToolAuthorizationMode = serde_json::from_str(r#""plan""#).unwrap();
        assert_eq!(parsed, ToolAuthorizationMode::Plan);
    }

    #[test]
    fn unknown_mode_errors_with_guidance() {
        let error = "auto".parse::<ToolAuthorizationMode>().unwrap_err();
        assert!(error.contains("plan, ask, or yolo"), "{error}");
    }
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
