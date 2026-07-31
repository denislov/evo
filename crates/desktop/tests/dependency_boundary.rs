use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
};

use syn::{
    Attribute, File, Ident, Item, ItemUse, UseTree, Visibility,
    visit::{self, Visit},
};

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn workspace_root() -> PathBuf {
    manifest_dir()
        .parent()
        .and_then(Path::parent)
        .expect("desktop crate sits two levels below the workspace root")
        .to_path_buf()
}

fn read_toml(path: impl AsRef<Path>) -> toml::Value {
    let path = path.as_ref();
    let source = fs::read_to_string(path)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()));
    toml::from_str(&source)
        .unwrap_or_else(|error| panic!("failed to parse {} as TOML: {error}", path.display()))
}

fn manifest() -> toml::Value {
    read_toml(manifest_dir().join("Cargo.toml"))
}

fn dependency_names(value: &toml::Value, names: &mut BTreeSet<String>) {
    let Some(table) = value.as_table() else {
        return;
    };
    for (key, child) in table {
        if matches!(
            key.as_str(),
            "dependencies" | "dev-dependencies" | "build-dependencies"
        ) && let Some(dependencies) = child.as_table()
        {
            names.extend(dependencies.keys().cloned());
        }
        dependency_names(child, names);
    }
}

fn parse_rust(path: impl AsRef<Path>) -> File {
    let path = path.as_ref();
    let source = fs::read_to_string(path)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()));
    syn::parse_file(&source)
        .unwrap_or_else(|error| panic!("failed to parse {} as Rust: {error}", path.display()))
}

fn public_surface(file: &File) -> BTreeSet<String> {
    file.items
        .iter()
        .filter_map(|item| match item {
            Item::Const(item) if matches!(item.vis, Visibility::Public(_)) => {
                Some(format!("const {}", item.ident))
            }
            Item::Enum(item) if matches!(item.vis, Visibility::Public(_)) => {
                Some(format!("enum {}", item.ident))
            }
            Item::Fn(item) if matches!(item.vis, Visibility::Public(_)) => {
                Some(format!("fn {}", item.sig.ident))
            }
            Item::Mod(item) if matches!(item.vis, Visibility::Public(_)) => {
                Some(format!("mod {}", item.ident))
            }
            Item::Static(item) if matches!(item.vis, Visibility::Public(_)) => {
                Some(format!("static {}", item.ident))
            }
            Item::Struct(item) if matches!(item.vis, Visibility::Public(_)) => {
                Some(format!("struct {}", item.ident))
            }
            Item::Trait(item) if matches!(item.vis, Visibility::Public(_)) => {
                Some(format!("trait {}", item.ident))
            }
            Item::Type(item) if matches!(item.vis, Visibility::Public(_)) => {
                Some(format!("type {}", item.ident))
            }
            Item::Union(item) if matches!(item.vis, Visibility::Public(_)) => {
                Some(format!("union {}", item.ident))
            }
            Item::Use(item) if matches!(item.vis, Visibility::Public(_)) => Some("use".into()),
            _ => None,
        })
        .collect()
}

fn rust_files(root: impl AsRef<Path>) -> Vec<PathBuf> {
    fn collect(root: &Path, files: &mut Vec<PathBuf>) {
        let mut entries = fs::read_dir(root)
            .unwrap_or_else(|error| panic!("failed to read {}: {error}", root.display()))
            .collect::<Result<Vec<_>, _>>()
            .unwrap_or_else(|error| panic!("failed to enumerate {}: {error}", root.display()));
        entries.sort_by_key(fs::DirEntry::path);
        for entry in entries {
            let path = entry.path();
            if path.is_dir() {
                collect(&path, files);
            } else if path.extension().is_some_and(|extension| extension == "rs") {
                files.push(path);
            }
        }
    }

    let root = root.as_ref();
    if !root.exists() {
        return Vec::new();
    }
    let mut files = Vec::new();
    collect(root, &mut files);
    files
}

fn layer_rust_files(layer: &str) -> Vec<PathBuf> {
    let source_root = manifest_dir().join("src");
    let mut files = rust_files(source_root.join(layer));
    let flat_module = source_root.join(format!("{layer}.rs"));
    if flat_module.is_file() {
        files.push(flat_module);
    }
    files.sort();
    files
}

