# Phase 10 / ARC-1010 可观测性完成报告

日期：2026-08-08

## 结论

ARC-1010 已完成。仓库新增独立 `observability` crate 作为唯一的可观测性外发隐私边界，
并把生命周期事实与外发机制彻底分离：领域 crate 只发不含正文和路径的结构化 `tracing`
event，subscriber 统一执行字段 allowlist、secrets/path scrub、UTF-8 字段预算和 JSON 总预算，
之后才允许 telemetry sink 或 crash writer 消费。

本次没有引入 OTLP、SaaS endpoint 或后台上传线程。当前产品没有已确定的 telemetry 后端，
因此长期 contract 是一个接收“已脱敏、有界 JSON bytes”的 `TelemetrySink`；CLI/Desktop
默认安装 subscriber 和 crash reporter，但 telemetry 保持关闭。未来接入真实后端不需要改动
任何 operation/session/tool/worktree/task/extension/index owner。

## 所有权

| Owner | 职责 |
| --- | --- |
| `coding-agent` | operation、session、tool 生命周期事实 |
| `workspace-runtime` | managed worktree、background task 生命周期事实 |
| `extension-host` | observe/tool/stop hook dispatch 结果；diagnostic/hook payload 外发前 scrub/budget |
| `code-intelligence` | index service、query、shutdown 生命周期事实 |
| `observability` | subscriber、字段 allowlist、scrubber、预算、telemetry envelope/consent、recent ring、crash writer |
| CLI/Desktop | composition 启动时安装全局 runtime；选择 crash 目录和 telemetry sink/config |

`observability` 不依赖任何第一方 crate。底层 owner 只依赖成熟的 `tracing` crate，不依赖
产品 config、文件 writer 或 telemetry backend。`ai`、`coding-agent` 和 `extension-host`
依赖 `observability` 仅用于同一个外发 scrub/budget contract。

## 结构化 tracing 覆盖

所有 ARC-1010 lifecycle event 使用 target `evo::lifecycle`，只记录 opaque ID、枚举标签、
状态、计数和 duration：

| Domain | 事件 |
| --- | --- |
| operation | admitted execution started；completed/failed；operation/session ID、kind、dispatch、duration |
| session | create/open/open-or-create/non-persistent started；ready/failed；shutdown stopped |
| tool | durable tool call started；completed/failed/persistence-failed |
| worktree | creating、ready、cancelled、failed、discarding、removed、lifecycle transition |
| task | background task started、finished；owner kind/ID、terminal state、timeout presence |
| extension | observe dispatch、tool/stop gate hook terminal outcome、hook count、duration |
| index | service started、query started/completed/failed、service stopped |

subscriber 不转发任意 tracing field。只有 reviewed allowlist 中的 lifecycle/performance 字段会
进入 safe event；`prompt`、`content`、`message`、`arguments`、`input`、`output`、`path`、
`command` 等字段即使被其他调用点记录也会被丢弃。tracing metadata 的 callsite `name` 也不进入
envelope，避免相对源文件路径泄漏。

## Scrub 与大小预算

统一 `SecretsScrubber` 支持：

- 运行时登记的 exact secret，按长度降序替换；
- credential JSON field 和 `key=value` assignment；
- Bearer token 与 `sk-` token；
- telemetry/crash/diagnostic 中的绝对路径、home 路径和相对 path-like token；
- scrub 后再做 UTF-8 安全截断，永远不在 code point 中间切断。

默认预算为单字段 512 bytes、单 telemetry event 8 KiB、recent ring 64 条、单 crash report
64 KiB。超出 event 总预算时按确定顺序删除字段；仍无法满足预算则不调用 sink。crash report
从最旧事件开始丢弃直到满足总预算。

外发路径的具体策略：

- `TelemetrySink::emit` 只接收最终 JSON bytes；没有 raw record API。
- `DiagnosticSink` 前置 scrub，message 512 bytes、code/extension ID 128 bytes、context 最多 32 项。
- hook event 在启动外部进程前执行 structural secret scrub，并强制 256 KiB 总预算；Tool gate
  超预算 fail closed，Observe/Stop 按各自既有安全语义拒绝派发或 fail open。
- `ai` 原 `src/scrub.rs` 和 `coding-agent` 原 `platform/io/redaction.rs` 已删除，不保留双实现或
  compatibility wrapper；`ai::api::resilience` 直接导出新的统一实现。

