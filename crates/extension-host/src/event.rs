//! 版本化 extension event DTO。
//!
//! [`ExtensionEvent`] 是产品与扩展之间的 wire 契约，与内部 `ProductEvent`
//! 完全隔离：
//!
//! - 信封带 `version` 字段（[`EXTENSION_EVENT_VERSION`]），缺少 `version`
//!   的旧输入按 1 处理（`#[serde(default)]` 向后兼容）。
//! - payload 是 untagged 变体，字段集互斥（无子集关系），保证反序列化
//!   判别唯一；字段缺失用 `#[serde(default)]` 容错。
//! - 事件 kind 带别名解析（PascalCase / camelCase / snake_case），未知
//!   kind 反序列化失败（fail closed）。
//! - tool 相关 payload 复用 `tool-contract` 的 [`ToolId`]，与产品共享同一
//!   Tool contract（Phase 7 Gate）。
//!
//! ARC-710 扩展事件业务字段时：新增可选字段用 `#[serde(default)]`；新增
//! payload 变体必须保证字段集与既有变体互斥。

// Adapted from xai-grok-hooks, SOURCE_REV d6937fe255dce4133c3d000a50f9cb94de12f06f;
// rewritten for Evo semantics (not a verbatim copy).
use serde::{Deserialize, Serialize};
use tool_contract::api::definition::ToolId;

/// 当前 wire 协议版本。输入 `version` 大于此值会被 host 拒绝
/// （fail closed）；缺少 `version` 按 1 读取。
pub const EXTENSION_EVENT_VERSION: u32 = 1;

fn default_event_version() -> u32 {
    EXTENSION_EVENT_VERSION
}

/// 扩展事件类型。`Deserialize` 接受 PascalCase / camelCase / snake_case
/// 拼写；未知拼写是错误（fail closed，避免扩展输入静默丢事件）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExtensionEventKind {
    SessionStart,
    UserPromptSubmit,
    PreToolUse,
    PostToolUse,
    PermissionDenied,
    Stop,
    SubagentStart,
    SubagentStop,
    PreCompact,
    PostCompact,
    MergeProposed,
    MergeApplied,
    SessionEnd,
}

impl ExtensionEventKind {
    /// 别名解析入口（与 `Deserialize` 共享单一来源）。
    pub fn try_parse(spelling: &str) -> Option<Self> {
        Self::from_key(spelling)
    }

    /// 已知拼写到事件类型的映射（别名表，单一来源）。
    fn from_key(s: &str) -> Option<Self> {
        Some(match s {
            "SessionStart" | "session_start" | "sessionStart" => Self::SessionStart,
            "UserPromptSubmit" | "user_prompt_submit" | "userPromptSubmit" => {
                Self::UserPromptSubmit
            }
            "PreToolUse" | "pre_tool_use" | "preToolUse" | "beforeShellExecution" => {
                Self::PreToolUse
            }
            "PostToolUse" | "post_tool_use" | "postToolUse" | "afterShellExecution" => {
                Self::PostToolUse
            }
            "PermissionDenied" | "permission_denied" | "permissionDenied" => Self::PermissionDenied,
            "Stop" | "stop" => Self::Stop,
            "SubagentStart" | "subagent_start" | "subagentStart" => Self::SubagentStart,
            "SubagentStop" | "subagent_stop" | "subagentStop" => Self::SubagentStop,
            "PreCompact" | "pre_compact" | "preCompact" => Self::PreCompact,
            "PostCompact" | "post_compact" | "postCompact" => Self::PostCompact,
            "MergeProposed" | "merge_proposed" | "mergeProposed" => Self::MergeProposed,
            "MergeApplied" | "merge_applied" | "mergeApplied" => Self::MergeApplied,
            "SessionEnd" | "session_end" | "sessionEnd" => Self::SessionEnd,
            _ => return None,
        })
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::SessionStart => "session_start",
            Self::UserPromptSubmit => "user_prompt_submit",
            Self::PreToolUse => "pre_tool_use",
            Self::PostToolUse => "post_tool_use",
            Self::PermissionDenied => "permission_denied",
            Self::Stop => "stop",
            Self::SubagentStart => "subagent_start",
            Self::SubagentStop => "subagent_stop",
            Self::PreCompact => "pre_compact",
            Self::PostCompact => "post_compact",
            Self::MergeProposed => "merge_proposed",
            Self::MergeApplied => "merge_applied",
            Self::SessionEnd => "session_end",
        }
    }
}

