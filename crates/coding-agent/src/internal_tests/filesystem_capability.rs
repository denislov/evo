//! Security regression tests for descriptor-bound workspace filesystem tools.

use agent_core::api::tool::{AgentTool, ToolExecutionContext};
use ai::api::conversation::ContentBlock;
use tokio_util::sync::CancellationToken;

use crate::runtime::capability::FilesystemCapability;
use crate::tools::filesystem::{edit, find, grep, ls, read, write};

fn output_text(blocks: &[ContentBlock]) -> String {
    blocks
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

async fn execute_bound(
    capability: FilesystemCapability,
    tool: AgentTool,
    operation_id: &str,
    tool_call_id: &str,
    arguments: serde_json::Value,
) -> Result<String, String> {
    let output = (tool.execute)(
        ToolExecutionContext::new(
            Some(operation_id),
            1,
            tool_call_id,
            tool.name,
            CancellationToken::new(),
        ),
        arguments,
        None,
    )
    .await?;
    drop(capability);
    Ok(output_text(&output.content))
}

#[cfg(unix)]
#[tokio::test]
async fn all_six_filesystem_tools_reject_external_symlink_authority() {
    use std::os::unix::fs::symlink;

    let workspace = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("secret.txt"), "external-unique-secret").unwrap();
    symlink(
        outside.path().join("secret.txt"),
        workspace.path().join("linked-file"),
    )
    .unwrap();
    symlink(outside.path(), workspace.path().join("linked-dir")).unwrap();

    assert!(
        read::read_execute(workspace.path(), serde_json::json!({"path": "linked-file"}),)
            .await
            .unwrap_err()
            .contains("cannot open")
    );
    assert!(
        edit::edit_execute(
            workspace.path(),
            serde_json::json!({
                "path": "linked-file",
                "edits": [{"oldText": "external", "newText": "mutated"}],
            }),
        )
        .await
        .unwrap_err()
        .contains("cannot open")
    );
    assert!(
        write::write_execute(
            workspace.path(),
            serde_json::json!({"path": "linked-file", "content": "mutated"}),
        )
        .await
        .unwrap_err()
        .contains("cannot be opened safely")
    );
    assert!(
        grep::grep_execute(
            workspace.path(),
            serde_json::json!({"path": "linked-dir", "pattern": "external-unique-secret"}),
        )
        .await
        .unwrap_err()
        .contains("cannot open")
    );
    assert!(
        find::find_execute(
            workspace.path(),
            serde_json::json!({"path": "linked-dir", "pattern": "*.txt"}),
        )
        .await
        .unwrap_err()
        .contains("cannot open directory")
    );
    assert!(
        ls::ls_execute(workspace.path(), serde_json::json!({"path": "linked-dir"}),)
            .await
            .unwrap_err()
            .contains("cannot open directory")
    );
    assert_eq!(
        std::fs::read_to_string(outside.path().join("secret.txt")).unwrap(),
        "external-unique-secret"
    );
}

#[cfg(windows)]
#[tokio::test]
async fn all_six_filesystem_tools_reject_external_ntfs_junction_authority() {
    let workspace = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("secret.txt"), "external-unique-secret").unwrap();
    junction::create(outside.path(), workspace.path().join("linked-dir")).unwrap();

    assert!(
        read::read_execute(
            workspace.path(),
            serde_json::json!({"path": "linked-dir/secret.txt"}),
        )
        .await
        .is_err()
    );
    assert!(
        edit::edit_execute(
            workspace.path(),
            serde_json::json!({
                "path": "linked-dir/secret.txt",
                "edits": [{"oldText": "external", "newText": "mutated"}],
            }),
        )
        .await
        .is_err()
    );
    assert!(
        write::write_execute(
            workspace.path(),
            serde_json::json!({"path": "linked-dir/created.txt", "content": "mutated"}),
        )
        .await
        .is_err()
    );
    assert!(
        grep::grep_execute(
            workspace.path(),
            serde_json::json!({"path": "linked-dir", "pattern": "external-unique-secret"}),
        )
        .await
        .is_err()
    );
    assert!(
        find::find_execute(
            workspace.path(),
            serde_json::json!({"path": "linked-dir", "pattern": "*.txt"}),
        )
        .await
        .is_err()
    );
    assert!(
        ls::ls_execute(workspace.path(), serde_json::json!({"path": "linked-dir"}),)
            .await
            .is_err()
    );
    assert_eq!(
        std::fs::read_to_string(outside.path().join("secret.txt")).unwrap(),
        "external-unique-secret"
    );
    assert!(!outside.path().join("created.txt").exists());
}

