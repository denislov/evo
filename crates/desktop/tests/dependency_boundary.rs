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
fn desktop_public_api_is_one_typed_application_surface() {
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
fn native_replay_authority_is_feature_gated_under_devtools() {
    let app = manifest_dir().join("src/app");
    assert!(
        !app.join("native_perf.rs").exists(),
        "native replay must not return to the app root"
    );
    assert!(app.join("devtools/mod.rs").is_file());
    assert!(app.join("devtools/native_replay.rs").is_file());

    let desktop_manifest = manifest();
    let features = desktop_manifest
        .get("features")
        .and_then(toml::Value::as_table)
        .expect("desktop manifest must declare features");
    assert!(features.contains_key("desktop-devtools"));

    let app_source = fs::read_to_string(app.join("../app.rs")).expect("app source is readable");
    assert!(app_source.contains("#[cfg(feature = \"desktop-devtools\")]\nmod devtools;"));
    assert!(app_source.contains(
        "#[cfg(feature = \"desktop-devtools\")]\n            if devtools::open_requested(cx)"
    ));

    let shell = fs::read_to_string(app.join("native_shell.rs")).expect("shell source is readable");
    for fixture in [
        "NativeVisualCatalogFixture",
        "NativeVisualDrawerFixture",
        "install_native_visual_catalog_fixture",
        "install_native_visual_drawer_fixture",
        "install_native_visual_home_project_fixture",
        "install_native_visual_non_reasoning_fixture",
    ] {
        let offset = shell
            .find(fixture)
            .unwrap_or_else(|| panic!("native replay fixture is missing: {fixture}"));
        let prefix = &shell[offset.saturating_sub(180)..offset];
        assert!(
            prefix.contains("#[cfg(feature = \"desktop-devtools\")]"),
            "native replay fixture must be feature gated: {fixture}"
        );
    }

    let scripts = workspace_root().join("scripts");
    for script in [
        "desktop-native-perf-gate.sh",
        "desktop-native-perf-gate.ps1",
        "desktop-click-to-photon.sh",
        "desktop-click-to-photon.ps1",
        "desktop-brand-visual-fixtures.sh",
        "desktop-visual-golden.sh",
    ] {
        let source = fs::read_to_string(scripts.join(script))
            .unwrap_or_else(|error| panic!("failed to read {script}: {error}"));
        assert!(
            source.contains("--features desktop-devtools"),
            "native replay script must enable desktop-devtools: {script}"
        );
    }

    let test_paths = [
        "ui::conversation::model::tests::desktop_release_empty_conversation_baseline",
        "ui::conversation::model::tests::desktop_release_ten_mib_interaction_baseline",
        "ui::conversation::model::tests::desktop_release_scale_content_and_streaming_matrix",
        "app::native_shell::tests::performance::desktop_release_gpui_headless_frame_and_input_replay",
        "app::native_shell::tests::performance::desktop_release_gpui_markdown_parser_matrix",
    ];
    for script in ["desktop-perf-gate.sh", "desktop-perf-gate.ps1"] {
        let source = fs::read_to_string(scripts.join(script))
            .unwrap_or_else(|error| panic!("failed to read {script}: {error}"));
        for test_path in test_paths {
            assert!(
                source.contains(test_path),
                "performance script must use the current test path {test_path}: {script}"
            );
        }
    }
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
                &["platform"],
                &["std", "fs"],
                &["std", "process"],
                &["std", "thread"],
                &["tokio"],
            ],
        );
        for forbidden in [
            "PreferenceLoad",
            "PreferenceRecovery",
            "PreferenceStore",
            "PreferenceStoreError",
            "PreferenceWriter",
            "ScratchWorkspaceError",
        ] {
            assert!(
                !facts.identifiers.contains(forbidden),
                "{} application module must not depend on platform preference authority {forbidden}",
                path.display()
            );
        }
    }
}