fn is_test_only_source(path: &Path) -> bool {
    path.file_stem().is_some_and(|stem| stem == "tests")
        || path
            .components()
            .any(|component| component.as_os_str() == "tests")
}

fn item_attributes(item: &Item) -> &[Attribute] {
    match item {
        Item::Const(item) => &item.attrs,
        Item::Enum(item) => &item.attrs,
        Item::ExternCrate(item) => &item.attrs,
        Item::Fn(item) => &item.attrs,
        Item::ForeignMod(item) => &item.attrs,
        Item::Impl(item) => &item.attrs,
        Item::Macro(item) => &item.attrs,
        Item::Mod(item) => &item.attrs,
        Item::Static(item) => &item.attrs,
        Item::Struct(item) => &item.attrs,
        Item::Trait(item) => &item.attrs,
        Item::TraitAlias(item) => &item.attrs,
        Item::Type(item) => &item.attrs,
        Item::Union(item) => &item.attrs,
        Item::Use(item) => &item.attrs,
        Item::Verbatim(_) | _ => &[],
    }
}

fn has_test_cfg(attributes: &[Attribute]) -> bool {
    attributes.iter().any(|attribute| {
        if !attribute.path().is_ident("cfg") {
            return false;
        }
        attribute
            .meta
            .require_list()
            .is_ok_and(|list| list.tokens.to_string() == "test")
    })
}

fn collect_use_paths(tree: &UseTree, prefix: &mut Vec<String>, paths: &mut BTreeSet<String>) {
    match tree {
        UseTree::Path(path) => {
            prefix.push(path.ident.to_string());
            collect_use_paths(&path.tree, prefix, paths);
            prefix.pop();
        }
        UseTree::Name(name) => {
            prefix.push(name.ident.to_string());
            paths.insert(prefix.join("::"));
            prefix.pop();
        }
        UseTree::Rename(rename) => {
            prefix.push(rename.ident.to_string());
            paths.insert(prefix.join("::"));
            prefix.pop();
        }
        UseTree::Glob(_) => {
            prefix.push("*".into());
            paths.insert(prefix.join("::"));
            prefix.pop();
        }
        UseTree::Group(group) => {
            for tree in &group.items {
                collect_use_paths(tree, prefix, paths);
            }
        }
    }
}

#[derive(Default)]
struct ProductionFacts {
    identifiers: BTreeSet<String>,
    paths: BTreeSet<String>,
    imports: BTreeSet<String>,
}

impl ProductionFacts {
    fn all_paths(&self) -> impl Iterator<Item = &str> {
        self.paths.iter().chain(&self.imports).map(String::as_str)
    }
}

impl<'ast> Visit<'ast> for ProductionFacts {
    fn visit_item(&mut self, item: &'ast Item) {
        if has_test_cfg(item_attributes(item)) {
            return;
        }
        visit::visit_item(self, item);
    }

    fn visit_ident(&mut self, ident: &'ast Ident) {
        self.identifiers.insert(ident.to_string());
        visit::visit_ident(self, ident);
    }

    fn visit_item_mod(&mut self, module: &'ast syn::ItemMod) {
        if module.ident == "tests" || has_test_cfg(&module.attrs) {
            return;
        }
        visit::visit_item_mod(self, module);
    }

    fn visit_item_use(&mut self, item: &'ast ItemUse) {
        let mut prefix = Vec::new();
        collect_use_paths(&item.tree, &mut prefix, &mut self.imports);
        visit::visit_item_use(self, item);
    }

    fn visit_path(&mut self, path: &'ast syn::Path) {
        let joined = path
            .segments
            .iter()
            .map(|segment| segment.ident.to_string())
            .collect::<Vec<_>>()
            .join("::");
        if !joined.is_empty() {
            self.paths.insert(joined);
        }
        visit::visit_path(self, path);
    }
}

fn production_facts(path: impl AsRef<Path>) -> ProductionFacts {
    let file = parse_rust(path);
    let mut facts = ProductionFacts::default();
    facts.visit_file(&file);
    facts
}

