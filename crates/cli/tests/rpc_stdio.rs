use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};

const RPC_JSONL_FRAME_BYTES: usize = 4 * 1024 * 1024;

#[test]
fn rpc_stdio_recovers_from_invalid_input_and_negotiates_before_state() {
    let runtime_dir = tempfile::tempdir().expect("create isolated evo runtime directory");
    let project_dir = tempfile::tempdir().expect("create isolated project directory");
    let mut child = Command::new(env!("CARGO_BIN_EXE_coding-agent"))
        .args(["--mode", "rpc"])
        .current_dir(project_dir.path())
        .env("EVO_DIR", runtime_dir.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start cli RPC process");

    {
        let stdin = child.stdin.as_mut().expect("RPC stdin should be piped");
        stdin
            .write_all(&vec![b'x'; RPC_JSONL_FRAME_BYTES + 1])
            .expect("write oversized frame");
        stdin.write_all(b"\n{\n").expect("write malformed JSON");
        stdin
            .write_all(b"{\"id\":\"first\",\"id\":\"second\",\"type\":\"get_state\"}\n")
            .expect("write duplicate-key JSON");
        stdin
            .write_all(b"{\"id\":\"before\",\"type\":\"get_state\"}\n")
            .expect("write pre-negotiation command");
        stdin
            .write_all(
                b"{\"id\":\"bad-version\",\"type\":\"hello\",\"protocol\":{\"family\":\"rpc\",\"major\":1,\"minor\":0}}\n",
            )
            .expect("write incompatible hello");
        let oversized_id = serde_json::json!({
            "id": "i".repeat(129),
            "type": "get_state",
        });
        writeln!(stdin, "{oversized_id}").expect("write oversized identifier");
        let too_many_images = serde_json::json!({
            "id": "images",
            "type": "prompt",
            "message": "bounded",
            "images": vec![serde_json::json!({}); 17],
        });
        writeln!(stdin, "{too_many_images}").expect("write oversized image collection");
        let mut too_deep = serde_json::json!({
            "id": "deep",
            "type": "get_state",
        });
        for _ in 0..65 {
            too_deep = serde_json::json!({"nested": too_deep});
        }
        writeln!(stdin, "{too_deep}").expect("write deeply nested request");
        stdin
            .write_all(
                b"{\"id\":\"h1\",\"type\":\"hello\",\"protocol\":{\"family\":\"rpc\",\"major\":3,\"minor\":0}}\n",
            )
            .expect("write compatible hello");
        stdin
            .write_all(b"{\"id\":\"state\",\"type\":\"get_state\"}\n")
            .expect("write negotiated state command");
        stdin
            .write_all(b"{\"id\":\"unknown\",\"type\":\"unknown_command\"}\n")
            .expect("write unsupported command");
        stdin
            .write_all(
                b"{\"id\":\"hello-again\",\"type\":\"hello\",\"protocol\":{\"family\":\"rpc\",\"major\":3,\"minor\":0}}\n",
            )
            .expect("write repeated hello");
    }
    drop(child.stdin.take());

    let output = child.wait_with_output().expect("wait for RPC process");
    assert!(
        output.status.success(),
        "RPC process failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let responses = String::from_utf8(output.stdout)
        .expect("RPC stdout must be UTF-8 JSONL")
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).expect("parse RPC response"))
        .collect::<Vec<_>>();
    assert_eq!(responses.len(), 12);

    assert_eq!(responses[0]["command"], "parse");
    assert_eq!(responses[0]["data"]["code"], "request_too_large");
    assert_eq!(
        responses[0]["data"]["maxBytes"],
        RPC_JSONL_FRAME_BYTES as u64
    );
    for response in &responses[1..=2] {
        assert_eq!(response["command"], "parse");
        assert_eq!(response["error"], "Failed to parse command: malformed JSON");
    }
    assert_eq!(
        responses[3]["data"]["code"],
        "protocol_negotiation_required"
    );
    assert_eq!(responses[3]["data"]["recovery"], "send_hello");
    assert_eq!(responses[4]["data"]["code"], "unsupported_protocol_version");
    assert_eq!(responses[4]["data"]["requested"]["major"], 1);
    assert_eq!(responses[4]["data"]["supported"]["major"], 3);
    assert_eq!(responses[5]["data"]["limit"], "identifier_bytes");
    assert_eq!(responses[6]["data"]["limit"], "image_count");
    assert_eq!(responses[7]["data"]["limit"], "json_depth");
    let expected_hello: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/rpc-hello-response.json"))
            .expect("parse RPC hello baseline");
    assert_eq!(responses[8], expected_hello);
    assert_eq!(responses[9]["id"], "state");
    assert_eq!(responses[9]["success"], true);
    assert_eq!(
        responses[9]["data"]["negotiatedProtocol"]["rpc"]["major"],
        3
    );
    assert_eq!(responses[10]["id"], "unknown");
    assert_eq!(responses[10]["success"], false);
    assert_eq!(responses[10]["command"], "unknown_command");
    assert_eq!(responses[11]["id"], "hello-again");
    assert_eq!(responses[11]["data"]["code"], "protocol_already_negotiated");
}

