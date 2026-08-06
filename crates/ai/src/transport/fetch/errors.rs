/// Failure classification for the safe fetch pipeline. The variant alone is
/// the stable contract; messages are human-readable and may change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FetchErrorKind {
    /// URL did not parse or had no usable host.
    InvalidUrl,
    /// Scheme is not http or https.
    InvalidScheme,
    /// URL carries userinfo credentials.
    UserInfoForbidden,
    /// A resolved address was rejected by the SSRF policy.
    SsrfBlocked,
    /// DNS resolution failed.
    DnsResolution,
    /// DNS resolution exceeded its budget.
    ResolveTimeout,
    /// TCP or TLS handshake exceeded its budget.
    ConnectTimeout,
    /// More redirects than the configured maximum.
    RedirectLimit,
    /// The server returned a non-success, non-redirect status.
    HttpStatus,
    /// The whole hop (request + body) exceeded its budget.
    RequestTimeout,
    /// The declared Content-Length exceeds the byte budget.
    ContentLengthOverBudget,
    /// Content-Encoding other than identity was negotiated.
    UnsupportedContentEncoding,
    /// Charset decoding of the body failed.
    ContentDecode,
    /// HTML conversion exceeded its budget.
    ConversionTimeout,
    /// Unexpected transport failure.
    Transport,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchError {
    pub kind: FetchErrorKind,
    pub message: String,
    pub details: Option<serde_json::Value>,
}

impl FetchError {
    pub fn new(kind: FetchErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            details: None,
        }
    }

    pub fn with_details(
        kind: FetchErrorKind,
        message: impl Into<String>,
        details: serde_json::Value,
    ) -> Self {
        Self {
            kind,
            message: message.into(),
            details: Some(details),
        }
    }
}

impl std::fmt::Display for FetchError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for FetchError {}

impl From<String> for FetchError {
    fn from(message: String) -> Self {
        Self::new(FetchErrorKind::Transport, message)
    }
}
