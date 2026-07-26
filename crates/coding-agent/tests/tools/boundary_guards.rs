//! Product tool ownership and provider-visibility boundaries.

use std::fs;
use std::path::PathBuf;

#[test]
fn runtime_service_validates_tools_before_provider_visibility() {
    let crate_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let source = fs::read_to_string(crate_root.join("src/services/runtime.rs"))
        .expect("read runtime service source");
    let start = source
        .find("fn build_agent_runtime_with_diagnostics(")
        .expect("runtime builder should exist");
    let end = source[start..]
        .find("fn apply_tool_policy(")
        .map(|offset| start + offset)
        .expect("runtime builder should be followed by apply_tool_policy");
    let body = &source[start..end];

    assert!(
        body.contains(".add_tool(tool)"),
        "RuntimeService must validate each provider-visible tool through fallible Agent::add_tool"
    );
    assert!(
        !body.contains(".try_add_tool("),
        "retired compatibility registration paths must not return"
    );
}

#[test]
fn product_output_reuses_core_shaping_without_absorbing_product_policy() {
    let crate_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let output = fs::read_to_string(crate_root.join("src/tools/output.rs"))
        .expect("read product output adapter");
    let shell =
        fs::read_to_string(crate_root.join("src/tools/shell.rs")).expect("read shell tool source");

    assert!(
        output.contains("agent_core::api::execution"),
        "product output adapter must reuse the frozen core execution facade"
    );
    assert!(
        output.contains("agent_core::api::execution::truncate_head"),
        "product head adapter must delegate truncated output shaping to core"
    );
    assert!(
        !output.contains("pub fn format_size("),
        "product tools must not restore a duplicate size formatter"
    );
    assert!(
        shell.contains("truncate_tail("),
        "shell output must use the shared core tail truncation contract"
    );
    assert!(
        shell.contains("[Output truncated:"),
        "shell's user-facing truncation marker remains product-owned"
    );
}