impl std::fmt::Display for ExtensionEventKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for ExtensionEventKind {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        Self::from_key(&s).ok_or_else(|| {
            serde::de::Error::custom(format!(
                "unknown extension event '{s}'; expected one of: session_start, \
                 user_prompt_submit, pre_tool_use, post_tool_use, permission_denied, \
                 stop, subagent_start, subagent_stop, pre_compact, post_compact, \
                 merge_proposed, merge_applied, session_end"
            ))
        })
    }
}

/// SubagentStop 的触发阶段。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SubagentStopPhase {
    Gate,
    Observe,
}

/// 事件 payload：internally-tagged 变体，`kind` 字段与
/// [`ExtensionEventKind`] 的 snake_case 一致（判别唯一、无歧义）。
///
/// 向后兼容约定：payload 内的可选字段缺失走 `#[serde(default)]`；新增
/// 变体必须给 `kind` 新值，不破坏既有变体的反序列化。
///
/// 骨架阶段只承载每类事件的最小业务字段；ARC-710 在此扩展。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ExtensionEventPayload {
    SessionStart {
        source: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        agent_type: Option<String>,
    },
    UserPromptSubmit {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        prompt: Option<String>,
    },
    PreToolUse {
        #[serde(rename = "toolName")]
        tool_name: ToolId,
        #[serde(rename = "toolInput")]
        tool_input: serde_json::Value,
        #[serde(rename = "toolInputTruncated", default)]
        tool_input_truncated: bool,
        /// 工具操作的目标路径（matcher 的 path 条件数据源）；无明确路径
        /// 时缺省。
        #[serde(rename = "path", default, skip_serializing_if = "Option::is_none")]
        path: Option<String>,
    },
    PostToolUse {
        #[serde(rename = "toolName")]
        tool_name: ToolId,
        #[serde(rename = "toolInput")]
        tool_input: serde_json::Value,
        #[serde(rename = "toolResult")]
        tool_result: serde_json::Value,
        #[serde(rename = "toolInputTruncated", default)]
        tool_input_truncated: bool,
        #[serde(rename = "toolResultTruncated", default)]
        tool_result_truncated: bool,
        #[serde(
            rename = "durationMs",
            default,
            skip_serializing_if = "Option::is_none"
        )]
        duration_ms: Option<u64>,
        #[serde(rename = "path", default, skip_serializing_if = "Option::is_none")]
        path: Option<String>,
    },
    PermissionDenied {
        #[serde(rename = "toolName")]
        tool_name: ToolId,
        reason: String,
        #[serde(rename = "path", default, skip_serializing_if = "Option::is_none")]
        path: Option<String>,
    },
    Stop {
        reason: String,
        #[serde(
            rename = "lastAssistantMessage",
            default,
            skip_serializing_if = "Option::is_none"
        )]
        last_assistant_message: Option<String>,
    },
    SubagentStart {
        #[serde(rename = "subagentType")]
        subagent_type: String,
    },
    SubagentStop {
        #[serde(rename = "subagentType")]
        subagent_type: String,
        phase: SubagentStopPhase,
        #[serde(
            rename = "stopReason",
            default,
            skip_serializing_if = "Option::is_none"
        )]
        stop_reason: Option<String>,
    },
    PreCompact {
        source: String,
        #[serde(rename = "entriesRemoved")]
        entries_removed: u32,
    },
    PostCompact {
        source: String,
        #[serde(rename = "entriesRemoved")]
        entries_removed: u32,
        /// compact 完成后 session 是否已恢复执行。
        resumed: bool,
    },
    /// 合并提案提交给工作区时触发（Observe gate）。
    MergeProposed {
        #[serde(rename = "proposalId")]
        proposal_id: String,
        #[serde(rename = "childWorktree")]
        child_worktree: String,
    },
    /// 合并已应用时触发（Observe gate）。
    MergeApplied {
        #[serde(rename = "proposalId")]
        proposal_id: String,
        #[serde(rename = "appliedEntries")]
        applied_entries: u32,
    },
    SessionEnd {
        reason: String,
        #[serde(rename = "turnCount", default, skip_serializing_if = "Option::is_none")]
        turn_count: Option<u64>,
    },
}

