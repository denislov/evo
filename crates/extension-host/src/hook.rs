//! Hook 声明（manifest `hooks` 数组）与解析。
//!
//! 每个扩展在 `extension.json` 的 `hooks` 数组中声明零到多个 hook。wire
//! 形状（camelCase，全部字段可选除 `name` / `command`）：
//!
//! ```json
//! {
//!   "name": "deny-bash-in-src",
//!   "event": "pre_tool_use",
//!   "matchTool": "bash",
//!   "matchPath": "src/",
//!   "matchProfile": "coder",
//!   "priority": 10,
//!   "command": "bin/hook.sh",
//!   "timeoutSecs": 5,
//!   "enabled": true
//! }
//! ```
//!
//! - `event`：绑定的事件（必填；同时是最严格的 matcher 事件条件）。
//! - `command`：命令。绝对路径直接执行；相对路径相对扩展目录解析。
//!   含 shell 元字符（空格/`|`/`&`/`;`/`>`/`<`/`$`/`~` 开头）时经
//!   `sh -c` 执行（与 xai-grok-hooks 同款路由）。
//! - `priority`：确定优先级。执行顺序：priority 降序，同优先级按
//!   `name` 字典序升序（稳定、可预测）。未声明 = 0。
//!
//! 解析容错：单个 hook 非法（事件拼写错、正则编译失败、字段非法）只
//! 记录错误并跳过该 hook，其余 hook 照常加载（与 manifest 容错一致）。

// Adapted from xai-grok-hooks, SOURCE_REV d6937fe255dce4133c3d000a50f9cb94de12f06f;
// HookSpec shape consulted; Evo adds priority ordering, path/profile
// matcher conditions, and drops HTTP/URL fields (HTTP runner is deferred debt).
use std::path::PathBuf;

use serde::Deserialize;

use crate::event::ExtensionEventKind;
use crate::matcher::HookMatcher;

/// `command` 触发 shell 路由的元字符集合。
fn is_shell_command(command: &str) -> bool {
    command.contains(' ')
        || command.contains('|')
        || command.contains('&')
        || command.contains(';')
        || command.contains('>')
        || command.contains('<')
        || command.contains('$')
        || command.starts_with('~')
}

/// 解析后的 hook 规格（已编译 matcher，可直接派发）。
#[derive(Debug, Clone)]
pub struct HookSpec {
    pub name: String,
    /// 绑定的事件（最严格的 matcher 事件条件）。
    pub event: ExtensionEventKind,
    /// 用户书写的原始 matcher 模式（诊断展示用）。
    pub match_tool: Option<String>,
    pub match_path: Option<String>,
    pub match_profile: Option<String>,
    /// 确定优先级：高者先执行；同优先级按 `name` 升序。
    pub priority: i32,
    pub command: String,
    /// 扩展目录（相对命令解析基准）。
    pub source_dir: PathBuf,
    /// 单次运行超时（秒）；`None` 用预算默认。
    pub timeout_secs: Option<u64>,
    pub enabled: bool,
    /// 已编译 matcher。
    pub matcher: HookMatcher,
}

impl HookSpec {
    /// 命令是否经 shell 路由执行。
    pub fn runs_via_shell(&self) -> bool {
        is_shell_command(&self.command)
    }

    /// 解析后的命令路径（绝对原样；相对相对扩展目录）。
    pub fn command_path(&self) -> PathBuf {
        let command = std::path::Path::new(&self.command);
        if command.is_absolute() {
            command.to_path_buf()
        } else {
            self.source_dir.join(command)
        }
    }
}

/// wire 形状（严格字段校验在 [`HookSpec::parse`] 内完成）。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawHookSpec {
    name: String,
    event: Option<String>,
    #[serde(default)]
    match_tool: Option<String>,
    #[serde(default)]
    match_path: Option<String>,
    #[serde(default)]
    match_profile: Option<String>,
    #[serde(default)]
    priority: i32,
    command: String,
    #[serde(default)]
    timeout_secs: Option<u64>,
    #[serde(default = "default_enabled")]
    enabled: bool,
}

