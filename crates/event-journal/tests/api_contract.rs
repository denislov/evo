use std::fs;
use std::path::PathBuf;

#[test]
fn stable_facade_is_the_only_public_root_module() {
    let source = fs::read_to_string(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/lib.rs"))
        .expect("read event-journal lib.rs");
    let public_modules = source
        .lines()
        .filter_map(|line| line.trim().strip_prefix("pub mod "))
        .map(|line| line.trim_end_matches(';'))
        .collect::<Vec<_>>();
    assert_eq!(public_modules, ["api"]);
}
