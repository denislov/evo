use std::sync::Arc;
use std::time::{Duration, SystemTime};

use http::header::{
    ACCEPT_ENCODING, CONTENT_ENCODING, CONTENT_LENGTH, CONTENT_TYPE, HOST, LOCATION, USER_AGENT,
};
use http::{Method, Request};
use url::Url;

use super::cache::{CacheConfig, FetchCache, cache_key};
use super::connector::{SafeConnector, server_name_for, tls_config};
use super::convert::{MediaKind, OutputFormat, convert_body, decode_body, media_kind};
use super::errors::{FetchError, FetchErrorKind};
use super::resolve::{DnsResolver, SystemResolver};
use super::ssrf::{BlockReason, validate_ip};

const USER_AGENT_VALUE: &str = concat!("Evo-WebFetch/", env!("CARGO_PKG_VERSION"));

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchClientConfig {
    /// Maximum redirects followed per fetch; every hop re-validates.
    pub max_redirects: usize,
    /// Budget for DNS resolution per hop.
    pub resolve_timeout: Duration,
    /// Budget for TCP connect and TLS handshake per hop.
    pub connect_timeout: Duration,
    /// Budget for the whole hop: request, headers, and body.
    pub total_timeout: Duration,
    /// Budget for HTML conversion.
    pub conversion_timeout: Duration,
    /// Default body byte budget when the caller does not override it.
    pub default_max_bytes: usize,
    /// Optional in-memory cache; a `None` value disables caching.
    pub cache: Option<CacheConfig>,
    /// Extra PEM root certificates trusted in addition to the system store.
    pub extra_ca_certificates: Vec<Vec<u8>>,
    /// Test-only escape hatch. Never set outside `cfg(test)` builds of the
    /// `test-support` feature; production binaries do not compile the field.
    #[cfg(feature = "test-support")]
    pub allow_loopback: bool,
}

impl Default for FetchClientConfig {
    fn default() -> Self {
        Self {
            max_redirects: 5,
            resolve_timeout: Duration::from_secs(5),
            connect_timeout: Duration::from_secs(10),
            total_timeout: Duration::from_secs(30),
            conversion_timeout: Duration::from_secs(10),
            default_max_bytes: 2 * 1024 * 1024,
            cache: Some(CacheConfig::default()),
            extra_ca_certificates: Vec::new(),
            #[cfg(feature = "test-support")]
            allow_loopback: false,
        }
    }
}

/// Test-only construction that permits loopback targets so integration tests
/// can run a local HTTP server.
#[cfg(feature = "test-support")]
impl FetchClient {
    pub fn for_testing(config: FetchClientConfig) -> Self {
        Self::new(FetchClientConfig {
            allow_loopback: true,
            ..config
        })
        .expect("test fetch client builds from system roots")
    }
}

pub struct FetchClient {
    config: FetchClientConfig,
    cache: Arc<FetchCache>,
    resolver: Arc<dyn DnsResolver>,
    tls: Arc<rustls::ClientConfig>,
}

impl std::fmt::Debug for FetchClient {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("FetchClient")
            .field("config", &self.config)
            .finish()
    }
}

impl FetchClient {
    pub fn new(config: FetchClientConfig) -> Result<Self, FetchError> {
        let tls = tls_config(&config.extra_ca_certificates)?;
        let cache = FetchCache::new(config.cache);
        Ok(Self {
            config,
            cache: Arc::new(cache),
            resolver: Arc::new(SystemResolver),
            tls,
        })
    }

    /// Inject a resolver (used by tests to simulate DNS without the network).
    #[cfg(test)]
    pub(crate) fn with_shared_state(mut self, resolver: Arc<dyn DnsResolver>) -> Self {
        self.resolver = resolver;
        self
    }

    pub async fn fetch(&self, request: FetchRequest) -> Result<FetchResult, FetchError> {
        let parsed = parse_and_validate_url(&request.url)?;
        let key = cache_key(normalize_url(parsed.clone()).as_str(), request.format);
        if let Some(cached) = self.cache.get(&key) {
            return Ok(FetchResult {
                from_cache: true,
                ..cached
            });
        }
        let max_bytes = request.max_bytes.unwrap_or(self.config.default_max_bytes);
        let mut current = parsed;
        for _hop in 0..=self.config.max_redirects {
            match self.fetch_hop(&current, max_bytes).await? {
                HopOutcome::Redirect { location } => {
                    let joined = current.join(&location).map_err(|error| {
                        FetchError::new(
                            FetchErrorKind::InvalidUrl,
                            format!("invalid redirect location `{location}`: {error}"),
                        )
                    })?;
                    current = parse_and_validate_url(joined.as_str())?;
                }
                HopOutcome::Response {
                    headers,
                    bytes,
                    truncated,
                } => {
                    let result = self
                        .finalize(current, headers, bytes, truncated, request.format)
                        .await?;
                    if !result.truncated {
                        self.cache.put(&key, result.clone());
                    }
                    return Ok(result);
                }
            }
        }
        Err(FetchError::new(
            FetchErrorKind::RedirectLimit,
            format!(
                "fetch exceeded the {} redirect limit",
                self.config.max_redirects
            ),
        ))
    }

