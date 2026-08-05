# Phase 2 Edit Engine 完成记录

日期：2026-08-05

范围：ARC-220 / `write`、`edit`、`hashline_edit`、`apply_patch` 的统一编辑内核。

## 本阶段已完成

- `tool-contract` 新增统一 `ChangeReceipt` contract，并通过 optional creation revision 的 round-trip 测试。
- `coding-agent` 新增 `mutation_receipt` helper，统一计算内容 revision（SHA-256）、字节数、行数、byte/line delta，并在 mutation fence 中同时校验 `expectedRevision` 与 `expectedTargetFingerprint`。
- `write` 已迁移到 `tool_runtime::TypedTool<WriteArgs>`：typed schema、pre-write revision fence、`ToolOutput.details.changeReceipt`（`origin=write`）和 blocking owner 生命周期保证均已固定。
- `edit` 已迁移到 `tool_runtime::TypedTool<EditArgs>`：typed replacements、exact/fuzzy replacement、CRLF/BOM、唯一性、重叠检测、非 UTF-8 拒绝逻辑保持在同一 edit engine；写入前校验 bound target revision/fingerprint；成功返回 `origin=edit` 的 receipt 和 unified patch。
- `edit` definition 声明 `read@V1` requirement，registry 缺少 read 或 behavior 过旧时 fail closed。
- `write`、`edit` 已进入 `RuntimeSnapshot.typed_tool_ids`，由 `RuntimeService` 按 profile allowlist、operation capability 和 filesystem capability 动态构造 typed runtime；不再存在 builtin legacy executable inventory。
- self-healing edit 与旧调用方只保留显式 DTO adapter；typed 主路径使用 `ToolOutput`，没有新增第二套写入实现。
- `text_match` 固定 exact -> rstrip -> trim -> Unicode confusable normalization 四级 seek-sequence；每级只有唯一候选时才允许继续，`edit` 与 `apply_patch` 共用该 normalization primitive。
- `hashline` 使用 12 hex line hash 和行号 hint；读取结果只为本次 `offset/limit` 窗口返回 `LINE:HASH→text` anchors，并受 2000 行/50 KiB details 双预算约束；编辑时允许在 ±15 行窗口内恢复移位锚点。
- hashline 批量编辑在同一 pre-edit snapshot 上验证所有 anchor，写入前检测 ambiguous/not-found、同一行重叠和模型误粘贴 anchor prefix，按解析后的 line identity 应用。
- `apply_patch` 支持 Codex envelope 的 `Begin/End Patch`、Add/Update/Delete 子集、bare/ranged hunk header 和 no-newline metadata；update hunk 使用共享 seek-sequence。本阶段没有实现 `Move to`，因此不声明完整 Codex grammar 兼容。
- `apply_patch` 只接受 workspace-local batch paths，Add/Update/Delete 全部通过 capability target、`FileMutation` 和 identity-checked unlink；每个文件返回独立 `ChangeReceipt`。
- `apply_patch` 在 commit 前绑定并去重全部路径，按稳定路径顺序持有全部 mutation fence，再统一读取 snapshot、验证 revision/UTF-8/hunk/result budget；任何可预检错误均保证零文件写入。commit 中途 I/O 失败返回 `partial_commit` details、失败路径、已确认 receipts 与 `stateUncertain=true`。
- `write`、`hashline_edit`、`apply_patch` 的 command/source/result/diff 均有显式产品预算；旧文件读取使用 metadata + `take(limit + 1)`，receipt 的 unified diff 超过 256 KiB 时按 optional contract 省略。
- `edit`、`hashline_edit` 和 `apply_patch` 均声明 `read@V1` requirement；缺少 read 或 behavior 过旧时 registry 构造失败。

## 后续范围

- ARC-240 已把 profile、authorization、capability、provider declaration 与 delegation capability inventory 改为 `ToolId`，并已物理删除八个 filesystem/bash builtin marker、`typed_tool_names` 和旧 rebinding facade。
- ARC-250 已将 custom injected tools 与 delegation executable 迁入 typed runtime，并删除 agent-core legacy branch；builtin edit engine 不再保留 compatibility adapter。
- crash-atomic write、跨 worktree lease 和 review/event 因果关联属于后续 ARC-300/500，不在本次 typed tool 迁移中重复实现。

## 验证

已通过：

```text
cargo test -p coding-agent --lib --all-features
162 passed, 0 failed

cargo fmt --all
git diff --check
```

完整 Gate：workspace check、release API snapshots、architecture gate 与 core performance gate 均通过；architecture 当前为 581 个 Rust files、14 条 production dependency edges、33 个既有 oversized debts、0 个 execution debts。first-text baseline 为 775 us，100k session hydration 为 0.19 s。

覆盖的新增行为包括 stale revision/target identity 拒绝、取消后的同路径串行化、typed write/edit/hashline/apply_patch 执行、requirements fail-closed、四级 seek-sequence 唯一性、±15 行 shifted anchor、批量重叠/误粘贴拒绝、全批 preflight、stale 后置文件零写入、oversized source 拒绝、Add/Update/Delete receipts 和非 UTF-8 原始 bytes 保持不变。

## 下一步

1. Phase 3：抽取 managed worktree 与 child capability isolation。
2. Phase 5：将 mutation receipt 接入 hunk review 与 durable changed-file projection。
