use super::errors::{FetchError, FetchErrorKind};

/// Requested projection of a fetched page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputFormat {
    /// HTML is converted to Markdown; plain text passes through.
    Markdown,
    /// Raw text projection: HTML is dropped from the page.
    Text,
}

impl OutputFormat {
    pub fn name(self) -> &'static str {
        match self {
            Self::Markdown => "markdown",
            Self::Text => "text",
        }
    }
}

/// Classification of a response body by its Content-Type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MediaKind {
    Html,
    PlainText,
    /// Anything else — PDF, images, video — is rejected unless a consumer
    /// explicitly opts in later.
    Other(String),
}

pub fn media_kind(content_type: Option<&str>) -> MediaKind {
    let mime = content_type
        .and_then(|value| value.split(';').next())
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    match mime.as_str() {
        "text/html" => MediaKind::Html,
        "text/plain" => MediaKind::PlainText,
        _ => MediaKind::Other(mime),
    }
}

/// Extract the charset label from a Content-Type header value.
fn charset_label(content_type: Option<&str>) -> Option<&str> {
    content_type.and_then(|value| {
        value.split(';').skip(1).find_map(|parameter| {
            let (key, value) = parameter.trim().split_once('=')?;
            key.trim()
                .eq_ignore_ascii_case("charset")
                .then(|| value.trim().trim_matches('"'))
        })
    })
}

/// Decode a body using the declared charset, falling back to UTF-8 lossy
/// decoding on unknown labels or invalid byte sequences.
pub fn decode_body(bytes: &[u8], content_type: Option<&str>) -> String {
    let Some(encoding) = charset_label(content_type)
        .and_then(|label| encoding_rs::Encoding::for_label(label.as_bytes()))
    else {
        return String::from_utf8_lossy(bytes).into_owned();
    };
    let (decoded, _used, _had_errors) = encoding.decode(bytes);
    decoded.into_owned()
}

/// Project a decoded body into the requested output format.
pub fn convert_body(
    body: &str,
    media: &MediaKind,
    format: OutputFormat,
) -> Result<String, FetchError> {
    match media {
        MediaKind::PlainText => Ok(body.to_string()),
        MediaKind::Html => match format {
            OutputFormat::Markdown => Ok(html2md::parse_html(body)),
            OutputFormat::Text => Ok(html_to_text(body)),
        },
        MediaKind::Other(mime) => Err(FetchError::with_details(
            FetchErrorKind::Transport,
            format!(
                "unsupported content type `{mime}`; only text/html and text/plain are accepted"
            ),
            serde_json::json!({ "contentType": mime }),
        )),
    }
}

/// Strip tags and collapse whitespace to keep a text projection bounded and
/// predictable. This is a fallback for `format: text`; Markdown is the
/// richer default projection.
fn html_to_text(html: &str) -> String {
    let mut text = String::with_capacity(html.len() / 2);
    let mut in_tag = false;
    for character in html.chars() {
        match character {
            '<' => in_tag = true,
            '>' if in_tag => in_tag = false,
            _ if !in_tag => text.push(character),
            _ => {}
        }
    }
    collapse_whitespace(&text)
}

fn collapse_whitespace(text: &str) -> String {
    let mut collapsed = String::with_capacity(text.len());
    let mut whitespace_run = false;
    for character in text.chars() {
        if character.is_whitespace() {
            whitespace_run = true;
        } else {
            if whitespace_run && !collapsed.is_empty() {
                collapsed.push(' ');
            }
            whitespace_run = false;
            collapsed.push(character);
        }
    }
    collapsed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn media_kind_classifies_common_types() {
        assert_eq!(
            media_kind(Some("text/html; charset=utf-8")),
            MediaKind::Html
        );
        assert_eq!(media_kind(Some("TEXT/PLAIN")), MediaKind::PlainText);
        assert_eq!(
            media_kind(None),
            MediaKind::Other(String::new()),
            "a missing Content-Type is unknown and must fail closed"
        );
        assert_eq!(
            media_kind(Some("application/pdf")),
            MediaKind::Other("application/pdf".into())
        );
        assert_eq!(
            media_kind(Some("image/png")),
            MediaKind::Other("image/png".into())
        );
        assert_eq!(
            media_kind(Some("video/mp4")),
            MediaKind::Other("video/mp4".into())
        );
    }

    #[test]
    fn decode_respects_declared_charset_and_falls_back() {
        let utf8 = "héllo".as_bytes();
        assert_eq!(decode_body(utf8, Some("text/html; charset=utf-8")), "héllo");
        let latin1 = b"caf\xe9";
        assert_eq!(
            decode_body(latin1, Some("text/html; charset=iso-8859-1")),
            "caf\u{e9}"
        );
        assert_eq!(decode_body(utf8, None), "héllo");
        assert_eq!(
            decode_body(b"\xff\xfe invalid", Some("text/html; charset=bogus-label")),
            "\u{fffd}\u{fffd} invalid"
        );
    }

    #[test]
    fn markdown_conversion_covers_common_structure() {
        let html = "<h1>Title</h1>\n<p>Hello <a href=\"https://example.com/x\">link</a>.</p>\n\
                    <ul><li>one</li><li>two</li></ul>\n<pre><code>fn main() {}</code></pre>";
        let markdown = html2md::parse_html(html);
        assert!(
            markdown.contains("Title"),
            "unexpected markdown: {markdown}"
        );
        assert!(
            markdown.contains("[link](https://example.com/x)"),
            "unexpected markdown: {markdown}"
        );
        assert!(
            markdown.contains("- one") || markdown.contains("* one"),
            "unexpected markdown: {markdown}"
        );
        assert!(
            markdown.contains("fn main() {}"),
            "unexpected markdown: {markdown}"
        );
    }

    #[test]
    fn text_projection_strips_tags_and_collapses_whitespace() {
        let html = "<div>\n  <p>Hello   world</p>\n  <p>second</p>\n</div>";
        assert_eq!(
            convert_body(html, &MediaKind::Html, OutputFormat::Text).unwrap(),
            "Hello world second"
        );
    }

    #[test]
    fn non_html_media_is_rejected() {
        let error = convert_body(
            "%PDF-1.4",
            &MediaKind::Other("application/pdf".into()),
            OutputFormat::Markdown,
        )
        .unwrap_err();
        assert!(error.message.contains("application/pdf"));
        assert_eq!(error.details.unwrap()["contentType"], "application/pdf");
    }

    #[test]
    fn plain_text_passes_through_unmodified() {
        assert_eq!(
            convert_body("plain  text", &MediaKind::PlainText, OutputFormat::Markdown).unwrap(),
            "plain  text"
        );
    }
}