    async fn fetch_hop(&self, url: &Url, max_bytes: usize) -> Result<HopOutcome, FetchError> {
        let host = url.host_str().ok_or_else(|| {
            FetchError::new(
                FetchErrorKind::InvalidUrl,
                format!("url `{url}` has no host"),
            )
        })?;
        let port = url.port_or_known_default().ok_or_else(|| {
            FetchError::new(
                FetchErrorKind::InvalidUrl,
                format!("url `{url}` has no port"),
            )
        })?;
        let addresses =
            tokio::time::timeout(self.config.resolve_timeout, self.resolve_host(host, port))
                .await
                .map_err(|_| {
                    FetchError::new(
                        FetchErrorKind::ResolveTimeout,
                        format!("DNS resolution for `{host}` exceeded its budget"),
                    )
                })??;
        let host_header = host_header(url, host);
        let server_name = server_name_for(host);
        let connector = SafeConnector {
            addresses: Arc::new(addresses),
            tls: (url.scheme() == "https")
                .then(|| Arc::new(tokio_rustls::TlsConnector::from(self.tls.clone()))),
            server_name,
            connect_timeout: self.config.connect_timeout,
        };
        let origin_form = origin_form(url);
        let request = Request::builder()
            .method(Method::GET)
            .uri(origin_form)
            .header(HOST, host_header)
            .header(ACCEPT_ENCODING, "identity")
            .header(USER_AGENT, USER_AGENT_VALUE)
            .body(http_body_util::Full::new(bytes::Bytes::new()))
            .map_err(|error| {
                FetchError::new(
                    FetchErrorKind::Transport,
                    format!("failed to build request: {error}"),
                )
            })?;
        tokio::time::timeout(self.config.total_timeout, async {
            let stream = connector.connect().await.map_err(|error| {
                let kind = if error.kind() == std::io::ErrorKind::TimedOut {
                    FetchErrorKind::ConnectTimeout
                } else {
                    FetchErrorKind::Transport
                };
                FetchError::new(kind, format!("failed to connect to `{host}`: {error}"))
            })?;
            let (mut sender, connection) = hyper::client::conn::http1::handshake(stream)
                .await
                .map_err(|error| {
                    FetchError::new(FetchErrorKind::Transport, format!("HTTP handshake failed: {error}"))
                })?;
            tokio::spawn(async move {
                let _ = connection.await;
            });
            let response = sender.send_request(request).await.map_err(|error| {
                FetchError::new(FetchErrorKind::Transport, format!("request failed: {error}"))
            })?;
            let status = response.status();
            if status.is_redirection() {
                let location = response
                    .headers()
                    .get(LOCATION)
                    .and_then(|value| value.to_str().ok())
                    .map(str::to_owned)
                    .ok_or_else(|| {
                        FetchError::new(
                            FetchErrorKind::Transport,
                            format!("redirect {status} without a Location header"),
                        )
                    })?;
                return Ok(HopOutcome::Redirect { location });
            }
            if !status.is_success() {
                return Err(FetchError::with_details(
                    FetchErrorKind::HttpStatus,
                    format!("server returned {status} for `{host}`"),
                    serde_json::json!({ "status": status.as_u16(), "url": url.as_str() }),
                ));
            }
            let headers = response.headers().clone();
            if let Some(length) = content_length(&headers)
                && length > max_bytes as u64
            {
                return Err(FetchError::with_details(
                    FetchErrorKind::ContentLengthOverBudget,
                    format!(
                        "server declared {length} bytes, exceeding the {max_bytes} byte budget"
                    ),
                    serde_json::json!({ "declaredBytes": length, "limitBytes": max_bytes }),
                ));
            }
            if let Some(encoding) = response.headers().get(CONTENT_ENCODING)
                && !encoding.as_bytes().eq_ignore_ascii_case(b"identity")
            {
                return Err(FetchError::new(
                    FetchErrorKind::UnsupportedContentEncoding,
                    format!(
                        "server negotiated unsupported content encoding `{}`",
                        encoding.to_str().unwrap_or("<binary>")
                    ),
                ));
            }
            let media = media_kind(headers.get(CONTENT_TYPE).and_then(|value| value.to_str().ok()));
            if let MediaKind::Other(mime) = &media {
                return Err(FetchError::with_details(
                    FetchErrorKind::Transport,
                    format!(
                        "unsupported content type `{mime}`; only text/html and text/plain are accepted"
                    ),
                    serde_json::json!({ "contentType": mime }),
                ));
            }
            let mut body = response.into_body();
            let mut collected = Vec::new();
            let mut truncated = false;
            while let Some(frame) = http_body_util::BodyExt::frame(&mut body).await {
                let frame = frame.map_err(|error| {
                    FetchError::new(FetchErrorKind::Transport, format!("body read failed: {error}"))
                })?;
                let chunk = frame.into_data().map_err(|_| {
                    FetchError::new(FetchErrorKind::Transport, "unexpected body trailer")
                })?;
                if collected.len() + chunk.len() > max_bytes {
                    truncated = true;
                    break;
                }
                collected.extend_from_slice(&chunk);
            }
            Ok(HopOutcome::Response {
                headers,
                bytes: collected,
                truncated,
            })
        })
        .await
        .map_err(|_| {
            FetchError::new(
                FetchErrorKind::RequestTimeout,
                format!("fetching `{url}` exceeded its time budget"),
            )
        })?
    }