fn rust_identifiers(path: impl AsRef<Path>) -> BTreeSet<String> {
    production_facts(path).identifiers
}

fn path_has_segments(path: &str, expected: &[&str]) -> bool {
    let segments = path.split("::").collect::<Vec<_>>();
    segments
        .windows(expected.len())
        .any(|window| window == expected)
}

fn assert_paths_exclude(path: &Path, facts: &ProductionFacts, forbidden: &[&[&str]]) {
    for dependency in forbidden {
        assert!(
            !facts
                .all_paths()
                .any(|candidate| path_has_segments(candidate, dependency)),
            "{} must not depend on {}",
            path.display(),
            dependency.join("::")
        );
    }
}

#[test]
fn desktop_depends_on_product_facade_without_bypassing_runtime_layers() {
    let mut names = BTreeSet::new();
    dependency_names(&manifest(), &mut names);

    assert!(names.contains("coding-agent"));
    for forbidden in ["ai", "agent-core", "tui"] {
        assert!(
            !names.contains(forbidden),
            "desktop must not depend directly on {forbidden}"
        );
    }
}

#[test]
fn unstable_ui_dependencies_are_exactly_pinned() {
    let manifest = manifest();
    let dependencies = manifest["dependencies"]
        .as_table()
        .expect("dependencies table");
    let component = &dependencies["gpui-component"];
    let assets = &dependencies["gpui-component-assets"];
    assert_eq!(
        component["rev"].as_str(),
        Some("bc174a7ec4534b2a4174fddde314b38d30d69093")
    );
    assert_eq!(
        component["git"].as_str(),
        Some("https://github.com/longbridge/gpui-component.git")
    );
    assert_eq!(assets["rev"], component["rev"]);
    assert_eq!(assets["git"], component["git"]);

    let targets = manifest["target"].as_table().expect("target table");
    for target in [
        "cfg(target_os = \"linux\")",
        "cfg(target_os = \"macos\")",
        "cfg(target_os = \"windows\")",
    ] {
        assert_eq!(
            targets[target]["dependencies"]["gpui"]["git"].as_str(),
            Some("https://github.com/zed-industries/zed.git")
        );
        assert!(
            targets[target]["dependencies"]
                .get("gpui_platform")
                .is_some()
        );
    }

    let lock = read_toml(workspace_root().join("Cargo.lock"));
    let packages = lock["package"].as_array().expect("Cargo.lock packages");
    assert!(packages.iter().any(|package| {
        package["name"].as_str() == Some("gpui")
            && package["source"].as_str()
                == Some(
                    "git+https://github.com/zed-industries/zed.git#30730a305ae235f3be44643d5895e142048ef701",
                )
    }));
}

#[test]
fn release_memory_probe_has_the_windows_runtime_dependencies() {
    let manifest = manifest();
    let windows = &manifest["target"]["cfg(target_os = \"windows\")"];
    let windows_sys = &windows["dependencies"]["windows-sys"];
    assert_eq!(windows_sys["version"].as_str(), Some("0.61"));
    let features = windows_sys["features"]
        .as_array()
        .expect("windows-sys features should be explicit")
        .iter()
        .filter_map(toml::Value::as_str)
        .collect::<BTreeSet<_>>();
    assert_eq!(
        features,
        BTreeSet::from([
            "Win32_Foundation",
            "Win32_System_ProcessStatus",
            "Win32_System_Threading",
        ])
    );
}

#[test]
fn external_gate_entrypoints_are_paired_and_committed() {
    let root = workspace_root();
    for pair in [
        [
            "scripts/desktop-native-perf-gate.sh",
            "scripts/desktop-native-perf-gate.ps1",
        ],
        [
            "scripts/desktop-perf-gate.sh",
            "scripts/desktop-perf-gate.ps1",
        ],
        [
            "scripts/desktop-click-to-photon.sh",
            "scripts/desktop-click-to-photon.ps1",
        ],
    ] {
        for relative in pair {
            let metadata = fs::metadata(root.join(relative))
                .unwrap_or_else(|error| panic!("missing gate {relative}: {error}"));
            assert!(
                metadata.is_file(),
                "gate entrypoint must be a file: {relative}"
            );
            assert!(
                metadata.len() > 0,
                "gate entrypoint must not be empty: {relative}"
            );
        }
    }
    for relative in [
        "scripts/desktop-click-to-photon-report.py",
        "scripts/desktop-click-to-photon-report-test.py",
        "scripts/desktop-visual-golden.sh",
        "crates/desktop/tests/goldens/native/REVIEW.md",
    ] {
        assert!(
            root.join(relative).is_file(),
            "missing gate artifact {relative}"
        );
    }
}

