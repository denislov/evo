# Phase 3 WorktreeBuilder 最小集完成记录

日期：2026-08-05

范围：ARC-310 / `workspace-runtime` `WorktreeBuilder`：Git worktree + copy/reflink fallback、cancellation、dirty source snapshot、untracked 同步与 ignore policy。

## 本阶段已完成

### WorktreeBuilder 最小集

- `WorktreeBuilder::new(source_handle, dest, owner_operation)` + `parent_session` + `working_tree_mode` + `cancellation_token` + `create()`；source 必须通过 `WorkspaceHandle` 进入，不接受无 identity 的裸 source path。
- **Git-linked 策略**（source 是 git repo）：`git worktree add --detach <dest>` 建立 tracked checkout，随后 dirty sync。
- **Copy fallback 策略**（非 git source）：整树复制，逐文件 `reflink_or_copy`（文件系统支持时 reflink，否则普通 copy），目录、symlink 与嵌套结构完整保留。
- **dirty sync**：`git status --porcelain -z --untracked-files=all` 按 NUL 协议解析 dirty tracked 修改（M/A/R/C）、untracked 文件与删除（D）；rename 使用 header 中的 destination 和后续 original token，先统一删除旧路径、再复制最终路径，支持 swap/type change。
- **fail-closed snapshot**：status 解析、目录迭代、删除或复制任一步失败都会让 create 失败并进入清理，不再返回携带 `issues` 的不完整成功快照；Git stdout/stderr 在读取期间执行硬预算，而不是完整读取后再判断大小。
- **ignore policy**：status 默认排除 ignored 文件，dirty sync 因此天然跳过 `.gitignore` 命中项；无额外 pattern 引擎。
- **cancellation**：`CancellationToken` 在 create 入口、Git 命令执行期间、命令结束后和 copy/sync 循环每 64 个条目检查；Git 运行在独立 Unix process group / Windows Job Object，取消会终止完整进程树。
- **验证式清理**：取消或失败时执行 `git worktree remove --force`、磁盘删除和 `git worktree prune --expire now`，随后同时复查 worktree registration 与 destination；无法证明清理完成时返回 `CleanupFailed`，不再吞掉清理错误。
- **路径防护**：dest 必须为绝对路径且完全不存在（包含 dangling symlink）；先消解 `.`/`..`，再 canonicalize source 与 destination 最近的现存父目录，拒绝直接、父目录跳转或 symlink alias 形成的 source 内 destination。
- **完整 managed identity**：成功返回不可拆分的 `ManagedWorktree { WorkspaceLease, WorktreeReport }`；child handle 固定为 `WorkspaceKind::ManagedChild`，lease 同时包含 owner operation、parent session、HEAD base revision 和 `Ready` lifecycle，report 包含 GitLinked/Copy creation mode 与复制统计。

### 关键修复（实现中发现）

- `reflink_copy::reflink_or_copy` 在目标已存在时返回 `AlreadyExists`（git checkout 已生成同名 tracked 文件），copy 前必须显式清除目标，否则 dirty sync 静默失败。
- 干净 git repo 的 status 为空时取消检查会被跳过：在 create 入口与 sync 入口补显式 `check_cancelled`。
- `WorktreeError` 字段命名避开 thiserror 的 `#[source]` 语义保留名。

## 验证

已通过：

```text
cargo test -p workspace-runtime --all-features
40 unit + 3 API contract passed, 0 failed（原 12 个 worktree 测试之外新增 9 个专项回归测试）

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

覆盖行为：tracked checkout 与 HEAD commit 一致、dirty 修改同步、untracked 同步（含嵌套目录逐文件）、ignored 跳过、删除同步、staged rename、porcelain copy token 顺序、malformed status fail-closed、CleanTracked 模式、copy fallback、symlink 保留、相对 dest 拒绝、dangling symlink、`..`/symlink source containment、managed identity/Ready lifecycle、预取消、copy 中途取消、Git hook 执行期间 process-tree cancel + registration/disk cleanup、缺失 source 拒绝。

两个 Windows target 的交叉检查当前仍被 ARC-300 `fs/capability/path.rs` 的既有 `cap_std::MetadataExt` 编译问题提前阻断；本轮 Windows Job Object 分支未进行实机执行，留待 ARC-350 Windows 测试矩阵统一验收。

## 后续范围

- ARC-320 registry、恢复与 GC：原子文件/JSONL registry 登记 worktree、启动时处理进程崩溃产生的 orphan、lifecycle 持久化。
- ARC-330 child capability 隔离：delegation/team 调用前申请 managed worktree，`WorkspaceAccessHandle` 替换为 child worktree 句柄。
- ARC-340 merge protocol：child 产生 `ChangeSet`/`MergeProposal`，parent base revision + child base revision 乐观校验。
- ARC-350 worktree 测试矩阵：并行 child 修改同名文件/hunk、crash、disk full、Windows 路径。