    async fn resolve_host(
        &self,
        host: &str,
        port: u16,
    ) -> Result<Vec<std::net::SocketAddr>, FetchError> {
        // `Url::host_str()` keeps the brackets of an IPv6 literal, so strip
        // them before parsing; otherwise the literal falls through to the DNS
        // resolver and skips the SSRF validation this function owns.
        if let Ok(ip) = host
            .trim_start_matches('[')
            .trim_end_matches(']')
            .parse::<std::net::IpAddr>()
        {
            self.validate_test_or_prod(ip)?;
            return Ok(vec![std::net::SocketAddr::new(ip, port)]);
        }
        let addresses = self
            .resolver
            .lookup(host, port)
            .await
            .map_err(|error| FetchError::new(FetchErrorKind::DnsResolution, error))?;
        if addresses.is_empty() {
            return Err(FetchError::new(
                FetchErrorKind::DnsResolution,
                format!("no addresses found for `{host}`"),
            ));
        }
        let mut validated = Vec::with_capacity(addresses.len());
        for address in addresses {
            if !self.allow_loopback() {
                validate_ip(address).map_err(|reason| self.blocked(address, reason))?;
            }
            validated.push(std::net::SocketAddr::new(address, port));
        }
        Ok(validated)
    }

    fn blocked(&self, address: std::net::IpAddr, reason: BlockReason) -> FetchError {
        FetchError::with_details(
            FetchErrorKind::SsrfBlocked,
            format!(
                "target {address} is blocked by the SSRF policy: {}",
                reason.describe()
            ),
            serde_json::json!({ "address": address.to_string(), "reason": reason.describe() }),
        )
    }

    fn validate_test_or_prod(&self, ip: std::net::IpAddr) -> Result<(), FetchError> {
        if self.allow_loopback() {
            return Ok(());
        }
        validate_ip(ip).map_err(|reason| self.blocked(ip, reason))
    }

    #[cfg(feature = "test-support")]
    fn allow_loopback(&self) -> bool {
        self.config.allow_loopback
    }

    #[cfg(not(feature = "test-support"))]
    fn allow_loopback(&self) -> bool {
        false
    }

