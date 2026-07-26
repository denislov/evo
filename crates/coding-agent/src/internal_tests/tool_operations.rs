//! Internal owner tests for injected tool operations.

use ai::api::conversation::ContentBlock;
use coding_agent::tools::FilesystemTarget;
use coding_agent::tools::ShellCapability;
use coding_agent::tools::filesystem::edit::{
    EditOperations, OpenedEditFile, edit_execute_with_operations,
};
use coding_agent::tools::filesystem::read::{ReadOperations, read_execute_with_operations};
use coding_agent::tools::filesystem::write::{WriteOperations, write_execute_with_operations};
use coding_agent::tools::shell::{BashOperations, BashOptions};
use coding_agent::tools::shell::{bash_execute_with_operations, bash_tool_with_operations};
use futures::future::{BoxFuture, FutureExt};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tempfile::tempdir;

type RecordedWrites = Arc<Mutex<Vec<(PathBuf, Vec<u8>)>>>;

fn text(blocks: &[ContentBlock]) -> String {
    blocks
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[derive(Default)]
struct FakeReadOps {
    paths: Mutex<Vec<PathBuf>>,
}

impl ReadOperations for FakeReadOps {
    fn read_file<'a>(
        &'a self,
        target: &'a FilesystemTarget,
    ) -> BoxFuture<'a, Result<Vec<u8>, String>> {
        async move {
            self.paths
                .lock()
                .unwrap()
                .push(target.display_path().to_path_buf());
            Ok(b"from fake read ops\n".to_vec())
        }
        .boxed()
    }
}

#[tokio::test]
async fn read_can_use_injected_operations() {
    let cwd = tempdir().unwrap();
    let ops = Arc::new(FakeReadOps::default());

    let blocks = read_execute_with_operations(
        cwd.path(),
        serde_json::json!({"path": "file.txt"}),
        ops.clone(),
    )
    .await
    .unwrap();

    assert!(text(&blocks).contains("from fake read ops"));
    assert_eq!(ops.paths.lock().unwrap()[0], cwd.path().join("file.txt"));
}

#[derive(Default)]
struct FakeWriteOps {
    writes: Mutex<Vec<(PathBuf, Vec<u8>)>>,
}

impl WriteOperations for FakeWriteOps {
    fn write_file<'a>(
        &'a self,
        target: &'a FilesystemTarget,
        content: &'a [u8],
    ) -> BoxFuture<'a, Result<(), String>> {
        async move {
            self.writes
                .lock()
                .unwrap()
                .push((target.display_path().to_path_buf(), content.to_vec()));
            Ok(())
        }
        .boxed()
    }
}

#[tokio::test]
async fn write_can_use_injected_operations() {
    let cwd = tempdir().unwrap();
    let ops = Arc::new(FakeWriteOps::default());

    let blocks = write_execute_with_operations(
        cwd.path(),
        serde_json::json!({"path": "out.txt", "content": "fake content"}),
        ops.clone(),
    )
    .await
    .unwrap();

    assert!(text(&blocks).contains("Successfully wrote 12 bytes to out.txt"));
    let writes = ops.writes.lock().unwrap();
    assert_eq!(writes.len(), 1);
    assert_eq!(writes[0].0, cwd.path().join("out.txt"));
    assert_eq!(writes[0].1, b"fake content");
    assert!(!cwd.path().join("out.txt").exists());
}

#[derive(Default)]
struct FakeEditOps {
    content: Arc<Mutex<Vec<u8>>>,
    writes: RecordedWrites,
}

impl FakeEditOps {
    fn with_content(content: &str) -> Self {
        Self {
            content: Arc::new(Mutex::new(content.as_bytes().to_vec())),
            writes: Arc::new(Mutex::new(Vec::new())),
        }
    }
}

impl EditOperations for FakeEditOps {
    fn open_file<'a>(
        &'a self,
        target: &'a FilesystemTarget,
    ) -> BoxFuture<'a, Result<Box<dyn OpenedEditFile>, String>> {
        let opened = FakeOpenedEditFile {
            path: target.display_path().to_path_buf(),
            content: self.content.clone(),
            writes: self.writes.clone(),
        };
        async move { Ok(Box::new(opened) as Box<dyn OpenedEditFile>) }.boxed()
    }
}

