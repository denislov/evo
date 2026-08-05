//! Crate-root public module boundary coverage.

use std::fs;
use std::path::PathBuf;

#[test]
fn stable_facade_is_the_only_public_root_module() {
    let crate_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let source = fs::read_to_string(crate_root.join("src/lib.rs")).expect("read ai lib.rs");
    assert_only_public_root_module(&source, "api");
}

fn assert_only_public_root_module(source: &str, expected: &str) {
    let mut public_modules = Vec::new();
    let mut brace_depth = 0_usize;

    for line in source.lines() {
        let trimmed = line.trim();
        if brace_depth == 0
            && let Some(module) = trimmed.strip_prefix("pub mod ").and_then(|module| {
                module
                    .trim_end_matches(';')
                    .trim_end_matches('{')
                    .split_whitespace()
                    .next()
            })
        {
            public_modules.push(module.to_owned());
        }
        brace_depth = brace_depth
            .saturating_add(line.matches('{').count())
            .saturating_sub(line.matches('}').count());
    }

    assert_eq!(
        public_modules,
        [expected],
        "ai must expose only its categorized api facade"
    );
}
