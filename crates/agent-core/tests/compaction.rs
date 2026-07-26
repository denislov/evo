mod common;

use agent_core::api::agent::{
    Agent, AgentEvent, AgentMessage, CompactionConfig, CompactionSettings,
};
use agent_core::api::compaction::{estimate_tokens, prepare_compaction, should_compact};
use ai::api::conversation::{AssistantMessage, ContentBlock, StopReason};
use ai::api::stream::StreamOptions;
use ai::api::testing::FauxProvider;
use common::{ProviderGuard, faux_model_with_window};
use futures::StreamExt;
use std::sync::Arc;

fn user_msg(text: &str) -> AgentMessage {
    AgentMessage::UserText {
        message_id: "u".into(),
        text: text.into(),
    }
}

fn compaction_msg(summary: &str, tokens: u32) -> AgentMessage {
    AgentMessage::CompactionSummary {
        message_id: "cs".into(),
        summary: summary.into(),
        tokens_before: tokens,
    }
}

/// Build an assistant message carrying provider `usage.total_tokens`, used to
/// anchor `estimate_context_tokens` in runtime compaction tests.
fn assistant_with_usage(total_tokens: u32) -> AgentMessage {
    let mut msg = AssistantMessage::empty("test", "test-model");
    msg.usage.total_tokens = total_tokens;
    msg.stop_reason = StopReason::Stop;
    AgentMessage::Assistant {
        message_id: "a".into(),
        message: msg,
    }
}

#[test]
fn estimate_text_tokens() {
    let msgs = vec![user_msg("hello world this is about twenty five")];
    let tokens = estimate_tokens(&msgs);
    assert!(tokens > 5, "should be >5 tokens, got {}", tokens);
}

#[test]
fn estimate_tokens_ignores_assistant_usage_and_uses_content_size() {
    let mut assistant = AssistantMessage::empty("test", "test-model");
    assistant.usage.total_tokens = 30_000;
    assistant.content.push(ContentBlock::Text {
        text: "assistant content".into(),
        text_signature: None,
    });
    let msgs = vec![
        user_msg(&"old context ".repeat(100)),
        AgentMessage::Assistant {
            message_id: "a".into(),
            message: assistant,
        },
    ];

    let without_usage = estimate_tokens(&msgs);
    assert!(
        without_usage < 30_000,
        "message sizing must not use provider total_tokens: {without_usage}"
    );
    assert!(without_usage > 0);
}

#[test]
fn should_compact_applies_threshold() {
    let settings = |reserve_tokens: u32| CompactionSettings {
        enabled: true,
        reserve_tokens,
        keep_recent_tokens: 0,
    };
    assert!(should_compact(95_000, 100_000, &settings(10_000)));
    assert!(!should_compact(89_000, 100_000, &settings(10_000)));
    // Degenerate zero context window never compacts (Rust safety guard).
    assert!(!should_compact(100, 0, &settings(0)));
}

#[test]
fn prepare_compaction_keeps_recent_messages() {
    let msgs = vec![
        user_msg("first message"),
        user_msg("second message"),
        user_msg("third message"),
        user_msg("fourth message"),
    ];
    let settings = CompactionSettings {
        enabled: true,
        reserve_tokens: 0,
        keep_recent_tokens: 5,
    };
    let (to_summarize, keep) = prepare_compaction(&msgs, &settings);
    assert!(!keep.is_empty(), "should keep some recent messages");
    assert!(
        to_summarize.len() + keep.len() == msgs.len(),
        "all messages accounted for: {} + {} != {}",
        to_summarize.len(),
        keep.len(),
        msgs.len()
    );
}

#[test]
fn prepare_compaction_no_split_for_small_conversation() {
    let msgs = vec![user_msg("hi")];
    let settings = CompactionSettings {
        enabled: true,
        reserve_tokens: 16_384,
        keep_recent_tokens: 20_000,
    };
    let (to_summarize, keep) = prepare_compaction(&msgs, &settings);
    assert!(to_summarize.is_empty());
    assert_eq!(keep.len(), 1);
}

