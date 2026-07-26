use ai::api::conversation::{AssistantMessage, StopReason};
use ai::api::hooks::{ProviderResponseInfo, ProviderStreamHooks};
use ai::api::model::{Model, ModelCost, ModelInput};
use ai::api::provider::{ProviderRegistry, builtin_provider_apis, register_builtins_into};
use ai::api::stream::{AssistantMessageEvent, EventStream, StreamOptions, complete};
use ai::transport::headers::merge_headers;
use ai::transport::http::send_json_stream;
use ai::transport::retry::RetryConfig;
use futures::StreamExt;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::Arc;
use std::sync::mpsc;
use tokio_util::sync::CancellationToken;

fn test_model() -> Model {
    Model {
        id: "test-model".into(),
        name: "Test Model".into(),
        api: "openai-responses".into(),
        provider: "openai".into(),
        base_url: "http://127.0.0.1".into(),
        reasoning: false,
        thinking_level_map: None,
        input: vec![ModelInput::Text],
        cost: ModelCost::default(),
        context_window: 128_000,
        max_tokens: 4096,
        headers: None,
        compat: None,
    }
}

fn done_stream(api: &'static str) -> EventStream {
    Box::pin(async_stream::stream! {
        yield AssistantMessageEvent::Done {
            reason: StopReason::Stop,
            message: AssistantMessage::empty(api, "test-model"),
        };
    })
}

fn pending_stream() -> EventStream {
    Box::pin(futures::stream::pending())
}

fn start_http_server(responses: Vec<String>) -> (String, mpsc::Receiver<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let (tx, rx) = mpsc::channel();

    std::thread::spawn(move || {
        for response in responses {
            let (mut stream, _) = listener.accept().unwrap();
            let request = read_http_request(&mut stream);
            tx.send(request).unwrap();
            stream.write_all(response.as_bytes()).unwrap();
            stream.flush().unwrap();
        }
    });

    (format!("http://{}", addr), rx)
}

fn start_stalled_http_server() -> (String, mpsc::Sender<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let (release_tx, release_rx) = mpsc::channel();
    std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let _request = read_http_request(&mut stream);
        let _ = release_rx.recv();
    });
    (format!("http://{addr}"), release_tx)
}

fn start_observed_stalled_http_server() -> (String, mpsc::Receiver<()>, mpsc::Sender<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let _request = read_http_request(&mut stream);
        started_tx.send(()).unwrap();
        let _ = release_rx.recv();
    });
    (format!("http://{addr}"), started_rx, release_tx)
}

fn contract_model(api: &str, base_url: String) -> Model {
    let provider = match api {
        "anthropic-messages" => "anthropic",
        "azure-openai-responses" => "azure-openai-responses",
        "google-generative-ai" => "google",
        "mistral-conversations" => "mistral",
        "openai-codex-responses" => "openai-codex",
        "openai-completions" | "openai-responses" => "openai",
        other => panic!("unclassified built-in provider API `{other}`"),
    };
    Model {
        id: "contract-model".into(),
        name: "Contract Model".into(),
        api: api.into(),
        provider: provider.into(),
        base_url,
        reasoning: true,
        thinking_level_map: None,
        input: vec![ModelInput::Text],
        cost: ModelCost::default(),
        context_window: 128_000,
        max_tokens: 4096,
        headers: None,
        compat: None,
    }
}

fn contract_options(api: &str, base_url: &str) -> StreamOptions {
    let mut options = StreamOptions {
        api_key: Some("contract-api-key".into()),
        ..Default::default()
    };
    if api == "openai-codex-responses" {
        options.api_key = Some("header.eyJodHRwczovL2FwaS5vcGVuYWkuY29tL2F1dGgiOnsiY2hhdGdwdF9hY2NvdW50X2lkIjoiY29udHJhY3RfYWNjb3VudCJ9fQ.sig".into());
    }
    if api == "azure-openai-responses" {
        options.azure_base_url = Some(base_url.into());
        options.azure_api_version = Some("2025-01-01-preview".into());
        options.azure_deployment_name = Some("contract-deployment".into());
    }
    options
}

fn empty_context() -> ai::api::conversation::Context {
    ai::api::conversation::Context {
        system_prompt: None,
        messages: Vec::new(),
        tools: None,
    }
}