    async fn finalize(
        &self,
        url: Url,
        headers: http::HeaderMap,
        bytes: Vec<u8>,
        truncated: bool,
        format: OutputFormat,
    ) -> Result<FetchResult, FetchError> {
        let content_type = headers
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or("")
            .to_string();
        let media = media_kind(Some(&content_type));
        let decoded = decode_body(&bytes, Some(&content_type));
        let converted = tokio::time::timeout(
            self.config.conversion_timeout,
            tokio::task::spawn_blocking(move || convert_body(&decoded, &media, format)),
        )
        .await
        .map_err(|_| {
            FetchError::new(
                FetchErrorKind::ConversionTimeout,
                "HTML conversion exceeded its time budget",
            )
        })?
        .map_err(|error| {
            FetchError::new(
                FetchErrorKind::Transport,
                format!("conversion task failed: {error}"),
            )
        })??;
        Ok(FetchResult {
            text: converted,
            final_url: normalize_url(url).to_string(),
            content_type,
            truncated,
            from_cache: false,
            fetched_at: SystemTime::now(),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchRequest {
    pub url: String,
    pub format: OutputFormat,
    /// Byte budget for the response body. Defaults to the client budget.
    pub max_bytes: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchResult {
    pub text: String,
    pub final_url: String,
    pub content_type: String,
    /// Set when the body or the projected output hit a budget.
    pub truncated: bool,
    pub from_cache: bool,
    pub fetched_at: SystemTime,
}

enum HopOutcome {
    Redirect {
        location: String,
    },
    Response {
        headers: http::HeaderMap,
        bytes: Vec<u8>,
        truncated: bool,
    },
}

fn parse_and_validate_url(url: &str) -> Result<Url, FetchError> {
    let url = Url::parse(url).map_err(|error| {
        FetchError::new(
            FetchErrorKind::InvalidUrl,
            format!("invalid url `{url}`: {error}"),
        )
    })?;
    match url.scheme() {
        "http" | "https" => {}
        scheme => {
            return Err(FetchError::new(
                FetchErrorKind::InvalidScheme,
                format!("unsupported url scheme `{scheme}`; only http and https are allowed"),
            ));
        }
    }
    if url.host_str().is_none() {
        return Err(FetchError::new(
            FetchErrorKind::InvalidUrl,
            format!("url `{url}` has no host"),
        ));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(FetchError::new(
            FetchErrorKind::UserInfoForbidden,
            "urls with embedded credentials are not allowed",
        ));
    }
    Ok(url)
}

/// Canonical form for cache keys: fragment dropped, scheme-default port
/// removed. A non-default explicit port must be kept so that `:8080` and the
/// bare host never share a cache entry.
fn normalize_url(mut url: Url) -> Url {
    url.set_fragment(None);
    let default_port = match url.scheme() {
        "http" => Some(80),
        "https" => Some(443),
        _ => None,
    };
    if url.port().is_some() && url.port() == default_port {
        let _ = url.set_port(None);
    }
    url
}

fn origin_form(url: &Url) -> String {
    match url.query() {
        Some(query) => format!("{}?{query}", url.path()),
        None => url.path().to_string(),
    }
}

fn host_header(url: &Url, host: &str) -> String {
    let host = match url.host() {
        Some(url::Host::Ipv6(ipv6)) => format!("[{ipv6}]"),
        _ => host.to_string(),
    };
    match url.port() {
        Some(port) => format!("{host}:{port}"),
        None => host,
    }
}

fn content_length(headers: &http::HeaderMap) -> Option<u64> {
    headers
        .get(CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse().ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_validation_rejects_bad_inputs() {
        let error = parse_and_validate_url("ftp://example.com/file").unwrap_err();
        assert_eq!(error.kind, FetchErrorKind::InvalidScheme);
        let error = parse_and_validate_url("http://user:pass@example.com/").unwrap_err();
        assert_eq!(error.kind, FetchErrorKind::UserInfoForbidden);
        let error = parse_and_validate_url("not a url").unwrap_err();
        assert_eq!(error.kind, FetchErrorKind::InvalidUrl);
        let error = parse_and_validate_url("http:///").unwrap_err();
        assert_eq!(error.kind, FetchErrorKind::InvalidUrl);
        assert!(parse_and_validate_url("https://example.com:8443/a?b=c#frag").is_ok());
    }

    #[test]
    fn origin_form_and_host_header_handle_ports_and_ipv6() {
        let url = parse_and_validate_url("http://example.com:8080/a/b?q=1").unwrap();
        assert_eq!(origin_form(&url), "/a/b?q=1");
        assert_eq!(host_header(&url, "example.com"), "example.com:8080");

        let url = parse_and_validate_url("http://example.com/plain").unwrap();
        assert_eq!(host_header(&url, "example.com"), "example.com");

        let url = parse_and_validate_url("http://[::1]:8080/x").unwrap();
        assert_eq!(host_header(&url, "::1"), "[::1]:8080");
    }

    #[test]
    fn normalization_drops_fragments_and_default_ports() {
        assert_eq!(
            normalize_url(parse_and_validate_url("http://Example.com:80/a#frag").unwrap()).as_str(),
            "http://example.com/a"
        );
        assert_eq!(
            normalize_url(parse_and_validate_url("https://example.com:443/a#frag").unwrap())
                .as_str(),
            "https://example.com/a"
        );
        assert_eq!(
            normalize_url(parse_and_validate_url("http://example.com:8080/a#frag").unwrap())
                .as_str(),
            "http://example.com:8080/a"
        );
    }
}