#[test]
fn preference_model_has_no_storage_workspace_or_thread_authority() {
    for path in layer_rust_files("preferences") {
        if is_test_only_source(&path) {
            continue;
        }
        let facts = production_facts(&path);
        assert_paths_exclude(
            &path,
            &facts,
            &[
                &["platform"],
                &["std", "fs"],
                &["std", "io"],
                &["std", "thread"],
                &["futures"],
            ],
        );
        for forbidden in [
            "PreferenceLoad",
            "PreferenceRecovery",
            "PreferenceStore",
            "PreferenceWriter",
            "ScratchWorkspaceError",
        ] {
            assert!(
                !facts.identifiers.contains(forbidden),
                "{} preference model must not own platform authority {forbidden}",
                path.display()
            );
        }
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
    let ui_root = manifest_dir().join("src/ui");
    for path in rust_files(&ui_root) {
        let relative = path
            .strip_prefix(&ui_root)
            .expect("UI path is under UI root");
        if is_test_only_source(&path)
            || relative == Path::new("mod.rs")
            || matches!(
                relative,
                path if path == Path::new("conversation/adapter.rs")
                    || path == Path::new("conversation/controller.rs")
                    || path == Path::new("conversation/layout_adapter.rs")
                    || path == Path::new("sessions/catalog_adapter.rs")
            )
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
fn presentation_features_have_single_authority_paths() {
    let source = manifest_dir().join("src");
    for removed in [
        source.join("conversation"),
        source.join("shell.rs"),
        source.join("app/native_shell/center_navigation.rs"),
        source.join("app/native_shell/composer_pane.rs"),
        source.join("app/native_shell/conversation_controller.rs"),
        source.join("app/native_shell/conversation_header.rs"),
        source.join("app/native_shell/conversation_pane.rs"),
        source.join("app/native_shell/desktop_controls.rs"),
        source.join("app/native_shell/desktop_style.rs"),
        source.join("app/native_shell/evo_brand.rs"),
        source.join("app/native_shell/home_pane.rs"),
        source.join("app/native_shell/inspector_pane.rs"),
        source.join("app/native_shell/project_catalog_controller.rs"),
        source.join("app/native_shell/sessions_pane.rs"),
        source.join("app/native_shell/skills_pane.rs"),
        source.join("app/native_shell/streaming_text.rs"),
    ] {
        assert!(
            !removed.exists(),
            "legacy presentation path must stay deleted: {}",
            removed.display()
        );
    }

    for current in [
        source.join("ui/components/controls.rs"),
        source.join("ui/components/streaming_text.rs"),
        source.join("ui/conversation/controller.rs"),
        source.join("ui/conversation/pane.rs"),
        source.join("ui/home.rs"),
        source.join("ui/inspector/pane.rs"),
        source.join("ui/inspector/review.rs"),
        source.join("ui/sessions/catalog_adapter.rs"),
        source.join("ui/sessions/pane.rs"),
        source.join("ui/shell/layout.rs"),
        source.join("ui/shell/state.rs"),
        source.join("ui/skills.rs"),
    ] {
        assert!(
            current.is_file(),
            "presentation authority path is missing: {}",
            current.display()
        );
    }

    for pure in [
        "composer.rs",
        "copy.rs",
        "layout.rs",
        "markdown.rs",
        "model.rs",
        "render_cache.rs",
        "viewport.rs",
    ] {
        let path = source.join("ui/conversation").join(pure);
        let facts = production_facts(&path);
        assert_paths_exclude(&path, &facts, &[&["gpui"]]);
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

#[test]
fn file_review_presentation_and_external_editor_platform_stay_isolated() {
    let root = manifest_dir().join("src");
    assert!(
        !root.join("file_review.rs").exists(),
        "the former mixed review/process module must stay deleted"
    );

    let review = root.join("ui/inspector/review.rs");
    let review_facts = production_facts(&review);
    assert_paths_exclude(
        &review,
        &review_facts,
        &[
            &["gpui"],
            &["platform"],
            &["std", "process"],
            &["std", "thread"],
            &["std", "fs"],
        ],
    );

    let editor = root.join("platform/external_editor.rs");
    let editor_facts = production_facts(&editor);
    assert_paths_exclude(
        &editor,
        &editor_facts,
        &[&["gpui"], &["ui"], &["runtime"], &["coding_agent"]],
    );

    for path in layer_rust_files("runtime") {
        if is_test_only_source(&path) {
            continue;
        }
        let facts = production_facts(&path);
        for forbidden in [
            "ExternalEditorPreference",
            "ExternalEditorConfig",
            "ExternalEditorConfigError",
            "ExternalEditorLaunchError",
        ] {
            assert!(
                !facts.identifiers.contains(forbidden),
                "{} runtime module must not own external-editor platform type {forbidden}",
                path.display()
            );
        }
    }
}

#[test]
fn runtime_contract_client_and_worker_paths_have_single_authorities() {
    let runtime = manifest_dir().join("src/runtime");
    let source_root = manifest_dir().join("src");
    for removed in [
        source_root.join("runtime.rs"),
        runtime.join("bridge.rs"),
        runtime.join("driver.rs"),
        runtime.join("dispatch.rs"),
    ] {
        assert!(
            !removed.exists(),
            "legacy runtime path must stay deleted: {}",
            removed.display()
        );
    }
    for current in [
        runtime.join("mod.rs"),
        runtime.join("client.rs"),
        runtime.join("protocol.rs"),
        runtime.join("worker/mod.rs"),
        runtime.join("worker/dispatch.rs"),
    ] {
        assert!(
            current.is_file(),
            "runtime authority path is missing: {}",
            current.display()
        );
    }

    let protocol = runtime.join("protocol.rs");
    let protocol_facts = production_facts(&protocol);
    assert_paths_exclude(
        &protocol,
        &protocol_facts,
        &[
            &["gpui"],
            &["ui"],
            &["platform"],
            &["std", "process"],
            &["std", "thread"],
            &["std", "fs"],
        ],
    );
    for forbidden in [
        "DesktopRuntimeBridge",
        "DesktopRuntimeBootstrap",
        "RuntimeCommandClient",
        "RuntimeState",
        "ActivePrompt",
        "CodingAgentSession",
    ] {
        assert!(
            !protocol_facts.identifiers.contains(forbidden),
            "protocol must not own client/worker authority {forbidden}"
        );
    }

    let client = runtime.join("client.rs");
    let client_facts = production_facts(&client);
    for forbidden in ["RuntimeState", "ActivePrompt", "CodingAgentSession"] {
        assert!(
            !client_facts.identifiers.contains(forbidden),
            "runtime client must not own worker authority {forbidden}"
        );
    }

    for worker in [
        runtime.join("worker/mod.rs"),
        runtime.join("worker/dispatch.rs"),
    ] {
        let facts = production_facts(&worker);
        for forbidden in [
            "DesktopRuntimeBridge",
            "DesktopRuntimeBootstrap",
            "DesktopRuntimeEventStream",
            "DesktopRuntimeShutdownGuard",
            "RuntimeCommandClient",
        ] {
            assert!(
                !facts.identifiers.contains(forbidden),
                "{} worker must not own client authority {forbidden}",
                worker.display()
            );
        }
    }
}
