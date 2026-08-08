# `ai-protocol`

Evo 的 provider-neutral AI 协议值：模型目录、conversation content、stream event、认证诊断与明确的 provider compatibility metadata。该 crate 不发送网络请求，也不拥有 provider registry。

公开入口全部位于 `ai_protocol::api`。序列化字段是跨 provider 和持久化契约，变更必须同步 protocol contract tests；不添加仅用于兼容旧字段的 alias。

第一方依赖：无。

验证：

```bash
cargo test -p ai-protocol --all-targets
cargo clippy -p ai-protocol --all-targets -- -D warnings
```

