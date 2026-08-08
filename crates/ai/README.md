# `ai`

Evo 的 AI provider 与网络传输实现。它负责 provider registry、认证解析、SSE、retry/circuit breaker、extra CA、secrets scrub 接线和 SSRF-safe web fetch；provider-neutral DTO 位于 `ai-protocol`。

公开入口位于 `ai::api::{model,client,auth,provider,error,transport,resilience,fetch}`。

第一方依赖：`ai-protocol`、`observability`。

验证：

```bash
cargo test -p ai --all-targets
cargo clippy -p ai --all-targets -- -D warnings
```

