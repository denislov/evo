# Evo 0.7.2 用户迁移说明

本文只记录当前版本仍影响用户的破坏性变化。更早的重构过程、临时 adapter 和已删除路径不再作为可用接口记录。

## 配置

Rust-native `settings.toml` 不再接受旧 TypeScript 客户端字段：

- `transport`
- `npm_command`
- `collapse_changelog`
- `warnings.anthropic_extra_usage`

这些字段会作为 unknown field 报错。请直接删除，不要改名为同义字段。当前 provider transport 设置是：

```toml
http_proxy = "http://127.0.0.1:8080"
websocket_connect_timeout_ms = 10000
```

`websocket_connect_timeout_ms` 是当前内建 provider HTTP client 的 connect timeout；值必须大于 0。

## 内部 Rust API

CLI、Desktop 和其他嵌入宿主只能从 `coding_agent::api::*` 导入产品类型。旧 crate-root symbol、内部 `app` / `runtime` / `services` / `operations` 路径和过渡 facade 已删除，不提供 alias。

Tool 接入必须使用 `tool-contract` + `tool-runtime`；旧 closure 型 `AgentTool`、legacy dispatch、inventory marker 和重复 schema validator 已删除。

`web_fetch` 的参数名固定为 `output_format`；旧的 `format` 参数不再接受。

## Session 与持久化

- Session/event/workspace/preferences/cache 都按各自 schema version 读取。
- 已支持旧版本时，migration 必须先备份并幂等执行；未知的新版本会明确拒绝。
- 当前没有 dual-write 或 migration feature；不要尝试通过 feature flag 打开旧 writer。
- 普通 session 打开是 bounded hydration，不代表完整归档。需要完整历史时使用显式 export API。
- 发现未完成 recovery 时，新的 session write 会被阻止，直到恢复完成或显式处理。

建议升级前备份 Evo 数据目录和项目工作区。若读取失败，请保留原始数据与错误信息，不要手工编辑 journal/outbox/registry 文件。

## Workspace 与并行 Agent

可写 child Agent 默认使用独立 managed worktree；父 workspace 在显式 merge 前不会被直接修改。旧的 shared-cwd 并发写入不再是默认行为。Merge/discard 应通过产品操作执行，不能手工删除 registry 中仍受管理的 worktree。

## 扩展与 MCP

扩展首次启用需要 trust decision。Hook 和 MCP stdio process 受 sandbox/capability policy 限制；平台能力不足时可能明确拒绝运行，不会静默降级为 unrestricted。MCP 工具通过 `mcp_search` / `mcp_use` 发现和调用。

## 更新与安装

首版官方更新资产只支持：

- Linux x86_64 GNU
- Windows x86_64 MSVC

CLI 与 Desktop 使用独立资产。安装脚本和内置 updater 都要求 GitHub Release 的 `checksums.txt`，SHA-256 校验成功后才安装。CLI 启动检查只提示；只有显式 `coding-agent update` 或 Desktop 确认操作才会下载和切换。

macOS、ARM、crates.io、系统包管理器和企业分发目前不属于官方 updater 支持范围。
