use std::sync::LazyLock;

use regex::Regex;

use crate::agent::types::Skill;

pub fn format_skills_for_system_prompt(skills: &[Skill]) -> String {
    if skills.is_empty() {
        return String::new();
    }

    let mut out = String::from("<available_skills>\n");

    for skill in skills {
        if skill.disable_model_invocation {
            continue;
        }
        out.push_str(&format!(
            "  <skill>\n    <name>{}</name>\n    <description>{}</description>\n    <location>{}</location>\n  </skill>\n",
            xml_escape(&skill.name),
            xml_escape(&skill.description),
            xml_escape(&skill.location),
        ));
    }

    out.push_str("</available_skills>");
    out
}

pub fn format_skill_invocation(
    name: &str,
    location: &str,
    content: &str,
    additional_instructions: Option<&str>,
) -> String {
    let mut out = format!(
        "<skill name=\"{}\" location=\"{}\">\n{}\n</skill>",
        xml_escape(name),
        xml_escape(location),
        content
    );

    if let Some(instructions) = additional_instructions {
        out.push_str(&format!("\n\n{}", instructions));
    }

    out
}

/// Parse command arguments respecting quoted strings (bash-style).
///
/// Mirrors TS `parseCommandArgs` in `packages/coding-agent/src/core/prompt-templates.ts`.
pub fn parse_command_args(args_string: &str) -> Vec<String> {
    let mut args: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut in_quote: Option<char> = None;

    for c in args_string.chars() {
        match in_quote {
            Some(quote) => {
                if c == quote {
                    in_quote = None;
                } else {
                    current.push(c);
                }
            }
            None => {
                if c == '"' || c == '\'' {
                    in_quote = Some(c);
                } else if c.is_whitespace() {
                    if !current.is_empty() {
                        args.push(std::mem::take(&mut current));
                    }
                } else {
                    current.push(c);
                }
            }
        }
    }

    if !current.is_empty() {
        args.push(current);
    }

    args
}

static SUBSTITUTE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\$\{(\d+):-([^}]*)\}|\$\{@:(\d+)(?::(\d+))?\}|\$(ARGUMENTS|@|\d+)")
        .expect("invalid substitute_args regex")
});

/// Substitute argument placeholders in template content.
///
/// Supports:
/// - $1, $2, ... for positional args
/// - $@ and $ARGUMENTS for all args
/// - ${N:-default} for positional arg N with default when missing/empty
/// - ${@:N} for args from Nth onwards (bash-style slicing)
/// - ${@:N:L} for L args starting from Nth
///
/// Mirrors TS `substituteArgs` in `packages/coding-agent/src/core/prompt-templates.ts`.
pub fn substitute_args(content: &str, args: &[String]) -> String {
    let all_args = args.join(" ");

    SUBSTITUTE_RE
        .replace_all(content, |caps: &regex::Captures| {
            // Group 1, 2: ${N:-default}
            if let Some(default_num) = caps.get(1) {
                let index: usize = default_num.as_str().parse().unwrap_or(1) - 1;
                let default_val = caps.get(2).map(|m| m.as_str()).unwrap_or("");
                match args.get(index) {
                    Some(v) if !v.is_empty() => v.clone(),
                    _ => default_val.to_string(),
                }
            }
            // Group 3, 4: ${@:N} or ${@:N:L}
            else if let Some(slice_start) = caps.get(3) {
                let start: usize = slice_start.as_str().parse().unwrap_or(1);
                // Treat 0 as 1 (bash convention: args start at 1)
                let start = if start == 0 { 0 } else { start - 1 };

                if start >= args.len() {
                    return String::new();
                }

                if let Some(slice_len) = caps.get(4) {
                    let len: usize = slice_len.as_str().parse().unwrap_or(0);
                    args[start..]
                        .iter()
                        .take(len)
                        .map(|s| s.as_str())
                        .collect::<Vec<&str>>()
                        .join(" ")
                } else {
                    args[start..]
                        .iter()
                        .map(|s| s.as_str())
                        .collect::<Vec<&str>>()
                        .join(" ")
                }
            }
            // Group 5: $ARGUMENTS, $@, or $N
            else if let Some(simple) = caps.get(5) {
                let s = simple.as_str();
                if s == "ARGUMENTS" || s == "@" {
                    all_args.clone()
                } else {
                    let index: usize = s.parse().unwrap_or(1) - 1;
                    args.get(index).cloned().unwrap_or_default()
                }
            } else {
                String::new()
            }
        })
        .to_string()
}