/// 版本化事件信封：公共元数据 + 独立 payload 对象（`kind` tag 判别）。
///
/// 序列化为 camelCase；反序列化对旧输入向后兼容（缺省字段走默认值）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtensionEvent {
    /// wire 协议版本；缺省按 [`EXTENSION_EVENT_VERSION`] 处理。
    #[serde(default = "default_event_version")]
    pub version: u32,
    #[serde(rename = "event")]
    pub kind: ExtensionEventKind,
    pub session_id: String,
    pub workspace_root: String,
    pub timestamp: String,
    /// 产生该事件的扩展 id（`None` 表示产品内建来源）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extension_id: Option<String>,
    pub payload: ExtensionEventPayload,
}

impl ExtensionEvent {
    pub fn new(
        kind: ExtensionEventKind,
        session_id: impl Into<String>,
        workspace_root: impl Into<String>,
        timestamp: impl Into<String>,
        payload: ExtensionEventPayload,
    ) -> Self {
        Self {
            version: EXTENSION_EVENT_VERSION,
            kind,
            session_id: session_id.into(),
            workspace_root: workspace_root.into(),
            timestamp: timestamp.into(),
            extension_id: None,
            payload,
        }
    }

    /// 校验 wire 版本；高于支持的版本失败（fail closed）。
    pub fn validate_version(&self) -> Result<(), crate::ExtensionError> {
        if self.version == EXTENSION_EVENT_VERSION {
            Ok(())
        } else {
            Err(crate::ExtensionError::UnsupportedVersion {
                version: self.version,
                supported: EXTENSION_EVENT_VERSION,
            })
        }
    }
}

/// 单个 `toolInput` / `toolResult` 序列化后的最大字节数（128 KB）。
///
/// hook 事件经 runner 的环境变量通道注入子进程，env 总量受
/// `ARG_MAX` 约束；超限的 JSON 值被截断为字符串并标记，与
/// xai-grok-hooks 的 `MAX_PAYLOAD_SIZE` 语义一致。
pub const MAX_HOOK_PAYLOAD_BYTES: usize = 128 * 1024;