#[cfg(unix)]
#[tokio::test]
async fn recursive_walks_do_not_follow_external_or_cyclic_links() {
    use std::os::unix::fs::symlink;

    let workspace = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("secret.txt"), "external-unique-secret").unwrap();
    symlink(outside.path(), workspace.path().join("external-link")).unwrap();
    symlink(".", workspace.path().join("cycle")).unwrap();

    let grep_output = grep::grep_execute(
        workspace.path(),
        serde_json::json!({"pattern": "external-unique-secret"}),
    )
    .await
    .unwrap();
    assert_eq!(output_text(&grep_output), "No matches found");

    let find_output = find::find_execute(workspace.path(), serde_json::json!({"pattern": "*.txt"}))
        .await
        .unwrap();
    assert_eq!(output_text(&find_output), "No files found matching pattern");
}

#[cfg(unix)]
#[tokio::test]
async fn write_cannot_create_through_external_linked_parent_or_dangling_leaf() {
    use std::os::unix::fs::symlink;

    let workspace = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    symlink(outside.path(), workspace.path().join("linked-parent")).unwrap();
    symlink(
        outside.path().join("missing-target"),
        workspace.path().join("dangling"),
    )
    .unwrap();

    assert!(
        write::write_execute(
            workspace.path(),
            serde_json::json!({"path": "linked-parent/created.txt", "content": "no"}),
        )
        .await
        .is_err()
    );
    assert!(
        write::write_execute(
            workspace.path(),
            serde_json::json!({"path": "dangling", "content": "no"}),
        )
        .await
        .is_err()
    );
    assert!(!outside.path().join("created.txt").exists());
    assert!(!outside.path().join("missing-target").exists());
}

#[tokio::test]
async fn bound_directory_tools_keep_the_authorized_directory_after_swap() {
    for tool_name in ["ls", "find", "grep"] {
        let workspace = tempfile::tempdir().unwrap();
        std::fs::create_dir(workspace.path().join("target")).unwrap();
        std::fs::write(
            workspace.path().join("target/authorized.txt"),
            "authorized-marker",
        )
        .unwrap();
        let capability = FilesystemCapability::new(workspace.path().to_path_buf()).unwrap();
        capability
            .bind_tool_target("op", tool_name, tool_name, "target")
            .await
            .unwrap();
        std::fs::rename(
            workspace.path().join("target"),
            workspace.path().join("authorized-dir"),
        )
        .unwrap();
        std::fs::create_dir(workspace.path().join("target")).unwrap();
        std::fs::write(
            workspace.path().join("target/replacement.txt"),
            "replacement-marker",
        )
        .unwrap();

        let (tool, arguments) = match tool_name {
            "ls" => (
                ls::ls_tool(capability.clone()),
                serde_json::json!({"path": "target"}),
            ),
            "find" => (
                find::find_tool(capability.clone()),
                serde_json::json!({"path": "target", "pattern": "*.txt"}),
            ),
            "grep" => (
                grep::grep_tool(capability.clone()),
                serde_json::json!({"path": "target", "pattern": "authorized-marker"}),
            ),
            _ => unreachable!(),
        };
        let output = execute_bound(capability, tool, "op", tool_name, arguments)
            .await
            .unwrap();
        assert!(output.contains("authorized"), "{tool_name}: {output}");
        assert!(!output.contains("replacement"), "{tool_name}: {output}");
    }
}

#[tokio::test]
async fn self_healing_edit_reads_and_writes_the_authorization_bound_file() {
    let workspace = tempfile::tempdir().unwrap();
    std::fs::write(workspace.path().join("target.txt"), "authorized-marker\n").unwrap();
    let capability = FilesystemCapability::new(workspace.path().to_path_buf()).unwrap();
    capability
        .bind_tool_target("op", "edit-call", "edit", "target.txt")
        .await
        .unwrap();
    std::fs::rename(
        workspace.path().join("target.txt"),
        workspace.path().join("authorized.txt"),
    )
    .unwrap();
    std::fs::write(workspace.path().join("target.txt"), "replacement-marker\n").unwrap();
    let tool = edit::edit_tool(capability.clone());

    let output = execute_bound(
        capability,
        tool,
        "op",
        "edit-call",
        serde_json::json!({
            "path": "target.txt",
            "edits": [{"oldText": "authorized", "newText": "edited"}],
        }),
    )
    .await
    .unwrap();

    assert!(output.contains("Successfully replaced 1 block"));
    assert_eq!(
        std::fs::read_to_string(workspace.path().join("authorized.txt")).unwrap(),
        "edited-marker\n"
    );
    assert_eq!(
        std::fs::read_to_string(workspace.path().join("target.txt")).unwrap(),
        "replacement-marker\n"
    );
}