fn default_enabled() -> bool {
    true
}

impl RawHookSpec {
    fn into_spec(self, source_dir: &std::path::Path) -> Result<HookSpec, String> {
        if self.name.trim().is_empty() {
            return Err("hook 'name' must not be empty".into());
        }
        if self.command.trim().is_empty() {
            return Err(format!(
                "hook '{name}' has an empty command",
                name = self.name
            ));
        }
        let event = self
            .event
            .as_deref()
            .ok_or_else(|| format!("hook '{name}' has no event", name = self.name))
            .and_then(|spelling| {
                parse_event(spelling)
                    .ok_or_else(|| format!("hook '{}' has unknown event '{spelling}'", self.name))
            })?;
        let matcher = HookMatcher::new(
            self.match_tool.as_deref(),
            self.match_path.as_deref(),
            self.match_profile.as_deref(),
        )
        .map_err(|error| {
            format!(
                "hook '{}' has an invalid matcher pattern: {error}",
                self.name
            )
        })?;
        Ok(HookSpec {
            name: self.name,
            event,
            match_tool: self.match_tool,
            match_path: self.match_path,
            match_profile: self.match_profile,
            priority: self.priority,
            command: self.command,
            source_dir: source_dir.to_path_buf(),
            timeout_secs: self.timeout_secs,
            enabled: self.enabled,
            matcher,
        })
    }
}

/// 解析事件拼写（支持 PascalCase / camelCase / snake_case 别名）。
pub fn parse_event(spelling: &str) -> Option<ExtensionEventKind> {
    ExtensionEventKind::try_parse(spelling)
}

/// 从一个 JSON 值解析 hook 数组（容错：坏 hook 记录错误并跳过）。
pub fn parse_hooks(
    value: &serde_json::Value,
    source_dir: &std::path::Path,
) -> (Vec<HookSpec>, Vec<String>) {
    let Some(array) = value.as_array() else {
        if value.is_null() {
            return (Vec::new(), Vec::new());
        }
        return (
            Vec::new(),
            vec!["manifest 'hooks' must be an array".to_string()],
        );
    };
    let mut specs = Vec::new();
    let mut errors = Vec::new();
    for (index, item) in array.iter().enumerate() {
        match serde_json::from_value::<RawHookSpec>(item.clone()) {
            Ok(raw) => match raw.into_spec(source_dir) {
                Ok(spec) => specs.push(spec),
                Err(detail) => errors.push(format!("hooks[{index}]: {detail}")),
            },
            Err(error) => errors.push(format!("hooks[{index}]: invalid hook: {error}")),
        }
    }
    (specs, errors)
}

