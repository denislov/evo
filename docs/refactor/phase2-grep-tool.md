# Phase 2 Grep Tool 完成记录

日期：2026-08-05

范围：ARC-210 / `grep`。

## 已完成

- `grep` 已通过 `tool-runtime::TypedTool<GrepArgs>` 注册和执行；legacy `AgentTool` marker 已物理删除。
- 参数契约固定为：`pattern` required；`path` 默认 `"."`；`glob` optional；`ignoreCase`、`literal` 默认 false；`context` 默认 0 且范围 `0..=20`；`limit` 默认 100 且范围 `1..=1_000`。
- `serde(deny_unknown_fields)`、camelCase `ignoreCase` schema 映射和 schema/serde 双重范围校验均已固定，未知字段、错误数字类型和越界值在 authorization hook 前 fail closed。
- 保留 regex/literal 搜索、globset `literal_separator` 过滤、capability walk、逐文件 5 MiB read budget、CRLF/CR 归一化、上下文窗口、每行 500 字符截断、1000 matches 和 50 KiB 总输出限制。
- regex/glob/参数错误归类为 `InvalidArguments`；blocking walk/task join/IO 错误归类为 `Execution`；过大文件继续按既有策略跳过并在输出 notice 中报告。
- 输出改为 `ToolOutput`，details 固定包含：

```json
{
  "path": ".",
  "pattern": "needle",
  "target_fingerprint": "<64 hex identity fingerprint>",
  "matches": 1,
  "skipped_large_files": 0,
  "truncated": false
}
```

## Runtime 接入

- `RuntimeSnapshot` 将 `grep` 从 legacy inventory 抽出，runtime service 按 profile allowlist、operation capability 和 filesystem capability 构造 typed tool。
- local executable、typed runtime definitions、provider declarations 和 authorization inventory 继续保持 ARC-200/210 的独立边界。
- typed runtime deadline/cancel、turn guard、structured error adapter 和 provider declaration filtering 均未新增旁路。

## 临时债务与 Phase 2 Gate

ARC-240 已将上游 profile/capability/authorization inventory 切换为 `ToolId`；ARC-250 已物理删除 `read_tool`、`ls_tool`、`find_tool`、`grep_tool` marker、`typed_tool_names`、custom/delegation `AgentTool` 与 agent-core Legacy dispatch。

ARC-210 自身已完成；Phase 2 总 Gate 仍由 ARC-220（mutation fence/edit）、ARC-230（shell）、ARC-240（requirements/ToolId）和 ARC-250（删除旧路径）组成。

## 验证

```text
cargo test -p coding-agent --lib --all-features
cargo check --workspace --all-targets --all-features
scripts/release-api-snapshots.sh
scripts/architecture-gate.sh
scripts/core-perf-gate.sh
git diff --check
```

最近结果：

```text
coding-agent: 139 passed
workspace check: passed
release API snapshots: passed
architecture: 576 Rust files，14 production edges，33 oversized debts，0 execution debts
agent first text delta: 809 us（core perf gate passed）
100k session hydration release test body: 0.19 s
noisy process output bounded/throttled test: passed
git diff --check: passed
```

## 下一步

进入 ARC-220：统一 `write/edit/apply_patch` mutation fence、read revision requirements、hashline anchor 和 `ChangeReceipt`，让本次 `read` 的 content revision/fingerprint details 具备真正的消费方。