#[test]
fn prepare_compaction_handles_compaction_summary() {
    let msgs = vec![
        compaction_msg("previous summary", 100),
        user_msg("hello after compaction"),
    ];
    let settings = CompactionSettings {
        enabled: true,
        reserve_tokens: 0,
        keep_recent_tokens: 10,
    };
    let (_to_summarize, keep) = prepare_compaction(&msgs, &settings);
    // Should handle gracefully
    assert!(!keep.is_empty());
}

#[test]
fn empty_messages_no_split() {
    let settings = CompactionSettings::default();
    let (to_summarize, keep) = prepare_compaction(&[], &settings);
    assert!(to_summarize.is_empty());
    assert!(keep.is_empty());
}

#[tokio::test]
async fn runtime_compaction_summarizes_before_provider_request() {
    let api = "runtime-compaction";
    let _provider_guard = ProviderGuard::register(
        api,
        Arc::new(FauxProvider::with_call_queue(vec![
            FauxProvider::text_call("summary of old context", StopReason::Stop),
            FauxProvider::text_call("final answer", StopReason::Stop),
        ])),
    );
    // Small context window so the context-window-gated trigger fires for
    // this fixture-sized conversation (default `faux_model` has
    // `context_window: 0`, which never auto-compacts).
    let mut config = _provider_guard.agent_config(faux_model_with_window(api, 100));
    config.max_turns = Some(3);
    config.compaction = Some(CompactionConfig {
        settings: CompactionSettings {
            enabled: true,
            reserve_tokens: 0,
            keep_recent_tokens: 8,
        },
        custom_instructions: None,
    });

    let agent = Agent::new(config);
    agent.add_message(user_msg(&"old context ".repeat(40)));
    agent.add_message(user_msg(&"more old context ".repeat(40)));

    let events: Vec<_> = agent.prompt("new prompt").collect().await;

    assert!(events.iter().any(|event| matches!(
        event,
        AgentEvent::SessionCompacted { summary, .. } if summary == "summary of old context"
    )));
    assert!(events.iter().any(|event| matches!(
        event,
        AgentEvent::AgentDone { message }
            if message.content.iter().any(|block| matches!(
                block,
                ContentBlock::Text { text, .. } if text == "final answer"
            ))
    )));
    assert!(agent.messages().iter().any(|message| matches!(
        message,
        AgentMessage::CompactionSummary { summary, .. } if summary == "summary of old context"
    )));
}

#[tokio::test]
async fn runtime_compaction_forwards_stream_options_to_summarization() {
    let api = "runtime-compaction-api-key";
    let provider = Arc::new(common::TestProvider::new(vec![
        common::text_turn("summary with key"),
        common::text_turn("final answer with key"),
    ]));
    let _provider_guard = ProviderGuard::register(api, provider.clone());
    // Small context window so the context-window-gated trigger fires.
    let mut config = _provider_guard.agent_config(faux_model_with_window(api, 100));
    config.max_turns = Some(3);
    config.stream_options = Some(StreamOptions {
        api_key: Some("test-api-key".to_string()),
        max_retries: Some(2),
        ..Default::default()
    });
    config.compaction = Some(CompactionConfig {
        settings: CompactionSettings {
            enabled: true,
            reserve_tokens: 0,
            keep_recent_tokens: 8,
        },
        custom_instructions: None,
    });

    let agent = Agent::new(config);
    agent.add_message(user_msg(&"old context ".repeat(40)));
    agent.add_message(user_msg(&"more old context ".repeat(40)));

    let events: Vec<_> = agent.prompt("new prompt").collect().await;

    assert!(events.iter().any(|event| matches!(
        event,
        AgentEvent::SessionCompacted { summary, .. } if summary == "summary with key"
    )));
    let recorded_options = provider.stream_options.lock().unwrap();
    let keys = recorded_options
        .iter()
        .map(|opts| opts.as_ref().and_then(|opts| opts.api_key.clone()))
        .collect::<Vec<_>>();
    assert_eq!(
        keys,
        vec![
            Some("test-api-key".to_string()),
            Some("test-api-key".to_string())
        ]
    );
    let retries = recorded_options
        .iter()
        .map(|opts| opts.as_ref().and_then(|opts| opts.max_retries))
        .collect::<Vec<_>>();
    assert_eq!(retries, vec![Some(2), Some(2)]);
}

