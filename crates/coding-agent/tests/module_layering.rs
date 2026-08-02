use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use proc_macro2::Span;
use syn::spanned::Spanned;
use syn::visit::{self, Visit};
use syn::{Attribute, Item, ItemUse, UseTree};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Layer {
    Kernel,
    Platform,
    Domain,
    Application,
    Api,
}

impl Layer {
    const ALL: [Self; 5] = [
        Self::Kernel,
        Self::Platform,
        Self::Domain,
        Self::Application,
        Self::Api,
    ];

    const fn index(self) -> usize {
        match self {
            Self::Kernel => 0,
            Self::Platform => 1,
            Self::Domain => 2,
            Self::Application => 3,
            Self::Api => 4,
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::Kernel => "L0 kernel",
            Self::Platform => "L1 platform",
            Self::Domain => "L2 domain",
            Self::Application => "L3 application",
            Self::Api => "L4 api",
        }
    }
}

#[derive(Debug, Clone)]
struct Dependency {
    path: String,
    line: usize,
}

fn crate_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn rust_files(root: &Path) -> Vec<PathBuf> {
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

    let mut files = Vec::new();
    collect(root, &mut files);
    files
}

fn has_test_cfg(attributes: &[Attribute]) -> bool {
    attributes.iter().any(|attribute| {
        attribute.path().is_ident("cfg")
            && attribute
                .meta
                .require_list()
                .is_ok_and(|list| list.tokens.to_string().contains("test"))
    })
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

#[derive(Default)]
struct CrateDependencyVisitor {
    dependencies: Vec<Dependency>,
}

impl CrateDependencyVisitor {
    fn record(&mut self, segments: &[String], span: Span) {
        if segments.first().is_some_and(|segment| segment == "crate") && segments.len() >= 2 {
            self.dependencies.push(Dependency {
                path: segments.join("::"),
                line: span.start().line,
            });
        }
    }

    fn visit_use_tree(&mut self, tree: &UseTree, prefix: &mut Vec<String>) {
        match tree {
            UseTree::Path(path) => {
                prefix.push(path.ident.to_string());
                self.visit_use_tree(&path.tree, prefix);
                prefix.pop();
            }
            UseTree::Name(name) => {
                prefix.push(name.ident.to_string());
                self.record(prefix, name.ident.span());
                prefix.pop();
            }
            UseTree::Rename(rename) => {
                prefix.push(rename.ident.to_string());
                self.record(prefix, rename.ident.span());
                prefix.pop();
            }
            UseTree::Glob(glob) => self.record(prefix, glob.star_token.span),
            UseTree::Group(group) => {
                for tree in &group.items {
                    self.visit_use_tree(tree, prefix);
                }
            }
        }
    }
}

impl<'ast> Visit<'ast> for CrateDependencyVisitor {
    fn visit_item(&mut self, item: &'ast Item) {
        if has_test_cfg(item_attributes(item)) {
            return;
        }
        visit::visit_item(self, item);
    }

    fn visit_item_mod(&mut self, module: &'ast syn::ItemMod) {
        if module.ident == "tests" || has_test_cfg(&module.attrs) {
            return;
        }
        visit::visit_item_mod(self, module);
    }

    fn visit_item_use(&mut self, item: &'ast ItemUse) {
        let mut prefix = Vec::new();
        self.visit_use_tree(&item.tree, &mut prefix);
    }

    fn visit_path(&mut self, path: &'ast syn::Path) {
        let segments = path
            .segments
            .iter()
            .map(|segment| segment.ident.to_string())
            .collect::<Vec<_>>();
        self.record(&segments, path.span());
        visit::visit_path(self, path);
    }
}

fn dependencies(path: &Path) -> Vec<Dependency> {
    let source = fs::read_to_string(path)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()));
    let syntax = syn::parse_file(&source)
        .unwrap_or_else(|error| panic!("failed to parse {}: {error}", path.display()));
    let mut visitor = CrateDependencyVisitor::default();
    visitor.visit_file(&syntax);
    visitor.dependencies
}