#[test]
fn vendored_ui_patch_state_matches_the_workspace_manifest_and_lockfile() {
    let root = workspace_root();
    let root_manifest = read_toml(root.join("Cargo.toml"));
    let patched = root_manifest["patch"]["https://github.com/longbridge/gpui-component.git"]
        .as_table()
        .expect("workspace declares the gpui-component patch table");
    assert_eq!(
        patched["gpui-component"]["path"].as_str(),
        Some("third-party/gpui-component/crates/ui")
    );
    assert_eq!(
        patched["gpui-component-assets"]["path"].as_str(),
        Some("third-party/gpui-component/crates/assets")
    );

    let patch_count = fs::read_dir(root.join("patches/gpui-component"))
        .expect("gpui-component patch archive exists")
        .filter_map(Result::ok)
        .filter(|entry| {
            entry
                .path()
                .extension()
                .is_some_and(|extension| extension == "patch")
        })
        .count();
    assert!(
        patch_count > 0,
        "the patched checkout must archive its delta"
    );

    let lock = read_toml(root.join("Cargo.lock"));
    let packages = lock["package"].as_array().expect("Cargo.lock packages");
    for name in ["gpui-component", "gpui-component-assets"] {
        let package = packages
            .iter()
            .find(|package| package["name"].as_str() == Some(name))
            .unwrap_or_else(|| panic!("Cargo.lock contains {name}"));
        assert!(
            package.get("source").is_none(),
            "{name} must resolve through the local workspace patch"
        );
    }
}

#[test]
fn desktop_public_api_is_one_typed_application_surface() {
    let library = parse_rust(manifest_dir().join("src/lib.rs"));
    assert_eq!(
        public_surface(&library),
        BTreeSet::from([
            "fn run".to_owned(),
            "struct DesktopApplicationOptions".to_owned(),
        ])
    );
    assert!(
        library
            .items
            .iter()
            .filter_map(|item| match item {
                Item::Mod(module) => Some(&module.vis),
                _ => None,
            })
            .all(|visibility| !matches!(visibility, Visibility::Public(_)))
    );

    let selected = PathBuf::from("/typed/project");
    let options = desktop::DesktopApplicationOptions::new(&selected).with_session_id("session-a");
    assert_eq!(options.cwd(), selected);
    assert_eq!(options.session_id(), Some("session-a"));
    assert!(!options.is_projectless());

    let projectless = desktop::DesktopApplicationOptions::projectless();
    assert!(projectless.is_projectless());
    assert_eq!(projectless.session_id(), None);
}

#[test]
fn production_sources_use_explicit_imports() {
    for path in rust_files(manifest_dir().join("src")) {
        if is_test_only_source(&path) {
            continue;
        }
        let facts = production_facts(&path);
        assert!(
            !facts.imports.contains("super::*"),
            "production module must not hide its authority dependencies behind use super::*: {}",
            path.display()
        );
    }
}

