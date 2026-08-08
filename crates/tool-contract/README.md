# `tool-contract`

Evo 的统一工具 vocabulary：`ToolId`、definition、capability/requirement、authorization risk、typed schema、output/progress/error 和 ranking contract。

公开入口位于 `tool_contract::api::{definition,schema,output,ranking}`。本 crate 不执行工具，也不依赖 AI conversation 类型。

第一方依赖：无。

验证：

```bash
cargo test -p tool-contract --all-targets
cargo clippy -p tool-contract --all-targets -- -D warnings
```

