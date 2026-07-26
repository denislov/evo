use std::collections::BTreeMap;

pub fn merge_headers(
    model_headers: Option<&serde_json::Value>,
    option_headers: Option<&serde_json::Value>,
    generated: impl IntoIterator<Item = (String, String)>,
) -> BTreeMap<String, String> {
    let mut headers = BTreeMap::new();

    for (key, value) in generated {
        headers.insert(key, value);
    }

    append_json_headers(&mut headers, model_headers);
    append_json_headers(&mut headers, option_headers);

    headers
}

fn append_json_headers(headers: &mut BTreeMap<String, String>, value: Option<&serde_json::Value>) {
    let Some(obj) = value.and_then(|v| v.as_object()) else {
        return;
    };
    for (key, value) in obj {
        if let Some(value) = value.as_str() {
            headers.insert(key.clone(), value.to_string());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_headers_appear_first() {
        let headers = merge_headers(
            None,
            None,
            [("content-type".into(), "application/json".into())],
        );
        assert_eq!(
            headers.get("content-type").map(String::as_str),
            Some("application/json")
        );
    }

    #[test]
    fn option_headers_override_model_headers() {
        let model = serde_json::json!({"x-custom": "model-value"});
        let opts = serde_json::json!({"x-custom": "option-value"});
        let headers = merge_headers(Some(&model), Some(&opts), []);
        assert_eq!(
            headers.get("x-custom").map(String::as_str),
            Some("option-value")
        );
    }

    #[test]
    fn generated_headers_preserved_when_not_overridden() {
        let opts = serde_json::json!({"x-extra": "extra-value"});
        let headers = merge_headers(
            None,
            Some(&opts),
            [("authorization".into(), "Bearer sk-test".into())],
        );
        assert_eq!(
            headers.get("authorization").map(String::as_str),
            Some("Bearer sk-test")
        );
        assert_eq!(
            headers.get("x-extra").map(String::as_str),
            Some("extra-value")
        );
    }

    #[test]
    fn option_headers_can_override_generated() {
        let opts = serde_json::json!({"content-type": "text/plain"});
        let headers = merge_headers(
            None,
            Some(&opts),
            [("content-type".into(), "application/json".into())],
        );
        assert_eq!(
            headers.get("content-type").map(String::as_str),
            Some("text/plain")
        );
    }

    #[test]
    fn empty_inputs_produce_empty_map() {
        let headers = merge_headers(None, None, []);
        assert!(headers.is_empty());
    }
}