#[test]
fn native_shell_root_and_refresh_authority_stay_bounded() {
    let shell = manifest_dir().join("src/app/native_shell.rs");
    let source = fs::read_to_string(&shell).expect("native shell source is readable");
    let production = source
        .split("#[cfg(test)]\nmod tests")
        .next()
        .expect("native shell has a production section");
    assert!(
        production.lines().count() <= 1_200,
        "NativeShell production adapter exceeded 1,200 lines"
    );
    assert!(
        !production.contains("set_view_model("),
        "root adapter must not construct feature ViewModels"
    );
    assert!(
        !production.contains("::view_model("),
        "root adapter must not invoke feature presenters"
    );

    let native_shell_dir = manifest_dir().join("src/app/native_shell");
    let mut refresh_authorities = production.matches("fn refresh_views(").count();
    assert!(
        !production.contains("fn notify_"),
        "legacy notify authority returned in {}",
        shell.display()
    );
    for path in rust_files(native_shell_dir) {
        if is_test_only_source(&path) {
            continue;
        }
        let source = fs::read_to_string(&path).expect("native shell module is readable");
        refresh_authorities += source.matches("fn refresh_views(").count();
        assert!(
            !source.contains("fn notify_"),
            "legacy notify authority returned in {}",
            path.display()
        );
    }
    assert_eq!(
        refresh_authorities, 1,
        "refresh_views must have one authority"
    );
}

#[test]
fn application_layer_has_no_ui_or_effect_executor_dependencies() {
    for path in layer_rust_files("application") {
        if is_test_only_source(&path) {
            continue;
        }
        let facts = production_facts(&path);
        assert_paths_exclude(
            &path,
            &facts,
            &[
                &["gpui"],
                &["std", "fs"],
                &["std", "process"],
                &["std", "thread"],
                &["tokio"],
            ],
        );
    }
}

fn assert_leaf_ui_dependencies(path: &Path) {
    let identifiers = rust_identifiers(path);
    for forbidden in [
        "NativeShell",
        "DesktopProjection",
        "RuntimeCommandClient",
        "DesktopRuntimeCommandHandle",
        "DesktopRuntimeBridge",
        "CommandTracker",
        "DesktopCommandLedger",
        "PreferenceStore",
        "PreferenceWriter",
    ] {
        assert!(
            !identifiers.contains(forbidden),
            "{} leaf UI must not depend on {forbidden}",
            path.display()
        );
    }
}

#[test]
fn leaf_ui_does_not_import_root_runtime_command_or_preference_authority() {
    let root = manifest_dir().join("src/app/native_shell");
    for relative in [
        "center_drawer_host.rs",
        "composer_pane.rs",
        "conversation_header.rs",
        "conversation_pane.rs",
        "desktop_controls.rs",
        "desktop_style.rs",
        "evo_brand.rs",
        "home_pane.rs",
        "inspector_pane.rs",
        "root_modal_host.rs",
        "sessions_pane.rs",
        "skills_pane.rs",
        "streaming_text.rs",
        "toast_host.rs",
    ] {
        assert_leaf_ui_dependencies(&root.join(relative));
    }

    let ui_root = manifest_dir().join("src/ui");
    for path in rust_files(&ui_root) {
        let relative = path
            .strip_prefix(&ui_root)
            .expect("UI path is under UI root");
        if is_test_only_source(&path)
            || relative == Path::new("mod.rs")
            || relative
                .components()
                .next()
                .is_some_and(|component| component.as_os_str() == "shell")
        {
            continue;
        }
        assert_leaf_ui_dependencies(&path);
    }
}

#[test]
fn runtime_and_platform_layers_do_not_depend_on_presentation() {
    for path in layer_rust_files("runtime") {
        if is_test_only_source(&path) {
            continue;
        }
        let facts = production_facts(&path);
        assert_paths_exclude(&path, &facts, &[&["gpui"], &["ui"]]);
        for forbidden in [
            "NativeShell",
            "ConversationPane",
            "ComposerPane",
            "InspectorPane",
            "SessionsPane",
            "UiChangeSet",
            "ShellUiState",
            "ViewModel",
        ] {
            assert!(
                !facts.identifiers.contains(forbidden),
                "{} runtime module must not depend on presentation identifier {forbidden}",
                path.display()
            );
        }
        for identifier in &facts.identifiers {
            assert!(
                !identifier.ends_with("Pane") && !identifier.ends_with("ViewModel"),
                "{} runtime module must not depend on presentation identifier {identifier}",
                path.display()
            );
        }
    }

    for path in layer_rust_files("platform") {
        if is_test_only_source(&path) {
            continue;
        }
        let facts = production_facts(&path);
        assert_paths_exclude(&path, &facts, &[&["ui"]]);
    }
}