fn read_http_request(stream: &mut std::net::TcpStream) -> String {
    let mut data = Vec::new();
    let mut buf = [0_u8; 1024];
    loop {
        let n = stream.read(&mut buf).unwrap();
        if n == 0 {
            break;
        }
        data.extend_from_slice(&buf[..n]);
        if data.windows(4).any(|w| w == b"\r\n\r\n") {
            break;
        }
    }

    let header_end = data
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .map(|pos| pos + 4)
        .unwrap_or(data.len());
    let headers = String::from_utf8_lossy(&data[..header_end]).to_string();
    let content_length = headers
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().ok())
                .flatten()
        })
        .unwrap_or(0);

    let already_read = data.len().saturating_sub(header_end);
    let remaining = content_length.saturating_sub(already_read);
    if remaining > 0 {
        let mut body = vec![0_u8; remaining];
        stream.read_exact(&mut body).unwrap();
        data.extend_from_slice(&body);
    }

    String::from_utf8_lossy(&data).to_string()
}

fn response(status: u16, headers: &[(&str, &str)], body: &str) -> String {
    let reason = match status {
        200 => "OK",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        _ => "Status",
    };
    let mut out = format!(
        "HTTP/1.1 {} {}\r\ncontent-length: {}\r\n",
        status,
        reason,
        body.len()
    );
    for (name, value) in headers {
        out.push_str(name);
        out.push_str(": ");
        out.push_str(value);
        out.push_str("\r\n");
    }
    out.push_str("\r\n");
    out.push_str(body);
    out
}

#[test]
fn headers_merge_priority() {
    let model = serde_json::json!({"x-custom": "model-value"});
    let opts = serde_json::json!({"x-custom": "option-value"});
    let headers = merge_headers(Some(&model), Some(&opts), []);
    assert_eq!(
        headers.get("x-custom").map(String::as_str),
        Some("option-value")
    );
}

#[test]
fn headers_generated_preserved() {
    let opts = serde_json::json!({"x-extra": "extra"});
    let headers = merge_headers(
        None,
        Some(&opts),
        [("authorization".into(), "Bearer sk-test".into())],
    );
    assert_eq!(
        headers.get("authorization").map(String::as_str),
        Some("Bearer sk-test")
    );
    assert_eq!(headers.get("x-extra").map(String::as_str), Some("extra"));
}

#[test]
fn retry_config_defaults() {
    let cfg = RetryConfig::from_options(None);
    assert_eq!(cfg.max_retries, 0);
    assert_eq!(cfg.max_retry_delay_ms, 10_000);
    assert_eq!(cfg.timeout_ms, None);
}

#[test]
fn retry_config_from_options() {
    let opts = StreamOptions {
        max_retries: Some(1),
        max_retry_delay_ms: Some(5_000),
        timeout_ms: Some(30_000),
        ..Default::default()
    };
    let cfg = RetryConfig::from_options(Some(&opts));
    assert_eq!(cfg.max_retries, 1);
    assert_eq!(cfg.max_retry_delay_ms, 5_000);
    assert_eq!(cfg.timeout_ms, Some(30_000));
}

#[test]
fn provider_option_matrix_rejects_inert_explicit_options() {
    const SSE_APIS: &[&str] = &[
        "anthropic-messages",
        "openai-completions",
        "openai-responses",
        "azure-openai-responses",
        "google-generative-ai",
        "mistral-conversations",
        "openai-codex-responses",
    ];
    for api in SSE_APIS {
        let options = StreamOptions {
            transport: Some("sse".into()),
            ..Default::default()
        };
        assert!(ai::transport::http::validate_options(api, Some(&options)).is_ok());
        let options = StreamOptions {
            transport: Some("websocket".into()),
            ..Default::default()
        };
        assert!(ai::transport::http::validate_options(api, Some(&options)).is_err());
    }

    for api in [
        "openai-responses",
        "azure-openai-responses",
        "mistral-conversations",
        "openai-codex-responses",
    ] {
        let options = StreamOptions {
            session_id: Some("session-1".into()),
            ..Default::default()
        };
        assert!(ai::transport::http::validate_options(api, Some(&options)).is_ok());
    }
    let unsupported_session = StreamOptions {
        session_id: Some("session-1".into()),
        ..Default::default()
    };
    assert!(
        ai::transport::http::validate_options("google-generative-ai", Some(&unsupported_session))
            .is_err()
    );

    let azure = StreamOptions {
        azure_resource_name: Some("resource".into()),
        ..Default::default()
    };
    assert!(ai::transport::http::validate_options("azure-openai-responses", Some(&azure)).is_ok());
    assert!(ai::transport::http::validate_options("openai-responses", Some(&azure)).is_err());

    let invalid_headers = StreamOptions {
        headers: Some(serde_json::json!({"x-value": 3})),
        ..Default::default()
    };
    assert!(
        ai::transport::http::validate_options("openai-responses", Some(&invalid_headers)).is_err()
    );
}

