use ai_protocol::api::compatibility::{ModelCompat, OpenAICompletionsCompat};
use ai_protocol::api::conversation::{
    AssistantMessage, ContentBlock, Context, Message, StopReason, Tool,
};
use ai_protocol::api::model::{Model, ModelCost, ModelInput};
use ai_protocol::api::stream::{AssistantMessageEvent, EventStream, complete};

#[test]
fn model_context_and_provider_metadata_round_trip_without_shape_drift() {
    let model = Model {
        id: "model-1".into(),
        name: "Model One".into(),
        api: "openai-completions".into(),
        provider: "provider-1".into(),
        base_url: "https://example.invalid/v1".into(),
        reasoning: true,
        thinking_level_map: None,
        input: vec![ModelInput::Text, ModelInput::Image],
        cost: ModelCost::default(),
        context_window: 128_000,
        max_tokens: 8_192,
        headers: Some(serde_json::json!({"x-test": "value"})),
        compat: Some(ModelCompat::OpenAICompletions(OpenAICompletionsCompat {
            supports_reasoning_effort: Some(true),
            ..Default::default()
        })),
    };
    let context = Context {
        system_prompt: Some("system".into()),
        messages: vec![Message::Assistant {
            content: vec![ContentBlock::ProviderItem {
                api: model.api.clone(),
                item: serde_json::json!({"id": "item-1", "type": "web_search_call"}),
            }],
        }],
        tools: Some(vec![Tool::web_search(), Tool::custom("patch", None)]),
    };

    let model_json = serde_json::to_value(&model).expect("serialize model");
    let context_json = serde_json::to_value(&context).expect("serialize context");
    assert_eq!(
        serde_json::from_value::<Model>(model_json).expect("deserialize model"),
        model
    );
    assert_eq!(
        serde_json::from_value::<Context>(context_json).expect("deserialize context"),
        context
    );
}

#[tokio::test]
async fn complete_accepts_only_successful_terminal_events() {
    let mut success = AssistantMessage::empty("faux", "model-1");
    success.content.push(ContentBlock::Text {
        text: "done".into(),
        text_signature: None,
    });
    let success_stream: EventStream =
        Box::pin(futures::stream::iter(vec![AssistantMessageEvent::Done {
            reason: StopReason::Stop,
            message: success.clone(),
        }]));
    assert_eq!(
        complete(success_stream).await.expect("successful stream"),
        success
    );

    let mut invalid = AssistantMessage::empty("faux", "model-1");
    invalid.stop_reason = StopReason::Error;
    let invalid_stream: EventStream =
        Box::pin(futures::stream::iter(vec![AssistantMessageEvent::Done {
            reason: StopReason::Error,
            message: invalid,
        }]));
    assert!(complete(invalid_stream).await.is_err());
}
