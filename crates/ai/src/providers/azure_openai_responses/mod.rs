use async_stream::stream;
use std::collections::BTreeMap;

use crate::protocol::{
    AssistantMessage, AssistantMessageEvent, Context, StopReason, StreamOptions,
};

use crate::model::Model;
use crate::protocol::stream::EventStream;
use crate::providers::openai::responses::{convert, stream, wire};
use crate::registry::ApiProvider;
use crate::transport::http::send_json_stream;

const DEFAULT_AZURE_API_VERSION: &str = "v1";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AzureRequestTarget {
    pub url: String,
    pub deployment_name: String,
}

pub struct AzureOpenAIResponsesProvider {
    client: reqwest::Client,
    api_key: Option<String>,
}

impl AzureOpenAIResponsesProvider {
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

pub fn build_request(
    model: &Model,
    ctx: &Context,
    opts: &Option<StreamOptions>,
) -> wire::ResponseCreateRequest {
    let mut request = convert::build_request(model, ctx, opts);
    request.model = resolve_deployment_name(model, opts);
    request
}

pub fn resolve_target(
    model: &Model,
    opts: &Option<StreamOptions>,
) -> Result<AzureRequestTarget, String> {
    let api_version = opts
        .as_ref()
        .and_then(|o| o.azure_api_version.clone())
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_AZURE_API_VERSION.into());

    let base_url = opts
        .as_ref()
        .and_then(|o| o.azure_base_url.clone())
        .or_else(|| {
            opts.as_ref()
                .and_then(|o| o.azure_resource_name.clone())
                .map(|resource| format!("https://{}.openai.azure.com/openai/v1", resource))
        })
        .or_else(|| {
            if model.base_url.trim().is_empty() {
                None
            } else {
                Some(model.base_url.clone())
            }
        })
        .ok_or_else(|| {
            "Azure OpenAI base URL is required. Pass azureBaseUrl, azureResourceName, model.baseUrl, or configure ProviderAuthResolver.".to_string()
        })?;

    let base_url = normalize_azure_base_url(&base_url)?;
    Ok(AzureRequestTarget {
        url: format!("{}/responses?api-version={}", base_url, api_version),
        deployment_name: resolve_deployment_name(model, opts),
    })
}

fn resolve_deployment_name(model: &Model, opts: &Option<StreamOptions>) -> String {
    opts.as_ref()
        .and_then(|o| o.azure_deployment_name.clone())
        .unwrap_or_else(|| model.id.clone())
}

fn normalize_azure_base_url(base_url: &str) -> Result<String, String> {
    let trimmed = base_url.trim().trim_end_matches('/');
    let Some((scheme, rest)) = trimmed.split_once("://") else {
        return Err(format!("Invalid Azure OpenAI base URL: {}", base_url));
    };
    if scheme != "https" && scheme != "http" {
        return Err(format!("Invalid Azure OpenAI base URL: {}", base_url));
    }

    let (host, path) = match rest.split_once('/') {
        Some((host, path)) => (host, format!("/{}", path.trim_end_matches('/'))),
        None => (rest, String::new()),
    };
    if host.is_empty() {
        return Err(format!("Invalid Azure OpenAI base URL: {}", base_url));
    }

    let is_azure_host =
        host.ends_with(".openai.azure.com") || host.ends_with(".cognitiveservices.azure.com");
    let path = if is_azure_host && (path.is_empty() || path == "/" || path == "/openai") {
        "/openai/v1".to_string()
    } else {
        path
    };

    Ok(format!("{}://{}{}", scheme, host, path)
        .trim_end_matches('/')
        .to_string())
}

fn build_headers(model: &Model, opts: &Option<StreamOptions>) -> BTreeMap<String, String> {
    let mut headers = BTreeMap::new();
    append_json_headers(&mut headers, model.headers.as_ref());
    if let Some(opts) = opts {
        append_json_headers(&mut headers, opts.headers.as_ref());
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

impl ApiProvider for AzureOpenAIResponsesProvider {
    fn stream(&self, model: &Model, ctx: Context, opts: Option<StreamOptions>) -> EventStream {
        let key = opts
            .as_ref()
            .and_then(|o| o.api_key.clone())
            .or_else(|| self.resolve_key());
        let Some(api_key) = key else {
            let model_id = model.id.clone();
            return Box::pin(stream! {
                let mut msg = AssistantMessage::empty("azure-openai-responses", &model_id);
                msg.provider = Some("azure-openai-responses".into());
                msg.error_message = Some("No Azure OpenAI API key found. Set AZURE_OPENAI_API_KEY or pass apiKey in options.".into());
                msg.stop_reason = StopReason::Error;
                yield AssistantMessageEvent::Error {
                    reason: StopReason::Error,
                    message: msg,
                };
            });
        };

        let target = match resolve_target(model, &opts) {
            Ok(target) => target,
            Err(error) => {
                let model_id = model.id.clone();
                return Box::pin(stream! {
                    let mut msg = AssistantMessage::empty("azure-openai-responses", &model_id);
                    msg.provider = Some("azure-openai-responses".into());
                    msg.error_message = Some(error);
                    msg.stop_reason = StopReason::Error;
                    yield AssistantMessageEvent::Error {
                        reason: StopReason::Error,
                        message: msg,
                    };
                });
            }
        };

        let req_body = build_request(model, &ctx, &opts);
        let payload = match serde_json::to_value(&req_body) {
            Ok(payload) => payload,
            Err(error) => return serialization_error(model, error),
        };
        let mut request = self
            .client
            .post(&target.url)
            .header("api-key", api_key)
            .header("content-type", "application/json")
            .header("accept", "text/event-stream");

        for (key, value) in build_headers(model, &opts) {
            request = request.header(key, value);
        }

        send_json_stream(
            &self.client,
            model,
            opts.as_ref(),
            "azure-openai-responses",
            request,
            payload,
            |body, model, cancel| {
                stream::process_with_api_name(body, model, cancel, "azure-openai-responses")
            },
        )
    }
}

fn serialization_error(model: &Model, error: serde_json::Error) -> EventStream {
    let model_id = model.id.clone();
    let provider = model.provider.clone();
    Box::pin(stream! {
        let mut message = AssistantMessage::empty("azure-openai-responses", &model_id);
        message.provider = Some(provider);
        message.error_message = Some(format!("Azure request serialization failed: {error}"));
        message.stop_reason = StopReason::Error;
        yield AssistantMessageEvent::Error {
            reason: StopReason::Error,
            message,
        };
    })
}
