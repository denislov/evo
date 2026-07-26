pub mod convert;
pub mod stream;
pub mod wire;

use async_stream::stream;
use std::collections::BTreeMap;

use crate::protocol::{
    AssistantMessage, AssistantMessageEvent, Context, StopReason, StreamOptions,
};

use crate::model::Model;
use crate::protocol::stream::EventStream;
use crate::registry::ApiProvider;
use crate::transport::http::send_json_stream;
use convert::build_request;

pub struct MistralProvider {
    client: reqwest::Client,
    api_key: Option<String>,
}

impl MistralProvider {
    pub fn new(api_key: Option<String>) -> Self {
        Self {
            client: crate::transport::client::authenticated_client(),
            api_key,
        }
    }

    fn resolve_key(&self) -> Option<String> {
        self.api_key.clone()
    }
}

pub fn build_headers(model: &Model, opts: &Option<StreamOptions>) -> BTreeMap<String, String> {
    let mut headers = BTreeMap::new();
    append_json_headers(&mut headers, model.headers.as_ref());
    if let Some(opts) = opts {
        append_json_headers(&mut headers, opts.headers.as_ref());
        if let Some(session_id) = &opts.session_id {
            headers
                .entry("x-affinity".into())
                .or_insert_with(|| session_id.clone());
        }
    }
    headers
}

fn append_json_headers(headers: &mut BTreeMap<String, String>, value: Option<&serde_json::Value>) {
    let Some(obj) = value.and_then(|v| v.as_object()) else {
        return;
    };
    for (key, value) in obj {
        if let Some(value) = value.as_str() {
            headers.insert(key.clone(), value.to_string());
        }
    }
}

impl ApiProvider for MistralProvider {
    fn stream(&self, model: &Model, ctx: Context, opts: Option<StreamOptions>) -> EventStream {
        let key = opts
            .as_ref()
            .and_then(|o| o.api_key.clone())
            .or_else(|| self.resolve_key());
        let Some(api_key) = key else {
            let model_id = model.id.clone();
            return Box::pin(stream! {
                let mut msg = AssistantMessage::empty("mistral-conversations", &model_id);
                msg.provider = Some("mistral".into());
                msg.error_message = Some("No Mistral API key found. Set MISTRAL_API_KEY or pass apiKey in options.".into());
                msg.stop_reason = StopReason::Error;
                yield AssistantMessageEvent::Error {
                    reason: StopReason::Error,
                    message: msg,
                };
            });
        };

        let req_body = build_request(model, &ctx, &opts);
        let payload = match serde_json::to_value(&req_body) {
            Ok(payload) => payload,
            Err(error) => return serialization_error(model, error),
        };
        let base_url = model.base_url.trim_end_matches('/');
        let url = if base_url.ends_with("/v1") {
            format!("{}/chat/completions", base_url)
        } else {
            format!("{}/v1/chat/completions", base_url)
        };

        let mut request = self
            .client
            .post(&url)
            .bearer_auth(api_key)
            .header("content-type", "application/json")
            .header("accept", "text/event-stream");

        for (key, value) in build_headers(model, &opts) {
            request = request.header(key, value);
        }

        send_json_stream(
            &self.client,
            model,
            opts.as_ref(),
            "mistral-conversations",
            request,
            payload,
            |body, model, cancel| stream::process(body, model, cancel),
        )
    }
}

fn serialization_error(model: &Model, error: serde_json::Error) -> EventStream {
    let model_id = model.id.clone();
    let provider = model.provider.clone();
    Box::pin(stream! {
        let mut message = AssistantMessage::empty("mistral-conversations", &model_id);
        message.provider = Some(provider);
        message.error_message = Some(format!("Mistral request serialization failed: {error}"));
        message.stop_reason = StopReason::Error;
        yield AssistantMessageEvent::Error {
            reason: StopReason::Error,
            message,
        };
    })
}
