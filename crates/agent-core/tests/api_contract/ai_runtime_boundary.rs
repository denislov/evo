use std::fs;
use std::path::Path;

#[test]
fn production_and_examples_do_not_use_global_provider_runtime() {
    let crate_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut violations = Vec::new();
    collect_global_provider_calls(&crate_root.join("src"), &mut violations);
    collect_global_provider_calls(&crate_root.join("examples"), &mut violations);

    assert!(
        violations.is_empty(),
        "agent-core must receive a scoped ProviderStreamer instead of using global ai runtime:\n{}",
        violations.join("\n")
    );
}

#[test]
fn ai_dependency_uses_only_edge_allowlisted_api_categories() {
    let crate_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut violations = Vec::new();

    collect_api_category_violations(
        &crate_root.join("src"),
        &["conversation", "hooks", "model", "stream"],
        &mut violations,
    );
    collect_api_category_violations(
        &crate_root.join("examples"),
        &[
            "client",
            "conversation",
            "hooks",
            "model",
            "stream",
            "testing",
        ],
        &mut violations,
    );

    assert!(
        violations.is_empty(),
        "agent-core may depend only on the categorized ai edge contract:\n{}",
        violations.join("\n")
    );
}

#[test]
fn ai_dependency_uses_only_edge_allowlisted_items() {
    let crate_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut violations = Vec::new();
    let allowlist = [
        (
            "conversation",
            &[
                "AssistantMessage",
                "ContentBlock",
                "Context",
                "Cost",
                "Message",
                "StopReason",
                "Tool",
                "Usage",
            ][..],
        ),
        ("hooks", &["ProviderResponseInfo", "ProviderStreamHooks"]),
        (
            "model",
            &["Model", "ModelCost", "ModelInput", "ThinkingConfig"],
        ),
        (
            "stream",
            &[
                "AssistantMessageEvent",
                "EventStream",
                "StreamOptions",
                "complete",
                "parse_terminal_json",
                "parse_streaming_json",
            ],
        ),
    ];

    collect_api_item_violations(&crate_root.join("src"), &allowlist, &mut violations);

    assert!(
        violations.is_empty(),
        "agent-core named ai items outside its exact edge allowlist:\n{}",
        violations.join("\n")
    );
}

#[test]
fn missing_provider_streamer_fails_explicitly() {
    let source = include_str!("../../src/agent/provider.rs");
    assert!(source.contains("provider streamer is required"));
    assert!(source.contains("missing_provider_streamer(model)"));
    assert!(!source.contains("ai::stream_model("));
    assert!(!source.contains("ai::registry::"));
}

fn collect_global_provider_calls(path: &Path, violations: &mut Vec<String>) {
    let metadata = fs::metadata(path).expect("provider boundary path should exist");
    if metadata.is_dir() {
        let mut entries = fs::read_dir(path)
            .expect("read provider boundary directory")
            .collect::<Result<Vec<_>, _>>()
            .expect("read provider boundary entries");
        entries.sort_by_key(|entry| entry.path());
        for entry in entries {
            collect_global_provider_calls(&entry.path(), violations);
        }
        return;
    }
    if path.extension().and_then(|extension| extension.to_str()) != Some("rs") {
        return;
    }

    let content = fs::read_to_string(path).expect("read provider boundary source");
    for (index, line) in content.lines().enumerate() {
        if line.contains("ai::stream_model(")
            || line.contains("ai::registry::stream_model(")
            || line.contains("ai::registry::register(")
            || line.contains("ai::registry::unregister(")
        {
            violations.push(format!("{}:{}: {}", path.display(), index + 1, line.trim()));
        }
    }
}

fn collect_api_category_violations(
    path: &Path,
    allowed_categories: &[&str],
    violations: &mut Vec<String>,
) {
    let metadata = fs::metadata(path).expect("ai category boundary path should exist");
    if metadata.is_dir() {
        let mut entries = fs::read_dir(path)
            .expect("read ai category boundary directory")
            .collect::<Result<Vec<_>, _>>()
            .expect("read ai category boundary entries");
        entries.sort_by_key(|entry| entry.path());
        for entry in entries {
            collect_api_category_violations(&entry.path(), allowed_categories, violations);
        }
        return;
    }
    if path.extension().and_then(|extension| extension.to_str()) != Some("rs") {
        return;
    }

    let content = fs::read_to_string(path).expect("read ai category boundary source");
    for (line_index, line) in content.lines().enumerate() {
        let mut remaining = line;
        while let Some(offset) = remaining.find("ai::api::") {
            let suffix = &remaining[offset + "ai::api::".len()..];
            let category: String = suffix
                .chars()
                .take_while(|character| character.is_ascii_alphanumeric() || *character == '_')
                .collect();
            if !allowed_categories.contains(&category.as_str()) {
                violations.push(format!(
                    "{}:{}: category `{}` is not allowed: {}",
                    path.display(),
                    line_index + 1,
                    if category.is_empty() {
                        "<flat-or-grouped>"
                    } else {
                        &category
                    },
                    line.trim()
                ));
            }
            remaining = suffix.get(category.len()..).unwrap_or_default();
        }
    }
}

fn collect_api_item_violations(
    path: &Path,
    allowlist: &[(&str, &[&str])],
    violations: &mut Vec<String>,
) {
    let metadata = fs::metadata(path).expect("ai item boundary path should exist");
    if metadata.is_dir() {
        let mut entries = fs::read_dir(path)
            .expect("read ai item boundary directory")
            .collect::<Result<Vec<_>, _>>()
            .expect("read ai item boundary entries");
        entries.sort_by_key(|entry| entry.path());
        for entry in entries {
            collect_api_item_violations(&entry.path(), allowlist, violations);
        }
        return;
    }
    if path.extension().and_then(|extension| extension.to_str()) != Some("rs") {
        return;
    }

    let content = fs::read_to_string(path).expect("read ai item boundary source");
    let mut remaining = content.as_str();
    while let Some(offset) = remaining.find("use ai::api::") {
        let statement = &remaining[offset..];
        let Some(end) = statement.find(';') else {
            violations.push(format!("{}: unterminated ai use", path.display()));
            break;
        };
        let statement = &statement[..end];
        let suffix = &statement["use ai::api::".len()..];
        let category_end = suffix.find("::").unwrap_or(suffix.len());
        let category = &suffix[..category_end];
        let imported = suffix.get(category_end + 2..).unwrap_or_default();
        let allowed = allowlist
            .iter()
            .find_map(|(candidate, items)| (*candidate == category).then_some(*items));

        for item in imported
            .split(|character: char| !character.is_ascii_alphanumeric() && character != '_')
            .filter(|item| !item.is_empty() && *item != "json")
        {
            if allowed.is_none_or(|items| !items.contains(&item)) {
                violations.push(format!(
                    "{}: `{category}::{item}` is not allowlisted in `{}`",
                    path.display(),
                    statement.split_whitespace().collect::<Vec<_>>().join(" ")
                ));
            }
        }
        remaining = &remaining[offset + end + 1..];
    }
}
