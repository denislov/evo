use base64::Engine;
use ring::digest;

pub fn pkce_challenge(verifier: &str) -> String {
    let hash = digest::digest(&digest::SHA256, verifier.as_bytes());
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(hash.as_ref())
}

pub fn success_html(message: &str) -> String {
    render_page(
        "Authentication successful",
        "Authentication successful",
        message,
        None,
    )
}

pub fn error_html(message: &str, details: Option<&str>) -> String {
    render_page(
        "Authentication failed",
        "Authentication failed",
        message,
        details,
    )
}

fn render_page(title: &str, heading: &str, message: &str, details: Option<&str>) -> String {
    let details = details
        .map(|d| format!(r#"<div class="details">{}</div>"#, escape_html(d)))
        .unwrap_or_default();
    format!(
        r#"<!doctype html>
<html lang="en">
<head><meta charset="utf-8" /><title>{}</title></head>
<body><main><h1>{}</h1><p>{}</p>{}</main></body>
</html>"#,
        escape_html(title),
        escape_html(heading),
        escape_html(message),
        details
    )
}

fn escape_html(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}
