//! Hook matcher：event / tool / path / profile 条件匹配与确定优先级。
//!
//! 每个 hook 绑定一个事件（[`HookSpec::event`]，即最严格的 event 条件），
//! matcher 对其余三个维度做次级过滤：
//!
//! - `tool` / `profile`：与 xai-grok-hooks 相同的 simple-vs-regex 语义 ——
//!   只含 `[A-Za-z0-9_|]` 的模式是精确名或 `|` 分隔列表（非正则，避免
//!   `^a|b|c$` 锚定错误）；其余按**非锚定正则**匹配。
//! - `path`：**前缀匹配**（大小写敏感）。路径条件面向「目录子树」直觉
//!   （`src/` 匹配 `src/main.rs`），不引入 glob 转义复杂度。
//! - 条件之间的语义是 AND：任一条件不满足则不匹配；未声明的条件 = 通配。
//!
//! 确定优先级（[`HookSpec::priority`]）：数值高者先执行；同优先级按
//! hook 名称字典序（稳定、可预测）。冲突规则见 `dispatcher` 的 transition
//! 测试：Tool gate 首个 deny 短路、deny 优先于 allow，与执行顺序无关。

// Adapted from xai-grok-hooks, SOURCE_REV d6937fe255dce4133c3d000a50f9cb94de12f06f;
// matcher vocabulary consulted; Evo adds path/profile dimensions and
// deterministic priority ordering (not present upstream).
use regex::Regex;

use crate::event::{ExtensionEvent, ExtensionEventKind, ExtensionEventPayload};

/// 从事件提取的 matcher 可判定上下文。
///
/// 缺失值（`None`）表示该事件没有对应维度：对应条件视为「未指定 = 通配」
/// （fail-open），与 xai-grok-hooks 的 `matcher_allows` 缺省语义一致。
#[derive(Debug, Clone, Copy)]
pub struct MatchContext<'a> {
    pub event: ExtensionEventKind,
    pub tool: Option<&'a str>,
    pub path: Option<&'a str>,
    pub profile: Option<&'a str>,
}

impl<'a> MatchContext<'a> {
    /// 从事件信封提取匹配上下文。
    pub fn from_event(event: &'a ExtensionEvent) -> Self {
        let (tool, path) = match &event.payload {
            ExtensionEventPayload::PreToolUse {
                tool_name, path, ..
            }
            | ExtensionEventPayload::PostToolUse {
                tool_name, path, ..
            }
            | ExtensionEventPayload::PermissionDenied {
                tool_name, path, ..
            } => (Some(tool_name.as_str()), path.as_deref()),
            _ => (None, None),
        };
        let profile = match &event.payload {
            ExtensionEventPayload::SessionStart { agent_type, .. } => agent_type.as_deref(),
            ExtensionEventPayload::SubagentStart { subagent_type, .. }
            | ExtensionEventPayload::SubagentStop { subagent_type, .. } => {
                Some(subagent_type.as_str())
            }
            _ => None,
        };
        Self {
            event: event.kind,
            tool,
            path,
            profile,
        }
    }

    /// 归一化后的 profile 值（空字符串视为缺省）。
    fn profile_value(&self) -> Option<&'a str> {
        self.profile.filter(|value| !value.is_empty())
    }
}

/// 单一模式条件：`All` / 精确列表 / 正则。
#[derive(Debug, Clone)]
enum Pattern {
    All,
    Exact(Vec<String>),
    Regex(Regex),
}

impl Pattern {
    /// 从用户模式编译。`None` 输入 = 未声明（匹配一切）。
    fn compile(pattern: Option<&str>) -> Result<Self, regex::Error> {
        let Some(pattern) = pattern else {
            return Ok(Self::All);
        };
        if pattern.is_empty() || pattern == "*" {
            Ok(Self::All)
        } else if is_simple_form(pattern) {
            Ok(Self::Exact(exact_names(pattern)))
        } else {
            Ok(Self::Regex(Regex::new(pattern)?))
        }
    }

    fn matches(&self, value: &str) -> bool {
        match self {
            Self::All => true,
            Self::Exact(names) => names.iter().any(|name| name == value),
            Self::Regex(regex) => regex.is_match(value),
        }
    }
}

/// simple 形式：只含 ASCII 字母数字、`_`、`|`（精确名或列表，非正则）。
fn is_simple_form(pattern: &str) -> bool {
    !pattern.is_empty()
        && pattern
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'|')
}

/// 展开 `|` 列表为精确名集合（去重、跳过空项）。
fn exact_names(pattern: &str) -> Vec<String> {
    let mut names = Vec::new();
    for term in pattern.split('|') {
        if !term.is_empty() && !names.iter().any(|name| name == term) {
            names.push(term.to_string());
        }
    }
    names
}