fn source_layer(relative: &Path) -> Layer {
    let first = relative
        .components()
        .next()
        .and_then(|component| component.as_os_str().to_str())
        .unwrap_or_default();
    match first {
        "kernel" | "limits.rs" => Layer::Kernel,
        "platform" | "mutex.rs" => Layer::Platform,
        "authorization.rs" | "config" | "profiles" | "theme" | "workspace.rs" => Layer::Domain,
        "app" | "lib.rs" => Layer::Api,
        // Cross-representation projections are an application integration
        // boundary even though the plan gives them a domain/ directory.
        "domain" => Layer::Application,
        "runtime" if relative.starts_with("runtime/facade") => Layer::Api,
        _ => Layer::Application,
    }
}

fn target_layer(path: &str) -> Option<Layer> {
    let mut segments = path.split("::");
    if segments.next()? != "crate" {
        return None;
    }
    Some(match segments.next()? {
        "kernel" | "limits" => Layer::Kernel,
        "platform" | "mutex" => Layer::Platform,
        "authorization" | "config" | "profiles" | "theme" | "workspace" => Layer::Domain,
        "api" => Layer::Api,
        // Several app modules currently own application input values. They are
        // deliberately classified as L3 targets until the Phase 4 composition
        // root split gives them a dedicated application namespace.
        "app" | "application" | "domain" | "events" | "operations" | "public_error"
        | "resources" | "runtime" | "services" | "session" | "test_support" | "tools" => {
            Layer::Application
        }
        _ => return None,
    })
}

fn validate_direction(source: Layer, target: Layer, path: &str) -> Result<(), String> {
    if target.index() > source.index() {
        Err(format!(
            "{} referenced {} dependency `{path}`",
            source.label(),
            target.label()
        ))
    } else {
        Ok(())
    }
}

fn find_layer_cycle(edges: &BTreeMap<Layer, BTreeSet<Layer>>) -> Option<Vec<Layer>> {
    fn visit(
        layer: Layer,
        edges: &BTreeMap<Layer, BTreeSet<Layer>>,
        visiting: &mut Vec<Layer>,
        visited: &mut BTreeSet<Layer>,
    ) -> Option<Vec<Layer>> {
        if let Some(start) = visiting.iter().position(|candidate| *candidate == layer) {
            let mut cycle = visiting[start..].to_vec();
            cycle.push(layer);
            return Some(cycle);
        }
        if !visited.insert(layer) {
            return None;
        }
        visiting.push(layer);
        for target in edges.get(&layer).into_iter().flatten().copied() {
            if target != layer
                && let Some(cycle) = visit(target, edges, visiting, visited)
            {
                return Some(cycle);
            }
        }
        visiting.pop();
        None
    }

    let mut visited = BTreeSet::new();
    for layer in Layer::ALL {
        if let Some(cycle) = visit(layer, edges, &mut Vec::new(), &mut visited) {
            return Some(cycle);
        }
    }
    None
}

#[test]
fn module_dependencies_follow_the_layer_table_and_are_acyclic() {
    let source_root = crate_root().join("src");
    let mut violations = Vec::new();
    let mut edges = BTreeMap::<Layer, BTreeSet<Layer>>::new();

    for file in rust_files(&source_root) {
        let relative = file
            .strip_prefix(&source_root)
            .expect("source path is relative");
        let source = source_layer(relative);
        for dependency in dependencies(&file) {
            let Some(target) = target_layer(&dependency.path) else {
                continue;
            };
            edges.entry(source).or_default().insert(target);
            if let Err(reason) = validate_direction(source, target, &dependency.path) {
                violations.push(format!(
                    "{}:{}: {reason}",
                    relative.display(),
                    dependency.line
                ));
            }
        }
    }

    if let Some(cycle) = find_layer_cycle(&edges) {
        violations.push(format!(
            "layer dependency cycle: {}",
            cycle
                .iter()
                .map(|layer| layer.label())
                .collect::<Vec<_>>()
                .join(" -> ")
        ));
    }

    assert!(
        violations.is_empty(),
        "coding-agent module layering violations:\n{}",
        violations.join("\n")
    );
}

#[test]
fn reverse_dependency_self_test_is_rejected_with_layer_names() {
    let error = validate_direction(
        Layer::Domain,
        Layer::Application,
        "crate::application::forbidden",
    )
    .expect_err("the synthetic reverse dependency must fail");
    assert!(error.contains("L2 domain"));
    assert!(error.contains("L3 application"));
    assert!(error.contains("crate::application::forbidden"));
}