#[tokio::test]
async fn every_builtin_provider_uses_the_shared_non_retryable_http_failure_contract() {
    let registry = ProviderRegistry::new();
    register_builtins_into(&registry);
    assert_eq!(
        registry.registered_apis(),
        builtin_provider_apis()
            .iter()
            .map(|api| (*api).to_string())
            .collect::<Vec<_>>()
    );

    for api in builtin_provider_apis() {
        let (base_url, requests) = start_http_server(vec![response(400, &[], "rejected")]);
        let provider = registry.lookup(api).unwrap();
        let model = contract_model(api, base_url.clone());
        let events = provider
            .stream(
                &model,
                empty_context(),
                Some(contract_options(api, &base_url)),
            )
            .collect::<Vec<_>>()
            .await;
        assert_eq!(events.len(), 1, "{api} emitted {events:?}");
        assert!(
            matches!(events[0], AssistantMessageEvent::Error { .. }),
            "{api} emitted {events:?}"
        );
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, AssistantMessageEvent::Done { .. })),
            "{api} reported HTTP failure as success"
        );
        requests
            .recv_timeout(std::time::Duration::from_secs(1))
            .unwrap_or_else(|_| panic!("{api} did not issue the expected local request"));
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn every_builtin_provider_cancels_a_stalled_send_with_one_aborted_terminal() {
    let registry = ProviderRegistry::new();
    register_builtins_into(&registry);

    for api in builtin_provider_apis() {
        let (base_url, started, release) = start_observed_stalled_http_server();
        let provider = registry.lookup(api).unwrap();
        let model = contract_model(api, base_url.clone());
        let cancellation = CancellationToken::new();
        let mut options = contract_options(api, &base_url);
        options.cancel = Some(cancellation.clone());
        let task = tokio::spawn(async move {
            provider
                .stream(&model, empty_context(), Some(options))
                .collect::<Vec<_>>()
                .await
        });
        started
            .recv_timeout(std::time::Duration::from_secs(1))
            .unwrap_or_else(|_| panic!("{api} did not enter the stalled request"));
        cancellation.cancel();
        let events = tokio::time::timeout(std::time::Duration::from_secs(1), task)
            .await
            .unwrap_or_else(|_| panic!("{api} cancellation did not wake the request"))
            .unwrap();
        assert_eq!(events.len(), 1, "{api} emitted {events:?}");
        assert!(
            matches!(
                &events[0],
                AssistantMessageEvent::Error {
                    reason: StopReason::Aborted,
                    ..
                }
            ),
            "{api} emitted {events:?}"
        );
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, AssistantMessageEvent::Done { .. })),
            "{api} reported cancellation as success"
        );
        let _ = release.send(());
    }
}

