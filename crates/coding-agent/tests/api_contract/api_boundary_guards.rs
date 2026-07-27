//! Stable facade and external-consumer boundary coverage.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

#[test]
fn external_consumer_fixtures_enforce_the_stable_facade_boundary() {
    run_external_consumer_fixtures();
}

#[derive(Clone, Copy)]
struct CompileFixture {
    category: &'static str,
    access_path: &'static str,
    source: &'static str,
    expected: ExpectedDiagnostic,
}

#[derive(Clone, Copy)]
struct ExpectedDiagnostic {
    code: &'static str,
    line: u64,
    column_start: u64,
    column_end: u64,
    forbidden: &'static str,
    forbidden_path: &'static str,
    symbol: &'static str,
    fragments: &'static [&'static str],
}

#[test]
fn external_diagnostic_matcher_requires_code_primary_span_and_forbidden_surface() {
    let diagnostic = serde_json::json!({
        "reason": "compiler-message",
        "message": {
            "code": { "code": "E0432" },
            "message": "unresolved import `coding_agent::api::Operation`",
            "spans": [{
                "file_name": "src/main.rs",
                "line_start": 1,
                "line_end": 1,
                "column_start": 30,
                "column_end": 39,
                "is_primary": true,
                "label": "no `Operation` in `api`"
            }],
            "children": [],
            "rendered": "error[E0432]: unresolved import `coding_agent::api::Operation`"
        }
    });
    let expected = ExpectedDiagnostic {
        code: "E0432",
        line: 1,
        column_start: 30,
        column_end: 39,
        forbidden: "Operation",
        forbidden_path: "coding_agent::api",
        symbol: "Operation",
        fragments: &["unresolved import", "coding_agent::api::Operation"],
    };

    assert!(diagnostic_matches(&diagnostic, &expected).is_ok());
    let mut enclosing_span = diagnostic.clone();
    enclosing_span["message"]["spans"][0]["column_start"] = 5.into();
    assert!(
        diagnostic_matches(&enclosing_span, &expected).is_ok(),
        "newer rustc versions may highlight the full forbidden import path"
    );
    let mut unrelated_span = diagnostic.clone();
    unrelated_span["message"]["spans"][0]["column_end"] = 20.into();
    assert!(diagnostic_matches(&unrelated_span, &expected).is_err());
    let wrong_code = ExpectedDiagnostic {
        code: "E0603",
        ..expected
    };
    assert!(diagnostic_matches(&diagnostic, &wrong_code).is_err());
}