/// Format a prompt template invocation, substituting arguments into the template content.
///
/// Delegates to [`substitute_args`] for full TS-compatible placeholders.
pub fn format_prompt_template_invocation(_name: &str, content: &str, args: &[String]) -> String {
    substitute_args(content, args)
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_skills_xml_block() {
        let skills = vec![Skill {
            name: "rust".into(),
            description: "Rust programming".into(),
            location: "/skills/rust/SKILL.md".into(),
            content: String::new(),
            disable_model_invocation: false,
        }];
        let formatted = format_skills_for_system_prompt(&skills);
        assert!(formatted.contains("<available_skills>"));
        assert!(formatted.contains("<name>rust</name>"));
        assert!(formatted.contains("Rust programming"));
        assert!(formatted.contains("</available_skills>"));
    }

    #[test]
    fn excludes_disabled_skills() {
        let skills = vec![
            Skill {
                name: "active".into(),
                description: "active".into(),
                location: "/a".into(),
                content: String::new(),
                disable_model_invocation: false,
            },
            Skill {
                name: "disabled".into(),
                description: "disabled".into(),
                location: "/d".into(),
                content: String::new(),
                disable_model_invocation: true,
            },
        ];
        let formatted = format_skills_for_system_prompt(&skills);
        assert!(formatted.contains("active"));
        assert!(!formatted.contains("disabled"));
    }

    #[test]
    fn empty_skills_returns_empty_string() {
        assert!(format_skills_for_system_prompt(&[]).is_empty());
    }

    #[test]
    fn skill_invocation_includes_location_and_content() {
        let text = format_skill_invocation(
            "rust",
            "/skills/rust/SKILL.md",
            "Rust programming guide",
            Some("use best practices"),
        );
        assert!(text.contains("rust"));
        assert!(text.contains("/skills/rust/SKILL.md"));
        assert!(text.contains("Rust programming guide"));
        assert!(text.contains("use best practices"));
    }

    // ── parse_command_args tests ──────────────────────────────────────

    // ── substitute_args tests ─────────────────────────────────────────

    fn owned_args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    #[test]
    fn template_invocation_applies_supported_placeholder_classes() {
        for (template, expected) in [
            ("Review $1 and ${2}", "Review foo and ${2}"),
            ("Args: $ARGUMENTS", "Args: foo bar"),
            ("Args: $@", "Args: foo bar"),
        ] {
            assert_eq!(
                format_prompt_template_invocation(
                    "template",
                    template,
                    &owned_args(&["foo", "bar"]),
                ),
                expected
            );
        }
    }

    #[test]
    fn parse_command_args_covers_whitespace_quotes_empty_and_unicode() {
        for (input, expected) in [
            ("a b c", vec!["a", "b", "c"]),
            ("a  b   c", vec!["a", "b", "c"]),
            ("\"first arg\" second", vec!["first arg", "second"]),
            ("'first arg' second", vec!["first arg", "second"]),
            ("", vec![]),
            ("   \t\n", vec![]),
            ("日本語 🎉 café", vec!["日本語", "🎉", "café"]),
        ] {
            assert_eq!(parse_command_args(input), expected, "input: {input:?}");
        }
    }

    #[test]
    fn substitute_all_args_aliases_and_literal_rules() {
        for (template, args, expected) in [
            ("Test: $ARGUMENTS", &[][..], "Test: "),
            ("Test: $@", &[][..], "Test: "),
            ("Test: $1", &[][..], "Test: "),
            ("Test: $ARGUMENTS", &["a", "b", "c"][..], "Test: a b c"),
            ("Test: $@", &["a", "b", "c"][..], "Test: a b c"),
            ("$ARGUMENTS and $ARGUMENTS", &["a", "b"][..], "a b and a b"),
            ("$@ and $@", &["a", "b"][..], "a b and a b"),
            (
                "$arguments $Arguments $ARGUMENTS",
                &["a", "b"][..],
                "$arguments $Arguments a b",
            ),
            (
                "$ARGUMENTS",
                &["日本語", "🎉", "café"][..],
                "日本語 🎉 café",
            ),
            ("$A $$ $ $ARGS", &["a"][..], "$A $$ $ $ARGS"),
            ("${1}", &["a"][..], "${1}"),
        ] {
            assert_eq!(
                substitute_args(template, &owned_args(args)),
                expected,
                "template: {template:?}"
            );
        }
    }

    #[test]
    fn substitute_positional_defaults_and_out_of_range_values() {
        for (template, args, expected) in [
            (
                "List exactly ${1:-7} next steps",
                &[][..],
                "List exactly 7 next steps",
            ),
            (
                "List exactly ${1:-7} next steps",
                &["3"][..],
                "List exactly 3 next steps",
            ),
            ("Mode: ${1:-brief}", &[""][..], "Mode: brief"),
            ("$1 $2 $3 $4 $5", &["a", "b"][..], "a b   "),
        ] {
            assert_eq!(substitute_args(template, &owned_args(args)), expected);
        }
    }

    #[test]
    fn substitute_array_slice_classes() {
        let args = owned_args(&["a", "b", "c", "d"]);
        for (template, expected) in [
            ("${@:2}", "b c d"),
            ("${@:2:2}", "b c"),
            ("${@:0}", "a b c d"),
        ] {
            assert_eq!(substitute_args(template, &args), expected);
        }
    }

    #[test]
    fn substitute_does_not_recursively_expand_argument_content() {
        assert_eq!(
            substitute_args("$ARGUMENTS", &owned_args(&["$1", "$ARGUMENTS"])),
            "$1 $ARGUMENTS"
        );
        assert_eq!(
            substitute_args("$@", &owned_args(&["$100", "$1"])),
            "$100 $1"
        );
    }

    #[test]
    fn xml_escape_works() {
        let escaped = xml_escape("<tag attr=\"val\">&amp;");
        assert_eq!(escaped, "&lt;tag attr=&quot;val&quot;&gt;&amp;amp;");
    }
}