/// 按确定优先级排序：priority 降序，同优先级按 name 字典序升序。
///
/// 该排序是 dispatcher 的唯一执行顺序来源；冲突（同优先级）由 name
/// 字典序打破，保证跨运行稳定。
pub fn sort_hooks(specs: &mut [HookSpec]) {
    specs.sort_by(|left, right| {
        right
            .priority
            .cmp(&left.priority)
            .then_with(|| left.name.cmp(&right.name))
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_full_hook_spec() {
        let value = json!({
            "name": "deny-bash",
            "event": "pre_tool_use",
            "matchTool": "bash",
            "matchPath": "src/",
            "matchProfile": "coder",
            "priority": 10,
            "command": "bin/hook.sh",
            "timeoutSecs": 7,
            "enabled": true
        });
        let (specs, errors) = parse_hooks(&json!([value]), std::path::Path::new("/ext/dir"));
        assert!(errors.is_empty(), "unexpected errors: {errors:?}");
        let spec = &specs[0];
        assert_eq!(spec.name, "deny-bash");
        assert_eq!(spec.event, ExtensionEventKind::PreToolUse);
        assert_eq!(spec.priority, 10);
        assert_eq!(spec.timeout_secs, Some(7));
        assert_eq!(
            spec.command_path(),
            std::path::PathBuf::from("/ext/dir/bin/hook.sh")
        );
        assert!(!spec.runs_via_shell());
    }

    #[test]
    fn minimal_hook_uses_defaults() {
        let value = json!({
            "name": "min",
            "event": "stop",
            "command": "check.sh"
        });
        let (specs, errors) = parse_hooks(&json!([value]), std::path::Path::new("/ext"));
        assert!(errors.is_empty(), "unexpected errors: {errors:?}");
        assert_eq!(specs[0].priority, 0);
        assert!(specs[0].enabled);
        assert_eq!(specs[0].timeout_secs, None);
    }

    #[test]
    fn shell_commands_are_detected() {
        for command in [
            "sh -c 'x'",
            "a | b",
            "a && b",
            "a; b",
            "cat > out",
            "x < in",
            "$HOME/bin/hook",
            "~/bin/hook",
        ] {
            assert!(
                is_shell_command(command),
                "{command:?} should route via shell"
            );
        }
        for command in ["bin/hook.sh", "/usr/bin/hook", "hook"] {
            assert!(
                !is_shell_command(command),
                "{command:?} should run directly"
            );
        }
    }

    #[test]
    fn missing_event_and_empty_fields_are_rejected() {
        let cases = [
            json!({"name": "x", "command": "run.sh"}),
            json!({"name": "x", "event": "no_such_event", "command": "run.sh"}),
            json!({"name": "  ", "event": "stop", "command": "run.sh"}),
            json!({"name": "x", "event": "stop", "command": "  "}),
        ];
        for value in cases {
            let (specs, errors) = parse_hooks(&json!([value]), std::path::Path::new("/ext"));
            assert!(specs.is_empty(), "specs for {value:?}: {specs:?}");
            assert_eq!(errors.len(), 1, "errors for {value:?}: {errors:?}");
        }
    }

    #[test]
    fn invalid_matcher_regex_skips_only_that_hook() {
        let value = json!([
            {"name": "bad", "event": "pre_tool_use", "command": "a.sh", "matchTool": "[invalid"},
            {"name": "good", "event": "pre_tool_use", "command": "b.sh"}
        ]);
        let (specs, errors) = parse_hooks(&value, std::path::Path::new("/ext"));
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].name, "good");
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("bad"));
    }

    #[test]
    fn non_array_hooks_is_an_error() {
        let (specs, errors) = parse_hooks(&json!({"name": "x"}), std::path::Path::new("/ext"));
        assert!(specs.is_empty());
        assert_eq!(errors.len(), 1);
    }

    #[test]
    fn sort_applies_priority_then_name() {
        let mut specs = vec![
            spec("b-low", 0),
            spec("a-mid", 5),
            spec("z-mid", 5),
            spec("a-high", 10),
        ];
        sort_hooks(&mut specs);
        let names: Vec<String> = specs.iter().map(|spec| spec.name.clone()).collect();
        assert_eq!(names, ["a-high", "a-mid", "z-mid", "b-low"]);
        // 稳定：同优先级内 name 升序，跨调用结果一致。
        sort_hooks(&mut specs);
        let names_again: Vec<String> = specs.iter().map(|spec| spec.name.clone()).collect();
        assert_eq!(names, names_again);
    }
    fn spec(name: &str, priority: i32) -> HookSpec {
        HookSpec {
            name: name.into(),
            event: ExtensionEventKind::Stop,
            match_tool: None,
            match_path: None,
            match_profile: None,
            priority,
            command: "run.sh".into(),
            source_dir: std::path::PathBuf::from("/ext"),
            timeout_secs: None,
            enabled: true,
            matcher: HookMatcher::match_all(),
        }
    }

    #[test]
    fn absolute_commands_are_not_resolved_against_source_dir() {
        let value = json!({
            "name": "abs",
            "event": "stop",
            "command": "/usr/local/bin/hook"
        });
        let (specs, _) = parse_hooks(&json!([value]), std::path::Path::new("/ext"));
        assert_eq!(
            specs[0].command_path(),
            std::path::PathBuf::from("/usr/local/bin/hook")
        );
    }
}
