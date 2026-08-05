# Phase 2 Find Tool 完成记录

日期：2026-08-05

范围：ARC-210 / `find`。

## 已完成

- `find` 已通过 `tool-runtime::TypedTool<FindArgs>` 注册和执行；legacy `AgentTool` marker 已物理删除。
- 参数契约固定为 `pattern` required、`path` 默认 `"."`、`limit` 默认 1000，`limit` 范围为 `1..=10_000`，schema 与 serde 均 fail closed，未知字段拒绝。
- 保留 globset `literal_separator` 语义、带 `/` 时匹配相对 POSIX 路径、不带 `/` 时匹配 basename、目录以 `/` 后缀、大小写稳定排序。
- 保留 `cap_walk::walk_target` 的 capability walk、symlink/path binding 约束和 walk budget；文件路径错误、目录/文件类型错误不会绕过 capability 层。
- 保留 10k 结果和 50 KiB 输出截断，以及 limit reached / byte limit notices。
- glob 解析和参数错误归类为 `InvalidArguments`，blocking walk、IO 与 task join 错误归类为 `Execution`。
- 输出改为 `ToolOutput`，details 固定包含：

```json
{
  "path": "src",
  "pattern": "**/*.rs",
  "target_fingerprint": "<64 hex identity fingerprint>",
  "total_matches": 3,
  "listed_matches": 3,
  "truncated": false
}
```

`target_fingerprint` 标识实际 walk root，是 ARC-220 mutation fence 做 read-before-edit 归因的前置信息。

## Runtime 接入

- `RuntimeSnapshot` 将 `find` 从 legacy inventory 抽出，runtime service 按 profile allowlist、operation capability 和 filesystem capability 构造 typed tool。
- `AgentState` 的 typed runtime declarations、local execution 和 provider declarations 仍保持独立；authorization inventory 同时接收迁移期 legacy tools 与 typed definitions。
- typed 参数校验发生在 before-tool authorization hook 之前，runtime deadline/cancel/turn guard 继续生效。

## 临时债务

ARC-240/250 完成后，`find_tool` marker、`typed_tool_names`、builtin rebinding facade、custom/delegation `AgentTool` 与 agent-core legacy branch 均已物理删除。

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
coding-agent: 141 passed
workspace check: passed
release API snapshots: passed
architecture: 577 Rust files，14 production edges，33 oversized debts，0 execution debts
core performance gate: passed
git diff --check: passed
```

## 下一步

继续 ARC-210/grep，沿用相同 typed runtime、capability target、authorization inventory 和迁移期 marker 机制；完成后进入 ARC-220 mutation fence 与统一 edit engine。
