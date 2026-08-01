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
use crate::transport::http::send_json_stream;

const API_NAME: &str = "deepseek-responses";

pub struct DeepSeekResponsesProvider {
    client: reqwest::Client,
    api_key: Option<String>,
}

impl DeepSeekResponsesProvider {
    pub fn new(api_key: Option<String>) -> Self {
        Self {
            client: crate::transport::client::authenticated_client(),
            api_key,
        }
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
        let Some(api_key) = self.resolve_key(opts.as_ref()) else {
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
        let mut request = self.client.post(url).bearer_auth(api_key);
        for (name, value) in merge_headers(
            model.headers.as_ref(),
            opts.as_ref().and_then(|options| options.headers.as_ref()),
            [
                ("content-type".into(), "application/json".into()),
                ("accept".into(), "text/event-stream".into()),
            ],
        ) {
            request = request.header(name, value);
        }

        send_json_stream(
            &self.client,
            model,
            opts.as_ref(),
            API_NAME,
            request,
            payload,
            |body, model, cancel| {
                crate::providers::openai::responses::stream::process_with_api_name(
                    body, model, cancel, API_NAME,
                )
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
    use crate::protocol::{AssistantMessageEvent, Context};
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
}
