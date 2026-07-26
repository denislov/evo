const TUI_RUNTIME_TEST_SOURCE: &str = include_str!("../render/runtime.rs");
const STDIN_BUFFER_SOURCE: &str = include_str!("../../src/input/stdin.rs");
const TERMINAL_SOURCE: &str = include_str!("../../src/terminal/lifecycle.rs");

#[test]
fn render_scheduler_tests_use_named_time_constants() {
    let mut violations = Vec::new();

    for (index, line) in TUI_RUNTIME_TEST_SOURCE.lines().enumerate() {
        if !line.contains("Duration::from_millis") {
            continue;
        }
        if line.trim_start().starts_with("const RENDER_SCHEDULER_") {
            continue;
        }
        violations.push(format!("{}: {}", index + 1, line.trim()));
    }

    assert!(
        violations.is_empty(),
        "render scheduler tests should use named timing constants instead of inline fixed durations:\n{}",
        violations.join("\n")
    );
}

#[test]
fn render_scheduler_tests_use_named_clock_anchor() {
    let mut violations = Vec::new();
    let lines: Vec<_> = TUI_RUNTIME_TEST_SOURCE.lines().collect();

    for (index, line) in lines.iter().enumerate() {
        if !line.contains("Instant::now()") {
            continue;
        }
        let prefix = lines[index.saturating_sub(2)..=index].join("\n");
        if prefix.contains("fn render_scheduler_clock_anchor") {
            continue;
        }
        violations.push(format!("{}: {}", index + 1, line.trim()));
    }

    assert!(
        violations.is_empty(),
        "render scheduler tests should use a named clock anchor helper instead of scattering Instant::now():\n{}",
        violations.join("\n")
    );
}

#[test]
fn stdin_buffer_tests_use_named_time_constants() {
    let mut violations = Vec::new();

    for (line_number, line) in stdin_buffer_test_lines() {
        if !line.contains("Duration::from_millis") {
            continue;
        }
        if line.trim_start().starts_with("const STDIN_BUFFER_") {
            continue;
        }
        violations.push(format!("{}: {}", line_number, line.trim()));
    }

    assert!(
        violations.is_empty(),
        "stdin_buffer unit tests should use named timing constants instead of inline fixed durations:\n{}",
        violations.join("\n")
    );
}

#[test]
fn stdin_buffer_tests_use_named_clock_anchor() {
    let mut violations = Vec::new();
    let lines: Vec<_> = STDIN_BUFFER_SOURCE.lines().collect();
    let start_index = stdin_buffer_tests_start_index(&lines);

    for index in start_index..lines.len() {
        let line = lines[index];
        if !line.contains("Instant::now()") {
            continue;
        }
        let prefix = lines[index.saturating_sub(2)..=index].join("\n");
        if prefix.contains("fn stdin_buffer_clock_anchor") {
            continue;
        }
        violations.push(format!("{}: {}", index + 1, line.trim()));
    }

    assert!(
        violations.is_empty(),
        "stdin_buffer unit tests should use a named clock anchor helper instead of scattering Instant::now():\n{}",
        violations.join("\n")
    );
}

#[test]
fn terminal_drain_input_test_uses_named_durations() {
    let mut violations = Vec::new();
    let lines: Vec<_> = TERMINAL_SOURCE.lines().collect();
    let start_index = terminal_tests_start_index(&lines);

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

fn stdin_buffer_test_lines() -> impl Iterator<Item = (usize, &'static str)> {
    let lines: Vec<_> = STDIN_BUFFER_SOURCE.lines().collect();
    let start_index = stdin_buffer_tests_start_index(&lines);
    lines
        .into_iter()
        .enumerate()
        .skip(start_index)
        .map(|(index, line)| (index + 1, line))
}

fn stdin_buffer_tests_start_index(lines: &[&str]) -> usize {
    source_tests_start_index(lines, "stdin_buffer")
}

fn terminal_tests_start_index(lines: &[&str]) -> usize {
    source_tests_start_index(lines, "terminal")
}

fn source_tests_start_index(lines: &[&str], source_name: &str) -> usize {
    lines
        .iter()
        .position(|line| line.contains("mod tests"))
        .unwrap_or_else(|| panic!("{source_name} source should contain a unit-test module"))
}
