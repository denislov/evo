const TUI_RUNTIME_TEST_SOURCE: &str = include_str!("../render/runtime.rs");
const STDIN_BUFFER_SOURCE: &str = include_str!("../../src/input/stdin.rs");
const TERMINAL_SOURCE: &str = include_str!("../../src/terminal/lifecycle.rs");

#[test]
fn tests_use_named_time_constants() {
    let cases: &[(&str, &str, &str, Option<&str>)] = &[
        (
            TUI_RUNTIME_TEST_SOURCE,
            "const RENDER_SCHEDULER_",
            "render scheduler",
            None,
        ),
        (
            STDIN_BUFFER_SOURCE,
            "const STDIN_BUFFER_",
            "stdin_buffer",
            Some("stdin_buffer"),
        ),
    ];
    for (source, named_prefix, label, tests_module) in cases {
        let mut violations = Vec::new();
        let lines: Vec<_> = source.lines().collect();
        let start_index = match tests_module {
            Some(name) => tests_start_index(&lines, name),
            None => 0,
        };
        for (index, line) in lines.iter().enumerate().skip(start_index) {
            if !line.contains("Duration::from_millis") {
                continue;
            }
            if line.trim_start().starts_with(named_prefix) {
                continue;
            }
            violations.push(format!("{}: {}", index + 1, line.trim()));
        }
        assert!(
            violations.is_empty(),
            "{label} tests should use named timing constants instead of inline fixed durations:\n{}",
            violations.join("\n")
        );
    }

    // terminal drain_input calls must not inline durations either
    let mut violations = Vec::new();
    let lines: Vec<_> = TERMINAL_SOURCE.lines().collect();
    let start_index = tests_start_index(&lines, "terminal");
    for index in start_index..lines.len() {
        let line = lines[index];
        if !line.contains("drain_input(") {
            continue;
        }
        let window = lines[index..std::cmp::min(index + 4, lines.len())].join("\n");
        if window.contains("Duration::from_millis") {
            violations.push(format!("{}: {}", index + 1, line.trim()));
        }
    }
    assert!(
        violations.is_empty(),
        "terminal drain_input tests should use named timing constants instead of inline fixed durations:\n{}",
        violations.join("\n")
    );
}

#[test]
fn tests_use_named_clock_anchor() {
    for (source, anchor_fn, start_at, label) in [
        (
            TUI_RUNTIME_TEST_SOURCE,
            "fn render_scheduler_clock_anchor",
            None,
            "render scheduler",
        ),
        (
            STDIN_BUFFER_SOURCE,
            "fn stdin_buffer_clock_anchor",
            Some("stdin_buffer"),
            "stdin_buffer",
        ),
    ] {
        let mut violations = Vec::new();
        let lines: Vec<_> = source.lines().collect();
        let start_index = match start_at {
            Some(name) => tests_start_index(&lines, name),
            None => 0,
        };
        for (index, line) in lines.iter().enumerate().skip(start_index) {
            if !line.contains("Instant::now()") {
                continue;
            }
            let prefix = lines[index.saturating_sub(2)..=index].join("\n");
            if prefix.contains(anchor_fn) {
                continue;
            }
            violations.push(format!("{}: {}", index + 1, line.trim()));
        }
        assert!(
            violations.is_empty(),
            "{label} tests should use a named clock anchor helper instead of scattering Instant::now():\n{}",
            violations.join("\n")
        );
    }
}

fn tests_start_index(lines: &[&str], source_name: &str) -> usize {
    lines
        .iter()
        .position(|line| line.contains("mod tests"))
        .unwrap_or_else(|| panic!("{source_name} source should contain a unit-test module"))
}