const FAIL_FIXTURES: [CompileFixture; 19] = [
    CompileFixture {
        category: "private-cli-runtime-seed",
        access_path: "runtime-category",
        source: "cli_run_options_api.rs",
        expected: unresolved(33, 46, "CliRunOptions", "coding_agent::api::runtime"),
    },
    CompileFixture {
        category: "retired-cli-category",
        access_path: "api",
        source: "run_cli_with_options_api.rs",
        expected: missing_module(24, 27, "cli", "coding_agent::api"),
    },
    CompileFixture {
        category: "retired-protocol-category",
        access_path: "api",
        source: "run_rpc_mode_for_io_api.rs",
        expected: unresolved(24, 32, "protocol", "coding_agent::api"),
    },
    CompileFixture {
        category: "private-prompt-runtime-seed",
        access_path: "operation-category",
        source: "prompt_run_options_api.rs",
        expected: unresolved(35, 51, "PromptRunOptions", "coding_agent::api::operation"),
    },
    CompileFixture {
        category: "operation-dispatch",
        access_path: "api",
        source: "operation_dispatch_api.rs",
        expected: unresolved(24, 43, "OperationDescriptor", "coding_agent::api"),
    },
    CompileFixture {
        category: "operation-dispatch",
        access_path: "root",
        source: "operation_dispatch_root.rs",
        expected: unresolved(19, 38, "OperationDescriptor", "coding_agent"),
    },
    CompileFixture {
        category: "operation-dispatch",
        access_path: "doc-hidden",
        source: "operation_dispatch_hidden.rs",
        expected: private_module(19, 26, "runtime"),
    },
    CompileFixture {
        category: "services",
        access_path: "api",
        source: "services_api.rs",
        expected: unresolved(25, 37, "EventService", "coding_agent::api"),
    },
    CompileFixture {
        category: "services",
        access_path: "root",
        source: "services_root.rs",
        expected: unresolved(20, 32, "EventService", "coding_agent"),
    },
    CompileFixture {
        category: "services",
        access_path: "doc-hidden",
        source: "services_hidden.rs",
        expected: private_module(19, 27, "services"),
    },
    CompileFixture {
        category: "plugin-options-registries",
        access_path: "api",
        source: "plugins_api.rs",
        expected: unresolved(25, 42, "PluginLoadOptions", "coding_agent::api"),
    },
    CompileFixture {
        category: "plugin-options-registries",
        access_path: "root",
        source: "plugins_root.rs",
        expected: unresolved(20, 37, "PluginLoadOptions", "coding_agent"),
    },
    CompileFixture {
        category: "plugin-options-registries",
        access_path: "doc-hidden",
        source: "plugins_hidden.rs",
        expected: missing_module(31, 42, "plugin_load", "coding_agent::operations"),
    },
    CompileFixture {
        category: "flow-contracts",
        access_path: "api",
        source: "flow_api.rs",
        expected: unresolved(25, 29, "Flow", "coding_agent::api"),
    },
    CompileFixture {
        category: "flow-contracts",
        access_path: "root",
        source: "flow_root.rs",
        expected: unresolved(20, 24, "Flow", "coding_agent"),
    },
    CompileFixture {
        category: "flow-contracts",
        access_path: "doc-hidden",
        source: "flow_hidden.rs",
        expected: private_module(19, 29, "operations"),
    },
    CompileFixture {
        category: "legacy-root-args-module",
        access_path: "root",
        source: "args_root.rs",
        expected: unresolved(19, 23, "args", "coding_agent"),
    },
    CompileFixture {
        category: "legacy-root-error-module",
        access_path: "root",
        source: "error_root.rs",
        expected: unresolved(19, 24, "error", "coding_agent"),
    },
    CompileFixture {
        category: "legacy-root-prompt-options-module",
        access_path: "root",
        source: "prompt_options_root.rs",
        expected: unresolved(19, 33, "prompt_options", "coding_agent"),
    },
];

const fn unresolved(
    column_start: u64,
    column_end: u64,
    forbidden: &'static str,
    forbidden_path: &'static str,
) -> ExpectedDiagnostic {
    ExpectedDiagnostic {
        code: "E0432",
        line: 1,
        column_start,
        column_end,
        forbidden,
        forbidden_path,
        symbol: forbidden,
        fragments: &["unresolved import"],
    }
}

const fn missing_module(
    column_start: u64,
    column_end: u64,
    forbidden: &'static str,
    forbidden_path: &'static str,
) -> ExpectedDiagnostic {
    ExpectedDiagnostic {
        code: "E0433",
        line: 1,
        column_start,
        column_end,
        forbidden,
        forbidden_path,
        symbol: forbidden,
        fragments: &["could not find"],
    }
}

const fn private_module(
    column_start: u64,
    column_end: u64,
    symbol: &'static str,
) -> ExpectedDiagnostic {
    ExpectedDiagnostic {
        code: "E0603",
        line: 1,
        column_start,
        column_end,
        forbidden: symbol,
        forbidden_path: "coding_agent",
        symbol,
        fragments: &["module", "is private"],
    }
}

