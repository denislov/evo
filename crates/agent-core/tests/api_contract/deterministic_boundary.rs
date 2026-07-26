const PARALLEL_TOOLS_TEST_SOURCE: &str = include_str!("../tool_hooks/parallel_tools.rs");

#[test]
fn agent_loop_tests_do_not_use_wall_clock_sleep_for_ordering() {
    let source_path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/agent/loop_runtime.rs");
    let source = std::fs::read_to_string(&source_path)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", source_path.display()));

    assert!(
        !source.contains("tokio::time::sleep"),
        "agent loop ordering tests should use observable events instead of wall-clock sleep"
    );
}

#[test]
fn parallel_tools_virtual_time_advances_use_named_durations() {
    assert_no_inline_virtual_time_advances(
        PARALLEL_TOOLS_TEST_SOURCE,
        "parallel_tools",
        "parallel_tools tests should use named virtual-time advance durations instead of inline fixed durations",
    );
}

fn assert_no_inline_virtual_time_advances(source: &str, source_name: &str, message: &str) {
    let mut violations = Vec::new();
    let lines: Vec<_> = source.lines().collect();

    for (index, line) in lines.iter().enumerate() {
        if !line.contains("tokio::time::advance(") {
            continue;
        }
        let window = lines[index..std::cmp::min(index + 3, lines.len())].join("\n");
        if window.contains("Duration::from_millis") {
            violations.push(format!("{}:{}: {}", source_name, index + 1, line.trim()));
        }
    }

    assert!(
        violations.is_empty(),
        "{}:\n{}",
        message,
        violations.join("\n")
    );
}
