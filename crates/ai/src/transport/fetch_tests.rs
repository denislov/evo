//! End-to-end tests of the safe fetch pipeline against a local HTTP server.
//! Loopback targets are permitted through the `test-support` build so these
//! tests exercise real sockets; every security-relevant negative test runs
//! with the strict client and asserts that no request ever reaches the peer.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::transport::fetch::cache::CacheConfig;
use crate::transport::fetch::convert::OutputFormat;
use crate::transport::fetch::errors::FetchErrorKind;
use crate::transport::fetch::resolve::{DnsResolver, ResolveFuture};
use crate::transport::fetch::{FetchClient, FetchClientConfig, FetchRequest};

struct MockServer {
    url: String,
    requests: Arc<AtomicUsize>,
    _task: tokio::task::JoinHandle<()>,
}

impl MockServer {
    /// Each entry is `(status, content_type, body)`. The last entry repeats
    /// for subsequent requests; `None` content type omits the header.
    async fn spawn(responses: Vec<(u16, Option<&'static str>, &'static str)>) -> Self {
        Self::spawn_impl(responses, true).await
    }

    /// Like [`Self::spawn`], but responses carry no Content-Length header so
    /// the connection close frames the body. This exercises the streaming
    /// truncation path instead of the declared-length rejection path.
    async fn spawn_chunked(responses: Vec<(u16, Option<&'static str>, &'static str)>) -> Self {
        Self::spawn_impl(responses, false).await
    }

    async fn spawn_impl(
        responses: Vec<(u16, Option<&'static str>, &'static str)>,
        declared_length: bool,
    ) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(AtomicUsize::new(0));
        let responses = Arc::new(Mutex::new(responses));
        let task = tokio::spawn({
            let requests = requests.clone();
            let responses = responses.clone();
            async move {
                loop {
                    let Ok((mut socket, _)) = listener.accept().await else {
                        break;
                    };
                    let requests = requests.clone();
                    let responses = responses.clone();
                    tokio::spawn(async move {
                        let mut buf = Vec::new();
                        let mut chunk = [0u8; 1024];
                        loop {
                            let n = socket.read(&mut chunk).await.unwrap_or(0);
                            if n == 0 {
                                return;
                            }
                            buf.extend_from_slice(&chunk[..n]);
                            if buf.windows(4).any(|w| w == b"\r\n\r\n") {
                                break;
                            }
                        }
                        requests.fetch_add(1, Ordering::SeqCst);
                        let (status, content_type, body) = {
                            // Keep one guard alive for the whole selection:
                            // a second `lock()` while the temporary guard of
                            // the outer expression is still alive would
                            // deadlock on a non-reentrant std Mutex.
                            let responses = responses.lock().unwrap();
                            responses
                                .get(requests.load(Ordering::SeqCst) - 1)
                                .cloned()
                                .unwrap_or_else(|| responses.last().cloned().unwrap())
                        };
                        let reason = match status {
                            200 => "OK",
                            301 => "Moved Permanently",
                            302 => "Found",
                            404 => "Not Found",
                            500 => "Internal Server Error",
                            _ => "X",
                        };
                        let content_type = content_type
                            .map(|value| format!("content-type: {value}\r\n"))
                            .unwrap_or_default();
                        let length = if declared_length {
                            format!("content-length: {}\r\n", body.len())
                        } else {
                            String::new()
                        };
                        let response = format!(
                            "HTTP/1.1 {status} {reason}\r\nconnection: close\r\n{content_type}{length}\r\n{body}"
                        );
                        socket.write_all(response.as_bytes()).await.unwrap_or(());
                    });
                }
            }
        });
        Self {
            url,
            requests,
            _task: task,
        }
    }

    fn request_count(&self) -> usize {
        self.requests.load(Ordering::SeqCst)
    }
}