fn run_external_consumer_fixtures() {
    let crate_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let fixture_root = crate_root.join("tests/fixtures/api_boundary");
    let workspace_root = crate_root
        .parent()
        .and_then(Path::parent)
        .expect("coding-agent should live below the workspace root");
    let fixture_work_root = workspace_root.join("target/api-boundary-fixtures");
    fs::create_dir_all(&fixture_work_root).expect("create project-local external consumer root");
    let consumer = tempfile::Builder::new()
        .prefix("consumer-")
        .tempdir_in(&fixture_work_root)
        .expect("create project-local external consumer directory");
    let source_dir = consumer.path().join("src");
    fs::create_dir(&source_dir).expect("create external consumer source directory");
    fs::write(
        consumer.path().join("Cargo.toml"),
        format!(
            "[package]\nname = \"coding-agent-api-boundary-fixture\"\nversion = \"0.0.0\"\nedition = \"2024\"\n\n[dependencies]\ncoding-agent = {{ path = {:?} }}\n\n[workspace]\n",
            crate_root
        ),
    )
    .expect("write external consumer manifest");
    fs::copy(
        workspace_root.join("Cargo.lock"),
        consumer.path().join("Cargo.lock"),
    )
    .expect("copy the workspace lockfile for deterministic offline resolution");

    let positive = compile_fixture(consumer.path(), &fixture_root.join("pass/stable_facade.rs"));
    assert!(
        positive.status.success(),
        "stable facade external consumer should compile:\n{}",
        command_diagnostics(&positive)
    );

    let expected_matrix = FAIL_FIXTURES
        .iter()
        .map(|fixture| (fixture.category, fixture.access_path))
        .collect::<BTreeSet<_>>();
    assert_eq!(
        expected_matrix.len(),
        19,
        "negative fixtures must cover internal contracts and retired root compatibility modules"
    );

    for fixture in FAIL_FIXTURES {
        let fixture_path = fixture_root.join("fail").join(fixture.source);
        validate_declared_source_span(&fixture_path, &fixture.expected);
        let output = compile_fixture(consumer.path(), &fixture_path);
        let diagnostics = command_diagnostics(&output);
        assert!(
            !output.status.success(),
            "{} must remain inaccessible through the {} path",
            fixture.category,
            fixture.access_path
        );
        let errors = compiler_error_diagnostics(&output);
        assert!(
            !errors.is_empty(),
            "Cargo emitted no rustc error diagnostic:\n{diagnostics}"
        );
        diagnostic_matches(&errors[0], &fixture.expected).unwrap_or_else(|mismatch| {
            panic!(
                "{} through {} failed for an unrelated first compiler error: {mismatch}\n{}",
                fixture.category, fixture.access_path, diagnostics
            )
        });
    }
}

fn compile_fixture(consumer_root: &Path, fixture: &Path) -> Output {
    fs::copy(fixture, consumer_root.join("src/main.rs")).unwrap_or_else(|error| {
        panic!("copy fixture {}: {error}", fixture.display());
    });
    Command::new(env!("CARGO"))
        .args(["check", "--offline", "--quiet", "--message-format=json"])
        .current_dir(consumer_root)
        .env("CARGO_TARGET_DIR", consumer_root.join("target"))
        .output()
        .unwrap_or_else(|error| panic!("run Cargo for fixture {}: {error}", fixture.display()))
}

fn validate_declared_source_span(fixture: &Path, expected: &ExpectedDiagnostic) {
    let source = fs::read_to_string(fixture)
        .unwrap_or_else(|error| panic!("read fixture {}: {error}", fixture.display()));
    let line = source
        .lines()
        .nth(expected.line.saturating_sub(1) as usize)
        .unwrap_or_else(|| {
            panic!(
                "fixture {} omitted line {}",
                fixture.display(),
                expected.line
            )
        });
    let start = expected.column_start.saturating_sub(1) as usize;
    let end = expected.column_end.saturating_sub(1) as usize;
    assert_eq!(
        line.get(start..end),
        Some(expected.forbidden),
        "declared forbidden span drifted in {}",
        fixture.display()
    );
    assert!(
        line.contains(expected.forbidden_path),
        "declared forbidden path `{}` is absent from {}",
        expected.forbidden_path,
        fixture.display()
    );
}

fn compiler_error_diagnostics(output: &Output) -> Vec<serde_json::Value> {
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter(|message| {
            message.get("reason").and_then(|value| value.as_str()) == Some("compiler-message")
                && message
                    .pointer("/message/level")
                    .and_then(|value| value.as_str())
                    == Some("error")
        })
        .collect()
}

