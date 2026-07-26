//! Autocomplete discovery and combination behavior.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use tui::api::input::{
    AutocompleteItem, AutocompleteOptions, CombinedAutocompleteProvider, SlashCommand,
};

static TEMP_ROOT_COUNTER: AtomicUsize = AtomicUsize::new(0);

fn temp_root() -> PathBuf {
    let suffix = TEMP_ROOT_COUNTER.fetch_add(1, Ordering::Relaxed);
    let root =
        std::env::temp_dir().join(format!("tui-autocomplete-{}-{suffix}", std::process::id()));
    fs::create_dir_all(&root).expect("create temp root");
    root
}

fn write_file(path: &Path) {
    fs::write(path, b"test").expect("write test file");
}

fn provider(base_path: &Path) -> CombinedAutocompleteProvider {
    CombinedAutocompleteProvider::with_env(
        vec![
            SlashCommand::new("model").description("Select model"),
            SlashCommand::new("session").argument_hint("[id]"),
        ],
        base_path,
        vec![
            ("HOME".to_string(), "/home/example".to_string()),
            ("HOSTNAME".to_string(), "devbox".to_string()),
        ],
    )
}

#[test]
fn autocomplete_suggests_slash_commands_with_fuzzy_matching() {
    let root = temp_root();
    let provider = provider(&root);

    let suggestions = provider
        .get_suggestions(&["/mo".to_string()], 0, 3, AutocompleteOptions::default())
        .expect("slash suggestions");

    assert_eq!(suggestions.prefix, "/mo");
    assert_eq!(suggestions.items[0].value, "model");
    assert_eq!(suggestions.items[0].label, "model");
}

#[test]
fn autocomplete_applies_slash_command_with_trailing_space() {
    let root = temp_root();
    let provider = provider(&root);

    let edit = provider.apply_completion(
        &["/mo please".to_string()],
        0,
        3,
        &AutocompleteItem::new("model", "model"),
        "/mo",
    );

    assert_eq!(edit.lines, vec!["/model  please".to_string()]);
    assert_eq!(edit.cursor_line, 0);
    assert_eq!(edit.cursor_col, 7);
}

#[test]
fn autocomplete_forced_file_completion_lists_directories_first() {
    let root = temp_root();
    fs::create_dir(root.join("src")).expect("create src");
    write_file(&root.join("sample.txt"));
    let provider = provider(&root);

    let suggestions = provider
        .get_suggestions(&["".to_string()], 0, 0, AutocompleteOptions { force: true })
        .expect("file suggestions");

    assert_eq!(suggestions.prefix, "");
    assert_eq!(suggestions.items[0].value, "src/");
    assert_eq!(suggestions.items[0].label, "src/");
    assert!(
        suggestions
            .items
            .iter()
            .any(|item| item.value == "sample.txt")
    );
}

#[test]
fn autocomplete_applies_at_file_completion_with_space_for_files() {
    let root = temp_root();
    write_file(&root.join("README.md"));
    let provider = provider(&root);

    let suggestions = provider
        .get_suggestions(&["@REA".to_string()], 0, 4, AutocompleteOptions::default())
        .expect("attachment suggestions");
    let edit = provider.apply_completion(
        &["@REA now".to_string()],
        0,
        4,
        &suggestions.items[0],
        &suggestions.prefix,
    );

    assert_eq!(suggestions.items[0].value, "@README.md");
    assert_eq!(edit.lines, vec!["@README.md  now".to_string()]);
    assert_eq!(edit.cursor_col, 11);
}

#[test]
fn autocomplete_suggests_environment_variables_from_injected_env() {
    let root = temp_root();
    let provider = provider(&root);

    let suggestions = provider
        .get_suggestions(
            &["echo $HO".to_string()],
            0,
            8,
            AutocompleteOptions::default(),
        )
        .expect("env suggestions");

    assert_eq!(suggestions.prefix, "$HO");
    assert_eq!(suggestions.items[0].value, "$HOME");
    assert_eq!(suggestions.items[0].label, "HOME");
}

#[test]
fn autocomplete_file_trigger_skips_bare_slash_commands() {
    let root = temp_root();
    let provider = provider(&root);

    assert!(!provider.should_trigger_file_completion(&["/model".to_string()], 0, 6));
    assert!(provider.should_trigger_file_completion(&["open ./".to_string()], 0, 7));
}
