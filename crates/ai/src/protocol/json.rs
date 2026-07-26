use serde::de::{self, Deserialize, Deserializer, MapAccess, SeqAccess, Visitor};

/// Repairs common JSON formatting issues: escapes raw control characters
/// (0x00-0x1F, excluding \t \n \r) and fixes invalid escape sequences.
pub fn repair_json(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                out.push('\\');
                if let Some(&next) = chars.peek() {
                    if next == '\\'
                        || next == '"'
                        || next == '/'
                        || next == 'b'
                        || next == 'f'
                        || next == 'n'
                        || next == 'r'
                        || next == 't'
                        || next == 'u'
                    {
                        // valid escape, keep it
                    } else {
                        // invalid escape, double-escape
                        out.push('\\');
                    }
                }
            }
            c if (c as u32) < 0x20 && c != '\t' && c != '\n' && c != '\r' => {
                // raw control char, escape it
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            _ => out.push(c),
        }
    }
    out
}

/// Attempts to parse streaming (possibly incomplete) JSON.
///
/// This parser is intentionally permissive for partial deltas: it tries strict
/// JSON, repaired JSON, and a best-effort completion of unclosed constructs.
/// Its output is display-only and must never be treated as executable tool
/// input. Callers finalizing tool arguments must use [`parse_terminal_json`].
pub fn parse_streaming_json(input: &str) -> serde_json::Value {
    try_parse_streaming_preview(input)
        .unwrap_or_else(|_| serde_json::Value::Object(serde_json::Map::new()))
}

/// Strictly parses the exact accumulated bytes of a terminal JSON value.
///
/// Unlike [`parse_streaming_json`], this never repairs escapes, auto-closes
/// incomplete values, discards trailing data, or substitutes `{}`.
pub fn parse_terminal_json(input: &str) -> Result<serde_json::Value, String> {
    let mut deserializer = serde_json::Deserializer::from_str(input);
    let value = StrictJsonValue::deserialize(&mut deserializer)
        .map_err(|_| "malformed terminal JSON".to_string())?;
    deserializer
        .end()
        .map_err(|_| "malformed terminal JSON".to_string())?;
    Ok(value.0)
}

struct StrictJsonValue(serde_json::Value);

impl<'de> Deserialize<'de> for StrictJsonValue {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(StrictJsonVisitor)
    }
}

struct StrictJsonVisitor;

impl<'de> Visitor<'de> for StrictJsonVisitor {
    type Value = StrictJsonValue;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("one complete JSON value without duplicate object keys")
    }

    fn visit_bool<E>(self, value: bool) -> Result<Self::Value, E> {
        Ok(StrictJsonValue(serde_json::Value::Bool(value)))
    }

    fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E> {
        Ok(StrictJsonValue(serde_json::Value::Number(value.into())))
    }

    fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E> {
        Ok(StrictJsonValue(serde_json::Value::Number(value.into())))
    }

    fn visit_f64<E>(self, value: f64) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        serde_json::Number::from_f64(value)
            .map(serde_json::Value::Number)
            .map(StrictJsonValue)
            .ok_or_else(|| E::custom("non-finite JSON number"))
    }

    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E> {
        Ok(StrictJsonValue(serde_json::Value::String(value.into())))
    }

    fn visit_string<E>(self, value: String) -> Result<Self::Value, E> {
        Ok(StrictJsonValue(serde_json::Value::String(value)))
    }

    fn visit_none<E>(self) -> Result<Self::Value, E> {
        Ok(StrictJsonValue(serde_json::Value::Null))
    }

    fn visit_unit<E>(self) -> Result<Self::Value, E> {
        Ok(StrictJsonValue(serde_json::Value::Null))
    }

    fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let mut values = Vec::new();
        while let Some(value) = sequence.next_element::<StrictJsonValue>()? {
            values.push(value.0);
        }
        Ok(StrictJsonValue(serde_json::Value::Array(values)))
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut values = serde_json::Map::new();
        while let Some(key) = map.next_key::<String>()? {
            if values.contains_key(&key) {
                return Err(de::Error::custom("duplicate JSON object key"));
            }
            let value = map.next_value::<StrictJsonValue>()?;
            values.insert(key, value.0);
        }
        Ok(StrictJsonValue(serde_json::Value::Object(values)))
    }
}

fn try_parse_streaming_preview(input: &str) -> Result<serde_json::Value, String> {
    if let Ok(v) = serde_json::from_str(input) {
        return Ok(v);
    }
    let repaired = repair_json(input);
    if let Ok(v) = serde_json::from_str(&repaired) {
        return Ok(v);
    }
    if let Ok(v) = serde_json::from_str(&close_incomplete(&repaired)) {
        return Ok(v);
    }
    Err("malformed streaming JSON".to_string())
}

/// Appends closing characters to make incomplete JSON parseable.
fn close_incomplete(s: &str) -> String {
    let mut out = s.to_string();
    let mut stack: Vec<char> = Vec::new();
    let mut in_string = false;
    let mut escaped = false;

    for c in s.chars() {
        if escaped {
            escaped = false;
            continue;
        }
        if c == '\\' && in_string {
            escaped = true;
            continue;
        }
        match c {
            '"' => in_string = !in_string,
            '{' if !in_string => stack.push('}'),
            '[' if !in_string => stack.push(']'),
            '}' | ']' if !in_string => {
                stack.pop();
            }
            _ => {}
        }
    }
    if in_string {
        out.push('"');
    }
    while let Some(bracket) = stack.pop() {
        out.push(bracket);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repair_escapes_control_chars() {
        let input = "hello\x01world";
        let repaired = repair_json(input);
        assert!(!repaired.contains('\x01'));
        assert!(repaired.contains("\\u0001"));
    }

    #[test]
    fn repair_fixes_bad_backslash() {
        let input = r#"{"key": "val\x"}"#;
        let repaired = repair_json(input);
        assert!(repaired.contains(r#"\\x"#));
    }

    #[test]
    fn parse_valid_json() {
        let v = parse_streaming_json(r#"{"a": 1}"#);
        assert_eq!(v["a"], 1);
    }

    #[test]
    fn parse_truncated_object() {
        let v = parse_streaming_json(r#"{"a": 1, "b": {"#);
        assert!(v.is_object());
    }

    #[test]
    fn parse_truncated_array() {
        let v = parse_streaming_json(r#"[1, 2, {"#);
        assert!(v.is_array());
    }

    #[test]
    fn parse_garbage_returns_empty_object() {
        let v = parse_streaming_json("not json at all!!!");
        assert!(v.is_object());
        assert!(v.as_object().unwrap().is_empty());
    }

    #[test]
    fn terminal_parser_accepts_only_exact_complete_json() {
        assert_eq!(
            parse_terminal_json(r#"{"path":"Cargo.toml"}"#).unwrap(),
            serde_json::json!({"path": "Cargo.toml"})
        );
        for malformed in [
            r#"{"path":"Cargo.toml""#,
            r#"{"path":"bad\x"}"#,
            r#"{"path":"Cargo.toml"} trailing"#,
            r#"{"path":"one","path":"two"}"#,
            r#"{"nested":{"value":1,"value":2}}"#,
            r#"[1,2,"#,
            "",
        ] {
            assert!(
                parse_terminal_json(malformed).is_err(),
                "terminal parser accepted {malformed:?}"
            );
        }
    }
}
