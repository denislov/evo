use std::sync::Arc;
use std::time::Duration;

use ai::api::fetch::{
    CacheConfig, FetchClient, FetchClientConfig, FetchError, FetchErrorKind, FetchRequest,
    FetchResult, OutputFormat,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tool_contract::api::definition::{
    AuthorizationRisk, ToolBehaviorVersion, ToolCapabilities, ToolDefinition, ToolExecutionMode,
    ToolId, ToolKind,
};
use tool_contract::api::output::{ToolContent, ToolError, ToolErrorKind, ToolOutput};
use tool_contract::api::schema::schema_for;
use tool_runtime::api::{DynamicTool, ToolCallContext, ToolFuture, TypedTool};

const DESCRIPTION: &str = "Fetch a URL and return its content as Markdown (default) or plain text. SSRF protection rejects private, link-local, and cloud-metadata addresses, and re-validates every redirect hop. Bodies are bounded to 2 MiB by default and 16 MiB at most; longer responses are truncated and marked as such. Successful results are cached briefly in memory.";
const DEFAULT_MAX_BYTES: usize = 2 * 1024 * 1024;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum WebFetchFormat {
    #[default]
    Markdown,
    Text,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct WebFetchArgs {
    /// URL to fetch. Only http and https schemes are accepted.
    url: String,
    /// Projection of the fetched page. Defaults to markdown. Accepted as
    /// `output_format` (schema) or `format` (alias).
    #[serde(default, alias = "format")]
    output_format: WebFetchFormat,
    /// Body byte budget override. Defaults to 2 MiB and must not exceed 16 MiB.
    #[serde(default)]
    max_bytes: Option<usize>,
}

impl WebFetchArgs {
    fn validate(&self) -> Result<FetchRequest, ToolError> {
        if self.url.is_empty() {
            return Err(ToolError::new(
                ToolErrorKind::InvalidArguments,
                "web_fetch: url must not be empty",
            ));
        }
        if self.url.len() > crate::limits::MAX_WEB_FETCH_URL_BYTES {
            return Err(ToolError::new(
                ToolErrorKind::InvalidArguments,
                format!(
                    "web_fetch: url exceeds the {} byte safety limit",
                    crate::limits::MAX_WEB_FETCH_URL_BYTES
                ),
            ));
        }
        if self
            .max_bytes
            .is_some_and(|bytes| bytes == 0 || bytes > crate::limits::MAX_WEB_FETCH_BYTES)
        {
            return Err(ToolError::new(
                ToolErrorKind::InvalidArguments,
                format!(
                    "web_fetch: max_bytes must be between 1 and {} bytes",
                    crate::limits::MAX_WEB_FETCH_BYTES
                ),
            ));
        }
        let format = match self.output_format {
            WebFetchFormat::Markdown => OutputFormat::Markdown,
            WebFetchFormat::Text => OutputFormat::Text,
        };
        Ok(FetchRequest {
            url: self.url.clone(),
            format,
            max_bytes: self.max_bytes,
        })
    }
}

/// Product-owned fetch budget. Mirrors the pipeline defaults so the product
/// budget survives any upstream default drift.
fn fetch_client_config() -> FetchClientConfig {
    FetchClientConfig {
        max_redirects: 5,
        resolve_timeout: Duration::from_secs(5),
        connect_timeout: Duration::from_secs(10),
        total_timeout: Duration::from_secs(30),
        conversion_timeout: Duration::from_secs(10),
        default_max_bytes: DEFAULT_MAX_BYTES,
        cache: Some(CacheConfig::default()),
        extra_ca_certificates: Vec::new(),
        ..FetchClientConfig::default()
    }
}

pub(crate) fn web_fetch_runtime_tool()
-> Result<Arc<dyn DynamicTool>, tool_runtime::api::ToolRegistryError> {
    let client = Arc::new(
        FetchClient::new(fetch_client_config())
            .expect("product fetch client builds from system roots"),
    );
    web_fetch_runtime_tool_with(client)
}

/// Build the tool over a caller-supplied client so tests can inject the
/// loopback-permitting `for_testing` construction.
pub(crate) fn web_fetch_runtime_tool_with(
    client: Arc<FetchClient>,
) -> Result<Arc<dyn DynamicTool>, tool_runtime::api::ToolRegistryError> {
    let definition = ToolDefinition {
        id: ToolId::new("web_fetch").expect("static tool id is valid"),
        kind: ToolKind::Function,
        description: DESCRIPTION.into(),
        parameters: schema_for::<WebFetchArgs>().expect("WebFetchArgs schema is valid"),
        capabilities: ToolCapabilities {
            read_only: true,
            execution: ToolExecutionMode::Parallel,
            cancel: true,
            timeout: true,
            streaming: false,
            provider_executed: false,
        },
        behavior: ToolBehaviorVersion::V1,
        authorization_risk: AuthorizationRisk::SideEffect,
        requirements: Vec::new(),
    };
    Ok(Arc::new(TypedTool::<WebFetchArgs>::new(
        definition,
        move |context, args| {
            let client = client.clone();
            Box::pin(async move { execute_web_fetch(&client, &context, args).await }) as ToolFuture
        },
    )?))
}

async fn execute_web_fetch(
    client: &FetchClient,
    context: &ToolCallContext,
    args: WebFetchArgs,
) -> Result<ToolOutput, ToolError> {
    let request = args.validate()?;
    let result = tokio::select! {
        _ = context.cancel.cancelled() => {
            return Err(ToolError::new(
                ToolErrorKind::Cancelled,
                "web_fetch: request cancelled",
            ));
        }
        result = client.fetch(request) => result,
    };
    result
        .map(success_output)
        .map_err(fetch_error_to_tool_error)
}

fn success_output(result: FetchResult) -> ToolOutput {
    let text = if result.truncated {
        format!(
            "{}\n\n[web_fetch: response exceeded the byte budget and was truncated; content is incomplete]",
            result.text
        )
    } else {
        result.text
    };
    ToolOutput {
        content: vec![ToolContent::Text { text }],
        details: Some(serde_json::json!({
            "final_url": result.final_url,
            "content_type": result.content_type,
            "truncated": result.truncated,
            "from_cache": result.from_cache,
        })),
        terminate: false,
    }
}

/// Map pipeline failures onto tool contract kinds. The variant alone is the
/// stable contract; the pipeline message is preserved for context.
fn fetch_error_to_tool_error(error: FetchError) -> ToolError {
    let kind = match error.kind {
        FetchErrorKind::InvalidUrl
        | FetchErrorKind::InvalidScheme
        | FetchErrorKind::UserInfoForbidden => ToolErrorKind::InvalidArguments,
        FetchErrorKind::SsrfBlocked => ToolErrorKind::Unauthorized,
        FetchErrorKind::DnsResolution | FetchErrorKind::Transport => ToolErrorKind::Unavailable,
        FetchErrorKind::ResolveTimeout
        | FetchErrorKind::ConnectTimeout
        | FetchErrorKind::RequestTimeout
        | FetchErrorKind::ConversionTimeout => ToolErrorKind::Timeout,
        FetchErrorKind::RedirectLimit
        | FetchErrorKind::HttpStatus
        | FetchErrorKind::ContentLengthOverBudget
        | FetchErrorKind::UnsupportedContentEncoding
        | FetchErrorKind::ContentDecode => ToolErrorKind::Execution,
    };
    ToolError {
        kind,
        message: format!("web_fetch: {}", error.message),
        details: error.details,
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use tokio_util::sync::CancellationToken;
    use tool_contract::api::definition::ToolId;
    use tool_contract::api::output::{ToolContent, ToolErrorKind};
    use tool_runtime::api::{ToolCallContext, ToolRegistry, ToolRuntime};

    use ai::api::fetch::FetchClient;

    use super::{
        WebFetchArgs, fetch_client_config, fetch_error_to_tool_error, web_fetch_runtime_tool_with,
    };

    fn runtime_for_testing() -> ToolRuntime {
        runtime_with(Arc::new(FetchClient::for_testing(fetch_client_config())))
    }

    fn runtime_strict() -> ToolRuntime {
        runtime_with(Arc::new(
            FetchClient::new(fetch_client_config()).expect("fetch client builds"),
        ))
    }

    fn runtime_with(client: Arc<FetchClient>) -> ToolRuntime {
        let mut registry = ToolRegistry::default();
        registry
            .register(web_fetch_runtime_tool_with(client).unwrap())
            .unwrap();
        ToolRuntime::new(registry).unwrap()
    }

    fn context() -> ToolCallContext {
        ToolCallContext::new(
            ToolId::new("web_fetch").unwrap(),
            "web-fetch-call",
            CancellationToken::new(),
        )
    }

    async fn execute(
        runtime: &ToolRuntime,
        url: &str,
    ) -> Result<tool_contract::api::output::ToolOutput, tool_contract::api::output::ToolError> {
        tokio::time::timeout(
            Duration::from_secs(10),
            runtime.execute(context(), serde_json::json!({ "url": url })),
        )
        .await
        .expect("web_fetch returns within the test budget")
    }

    fn terminal_text(output: tool_contract::api::output::ToolOutput) -> String {
        match output.content.as_slice() {
            [ToolContent::Text { text }] => text.clone(),
            _ => panic!("expected one text block"),
        }
    }

    struct MockServer {
        url: String,
        requests: Arc<AtomicUsize>,
        _thread: std::thread::JoinHandle<()>,
    }

    impl MockServer {
        fn spawn(status: u16, content_type: Option<&str>, body: String) -> Self {
            Self::spawn_impl(status, content_type, body, true)
        }

        /// Response without a Content-Length header; the connection close
        /// frames the body, exercising the streaming truncation path.
        fn spawn_undelimited(status: u16, content_type: Option<&str>, body: String) -> Self {
            Self::spawn_impl(status, content_type, body, false)
        }

        fn spawn_impl(
            status: u16,
            content_type: Option<&str>,
            body: String,
            declared_length: bool,
        ) -> Self {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind loopback");
            let url = format!("http://{}", listener.local_addr().unwrap());
            let requests = Arc::new(AtomicUsize::new(0));
            let counted = requests.clone();
            let content_type = content_type.map(str::to_owned);
            let thread = std::thread::spawn(move || {
                for stream in listener.incoming() {
                    let Ok(mut stream) = stream else { break };
                    let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
                    let mut buf = [0u8; 4096];
                    let mut filled = 0;
                    loop {
                        match stream.read(&mut buf[filled..]) {
                            Ok(0) => break,
                            Ok(n) => {
                                filled += n;
                                if filled == buf.len()
                                    || buf[..filled].windows(4).any(|w| w == b"\r\n\r\n")
                                {
                                    break;
                                }
                            }
                            Err(_) => break,
                        }
                    }
                    counted.fetch_add(1, Ordering::SeqCst);
                    let reason = match status {
                        200 => "OK",
                        301 => "Moved Permanently",
                        302 => "Found",
                        404 => "Not Found",
                        500 => "Internal Server Error",
                        _ => "X",
                    };
                    let content_type = content_type
                        .as_deref()
                        .map(|value| format!("Content-Type: {value}\r\n"))
                        .unwrap_or_default();
                    let length = if declared_length {
                        format!("Content-Length: {}\r\n", body.len())
                    } else {
                        String::new()
                    };
                    let response = format!(
                        "HTTP/1.1 {status} {reason}\r\nConnection: close\r\n{content_type}{length}\r\n{body}"
                    );
                    let _ = stream.write_all(response.as_bytes());
                    let _ = stream.flush();
                }
            });
            Self {
                url,
                requests,
                _thread: thread,
            }
        }

        fn request_count(&self) -> usize {
            self.requests.load(Ordering::SeqCst)
        }
    }

    #[test]
    fn definition_is_read_only_and_network_scoped() {
        let tool = web_fetch_runtime_tool_with(Arc::new(
            FetchClient::new(fetch_client_config()).expect("fetch client builds"),
        ))
        .unwrap();
        let definition = tool.definition();
        assert_eq!(definition.id.as_str(), "web_fetch");
        assert_eq!(
            definition.kind,
            tool_contract::api::definition::ToolKind::Function
        );
        assert!(definition.capabilities.read_only);
        assert_eq!(
            definition.capabilities.execution,
            tool_contract::api::definition::ToolExecutionMode::Parallel
        );
        assert!(definition.capabilities.cancel);
        assert!(definition.capabilities.timeout);
        assert!(!definition.capabilities.streaming);
        assert!(!definition.capabilities.provider_executed);
        assert_eq!(
            definition.authorization_risk,
            tool_contract::api::definition::AuthorizationRisk::SideEffect
        );
        assert_eq!(definition.parameters["additionalProperties"], false);
        assert_eq!(definition.parameters["properties"]["url"]["type"], "string");
        let schema = serde_json::to_string(&definition.parameters).unwrap();
        assert!(schema.contains("\"output_format\""), "{schema}");
        assert!(schema.contains("\"markdown\""), "{schema}");
        assert!(schema.contains("\"text\""), "{schema}");
        assert!(
            definition.parameters["required"]
                .as_array()
                .unwrap()
                .contains(&serde_json::json!("url"))
        );
    }

    #[tokio::test]
    async fn invalid_arguments_are_rejected_without_network() {
        let runtime = runtime_strict();
        for arguments in [
            serde_json::json!({ "url": "" }),
            serde_json::json!({ "url": "https://example.com", "output_format": "pdf" }),
            serde_json::json!({ "url": "https://example.com", "max_bytes": 0 }),
            serde_json::json!({ "url": "https://example.com", "max_bytes": crate::limits::MAX_WEB_FETCH_BYTES + 1 }),
            serde_json::json!({ "url": "https://example.com", "unexpected": true }),
        ] {
            let error = runtime.execute(context(), arguments).await.unwrap_err();
            assert_eq!(error.kind, ToolErrorKind::InvalidArguments, "{error:?}");
        }
    }

    #[tokio::test]
    async fn invalid_url_maps_to_invalid_arguments_without_network() {
        let error = execute(&runtime_strict(), "not a url").await.unwrap_err();
        assert_eq!(error.kind, ToolErrorKind::InvalidArguments);
        assert!(error.message.contains("web_fetch"));
    }

    #[tokio::test]
    async fn ssrf_blocks_private_and_loopback_targets() {
        let runtime = runtime_strict();
        for url in [
            "http://127.0.0.1:9/",
            "http://[::1]:9/",
            "http://169.254.169.254/latest/meta-data/",
            "http://10.0.0.1/",
            "http://192.168.1.1/",
        ] {
            let error = execute(&runtime, url).await.unwrap_err();
            assert_eq!(error.kind, ToolErrorKind::Unauthorized, "{url}: {error:?}");
            assert!(error.message.contains("SSRF"), "{url}: {error:?}");
        }
    }

    #[tokio::test]
    async fn returns_markdown_with_metadata() {
        let server = MockServer::spawn(
            200,
            Some("text/html; charset=utf-8"),
            "<html><body><h1>Hello</h1><p>World</p></body></html>".into(),
        );
        let output = execute(&runtime_for_testing(), &server.url).await.unwrap();
        let text = terminal_text(output.clone());
        assert!(text.contains("Hello"), "{text}");
        assert!(text.contains("World"), "{text}");
        let details = output.details.unwrap();
        assert!(
            details["final_url"]
                .as_str()
                .unwrap()
                .starts_with(&server.url),
            "final_url must keep the request host"
        );
        assert!(
            details["content_type"]
                .as_str()
                .unwrap()
                .contains("text/html")
        );
        assert_eq!(details["truncated"], false);
        assert_eq!(details["from_cache"], false);
    }

    #[tokio::test]
    async fn text_format_strips_html() {
        let server = MockServer::spawn(
            200,
            Some("text/html"),
            "<html><body><h1>Title</h1><p>  Body  text </p></body></html>".into(),
        );
        let runtime = runtime_for_testing();
        let output = tokio::time::timeout(
            Duration::from_secs(10),
            runtime.execute(
                context(),
                serde_json::json!({ "url": server.url, "format": "text" }),
            ),
        )
        .await
        .expect("returns within the test budget")
        .unwrap();
        let text = terminal_text(output);
        assert!(!text.contains('<'), "{text}");
        assert!(text.contains("Body text"), "{text}");
    }

    #[tokio::test]
    async fn truncation_is_marked_in_text_and_details() {
        let body = "x".repeat(512);
        let server = MockServer::spawn_undelimited(200, Some("text/plain"), body);
        let runtime = runtime_for_testing();
        let output = tokio::time::timeout(
            Duration::from_secs(10),
            runtime.execute(
                context(),
                serde_json::json!({ "url": server.url, "max_bytes": 64 }),
            ),
        )
        .await
        .expect("returns within the test budget")
        .unwrap();
        let text = terminal_text(output.clone());
        assert!(text.contains("truncated"), "{text}");
        assert_eq!(output.details.unwrap()["truncated"], true);
    }

    #[tokio::test]
    async fn repeated_requests_are_served_from_cache() {
        let server = MockServer::spawn(200, Some("text/plain"), "cached body".into());
        let runtime = runtime_for_testing();
        let first = execute(&runtime, &server.url).await.unwrap();
        assert_eq!(first.details.unwrap()["from_cache"], false);
        let second = execute(&runtime, &server.url).await.unwrap();
        assert_eq!(second.details.unwrap()["from_cache"], true);
        assert_eq!(
            server.request_count(),
            1,
            "second request must hit the cache"
        );
    }

    #[tokio::test]
    async fn http_status_errors_map_to_execution() {
        let server = MockServer::spawn(404, Some("text/html"), "missing".into());
        let error = execute(&runtime_for_testing(), &server.url)
            .await
            .unwrap_err();
        assert_eq!(error.kind, ToolErrorKind::Execution);
        assert!(error.message.contains("404"), "{error:?}");
        assert_eq!(error.details.unwrap()["status"], 404);
    }

    #[test]
    fn every_pipeline_failure_kind_maps_to_a_tool_kind() {
        use ai::api::fetch::FetchErrorKind;
        let cases = [
            (FetchErrorKind::InvalidUrl, ToolErrorKind::InvalidArguments),
            (
                FetchErrorKind::InvalidScheme,
                ToolErrorKind::InvalidArguments,
            ),
            (
                FetchErrorKind::UserInfoForbidden,
                ToolErrorKind::InvalidArguments,
            ),
            (FetchErrorKind::SsrfBlocked, ToolErrorKind::Unauthorized),
            (FetchErrorKind::DnsResolution, ToolErrorKind::Unavailable),
            (FetchErrorKind::Transport, ToolErrorKind::Unavailable),
            (FetchErrorKind::ResolveTimeout, ToolErrorKind::Timeout),
            (FetchErrorKind::ConnectTimeout, ToolErrorKind::Timeout),
            (FetchErrorKind::RequestTimeout, ToolErrorKind::Timeout),
            (FetchErrorKind::ConversionTimeout, ToolErrorKind::Timeout),
            (FetchErrorKind::RedirectLimit, ToolErrorKind::Execution),
            (FetchErrorKind::HttpStatus, ToolErrorKind::Execution),
            (
                FetchErrorKind::ContentLengthOverBudget,
                ToolErrorKind::Execution,
            ),
            (
                FetchErrorKind::UnsupportedContentEncoding,
                ToolErrorKind::Execution,
            ),
            (FetchErrorKind::ContentDecode, ToolErrorKind::Execution),
        ];
        for (pipeline, tool) in cases {
            let error = fetch_error_to_tool_error(ai::api::fetch::FetchError {
                kind: pipeline,
                message: "test message".into(),
                details: Some(serde_json::json!({ "status": 500 })),
            });
            assert_eq!(error.kind, tool, "{pipeline:?}");
            assert!(error.message.contains("test message"));
            assert_eq!(error.details.unwrap()["status"], 500);
        }
    }

    #[test]
    fn argument_schema_rejects_unknown_fields_and_missing_url() {
        let parsed = serde_json::from_value::<WebFetchArgs>(serde_json::json!({
            "max_bytes": 1024,
        }));
        assert!(parsed.is_err(), "url is required");
        let parsed = serde_json::from_value::<WebFetchArgs>(serde_json::json!({
            "url": "https://example.com",
            "sneaky": true,
        }));
        assert!(parsed.is_err(), "unknown fields must fail closed");
        let parsed = serde_json::from_value::<WebFetchArgs>(serde_json::json!({
            "url": "https://example.com",
        }))
        .unwrap();
        let request = parsed.validate().unwrap();
        assert_eq!(request.format, ai::api::fetch::OutputFormat::Markdown);
        assert_eq!(request.max_bytes, None);
    }

    #[test]
    fn max_bytes_validation_bounds_are_exact() {
        let at_limit = serde_json::from_value::<WebFetchArgs>(serde_json::json!({
            "url": "https://example.com",
            "max_bytes": crate::limits::MAX_WEB_FETCH_BYTES,
        }))
        .unwrap();
        assert!(at_limit.validate().is_ok());
        let over_limit = serde_json::from_value::<WebFetchArgs>(serde_json::json!({
            "url": "https://example.com",
            "max_bytes": crate::limits::MAX_WEB_FETCH_BYTES + 1,
        }))
        .unwrap();
        assert!(over_limit.validate().is_err());
    }
}
