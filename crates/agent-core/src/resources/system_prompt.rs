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
            // Group 1, 2: ${N:-default}. Mirrors TS `substituteArgs`: an
            // index of 0 (or an unparsable index) resolves past the start of
            // the argument list, so it falls through to the default value.
            if let Some(default_num) = caps.get(1) {
                let index = positional_index(default_num.as_str());
                let default_val = caps.get(2).map(|m| m.as_str()).unwrap_or("");
                match index.and_then(|i| args.get(i)) {
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
                    positional_index(s)
                        .and_then(|i| args.get(i))
                        .cloned()
                        .unwrap_or_default()
                }
            } else {
                String::new()
            }
        })
        .to_string()
}

/// Resolve a `$N`-style placeholder to a zero-based index. `None` (instead of
/// `0`) when the text is unparsable or zero, matching TS behavior where
/// `args[Number(s) - 1]` is `undefined` for those inputs; this also avoids an
/// `usize` underflow when the placeholder is `0`.
fn positional_index(text: &str) -> Option<usize> {
    text.parse::<usize>().ok()?.checked_sub(1)
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
    use super::{parse_command_args, substitute_args};

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_string()).collect()
    }

    #[test]
    fn substitutes_positional_arguments() {
        assert_eq!(
            substitute_args("run $1 with $2", &args(&["tool", "a", "b"])),
            "run tool with a"
        );
    }

    #[test]
    fn substitutes_all_arguments_forms() {
        let values = args(&["a", "b", "c"]);
        assert_eq!(substitute_args("$@", &values), "a b c");
        assert_eq!(substitute_args("$ARGUMENTS", &values), "a b c");
        assert_eq!(substitute_args("${@:2}", &values), "b c");
        assert_eq!(substitute_args("${@:2:1}", &values), "b");
    }

    #[test]
    fn substitutes_default_values() {
        let values = args(&["a"]);
        assert_eq!(substitute_args("${1:-none}", &values), "a");
        assert_eq!(substitute_args("${2:-none}", &values), "none");
        assert_eq!(substitute_args("${2:-}", &values), "");
    }

    #[test]
    fn empty_arguments_use_the_default() {
        assert_eq!(substitute_args("${1:-fallback}", &args(&[""])), "fallback");
    }

    #[test]
    fn out_of_range_and_unparsable_indices_do_not_panic() {
        assert_eq!(substitute_args("$9", &args(&["a"])), "");
        assert_eq!(substitute_args("${9:-none}", &args(&["a"])), "none");
        assert_eq!(
            substitute_args("${xyz:-none}", &args(&["a"])),
            "${xyz:-none}"
        );
    }

    #[test]
    fn zero_index_does_not_underflow() {
        assert_eq!(substitute_args("$0", &args(&["a", "b"])), "");
        assert_eq!(substitute_args("${0:-none}", &args(&["a", "b"])), "none");
        assert_eq!(substitute_args("${00:-none}", &args(&["a", "b"])), "none");
    }

    #[test]
    fn parses_quoted_command_arguments() {
        assert_eq!(
            parse_command_args("one \"two three\" four 'five six'"),
            vec!["one", "two three", "four", "five six"]
        );
        assert_eq!(parse_command_args(""), Vec::<String>::new());
    }
}
