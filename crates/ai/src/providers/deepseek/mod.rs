pub mod convert;
pub mod wire;

use async_stream::stream;

use crate::model::Model;
use crate::protocol::stream::EventStream;
use crate::protocol::{
    AssistantMessage, AssistantMessageEvent, Context, StopReason, StreamOptions,
};
use crate::registry::ApiProvider;
use crate::transport::headers::merge_headers;
use crate::transport::http::{
    SendResilience, credential_refresh_slot, send_json_stream_with_resilience,
};

const API_NAME: &str = "deepseek-responses";

pub struct DeepSeekResponsesProvider {
    client: reqwest::Client,
    api_key: Option<String>,
}

impl DeepSeekResponsesProvider {
    #[cfg(test)]
    pub fn new(api_key: Option<String>) -> Self {
        let client = crate::transport::client::authenticated_client(&Default::default())
            .expect("the default provider HTTP client should build");
        Self::with_client(api_key, client)
    }

    pub(crate) fn with_client(api_key: Option<String>, client: reqwest::Client) -> Self {
        Self { client, api_key }
    }

    fn resolve_key(&self, opts: Option<&StreamOptions>) -> Option<String> {
        opts.and_then(|options| options.api_key.clone())
            .or_else(|| self.api_key.clone())
    }
}

pub fn resolve_responses_url(base_url: &str) -> String {
    let base_url = base_url.trim().trim_end_matches('/');
    if base_url.ends_with("/responses") {
        base_url.into()
    } else if base_url.ends_with("/v1") {
        format!("{base_url}/responses")
    } else {
        format!("{base_url}/v1/responses")
    }
}

impl ApiProvider for DeepSeekResponsesProvider {
    fn stream(&self, model: &Model, ctx: Context, opts: Option<StreamOptions>) -> EventStream {
        self.stream_with_resilience(model, ctx, opts, SendResilience::default())
    }

    fn stream_with_resilience(
        &self,
        model: &Model,
        ctx: Context,
        opts: Option<StreamOptions>,
        resilience: SendResilience,
    ) -> EventStream {
        let Some(_api_key) = self.resolve_key(opts.as_ref()) else {
            return error_stream(
                model,
                "No DeepSeek API key found. Set DEEPSEEK_API_KEY or pass apiKey in options.",
            );
        };

        let body = match convert::build_request(model, &ctx, opts.as_ref()) {
            Ok(body) => body,
            Err(error) => return error_stream(model, error),
        };
        let payload = match serde_json::to_value(body) {
            Ok(payload) => payload,
            Err(error) => {
                return error_stream(
                    model,
                    format!("DeepSeek request serialization failed: {error}"),
                );
            }
        };

        let url = resolve_responses_url(&model.base_url);
        let client = self.client.clone();
        let initial_opts = opts.clone();
        let model_headers = model.headers.clone();
        let self_api_key = self.api_key.clone();
        let (build_request, refresh) = credential_refresh_slot(
            move |current: Option<&StreamOptions>, payload| {
                let current = current.or(initial_opts.as_ref());
                let key = current
                    .and_then(|o| o.api_key.clone())
                    .or_else(|| self_api_key.clone());
                let mut request = client.post(&url);
                if let Some(key) = key {
                    request = request.bearer_auth(key);
                }
                for (name, value) in merge_headers(
                    model_headers.as_ref(),
                    current.and_then(|o| o.headers.as_ref()),
                    [
                        ("content-type".into(), "application/json".into()),
                        ("accept".into(), "text/event-stream".into()),
                    ],
                ) {
                    request = request.header(name, value);
                }
                Ok(request.json(payload))
            },
            resilience.refresh_auth,
            opts.clone(),
        );

        send_json_stream_with_resilience(
            model,
            opts.as_ref(),
            API_NAME,
            payload,
            build_request,
            |body, model, cancel| {
                crate::providers::responses::stream::process_with_api_name(
                    body, model, cancel, API_NAME,
                )
            },
            SendResilience {
                breaker: resilience.breaker,
                refresh_auth: refresh,
                scrubber: resilience.scrubber,
            },
        )
    }
}

