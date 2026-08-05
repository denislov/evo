# Phase 4 / ARC-420：Review domain/API

> 状态：完成（复验 2026-08-06）
> 前序：ARC-400 `FsEventService`、ARC-410 `HunkTracker actor`
> 目标：让 Review UI/API 只消费 tracker 的文件事实，提供可验证的 accept/reject 与统一跨 adapter DTO。

## 决策

- **baseline→current 是唯一 review 模型**：每个 tracked file 同时保存可验证的
  `baseline`、当前磁盘 `current`、active snapshot、stable `HunkId`、revision、source
  attribution 与 bounded diff。连续 receipt 从 original baseline 重算，不覆盖最近一次
  diff。creation、deletion 与 empty file 由 `after_exists` 明确区分。
- **typed receipt 直连 producer**：write、edit、hashline_edit、apply_patch 在 mutation
  commit 后直接把 `ChangeReceipt` 交给 `ReviewService`/`HunkTracker`，不再从
  `ToolOutput.details` 推断文件变化。apply_patch 多文件 tracking 失败时返回 committed、
  tracked、untracked receipts 与 reconcile 标记。
- **来源和身份不猜测**：receipt/event 因果关联按 path、存在性和 after revision 匹配；
  外部非重叠修改不会覆盖已有 Agent hunk attribution。文件级 source 可标记
  `external_edit_on_agent_file`，hunk 保留自己的 Agent source。
- **accept/reject 是两阶段 capability flow**：先通过 session facade 校验 DTO identity、
  sequence、after revision；再用 `WorkspaceAccessHandle` 打开 capability-bound target，
  复验内容 revision、存在性和 target fingerprint。tracker actor 随后重验磁盘事实、
  HunkId/sequence/fingerprint 并生成 typed `RejectPlan`；reject 写盘前再次复验 bound
  identity，成功后以 `HookEdit` receipt 重算 snapshot。stale、替换 inode、不可恢复
  baseline、binary/oversized 内容均 fail closed。
- **统一 product DTO 与事件**：`CodingAgentReviewChange`/
  `CodingAgentReviewHunk` 同时作为 snapshot 与 live `Review::Changed` event 的字段
  来源。client projection 收到 Review event 后替换 `context.changes`；ToolOutput JSON
  不再参与 context fold。协议为 breaking bump：`product_event 4.0`、`ui_snapshot 4.0`。
- **child discard 复用既有 operation contract**：`discard_child_proposal` 是 session facade
  wrapper，内部只调用 `DiscardChildWorktree` typed operation；Desktop adapter 通过同一
  wrapper，旧 `ReviewChangedFile` adapter 命名已改为 `OpenChange`。

## 落点

| 变更 | 位置 |
| --- | --- |
| tracker baseline/current、stable hunk、accept/reject plan | `crates/change-tracker/src/hunk/{actor,reconstruct,state,validation}.rs`、`hunk.rs` |
| typed producer receipt | `crates/coding-agent/src/tools/filesystem/{mutation_receipt,write,edit,hashline,apply_patch}.rs` |
| session Review service 与统一 facade | `crates/coding-agent/src/services/review.rs`、`runtime/file_review.rs`、`runtime/file_review_tests.rs` |
| shared DTO/live event/projection | `crates/coding-agent/src/events/review.rs`、`events/mod.rs`、`runtime/client/{connection,context_fold}.rs` |
| Desktop adapter rename and projection contract | `crates/desktop/src/runtime/{protocol,client,worker}.rs`、`runtime/tests/recovery.rs` |

## 验证

```text
cargo test --locked -p change-tracker --all-features
45 passed

cargo test --locked -p coding-agent --all-features
178 unit tests + 2 API contract + 2 module layering + 7 doctests passed

cargo test --locked -p workspace-runtime --all-features
91 unit tests + 3 API contract passed

cargo test --locked -p desktop --all-features
303 passed，5 ignored（release performance gate）

bash scripts/release-api-snapshots.sh
全部 package/API/projection/operation/dependency boundary targets passed

cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
通过

CARGO_TARGET_DIR=target/arc420-gate cargo test --locked --workspace --all-features
通过

CARGO_TARGET_DIR=target/arc420-gate bash scripts/gate.sh
通过（rust_files=629，dependency_edges=17，oversized_debts=32，execution_debts=0）

git diff --check
通过
```

架构限制：新增 production Rust 文件均低于 900 行，`execution_debts=0`。Review action
覆盖 stale revision、stale HunkId、target replacement、creation/deletion/empty file、
capability-bound reject、同 tool batch path authorization、typed receipt 到 live projection
的端到端路径。

## 后续

- ARC-430：将 tracker snapshot、workspace snapshot、session event cursor 和 active branch
  纳入一致的 rewind branch 恢复协议。
- ARC-440：补 watcher gap full reconcile、复杂冲突 reject、rewind 后继续编辑/merge 与
  crash reopen 的跨域矩阵。
