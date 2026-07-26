//! Product-independence boundary coverage for core tools and runtime.

use std::fs;
use std::path::{Path, PathBuf};

#[test]
fn agent_core_tool_runtime_has_no_coding_agent_product_imports() {
    let crate_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repo_root = crate_root
        .parent()
        .and_then(Path::parent)
        .expect("crate should live under crates/agent-core");
    let mut violations = Vec::new();

    collect_product_imports(repo_root, &crate_root.join("src"), &mut violations);

    assert!(
        violations.is_empty(),
        "agent-core tool/runtime source must not import coding-agent product ownership:\n{}",
        violations.join("\n")
    );
}

#[test]
fn tool_invocation_context_is_generic_and_runtime_constructed() {
    let crate_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let tool = fs::read_to_string(crate_root.join("src/agent/types/tool.rs"))
        .expect("read tool contract source");
    let nodes = fs::read_to_string(crate_root.join("src/agent/turn/nodes.rs"))
        .expect("read agent turn tool execution source");

    assert!(tool.contains("pub struct ToolExecutionContext"));
    for getter in [
        "pub fn scope_id(&self)",
        "pub fn turn(&self)",
        "pub fn tool_call_id(&self)",
        "pub fn tool_name(&self)",
        "pub fn cancel_token(&self)",
    ] {
        assert!(
            tool.contains(getter),
            "missing tool context contract: {getter}"
        );
    }
    assert!(nodes.contains("ToolExecutionContext::new("));
    assert!(nodes.contains("ctx.config.tool_execution_scope.clone()"));
    assert!(nodes.contains("ctx.cancel_token.clone()"));
    assert!(!tool.contains("CodingAgent"));
}

fn collect_product_imports(repo_root: &Path, path: &Path, violations: &mut Vec<String>) {
    let Ok(metadata) = fs::metadata(path) else {
        return;
    };

    if metadata.is_dir() {
        let mut entries = fs::read_dir(path)
            .expect("read source directory")
            .collect::<Result<Vec<_>, _>>()
            .expect("read source entries");
        entries.sort_by_key(|entry| entry.path());
        for entry in entries {
            collect_product_imports(repo_root, &entry.path(), violations);
        }
        return;
    }

    if path.extension().and_then(|extension| extension.to_str()) != Some("rs") {
        return;
    }

    let relative = path
        .strip_prefix(repo_root)
        .expect("scanned file should be under repo root")
        .to_string_lossy()
        .replace('\\', "/");
    let content = fs::read_to_string(path).expect("read source file");
    for (line_index, line) in content.lines().enumerate() {
        if line.contains("coding_agent")
            || line.contains("CodingAgentSession")
            || line.contains("CodingAgentEvent")
        {
            violations.push(format!("{}:{}: {}", relative, line_index + 1, line.trim()));
        }
    }
}
