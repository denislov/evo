use crate::app::prompt_execution::CodingAgentPromptExecution;
use crate::internal_tests::product_fixture::command::{
    PromptRuntimeOptions, run_prompt_text_for_tests,
};
use crate::internal_tests::support;

use agent_core::api::tool::{AgentTool, AgentToolOutput};
use ai::api::conversation::{ContentBlock, StopReason};
use ai::api::model::{Model, ModelCost, ModelInput};
use ai::api::testing::{FauxCall, FauxProvider, FauxResponse, FauxToolCall};
use coding_agent::api::event::{CodingAgentMessageProductEvent, CodingAgentProductEventKind};
use coding_agent::api::operation::{
    CodingAgentPromptExecutionUpdate, PromptInvocation, PromptTurnOutcome,
};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use support::ProviderGuard;

fn faux_model(api: &str) -> Model {
    Model {
        id: "faux-model".into(),
        name: "Faux Model".into(),
        api: api.into(),
        provider: "faux".into(),
        base_url: String::new(),
        reasoning: false,
        thinking_level_map: None,
        input: vec![ModelInput::Text],
        cost: ModelCost {
            known: true,
            input: 0.0,
            output: 0.0,
            cache_read: 0.0,
            cache_write: 0.0,
        },
        context_window: 0,
        max_tokens: 0,
        headers: None,
        compat: None,
    }
}

fn text_response(text: &str) -> FauxResponse {
    FauxResponse {
        text_deltas: vec![text.to_string()],
        thinking_deltas: vec![],
        tool_calls: vec![],
    }
}

fn echo_tool() -> AgentTool {
    AgentTool {
        name: "echo".into(),
        description: "echoes input".into(),
        parameters: serde_json::json!({"type": "object", "properties": {"text": {"type": "string"}}}),
        execution_mode: None,
        execute: Arc::new(|_context, args, _on_update| {
            let text = args
                .get("text")
                .and_then(|value| value.as_str())
                .unwrap_or("");
            let result = vec![ContentBlock::Text {
                text: format!("echo: {text}"),
                text_signature: None,
            }];
            Box::pin(async move { Ok(AgentToolOutput::new(result)) })
        }),
    }
}

fn mutation_tool(executions: Arc<AtomicUsize>) -> AgentTool {
    AgentTool {
        name: "mutate_external_state".into(),
        description: "mutates external state".into(),
        parameters: serde_json::json!({"type": "object"}),
        execution_mode: None,
        execute: Arc::new(move |_context, _args, _on_update| {
            let executions = executions.clone();
            Box::pin(async move {
                executions.fetch_add(1, Ordering::SeqCst);
                Ok(AgentToolOutput::new(vec![ContentBlock::Text {
                    text: "mutated".into(),
                    text_signature: None,
                }]))
            })
        }),
    }
}

#[tokio::test]
async fn prompt_execution_stream_yields_product_events_before_typed_completion() {
    let api = "coding-print-event-stream";
    let _provider_guard =
        ProviderGuard::register(api, Arc::new(FauxProvider::simple_text("streamed")));
    let execution = CodingAgentPromptExecution::from_internal(PromptRuntimeOptions {
        model: faux_model(api),
        api_key: None,
        auth_diagnostics: Vec::new(),
        system_prompt: None,
        max_turns: Some(2),
        tools: Vec::new(),
        register_builtins: false,
        ai_client: Some(_provider_guard.ai_client()),
        session: None,
        session_target: None,
        session_name: None,
        thinking_level: None,
        tool_execution: None,
        resources: agent_core::api::resources::AgentResources::default(),
        settings: None,
        invocation: PromptInvocation::Text("hello".into()),
    });
    assert_eq!(execution.metadata().api, api);
    assert_eq!(execution.metadata().provider, "faux");
    assert_eq!(execution.metadata().model, "faux-model");

    let mut stream = execution.start().await.unwrap();
    let mut saw_delta = false;
    let mut completed = None;
    while let Some(update) = stream.next().await.unwrap() {
        match update {
            CodingAgentPromptExecutionUpdate::Event(event) => {
                saw_delta |= matches!(
                    event.event(),
                    CodingAgentProductEventKind::Message(
                        CodingAgentMessageProductEvent::Delta { text, .. }
                    ) if text == "streamed"
                );
            }
            CodingAgentPromptExecutionUpdate::Completed(outcome) => {
                completed = Some(outcome);
            }
        }
    }

    assert!(saw_delta);
    assert!(matches!(
        completed,
        Some(PromptTurnOutcome::Success { final_text, .. }) if final_text == "streamed"
    ));
}

#[tokio::test]
async fn prints_single_turn_text_response() {
    let api = "coding-print-text";
    let _provider_guard = ProviderGuard::register(
        api,
        Arc::new(FauxProvider::new(vec![text_response("Hello")])),
    );

    let output = run_prompt_text_for_tests(PromptRuntimeOptions {
        model: faux_model(api),
        api_key: None,
        auth_diagnostics: Vec::new(),
        system_prompt: None,
        max_turns: Some(5),
        tools: Vec::new(),
        register_builtins: false,
        ai_client: Some(_provider_guard.ai_client()),
        session: None,
        session_target: None,
        session_name: None,
        thinking_level: None,
        tool_execution: None,
        resources: agent_core::api::resources::AgentResources::default(),
        settings: None,
        invocation: PromptInvocation::Text("hi".into()),
    })
    .await
    .unwrap();

    assert_eq!(output, "Hello");
}