/// 已编译的 hook matcher。空 matcher 匹配一切（[`HookMatcher::match_all`]）。
#[derive(Debug, Clone)]
pub struct HookMatcher {
    tool: Pattern,
    path: Pattern,
    profile: Pattern,
}

impl HookMatcher {
    /// 编译一个 matcher。`None` 条件 = 通配；非法正则返回错误（配置层
    /// 记录并跳过该 hook，fail closed，不静默放宽为匹配一切）。
    pub fn new(
        match_tool: Option<&str>,
        match_path: Option<&str>,
        match_profile: Option<&str>,
    ) -> Result<Self, regex::Error> {
        Ok(Self {
            tool: Pattern::compile(match_tool)?,
            path: Pattern::compile_path(match_path)?,
            profile: Pattern::compile(match_profile)?,
        })
    }

    /// 匹配一切事件的 matcher（无任何条件）。
    pub fn match_all() -> Self {
        Self {
            tool: Pattern::All,
            path: Pattern::All,
            profile: Pattern::All,
        }
    }

    /// 判定上下文是否匹配。事件维度由调用方（dispatcher）按
    /// [`HookSpec::event`] 过滤，matcher 只判定次级条件。
    pub fn matches(&self, context: &MatchContext<'_>) -> bool {
        let tool_ok = match context.tool {
            Some(value) => self.tool.matches(value),
            None => true,
        };
        if !tool_ok {
            return false;
        }
        let path_ok = match context.path {
            Some(value) => self.path.matches_path_prefix(value),
            None => true,
        };
        if !path_ok {
            return false;
        }
        match context.profile_value() {
            Some(value) => self.profile.matches(value),
            None => true,
        }
    }
}

impl Pattern {
    /// path 条件：前缀匹配。输入模式原样参与判定（不 trim、不规范化）。
    fn compile_path(pattern: Option<&str>) -> Result<Self, regex::Error> {
        Ok(match pattern {
            None => Self::All,
            Some(pattern) if pattern.is_empty() || pattern == "*" => Self::All,
            Some(prefix) => Self::Exact(vec![prefix.to_string()]),
        })
    }

    fn matches_path_prefix(&self, value: &str) -> bool {
        match self {
            Self::All => true,
            Self::Exact(prefixes) => prefixes.iter().any(|prefix| value.starts_with(prefix)),
            // path 条件从不走正则分支；防御性兜底。
            Self::Regex(regex) => regex.is_match(value),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{EXTENSION_EVENT_VERSION, SubagentStopPhase};
    use serde_json::json;
    use tool_contract::api::definition::ToolId;

    fn tool(name: &str) -> ToolId {
        ToolId::new(name).unwrap()
    }

    fn context<'a>(
        kind: ExtensionEventKind,
        tool: Option<&'a str>,
        path: Option<&'a str>,
        profile: Option<&'a str>,
    ) -> MatchContext<'a> {
        MatchContext {
            event: kind,
            tool,
            path,
            profile,
        }
    }

    #[test]
    fn match_all_matches_everything() {
        let matcher = HookMatcher::match_all();
        assert!(matcher.matches(&context(
            ExtensionEventKind::PreToolUse,
            Some("bash"),
            Some("/workspace/x"),
            None
        )));
        assert!(matcher.matches(&context(ExtensionEventKind::Stop, None, None, None)));
    }

    #[test]
    fn no_conditions_fire_all() {
        let matcher = HookMatcher::new(None, None, None).unwrap();
        assert!(matcher.matches(&context(
            ExtensionEventKind::PreToolUse,
            Some("read_file"),
            Some("a.txt"),
            None
        )));
    }

    #[test]
    fn tool_exact_and_pipe_list() {
        let matcher = HookMatcher::new(Some("read_file|list_dir"), None, None).unwrap();
        assert!(matcher.matches(&context(
            ExtensionEventKind::PreToolUse,
            Some("read_file"),
            None,
            None
        )));
        assert!(matcher.matches(&context(
            ExtensionEventKind::PreToolUse,
            Some("list_dir"),
            None,
            None
        )));
        assert!(!matcher.matches(&context(
            ExtensionEventKind::PreToolUse,
            Some("my_read_file"),
            None,
            None
        )));
        assert!(!matcher.matches(&context(
            ExtensionEventKind::PreToolUse,
            Some("grep"),
            None,
            None
        )));
    }

    #[test]
    fn tool_regex_is_unanchored() {
        let matcher = HookMatcher::new(Some("run_.*"), None, None).unwrap();
        assert!(matcher.matches(&context(
            ExtensionEventKind::PreToolUse,
            Some("run_terminal_command"),
            None,
            None
        )));
        assert!(matcher.matches(&context(
            ExtensionEventKind::PreToolUse,
            Some("xrun_yyy"),
            None,
            None
        )));
        assert!(!matcher.matches(&context(
            ExtensionEventKind::PreToolUse,
            Some("read_file"),
            None,
            None
        )));
    }

