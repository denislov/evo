# `desktop`

Evo 的 GPUI 原生桌面适配器。它拥有窗口生命周期、application reducer、runtime worker/client、conversation/session/inspector UI、deterministic replay、视觉 fixture 和确认式 updater UI。

Desktop 只能通过 `coding_agent::api` 消费产品 projection，不解析 raw diff，也不自行推导 operation terminal。

第一方依赖：`coding-agent`、`observability`、`release-updater`。

验证：

```bash
cargo check -p desktop --all-targets
cargo test -p desktop --all-targets
cargo clippy -p desktop --all-targets -- -D warnings
```

