/// HTTP client for requests that may carry provider credentials.
///
/// Redirects are disabled because provider-specific secret headers and query
/// parameters are not uniformly recognized by HTTP client redirect policies.
/// A redirect is surfaced as its original non-success response instead.
pub(crate) fn authenticated_client() -> reqwest::Client {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .expect("the default rustls HTTP client configuration should build")
}
