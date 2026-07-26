use std::fs;
use std::path::PathBuf;

#[test]
fn root_public_modules_are_marked_migration_private() {
    let crate_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let lib_source = fs::read_to_string(crate_root.join("src/lib.rs")).expect("read ai lib.rs");
    let mut violations = Vec::new();
    let mut previous_non_empty = "";
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
            && previous_non_empty != "#[doc(hidden)]"
        {
            violations.push(format!(
                "src/lib.rs:{}: root module `{module}` should be marked #[doc(hidden)] migration-private",
                line_index + 1
            ));
        }

        if !trimmed.is_empty() {
            previous_non_empty = trimmed;
        }

        brace_depth = brace_depth
            .saturating_add(line.matches('{').count())
            .saturating_sub(line.matches('}').count());
    }

    assert!(
        violations.is_empty(),
        "ai::api must remain the only public root module:\n{}",
        violations.join("\n")
    );
}

#[test]
fn testing_facade_requires_explicit_test_support() {
    let crate_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let api_source = fs::read_to_string(crate_root.join("src/api.rs")).expect("read ai api.rs");
    let testing_module = api_source
        .find("pub mod testing")
        .expect("categorized testing facade should exist");
    let prefix = &api_source[..testing_module];
    let previous_non_empty = prefix
        .lines()
        .rev()
        .find(|line| !line.trim().is_empty())
        .map(str::trim);

    assert_eq!(
        previous_non_empty,
        Some("#[cfg(any(test, feature = \"test-support\"))]"),
        "ai::api::testing must be absent from normal dependency builds"
    );
}

#[test]
fn stable_facade_does_not_advertise_dto_only_image_generation() {
    let crate_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let api_source = fs::read_to_string(crate_root.join("src/api.rs")).expect("read ai api.rs");
    assert!(
        !api_source.contains("pub mod images"),
        "image-generation DTOs must remain private until a production provider contract exists"
    );
}
