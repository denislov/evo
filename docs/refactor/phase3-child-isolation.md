# Phase 3 / ARC-330：Child capability 隔离

> 状态：完成（2026-08-05）
> 前序：ARC-300 `workspace-runtime`、ARC-310 WorktreeBuilder、ARC-320 Registry/恢复/GC
> 目标：任何 delegation/team child 都不再直接写父 workspace

## 决策

- **写权限 child 必须申请 managed worktree**：`ChildWorkspacePolicy::decide` 依据
  parent capability 与 child profile 工具集判定三种策略：
  - `Managed`：parent 与 child profile 的有效工具交集持有 write/edit/hashline_edit/apply_patch/bash 任一工具 → 必须
    申请独立 managed worktree，child filesystem/shell cwd 绑定 worktree。
  - `ReadOnlyShared`：child 仅只读工具（read/ls/find/grep）→ 共享父 read path；
    工具集本身就是只读护栏，无写路径可逃逸。
  - `Projectless`：parent 无 workspace authority → child 同样无 workspace。
- **显式 shared-cwd 模式**：当前产品没有共享 cwd 的配置入口，因此不提供降级
  路径——未配置 registry 时 delegation/team child fail-closed（
  `UnsupportedCapability: child workspace isolation requires a managed worktree registry`），
  绝不静默退回父目录。将来若引入显式 shared-cwd profile 配置，需在
  `ChildWorkspacePolicy` 中显式添加该分支并保持 fail-closed 默认。
- **不再 clone 父 capability**：`capability_snapshot_for_delegated_profile` 新增
  `ChildWorkspaceBinding` 参数，`Managed` 时用 child worktree 的
  `WorkspaceHandle` 重新 `WorkspaceAccessHandle::open`（复用父的 shell 配置），
  删除原先 `parent.workspace.clone()` 路径。
- **child runtime 重定向**：`RuntimeSnapshot` 新增 `child_workspace` 覆盖
  （cwd + WorkspaceHandle），`PromptTurnOptions::bind_child_workspace` 在 child
  admission 前把 child turn 的 cwd/workspace 指向 worktree；root admission 路径
  不受影响（child 不走 `snapshot_input_for_operation`）。
- **生命周期**：`ChildWorktreeLease` 持有 registry + record；child 到达任何
  terminal（成功/失败/取消）后 `release()`，discard 持久化
  `Ready/Active/MergePending -> Discarded -> Cleaning -> Removed`，再删 git 注册、
  目录、prune、记录；release 失败不会提前标记 released，`Drop` 会再次尝试，panic 也不泄漏。
- **取消传播**：team runner 将父/team cancellation token 传入每个成员和 supervisor 的
  worktree provisioning；取消期间不会使用脱离父生命周期的临时 token。正常取消创建会
  清理 `Creating` record，进程崩溃则由 ARC-320 startup maintenance 恢复。
- **并发上限**：删除 `MAX_TEAM_MEMBER_CONCURRENCY = 2`。team member 并发由
  `WorktreeRegistry` 容量决定（`open_with_capacity`，产品默认 4）：
  `team_member_concurrency = min(capacity, member_count)`；容量耗尽时 worktree
  申请 fail-closed，不排队、不超售。

## 落点

| 变更 | 位置 |
| --- | --- |
| registry capacity / discard / CapacityExhausted | `workspace-runtime/src/worktree/registry.rs` |
| registry 注入 OperationControl（生命周期宿主） | `coding-agent/src/application/operation/control/*`、`runtime/facade/lifecycle.rs` |
| session 可选 registry 目录（测试隔离用） | `CodingAgentSessionOptions::with_worktree_registry_dir` |
| 策略/绑定/lease/申请 | `coding-agent/src/operations/delegation/worktree.rs` |
| snapshot 重构（不再 clone 父） | `coding-agent/src/operations/delegation/mod.rs` |
| child runtime 重定向 | `coding-agent/src/operations/prompt/context.rs` |
| agent invocation 集成 | `coding-agent/src/operations/agent_invocation/runner.rs` |
| team invocation 集成 + 并发预算 | `coding-agent/src/operations/team_invocation/runner.rs` |

## 验证

```text
cargo test --locked -p workspace-runtime --all-features
65 passed（新增 capacity / discard / lifecycle recovery / startup owner liveness / 身份防伪测试）

cargo test --locked -p coding-agent --all-features
159 passed（含 worktree policy、agent e2e 写 child 隔离、team cancellation、fail-closed、容量耗尽、
read-only/projectless 策略、Drop 兜底回收、typed handle）

cargo test --workspace --all-features
全部通过

bash scripts/gate.sh
architecture_gate rust_files=602 dependency_edges=15 oversized_debts=32 execution_debts=0 mode=incremental
```

e2e 覆盖的隔离证据：child 完成一次真实 write 后，父 workspace 逐字节不变、
child capability 的 workspace root 位于 registry worktrees 根下、child runtime
cwd 与 worktree 一致、terminal 后 worktree 与记录均被回收。

## 后续

- ARC-340 merge protocol 将把 `discard` 替换为 `MergePending -> Merged` 提案路径；
  本阶段 child 产物不并入父目录。
- 显式 shared-cwd profile 配置若引入，需在 `ChildWorkspacePolicy` 增加显式分支。
