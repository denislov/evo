const THEME_RELOAD_SOURCE: &str = include_str!("../../src/theme/reload.rs");
const THEME_TEST_SOURCE: &str = include_str!("../config_request/theme.rs");
const TOOL_BASH_TEST_SOURCE: &str = include_str!("../../src/internal_tests/tool_bash.rs");
const INTERACTIVE_LOOP_SOURCE: &str = include_str!("../../../cli/src/interactive/loop.rs");
const FILE_MUTATION_QUEUE_TEST_SOURCE: &str =
    include_str!("../../src/internal_tests/file_mutation_queue.rs");

#[test]
fn product_fullscreen_visual_vocabulary_stays_out_of_tui() {
    fn rust_sources(root: &std::path::Path, output: &mut Vec<std::path::PathBuf>) {
        for entry in std::fs::read_dir(root).expect("read source directory") {
            let path = entry.expect("read source entry").path();
            if path.is_dir() {
                rust_sources(&path, output);
            } else if path.extension().is_some_and(|extension| extension == "rs") {
                output.push(path);
            }
        }
    }

    let tui = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../tui/src");
    let mut sources = Vec::new();
    rust_sources(&tui, &mut sources);
    let forbidden = [
        "Context rail",
        "Context drawer",
        "Context page",
        "Composer assistance",
        "TransientOverlayRole",
    ];
    let mut violations = Vec::new();
    for path in sources {
        let source = std::fs::read_to_string(&path).expect("read tui source");
        for term in forbidden {
            if source.contains(term) {
                violations.push(format!("{}: {term}", path.display()));
            }
        }
    }
    assert!(
        violations.is_empty(),
        "product fullscreen vocabulary belongs to coding-agent:\n{}",
        violations.join("\n")
    );
}

#[test]
fn theme_reload_worker_does_not_poll_with_thread_sleep() {
    assert!(
        !THEME_RELOAD_SOURCE.contains("std::thread::sleep"),
        "theme reload worker should use condition-based waiting instead of fixed thread sleeps"
    );
}

#[test]
fn tool_bash_timeout_tests_do_not_poll_with_fixed_sleep() {
    assert!(
        !TOOL_BASH_TEST_SOURCE.contains("tokio::time::sleep"),
        "tool_bash timeout tests should use process-exit observation instead of fixed sleep polling"
    );
}

#[test]
fn tool_bash_hang_guards_use_named_timeout_helper() {
    let mut violations = Vec::new();
    let lines: Vec<_> = TOOL_BASH_TEST_SOURCE.lines().collect();

    for (index, line) in lines.iter().enumerate() {
        if !line.contains("tokio::time::timeout(") {
            continue;
        }
        let prefix = lines[index.saturating_sub(8)..index].join("\n");
        if prefix.contains("async fn run_bash_with_hang_guard") {
            continue;
        }
        let window = lines[index..std::cmp::min(index + 8, lines.len())].join("\n");
        if window.contains("bash_execute(") {
            violations.push(format!("{}: {}", index + 1, line.trim()));
        }
    }

    assert!(
        violations.is_empty(),
        "tool_bash command hang tests should route bash_execute timeouts through a named helper instead of scattering fixed-duration harness waits:\n{}",
        violations.join("\n")
    );
}

#[test]
fn tool_bash_pid_exit_waits_use_named_timeout_constants() {
    let mut violations = Vec::new();
    let lines: Vec<_> = TOOL_BASH_TEST_SOURCE.lines().collect();

    for (index, line) in lines.iter().enumerate() {
        if !line.contains("wait_for_pid_to_exit(") || line.contains("async fn wait_for_pid_to_exit")
        {
            continue;
        }
        let window = lines[index..std::cmp::min(index + 4, lines.len())].join("\n");
        if window.contains("Duration::from_") {
            violations.push(format!("{}: {}", index + 1, line.trim()));
        }
    }

    assert!(
        violations.is_empty(),
        "tool_bash PID-exit observation waits should use named timeout constants instead of inline fixed durations:\n{}",
        violations.join("\n")
    );
}

#[test]
fn tool_bash_shell_timing_literals_use_named_constants() {
    let mut violations = Vec::new();

    for (index, line) in TOOL_BASH_TEST_SOURCE.lines().enumerate() {
        if contains_numeric_literal_after(line, "sleep ")
            || contains_numeric_literal_after(line, "\"timeout\":")
            || contains_numeric_literal_after(line, "Command timed out after ")
        {
            violations.push(format!("{}: {}", index + 1, line.trim()));
        }
    }

    assert!(
        violations.is_empty(),
        "tool_bash timing-sensitive shell sleeps, command timeouts, and timeout assertions should use named constants/helpers instead of inline fixed values:\n{}",
        violations.join("\n")
    );
}

