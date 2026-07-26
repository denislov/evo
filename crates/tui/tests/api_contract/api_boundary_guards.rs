//! Crate-root public module boundary coverage.

use std::fs;
use std::path::PathBuf;

#[test]
fn stable_facade_is_the_only_public_root_module() {
    let crate_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let lib_source = fs::read_to_string(crate_root.join("src/lib.rs")).expect("read tui lib.rs");
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
        "tui::api must be the only public root module:\n{}",
        violations.join("\n")
    );
}
