# Phase 0 架构与性能基线

记录日期：2026-08-05。

这份文档是 ARC-000 的固定证据。结构数据由脚本从当前工作树生成；性能数据是同一台机器上的本地基线，主要用于发现数量级回归，不用于跨机器排名。两份 Grok 调研报告是决策输入，不是 Evo 规模或性能的长期真相。

## 复现入口

```bash
scripts/architecture-baseline.sh
scripts/architecture-gate.sh
scripts/core-perf-gate.sh
scripts/desktop-perf-gate.sh
scripts/desktop-native-perf-gate.sh
scripts/release-api-snapshots.sh
```

日志分别写入 `target/core-perf/`、`target/desktop-perf/` 和 `target/release-api-snapshots/`。生成目录不进入版本控制。

## 测量环境

| 项目 | 值 |
| --- | --- |
| 基础 commit | `2cd3ddfcb4b06df5a144b6706933271a52ed466b` |
| Workspace version | `0.7.2` |
| OS | Linux `6.12.100+deb13-amd64` x86_64 |
| CPU | AMD Ryzen 7 7840HS，8 cores / 16 threads |
| 内存 | 15,912,947,712 bytes |
| Rust | `rustc 1.96.0 (ac68faa20 2026-05-25)` |
| Cargo | `cargo 1.96.0 (30a34c682 2026-05-25)` |
| Native display | `DISPLAY=:0` |

## Crate 结构

以下内容由 `scripts/architecture-baseline.sh` 生成。`Test markers` 统计 `#[test]` 与 `#[tokio::test]`；内联单元测试仍计入所在 production 文件的 LOC，因此该列是稳定的测试入口计数，不是假装精确的测试代码规模。

| Crate | Production files | Production LOC | Test files | Test LOC | Test markers | First-party dependencies | Largest production file |
| --- | ---: | ---: | ---: | ---: | ---: | --- | --- |
| `agent-core` | 47 | 8459 | 1 | 40 | 56 | `ai` | `crates/agent-core/src/agent/turn/nodes.rs` (1199) |
| `ai` | 64 | 10501 | 2 | 816 | 36 | - | `crates/ai/src/providers/responses/stream.rs` (781) |
| `cli` | 47 | 27619 | 6 | 2267 | 106 | `coding-agent`, `tui` | `crates/cli/src/interactive/root.rs` (4450) |
| `coding-agent` | 201 | 58193 | 7 | 2097 | 122 | `agent-core`, `ai` | `crates/coding-agent/src/app/embedding.rs` (899) |
| `desktop` | 76 | 34060 | 18 | 10737 | 236 | `coding-agent` | `crates/desktop/src/ui/conversation/pane.rs` (2814) |
| `evo` | 1 | 3 | 0 | 0 | 0 | - | `src/main.rs` (3) |
| `tui` | 44 | 11114 | 37 | 4682 | 236 | - | `crates/tui/src/component/editor/mod.rs` (1422) |

First-party dependency graph：

```text
agent-core -> ai
cli -> coding-agent
cli -> tui
coding-agent -> agent-core
coding-agent -> ai
desktop -> coding-agent
```

当前图无环。Phase 1 会删除根 `evo` package，并用新的目标 crate 边界替换这张图。

## 文件规模债务

增量 Gate 当前登记 33 个既有超限文件：production 上限 900 行，test 上限 1,200 行。已登记文件不得增长，新文件不得超限，降到上限内后必须立即删除债务项；最终 Gate 要求登记清零。

| Area | Count | Largest current debt |
| --- | ---: | --- |
| `agent-core` production | 2 | `agent/turn/nodes.rs`，1199 行 |
| `cli` production | 12 | `interactive/root.rs`，4450 行 |
| `desktop` production | 14 | `ui/conversation/pane.rs`，2814 行 |
| `desktop` tests | 3 | `app/native_shell/tests/responsive.rs`，1510 行 |
| `tui` production | 3 | `component/editor/mod.rs`，1422 行 |

完整逐文件清单以 `scripts/architecture/oversized-rust-debt.tsv` 和 `scripts/architecture-baseline.sh` 输出为准。

## 契约 fixture

| Bytes | SHA-256 | Path |
| ---: | --- | --- |
| 3637 | `a6c9dcf4e45ce638b299de0a160067429cea2b01225c14d84743717c74f5e1c6` | `crates/coding-agent/tests/fixtures/client_projection/all-product-event-families.json` |
| 7013 | `d7856057d8d6b6421e8168c67713c961d7669cd4060eb294b68badd70c886911` | `crates/coding-agent/tests/fixtures/client_projection/cross-adapter-events.json` |
| 1780 | `4c959953297f35edcfe7d8c9feb796abd5d0b583eb945fcfe5a6bfeaece26bbe` | `crates/coding-agent/tests/fixtures/client_projection/cross-adapter-projection.json` |