/// A server that redirects into the configured location chain, then serves.
async fn spawn_redirect_chain(locations: Vec<String>, final_body: &'static str) -> MockServer {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let requests = Arc::new(AtomicUsize::new(0));
    let task = tokio::spawn({
        let requests = requests.clone();
        let locations = locations.clone();
        let base = base.clone();
        async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    break;
                };
                let requests = requests.clone();
                let locations = locations.clone();
                let base = base.clone();
                tokio::spawn(async move {
                    let mut buf = Vec::new();
                    let mut chunk = [0u8; 1024];
                    loop {
                        let n = socket.read(&mut chunk).await.unwrap_or(0);
                        if n == 0 {
                            return;
                        }
                        buf.extend_from_slice(&chunk[..n]);
                        if buf.windows(4).any(|w| w == b"\r\n\r\n") {
                            break;
                        }
                    }
                    let index = requests.fetch_add(1, Ordering::SeqCst);
                    if index < locations.len() {
                        // An empty location redirects back to this server's
                        // `/start`, letting tests build an unbounded loop.
                        let location = if locations[index].is_empty() {
                            format!("{base}/start")
                        } else {
                            locations[index].clone()
                        };
                        let response = format!(
                            "HTTP/1.1 302 Found\r\nlocation: {location}\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
                        );
                        socket.write_all(response.as_bytes()).await.unwrap_or(());
                    } else {
                        let body = format!(
                            "HTTP/1.1 200 OK\r\ncontent-type: text/plain\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                            final_body.len(),
                            final_body
                        );
                        socket.write_all(body.as_bytes()).await.unwrap_or(());
                    }
                });
            }
        }
    });
    MockServer {
        url: format!("{base}/start"),
        requests,
        _task: task,
    }
}

fn testing_client() -> FetchClient {
    FetchClient::for_testing(FetchClientConfig::default())
}

fn strict_client() -> FetchClient {
    FetchClient::new(FetchClientConfig::default()).expect("default client builds")
}

async fn fetch_text(
    client: &FetchClient,
    url: &str,
) -> Result<String, crate::transport::fetch::errors::FetchError> {
    client
        .fetch(FetchRequest {
            url: url.into(),
            format: OutputFormat::Markdown,
            max_bytes: None,
        })
        .await
        .map(|result| result.text)
}

#[tokio::test]
async fn fetches_html_and_converts_to_markdown() {
    let server = MockServer::spawn(vec![(
        200,
        Some("text/html; charset=utf-8"),
        "<h1>Hello</h1><p>body <b>text</b></p>",
    )])
    .await;
    let client = testing_client();
    let text = fetch_text(&client, &server.url).await.unwrap();
    assert!(text.contains("Hello"), "unexpected markdown: {text}");
    assert!(text.contains("body"), "unexpected markdown: {text}");
    assert!(
        text.contains("**text**"),
        "bold must project to markdown: {text}"
    );
}

#[tokio::test]
async fn fetches_plain_text_verbatim() {
    let server =
        MockServer::spawn(vec![(200, Some("text/plain; charset=utf-8"), "raw text")]).await;
    let client = testing_client();
    let text = fetch_text(&client, &server.url).await.unwrap();
    assert_eq!(text, "raw text");
}

#[tokio::test]
async fn follows_redirects_and_revalidates_every_hop() {
    let target = MockServer::spawn(vec![(200, Some("text/plain"), "landed")]).await;
    let server = spawn_redirect_chain(vec![target.url.clone()], "never").await;
    let client = testing_client();
    let text = fetch_text(&client, &server.url).await.unwrap();
    assert_eq!(text, "landed");
    assert_eq!(server.request_count(), 1);
    assert_eq!(target.request_count(), 1);
}

