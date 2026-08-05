# Phase 2 Read Tool 完成记录

日期：2026-08-05

范围：ARC-210 / `read`。

## 已完成

- `read` 已通过 `tool-runtime::TypedTool<ReadArgs>` 注册和执行，不再进入 legacy `AgentTool` executable path。
- 参数契约改为 `serde(deny_unknown_fields)` + `JsonSchema`，`offset`/`limit` 的 `1..=5_242_881` 约束同时在 schema 与反序列化阶段 fail closed。
- 保留 5 MiB 文件读取预算、2000 行/50 KiB 输出截断、offset/limit continuation，以及 JPEG/PNG/GIF/WebP 图片读取。
- 图片路径继续执行 encoded-size、dimension 和 decode-allocation 限制。
- 文件访问继续使用 `FilesystemCapability`、operation-bound target 和已打开 capability handle，不重新按字符串路径打开文件。
- 输出改为 `ToolOutput`，失败改为 `ToolErrorKind`；AI conversation 映射只经过 ARC-200 adapter。
- 输出 details 固定包含：

```json
{
  "path": "notes.txt",
  "target_fingerprint": "<64 hex identity fingerprint>",
  "content_sha256": "<64 hex content revision>",
  "bytes": 11
}
```

其中 `target_fingerprint` 标识实际打开对象，`content_sha256` 标识本次读取内容 revision，二者是 ARC-220 read-before-edit mutation fence 的前置契约。

## Runtime 接入

- `AgentState` 可同时持有迁移期 legacy tools、typed `ToolRuntime` 与 provider-only declarations，并校验三类 ID 全局唯一。
- provider request 会声明 runtime definitions；本地 tool call 按 ID 路由到 typed runtime。
- typed 参数预校验发生在 before-tool authorization hook 之前，executor 内仍二次反序列化，避免绕过 runtime 入口。
- runtime deadline/cancel、turn-level execution guard、progress adapter、termination 和 structured error 均已接通。
- coding-agent 根据 profile allowlist、operation capability 和 filesystem capability 动态构造 typed `read`；authorization inventory 同时接受 legacy 与 typed definitions。

## 临时债务

上游 profile/filter API 仍以 `Vec<AgentTool>` 表示 inventory，因此 `read_tool_with_operations` 暂时只返回不可执行 marker。`RuntimeSnapshot` 构造时立即抽走该 marker，实际 Agent 不注册它。

ARC-240/250 完成后，该 marker、`typed_tool_names`、builtin rebinding facade、custom/delegation `AgentTool` 与 agent-core legacy branch 均已物理删除。

## 验证

```text
cargo test -p tool-runtime --all-features
cargo test -p agent-core --all-features
cargo test -p coding-agent --lib
cargo check --workspace --all-targets --all-features
scripts/release-api-snapshots.sh
scripts/architecture-gate.sh
scripts/core-perf-gate.sh
git diff --check
```

最近结果：

```text
tool-runtime: 8 passed，API contract passed
agent-core: 58 passed，1 ignored release baseline，API contract passed
coding-agent: 135 passed
workspace check: passed
release API snapshots: passed
architecture: 577 Rust files，14 production edges，33 oversized debts，0 execution debts
agent first text delta: 131 us
100k session hydration release test body: 0.20 s
noisy process output bounded/throttled test: passed
git diff --check: passed
```

## 下一步

继续 ARC-210，按 `ls -> find -> grep` 顺序迁移剩余只读文件工具。`ls` 采用同一 typed runtime、capability target、authorization inventory 和临时 marker 机制。
