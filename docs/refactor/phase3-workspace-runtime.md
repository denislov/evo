# Phase 3 Workspace Runtime 抽取完成记录

日期：2026-08-05

范围：ARC-300 / `workspace-runtime` 抽取：filesystem capability、path binding、mutation fence、process primitive 与 workspace identity。

## 本阶段已完成

### workspace-runtime crate

- 新增 `api` facade（唯一公共入口）：
  - identity：`WorkspaceId`（derived / user-supplied、128 字节、强制 kind 前缀）、`WorkspaceKind`（Source / ManagedChild / Projectless / Legacy）、`WorkspaceHandle`（id + kind + absolute root）、`WorkspaceLease`（owner operation / parent session / base revision + 严格 lifecycle transition 状态机）、`WorkspaceLifecycle`；删除可任意组合 id/kind 的 `with_id`，user-supplied identity 只能通过 kind-aware constructor 创建。
  - authority：`WorkspaceAccessHandle` 是 coding-agent 唯一持有的 opaque operation handle，内部统一拥有 filesystem capability、authorization binding table 与 shell configuration；底层 `FilesystemCapability` / `ShellCapability` 不再从 facade 导出。
  - `WorkspaceError`：只保留平台层真实出现的两种失败（`Resource`、`UnsupportedCapability`）。
  - fs：`FilesystemCapability`、`FilesystemTarget`、`FilesystemBindingDescriptor`、`FilesystemPathPreview`、`FilesystemReviewTargetError`、`FileMutation`/`MutationGuard`、`OpenedEditFile`、`CapWalkEntry`/`CapWalkRoot`/`walk_target` 与 walk 预算。
  - process：`ShellCapability`、`ProcessSpec`、`ProcessOutput`、`ProcessOutcome`、`OutputBudget`、`ProgramKind`、`EnvPolicy`、`run`、`path_exists`、`resolve_shell_path`。
- `CodingAgentResolvedWorkspace` 持有 `WorkspaceHandle`：Project → `WorkspaceKind::Source`，Projectless → kind-prefixed user identity + `WorkspaceKind::Projectless`，Legacy → `WorkspaceKind::Legacy`；operation admission 优先沿用已解析 identity，并在 opaque access handle 内打开 authority。
- 依赖原则落实：workspace-runtime 不依赖 `coding-agent`、`agent-core` 或任何 UI crate。process 输出 tail 截断改为 crate 内实现（原依赖 `agent_core::api::execution::truncate_tail`），保留 trailing newline、line limit、byte limit 和 UTF-8 partial-line 语义；`OpenedEditFile` 的大小标签同理。

### coding-agent 收敛

- `platform/fs`、`platform/process` 已物理删除；`platform` 只保留 `io`、`time`。
- `OperationCapabilitySnapshot` 由 `filesystem + shell` 两个底层 capability 字段收敛为单一 `WorkspaceAccessHandle`；authorization、tools、self-healing edit 与 child/delegation snapshot 只 clone opaque handle，coding-agent production source 对 `FilesystemCapability` / `ShellCapability` 的引用为 0。
- 全部 20 个使用点（tools/filesystem/*、tools/shell、runtime/file_review、services/authorization、application/capability、application/operation/permit、operations/self_healing_edit、delegation 测试等）改为通过 `WorkspaceAccessHandle` 调用。
- `CodingSessionError` 增加 `From<WorkspaceError>` 映射（`UnsupportedCapability` / `Resource`），产品错误词汇不变，`public_error` 映射无需改动。
- mutex poison 策略随 capability 迁移为 crate 内 `resource` 模块；`workspace-runtime` 与 `coding-agent` 各持一份（跨 crate 无法共享私有策略）。

## 验证

已通过：

```text
cargo test -p workspace-runtime --all-features
18 unit + 2 API contract passed, 0 failed（identity/lease lifecycle、kind-prefixed user identity、binding 容量、
symlink 拒绝、capability walk fd 恒定、mutation fence 所有权/panic、process
teardown/timeout/预算、tail trailing-newline/byte-limit golden）

cargo test --workspace --all-features
全部通过（coding-agent 149 lib tests 通过）

cargo clippy --workspace --all-targets --all-features -- -D warnings
0 errors

cargo fmt --all -- --check
干净

bash scripts/architecture-gate.sh
architecture_gate rust_files=592 dependency_edges=15 oversized_debts=32 execution_debts=0 mode=incremental

bash scripts/gate.sh
全部通过（含 internal-dependencies.tsv 新增 coding-agent → workspace-runtime 边）

bash scripts/core-perf-gate.sh
三个固定入口各运行 1 个 release test；process noisy-output 入口已迁移到 workspace-runtime，
脚本在 filter 匹配数量不是 1 时失败，禁止静默 0-test false-green

bash scripts/release-api-snapshots.sh
全部通过；workspace-runtime api_contract 已进入显式 release API inventory
```

## 关键决策

- 不搬 `platform/io`（bounded/output/redaction）与 `platform/time`：它们服务产品层（redaction、session id、app 读取），不是 workspace capability 的边界。
- `WorkspaceError` 只含平台层真实变体；`CodingSessionError` 通过 `From` 映射保持既有 public error 契约。
- operation cleanup 测试通过 authorization-bound target 的可取回性和 fd 数量验证精确清理；底层 capability 类型及其 inspection API 保持私有。
- process 测试 fixture（`ProcessFixture`）复制到 workspace-runtime 测试模块；coding-agent 测试仍用自己的 `test_support::ProcessFixture`。
- `process.rs` 拆分 `process/mod.rs`（676 行生产）+ `process/tests_file.rs`，满足 900 行生产上限。

## 后续范围

- ARC-310 `WorktreeBuilder` 最小集（Git worktree + copy/reflink fallback、cancel、dirty snapshot、untracked 同步）。
- ARC-320 registry、恢复与 GC（原子文件/JSONL registry、orphan 扫描、age/disk budget）。
- ARC-330 child capability 隔离（delegation/team 强制 managed worktree，`WorkspaceKind::ManagedChild` 首次落地）。
- ARC-340 merge protocol（`ChangeSet` / `MergeProposal`、base revision 乐观校验）。
- ARC-350 worktree 测试矩阵 → Phase 3 Gate。
