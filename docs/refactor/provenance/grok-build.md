# grok-build provenance

- Status: evaluated; no production code copied by Phase 0.
- Local source: `third-party/grok-build`.
- Checkout revision at architecture study: `ed6d543643628663873c5de28298e022ed634238`.
- Recorded upstream `SOURCE_REV`: `d6937fe255dce4133c3d000a50f9cb94de12f06f`.
- Primary license: Apache-2.0; verify each selected module before copying.
- Primary notice sources: `third-party/grok-build/THIRD-PARTY-NOTICES` and crate-local notice files.
- Planned destinations and adaptation mode: see `docs/Evo完整架构重构计划.md` section 5.
- Sync policy: selective one-time adaptation; Evo does not track Grok aggregation crates wholesale.

每个实际移植任务必须在本文件追加源 crate、具体路径、测试、目标路径和本地修改，不能只引用本总记录。

---

## ARC-700 `extension-host` 抽取（2026-08-06）

Status: adapted（逐文件小步改写，未整文件复制；参考均标注 `Adapted from xai-grok-hooks` 来源注释）
Upstream repository: https://github.com/bytecodealliance/xai-grok（vendored at `third-party/grok-build`）
Upstream revision: `d6937fe255dce4133c3d000a50f9cb94de12f06f`
Source paths: `third-party/grok-build/crates/codegen/xai-grok-hooks/src/`
  - `lib.rs`（模块组织与 crate 文档风格）
  - `config.rs`（config layer 容错与「坏层跳过其余照常」模式、TOML/JSON 双路径）
  - `discovery.rs`（目录缺失为空的容错、坏文件继续扫描、稳定排序、dedup 思路）
  - `trust.rs`（folder trust 单一权威设计原则；Evo 不复制 legacy 迁移代码）
  - `error.rs`（结构化 thiserror 错误携带 path/name 模式）
  - `event.rs`（envelope 元数据字段集与 payload 判别思路；别名解析思想）
  - `matcher.rs` / `runner/mod.rs`（仅阅读作为 ARC-710 参考，本 ARC 未移植）
License/notices: Apache-2.0（`third-party/grok-build/THIRD-PARTY-NOTICES`）
Destination paths: `crates/extension-host/src/{lib,api,error,event,config,discovery,trust,budget,diagnostic}.rs`、`crates/extension-host/src/host/{mod,tests_host}.rs`；coding-agent 端口 `crates/coding-agent/src/services/ports.rs`（仅参考设计，无代码复制）
Tests carried over: 无直接复制；按 Evo 语义重写 —— DTO golden/round-trip、向后兼容 default、config merge 优先级与冲突、discovery 容错、trust 边界、lifecycle/shutdown/panic/budget
Local modifications:
  - 事件 DTO 版本化（Grok 无 version）；payload 改 internally-tagged（Grok untagged 且仅 Serialize）
  - 事件业务字段按 Evo 重设计，不照抄事件全集
  - discovery 从「目录内散落 JSON」改为「每扩展一个目录 + extension.json manifest」
  - trust 从 legacy 迁移辅助改为 TrustStore 抽象 + 三态判定 + EnableRequest 首次启用 DTO
  - budget 从 per-hook timeout 改为 per-extension 多维 per-session 预算
  - 新增 host 生命周期（discovery/config/trust/lifecycle/budget/diagnostics/shutdown）——
    Grok 无 host 概念，属 Evo 独立设计（参考了 `xai-grok-config` 的 layer 思想）
  - runner/matcher 未移植（ARC-710）
Sync policy: 不跟随上游（一次性适配）；后续 ARC 若再参考 `matcher.rs`/`runner/*` 需单独登记。

---

## ARC-710 User hooks（2026-08-07）

Status: adapted（小步改写 + 重新设计，未整文件复制；参考文件均标注 `Adapted from xai-grok-hooks` 来源注释）
Upstream repository: https://github.com/bytecodealliance/xai-grok（vendored at `third-party/grok-build`）
Upstream revision: `d6937fe255dce4133c3d000a50f9cb94de12f06f`
Source paths: `third-party/grok-build/crates/codegen/xai-grok-hooks/src/`
  - `matcher.rs`（simple-vs-regex 精确/正则语义、pipe 列表防锚定错误、缺省=通配）
  - `runner/command.rs`（shell 元字符路由、timeout 终止进程树、输出截断上限 64KB、
    exit-code 阶梯、JSON 决策优先于退出码、deny/block 默认 reason 文案）
  - `dispatcher.rs`（首个 deny 短路、Stop 信号聚合与 first force-stop wins、
    fail-open 语义与理由）
  - `event.rs`（信封元数据、SubagentStopPhase、payload 判别思想、`MAX_PAYLOAD_SIZE` 截断）
  - `runner/http.rs`：仅阅读评估；HTTP runner 未移植（SSRF 安全 fetch 管线在 Evo `ai`
    crate 内部，下沉共享 HTTP 层为 Phase 8+ 工作，登记为验证债务）
License/notices: Apache-2.0（`third-party/grok-build/THIRD-PARTY-NOTICES`）
Destination paths:
  - `crates/extension-host/src/{matcher,hook,runner,dispatcher}.rs`（新模块）
  - `crates/extension-host/src/{event,discovery,host/mod,budget,api}.rs`（扩展）
  - `crates/extension-host/src/runner/tests_interpret.rs`、
    `crates/extension-host/src/dispatcher/tests_dispatcher.rs`、
    `crates/extension-host/tests/hook_runner.rs`（测试）
  - coding-agent 接线：`crates/coding-agent/src/services/ports.rs`（LiveExtensionHostPort /
    ExtensionEventSink / ExtensionHostService 扩展）、`session/view.rs`、
    `runtime/facade/{lifecycle,connection}.rs`、`runtime/owners.rs`、
    `application/operation/dispatch.rs`、`operations/{prompt/mod.rs,prompt/context.rs,
    prompt/context/setup.rs,merge/runner.rs}`、`services/{runtime.rs,authorization.rs}`、
    `runtime/facade/hooks_tests.rs`（测试）
Tests carried over: 无直接复制；按 Evo 语义重写 —— matcher 四维条件、
priority 排序、gate transition-table（Tool/Stop 各 9+ 行）、runner 超时/取消/洪泛/
崩溃/退出码/sandbox 注入、host shutdown 在途 hook 取消、coding-agent 适配器集成
（session 生命周期事件、gate 决策暴露、首次启用展示、permission 事件）
Local modifications:
  - 事件通道：Grok stdin 注入 → Evo 环境变量（`ProcessSpec` 固定 stdin=null）
  - matcher 新增 path（前缀语义）与 profile 维度；新增 `priority` 确定排序
    （Grok 无优先级，按配置顺序）
  - runner 强制 `SandboxProfile` + `SandboxCapability` 探测：sandbox 失败是唯一
    fail-closed 类别（Grok 全部 fail-open，且无沙箱概念）
  - gate 拆 host 通道（Observe 派发）与 `HookGate`（产品同步调用），避免事件双跑
  - 结构化结果枚举（Success/ToolDecision/StopSignals/TimedOut/Cancelled/
    OutputLimited/Failed/SpawnFailed/SandboxUnsupported）替代 Grok 的字符串失败
  - dispatch 事件逐条独立 task + panic fail-closed + shutdown 取消在途 hook
    （Grok 无 host 生命周期）
  - 无 HTTP runner；`StopSignals` 聚合三信号；JSON 决策协议保留但简化字段集
Sync policy: 不跟随上游（一次性适配）；`runner/http.rs` 若后续移植需单独登记。