fn diagnostic_matches(
    diagnostic: &serde_json::Value,
    expected: &ExpectedDiagnostic,
) -> Result<(), String> {
    let actual_code = diagnostic
        .pointer("/message/code/code")
        .and_then(|value| value.as_str());
    if actual_code != Some(expected.code) {
        return Err(format!(
            "expected {}, found {:?}",
            expected.code, actual_code
        ));
    }

    let primary = diagnostic
        .pointer("/message/spans")
        .and_then(|value| value.as_array())
        .and_then(|spans| {
            spans.iter().find(|span| {
                span.get("is_primary").and_then(|value| value.as_bool()) == Some(true)
                    && span
                        .get("file_name")
                        .and_then(|value| value.as_str())
                        .is_some_and(|file| file == "src/main.rs" || file.ends_with("/src/main.rs"))
                    && span.get("line_start").and_then(|value| value.as_u64())
                        == Some(expected.line)
                    && span
                        .get("column_start")
                        .and_then(|value| value.as_u64())
                        .is_some_and(|start| start <= expected.column_start)
                    && span
                        .get("column_end")
                        .and_then(|value| value.as_u64())
                        .is_some_and(|end| end >= expected.column_end)
            })
        })
        .ok_or_else(|| {
            format!(
                "missing primary src/main.rs span enclosing {}:{}-{}",
                expected.line, expected.column_start, expected.column_end
            )
        })?;

    let mut text = diagnostic
        .pointer("/message/message")
        .and_then(|value| value.as_str())
        .unwrap_or_default()
        .to_owned();
    if let Some(rendered) = diagnostic
        .pointer("/message/rendered")
        .and_then(|value| value.as_str())
    {
        text.push_str(rendered);
    }
    if let Some(label) = primary.get("label").and_then(|value| value.as_str()) {
        text.push_str(label);
    }
    if let Some(children) = diagnostic
        .pointer("/message/children")
        .and_then(|value| value.as_array())
    {
        for child in children {
            if let Some(message) = child.get("message").and_then(|value| value.as_str()) {
                text.push_str(message);
            }
        }
    }
    for fragment in std::iter::once(&expected.symbol)
        .chain(std::iter::once(&expected.forbidden))
        .chain(expected.fragments.iter())
    {
        if !text.contains(fragment) {
            return Err(format!("diagnostic omitted fragment `{fragment}`"));
        }
    }
    Ok(())
}

