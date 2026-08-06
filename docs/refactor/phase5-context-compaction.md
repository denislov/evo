# Phase 5 / ARC-520：Context/compaction 策略

> 状态：完成（2026-08-06）
> 前序：ARC-500（Agent actor 化）、ARC-510（prompt queue）
> 目标：引入统一 token estimation（模型 override）、compaction 切点禁止落在 tool pair 中间、verbatim→fitted→lossy 失败降级、专用 sampler seam

## 当前架构

### Token estimation（`compaction/estimate.rs`）

- `estimate_text_tokens(text)`：`text.len().div_ceil(4)`，硬编码 4 bytes/token
- `estimate_tokens(messages)`：遍历 messages，`saturating_add` 各消息估算
- `estimate_context_tokens(messages)`：优先用最后一个有效 assistant usage 作为 anchor
- 已有饱和运算 ✅，已有明确 rounding（`div_ceil`）✅
- **缺少模型级 override**：bytes_per_token 硬编码为 4

### Compaction trigger & prepare（`compaction/prepare.rs`）

- `should_compact(estimated_tokens, context_window, settings)`：`estimated_tokens > context_window - reserve_tokens`
- `prepare_compaction(messages, settings)`：从尾部向前遍历，保留 `keep_recent_tokens` 内的消息
- **已有 trailing tool-result 保护**：尾部 ToolResult 被跳过以找到对应的 Assistant
- **缺少中间 tool pair 保护**：切点可能拆开 `Assistant(tool_call)` 在 keep、`ToolResult` 在 to_summarize

### Compaction summarize（`compaction/summarize.rs`）

- `summarize_with_provider_streamer(model, messages, ...)`：使用 agent 的 model 生成摘要
- `max_tokens = Some(4096)`（已有大小限制 ✅）
- **缺少专用 sampler seam**：不能配置不同的 model/参数
- **缺少失败降级**：summarization 失败直接返回 error

### Turn engine compaction（`nodes.rs:262`）

- `maybe_compact_runtime_context`：调用 `estimate_context_tokens` -> `should_compact` -> `prepare_compaction` -> `summarize_with_provider_streamer`
- `split_for_compaction_after_usage_anchor`：当 `prepare_compaction` 无 to_summarize 时，用 usage anchor 做 fallback split

### CompactionSettings（`agent-core/src/agent/types/config.rs`）

```rust
pub struct CompactionSettings {
    pub enabled: bool,
    pub reserve_tokens: u32,
    pub keep_recent_tokens: u32,
}
```

### Event sourcing ✅

compaction 结果作为 `CompactionSummary` message 和 `SessionCompacted` event。

## 目标改动

### 1. 模型级 token override

引入 `TokenEstimationConfig`：

```rust
#[derive(Debug, Clone, Copy)]
pub struct TokenEstimationConfig {
    pub bytes_per_token: u32,
}

impl Default for TokenEstimationConfig {
    fn default() -> Self { Self { bytes_per_token: 4 } }
}
```

`CompactionSettings` 增加 `token_estimation: TokenEstimationConfig`。

`estimate_text_tokens`、`estimate_tokens`、`estimate_context_tokens` 接收 `bytes_per_token: u32` 参数（或 `&TokenEstimationConfig`）。所有调用方适配。

### 2. Compaction 切点 tool pair 保护

在 `prepare_compaction` 中，确定切点后检查 `keep_recent` 开头是否有孤立的 `ToolResult`（对应的 `Assistant(tool_call)` 不在 keep_recent 中）。如果有，把它们移到 `to_summarize`。

```rust
// After the main loop, before returning:
while let Some(first) = keep_recent.first() {
    if matches!(first, AgentMessage::ToolResult { .. }) {
        // Check if the corresponding Assistant is in keep_recent
        let has_assistant = keep_recent.iter().any(|m|
            matches!(m, AgentMessage::Assistant { .. })
        );
        if !has_assistant {
            let orphan = keep_recent.remove(0);
            to_summarize.push(orphan);
        } else {
            break;
        }
    } else {
        break;
    }
}
```