    #[test]
    fn tool_whitespace_is_regex_that_matches_nothing() {
        let matcher = HookMatcher::new(Some("   "), None, None).unwrap();
        assert!(!matcher.matches(&context(
            ExtensionEventKind::PreToolUse,
            Some("read_file"),
            None,
            None
        )));
    }

    #[test]
    fn path_is_prefix_match() {
        let matcher = HookMatcher::new(None, Some("src/"), None).unwrap();
        assert!(matcher.matches(&context(
            ExtensionEventKind::PreToolUse,
            None,
            Some("src/main.rs"),
            None
        )));
        assert!(matcher.matches(&context(
            ExtensionEventKind::PreToolUse,
            None,
            Some("src/"),
            None
        )));
        assert!(!matcher.matches(&context(
            ExtensionEventKind::PreToolUse,
            None,
            Some("lib/main.rs"),
            None
        )));
        // 精确文件名也按前缀工作。
        let matcher = HookMatcher::new(None, Some("a.txt"), None).unwrap();
        assert!(matcher.matches(&context(
            ExtensionEventKind::PreToolUse,
            None,
            Some("a.txt"),
            None
        )));
        assert!(!matcher.matches(&context(
            ExtensionEventKind::PreToolUse,
            None,
            Some("ab.txt"),
            None
        )));
    }

    #[test]
    fn profile_exact_matches_subagent_type() {
        let matcher = HookMatcher::new(None, None, Some("explore|reviewer")).unwrap();
        assert!(matcher.matches(&context(
            ExtensionEventKind::SubagentStop,
            None,
            None,
            Some("explore")
        )));
        assert!(matcher.matches(&context(
            ExtensionEventKind::SubagentStop,
            None,
            None,
            Some("reviewer")
        )));
        assert!(!matcher.matches(&context(
            ExtensionEventKind::SubagentStop,
            None,
            None,
            Some("coder")
        )));
    }

    #[test]
    fn empty_profile_value_fires_all() {
        let matcher = HookMatcher::new(None, None, Some("explore")).unwrap();
        // 空 profile 值按缺省处理：条件视为未指定 → 匹配（fail-open）。
        assert!(matcher.matches(&context(
            ExtensionEventKind::SubagentStop,
            None,
            None,
            Some("")
        )));
        assert!(matcher.matches(&context(ExtensionEventKind::SubagentStop, None, None, None)));
    }

    #[test]
    fn all_conditions_are_anded() {
        let matcher = HookMatcher::new(Some("edit"), Some("src/"), Some("coder")).unwrap();
        let hit = context(
            ExtensionEventKind::PreToolUse,
            Some("edit"),
            Some("src/a.rs"),
            Some("coder"),
        );
        assert!(matcher.matches(&hit));
        assert!(!matcher.matches(&context(
            ExtensionEventKind::PreToolUse,
            Some("read_file"),
            Some("src/a.rs"),
            Some("coder")
        )));
        assert!(!matcher.matches(&context(
            ExtensionEventKind::PreToolUse,
            Some("edit"),
            Some("lib/a.rs"),
            Some("coder")
        )));
        assert!(!matcher.matches(&context(
            ExtensionEventKind::PreToolUse,
            Some("edit"),
            Some("src/a.rs"),
            Some("explore")
        )));
    }

    #[test]
    fn invalid_regex_is_an_error() {
        assert!(HookMatcher::new(Some("[invalid"), None, None).is_err());
        assert!(HookMatcher::new(None, None, Some("[invalid")).is_err());
    }

    #[test]
    fn context_extracts_tool_path_profile() {
        let mut event = ExtensionEvent::new(
            ExtensionEventKind::PreToolUse,
            "s1",
            "/ws",
            "t",
            ExtensionEventPayload::PreToolUse {
                tool_name: tool("edit"),
                tool_input: json!({}),
                tool_input_truncated: false,
                path: Some("src/a.rs".into()),
            },
        );
        event.version = EXTENSION_EVENT_VERSION;
        let context = MatchContext::from_event(&event);
        assert_eq!(context.event, ExtensionEventKind::PreToolUse);
        assert_eq!(context.tool, Some("edit"));
        assert_eq!(context.path, Some("src/a.rs"));
        assert_eq!(context.profile, None);

        event.payload = ExtensionEventPayload::SubagentStop {
            subagent_type: "explore".into(),
            phase: SubagentStopPhase::Gate,
            stop_reason: None,
        };
        let context = MatchContext::from_event(&event);
        assert_eq!(context.tool, None);
        assert_eq!(context.profile, Some("explore"));
    }
}