fn command_diagnostics(output: &Output) -> String {
    format!(
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

#[test]
fn api_is_the_only_public_root_module() {
    let crate_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let lib_source = fs::read_to_string(crate_root.join("src/lib.rs"))
        .expect("coding-agent lib.rs should be readable");
    let mut violations = Vec::new();
    let mut depth = 0usize;

    for (index, line) in lib_source.lines().enumerate() {
        let trimmed = line.trim();
        if depth == 0 && trimmed.starts_with("pub mod ") {
            let module_name = trimmed
                .trim_start_matches("pub mod ")
                .trim_end_matches(';')
                .trim_end_matches('{')
                .trim();
            if module_name != "api" {
                violations.push(format!("{}: {}", index + 1, trimmed));
            }
        }

        for character in line.chars() {
            match character {
                '{' => depth += 1,
                '}' => depth = depth.saturating_sub(1),
                _ => {}
            }
        }
    }

    assert!(
        violations.is_empty(),
        "coding_agent::api must remain the only public root module:\n{}",
        violations.join("\n")
    );
}

#[test]
fn stable_api_has_only_scenario_categories_and_no_flat_exports() {
    let crate_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let lib_source = fs::read_to_string(crate_root.join("src/lib.rs"))
        .expect("coding-agent lib.rs should be readable");
    let api_module =
        module_body(&lib_source, "pub mod api").expect("stable api module should be balanced");

    let expected = BTreeSet::from([
        "authorization",
        "client",
        "embedding",
        "error",
        "event",
        "operation",
        "review",
        "runtime",
        "settings",
        "view",
    ]);
    let mut actual = BTreeSet::new();
    let mut flat_exports = Vec::new();
    let mut depth = 0usize;

    for (index, line) in api_module.lines().enumerate() {
        let trimmed = line.trim();
        if depth == 0 {
            if let Some(rest) = trimmed.strip_prefix("pub mod ") {
                if let Some(name) = rest.split_whitespace().next() {
                    actual.insert(name.trim_end_matches(['{', ';']));
                }
            } else if trimmed.starts_with("pub use ") {
                flat_exports.push(format!("{}: {trimmed}", index + 1));
            }
        }

        for character in line.chars() {
            match character {
                '{' => depth += 1,
                '}' => depth = depth.saturating_sub(1),
                _ => {}
            }
        }
    }

    assert_eq!(actual, expected, "stable facade category set drifted");
    assert!(
        flat_exports.is_empty(),
        "stable facade must not regain flat exports:\n{}",
        flat_exports.join("\n")
    );
}

#[test]
fn changed_file_review_facade_is_narrow_and_does_not_expand_wire_protocols() {
    let crate_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let lib_source = fs::read_to_string(crate_root.join("src/lib.rs"))
        .expect("coding-agent lib.rs should be readable");
    let review_module = module_body(&lib_source, "pub mod review")
        .expect("changed-file review category should have a balanced body");
    for required in [
        "CodingAgentExternalEditorTarget",
        "CodingAgentFileChangeIdentity",
        "CodingAgentFileReview",
        "CodingAgentFileReviewRequest",
        "CodingAgentFileRevision",
    ] {
        assert!(
            review_module.contains(required),
            "changed-file review facade omitted {required}"
        );
    }
    for forbidden in [
        "FilesystemCapability",
        "FilesystemTarget",
        "RuntimeHost",
        "SessionService",
        "EventService",
        "ProfileRegistry",
        "Repository",
        "Storage",
    ] {
        assert!(
            !review_module.contains(forbidden),
            "changed-file review facade leaked internal authority {forbidden}"
        );
    }

    let implementation = fs::read_to_string(crate_root.join("src/runtime/file_review.rs"))
        .expect("read review owner");
    for stable_code in [
        "file_review_change_unauthorized",
        "file_review_revision_stale",
        "file_review_outside_project",
        "file_review_symlink_disallowed",
        "file_review_target_changed",
        "file_review_too_large",
        "file_review_binary",
        "file_review_invalid_utf8",
    ] {
        assert!(
            implementation.contains(stable_code),
            "changed-file review omitted stable fault code {stable_code}"
        );
    }
    for relative in [
        "src/protocol/mod.rs",
        "src/protocol/types.rs",
        "src/protocol/jsonl.rs",
        "src/protocol/version.rs",
    ] {
        let path = crate_root.join(relative);
        assert!(
            !path.exists(),
            "coding-agent must not retain wire protocol owner {}",
            path.display()
        );
    }
}

#[test]
fn retired_cli_facade_is_absent() {
    let crate_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let lib_source = fs::read_to_string(crate_root.join("src/lib.rs"))
        .expect("coding-agent lib.rs should be readable");
    assert!(
        module_body(&lib_source, "pub mod cli {").is_none(),
        "coding-agent must not expose a process/CLI facade"
    );
    for retired in [
        "CliOutput",
        "run_headless_invocation",
        "run_interactive_invocation",
        "run_rpc_invocation",
    ] {
        assert!(
            !lib_source.contains(retired),
            "retired process runner surface returned: {retired}"
        );
    }
}

#[test]
fn implementation_modules_stay_private_and_retired_roots_stay_absent() {
    let crate_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let lib_source = fs::read_to_string(crate_root.join("src/lib.rs"))
        .expect("coding-agent lib.rs should be readable");

    for module in [
        "app",
        "config",
        "resources",
        "runtime",
        "session",
        "theme",
        "tools",
    ] {
        assert!(
            lib_source.contains(&format!("mod {module};")),
            "retired root compatibility module `{module}` should remain private"
        );
        assert!(
            !lib_source.contains(&format!("pub mod {module};")),
            "retired root compatibility module `{module}` must not become public again"
        );
    }
    for retired_owner in ["adapters", "protocol"] {
        assert!(
            !lib_source.contains(&format!("mod {retired_owner};"))
                && !lib_source.contains(&format!("pub mod {retired_owner};")),
            "application-owned `{retired_owner}` module must stay absent from coding-agent"
        );
        assert!(
            !crate_root
                .join("src")
                .join(retired_owner)
                .join("mod.rs")
                .exists(),
            "application-owned `{retired_owner}` module must stay absent from coding-agent"
        );
    }

    for retired in [
        "args",
        "error",
        "input",
        "interactive",
        "list_models",
        "models",
        "print_mode",
        "prompt_options",
        "request",
    ] {
        assert!(
            !lib_source.contains(&format!("mod {retired};"))
                && !lib_source.contains(&format!("pub mod {retired};")),
            "retired root compatibility module `{retired}` must stay absent"
        );
    }

    let app_source = fs::read_to_string(crate_root.join("src/app/mod.rs"))
        .expect("app module should be readable");
    assert!(
        !app_source.contains("mod cli;"),
        "the product application must not retain a CLI owner bucket"
    );
    assert!(!app_source.contains("mod shutdown;"));
    assert!(!app_source.contains("pub mod cli;"));
    assert!(
        !crate_root.join("src/app/cli").exists(),
        "the retired product CLI owner directory must stay deleted"
    );
    for owner in [
        "application",
        "error",
        "model_selection",
        "prompt_input",
        "prompt_runtime",
        "startup",
    ] {
        assert!(
            app_source.contains(&format!("pub(crate) mod {owner};")),
            "product application owner `{owner}` must remain crate-private"
        );
        assert!(
            !app_source.contains(&format!("pub mod {owner};")),
            "product application owner `{owner}` must not become public"
        );
    }

    let tools_source = fs::read_to_string(crate_root.join("src/tools/mod.rs"))
        .expect("tools module should be readable");
    for owner in ["filesystem", "mutation_queue", "output", "shell"] {
        assert!(
            tools_source.contains(&format!("pub(crate) mod {owner};")),
            "tool owner `{owner}` must remain crate-private"
        );
    }
    for retired in [
        "bash",
        "edit",
        "edit_diff",
        "file_mutation_queue",
        "find",
        "grep",
        "ls",
        "path",
        "read",
        "truncate",
        "write",
    ] {
        assert!(
            !tools_source.contains(&format!("mod {retired};"))
                && !crate_root.join(format!("src/tools/{retired}.rs")).exists(),
            "retired flat tool module `{retired}` must stay absent"
        );
    }

    assert!(
        !fs::read_to_string(crate_root.join("src/lib.rs"))
            .expect("read crate root")
            .contains("mod plugins;")
            && !crate_root.join("src/plugins/mod.rs").exists()
            && !crate_root.join("src/plugins/capability.rs").exists()
            && !crate_root.join("src/plugins/contributions/mod.rs").exists(),
        "retired plugin capability/contribution module must stay absent"
    );
}

#[test]
fn root_reexports_are_removed_after_breaking_facade_migration() {
    let crate_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let lib_source = fs::read_to_string(crate_root.join("src/lib.rs"))
        .expect("coding-agent lib.rs should be readable");
    let before_api = lib_source
        .split("pub mod api {")
        .next()
        .expect("api module should exist");
    let before_api_lines = before_api.lines().collect::<Vec<_>>();

    let mut violations = Vec::new();
    for (index, line) in before_api_lines.iter().enumerate() {
        let trimmed = line.trim();
        if !trimmed.starts_with("pub use ") {
            continue;
        }
        violations.push(format!("{}: {}", index + 1, trimmed));
    }

    assert!(
        violations.is_empty(),
        "root reexports must stay removed; stable users import a categorized coding_agent::api path:\n{}",
        violations.join("\n")
    );
}

#[test]
fn coding_session_run_is_the_canonical_operation_dispatcher() {
    let crate_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let source = fs::read_to_string(crate_root.join("src/runtime/submission.rs"))
        .expect("operation submission owner should be readable");
    let run_body =
        function_body(&source, "pub async fn run(").expect("CodingAgentSession::run should exist");
    let internal_run_body = function_body(&source, "pub(crate) async fn run_internal(")
        .expect("CodingAgentSession::run_internal should exist");

    for required in [
        "self.run_internal(operation)",
        "CodingAgentPublicError::from",
    ] {
        assert!(
            run_body.contains(required),
            "CodingAgentSession::run should contain {required}"
        );
    }

    for required in [
        "into_internal(",
        "operation.descriptor().dispatch_mode",
        "OperationDispatchMode::Async",
        "OperationDispatchMode::SyncReadOnly",
        "OperationDispatchMode::SyncMutable",
        "run_operation(operation, submission).await",
        "run_sync_operation(operation, submission)",
        "run_sync_mut_operation(operation, submission)",
        "CodingAgentOperationOutcome::from_internal(outcome)",
    ] {
        assert!(
            internal_run_body.contains(required),
            "CodingAgentSession::run_internal should contain {required}"
        );
    }

    for forbidden in [
        ".prompt(",
        ".compact(",
        ".summarize_branch(",
        ".self_healing_edit_with_options(",
        ".invoke_agent(",
        ".invoke_team(",
        ".export_current(",
        ".export_current_html(",
        "CodingAgentOperationOutcome::Prompt(",
        "CodingAgentOperationOutcome::Compact(",
        "CodingAgentOperationOutcome::BranchSummary(",
        "CodingAgentOperationOutcome::SelfHealingEdit(",
        "CodingAgentOperationOutcome::AgentInvocation(",
        "CodingAgentOperationOutcome::AgentTeam(",
        "CodingAgentOperationOutcome::PluginLoad(",
        "CodingAgentOperationOutcome::DefaultAgentProfileChanged",
        "CodingAgentOperationOutcome::DelegationApproved",
        "CodingAgentOperationOutcome::DelegationRejected",
        "CodingAgentOperationOutcome::SessionForked",
        "CodingAgentOperationOutcome::ActiveLeafSwitched",
        "CodingAgentOperationOutcome::SessionTreeLabelChanged",
        "CodingAgentOperationOutcome::Export(",
        "CodingAgentOperationOutcome::ExportHtml(",
    ] {
        assert!(
            !internal_run_body.contains(forbidden),
            "CodingAgentSession::run_internal must not call compatibility workflow {forbidden}"
        );
    }
}

fn function_body<'a>(source: &'a str, signature: &str) -> Option<&'a str> {
    let signature_start = source.find(signature)?;
    let body_start = signature_start + source[signature_start..].find('{')?;
    let mut depth = 0usize;

    for (offset, character) in source[body_start..].char_indices() {
        match character {
            '{' => depth += 1,
            '}' => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some(&source[body_start + 1..body_start + offset]);
                }
            }
            _ => {}
        }
    }

    None
}