### 3. 失败降级 ladder（verbatim → fitted → lossy）

在 `maybe_compact_runtime_context` 中：

1. **Lossy**（当前已有）：`summarize_with_provider_streamer` 生成摘要
2. **Fitted**（新增降级）：summarization 失败时，不 summarize，直接截断到 `keep_recent`，插入一个标注截断的 `CompactionSummary`（summary 内容为 "Compaction failed: history truncated without summary"）
3. **Verbatim**（不触发）：如果 `keep_recent` 本身就在窗口内，不需要 compaction

```rust
let summary = match summarize_with_provider_streamer(...).await {
    Ok(summary) => summary,
    Err(error) => {
        // Fitted fallback: truncate without summary
        format!("Compaction fallback: history truncated ({})", error)
    }
};
```

### 4. 专用 sampler seam

`CompactionSettings` 增加 `sampler: Option<CompactionSampler>`：

```rust
#[derive(Debug, Clone)]
pub struct CompactionSampler {
    pub model: Option<Model>,
    pub max_tokens: Option<u32>,
}
```

`summarize_with_provider_streamer` 接收 `sampler: Option<&CompactionSampler>` 参数：
- `sampler.model` 优先于 `model` 参数
- `sampler.max_tokens` 优先于硬编码的 4096

### 5. 统一有界格式

`CompactionSettings` 增加 `summary_max_chars: usize`（默认 8192）。summarize 完成后截断 summary 到 `summary_max_chars`。

## 关键决策

1. **`TokenEstimationConfig` 放在 `CompactionSettings` 中**：不修改 `Model` 类型，只在 compaction 相关路径使用。`estimate_tokens` 的非 compaction 调用方（如 `coding-agent/operations/compaction/runner.rs`）使用 `TokenEstimationConfig::default()`。

2. **tool pair 保护是 prepare_compaction 的后处理**：不改变主遍历逻辑，只在确定切点后调整。

3. **fitted 降级不丢弃消息**：keep_recent 中的消息保留，只缺少摘要。标注 fallback 原因。

4. **sampler seam 是可选的**：`None` 时使用当前行为（agent model + 4096 max_tokens）。

5. **不改变 event sourcing**：compaction 结果仍作为 `CompactionSummary` message 和 `SessionCompacted` event。

## 分步实现

### 步骤 1：TokenEstimationConfig
- 定义 `TokenEstimationConfig`
- `estimate_text_tokens`、`estimate_tokens`、`estimate_context_tokens` 接收 `bytes_per_token`
- `CompactionSettings` 增加 `token_estimation` 字段
- 适配所有调用方

### 步骤 2：tool pair 保护
- `prepare_compaction` 增加 keep_recent 开头孤立 ToolResult 检查
- 新增测试

### 步骤 3：失败降级
- `maybe_compact_runtime_context` 中 summarization 失败时降级到 fitted
- 新增测试

### 步骤 4：Sampler seam
- 定义 `CompactionSampler`
- `CompactionSettings` 增加 `sampler` 字段
- `summarize_with_provider_streamer` 接收 sampler 参数
- `maybe_compact_runtime_context` 传入 sampler

### 步骤 5：统一有界格式
- `CompactionSettings` 增加 `summary_max_chars`
- summarize 完成后截断

### 步骤 6：测试
- tool pair 不被拆开（中间和尾部）
- fitted 降级（summarization 失败后仍能完成 compaction）
- sampler override（不同 model/max_tokens）
- summary 截断
- bytes_per_token override

## 验证

```text
cargo test --locked -p agent-core --all-features
cargo test --locked -p coding-agent --all-features
cargo clippy --workspace --all-targets --all-features -- -D warnings
bash scripts/architecture-gate.sh
```
