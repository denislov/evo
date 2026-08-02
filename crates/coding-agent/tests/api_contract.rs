//! Crate-root public module boundary coverage.

use std::fs;
use std::path::PathBuf;

use syn::{Fields, Item, Visibility};

#[test]
fn stable_facade_is_the_only_public_root_module() {
    let crate_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let lib_source =
        fs::read_to_string(crate_root.join("src/lib.rs")).expect("read coding-agent lib.rs");
    let mut violations = Vec::new();
    let mut brace_depth = 0_usize;

    for (line_index, line) in lib_source.lines().enumerate() {
        let trimmed = line.trim();
        if brace_depth == 0
            && let Some(module) = trimmed.strip_prefix("pub mod ").and_then(|module| {
                module
                    .trim_end_matches(';')
                    .trim_end_matches('{')
                    .split_whitespace()
                    .next()
            })
            && module != "api"
        {
            violations.push(format!(
                "src/lib.rs:{}: root implementation module `{module}` must remain private",
                line_index + 1
            ));
        }

        brace_depth = brace_depth
            .saturating_add(line.matches('{').count())
            .saturating_sub(line.matches('}').count());
    }

    assert!(
        violations.is_empty(),
        "coding_agent::api must be the only public root module:\n{}",
        violations.join("\n")
    );
}

#[test]
fn evolving_session_response_dtos_cannot_be_constructed_with_downstream_literals() {
    const RESPONSE_DTOS: [&str; 8] = [
        "CodingAgentSessionView",
        "CodingAgentRecoveryPending",
        "CodingAgentRecoveryResolutionResult",
        "CodingAgentRecoveryRetryResult",
        "CodingAgentSessionSummary",
        "CodingAgentSessionOverview",
        "CodingAgentSessionOpenTarget",
        "CodingAgentTranscriptSnapshot",
    ];

    let crate_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let path = crate_root.join("src/session/view.rs");
    let source = fs::read_to_string(&path).expect("read session response DTO source");
    let syntax = syn::parse_file(&source).expect("parse session response DTO source");
    let mut protected = Vec::new();

    for item in syntax.items {
        let Item::Struct(item) = item else {
            continue;
        };
        if !RESPONSE_DTOS.contains(&item.ident.to_string().as_str()) {
            continue;
        }
        let non_exhaustive = item
            .attrs
            .iter()
            .any(|attribute| attribute.path().is_ident("non_exhaustive"));
        let has_private_field = match &item.fields {
            Fields::Named(fields) => fields
                .named
                .iter()
                .any(|field| !matches!(field.vis, Visibility::Public(_))),
            Fields::Unnamed(fields) => fields
                .unnamed
                .iter()
                .any(|field| !matches!(field.vis, Visibility::Public(_))),
            Fields::Unit => false,
        };
        assert!(
            non_exhaustive || has_private_field,
            "{} must be #[non_exhaustive] or expose at least one private field so adding response fields does not break downstream compilation",
            item.ident
        );
        protected.push(item.ident.to_string());
    }

    protected.sort();
    let mut expected = RESPONSE_DTOS.map(str::to_owned).to_vec();
    expected.sort();
    assert_eq!(
        protected, expected,
        "the response DTO guard must cover every named stable session response"
    );

    for constructor in [
        "impl CodingAgentSessionView",
        "pub fn new(",
        "impl CodingAgentRecoveryPending",
        "pub fn from_parts(",
        "impl CodingAgentTranscriptSnapshot",
    ] {
        assert!(
            source.contains(constructor),
            "protected DTOs consumed by adapters must retain constructor marker `{constructor}`"
        );
    }
}