#[test]
fn stable_api_does_not_export_compatibility_event_receiver() {
    let crate_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let lib_source = fs::read_to_string(crate_root.join("src/lib.rs"))
        .expect("coding-agent lib.rs should be readable");
    let compatibility_receiver = ["CodingAgent", "EventReceiver"].concat();
    let api_module = lib_source
        .split("pub mod api {")
        .nth(1)
        .expect("api module should exist")
        .split("\n}\n\n#[cfg")
        .next()
        .expect("api module should end before test support");

    assert!(
        !api_module.contains(&compatibility_receiver),
        "stable api should export the product-event receiver instead of the compatibility receiver"
    );
}

#[test]
fn stable_api_excludes_internal_runtime_contracts() {
    let crate_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let lib_source = fs::read_to_string(crate_root.join("src/lib.rs"))
        .expect("coding-agent lib.rs should be readable");
    let api_module = module_body(&lib_source, "pub mod api")
        .expect("stable api module should have a balanced body");
    let exported_identifiers = api_module
        .split(|character: char| !character.is_ascii_alphanumeric() && character != '_')
        .filter(|identifier| !identifier.is_empty())
        .collect::<BTreeSet<_>>();

    for forbidden in [
        "Operation",
        "OperationDescriptor",
        "OperationExecution",
        "OperationDispatchMode",
        "PluginLoadOptions",
        "RuntimeService",
        "SessionService",
        "EventService",
        "PluginService",
        "PluginLoadService",
        "CapabilityService",
        "WorkflowService",
        "CodingSessionError",
        "CodingDiagnostic",
        "CodingDiagnosticSeverity",
        "ProfileDiagnostic",
        "AgentProfile",
        "TeamProfile",
        "ProfileRegistry",
        "ProfileRegistryOptions",
        "PluginRegistry",
        "Flow",
        "FlowNode",
        "FlowOutcome",
        "CompactCancellationHandle",
        "CompactCancellationRejection",
        "CancellationToken",
    ] {
        assert!(
            !exported_identifiers.contains(forbidden),
            "stable api must not re-export internal runtime contract {forbidden}"
        );
    }
}

