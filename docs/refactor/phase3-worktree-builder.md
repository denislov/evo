# Phase 3 WorktreeBuilder 最小集完成记录

日期：2026-08-05

范围：ARC-310 / `workspace-runtime` `WorktreeBuilder`：Git worktree + copy/reflink fallback、cancellation、dirty source snapshot、untracked 同步与 ignore policy。

## 本阶段已完成

### WorktreeBuilder 最小集

- `WorktreeBuilder::new(source, dest)` + `working_tree_mode` + `cancellation_token` + `create()`。
- **Git-linked 策略**（source 是 git repo）：`git worktree add --detach <dest>` 建立 tracked checkout，随后 dirty sync。
- **Copy fallback 策略**（非 git source）：整树复制，逐文件 `reflink_or_copy`（文件系统支持时 reflink，否则普通 copy），目录、symlink 与嵌套结构完整保留。
- **dirty sync**：`git status --porcelain -z --untracked-files=all` 解析 dirty tracked 修改（M/A/R/C）、untracked 文件与删除（D），逐一镜像到 worktree；删除在 worktree 中同步移除。
- **ignore policy**：status 默认排除 ignored 文件，dirty sync 因此天然跳过 `.gitignore` 命中项；无额外 pattern 引擎。
- **cancellation**：`CancellationToken` 在 create 入口、git 命令前后、copy/sync 循环每 64 个条目检查；取消或失败时先 `git worktree remove --force` 注销 git 注册，再删除 dest，不留下半成品。
- **防护**：dest 必须不存在、且不得位于 source 内部（否则 worktree 自身会被 status 当成 untracked 目录递归进 sync）。
- **identity 支撑**：`WorktreeReport` 返回 `worktree_path`、`commit`（git source 的 HEAD）、`creation_mode`（GitLinked/Copy）与 copy 统计；owner operation / parent session / base revision / lifecycle 由既有 `WorkspaceLease` 承载（ARC-330 组合使用）。

### 关键修复（实现中发现）

- `reflink_copy::reflink_or_copy` 在目标已存在时返回 `AlreadyExists`（git checkout 已生成同名 tracked 文件），copy 前必须显式清除目标，否则 dirty sync 静默失败。
- 干净 git repo 的 status 为空时取消检查会被跳过：在 create 入口与 sync 入口补显式 `check_cancelled`。
- `WorktreeError` 字段命名避开 thiserror 的 `#[source]` 语义保留名。

## 验证

已通过：

```text
cargo test -p workspace-runtime --all-features
31 passed, 0 failed（新增 12 个 worktree 测试，3 轮重复运行稳定）

cargo test --workspace --all-features
全部通过

cargo clippy --workspace --all-targets --all-features -- -D warnings
0 errors

cargo fmt --all -- --check
干净

bash scripts/architecture-gate.sh
architecture_gate rust_files=595 dependency_edges=15 oversized_debts=32 execution_debts=0 mode=incremental

bash scripts/release-api-snapshots.sh
全部通过
```

覆盖行为：tracked checkout 与 HEAD commit 一致、dirty 修改同步、untracked 同步（含嵌套目录逐文件）、ignored 跳过、删除同步、CleanTracked 模式不同步、copy fallback 全量镜像、symlink 保留（git dirty sync 与 copy fallback）、dest 已存在拒绝、dest 在 source 内拒绝、预取消清理、copy 中途取消清理、缺失 source 拒绝。

## 后续范围

- ARC-320 registry、恢复与 GC：原子文件/JSONL registry 登记 worktree、启动 orphan 扫描（含本 builder 无法注销的 git worktree 记录）、lifecycle 持久化。
- ARC-330 child capability 隔离：delegation/team 调用前申请 managed worktree，`WorkspaceAccessHandle` 替换为 child worktree 句柄。
- ARC-340 merge protocol：child 产生 `ChangeSet`/`MergeProposal`，parent base revision + child base revision 乐观校验。
- ARC-350 worktree 测试矩阵：并行 child 修改同名文件/hunk、crash、disk full、Windows 路径。