fn error_stream(model: &Model, error: impl Into<String>) -> EventStream {
    let model_id = model.id.clone();
    let provider = model.provider.clone();
    let error = error.into();
    Box::pin(stream! {
        let mut message = AssistantMessage::empty(API_NAME, &model_id);
        message.provider = Some(provider);
        message.error_message = Some(error);
        message.stop_reason = StopReason::Error;
        yield AssistantMessageEvent::Error {
            reason: StopReason::Error,
            message,
        };
    })
}

#[cfg(test)]
mod tests {
    use super::{DeepSeekResponsesProvider, resolve_responses_url};
    use crate::protocol::{
        AssistantMessageEvent, ContentBlock, Context, Message, StreamOptions, ThinkingConfig,
    };
    use crate::registry::ApiProvider;
    use futures::StreamExt;

    #[test]
    fn responses_url_accepts_root_v1_and_explicit_endpoint() {
        assert_eq!(
            resolve_responses_url("https://api.deepseek.com"),
            "https://api.deepseek.com/v1/responses"
        );
        assert_eq!(
            resolve_responses_url("https://api.deepseek.com/v1/"),
            "https://api.deepseek.com/v1/responses"
        );
        assert_eq!(
            resolve_responses_url("https://proxy.example/deepseek/v1/responses"),
            "https://proxy.example/deepseek/v1/responses"
        );
    }

    #[tokio::test]
    async fn missing_credentials_keep_deepseek_provider_identity() {
        let model = crate::model::get_model("deepseek", "deepseek-v4-flash")
            .expect("DeepSeek V4 Flash is in the catalog");
        let provider = DeepSeekResponsesProvider::new(None);
        let mut stream = provider.stream(
            &model,
            Context {
                system_prompt: Some("hello".into()),
                messages: Vec::new(),
                tools: None,
            },
            None,
        );

        let event = stream
            .next()
            .await
            .expect("missing credentials emits an error");
        let AssistantMessageEvent::Error { message, .. } = event else {
            panic!("expected an error event");
        };
        assert_eq!(message.api, "deepseek-responses");
        assert_eq!(message.provider.as_deref(), Some("deepseek"));
    }

    /// Paid, opt-in contract test. The key is supplied by the caller and is
    /// never read from or written to a repository path.
    #[tokio::test]
    #[ignore = "requires DEEPSEEK_LIVE_API_KEY and performs a paid network request"]
    async fn live_reasoning_stream_matches_provider_contract() {
        let api_key = std::env::var("DEEPSEEK_LIVE_API_KEY")
            .expect("set DEEPSEEK_LIVE_API_KEY to run the paid contract test");
        let model = crate::model::get_model("deepseek", "deepseek-v4-flash")
            .expect("DeepSeek V4 Flash is in the catalog");
        let provider = DeepSeekResponsesProvider::new(Some(api_key));
        let context = Context {
            system_prompt: None,
            messages: vec![Message::User {
                content: vec![ContentBlock::Text {
                    text: "What is 13 * 17? Answer with the number only.".into(),
                    text_signature: None,
                }],
            }],
            tools: None,
        };
        let options = StreamOptions {
            max_tokens: Some(96),
            timeout_ms: Some(60_000),
            thinking: Some(ThinkingConfig {
                enabled: true,
                budget_tokens: None,
                effort: Some("low".into()),
            }),
            ..StreamOptions::default()
        };

        let message =
            crate::protocol::stream::complete(provider.stream(&model, context, Some(options)))
                .await
                .expect("live DeepSeek stream completes successfully");
        assert_eq!(message.api, "deepseek-responses");
        assert_eq!(message.provider.as_deref(), Some("deepseek"));
        assert_eq!(message.response_model.as_deref(), Some("deepseek-v4-flash"));
        assert!(message.usage.total_tokens > 0);
        assert!(message.content.iter().any(|block| matches!(
            block,
            ContentBlock::Thinking {
                provider_metadata: Some(metadata),
                ..
            } if metadata.api == "deepseek-responses" && metadata.item_id.is_some()
        )));
        assert!(message.content.iter().any(|block| matches!(
            block,
            ContentBlock::Text { text, .. } if !text.is_empty()
        )));
    }
}
