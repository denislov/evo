use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
};

use syn::{
    File, Ident, Item, Visibility,
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

fn external_modules(file: &File) -> BTreeSet<String> {
    file.items
        .iter()
        .filter_map(|item| match item {
            Item::Mod(module) if module.content.is_none() => Some(module.ident.to_string()),
            _ => None,
        })
        .collect()
}

#[derive(Default)]
struct IdentifierCollector {
    identifiers: BTreeSet<String>,
}

impl<'ast> Visit<'ast> for IdentifierCollector {
    fn visit_ident(&mut self, ident: &'ast Ident) {
        self.identifiers.insert(ident.to_string());
        visit::visit_ident(self, ident);
    }

    fn visit_item_mod(&mut self, module: &'ast syn::ItemMod) {
        if module.ident == "tests" {
            return;
        }
        visit::visit_item_mod(self, module);
    }
}

fn rust_identifiers(path: impl AsRef<Path>) -> BTreeSet<String> {
    let file = parse_rust(path);
    let mut collector = IdentifierCollector::default();
    collector.visit_file(&file);
    collector.identifiers
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
fn native_shell_has_one_explicit_child_module_graph() {
    let native_root = manifest_dir().join("src/app/native_shell");
    let shell = parse_rust(manifest_dir().join("src/app/native_shell.rs"));
    let modules = external_modules(&shell);
    assert_eq!(
        modules,
        BTreeSet::from([
            "center_drawer_host".into(),
            "center_navigation".into(),
            "commands".into(),
            "composer_pane".into(),
            "conversation_controller".into(),
            "conversation_header".into(),
            "conversation_pane".into(),
            "desktop_controls".into(),
            "desktop_style".into(),
            "evo_brand".into(),
            "home_pane".into(),
            "inspector_pane".into(),
            "project_catalog_controller".into(),
            "root_modal_host".into(),
            "sessions_pane".into(),
            "skills_pane".into(),
            "streaming_text".into(),
            "toast_host".into(),
            "update".into(),
        ])
    );
    for module in modules {
        assert!(native_root.join(format!("{module}.rs")).is_file());
    }

    for removed in [
        "home_recent.rs",
        "home_skills.rs",
        "overlay_host.rs",
        "context_overlay.rs",
        "narrow_context.rs",
        "session_refresh_timer.rs",
        "thinking_menu.rs",
    ] {
        assert!(
            !native_root.join(removed).exists(),
            "legacy module must stay deleted: {removed}"
        );
    }
}

#[test]
fn child_views_do_not_import_root_or_product_authority() {
    let root = manifest_dir().join("src/app/native_shell");
    let policies: &[(&str, &[&str])] = &[
        (
            "desktop_controls.rs",
            &[
                "NativeShell",
                "DesktopProjection",
                "DesktopCommandLedger",
                "ConversationController",
            ],
        ),
        (
            "conversation_header.rs",
            &["NativeShell", "DesktopProjection", "DesktopCommandLedger"],
        ),
        (
            "sessions_pane.rs",
            &["NativeShell", "DesktopProjection", "DesktopCommandLedger"],
        ),
        (
            "composer_pane.rs",
            &["NativeShell", "DesktopProjection", "DesktopCommandLedger"],
        ),
        (
            "inspector_pane.rs",
            &["NativeShell", "DesktopProjection", "DesktopCommandLedger"],
        ),
        (
            "toast_host.rs",
            &["NativeShell", "DesktopProjection", "DesktopCommandLedger"],
        ),
        (
            "root_modal_host.rs",
            &["NativeShell", "DesktopProjection", "DesktopCommandLedger"],
        ),
        (
            "center_drawer_host.rs",
            &["NativeShell", "DesktopProjection", "DesktopCommandLedger"],
        ),
        (
            "evo_brand.rs",
            &["NativeShell", "DesktopProjection", "DesktopCommandLedger"],
        ),
    ];

    for (relative, forbidden) in policies {
        let identifiers = rust_identifiers(root.join(relative));
        for identifier in *forbidden {
            assert!(
                !identifiers.contains(*identifier),
                "{relative} must not depend on {identifier}"
            );
        }
    }
}

#[test]
fn runtime_modules_do_not_depend_on_native_presentation() {
    let runtime_root = manifest_dir().join("src/runtime");
    for relative in ["bridge.rs", "dispatch.rs", "driver.rs", "protocol.rs"] {
        let identifiers = rust_identifiers(runtime_root.join(relative));
        for forbidden in [
            "gpui",
            "NativeShell",
            "ConversationPane",
            "ComposerPane",
            "InspectorPane",
            "SessionsPane",
        ] {
            assert!(
                !identifiers.contains(forbidden),
                "runtime/{relative} must not depend on presentation identifier {forbidden}"
            );
        }
    }
}