Config、session manifest 和 JSONL frame 当前主要由 typed round-trip、transition table 与真实临时仓库测试固定，不维护第二份手写大样本。具体清单见 `docs/refactor/phase0-contract-inventory.md`。

## CLI 启动

先执行 `cargo build -p cli`，再测量预构建 debug binary 的进程启动与 `--help` 完成时间。样本单位为微秒，已排序：

```text
6129 6283 6815 6829 7136 7163 7433 7522 7805 12979
```

| 指标 | 结果 |
| --- | ---: |
| Median | 7,150 us |
| P95（nearest-rank，n=10） | 12,979 us |
| 最小值 | 6,129 us |

该指标只覆盖进程装载、参数解析和 help 输出，不覆盖首次构建。

## Core pipeline 性能

`scripts/core-perf-gate.sh` 使用 release profile 和固定测试入口：

| 场景 | 结果 | 固定约束 |
| --- | ---: | --- |
| Faux provider 首个 `TextDelta` | 107 us | 本地 Agent -> provider streamer -> event pipeline 不超过 50 ms |
| 100k event session hydration | test body 0.25 s | reverse bootstrap < 2 s；完整 bounded hydration < 3 s |
| 16 MiB noisy process output | test body 0.07 s | 输出内存有界，更新被节流 |

首 `TextDelta` 是 deterministic local pipeline baseline，不是外部模型 TTFT。真实 provider 的 DNS、TLS、区域网络、服务排队和模型推理时间不进入架构 Gate；需要做产品 SLO 时应在独立、带 provider/region/model 标签的观测系统中采集。

## Desktop headless 性能

`scripts/desktop-perf-gate.sh` 的五项 release 测试全部通过。

10 MiB / 10k transcript hydration：

| 指标 | 结果 | Budget |
| --- | ---: | ---: |
| Fixture bytes | 12,948,890 | >= 10 MiB |
| Hydration | 14,693 us | 由分配、RSS 与交互预算共同约束 |
| Allocations | 30,003 | 40,064 |
| Allocated bytes | 3,070,015 | 8 MiB |
| Retained bytes | 13,108,890 | 受 transcript hard limit 约束 |
| RSS growth | 1,875,968 bytes | 64 MiB |
| Scroll/render preparation P95 | 234 us | 16,700 us |
| Composer input P95 | 1 us | 16,700 us |

该场景原先因 `VecDeque` 增量扩容累计分配 9,443,935 bytes 而失败。Hydration 现在按 `min(snapshot.items.len(), MAX_TRANSCRIPT_BLOCKS)` 一次预留，正文仍 move 进入 projection，行为与 retained bytes 不变。

Scale matrix：

| Blocks | Hydration | Allocated bytes | Retained bytes | RSS growth |
| ---: | ---: | ---: | ---: | ---: |
| 1 | 17 us | 322 | 25 | 266,240 |
| 100 | 31 us | 30,715 | 2,590 | 32,768 |
| 1,000 | 316 us | 307,015 | 26,890 | 147,456 |
| 10,000 | 3,148 us | 3,070,015 | 278,890 | 1,679,360 |

Headless full-tree frame/input：

| 指标 | 结果 | Budget |
| --- | ---: | ---: |
| CPU frame P95, 10k blocks | 2,004 us | 16,700 us |
| Input roundtrip P95 | 3,819 us | 16,700 us |
| Input change to render P95 | 209 us | 16,700 us |
| Window RSS growth | 25,366,528 bytes | 64 MiB |

Markdown parser P95 最慢样本为 `markdown_256k` 的 82,250 us，低于 150,000 us 完成预算。

## Desktop native 性能

`scripts/desktop-native-perf-gate.sh` 在真实 GPUI window 下完成：

| 指标 | 结果 | Budget |
| --- | ---: | ---: |
| Presented frame cadence P95 | 8,346 us | informational |
| GPU/present frame P95 | 3,583 us | 16,700 us |
| GPU/present frame P99 | 3,853 us | 33,000 us |
| Input dispatch to post-render P95 | 8,346 us | 50,000 us |
| Input dispatch to post-render P99 | 8,379 us | informational |
| RSS before window | 34,443,264 bytes | informational |
| RSS after warmup | 153,554,944 bytes | <= 256 MiB absolute |
| Startup RSS growth | 119,111,680 bytes | informational |
| Steady RSS growth | 28,672 bytes | <= 64 MiB |
| Production Markdown parse-to-layout P95 | 125 us | 150,000 us |

Native Gate 会在没有 `DISPLAY`/`WAYLAND_DISPLAY` 的环境返回 exit 2；Linux CI 应通过 Xvfb 或真实 compositor 提供显示环境，而不是静默跳过。

## Phase 0 解释边界

这些数据固定的是重构前后的行为和数量级。Phase 1 以后允许通过所有权收敛、零拷贝、worktree 隔离等方式显著改善结果，但任何恶化都必须有明确原因和新的预算评审。不得为了让 Gate 变绿而直接放宽预算或删除 fixture。
