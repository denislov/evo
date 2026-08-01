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
    use super::parse_frontmatter;

    #[test]
    fn parses_metadata_and_body() {
        let (meta, body, diagnostics) = parse_frontmatter(
            "---\nname: test-skill\ndescription: A skill\n---\n# Skill body\ncontent",
        );
        assert!(diagnostics.is_empty());
        assert_eq!(meta["name"], "test-skill");
        assert_eq!(meta["description"], "A skill");
        assert_eq!(body, "# Skill body\ncontent");
    }

    #[test]
    fn missing_frontmatter_keeps_the_whole_content() {
        let (meta, body, diagnostics) = parse_frontmatter("plain markdown\nno metadata");
        assert!(diagnostics.is_empty());
        assert!(meta.is_null() || meta.as_mapping().is_some_and(|m| m.is_empty()));
        assert_eq!(body, "plain markdown\nno metadata");
    }

    #[test]
    fn unclosed_frontmatter_is_diagnosed_and_ignored() {
        let (meta, body, diagnostics) = parse_frontmatter("---\nname: x\nno closing marker");
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].code, "frontmatter_no_closing");
        assert_eq!(body, "---\nname: x\nno closing marker");
        assert!(meta.as_mapping().is_some_and(|m| m.is_empty()));
    }

    #[test]
    fn closing_marker_must_be_on_its_own_line() {
        let (_, body, diagnostics) = parse_frontmatter("---\nname: x\n--- not a closing\ncontent");
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(body, "---\nname: x\n--- not a closing\ncontent");
    }

    #[test]
    fn crlf_input_is_normalized() {
        let (meta, body, diagnostics) =
            parse_frontmatter("---\r\nname: x\r\n---\r\nbody line");
        assert!(diagnostics.is_empty());
        assert_eq!(meta["name"], "x");
        assert_eq!(body, "body line");
    }

    #[test]
    fn invalid_yaml_is_diagnosed() {
        let (meta, body, diagnostics) = parse_frontmatter("---\nname: [unclosed\n---\nbody");
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].code, "frontmatter_parse_error");
        assert_eq!(body, "body");
        assert!(meta.as_mapping().is_some_and(|m| m.is_empty()));
    }

    #[test]
    fn empty_body_is_supported() {
        let (_, body, diagnostics) = parse_frontmatter("---\nname: x\n---");
        assert!(diagnostics.is_empty());
        assert_eq!(body, "");
    }
}
