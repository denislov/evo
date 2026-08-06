# Phase 6 / ARC-630：Provider resilience

> 状态：完成
> 前序：Phase 5 Gate（session actor、context compaction、prompt queue）
> 目标：provider 故障不再引发无限重试风暴 —— circuit breaker 切断发往故障
> provider 的请求；401 凭据过期单次 refresh/retry；统一 transport builder 支持
> extra CA；所有外发错误消息经 secrets scrubber 脱敏。
> Phase 6 Gate 判定项：provider failure 不引发无限重试风暴（本 ARC 覆盖）；
> 长任务可查询和取消、shell OS policy、web fetch SSRF 由其他 ARC 覆盖。

## 决策

- **breaker 按 `(provider, api_name)` 分 key，注册表懒创建**：`AiClient` 持
  有共享 `CircuitBreakerRegistry`（`Arc` + `Mutex<HashMap>`），一次调用拿到的
  breaker 实例自带 key，跨请求共享状态；不同 provider/endpoint 互不影响，
  一个故障源不会熔断其他源。`CircuitBreaker` 内部 `Mutex` 短临界区保护
  状态机，满足 lazy async-stream 的多并发访问。
- **滑动窗口满 `window_size` 个样本才评估失败率**（失败数 × 100 / 窗口 ≥
  阈值即 open）：窗口未满不触发，行为确定、可测；满后每记录一个结果挤出
  最旧样本，失败率回落会自动保持 Closed。
- **failure 只记「可重试类」失败**：网络错误、超时、retryable status
  （408/409/429/5xx）；401/403/404 这类配置或权限错误不记入，避免 breaker
  被错误配置打开。`before_request` 返回 `Reject` 时直接 yield
  `ProviderErrorKind::CircuitOpen` 错误事件，不构建、不发送请求，并复用
  既有 retry 上限，风暴在源头终止。
- **HalfOpen 放行 `half_open_max_probes` 个并发 probe**：probe 成功 → 立即
  Closed（窗口重置）；失败 → 重新 Open 并重置 `opened_at`，等待完整
  `open_duration_ms` 后才再探。
- **clock 可注入**：`Clock` trait + `SystemClock`，测试用 `MockClock`
  （原子计数）推进时间，不依赖真实 sleep。
- **401 单次 refresh 走 `credential_refresh_slot` 槽位**：provider 的
  request builder 闭包每次构建时从槽位读当前凭据；401 时发送路径调用一次
  refresh 回调（重新 `resolve_model_auth` 快照 + 覆盖应用），槽位更新后
  原地重发，不消耗 `max_retries` 预算；仍 401 则原样失败。refresh 只在
  opts 携带「自动凭据」诊断（`options_contain_automatic_credentials`）时
  装配，显式 `api_key` 永不被覆盖（registry 层 + 发送路径双保险）。
- **`ApiProvider` 增加带默认实现的 `stream_with_resilience`**：默认转发
  `stream`，外部自定义 provider 零改动；7 个 builtin provider 全部接线，
  从 `AiClient::stream_model` → registry → provider → `send_json_stream_with_resilience`
  全链路贯通。`send_json_stream` / `send_json_stream_with_request_factory`
  签名不变（内部转发默认 `SendResilience`）。
- **extra CA 走统一 builder**：`TransportConfig::extra_ca_certificates`
  （PEM 字节）+ `with_extra_ca`，`authenticated_client` 是唯一 HTTP client
  构造点（`with_registry` 分支重构为复用同一入口）；无效 PEM 在 try 构造时
  报结构化错误，与 `connect_timeout=0` 校验一致。
- **scrubber 分层脱敏**：`SecretsScrubber` 先做精确替换（secret 按长度降序，
  短于 8 字符忽略防误伤），再做结构性模式（JSON 键值、`key=value`、Bearer、
  `sk-` token），先结构后 token 避免重复脱敏；`SecretStore` 并发收集
  `AiClient` 每次调用的自动凭据，`http.rs` 的错误事件出口统一过
  `scrub_error_message`。日志、telemetry、crash report、hook payload 由各
  消费方接入公开 `api::resilience` API（本 ARC 保证 ai crate 内部外发数据
  脱敏 + 模块可复用）。

