use regex::Regex;
use std::sync::OnceLock;

pub(crate) fn redact_sensitive_text(text: &str) -> String {
    static ASSIGNMENT: OnceLock<Regex> = OnceLock::new();
    static JSON_FIELD: OnceLock<Regex> = OnceLock::new();
    static BEARER: OnceLock<Regex> = OnceLock::new();
    static ABSOLUTE_PATH: OnceLock<Regex> = OnceLock::new();
    let assignment = ASSIGNMENT.get_or_init(|| {
        Regex::new(r"(?i)\b(api[_-]?key|token|password|passwd|secret)\s*=\s*([^\s;&|]+)")
            .expect("redaction regex is valid")
    });
    let json_field = JSON_FIELD.get_or_init(|| {
        Regex::new(
            r#"(?i)([\"']?(?:api[_-]?key|token|password|passwd|secret)[\"']?\s*:\s*)[\"'][^\"']+[\"']"#,
        )
        .expect("redaction regex is valid")
    });
    let bearer = BEARER
        .get_or_init(|| Regex::new(r"(?i)\bbearer\s+[^\s;&|]+").expect("redaction regex is valid"));
    let absolute_path = ABSOLUTE_PATH.get_or_init(|| {
        Regex::new(r#"(?:\b[A-Za-z]:\\[^\s"'`;|]+|(?:^|[\s(])(?:/|~/)[^\s"'`;|]+)"#)
            .expect("absolute path redaction regex is valid")
    });
    let redacted = assignment.replace_all(text, "$1=<redacted>");
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