## Telemetry consent 与 schema

`TelemetryConfig::default()`/`disabled()` 为默认值。只有
`TelemetryConfig::enabled(TelemetryConsent)` 才能开启；安装阶段还会再次校验 consent 存在且
version 非空，缺失时 fail closed。

每个 telemetry envelope 固定包含：

- `schemaVersion = 1`；
- `consent.schemaVersion = 1`；
- bounded `consentVersion`；
- `grantedAtUnixMs`；
- scrubbed/bounded structured event。

CLI/Desktop 当前都传入 `NoopTelemetrySink` 和 disabled config。因此没有用户明确 consent 时，
不会产生 telemetry payload；subscriber 仍维护安全、短小的 recent ring 供 crash report 使用。

## Crash report 隐私和持久化

panic hook 保留并调用原 hook，但 report 只记录：schema version、timestamp、panic payload
kind（`str`/`string`/`opaque`）、bounded thread name、package version 和最近 safe events。

明确不记录：

- panic payload message；
- prompt 或文件内容；
- API key/Bearer/registered secret；
- source location、未脱敏路径或 backtrace。

writer 在持久化前对 recent events 再执行一次 allowlist + scrub + budget，不能依赖 subscriber
已经做过处理。目录先创建并在 Unix 收紧为 `0700`，临时文件以 `0600` create-new，写入后
`sync_all`，再 rename 到最终 JSON 并同步目录。任何写入错误都不替代原 panic 行为。

## API 与依赖图

- workspace 新增成员和依赖：`observability`、`tracing`、`tracing-subscriber`。
- `scripts/architecture/internal-dependencies.tsv` 登记 `ai`、`coding-agent`、`extension-host`、
  CLI、Desktop 到 `observability` 的真实依赖边。
- `scripts/release-api-snapshots.sh` 新增 `observability` API contract。
- `docs/architecture.md` 增加数据流、隐私边界、默认关闭策略和 crash writer contract。

## 验证重点

自动化测试固定了以下不变量：

- telemetry 默认关闭时 sink 收不到数据，recent safe ring 仍工作；
- enabled telemetry 必须有非空 consent，payload 同时包含 telemetry/consent schema；
- exact secret、credential assignment、Bearer、API key、绝对/相对路径在 sink/report 中均消失；
- 未列入 allowlist 的 `prompt`/`content` 字段不会进入 telemetry 或 crash report；
- 字段、event、report 预算均保持 UTF-8/JSON 有效；
- crash writer 不保存 panic message/backtrace/location，超预算时丢弃最旧事件；
- extension diagnostic sink 收到的 record 已 scrub 且 bounded。

实际验证结果：

- `cargo test -p observability --all-targets --quiet`：10 unit + 1 API contract 通过；
- `cargo test -p ai --all-targets --features test-support`：100 unit 通过、1 个付费 live test
  ignored、1 API contract 通过；
- `cargo test -p workspace-runtime --all-targets --quiet`：127 unit + 3 API contract 通过；
- `cargo test -p extension-host --all-targets`：175 unit + 20 hook runner + 26 MCP lifecycle
  通过；
- `cargo test -p code-intelligence --all-targets --quiet`：261 + 24 + 15 + 4，共 304 项通过；
- `cargo test -p coding-agent --all-targets --quiet -- --test-threads=4`：238 unit + 2 API
  contract 通过；
- `cargo test -p cli --all-targets --quiet`：107 项通过；
- `cargo test -p desktop --all-targets --quiet`：304 unit + 11 dependency boundary 通过，
  5 项按既有条件 ignored；
- `cargo clippy --workspace --all-targets -- -D warnings` 与最终受影响 crate clippy 复验通过；
- `cargo check --workspace --all-targets`、`scripts/release-api-snapshots.sh` 通过；
- `scripts/architecture-gate.sh`：`rust_files=862`、`dependency_edges=33`、
  `oversized_debts=1`、`execution_debts=0`；新增 instrumentation 曾使 worktree registry
  超过 900 行，已拆入私有 `registry/lifecycle_trace.rs`，没有新增 debt；
- `cargo fmt --all -- --check`、`git diff --check` 通过。

ARC-1010 完成不代表 Phase 10 Final Gate；ARC-1020 updater/发布和 ARC-1030 最终清算仍待推进。