#[tokio::test]
async fn redirect_beyond_limit_is_rejected() {
    // Empty locations redirect back to this server's own `/start`, forming an
    // unbounded redirect loop that is always reachable; only the hop budget
    // can terminate it.
    let server = spawn_redirect_chain(
        vec!["".to_string(), "".to_string(), "".to_string()],
        "never",
    )
    .await;
    let client = FetchClient::for_testing(FetchClientConfig {
        max_redirects: 2,
        ..FetchClientConfig::default()
    });
    let error = fetch_text(&client, &server.url).await.unwrap_err();
    assert_eq!(error.kind, FetchErrorKind::RedirectLimit);
}

#[tokio::test]
async fn redirect_into_blocked_address_is_rejected_before_connecting() {
    let server = spawn_redirect_chain(vec!["http://127.0.0.1:9/secret".to_string()], "never").await;
    let client = strict_client();
    let error = fetch_text(&client, &server.url).await.unwrap_err();
    assert_eq!(error.kind, FetchErrorKind::SsrfBlocked);
    assert!(
        error.message.contains("127.0.0.1"),
        "unexpected: {}",
        error.message
    );
}

#[tokio::test]
async fn loopback_target_is_blocked_and_never_contacted() {
    let server = MockServer::spawn(vec![(200, Some("text/plain"), "should not be served")]).await;
    let client = strict_client();
    let error = fetch_text(&client, &server.url).await.unwrap_err();
    assert_eq!(error.kind, FetchErrorKind::SsrfBlocked);
    assert_eq!(
        server.request_count(),
        0,
        "blocked target must never be contacted"
    );
}

#[tokio::test]
async fn rfc1918_and_metadata_targets_are_blocked() {
    let client = strict_client();
    for url in [
        "http://10.0.0.1/",
        "http://172.16.0.1/",
        "http://192.168.1.1/",
        "http://169.254.169.254/latest/meta-data/",
        "http://[::1]/",
        "http://[::ffff:127.0.0.1]/",
    ] {
        let error = fetch_text(&client, url).await.unwrap_err();
        assert_eq!(
            error.kind,
            FetchErrorKind::SsrfBlocked,
            "url {url} must be blocked"
        );
        assert_eq!(
            error.details.as_ref().unwrap()["reason"],
            match url {
                "http://10.0.0.1/" | "http://172.16.0.1/" | "http://192.168.1.1/" =>
                    "RFC1918 private address",
                "http://169.254.169.254/latest/meta-data/" => "cloud metadata endpoint",
                "http://[::1]/" => "loopback address",
                _ => "IPv4-mapped IPv6 address",
            },
            "url {url} must report the exact policy hit"
        );
    }
}

#[tokio::test]
async fn connection_is_pinned_to_the_validated_address() {
    // The redirect target `rebind.example` is resolved exactly once per hop.
    // The first lookup yields a public record the policy accepts; a second
    // lookup — which a "validate then let the client re-resolve" design would
    // perform — yields the metadata address and must never happen.
    #[derive(Clone)]
    struct RebindingResolver(Arc<Mutex<Vec<Vec<std::net::IpAddr>>>>);
    impl DnsResolver for RebindingResolver {
        fn lookup(&self, _host: &str, _port: u16) -> ResolveFuture {
            let records = self.0.clone();
            Box::pin(async move {
                let mut records = records.lock().unwrap();
                if records.len() == 1 {
                    return Ok(records[0].clone());
                }
                Ok(records.remove(0))
            })
        }
    }
    let target = MockServer::spawn(vec![(200, Some("text/plain"), "pinned")]).await;
    let target_port = url::Url::parse(&target.url).unwrap().port().unwrap();
    let redirect = format!("http://rebind.example:{target_port}/pinned");
    let server = spawn_redirect_chain(vec![redirect], "never").await;
    let resolver = RebindingResolver(Arc::new(Mutex::new(vec![
        vec!["127.0.0.1".parse().unwrap()],
        vec!["169.254.169.254".parse().unwrap()],
    ])));
    let client = testing_client().with_shared_state(Arc::new(resolver));
    let text = fetch_text(&client, &server.url).await.unwrap();
    assert_eq!(text, "pinned");
    assert_eq!(
        target.request_count(),
        1,
        "hop 2 must dial the validated address, never re-resolve"
    );
}

