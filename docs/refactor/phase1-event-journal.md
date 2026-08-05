# Phase 1 Event Journal 完成记录

日期：2026-08-05

范围：ARC-140 与 Phase 1 总 Gate。

## 已完成

- 新增 `event-journal`，提供 checksum durable frame、1 MiB record budget、append/fsync、跨进程 writer lease、outbox-before-record checkpoint、torn-tail repair、bounded reverse tail reader 和 forward visitor。
- codec 通过 encode/decode/sequence closure 注入；crate 不依赖 `coding-agent`，不认识 `SessionEventEnvelope`、ProductEvent、manifest、operation 或 projection。
- `coding-agent` 保留 session identity/schema/version 校验、sequence assignment、outbox source-event validation、transaction uncertainty、recovery policy、replay 和 projection。
- `SessionLogStore` 通过 journal facade 组合上述能力；旧 frame、append、repair、reverse reader 和通用 line reader 实现已删除。
- durable frame schema/version 保持 `evo.session.frame` v2，已有 session 日志无需格式迁移或 dual-read。
- fault plan 继续由产品测试注入，实际 partial write/fsync fault 在 `event-journal` 执行。

## 持久化不变量

- outbox 在 session events 之前落盘，events 不会先于 publication obligation 可见。
- lease 持有期间 sequence 单调递增，第二 writer fail closed。
- 有效但缺 newline 的末帧补 newline；校验失败的 torn tail 截断至最后完整 frame。
- bounded hydration 只扫描和保留预算允许的尾部，不为完整日志预分配内存。
- journal error 在 composition boundary 映射为产品 `CodingSessionError`；write budget rejection 保持结构化类型。

## 验证

```text
cargo check --workspace --all-targets --all-features
cargo test -p event-journal
cargo test -p coding-agent --lib
scripts/release-api-snapshots.sh
scripts/architecture-gate.sh
scripts/core-perf-gate.sh
scripts/desktop-perf-gate.sh
scripts/desktop-native-perf-gate.sh
```

关键结果：

```text
event-journal: 4 unit + 1 API contract passed
coding-agent: 129 passed
architecture: 576 Rust files, 12 production edges, 33 grandfathered oversized debts, 0 execution debts
agent first text delta: 66 us
100k session hydration: 0.20 s release test body
10 MiB desktop hydration allocated: 3,070,015 bytes
desktop scroll render P95: 172 us
native GPU/present frame P95/P99: 3,491 / 3,972 us
native input-to-post-render P95: 8,353 us
native steady RSS growth: 36,864 bytes
```

首次 native run 因 X11 DPMS 报告 `Monitor is Off` 被 compositor 节流到 1 Hz；唤醒 DPMS 后未修改代码或阈值重新运行并通过。