// ---- Context-window-gated trigger (TS parity) ----

#[tokio::test]
async fn runtime_compaction_does_not_trigger_on_large_context_model() {
    // deepseek-v4-flash-style: 1M context window. ~50k estimated tokens is
    // far below the 1,000,000 - 16,384 = 983,616 trigger threshold, so auto
    // compaction must NOT fire. Regression for the "compacts at ~1% usage"
    // bug where the trigger ignored the active model's context window.
    let api = "runtime-no-compact-large";
    let _provider_guard = ProviderGuard::register(
        api,
        Arc::new(FauxProvider::with_call_queue(vec![
            FauxProvider::text_call("final answer", StopReason::Stop),
        ])),
    );
    let mut config = _provider_guard.agent_config(faux_model_with_window(api, 1_000_000));
    config.max_turns = Some(3);
    config.compaction = Some(CompactionConfig {
        settings: CompactionSettings::default(),
        custom_instructions: None,
    });

    let agent = Agent::new(config);
    agent.add_message(assistant_with_usage(50_000));
    agent.add_message(user_msg("continue the work"));

    let events: Vec<_> = agent.prompt("next prompt").collect().await;

    assert!(
        !events
            .iter()
            .any(|event| matches!(event, AgentEvent::SessionCompacted { .. })),
        "must not compact at ~5% of a 1M context window: {events:?}"
    );
    assert!(events.iter().any(|event| matches!(
        event,
        AgentEvent::AgentDone { message }
            if message.content.iter().any(|block| matches!(
                block,
                ContentBlock::Text { text, .. } if text == "final answer"
            ))
    )));
}

#[tokio::test]
async fn runtime_compaction_triggers_near_context_limit() {
    // 100k context window; ~95k estimated tokens exceeds the
    // 100,000 - 16,384 = 83,616 trigger threshold, so compaction fires and
    // the summary is emitted before the final provider turn.
    let api = "runtime-compact-near-limit";
    let _provider_guard = ProviderGuard::register(
        api,
        Arc::new(FauxProvider::with_call_queue(vec![
            FauxProvider::text_call("summary of old context", StopReason::Stop),
            FauxProvider::text_call("final answer", StopReason::Stop),
        ])),
    );
    let mut config = _provider_guard.agent_config(faux_model_with_window(api, 100_000));
    config.max_turns = Some(3);
    config.compaction = Some(CompactionConfig {
        settings: CompactionSettings::default(),
        custom_instructions: None,
    });

    let agent = Agent::new(config);
    agent.add_message(assistant_with_usage(95_000));
    agent.add_message(user_msg("continue the work"));

    let events: Vec<_> = agent.prompt("next prompt").collect().await;

    assert!(events.iter().any(|event| matches!(
        event,
        AgentEvent::SessionCompacted { summary, .. } if summary == "summary of old context"
    )));
}

#[tokio::test]
async fn runtime_compaction_zero_context_window_never_compacts() {
    // context_window == 0 is the "no model metadata" guard: never auto
    // compact, even when the estimate is large. Mirrors the `should_compact`
    // `context_window > 0` guard.
    let api = "runtime-no-compact-zero-window";
    let _provider_guard = ProviderGuard::register(
        api,
        Arc::new(FauxProvider::with_call_queue(vec![
            FauxProvider::text_call("final answer", StopReason::Stop),
        ])),
    );
    let mut config = _provider_guard.agent_config(faux_model_with_window(api, 0));
    config.max_turns = Some(3);
    config.compaction = Some(CompactionConfig {
        settings: CompactionSettings::default(),
        custom_instructions: None,
    });

    let agent = Agent::new(config);
    agent.add_message(assistant_with_usage(50_000));
    agent.add_message(user_msg("continue the work"));

    let events: Vec<_> = agent.prompt("next prompt").collect().await;

    assert!(
        !events
            .iter()
            .any(|event| matches!(event, AgentEvent::SessionCompacted { .. })),
        "context_window == 0 must never auto compact: {events:?}"
    );
}