#[tokio::test]
async fn mixed_dns_records_fail_closed() {
    #[derive(Clone)]
    struct MixedResolver;
    impl DnsResolver for MixedResolver {
        fn lookup(&self, _host: &str, _port: u16) -> ResolveFuture {
            Box::pin(async {
                Ok(vec![
                    "93.184.216.34".parse().unwrap(),
                    "127.0.0.1".parse().unwrap(),
                ])
            })
        }
    }
    let client = strict_client().with_shared_state(Arc::new(MixedResolver));
    let error = client
        .fetch(FetchRequest {
            url: "http://mixed.example/".into(),
            format: OutputFormat::Markdown,
            max_bytes: None,
        })
        .await
        .unwrap_err();
    assert_eq!(error.kind, FetchErrorKind::SsrfBlocked);
    assert!(
        error.message.contains("127.0.0.1"),
        "unexpected: {}",
        error.message
    );
}

#[tokio::test]
async fn declared_content_length_over_budget_fails_before_reading() {
    let server = MockServer::spawn(vec![(200, Some("text/plain"), "x".repeat(100).leak())]).await;
    let client = testing_client();
    let error = client
        .fetch(FetchRequest {
            url: server.url.clone(),
            format: OutputFormat::Markdown,
            max_bytes: Some(50),
        })
        .await
        .unwrap_err();
    assert_eq!(error.kind, FetchErrorKind::ContentLengthOverBudget);
    assert_eq!(error.details.unwrap()["declaredBytes"], 100);
}

#[tokio::test]
async fn streaming_body_over_budget_is_truncated_and_reported() {
    // No Content-Length header, so the connection close frames the body and
    // the streaming truncation path (not the declared-length rejection) runs.
    let server =
        MockServer::spawn_chunked(vec![(200, Some("text/plain"), "x".repeat(4096).leak())]).await;
    let client = testing_client();
    let result = client
        .fetch(FetchRequest {
            url: server.url,
            format: OutputFormat::Markdown,
            max_bytes: Some(1024),
        })
        .await
        .unwrap();
    assert!(result.truncated, "oversized body must be marked truncated");
    assert!(result.text.len() <= 1024);
}

#[tokio::test]
async fn non_html_media_is_rejected() {
    let server = MockServer::spawn(vec![(200, Some("application/pdf"), "%PDF-1.4 fake")]).await;
    let client = testing_client();
    let error = fetch_text(&client, &server.url).await.unwrap_err();
    assert!(error.message.contains("application/pdf"));
}

#[tokio::test]
async fn cache_hits_never_touch_the_network_again() {
    let server = MockServer::spawn(vec![(200, Some("text/plain"), "cached body")]).await;
    let client = testing_client();
    let first = fetch_text(&client, &server.url).await.unwrap();
    let second = fetch_text(&client, &server.url).await.unwrap();
    assert_eq!(first, "cached body");
    assert_eq!(second, "cached body");
    assert_eq!(server.request_count(), 1, "cache hit must not re-request");
}

#[tokio::test]
async fn truncated_results_are_not_cached() {
    let server =
        MockServer::spawn_chunked(vec![(200, Some("text/plain"), "x".repeat(4096).leak())]).await;
    let client = testing_client();
    for _ in 0..2 {
        let result = client
            .fetch(FetchRequest {
                url: server.url.clone(),
                format: OutputFormat::Markdown,
                max_bytes: Some(1024),
            })
            .await
            .unwrap();
        assert!(result.truncated);
    }
    assert_eq!(
        server.request_count(),
        2,
        "truncated results must not be cached"
    );
}