#[test]
fn interactive_loop_shutdown_drain_uses_named_durations() {
    let mut violations = Vec::new();
    let lines: Vec<_> = INTERACTIVE_LOOP_SOURCE.lines().collect();

    for (index, line) in lines.iter().enumerate() {
        if !line.contains(".drain_input(") {
            continue;
        }
        let window = lines[index..std::cmp::min(index + 4, lines.len())].join("\n");
        if window.contains("Duration::from_") {
            violations.push(format!("{}: {}", index + 1, line.trim()));
        }
    }

    assert!(
        violations.is_empty(),
        "interactive loop shutdown drain should use named duration constants instead of inline fixed durations:\n{}",
        violations.join("\n")
    );
}

#[test]
fn file_mutation_queue_tests_use_named_channel_timeout_helper() {
    let mut violations = Vec::new();
    let lines: Vec<_> = FILE_MUTATION_QUEUE_TEST_SOURCE.lines().collect();

    for (index, line) in lines.iter().enumerate() {
        if !line.contains("tokio::time::timeout(Duration::from") {
            continue;
        }
        let prefix = lines[index.saturating_sub(6)..index].join("\n");
        if prefix.contains("async fn recv_file_mutation_signal") {
            continue;
        }
        let window = lines[index..std::cmp::min(index + 4, lines.len())].join("\n");
        if window.contains("entered_rx") {
            violations.push(format!("{}: {}", index + 1, line.trim()));
        }
    }

    assert!(
        violations.is_empty(),
        "file_mutation_queue tests should route channel waits through a named timeout helper instead of scattering fixed-duration entered_rx waits:\n{}",
        violations.join("\n")
    );
}

#[test]
fn theme_reload_unit_tests_use_named_time_constants() {
    let mut violations = Vec::new();
    let lines: Vec<_> = THEME_RELOAD_SOURCE.lines().collect();
    let start_index = source_tests_start_index(&lines, "theme reload");

    for (index, &line) in lines.iter().enumerate().skip(start_index) {
        if !line.contains("Duration::from_millis") {
            continue;
        }
        if line.trim_start().starts_with("const THEME_RELOAD_TEST_") {
            continue;
        }
        violations.push(format!("{}: {}", index + 1, line.trim()));
    }

    assert!(
        violations.is_empty(),
        "theme reload unit tests should use named timing constants instead of inline fixed durations:\n{}",
        violations.join("\n")
    );
}

#[test]
fn theme_reload_unit_tests_use_named_clock_anchor() {
    let mut violations = Vec::new();
    let lines: Vec<_> = THEME_RELOAD_SOURCE.lines().collect();
    let start_index = source_tests_start_index(&lines, "theme reload");

    for index in start_index..lines.len() {
        let line = lines[index];
        if !line.contains("Instant::now()") {
            continue;
        }
        let prefix = lines[index.saturating_sub(2)..=index].join("\n");
        if prefix.contains("fn theme_reload_test_clock_anchor") {
            continue;
        }
        violations.push(format!("{}: {}", index + 1, line.trim()));
    }

    assert!(
        violations.is_empty(),
        "theme reload unit tests should use a named clock anchor helper instead of scattering Instant::now():\n{}",
        violations.join("\n")
    );
}

#[test]
fn theme_watcher_tests_use_named_debounce_durations() {
    let mut violations = Vec::new();
    let lines: Vec<_> = THEME_TEST_SOURCE.lines().collect();

    for (index, line) in lines.iter().enumerate() {
        if !line.contains("ThemeWatcher::start(") {
            continue;
        }
        let window = lines[index..std::cmp::min(index + 8, lines.len())].join("\n");
        if window.contains("Duration::from_millis") {
            violations.push(format!("{}: {}", index + 1, line.trim()));
        }
    }

    assert!(
        violations.is_empty(),
        "theme watcher tests should use named debounce duration constants instead of inline fixed durations:\n{}",
        violations.join("\n")
    );
}

fn contains_numeric_literal_after(line: &str, marker: &str) -> bool {
    line.split_once(marker)
        .and_then(|(_, suffix)| suffix.trim_start().chars().next())
        .is_some_and(|character| character.is_ascii_digit())
}

fn source_tests_start_index(lines: &[&str], source_name: &str) -> usize {
    lines
        .iter()
        .position(|line| line.trim() == "#[cfg(test)]")
        .unwrap_or_else(|| panic!("{source_name} source should contain a #[cfg(test)] module"))
}

#[test]
fn theme_watcher_tests_use_named_signal_timeout_helper() {
    let mut violations = Vec::new();
    let lines: Vec<_> = THEME_TEST_SOURCE.lines().collect();

    for (index, line) in lines.iter().enumerate() {
        if !line.contains("tokio::time::timeout(Duration::from") {
            continue;
        }
        let window = lines[index..std::cmp::min(index + 4, lines.len())].join("\n");
        if window.contains("signal.recv()") {
            violations.push(format!("{}: {}", index + 1, line.trim()));
        }
    }

    assert!(
        violations.is_empty(),
        "theme watcher tests should route reload signal waits through a named timeout helper instead of scattering fixed-duration signal.recv waits:\n{}",
        violations.join("\n")
    );
}
