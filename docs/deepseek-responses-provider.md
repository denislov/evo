# DeepSeek Responses Provider

本文记录 `ai` crate 中 DeepSeek Responses provider 的实现边界、验证方式和已知外部限制。实现依据为 DeepSeek 官方 Responses API 文档，并在 2026-08-02 使用真实 `deepseek-v4-flash` 请求校验。

## 接入方式

模型目录中的 `deepseek/deepseek-v4-flash` 使用独立 API 标识 `deepseek-responses`，请求端点为：

```text
https://api.deepseek.com/v1/responses
```

运行时认证仍通过 provider auth resolver 注入。真实契约测试不会直接解析配置文件，也不会把密钥写入仓库；调用者应从自己的凭据存储中读取密钥并仅通过进程环境传入：

```bash
DEEPSEEK_LIVE_API_KEY='…' \
  cargo test -p ai providers::deepseek::tests::live_reasoning_stream_matches_provider_contract \
  -- --ignored --exact --nocapture
```

该测试会产生真实 API 请求和费用，默认标记为 `ignored`。仓库中的 `fixtures/*.sse` 来自真实响应，已经替换响应 ID、缩短自然语言内容，并确认不包含认证信息。

## 已实现能力

- 独立 `DeepSeekResponsesProvider`、认证、URL 解析、HTTP/SSE 传输和 provider identity。
- DeepSeek V4 Flash 的 text input/output、system instructions 和最大输出 token。
- reasoning effort：`none`、`low`、`high`、`max`；通用 thinking level 按模型目录映射。
- reasoning item ID 的结构化保存、session 持久化和无状态多轮回传；兼容初版 provider 写入 UUID `thinking_signature` 的历史 session。
- function tool、`web_search` 和 DeepSeek 当前唯一支持的 custom tool `apply_patch`。
- function/custom tool output 回传，以及 web-search provider item 原样回传。
- `temperature`、`top_p`、`top_logprobs`、`tool_choice`、`text.format` 和 `user` 的序列化与本地范围校验。
- response model、response ID、cached token、reasoning token、总 token 和成本统计。
- completed、failed、cancelled、max-output-token incomplete 及未知显著事件的严格终态处理。
- provider-neutral 的 web-search lifecycle 事件；coding-agent 将其映射为 tool started/updated/completed 事件。

Responses SSE 解析器已提升到 `providers/responses/`，供 OpenAI Responses、OpenAI Codex Responses 和 DeepSeek 共用。provider 特有的请求转换仍分别维护，避免把 DeepSeek 限制伪装成 OpenAI compatibility 选项。

## 非流式调用

`ApiProvider` 的传输契约统一为事件流。只需要完整结果的调用方使用：

```rust,ignore
let message = client.complete_model(&model, context, options).await?;
```

`complete_model` 收集同一条经过验证的事件流并返回 terminal `AssistantMessage`。这里有意不维护第二套非流式 HTTP 请求、wire model 和解析状态机，因此不是待办债务。

## DeepSeek 协议限制

以下限制会在发送 HTTP 请求前尽量明确报错：

- Responses API 是无状态协议，不支持 `session_id`、`previous_response_id` 或 conversation state；续轮由调用方回传完整 input items。
- context cache 自动启用，不接受 `prompt_cache_key` 或 cache-retention 控制。
- 当前不支持图片输入。
- thinking 模式不发送 `temperature`/`top_p`，并只接受 `tool_choice = auto | none` 或省略。
- custom tool 当前只接受 `apply_patch`。
- provider item 只允许由同一个 `deepseek-responses` provider 产生的 `web_search_call`，避免跨 provider 误回放。

DeepSeek 文档还明确不支持 `store`、`background`、`metadata`、`include`、`prompt`、`truncation`、`service_tier`、`safety_identifier`、context management 和 `stream_options`。这些字段当前没有暴露为 DeepSeek 选项，不能被静默忽略。

## 模型目录再生成

`crates/ai/tools/model_overrides.json` 是仓库内的 canonical override。模型生成脚本每次读取上游目录后都会自动应用它，并要求每条 override 恰好命中一个模型；回归测试同时验证生成后的 bundled catalog 没有漂移。

因此上游目录再生成不会把 Flash 恢复到旧的 `openai-completions` 路由。`deepseek-v4-pro` 暂不切换：截至 2026-08-02，真实 API 返回明确的产品门禁错误，说明 Pro 的 Codex/Responses 集成尚未开放。这是上游能力门禁，不是本地实现债务；开放后需要新增真实 fixture 和契约测试再调整目录。

## 验证矩阵

离线回归覆盖：

- reasoning + function call 的事件顺序、结构化 metadata 和 usage；
- custom `apply_patch` 的 raw string delta/done；
- web search 的 in-progress/searching/completed lifecycle 和 provider item 回放；
- OpenAI Responses 共享解析器的 reasoning/encrypted-content 回放；
- 历史 session schema 和初版 DeepSeek UUID metadata 迁移；
- 不支持选项、跨 provider item、缺失 reasoning item ID 和未知终态的 fail-closed 行为；
- 模型目录 override 一致性。

真实 API 已人工/自动验证 Flash 的普通文本、reasoning、function tool、custom apply-patch、web search 及多轮 item 回传；提交前应再次运行 ignored contract test。