/// 截断 JSON 值到 [`MAX_HOOK_PAYLOAD_BYTES`]，返回 `(值, 是否截断)`。
///
/// 截断在字符边界上进行（不劈开多字节码点），结果用字符串承载并追加
/// ` [truncated]` 标记。
pub fn truncate_json_payload(value: serde_json::Value) -> (serde_json::Value, bool) {
    let serialized = serde_json::to_string(&value).unwrap_or_default();
    if serialized.len() <= MAX_HOOK_PAYLOAD_BYTES {
        return (value, false);
    }
    let mut end = MAX_HOOK_PAYLOAD_BYTES;
    while !serialized.is_char_boundary(end) {
        end -= 1;
    }
    let mut truncated = serialized[..end].to_string();
    truncated.push_str(" [truncated]");
    (serde_json::Value::String(truncated), true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn tool(name: &str) -> ToolId {
        ToolId::new(name).unwrap()
    }

    fn base_event() -> ExtensionEvent {
        ExtensionEvent::new(
            ExtensionEventKind::PreToolUse,
            "s1",
            "/ws",
            "2026-08-06T00:00:00Z",
            ExtensionEventPayload::PreToolUse {
                tool_name: tool("read_file"),
                tool_input: json!({"path": "a.txt"}),
                tool_input_truncated: false,
                path: None,
            },
        )
    }

    #[test]
    fn golden_serialization_matches_camel_case_json() {
        let event = base_event();
        let value = serde_json::to_value(&event).unwrap();
        let golden = json!({
            "version": 1,
            "event": "pre_tool_use",
            "sessionId": "s1",
            "workspaceRoot": "/ws",
            "timestamp": "2026-08-06T00:00:00Z",
            "payload": {
                "kind": "pre_tool_use",
                "toolName": "read_file",
                "toolInput": {"path": "a.txt"},
                "toolInputTruncated": false,
            }
        });
        assert_eq!(value, golden);
    }

    #[test]
    fn golden_round_trip() {
        let event = base_event();
        let value = serde_json::to_value(&event).unwrap();
        let back: ExtensionEvent = serde_json::from_value(value).unwrap();
        assert_eq!(back, event);
    }

    #[test]
    fn every_payload_variant_round_trips() {
        let cases = [
            ExtensionEventPayload::SessionStart {
                source: "new".into(),
                model_id: Some("doubao-1".into()),
                agent_type: None,
            },
            ExtensionEventPayload::UserPromptSubmit {
                prompt: Some("fix the build".into()),
            },
            ExtensionEventPayload::PreToolUse {
                tool_name: tool("read_file"),
                tool_input: json!({"path": "/x"}),
                tool_input_truncated: false,
                path: Some("src/main.rs".into()),
            },
            ExtensionEventPayload::PostToolUse {
                tool_name: tool("read_file"),
                tool_input: json!({"path": "/x"}),
                tool_result: json!({"ok": true}),
                tool_input_truncated: false,
                tool_result_truncated: true,
                duration_ms: Some(12),
                path: None,
            },
            ExtensionEventPayload::PermissionDenied {
                tool_name: tool("bash"),
                reason: "policy denies network".into(),
                path: None,
            },
            ExtensionEventPayload::Stop {
                reason: "end_turn".into(),
                last_assistant_message: Some("done".into()),
            },
            ExtensionEventPayload::SubagentStart {
                subagent_type: "explore".into(),
            },
            ExtensionEventPayload::SubagentStop {
                subagent_type: "explore".into(),
                phase: SubagentStopPhase::Gate,
                stop_reason: None,
            },
            ExtensionEventPayload::PreCompact {
                source: "auto".into(),
                entries_removed: 42,
            },
            ExtensionEventPayload::PostCompact {
                source: "manual".into(),
                entries_removed: 7,
                resumed: true,
            },
            ExtensionEventPayload::MergeProposed {
                proposal_id: "p-1".into(),
                child_worktree: "wt-1".into(),
            },
            ExtensionEventPayload::MergeApplied {
                proposal_id: "p-1".into(),
                applied_entries: 42,
            },
            ExtensionEventPayload::SessionEnd {
                reason: "user_stop".into(),
                turn_count: Some(3),
            },
        ];
        for payload in cases {
            let event =
                ExtensionEvent::new(ExtensionEventKind::SessionStart, "s", "/w", "t", payload);
            let value = serde_json::to_value(&event).unwrap();
            let back: ExtensionEvent = serde_json::from_value(value).unwrap();
            assert_eq!(back, event);
        }
    }

    #[test]
    fn old_input_without_version_and_extension_id_still_loads() {
        let old = r#"{
            "event": "session_end",
            "sessionId": "s1",
            "workspaceRoot": "/ws",
            "timestamp": "2026-08-06T00:00:00Z",
            "payload": {
                "kind": "session_end",
                "reason": "user_stop",
                "turnCount": 5
            }
        }"#;
        let event: ExtensionEvent = serde_json::from_str(old).unwrap();
        assert_eq!(
            event.version, EXTENSION_EVENT_VERSION,
            "missing version defaults to 1"
        );
        assert_eq!(event.extension_id, None);
        assert_eq!(
            event.payload,
            ExtensionEventPayload::SessionEnd {
                reason: "user_stop".into(),
                turn_count: Some(5),
            }
        );
    }

    #[test]
    fn new_optional_fields_default_when_absent() {
        let minimal = r#"{
            "event": "pre_tool_use",
            "sessionId": "s",
            "workspaceRoot": "/w",
            "timestamp": "t",
            "payload": {
                "kind": "pre_tool_use",
                "toolName": "read_file",
                "toolInput": {}
            }
        }"#;
        let event: ExtensionEvent = serde_json::from_str(minimal).unwrap();
        assert_eq!(
            event.payload,
            ExtensionEventPayload::PreToolUse {
                tool_name: tool("read_file"),
                tool_input: json!({}),
                tool_input_truncated: false,
                path: None,
            }
        );
    }

    #[test]
    fn event_kind_accepts_aliases_and_rejects_unknown() {
        for (spelling, expected) in [
            ("SessionStart", ExtensionEventKind::SessionStart),
            ("session_start", ExtensionEventKind::SessionStart),
            ("sessionStart", ExtensionEventKind::SessionStart),
            ("PreToolUse", ExtensionEventKind::PreToolUse),
            ("beforeShellExecution", ExtensionEventKind::PreToolUse),
            ("stop", ExtensionEventKind::Stop),
            ("PostCompact", ExtensionEventKind::PostCompact),
            ("merge_proposed", ExtensionEventKind::MergeProposed),
            ("MergeApplied", ExtensionEventKind::MergeApplied),
        ] {
            let kind: ExtensionEventKind =
                serde_json::from_str(&format!("\"{spelling}\"")).unwrap();
            assert_eq!(kind, expected, "alias {spelling}");
        }
        assert!(serde_json::from_str::<ExtensionEventKind>("\"UnknownEvent\"").is_err());
        assert!(serde_json::from_str::<ExtensionEventKind>("\"SessionStartX\"").is_err());
    }

    #[test]
    fn event_kind_serializes_snake_case() {
        assert_eq!(
            serde_json::to_value(ExtensionEventKind::SubagentStop).unwrap(),
            json!("subagent_stop")
        );
    }

    #[test]
    fn payload_kind_tag_discriminates_variants() {
        // kind tag 是唯一判别：同名事件下字段相同也不会串到其它变体。
        let pre_compact = r#"{
            "event": "pre_compact",
            "sessionId": "s",
            "workspaceRoot": "/w",
            "timestamp": "t",
            "payload": {
                "kind": "pre_compact",
                "source": "auto",
                "entriesRemoved": 42
            }
        }"#;
        let event: ExtensionEvent = serde_json::from_str(pre_compact).unwrap();
        assert_eq!(
            event.payload,
            ExtensionEventPayload::PreCompact {
                source: "auto".into(),
                entries_removed: 42,
            }
        );

        // 字段相同、kind 不同 → 不同变体（SessionStart 的 source 不串到 PreCompact）。
        let session_start = r#"{
            "event": "session_start",
            "sessionId": "s",
            "workspaceRoot": "/w",
            "timestamp": "t",
            "payload": {"kind": "session_start", "source": "auto"}
        }"#;
        let event: ExtensionEvent = serde_json::from_str(session_start).unwrap();
        assert_eq!(
            event.payload,
            ExtensionEventPayload::SessionStart {
                source: "auto".into(),
                model_id: None,
                agent_type: None,
            }
        );

        // 未知 kind → fail closed。
        let unknown = r#"{
            "event": "pre_tool_use",
            "sessionId": "s",
            "workspaceRoot": "/w",
            "timestamp": "t",
            "payload": {"kind": "no_such_kind", "toolName": "read_file", "toolInput": {}}
        }"#;
        assert!(serde_json::from_str::<ExtensionEvent>(unknown).is_err());
    }

    #[test]
    fn validate_version_accepts_current_and_rejects_others() {
        let event = base_event();
        assert!(event.validate_version().is_ok());

        let mut future = base_event();
        future.version = 99;
        assert!(future.validate_version().is_err());

        let mut zero = base_event();
        zero.version = 0;
        assert!(zero.validate_version().is_err());
    }

    #[test]
    fn invalid_tool_name_fails_deserialization() {
        // ToolId 校验：非法字符 → payload 反序列化失败（fail closed）。
        let bad = r#"{
            "event": "pre_tool_use",
            "sessionId": "s",
            "workspaceRoot": "/w",
            "timestamp": "t",
            "payload": {
                "kind": "pre_tool_use",
                "toolName": "not a valid tool name!",
                "toolInput": {}
            }
        }"#;
        assert!(serde_json::from_str::<ExtensionEvent>(bad).is_err());
    }

    #[test]
    fn display_matches_wire_snake_case() {
        for kind in [
            ExtensionEventKind::SessionStart,
            ExtensionEventKind::UserPromptSubmit,
            ExtensionEventKind::PreToolUse,
            ExtensionEventKind::PostToolUse,
            ExtensionEventKind::PermissionDenied,
            ExtensionEventKind::Stop,
            ExtensionEventKind::SubagentStart,
            ExtensionEventKind::SubagentStop,
            ExtensionEventKind::PreCompact,
            ExtensionEventKind::PostCompact,
            ExtensionEventKind::MergeProposed,
            ExtensionEventKind::MergeApplied,
            ExtensionEventKind::SessionEnd,
        ] {
            assert_eq!(
                kind.to_string(),
                serde_json::to_value(kind).unwrap().as_str().unwrap(),
                "{kind:?} Display drifted from serialization"
            );
        }
    }

    #[test]
    fn truncate_json_payload_small_and_large() {
        let small = json!({"key": "small"});
        let (kept, truncated) = truncate_json_payload(small.clone());
        assert!(!truncated);
        assert_eq!(kept, small);

        let large = serde_json::Value::String("x".repeat(MAX_HOOK_PAYLOAD_BYTES + 1000));
        let (clipped, truncated) = truncate_json_payload(large);
        assert!(truncated);
        assert!(clipped.as_str().unwrap().ends_with(" [truncated]"));

        let unicode = serde_json::Value::String("€".repeat(MAX_HOOK_PAYLOAD_BYTES));
        let (clipped, truncated) = truncate_json_payload(unicode);
        assert!(truncated);
        assert!(clipped.as_str().unwrap().ends_with(" [truncated]"));
    }

    #[test]
    fn merge_payloads_round_trip_with_camel_case_wire() {
        for (payload, wire) in [
            (
                ExtensionEventPayload::MergeProposed {
                    proposal_id: "p-1".into(),
                    child_worktree: "wt-7".into(),
                },
                r#"{"kind":"merge_proposed","proposalId":"p-1","childWorktree":"wt-7"}"#,
            ),
            (
                ExtensionEventPayload::MergeApplied {
                    proposal_id: "p-1".into(),
                    applied_entries: 3,
                },
                r#"{"kind":"merge_applied","proposalId":"p-1","appliedEntries":3}"#,
            ),
        ] {
            let value = serde_json::to_value(&payload).unwrap();
            assert_eq!(
                value,
                serde_json::from_str::<serde_json::Value>(wire).unwrap(),
                "wire shape drifted"
            );
            let back: ExtensionEventPayload = serde_json::from_value(value).unwrap();
            assert_eq!(back, payload);
        }
    }
}
