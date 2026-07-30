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
