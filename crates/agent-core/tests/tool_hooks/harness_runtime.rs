use agent_core::api::agent::AgentMessage;
use agent_core::api::execution::{
    ExecutionError, ExecutionEvent, ExecutionOutput, FileErrorCode, FileKind, FileSystem, Shell,
};
use agent_core::api::testing::InMemoryExecutionEnv;
use ai::api::conversation::{ContentBlock, Message};
use futures::StreamExt;

#[test]
fn custom_messages_convert_to_context_and_session_wire_shape() {
    let messages = vec![
        AgentMessage::BashExecution {
            message_id: "bash_1".into(),
            command: "cargo test".into(),
            output: "ok".into(),
            exit_code: Some(0),
            cancelled: false,
            truncated: true,
            full_output_path: Some("/tmp/full.log".into()),
            exclude_from_context: false,
            timestamp: 123,
        },
        AgentMessage::Custom {
            message_id: "custom_1".into(),
            custom_type: "note".into(),
            content: vec![ContentBlock::Text {
                text: "remember this".into(),
                text_signature: None,
            }],
            display: true,
            details: Some(serde_json::json!({"source": "test"})),
            timestamp: 124,
        },
        AgentMessage::BranchSummary {
            message_id: "branch_1".into(),
            summary: "branch result".into(),
            from_id: "entry_7".into(),
            timestamp: 125,
        },
    ];

    let ctx =
        agent_core::api::testing::convert_to_context(&None, &messages, &[], &Default::default());
    assert_eq!(ctx.messages.len(), 3);
    let text = match &ctx.messages[0] {
        Message::User { content } => match &content[0] {
            ContentBlock::Text { text, .. } => text,
            _ => panic!("expected text"),
        },
        _ => panic!("expected user message"),
    };
    assert!(text.contains("Ran `cargo test`"));
    assert!(text.contains("Output truncated"));

    let stored = agent_core::api::transcript::agent_message_to_stored(&messages[0], 999).unwrap();
    let json = serde_json::to_value(stored).unwrap();
    assert_eq!(json["role"], "bashExecution");
    assert_eq!(json["command"], "cargo test");
    assert_eq!(json["timestamp"], 123);

    let stored = agent_core::api::transcript::agent_message_to_stored(&messages[1], 999).unwrap();
    let json = serde_json::to_value(stored).unwrap();
    assert_eq!(json["role"], "custom");
    assert_eq!(json["customType"], "note");

    let stored = agent_core::api::transcript::agent_message_to_stored(&messages[2], 999).unwrap();
    let json = serde_json::to_value(stored).unwrap();
    assert_eq!(json["role"], "branchSummary");
    assert_eq!(json["fromId"], "entry_7");
}

#[tokio::test]
async fn in_memory_execution_env_supports_file_and_shell_traits() {
    let env = InMemoryExecutionEnv::new("/workspace");
    env.write_file("/workspace/src/main.rs", b"fn main() {}\n")
        .await
        .unwrap();
    env.append_file("/workspace/src/main.rs", b"// done\n")
        .await
        .unwrap();

    assert!(env.exists("/workspace/src/main.rs").await.unwrap());
    assert_eq!(
        env.read_text_file("/workspace/src/main.rs").await.unwrap(),
        "fn main() {}\n// done\n"
    );
    assert_eq!(
        env.read_text_lines("/workspace/src/main.rs", Some(1))
            .await
            .unwrap(),
        vec!["fn main() {}".to_string()]
    );

    let entries = env.list_dir("/workspace/src").await.unwrap();
    assert_eq!(entries[0].name, "main.rs");
    assert_eq!(entries[0].kind, FileKind::File);

    env.set_command(
        "cargo test",
        ExecutionOutput {
            stdout: "ok".into(),
            stderr: String::new(),
            exit_code: 0,
        },
    );
    let events = env
        .exec_stream("cargo test", None)
        .collect::<Vec<_>>()
        .await;
    assert!(matches!(
        events.as_slice(),
        [Ok(ExecutionEvent::Stdout(stdout)), Ok(ExecutionEvent::Exit(0))]
            if stdout == "ok"
    ));

    let err = env.read_text_file("/workspace/missing").await.unwrap_err();
    assert_eq!(err.code(), FileErrorCode::NotFound);
    let events = env.exec_stream("missing", None).collect::<Vec<_>>().await;
    assert!(matches!(
        events.as_slice(),
        [Err(ExecutionError::ShellUnavailable { .. })]
    ));
}