#[test]
fn client_connection_is_stateful_but_not_a_dispatcher_or_service_escape_hatch() {
    let crate_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let source = fs::read_to_string(crate_root.join("src/runtime/client/projection.rs"))
        .expect("public projection source should be readable");
    let connection = source
        .split("pub struct CodingAgentClientConnection")
        .nth(1)
        .expect("public connection should exist")
        .split("pub struct CodingAgentReconnectReceiver")
        .next()
        .unwrap();
    assert!(connection.contains("coordinator: Arc<SnapshotCoordinator>"));
    assert!(connection.contains("prepare_client_submission("));
    assert!(connection.contains("CodingAgentPreparedSubmission"));
    for forbidden in [
        "pub async fn run(",
        "pub async fn submit(",
        "RuntimeService",
        "SessionService",
        "ProductEventReceiver",
    ] {
        assert!(
            !connection.contains(forbidden),
            "connection leaked {forbidden}"
        );
    }
}

#[test]
fn public_lifecycle_values_are_curated_without_authority_leaks() {
    let crate_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let lib_source = fs::read_to_string(crate_root.join("src/lib.rs"))
        .expect("coding-agent lib.rs should be readable");
    let projection = fs::read_to_string(crate_root.join("src/runtime/client/projection.rs"))
        .expect("public projection should be readable");
    let errors = fs::read_to_string(crate_root.join("src/runtime/error.rs"))
        .expect("coding session errors should be readable");
    let api_module = module_body(&lib_source, "pub mod api")
        .expect("stable api module should have a balanced body");
    let exported_identifiers = api_module
        .split(|character: char| !character.is_ascii_alphanumeric() && character != '_')
        .filter(|identifier| !identifier.is_empty())
        .collect::<BTreeSet<_>>();

    for required in [
        "CodingAgentOperationTask",
        "CodingAgentDetachOutcome",
        "CodingAgentShutdownOutcome",
        "CodingAgentLifecycleRejection",
        "CodingAgentSubmittedEventDurability",
        "CodingAgentOutcomeAcknowledgementId",
        "CodingAgentSubmittedTerminalAnchor",
        "CodingAgentTerminalUncertainty",
    ] {
        assert!(
            exported_identifiers.contains(required),
            "stable api omitted adjacent lifecycle value {required}"
        );
    }

    for forbidden in [
        "SnapshotCoordinator",
        "ClientHandle",
        "ClientGeneration",
        "EventService",
        "ProductEventReceiver",
        "OperationControl",
        "Sender",
        "Receiver",
        "HashMap",
        "BTreeMap",
        "VecDeque",
        "LifecycleEpoch",
        "ReceiptSignature",
    ] {
        assert!(
            !exported_identifiers.contains(forbidden),
            "stable lifecycle api leaked internal authority {forbidden}"
        );
    }

    let acknowledgement = projection
        .split("pub struct CodingAgentOutcomeAcknowledgementId")
        .nth(1)
        .expect("opaque outcome acknowledgement should exist")
        .split("pub enum CodingAgentTerminalUncertainty")
        .next()
        .expect("terminal uncertainty should follow acknowledgement");
    assert!(acknowledgement.starts_with("(String);"));
    assert!(!acknowledgement.contains("pub fn new("));
    assert!(!acknowledgement.contains("pub fn generation("));
    assert!(!acknowledgement.contains("pub fn signature("));

    for source in [&projection, &errors] {
        for forbidden in ["format!(\"{:?}\"", "format!(\"{:#?}\""] {
            assert!(
                !source.contains(forbidden),
                "stable lifecycle identity/code must not use Debug formatting: {forbidden}"
            );
        }
    }

    for stable_code in [
        "\"detached\"",
        "\"stale_generation\"",
        "\"runtime_shut_down\"",
    ] {
        assert!(
            errors.contains(stable_code),
            "lifecycle rejection omitted explicit stable code {stable_code}"
        );
    }

    let connection = projection
        .split("impl CodingAgentClientConnection")
        .nth(1)
        .expect("public connection implementation should exist")
        .split("pub struct CodingAgentReconnectReceiver")
        .next()
        .expect("reconnect receiver should follow connection");
    for forbidden in [
        "pub async fn run(",
        "pub async fn submit(",
        "pub fn dispatch(",
        "pub fn detach_client(",
        "pub fn shutdown_client(",
        "compact_cancellation",
        "cancel_operation",
        "operation_generation",
    ] {
        assert!(
            !connection.contains(forbidden),
            "connection leaked lifecycle/operation authority through {forbidden}"
        );
    }
}

#[test]
fn public_lifecycle_connection_derives_detach_authority_from_self() {
    let crate_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let projection = fs::read_to_string(crate_root.join("src/runtime/client/projection.rs"))
        .expect("public projection should be readable");
    let connection = projection
        .split("impl CodingAgentClientConnection")
        .nth(1)
        .expect("public connection implementation should exist")
        .split("pub struct CodingAgentReconnectReceiver")
        .next()
        .unwrap();

    assert!(connection.contains(
        "pub fn detach(&self) -> Result<CodingAgentDetachOutcome, CodingAgentPublicError>"
    ));
    assert!(connection.contains(".detach(&self.handle())"));
    assert!(!connection.contains("pub fn detach_client("));
    assert!(!connection.contains("pub fn detach_generation("));
}

fn module_body<'a>(source: &'a str, declaration: &str) -> Option<&'a str> {
    let declaration_start = source.find(declaration)?;
    let body_start = declaration_start + source[declaration_start..].find('{')?;
    let mut depth = 0usize;

    for (offset, character) in source[body_start..].char_indices() {
        match character {
            '{' => depth += 1,
            '}' => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some(&source[body_start + 1..body_start + offset]);
                }
            }
            _ => {}
        }
    }

    None
}
