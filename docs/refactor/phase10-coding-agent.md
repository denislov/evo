# Phase 10 / ARC-1000：`coding-agent` 最终瘦身

> 状态：完成（2026-08-07）

## 目标

ARC-1000 收敛 `coding-agent` 的最终产品边界：审计 FS/process/tool/journal/index
所有权，清偿 Phase 10 文件规模债务，更新 crate graph/API 文档，删除 Phase 9 期间依赖
目录启发式的过渡 AST 分层分类，并决定是否需要独立 `coding-agent-protocol`。

## 所有权审计

| 能力 | 最终 owner | `coding-agent` 保留内容 | 审计结果 |
| --- | --- | --- | --- |
| workspace FS / process / sandbox / background task | `workspace-runtime` | workspace handle、产品授权、tool DTO/event/receipt 接线 | mutation fence、opened file、process tree、sandbox、worktree、task registry 均已下沉；本次将 hook attribution 的直接 `std::fs::read` 改为 `WorkspaceAccessHandle` + `OpenedEditFile` |
| tool vocabulary / runtime | `tool-contract` / `tool-runtime` | 内建工具 schema、产品风险、输出形态、authorization/review 组合 | registry、requirements、cancel、deadline、progress、concurrency 由独立 crate 提供；`coding-agent` 不保留第二套 runtime |
| durable journal | `event-journal` | session manifest、业务 codec、transaction/finalization/replay/recovery policy | frame、lease、append/checkpoint、bounded read、torn-tail repair 位于 `event-journal`；产品事件枚举不下沉 |
| FS semantic facts / hunk review | `change-tracker` | session/application 编排、ProductEvent projection | watcher、hunk identity、receipt correlation、accept/reject/rewind 文件事实位于 `change-tracker` |
| graph / LSP index | `code-intelligence` | 服务装配、内建 query tool 与按需 context 接线 | parser/cache/index/LSP lifecycle 不在 `coding-agent` 重复实现 |
| extension / MCP | `extension-host` | trust/authorization 的产品接线、session event adapter | hook runner、MCP transport/lifecycle/credential/tool provider 位于 `extension-host` |

配置、Profile、主题、session manifest、session replay 和 rewind checkpoint 是产品配置或产品
持久化，不是用户 workspace capability 的重复实现，因此继续由 `coding-agent` 拥有。内建
filesystem/shell/web/code tools 保留的是产品 tool adapter；实际 FS/process/network/index
authority 来自对应基础 crate。

## 文件规模债务清偿

四个 Phase 10 production debt 全部拆到 900 行上限内：

| 原文件 | 收敛后行数 | 新 owner 模块 |
| --- | ---: | --- |
| `app/embedding.rs` | 724 | `app/embedding/session_access.rs` 拥有 session query/bootstrap/create/open 适配 |
| `application/operation/contract.rs` | 800 | `application/operation/contract/descriptor.rs` 拥有 descriptor 验证与 child 派生 |
| `application/operation/dispatch.rs` | 862 | `application/operation/dispatch/terminal.rs` 拥有 durable terminal outbox 提交 |
| `operations/team_invocation/runner.rs` | 832 | `operations/team_invocation/runner/model.rs` 拥有 options/outcome DTO |

`scripts/architecture/oversized-rust-debt.tsv` 删除上述四项。Architecture gate 的
`oversized_debts` 从 5 降到 1；剩余项是 Phase 5 的
`agent-core/src/agent/turn/nodes.rs`，不属于 ARC-1000。

## AST 分类清理

删除 `crates/coding-agent/tests/module_layering.rs` 和仅供该测试使用的 `proc-macro2`
dev-dependency。该测试按首级目录猜测五层，并把 `domain/projection`、`app`、
`runtime/facade` 作为例外重新分类；它守卫的是路径约定，不是 Rust/Cargo authority。

长期边界由以下机制承担：

- Cargo 第一方依赖图与无环检查；
- crate/module visibility；
- `coding_agent::api` 唯一公开 facade 的 API contract；
- CLI/Desktop 不得绕过 `coding_agent::api` 的 architecture gate；
- workspace/tool/journal/index 等基础能力的独立 crate 编译边界；
- production 900 行上限与最终 debt gate。

`syn` dev-dependency继续用于 `api_contract.rs` 的公开 DTO 构造约束；删除的是过渡 layer
分类，不是公开 API 语法检查。

## `coding-agent-protocol` 决策

本阶段不新增 `coding-agent-protocol`。CLI、Desktop、`scenario-testing` 都是同 workspace、
同版本、in-process 的 Rust consumer；CLI JSONL RPC wire 由 CLI adapter 独立拥有。目前没有
第二个需要独立版本或独立发布的 ProductEvent/operation DTO consumer，拆 crate 只会增加
re-export 和同步成本。

若以后出现跨进程或独立发布客户端，再只抽纯 DTO、protocol version 与兼容性测试；
session repository、业务校验、projection 和 runtime handle 继续归 `coding-agent`。

## 文档更新

- `crates/coding-agent/README.md`：更新内部职责边界、删除 AST 分层守卫说明、记录 protocol
  crate 决策；
- `docs/architecture.md`：更新为当前 15-crate workspace、真实依赖图、Agent actor、基础能力
  owner 与 capability-based execution；
- `docs/Evo完整架构重构计划.md`：标记 ARC-1000 完成并登记本报告。

## 验证

以下命令于 2026-08-07 通过：

```text
cargo check -p coding-agent --all-targets
cargo test -p coding-agent --all-targets --quiet
  245 unit tests passed
  2 api_contract tests passed
cargo clippy -p coding-agent --all-targets -- -D warnings
cargo check --workspace --all-targets
scripts/release-api-snapshots.sh
  coding-agent api/product-event/operation/capability contracts passed
  Desktop dependency boundary 11 passed
  TUI API boundary 5 passed
scripts/architecture-gate.sh
  rust_files=858
  dependency_edges=28
  oversized_debts=1
  execution_debts=0
cargo fmt --all -- --check
git diff --check
```

ARC-1000 不宣告 Phase 10 Final Gate：ARC-1010～1030、跨平台验证与 Phase 5 剩余文件规模
债务仍由后续任务清偿。