## 落点

| 变更 | 位置 |
| --- | --- |
| circuit breaker 状态机 + registry + Clock | `crates/ai/src/transport/circuit_breaker.rs`（新增） |
| secrets scrubber + SecretStore | `crates/ai/src/scrub.rs`（新增） |
| 发送路径：breaker/401 refresh/scrub | `crates/ai/src/transport/http.rs` |
| CircuitOpen 错误类型 | `crates/ai/src/transport/error.rs` |
| extra CA + 统一 builder | `crates/ai/src/transport/client.rs` |
| 401 refresh 装配 + trait 扩展 | `crates/ai/src/registry/provider.rs` |
| 覆盖式 apply + 自动凭据判定 | `crates/ai/src/registry/auth.rs` |
| AiClient 持 breaker registry + SecretStore | `crates/ai/src/client.rs` |
| 7 个 builtin provider 接线 | `crates/ai/src/providers/*/mod.rs` |
| 公开 facade | `crates/ai/src/api.rs`（`api::resilience`） |
| 测试（breaker transition table、401 refresh、CA、scrub） | `circuit_breaker.rs`、`http_tests.rs`、`registry/provider_tests.rs`、`client.rs`（新增/内嵌） |
| CA fixture | `crates/ai/src/transport/fixtures/test-ca.pem`（新增，自签无密钥） |

## 验证

```text
cargo check --workspace --all-features
通过（ai / agent-core / coding-agent / desktop / cli 全编译）

cargo test --locked -p ai --all-features
70 passed + 1 doc/example passed
其中 ARC-630 新增 32 项：
- circuit breaker 16 项：config 校验、窗口未满不 open、阈值百分比、滑动窗口挤出、
  Closed->Open / Open->HalfOpen / HalfOpen->Closed / HalfOpen->Open 全转换、
  open 期间 Reject 与 retry_after_ms、时钟推进后放行、half-open probe 并发上限、
  key 隔离（registry 按 (provider, api) 分桶）、共享 MockClock
- 401 refresh 4 项（tokio + mock HTTP server）：401->refresh->200 成功且两次
  请求凭据不同、refresh 后仍 401 单次失败、显式 api_key 零 refresh、refresh
  无新凭据不轮换
- breaker 发送路径集成 1 项：open 时请求不构建不发送，yield CircuitOpen
- provider 端到端 1 项：DeepSeekResponsesProvider 401->refresh->真实 SSE Done
- registry 装配 2 项：自动凭据得到 refresh 闭包、显式 key 无 refresh 且不被覆盖
- transport 4 项：无效 PEM 结构化报错、有效 CA 构建 client、空 bundle 归一化、
  scrub_error_message 脱敏
- scrub 12 项：精确替换/长优先/中文上下文/短 secret 忽略/sk-token/Bearer/
  JSON 键值/赋值键/幂等/无匹配原样/空输入/防重复脱敏

cargo clippy -p ai --all-targets --all-features -- -D warnings
通过（0 warnings）

scripts/architecture-gate.sh
architecture_gate rust_files=641 dependency_edges=17 oversized_debts=35 execution_debts=0

cargo fmt --all -- --check
通过
```

## 后续

- 日志、telemetry、crash report 与 hook payload 的 scrubber 接入由各消费方
  （coding-agent / desktop / extension-host）通过 `api::resilience` 完成，
  不在本 ARC 内。
- 401 refresh 当前只覆盖 api_key 形态凭据；headers 形态的凭据轮换走同一
  槽位机制，但 builtin provider 均无此形态，未单测。
- ARC-720 MCP provider 的 OAuth 401 refresh 复用本 ARC 的 auth seam。
