use std::time::Duration;

/// Product-configurable connection policy shared by every built-in provider.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TransportConfig {
    pub http_proxy: Option<String>,
    pub connect_timeout_ms: Option<u64>,
    /// Extra root certificates as PEM bytes, trusted in addition to the
    /// system roots. Built with [`TransportConfig::with_extra_ca`]; invalid
    /// PEM fails client construction with a structured error.
    pub extra_ca_certificates: Option<Vec<Vec<u8>>>,
}

impl TransportConfig {
    pub fn new(http_proxy: Option<String>, connect_timeout_ms: Option<u64>) -> Self {
        Self {
            http_proxy: http_proxy.and_then(|proxy| {
                let proxy = proxy.trim();
                (!proxy.is_empty()).then(|| proxy.to_owned())
            }),
            connect_timeout_ms,
            extra_ca_certificates: None,
        }
    }

    /// Add extra PEM root certificates. Empty bundles are normalized away.
    pub fn with_extra_ca(mut self, certificates: Vec<Vec<u8>>) -> Self {
        let certificates = certificates
            .into_iter()
            .filter(|pem| !pem.is_empty())
            .collect::<Vec<_>>();
        self.extra_ca_certificates = (!certificates.is_empty()).then_some(certificates);
        self
    }
}

/// HTTP client for requests that may carry provider credentials.
///
/// Redirects are disabled because provider-specific secret headers and query
/// parameters are not uniformly recognized by HTTP client redirect policies.
/// A redirect is surfaced as its original non-success response instead.
pub(crate) fn authenticated_client(config: &TransportConfig) -> Result<reqwest::Client, String> {
    let mut builder = reqwest::Client::builder().redirect(reqwest::redirect::Policy::none());
    if let Some(proxy) = config.http_proxy.as_deref() {
        let proxy = reqwest::Proxy::all(proxy)
            .map_err(|error| format!("invalid HTTP proxy `{proxy}`: {error}"))?;
        builder = builder.proxy(proxy);
    }
    if let Some(timeout_ms) = config.connect_timeout_ms {
        if timeout_ms == 0 {
            return Err("transport connect timeout must be at least 1ms".into());
        }
        builder = builder.connect_timeout(Duration::from_millis(timeout_ms));
    }
    if let Some(certificates) = &config.extra_ca_certificates {
        for pem in certificates {
            let certificate = reqwest::Certificate::from_pem(pem)
                .map_err(|error| format!("invalid extra CA certificate PEM: {error}"))?;
            builder = builder.add_root_certificate(certificate);
        }
    }
    builder
        .build()
        .map_err(|error| format!("failed to build provider HTTP client: {error}"))
}