#[test]
fn rpc_stdio_flushes_before_eof_and_returns_idempotent_detach_status() {
    let runtime_dir = tempfile::tempdir().expect("create isolated evo runtime directory");
    let project_dir = tempfile::tempdir().expect("create isolated project directory");
    let mut child = Command::new(env!("CARGO_BIN_EXE_coding-agent"))
        .args(["--mode", "rpc"])
        .current_dir(project_dir.path())
        .env("EVO_DIR", runtime_dir.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start cli RPC process");
    let mut stdin = child.stdin.take().expect("RPC stdin should be piped");
    let mut stdout = BufReader::new(child.stdout.take().expect("RPC stdout should be piped"));

    stdin
        .write_all(
            b"{\"id\":\"hello\",\"type\":\"hello\",\"protocol\":{\"family\":\"rpc\",\"major\":3,\"minor\":0}}\n",
        )
        .expect("write compatible hello");
    stdin.flush().expect("flush compatible hello");
    let hello = read_json_line(&mut stdout);
    assert_eq!(hello["id"], "hello");
    assert_eq!(hello["success"], true);

    stdin
        .write_all(b"{\"id\":\"state\",\"type\":\"get_state\"}\n")
        .expect("write state command before EOF");
    stdin.flush().expect("flush state command before EOF");
    let state = read_json_line(&mut stdout);
    assert_eq!(state["id"], "state");
    assert_eq!(state["success"], true);
    assert_eq!(state["data"]["negotiatedProtocol"]["rpc"]["major"], 3);

    for command in [
        b"{\"id\":\"thinking\",\"type\":\"set_thinking_level\",\"level\":\"high\"}\n".as_slice(),
        b"{\"id\":\"steering\",\"type\":\"set_steering_mode\",\"mode\":\"one-at-a-time\"}\n"
            .as_slice(),
        b"{\"id\":\"follow-up\",\"type\":\"set_follow_up_mode\",\"mode\":\"all\"}\n".as_slice(),
        b"{\"id\":\"name\",\"type\":\"set_session_name\",\"name\":\"Review workspace\"}\n"
            .as_slice(),
    ] {
        stdin
            .write_all(command)
            .expect("write adapter-local state command before EOF");
        stdin
            .flush()
            .expect("flush adapter-local state command before EOF");
        let response = read_json_line(&mut stdout);
        assert_eq!(response["success"], true, "{response}");
    }
    stdin
        .write_all(b"{\"id\":\"updated-state\",\"type\":\"get_state\"}\n")
        .expect("write updated state command before EOF");
    stdin.flush().expect("flush updated state before EOF");
    let updated = read_json_line(&mut stdout);
    assert_eq!(updated["data"]["thinkingLevel"], "high");
    assert_eq!(updated["data"]["steeringMode"], "one-at-a-time");
    assert_eq!(updated["data"]["followUpMode"], "all");
    assert_eq!(updated["data"]["sessionName"], "Review workspace");
    assert_eq!(updated["data"]["sessionNamePersistence"], "adapter_local");

    stdin
        .write_all(b"{\"id\":\"detach\",\"type\":\"detach\"}\n")
        .expect("write detach command before EOF");
    stdin.flush().expect("flush detach command before EOF");
    let detached = read_json_line(&mut stdout);
    assert_eq!(detached["id"], "detach");
    assert_eq!(detached["command"], "detach");
    assert_eq!(detached["data"]["status"], "already_detached");

    drop(stdin);
    drop(stdout);
    let output = child.wait_with_output().expect("wait for RPC process");
    assert!(
        output.status.success(),
        "RPC process failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn read_json_line(reader: &mut impl BufRead) -> serde_json::Value {
    let mut line = String::new();
    let read = reader
        .read_line(&mut line)
        .expect("read RPC JSONL response");
    assert_ne!(read, 0, "RPC process closed stdout before responding");
    serde_json::from_str(&line).expect("RPC response must be valid JSON")
}