#[tokio::test]
async fn every_builtin_provider_retries_and_applies_hooks_to_the_actual_attempts() {
    let registry = ProviderRegistry::new();
    register_builtins_into(&registry);

    for api in builtin_provider_apis() {
        let (base_url, requests) = start_http_server(vec![
            response(500, &[("x-attempt", "first")], "retry"),
            response(400, &[("x-attempt", "second")], "stop"),
        ]);
        let (response_tx, response_rx) = mpsc::channel();
        let hooks = ProviderStreamHooks {
            on_payload: Some(Arc::new(|_, mut payload| {
                Box::pin(async move {
                    payload["contract_hook"] = serde_json::json!(true);
                    Ok(payload)
                })
            })),
            on_response: Some(Arc::new(move |response| {
                let response_tx = response_tx.clone();
                Box::pin(async move {
                    response_tx.send(response.status).unwrap();
                    Ok(())
                })
            })),
        };
        let provider = registry.lookup(api).unwrap();
        let model = contract_model(api, base_url.clone());
        let mut options = contract_options(api, &base_url);
        options.max_retries = Some(1);
        options.hooks = Some(hooks);
        let events = provider
            .stream(&model, empty_context(), Some(options))
            .collect::<Vec<_>>()
            .await;
        assert_eq!(events.len(), 1, "{api} emitted {events:?}");
        assert!(matches!(events[0], AssistantMessageEvent::Error { .. }));

        let first = requests
            .recv_timeout(std::time::Duration::from_secs(1))
            .unwrap_or_else(|_| panic!("{api} did not issue its first attempt"));
        let second = requests
            .recv_timeout(std::time::Duration::from_secs(1))
            .unwrap_or_else(|_| panic!("{api} did not issue its retry"));
        assert!(first.contains(r#""contract_hook":true"#), "{api}: {first}");
        assert!(
            second.contains(r#""contract_hook":true"#),
            "{api}: {second}"
        );
        assert_eq!(
            [
                response_rx
                    .recv_timeout(std::time::Duration::from_secs(1))
                    .unwrap(),
                response_rx
                    .recv_timeout(std::time::Duration::from_secs(1))
                    .unwrap(),
            ],
            [Some(500), Some(400)],
            "{api} did not invoke the response hook once per attempt"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn every_builtin_provider_applies_one_deadline_to_a_stalled_send() {
    let registry = ProviderRegistry::new();
    register_builtins_into(&registry);

    for api in builtin_provider_apis() {
        let (base_url, started, release) = start_observed_stalled_http_server();
        let provider = registry.lookup(api).unwrap();
        let model = contract_model(api, base_url.clone());
        let mut options = contract_options(api, &base_url);
        options.timeout_ms = Some(100);
        let task = tokio::spawn(async move {
            provider
                .stream(&model, empty_context(), Some(options))
                .collect::<Vec<_>>()
                .await
        });
        started
            .recv_timeout(std::time::Duration::from_secs(1))
            .unwrap_or_else(|_| panic!("{api} did not enter the stalled request"));
        let events = tokio::time::timeout(std::time::Duration::from_secs(1), task)
            .await
            .unwrap_or_else(|_| panic!("{api} timeout did not wake the request"))
            .unwrap();
        assert_eq!(events.len(), 1, "{api} emitted {events:?}");
        assert!(
            matches!(
                &events[0],
                AssistantMessageEvent::Error {
                    reason: StopReason::Error,
                    ..
                }
            ),
            "{api} emitted {events:?}"
        );
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, AssistantMessageEvent::Done { .. })),
            "{api} reported timeout as success"
        );
        let _ = release.send(());
    }
}

#[tokio::test]
async fn every_builtin_provider_rejects_missing_credentials_before_network_io() {
    let registry = ProviderRegistry::new();
    register_builtins_into(&registry);

    for api in builtin_provider_apis().iter().copied() {
        let provider = registry.lookup(api).unwrap();
        let model = contract_model(api, "http://127.0.0.1:1".into());
        let events = provider
            .stream(&model, empty_context(), None)
            .collect::<Vec<_>>()
            .await;
        assert_eq!(events.len(), 1, "{api} emitted {events:?}");
        assert!(matches!(events[0], AssistantMessageEvent::Error { .. }));
    }
}

#[tokio::test]
async fn response_hook_receives_safe_status_metadata() {
    let (url, _requests) = start_http_server(vec![response(
        200,
        &[
            ("content-type", "text/event-stream"),
            ("x-request-id", "req-123"),
        ],
        "",
    )]);
    let client = reqwest::Client::new();
    let model = test_model();
    let (tx, rx) = mpsc::channel::<ProviderResponseInfo>();
    let hooks = ProviderStreamHooks {
        on_payload: None,
        on_response: Some(Arc::new(move |info| {
            let tx = tx.clone();
            Box::pin(async move {
                tx.send(info).unwrap();
                Ok(())
            })
        })),
    };
    let opts = StreamOptions {
        hooks: Some(hooks),
        ..Default::default()
    };

    let stream = send_json_stream(
        &client,
        &model,
        Some(&opts),
        "openai-responses",
        client.post(&url),
        serde_json::json!({"message": "hello"}),
        |_body, _model, _cancel| done_stream("openai-responses"),
    );

    complete(stream).await.unwrap();
    let info = rx.try_recv().unwrap();
    assert_eq!(info.status, Some(200));
    assert_eq!(
        serde_json::to_value(info).unwrap(),
        serde_json::json!({"status": 200})
    );
}

#[tokio::test]
async fn non_success_body_and_headers_never_cross_the_transport_error_boundary() {
    const CANARY: &str = "ai_SECRET_CANARY_7f924c";
    let (url, _requests) = start_http_server(vec![response(
        401,
        &[
            ("authorization", CANARY),
            ("set-cookie", CANARY),
            ("x-request-id", CANARY),
        ],
        &format!("provider echoed credential: {CANARY}"),
    )]);
    let client = reqwest::Client::new();
    let model = test_model();
    let options = StreamOptions {
        api_key: Some(CANARY.to_string()),
        ..Default::default()
    };

    let events = send_json_stream(
        &client,
        &model,
        Some(&options),
        "openai-responses",
        client.post(&url),
        serde_json::json!({"message": "hello"}),
        |_body, _model, _cancel| done_stream("openai-responses"),
    )
    .collect::<Vec<_>>()
    .await;

    assert_eq!(events.len(), 1);
    let rendered = format!("{events:?}");
    assert!(!rendered.contains(CANARY), "{rendered}");
    assert!(rendered.contains("status 401"), "{rendered}");
}

#[tokio::test]
async fn network_and_hook_failures_do_not_project_raw_error_strings() {
    const CANARY: &str = "ai_NETWORK_CANARY_c81e";
    let client = reqwest::Client::new();
    let model = test_model();
    let network_events = send_json_stream(
        &client,
        &model,
        None,
        "openai-responses",
        client.post(format!("http://127.0.0.1:1/fail?token={CANARY}")),
        serde_json::json!({}),
        |_body, _model, _cancel| done_stream("openai-responses"),
    )
    .collect::<Vec<_>>()
    .await;
    let network_rendered = format!("{network_events:?}");
    assert!(!network_rendered.contains(CANARY), "{network_rendered}");
    assert!(
        network_rendered.contains("HTTP request failed"),
        "{network_rendered}"
    );

    let (url, _requests) = start_http_server(vec![response(
        200,
        &[("content-type", "text/event-stream")],
        "",
    )]);
    let hooks = ProviderStreamHooks {
        on_payload: None,
        on_response: Some(Arc::new(|_info| {
            Box::pin(async { Err(CANARY.to_string()) })
        })),
    };
    let options = StreamOptions {
        hooks: Some(hooks),
        ..Default::default()
    };
    let hook_events = send_json_stream(
        &client,
        &model,
        Some(&options),
        "openai-responses",
        client.post(&url),
        serde_json::json!({}),
        |_body, _model, _cancel| done_stream("openai-responses"),
    )
    .collect::<Vec<_>>()
    .await;
    let hook_rendered = format!("{hook_events:?}");
    assert!(!hook_rendered.contains(CANARY), "{hook_rendered}");
    assert!(
        hook_rendered.contains("Response hook failed"),
        "{hook_rendered}"
    );
}

#[test]
fn transport_source_never_collects_an_unbounded_error_body_or_raw_header_map() {
    let transport = include_str!("../transport/http.rs");
    let hooks = include_str!("../protocol/hooks.rs");

    assert!(!transport.contains("response.text()"));
    assert!(!transport.contains("headers_to_json"));
    assert!(!hooks.contains("pub headers:"));
}

#[tokio::test]
async fn retry_after_above_cap_errors_without_retrying() {
    let (url, requests) = start_http_server(vec![
        response(429, &[("retry-after", "5")], "slow down"),
        response(200, &[("content-type", "text/event-stream")], ""),
    ]);
    let client = reqwest::Client::new();
    let model = test_model();
    let opts = StreamOptions {
        max_retries: Some(1),
        max_retry_delay_ms: Some(1_000),
        ..Default::default()
    };

    let stream = send_json_stream(
        &client,
        &model,
        Some(&opts),
        "openai-responses",
        client.post(&url),
        serde_json::json!({"message": "hello"}),
        |_body, _model, _cancel| done_stream("openai-responses"),
    );

    let err = complete(stream).await.unwrap_err();
    assert!(err.contains("Retry-After 5000ms exceeds max_retry_delay_ms 1000"));
    requests.try_recv().unwrap();
    assert!(requests.try_recv().is_err());
}

#[tokio::test]
async fn explicit_retry_retries_retryable_status_once() {
    let (url, requests) = start_http_server(vec![
        response(500, &[], "temporary failure"),
        response(200, &[("content-type", "text/event-stream")], ""),
    ]);
    let client = reqwest::Client::new();
    let model = test_model();
    let opts = StreamOptions {
        max_retries: Some(1),
        ..Default::default()
    };

    let stream = send_json_stream(
        &client,
        &model,
        Some(&opts),
        "openai-responses",
        client.post(&url),
        serde_json::json!({"message": "hello"}),
        |_body, _model, _cancel| done_stream("openai-responses"),
    );

    complete(stream).await.unwrap();
    requests.try_recv().unwrap();
    requests.try_recv().unwrap();
}

#[tokio::test]
async fn authenticated_transport_never_follows_cross_origin_redirects() {
    let (redirect_target, target_requests) = start_http_server(vec![response(
        200,
        &[("content-type", "text/event-stream")],
        "",
    )]);
    let (origin, origin_requests) =
        start_http_server(vec![response(307, &[("location", &redirect_target)], "")]);
    let client = crate::transport::client::authenticated_client();
    let model = test_model();
    let secret = "redirect-secret-canary";
    let stream = send_json_stream(
        &client,
        &model,
        None,
        "openai-responses",
        client
            .post(&origin)
            .bearer_auth(secret)
            .header("x-api-key", secret),
        serde_json::json!({"message": "hello"}),
        |_body, _model, _cancel| done_stream("openai-responses"),
    );

    let error = complete(stream).await.unwrap_err();
    assert!(error.contains("307"));
    let first_request = origin_requests.try_recv().unwrap();
    assert!(first_request.contains(secret));
    assert!(target_requests.try_recv().is_err());
}

#[test]
fn every_builtin_provider_uses_the_non_redirecting_authenticated_client() {
    let provider_sources = [
        include_str!("../providers/anthropic/mod.rs"),
        include_str!("../providers/azure_openai_responses/mod.rs"),
        include_str!("../providers/google/mod.rs"),
        include_str!("../providers/mistral/mod.rs"),
        include_str!("../providers/openai/completions/mod.rs"),
        include_str!("../providers/openai/responses/mod.rs"),
        include_str!("../providers/openai_codex_responses/mod.rs"),
    ];
    for source in provider_sources {
        assert!(source.contains("authenticated_client()"));
        assert!(!source.contains("reqwest::Client::new()"));
    }
    let client = include_str!("../transport/client.rs");
    assert!(client.contains("Policy::none()"));
}

#[tokio::test]
async fn payload_hook_mutates_actual_request_body() {
    let (url, requests) = start_http_server(vec![response(
        200,
        &[("content-type", "text/event-stream")],
        "",
    )]);
    let client = reqwest::Client::new();
    let model = test_model();
    let hooks = ProviderStreamHooks {
        on_payload: Some(Arc::new(|_model, mut payload| {
            Box::pin(async move {
                payload["message"] = serde_json::json!("hooked");
                Ok(payload)
            })
        })),
        on_response: None,
    };
    let opts = StreamOptions {
        hooks: Some(hooks),
        ..Default::default()
    };

    let stream = send_json_stream(
        &client,
        &model,
        Some(&opts),
        "openai-responses",
        client.post(&url),
        serde_json::json!({"message": "original"}),
        |_body, _model, _cancel| done_stream("openai-responses"),
    );

    complete(stream).await.unwrap();
    let request = requests.try_recv().unwrap();
    assert!(request.contains(r#""message":"hooked""#), "{request}");
    assert!(!request.contains(r#""message":"original""#), "{request}");
}

#[tokio::test]
async fn cancellation_wakes_a_stalled_request_send() {
    let (url, release) = start_stalled_http_server();
    let client = reqwest::Client::new();
    let model = test_model();
    let cancel = CancellationToken::new();
    let opts = StreamOptions {
        cancel: Some(cancel.clone()),
        ..Default::default()
    };
    let stream = send_json_stream(
        &client,
        &model,
        Some(&opts),
        "openai-responses",
        client.post(&url),
        serde_json::json!({}),
        |_body, _model, _cancel| done_stream("openai-responses"),
    );
    let task = tokio::spawn(complete(stream));
    tokio::task::yield_now().await;
    cancel.cancel();
    let error = tokio::time::timeout(std::time::Duration::from_secs(1), task)
        .await
        .expect("cancellation should wake request send")
        .unwrap()
        .unwrap_err();
    assert!(error.contains("cancelled"));
    let _ = release.send(());
}

#[tokio::test]
async fn one_deadline_covers_stalled_send_hook_and_body() {
    let (url, release) = start_stalled_http_server();
    let client = reqwest::Client::new();
    let model = test_model();
    let opts = StreamOptions {
        timeout_ms: Some(10),
        ..Default::default()
    };
    let stream = send_json_stream(
        &client,
        &model,
        Some(&opts),
        "openai-responses",
        client.post(&url),
        serde_json::json!({}),
        |_body, _model, _cancel| done_stream("openai-responses"),
    );
    assert!(complete(stream).await.unwrap_err().contains("10ms"));
    let _ = release.send(());

    let hooks = ProviderStreamHooks {
        on_payload: Some(Arc::new(|_, _| Box::pin(futures::future::pending()))),
        on_response: None,
    };
    let opts = StreamOptions {
        timeout_ms: Some(10),
        hooks: Some(hooks),
        ..Default::default()
    };
    let stream = send_json_stream(
        &client,
        &model,
        Some(&opts),
        "openai-responses",
        client.post("http://127.0.0.1:1"),
        serde_json::json!({}),
        |_body, _model, _cancel| done_stream("openai-responses"),
    );
    assert!(complete(stream).await.unwrap_err().contains("10ms"));

    let (url, _requests) = start_http_server(vec![response(200, &[], "")]);
    let opts = StreamOptions {
        timeout_ms: Some(10),
        ..Default::default()
    };
    let stream = send_json_stream(
        &client,
        &model,
        Some(&opts),
        "openai-responses",
        client.post(&url),
        serde_json::json!({}),
        |_body, _model, _cancel| pending_stream(),
    );
    assert!(complete(stream).await.unwrap_err().contains("10ms"));
}

#[tokio::test]
async fn cancellation_wakes_pending_hooks_and_body_streams() {
    for pending_hook in [true, false] {
        let client = reqwest::Client::new();
        let model = test_model();
        let cancel = CancellationToken::new();
        let hooks = pending_hook.then(|| ProviderStreamHooks {
            on_payload: Some(Arc::new(|_, _| Box::pin(futures::future::pending()))),
            on_response: None,
        });
        let server = (!pending_hook).then(|| start_http_server(vec![response(200, &[], "")]));
        let url = server
            .as_ref()
            .map(|(url, _)| url.as_str())
            .unwrap_or("http://127.0.0.1:1");
        let opts = StreamOptions {
            cancel: Some(cancel.clone()),
            hooks,
            ..Default::default()
        };
        let stream = send_json_stream(
            &client,
            &model,
            Some(&opts),
            "openai-responses",
            client.post(url),
            serde_json::json!({}),
            move |_body, _model, _cancel| {
                if pending_hook {
                    done_stream("openai-responses")
                } else {
                    pending_stream()
                }
            },
        );
        let task = tokio::spawn(complete(stream));
        tokio::task::yield_now().await;
        cancel.cancel();
        let error = tokio::time::timeout(std::time::Duration::from_secs(1), task)
            .await
            .expect("cancellation should wake the pending phase")
            .unwrap()
            .unwrap_err();
        assert!(error.contains("cancelled"));
    }
}

#[tokio::test]
async fn zero_timeout_is_an_immediate_end_to_end_deadline() {
    let client = reqwest::Client::new();
    let model = test_model();
    let opts = StreamOptions {
        timeout_ms: Some(0),
        ..Default::default()
    };
    let stream = send_json_stream(
        &client,
        &model,
        Some(&opts),
        "openai-responses",
        client.post("http://127.0.0.1:1"),
        serde_json::json!({}),
        |_body, _model, _cancel| done_stream("openai-responses"),
    );
    assert!(complete(stream).await.unwrap_err().contains("0ms"));
}
// Internal transport-contract tests.