#[tokio::test]
async fn failed_fetches_are_not_cached() {
    let server = MockServer::spawn(vec![(404, Some("text/plain"), "missing")]).await;
    let client = testing_client();
    let first = fetch_text(&client, &server.url).await.unwrap_err();
    assert_eq!(first.kind, FetchErrorKind::HttpStatus);
    let second = fetch_text(&client, &server.url).await.unwrap_err();
    assert_eq!(second.kind, FetchErrorKind::HttpStatus);
    assert_eq!(
        server.request_count(),
        2,
        "failed fetches must not be cached"
    );
}

#[tokio::test]
async fn resolve_timeout_is_budgeted() {
    #[derive(Clone)]
    struct SlowResolver;
    impl DnsResolver for SlowResolver {
        fn lookup(&self, _host: &str, _port: u16) -> ResolveFuture {
            Box::pin(async {
                tokio::time::sleep(std::time::Duration::from_secs(30)).await;
                Ok(vec![])
            })
        }
    }
    let client = FetchClient::for_testing(FetchClientConfig {
        resolve_timeout: std::time::Duration::from_millis(50),
        ..FetchClientConfig::default()
    })
    .with_shared_state(Arc::new(SlowResolver));
    let error = client
        .fetch(FetchRequest {
            url: "http://slow.example/".into(),
            format: OutputFormat::Markdown,
            max_bytes: None,
        })
        .await
        .unwrap_err();
    assert_eq!(error.kind, FetchErrorKind::ResolveTimeout);
}

#[tokio::test]
async fn connect_failures_map_to_transport_errors() {
    // Deterministic replacement for a previously network-dependent test: the
    // resolver pins a domain to 127.0.0.1:1 (outside the ephemeral port range,
    // so no concurrent MockServer can take it), and the connect must fail
    // with ECONNREFUSED instead of relying on unroutable TEST-NET addresses
    // whose outcome depends on the kernel.
    #[derive(Clone)]
    struct LocalOnlyResolver;
    impl DnsResolver for LocalOnlyResolver {
        fn lookup(&self, _host: &str, _port: u16) -> ResolveFuture {
            Box::pin(async { Ok(vec![std::net::IpAddr::from([127, 0, 0, 1])]) })
        }
    }
    let client = FetchClient::for_testing(FetchClientConfig::default())
        .with_shared_state(Arc::new(LocalOnlyResolver));
    let error = client
        .fetch(FetchRequest {
            url: "http://refused.invalid:1/".into(),
            format: OutputFormat::Markdown,
            max_bytes: None,
        })
        .await
        .unwrap_err();
    assert_eq!(
        error.kind,
        FetchErrorKind::Transport,
        "connect to a closed local port must fail, got {:?}: {}",
        error.kind,
        error.message
    );
}

#[tokio::test]
async fn cache_ttl_expiry_forces_a_second_request() {
    let server = MockServer::spawn(vec![(200, Some("text/plain"), "tick")]).await;
    let client = FetchClient::for_testing(FetchClientConfig {
        cache: Some(CacheConfig {
            ttl: std::time::Duration::from_millis(20),
            ..CacheConfig::default()
        }),
        ..FetchClientConfig::default()
    });
    let first = fetch_text(&client, &server.url).await.unwrap();
    assert_eq!(first, "tick");
    tokio::time::sleep(std::time::Duration::from_millis(60)).await;
    let second = fetch_text(&client, &server.url).await.unwrap();
    assert_eq!(second, "tick");
    assert_eq!(
        server.request_count(),
        2,
        "expired entry must be re-fetched"
    );
}

#[tokio::test]
async fn text_format_projects_html_to_plain_text() {
    let server = MockServer::spawn(vec![(
        200,
        Some("text/html"),
        "<div><p>one</p><p>two</p></div>",
    )])
    .await;
    let client = testing_client();
    let result = client
        .fetch(FetchRequest {
            url: server.url,
            format: OutputFormat::Text,
            max_bytes: None,
        })
        .await
        .unwrap();
    assert!(
        result.text.contains("one") && result.text.contains("two"),
        "both paragraphs must be projected, got: {}",
        result.text
    );
}