struct FakeOpenedEditFile {
    path: PathBuf,
    content: Arc<Mutex<Vec<u8>>>,
    writes: RecordedWrites,
}

impl OpenedEditFile for FakeOpenedEditFile {
    fn read_file<'a>(&'a self) -> BoxFuture<'a, Result<Vec<u8>, String>> {
        async move { Ok(self.content.lock().unwrap().clone()) }.boxed()
    }

    fn write_file<'a>(&'a self, content: &'a [u8]) -> BoxFuture<'a, Result<(), String>> {
        async move {
            self.writes
                .lock()
                .unwrap()
                .push((self.path.clone(), content.to_vec()));
            *self.content.lock().unwrap() = content.to_vec();
            Ok(())
        }
        .boxed()
    }
}

#[tokio::test]
async fn edit_can_use_injected_operations() {
    let cwd = tempdir().unwrap();
    let ops = Arc::new(FakeEditOps::with_content("one\ntwo\n"));

    let output = edit_execute_with_operations(
        cwd.path(),
        serde_json::json!({
            "path": "edit.txt",
            "edits": [{"oldText": "two", "newText": "deux"}]
        }),
        ops.clone(),
    )
    .await
    .unwrap();

    assert!(text(&output.content).contains("Successfully replaced 1 block"));
    let writes = ops.writes.lock().unwrap();
    assert_eq!(writes.len(), 1);
    assert_eq!(writes[0].0, cwd.path().join("edit.txt"));
    assert_eq!(writes[0].1, b"one\ndeux\n");
    let details = output.details.expect("edit output should include details");
    assert_eq!(details["firstChangedLine"], serde_json::json!(2));
}

#[derive(Default)]
struct FakeBashOps {
    calls: Mutex<Vec<(PathBuf, String)>>,
}

impl BashOperations for FakeBashOps {
    fn execute<'a>(
        &'a self,
        cwd: &'a Path,
        args: serde_json::Value,
        _options: &'a BashOptions,
        on_update: Option<agent_core::api::tool::ToolUpdateCallback>,
    ) -> BoxFuture<'a, Result<Vec<ContentBlock>, String>> {
        async move {
            let command = args
                .get("command")
                .and_then(|value| value.as_str())
                .unwrap_or_default()
                .to_string();
            self.calls
                .lock()
                .unwrap()
                .push((cwd.to_path_buf(), command));
            if let Some(on_update) = on_update {
                on_update(agent_core::api::tool::AgentToolOutput::new(vec![
                    ContentBlock::Text {
                        text: "fake update".into(),
                        text_signature: None,
                    },
                ]));
            }
            Ok(vec![ContentBlock::Text {
                text: "fake final".into(),
                text_signature: None,
            }])
        }
        .boxed()
    }
}

#[tokio::test]
async fn bash_can_use_injected_operations_and_stream_updates() {
    let cwd = tempdir().unwrap();
    let ops = Arc::new(FakeBashOps::default());
    let updates = Arc::new(Mutex::new(Vec::new()));
    let update_sink = {
        let updates = updates.clone();
        Arc::new(move |output: agent_core::api::tool::AgentToolOutput| {
            updates.lock().unwrap().push(text(&output.content));
        })
    };

    let blocks = bash_execute_with_operations(
        cwd.path(),
        serde_json::json!({"command": "echo real shell should not run"}),
        &BashOptions::default(),
        Some(update_sink),
        ops.clone(),
    )
    .await
    .unwrap();

    assert_eq!(text(&blocks), "fake final");
    assert_eq!(updates.lock().unwrap().as_slice(), ["fake update"]);
    assert_eq!(
        ops.calls.lock().unwrap()[0],
        (
            cwd.path().to_path_buf(),
            "echo real shell should not run".to_string()
        )
    );
}

#[tokio::test]
async fn bash_tool_accepts_injected_operations() {
    let cwd = tempdir().unwrap();
    let ops = Arc::new(FakeBashOps::default());
    let tool =
        bash_tool_with_operations(ShellCapability::new(cwd.path().to_path_buf()), ops.clone());

    let output = (tool.execute)(
        agent_core::api::tool::ToolExecutionContext::standalone(tool.name.clone()),
        serde_json::json!({"command": "from tool"}),
        None,
    )
    .await
    .unwrap();

    assert_eq!(text(&output.content), "fake final");
    assert_eq!(ops.calls.lock().unwrap()[0].1, "from tool");
}