#[tokio::test]
async fn treats_length_as_successful_final_text() {
    let api = "coding-print-length";
    let _provider_guard = ProviderGuard::register(
        api,
        Arc::new(FauxProvider::with_call_queue(vec![FauxCall {
            responses: vec![text_response("Partial final text")],
            stop_reason: StopReason::Length,
        }])),
    );

    let output = run_prompt_text_for_tests(PromptRuntimeOptions {
        model: faux_model(api),
        api_key: None,
        auth_diagnostics: Vec::new(),
        system_prompt: None,
        max_turns: Some(5),
        tools: Vec::new(),
        register_builtins: false,
        ai_client: Some(_provider_guard.ai_client()),
        session: None,
        session_target: None,
        session_name: None,
        thinking_level: None,
        tool_execution: None,
        resources: agent_core::api::resources::AgentResources::default(),
        settings: None,
        invocation: PromptInvocation::Text("hi".into()),
    })
    .await
    .unwrap();

    assert_eq!(output, "Partial final text");
}

#[tokio::test]
async fn returns_agent_failure_on_error_stop_reason() {
    let api = "coding-print-error";
    let _provider_guard = ProviderGuard::register(
        api,
        Arc::new(FauxProvider::with_call_queue(vec![FauxCall {
            responses: vec![FauxResponse {
                text_deltas: vec![],
                thinking_deltas: vec![],
                tool_calls: vec![],
            }],
            stop_reason: StopReason::Error,
        }])),
    );

    let error = run_prompt_text_for_tests(PromptRuntimeOptions {
        model: faux_model(api),
        api_key: None,
        auth_diagnostics: Vec::new(),
        system_prompt: None,
        max_turns: Some(5),
        tools: Vec::new(),
        register_builtins: false,
        ai_client: Some(_provider_guard.ai_client()),
        session: None,
        session_target: None,
        session_name: None,
        thinking_level: None,
        tool_execution: None,
        resources: agent_core::api::resources::AgentResources::default(),
        settings: None,
        invocation: PromptInvocation::Text("hi".into()),
    })
    .await
    .unwrap_err();

    assert_eq!(error.to_string(), "The model provider request failed.");
}

#[tokio::test]
async fn supports_tool_call_loop_with_injected_tool() {
    let api = "coding-print-tool-loop";
    let _provider_guard = ProviderGuard::register(
        api,
        Arc::new(FauxProvider::with_call_queue(vec![
            FauxCall {
                responses: vec![FauxResponse {
                    text_deltas: vec![],
                    thinking_deltas: vec![],
                    tool_calls: vec![FauxToolCall {
                        id: "tool_1".into(),
                        name: "echo".into(),
                        deltas: vec!["{\"text\":".into(), "\"hi\"}".into()],
                        final_arguments: serde_json::json!({"text": "hi"}),
                    }],
                }],
                stop_reason: StopReason::ToolUse,
            },
            FauxCall {
                responses: vec![text_response("Tool completed")],
                stop_reason: StopReason::Stop,
            },
        ])),
    );

    let output = run_prompt_text_for_tests(PromptRuntimeOptions {
        model: faux_model(api),
        api_key: None,
        auth_diagnostics: Vec::new(),
        system_prompt: None,
        max_turns: Some(5),
        tools: vec![echo_tool()],
        register_builtins: false,
        ai_client: Some(_provider_guard.ai_client()),
        session: None,
        session_target: None,
        session_name: None,
        thinking_level: None,
        tool_execution: None,
        resources: agent_core::api::resources::AgentResources::default(),
        settings: None,
        invocation: PromptInvocation::Text("echo hi".into()),
    })
    .await
    .unwrap();

    assert_eq!(output, "Tool completed");
}

#[tokio::test]
async fn print_mode_denies_unknown_mutation_without_waiting_or_executing() {
    let api = "coding-print-tool-authorization-deny";
    let _provider_guard = ProviderGuard::register(
        api,
        Arc::new(FauxProvider::with_call_queue(vec![
            FauxCall {
                responses: vec![FauxResponse {
                    text_deltas: vec![],
                    thinking_deltas: vec![],
                    tool_calls: vec![FauxToolCall {
                        id: "tool_mutate".into(),
                        name: "mutate_external_state".into(),
                        deltas: vec!["{}".into()],
                        final_arguments: serde_json::json!({}),
                    }],
                }],
                stop_reason: StopReason::ToolUse,
            },
            FauxCall {
                responses: vec![text_response("Mutation was denied; continuing safely")],
                stop_reason: StopReason::Stop,
            },
        ])),
    );
    let executions = Arc::new(AtomicUsize::new(0));

    let output = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        run_prompt_text_for_tests(PromptRuntimeOptions {
            model: faux_model(api),
            api_key: None,
            auth_diagnostics: Vec::new(),
            system_prompt: None,
            max_turns: Some(5),
            tools: vec![mutation_tool(executions.clone())],
            register_builtins: false,
            ai_client: Some(_provider_guard.ai_client()),
            session: None,
            session_target: None,
            session_name: None,
            thinking_level: None,
            tool_execution: None,
            resources: agent_core::api::resources::AgentResources::default(),
            settings: None,
            invocation: PromptInvocation::Text("mutate".into()),
        }),
    )
    .await
    .expect("print mode must not wait for interactive authorization")
    .unwrap();

    assert_eq!(output, "Mutation was denied; continuing safely");
    assert_eq!(executions.load(Ordering::SeqCst), 0);
}
