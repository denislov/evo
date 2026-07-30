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
