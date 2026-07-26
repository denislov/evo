use crate::agent::types::{DiagnosticSeverity, ResourceDiagnostic};
use serde_yaml::Value;

pub fn parse_frontmatter(content: &str) -> (Value, String, Vec<ResourceDiagnostic>) {
    let mut diagnostics = Vec::new();
    let normalized = content.replace("\r\n", "\n");

    if !normalized.starts_with("---\n") {
        return (
            Value::Mapping(serde_yaml::Mapping::new()),
            normalized,
            diagnostics,
        );
    }

    let rest = &normalized[4..];
    let end_marker = "\n---";
    let end_pos = match rest.find(end_marker) {
        Some(pos)
            if rest[..pos].ends_with('\n')
                || rest
                    .get(pos + end_marker.len()..)
                    .is_none_or(|s| s.starts_with('\n') || s.is_empty()) =>
        {
            pos
        }
        Some(_) => {
            diagnostics.push(ResourceDiagnostic {
                severity: DiagnosticSeverity::Warning,
                code: "frontmatter_no_closing".into(),
                message: "frontmatter does not have closing --- on its own line".into(),
                path: std::path::PathBuf::new(),
            });
            return (
                Value::Mapping(serde_yaml::Mapping::new()),
                normalized,
                diagnostics,
            );
        }
        None => {
            diagnostics.push(ResourceDiagnostic {
                severity: DiagnosticSeverity::Warning,
                code: "frontmatter_no_closing".into(),
                message: "no closing --- found for frontmatter".into(),
                path: std::path::PathBuf::new(),
            });
            return (
                Value::Mapping(serde_yaml::Mapping::new()),
                normalized,
                diagnostics,
            );
        }
    };

    let yaml_str = &rest[..end_pos];
    let metadata = match serde_yaml::from_str::<Value>(yaml_str) {
        Ok(v) => v,
        Err(e) => {
            diagnostics.push(ResourceDiagnostic {
                severity: DiagnosticSeverity::Warning,
                code: "frontmatter_parse_error".into(),
                message: format!("failed to parse frontmatter YAML: {}", e),
                path: std::path::PathBuf::new(),
            });
            Value::Mapping(serde_yaml::Mapping::new())
        }
    };

    let body_start = end_pos + end_marker.len();
    let body = if body_start < rest.len() {
        rest[body_start..].trim_start_matches('\n').to_string()
    } else {
        String::new()
    };

    (metadata, body, diagnostics)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_frontmatter_returns_empty_metadata() {
        let (meta, body, diags) = parse_frontmatter("hello world");
        assert!(diags.is_empty());
        assert_eq!(body, "hello world");
        assert_eq!(meta, Value::Mapping(serde_yaml::Mapping::new()));
    }

    #[test]
    fn basic_frontmatter_parsed() {
        let input = "---\nname: rust\ndescription: Rust skill\n---\n\nContent here";
        let (meta, body, diags) = parse_frontmatter(input);
        assert!(diags.is_empty());
        assert_eq!(meta["name"].as_str().unwrap(), "rust");
        assert_eq!(meta["description"].as_str().unwrap(), "Rust skill");
        assert_eq!(body, "Content here");
    }

    #[test]
    fn crlf_normalized() {
        let input = "---\r\nname: test\r\n---\r\nbody";
        let (meta, body, diags) = parse_frontmatter(input);
        assert!(diags.is_empty());
        assert_eq!(meta["name"].as_str().unwrap(), "test");
        assert_eq!(body, "body");
    }

    #[test]
    fn invalid_yaml_returns_warning() {
        let input = "---\ninvalid: [unclosed\n---\nbody";
        let (_, _, diags) = parse_frontmatter(input);
        assert!(!diags.is_empty());
    }
}
