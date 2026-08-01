use regex::Regex;
use std::sync::OnceLock;

pub(crate) fn redact_sensitive_text(text: &str) -> String {
    static ASSIGNMENT: OnceLock<Regex> = OnceLock::new();
    static JSON_FIELD: OnceLock<Regex> = OnceLock::new();
    static BEARER: OnceLock<Regex> = OnceLock::new();
    static ABSOLUTE_PATH: OnceLock<Regex> = OnceLock::new();
    // The key may be prefixed by an environment-style section (`OPENAI_`,
    // `GITHUB_`, `AWS_`, `DB_`...) or a separator. A plain `\b` anchor fails
    // for `OPENAI_API_KEY=` because `_` is a word character, leaving the most
    // common credential formats unredacted.
    let assignment = ASSIGNMENT.get_or_init(|| {
        Regex::new(
            r"(?i)((?:^|[\s;&|(?]|_)(?:[a-z0-9]+_)*(?:api[_-]?key|access[_-]?key|auth[_-]?token|token|password|passwd|secret)\s*=\s*)[^\s;&|]+",
        )
        .expect("redaction regex is valid")
    });
    let json_field = JSON_FIELD.get_or_init(|| {
        Regex::new(
            r#"(?i)([\"']?(?:[a-z0-9]+_)*(?:api[_-]?key|access[_-]?key|auth[_-]?token|token|password|passwd|secret)[\"']?\s*:\s*)[\"'][^\"']+[\"']"#,
        )
        .expect("redaction regex is valid")
    });
    let bearer = BEARER
        .get_or_init(|| Regex::new(r"(?i)\bbearer\s+[^\s;&|]+").expect("redaction regex is valid"));
    let absolute_path = ABSOLUTE_PATH.get_or_init(|| {
        Regex::new(r#"(?:\b[A-Za-z]:\\[^\s"'`;|]+|(?:^|[\s(])(?:/|~/)[^\s"'`;|]+)"#)
            .expect("absolute path redaction regex is valid")
    });
    let redacted = assignment.replace_all(text, "$1<redacted>");
    let redacted = json_field.replace_all(&redacted, "$1\"<redacted>\"");
    let redacted = bearer.replace_all(&redacted, "Bearer <redacted>");
    absolute_path
        .replace_all(&redacted, |captures: &regex::Captures<'_>| {
            let matched = captures.get(0).expect("whole regex match exists").as_str();
            if matched.starts_with(char::is_whitespace) || matched.starts_with('(') {
                format!("{}<path>", &matched[..1])
            } else {
                "<path>".to_owned()
            }
        })
        .into_owned()
}

pub(crate) fn redact_and_bound(text: &str, max_bytes: usize) -> String {
    let redacted = redact_sensitive_text(text);
    if redacted.len() <= max_bytes {
        return redacted;
    }
    let mut end = max_bytes;
    while !redacted.is_char_boundary(end) {
        end -= 1;
    }
    redacted[..end].to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_key_assignments_are_redacted() {
        assert_eq!(
            redact_sensitive_text("api_key=sk-abcdef123"),
            "api_key=<redacted>"
        );
        assert_eq!(redact_sensitive_text("token=abc"), "token=<redacted>");
        assert_eq!(redact_sensitive_text("password=hunter2"), "password=<redacted>");
    }

    #[test]
    fn env_style_prefixed_keys_are_redacted() {
        assert_eq!(
            redact_sensitive_text("OPENAI_API_KEY=sk-abcdef123"),
            "OPENAI_API_KEY=<redacted>"
        );
        assert_eq!(
            redact_sensitive_text("GITHUB_TOKEN=ghp_abc123"),
            "GITHUB_TOKEN=<redacted>"
        );
        assert_eq!(
            redact_sensitive_text("AWS_SECRET_ACCESS_KEY=AKIA12345"),
            "AWS_SECRET_ACCESS_KEY=<redacted>"
        );
        assert_eq!(
            redact_sensitive_text("export DB_PASSWORD=hunter2"),
            "export DB_PASSWORD=<redacted>"
        );
    }

    #[test]
    fn json_fields_with_prefixed_keys_are_redacted() {
        assert_eq!(
            redact_sensitive_text(r#"{"OPENAI_API_KEY": "sk-abc"}"#),
            r#"{"OPENAI_API_KEY": "<redacted>"}"#
        );
        assert_eq!(
            redact_sensitive_text(r#"{"api_key":"sk-abc","x":1}"#),
            r#"{"api_key":"<redacted>","x":1}"#
        );
    }

    #[test]
    fn url_query_tokens_are_redacted() {
        assert_eq!(
            redact_sensitive_text("?token=abc&x=1"),
            "?token=<redacted>&x=1"
        );
        assert_eq!(
            redact_sensitive_text("a=1;token=def"),
            "a=1;token=<redacted>"
        );
    }

    #[test]
    fn bearer_tokens_are_redacted() {
        assert_eq!(
            redact_sensitive_text("Authorization: Bearer eyJhbGciOi"),
            "Authorization: Bearer <redacted>"
        );
    }

    #[test]
    fn plain_words_are_not_redacted() {
        assert_eq!(
            redact_sensitive_text("the token economy is booming"),
            "the token economy is booming"
        );
        assert_eq!(redact_sensitive_text("token"), "token");
    }

    #[test]
    fn redact_and_bound_respects_char_boundaries() {
        let redacted = redact_and_bound("中文" .repeat(20).as_str().repeat(1).as_str(), 3);
        assert!(redacted.is_char_boundary(redacted.len()));
        assert!(redacted.len() <= 3);
    }
}
