const AGENT_INVOCATION_RUNNER: &str =
    include_str!("../../src/operations/agent_invocation/runner.rs");
const AGENT_TEAM_RUNNER: &str = include_str!("../../src/operations/team_invocation/runner.rs");
const BRANCH_SUMMARY_SERVICE: &str = include_str!("../../src/operations/branch_summary/mod.rs");
const MANUAL_COMPACTION_SERVICE: &str = include_str!("../../src/operations/compaction/mod.rs");
const PROMPT_CONTEXT: &str = include_str!("../../src/operations/prompt/context.rs");
const PROMPT_EXECUTION: &str = include_str!("../../src/operations/prompt/mod.rs");
const INTERACTIVE_EVENT_BRIDGE: &str = include_str!("../../../cli/src/interactive/event_bridge.rs");
const INTERACTIVE_LOOP: &str = include_str!("../../../cli/src/interactive/loop.rs");
const INTERACTIVE_ROOT: &str = include_str!("../../../cli/src/interactive/root.rs");
const SESSION_SERVICE: &str = include_str!("../../src/session/service.rs");
const SESSION_FACADE_SERVICE: &str = include_str!("../../src/services/session.rs");
const PUBLIC_EVENT: &str = include_str!("../../src/events/mod.rs");
const AGENT_EVENT: &str = include_str!("../../src/events/agent.rs");
const CAPABILITY_EVENT: &str = include_str!("../../src/events/capability.rs");
const DIAGNOSTIC_EVENT: &str = include_str!("../../src/events/diagnostic.rs");
const DELEGATION_EVENT: &str = include_str!("../../src/events/delegation.rs");
const MESSAGE_EVENT: &str = include_str!("../../src/events/message.rs");
const PROMPT_EVENT: &str = include_str!("../../src/events/prompt.rs");
const PROFILE_EVENT: &str = include_str!("../../src/events/profile.rs");
const SESSION_EVENT: &str = include_str!("../../src/events/session.rs");
const TEAM_EVENT: &str = include_str!("../../src/events/team.rs");
const TOOL_EVENT: &str = include_str!("../../src/events/tool.rs");
const RUNTIME_EVENT: &str = include_str!("../../src/events/runtime.rs");
const RECOVERY_EVENT: &str = include_str!("../../src/events/recovery.rs");
const WORKFLOW_EVENT: &str = include_str!("../../src/events/workflow.rs");
const PUBLIC_PROJECTION: &str = include_str!("../../src/runtime/client/projection.rs");
const PRODUCT_CLIENT_PROJECTION: &str =
    include_str!("../../src/runtime/client/product_projection.rs");
const PUBLIC_OPERATION: &str = include_str!("../../src/runtime/outcome.rs");
const PRODUCT_EVENT_CONTRACT: &str = include_str!("../../../../docs/product-event-contract.md");
const CRATE_ROOT: &str = include_str!("../../src/lib.rs");
const INTERACTIVE_EVENT_TESTS: &str =
    include_str!("../../../cli/src/interactive/event_bridge_tests.rs");

#[test]
fn typed_public_event_boundary_is_fail_closed() {
    let documented_events = region(
        PRODUCT_EVENT_CONTRACT,
        "<!-- product-event-inventory:start -->",
        "<!-- product-event-inventory:end -->",
    );
    assert_eq!(
        documented_events
            .lines()
            .filter(|line| line.starts_with("| `"))
            .count(),
        52,
        "the authoritative product-event inventory must contain 52 rows"
    );
    for forbidden in ["format!(\"{:?}\"", "format!(\"{:#?}\""] {
        assert!(
            !PUBLIC_EVENT.contains(forbidden) && !PUBLIC_PROJECTION.contains(forbidden),
            "public event identity must not be derived through Debug formatting: {forbidden}"
        );
    }

    for line in PUBLIC_EVENT.lines().filter(|line| line.contains("pub ")) {
        assert!(
            !line.contains("CodingAgentEvent"),
            "public event declaration leaks the compatibility event: {line}"
        );
    }

    assert!(
        !PUBLIC_EVENT.contains("CodingAgentEvent")
            && !WORKFLOW_EVENT.contains("CodingAgentEvent")
            && !RECOVERY_EVENT.contains("CodingAgentEvent"),
        "the deleted centralized compatibility event must not return"
    );

    for family in [
        "Session",
        "Profile",
        "Agent",
        "Team",
        "Message",
        "Tool",
        "Runtime",
        "Delegation",
        "Workflow",
        "Diagnostic",
        "Capability",
    ] {
        assert!(
            PRODUCT_EVENT_CONTRACT.contains(&format!("| {family} |")),
            "product-event contract omits family {family}"
        );
    }
}

#[test]
fn full_event_inventory_is_source_fixture_and_document_complete() {
    let source = owner_event_variants();
    let fixture = fixture_event_variants(region(
        PUBLIC_EVENT,
        "// product-event-fixture:start",
        "// product-event-fixture:end",
    ));
    let expected = public_inventory_rows(region(
        PUBLIC_EVENT,
        "// product-event-inventory:start",
        "// product-event-inventory:end",
    ));
    let documented = documented_inventory_rows(region(
        PRODUCT_EVENT_CONTRACT,
        "<!-- product-event-inventory:start -->",
        "<!-- product-event-inventory:end -->",
    ));

    assert_eq!(
        source.len(),
        52,
        "owner event types must contain 52 normalized variants"
    );
    assert_eq!(fixture.len(), 52, "fixture must construct 52 variants");
    assert_eq!(expected.len(), 52, "test inventory must contain 52 rows");
    assert_eq!(
        documented.len(),
        52,
        "document inventory must contain 52 rows"
    );
    assert_eq!(source, fixture, "fixture drifted from owner event types");
    assert_eq!(
        expected, documented,
        "documented product-event inventory drifted from the executable inventory"
    );
    let expected_variants: std::collections::BTreeSet<_> = expected
        .iter()
        .map(|(variant, _, _)| variant.clone())
        .collect();
    assert_eq!(
        source, expected_variants,
        "inventory omitted or added an internal variant"
    );
}

#[test]
fn operation_outcome_documentation_matches_public_enums_exactly() {
    let operations = enum_variants(PUBLIC_OPERATION, "CodingAgentOperation");
    let outcomes = enum_variants(PUBLIC_OPERATION, "CodingAgentOperationOutcome");
    let matrix = region(
        PRODUCT_EVENT_CONTRACT,
        "<!-- operation-outcome-matrix:start -->",
        "<!-- operation-outcome-matrix:end -->",
    );
    let rows: Vec<_> = matrix
        .lines()
        .filter(|line| line.starts_with("| `"))
        .map(|line| {
            let columns: Vec<_> = line.split('|').map(str::trim).collect();
            (
                columns[1].trim_matches('`').to_owned(),
                columns[2].trim_matches('`').to_owned(),
            )
        })
        .collect();

    assert_eq!(
        rows.len(),
        operations.len(),
        "one matrix row is required per operation"
    );
    assert_eq!(
        rows.len(),
        outcomes.len(),
        "one matrix row is required per outcome"
    );
    let documented_operations: std::collections::BTreeSet<_> = rows
        .iter()
        .map(|(operation, _)| operation.clone())
        .collect();
    let documented_outcomes: std::collections::BTreeSet<_> =
        rows.iter().map(|(_, outcome)| outcome.clone()).collect();
    assert_eq!(
        documented_operations.len(),
        rows.len(),
        "operation rows must be unique"
    );
    assert_eq!(
        documented_outcomes.len(),
        rows.len(),
        "outcome rows must be unique"
    );
    assert_eq!(documented_operations, operations.into_iter().collect());
    assert_eq!(documented_outcomes, outcomes.into_iter().collect());
}

fn region<'a>(source: &'a str, start: &str, end: &str) -> &'a str {
    let start = source
        .find(start)
        .unwrap_or_else(|| panic!("missing region start: {start}"));
    let rest = &source[start..];
    let end = rest
        .find(end)
        .unwrap_or_else(|| panic!("missing region end: {end}"));
    &rest[..end]
}

fn enum_variants(source: &str, enum_name: &str) -> std::collections::BTreeSet<String> {
    let declaration = [
        format!("pub enum {enum_name} {{"),
        format!("pub(crate) enum {enum_name} {{"),
    ]
    .into_iter()
    .find(|declaration| source.contains(declaration))
    .unwrap_or_else(|| panic!("missing {enum_name}"));
    let start = source.find(&declaration).expect("declaration was found");
    let body = &source[start + declaration.len()..];
    let mut depth = 1_i32;
    let mut variants = std::collections::BTreeSet::new();
    for line in body.lines() {
        if depth == 1 {
            let trimmed = line.trim();
            let candidate = trimmed
                .split(['(', ' ', '{', ','])
                .next()
                .unwrap_or_default();
            if candidate.chars().next().is_some_and(char::is_uppercase) {
                variants.insert(candidate.to_owned());
            }
        }
        depth += line.matches('{').count() as i32;
        depth -= line.matches('}').count() as i32;
        if depth == 0 {
            break;
        }
    }
    assert!(
        !variants.is_empty(),
        "{enum_name} inventory must not be empty"
    );
    variants
}

fn owner_event_variants() -> std::collections::BTreeSet<String> {
    let mut variants = std::collections::BTreeSet::from([
        "SessionCompactionCompleted".to_owned(),
        "OperationRecovered".to_owned(),
    ]);
    variants.extend(
        enum_variants(SESSION_EVENT, "SessionWriteEvent")
            .into_iter()
            .map(|variant| format!("SessionWrite{variant}")),
    );
    variants.extend(
        enum_variants(PROMPT_EVENT, "PromptEvent")
            .into_iter()
            .map(|variant| format!("Prompt{variant}")),
    );
    variants.extend(
        enum_variants(SESSION_EVENT, "SessionLifecycleEvent")
            .into_iter()
            .map(|variant| format!("Session{variant}")),
    );
    variants.extend(
        enum_variants(PROFILE_EVENT, "ProfileEvent")
            .into_iter()
            .map(|variant| match variant.as_str() {
                "DefaultChanged" => "DefaultAgentProfileChanged".to_owned(),
                _ => format!("Profile{variant}"),
            }),
    );
    variants.extend(enum_variants(DIAGNOSTIC_EVENT, "DiagnosticEvent"));
    variants.extend(
        enum_variants(CAPABILITY_EVENT, "CapabilityEvent")
            .into_iter()
            .map(|variant| format!("Capability{variant}")),
    );
    variants.extend(
        enum_variants(AGENT_EVENT, "AgentInvocationEvent")
            .into_iter()
            .map(|variant| format!("AgentInvocation{variant}")),
    );
    variants.extend(
        enum_variants(TEAM_EVENT, "TeamEvent")
            .into_iter()
            .map(|variant| format!("AgentTeam{variant}")),
    );
    variants.extend(
        enum_variants(AGENT_EVENT, "AgentStreamEvent")
            .into_iter()
            .map(|variant| match variant.as_str() {
                "TurnStarted" => "AgentTurnStarted".to_owned(),
                _ => variant,
            }),
    );
    variants.extend(
        enum_variants(MESSAGE_EVENT, "MessageEvent")
            .into_iter()
            .map(|variant| format!("AssistantMessage{variant}"))
            .map(|variant| {
                variant.replace("AssistantMessageThinkingDelta", "AssistantThinkingDelta")
            }),
    );
    variants.extend(
        enum_variants(TOOL_EVENT, "ToolEvent")
            .into_iter()
            .map(|variant| format!("ToolCall{variant}")),
    );
    variants.extend(
        enum_variants(DELEGATION_EVENT, "DelegationEvent")
            .into_iter()
            .map(|variant| format!("Delegation{variant}")),
    );
    variants.extend(
        enum_variants(RUNTIME_EVENT, "RuntimeEvent")
            .into_iter()
            .map(|variant| format!("Runtime{variant}")),
    );
    variants.extend(
        enum_variants(WORKFLOW_EVENT, "SelfHealingEditEvent")
            .into_iter()
            .map(|variant| format!("SelfHealingEdit{variant}")),
    );
    variants
}

fn fixture_event_variants(source: &str) -> std::collections::BTreeSet<String> {
    let mut variants = std::collections::BTreeSet::new();
    for line in source.lines() {
        let mut rest = line;
        let Some((start, prefix)) = [
            "SessionWriteEvent::",
            "SessionCompactionEvent",
            "PromptEvent::",
            "SessionLifecycleEvent::",
            "ProfileEvent::",
            "DiagnosticEvent::",
            "CapabilityEvent::",
            "AgentInvocationEvent::",
            "TeamEvent::",
            "AgentStreamEvent::",
            "MessageEvent::",
            "ToolEvent::",
            "DelegationEvent::",
            "RuntimeEvent::",
            "SelfHealingEditEvent::",
            "RecoveryEvent",
        ]
        .into_iter()
        .filter_map(|prefix| rest.find(prefix).map(|start| (start, prefix)))
        .min_by_key(|(start, _)| *start) else {
            continue;
        };
        if prefix == "SessionCompactionEvent" {
            variants.insert("SessionCompactionCompleted".to_owned());
            continue;
        }
        if prefix == "RecoveryEvent" {
            variants.insert("OperationRecovered".to_owned());
            continue;
        }
        rest = &rest[start + prefix.len()..];
        let name = rest
            .split(['{', '(', ',', ' ', '\n'])
            .next()
            .unwrap_or_default();
        if !name.is_empty() && name.chars().next().is_some_and(char::is_uppercase) {
            let name = match prefix {
                "SessionWriteEvent::" => format!("SessionWrite{name}"),
                "PromptEvent::" => format!("Prompt{name}"),
                "SessionLifecycleEvent::" => format!("Session{name}"),
                "ProfileEvent::" if name == "DefaultChanged" => {
                    "DefaultAgentProfileChanged".to_owned()
                }
                "ProfileEvent::" => format!("Profile{name}"),
                "CapabilityEvent::" => format!("Capability{name}"),
                "AgentInvocationEvent::" => format!("AgentInvocation{name}"),
                "TeamEvent::" => format!("AgentTeam{name}"),
                "AgentStreamEvent::" if name == "TurnStarted" => "AgentTurnStarted".to_owned(),
                "AgentStreamEvent::" => name.to_owned(),
                "MessageEvent::" if name == "ThinkingDelta" => "AssistantThinkingDelta".to_owned(),
                "MessageEvent::" => format!("AssistantMessage{name}"),
                "ToolEvent::" => format!("ToolCall{name}"),
                "DelegationEvent::" => format!("Delegation{name}"),
                "RuntimeEvent::" => format!("Runtime{name}"),
                "SelfHealingEditEvent::" => format!("SelfHealingEdit{name}"),
                _ => name.to_owned(),
            };
            variants.insert(name);
        }
    }
    variants
}

fn public_inventory_rows(source: &str) -> std::collections::BTreeSet<(String, String, String)> {
    let mut strings = Vec::new();
    let mut families = Vec::new();
    for line in source.lines() {
        let quoted: Vec<_> = line.split('"').collect();
        if quoted.len() >= 3 {
            strings.push(quoted[1].to_owned());
        }
        if let Some(family) = line
            .split("CodingAgentProductEventFamily::")
            .nth(1)
            .and_then(|value| value.split([',', ' ', '\n']).next())
        {
            families.push(family.to_ascii_lowercase());
        }
    }
    assert_eq!(
        strings.len(),
        families.len() * 2,
        "malformed executable inventory"
    );
    families
        .into_iter()
        .enumerate()
        .map(|(index, family)| {
            (
                strings[index * 2].clone(),
                family,
                strings[index * 2 + 1].clone(),
            )
        })
        .collect()
}

fn documented_inventory_rows(source: &str) -> std::collections::BTreeSet<(String, String, String)> {
    source
        .lines()
        .filter(|line| line.starts_with("| `"))
        .map(|line| {
            let columns: Vec<_> = line.split('|').map(str::trim).collect();
            assert_eq!(
                columns.len(),
                5,
                "malformed documented inventory row: {line}"
            );
            (
                columns[1].trim_matches('`').to_owned(),
                columns[2].trim_matches('`').to_owned(),
                columns[3].trim_matches('`').to_owned(),
            )
        })
        .collect()
}

#[test]
fn workflow_flows_emit_diagnostics_through_event_service_helpers() {
    for (name, source) in [
        ("agent_invocation_runner", AGENT_INVOCATION_RUNNER),
        ("agent_team_runner", AGENT_TEAM_RUNNER),
    ] {
        assert!(
            !source.contains("self.event_service.emit(CodingAgentEvent::Diagnostic"),
            "{name} constructs diagnostic events directly instead of using EventService::emit_diagnostic"
        );
    }
}

#[test]
fn nested_workflows_use_explicit_typed_runners() {
    for (name, source) in [
        ("agent_invocation_runner", AGENT_INVOCATION_RUNNER),
        ("agent_team_runner", AGENT_TEAM_RUNNER),
    ] {
        assert!(
            source.contains("PromptTurnRunner::new()?.run_typed"),
            "{name} should invoke PromptTurnRunner directly for nested prompt subflows"
        );
        assert!(
            !source.contains("WorkflowService"),
            "{name} must not reference the removed WorkflowService"
        );
    }

    let delegation_execution = include_str!("../../src/operations/delegation/execution.rs");
    assert!(delegation_execution.contains("OperationScheduler::admit_child"));
    assert!(!delegation_execution.contains("WorkflowService"));
    assert!(
        delegation_execution.contains("AgentInvocationRunner::new()")
            && delegation_execution.contains("AgentTeamRunner::new()"),
        "delegation execution should invoke typed runners directly"
    );

    for (name, source) in [
        ("agent_invocation_runner", AGENT_INVOCATION_RUNNER),
        ("agent_team_runner", AGENT_TEAM_RUNNER),
    ] {
        for needle in [
            "PromptTurnFlow::new()?.run",
            "AgentInvocationFlow::new()?.run",
            "AgentTeamFlow::new()?.run",
            "WorkflowService::new()",
        ] {
            assert!(
                !source.contains(needle),
                "{name} should route nested workflow execution through direct typed runners instead of `{needle}`"
            );
        }
    }
}

#[test]
fn prompt_context_records_flow_completion_as_state_not_as_an_event() {
    assert!(
        PROMPT_CONTEXT.contains("completion_recorded: bool")
            && PROMPT_CONTEXT.contains("self.completion_recorded = true"),
        "PromptTurnContext should own explicit idempotent Flow completion state"
    );
    assert!(
        !PROMPT_CONTEXT.contains("CodingAgentEvent::PromptCompleted")
            && !PROMPT_CONTEXT.contains("prompt_completed_event"),
        "PromptTurnContext must not cache a Prompt terminal event that EventService regenerates"
    );
}

#[test]
fn session_service_builds_session_write_events_through_event_service_helpers() {
    let production_source = SESSION_SERVICE
        .split("\n#[cfg(test)]\nmod tests")
        .next()
        .expect("session_service source should be present");

    assert!(
        !production_source.contains("CodingAgentEvent::SessionWrite"),
        "SessionService should build session-write events through EventService helpers"
    );
}

#[test]
fn workflow_flows_emit_prompt_outcomes_through_event_service_helpers() {
    for (name, source) in [
        ("agent_invocation_runner", AGENT_INVOCATION_RUNNER),
        ("agent_team_runner", AGENT_TEAM_RUNNER),
    ] {
        for event_name in ["PromptCompleted", "PromptAborted", "PromptFailed"] {
            let needle = format!("self.event_service.emit(CodingAgentEvent::{event_name}");
            assert!(
                !source.contains(&needle),
                "{name} constructs {event_name} directly instead of using EventService prompt outcome helpers"
            );
        }
    }
}

#[test]
fn manual_compaction_prompt_outcomes_are_built_by_flow_boundary() {
    assert!(
        MANUAL_COMPACTION_SERVICE.contains("manual_compaction_success_outcome")
            && MANUAL_COMPACTION_SERVICE.contains("manual_compaction_failed_outcome"),
        "ManualCompactionService should delegate manual compaction outcome construction to flow-boundary helpers"
    );

    for variant in ["PromptTurnOutcome::Success", "PromptTurnOutcome::Failed"] {
        assert!(
            !MANUAL_COMPACTION_SERVICE.contains(variant),
            "ManualCompactionService should delegate manual compaction outcome construction to the flow boundary instead of building {variant} inline"
        );
    }
}

#[test]
fn branch_summary_prompt_outcomes_are_built_by_flow_boundary() {
    assert!(
        BRANCH_SUMMARY_SERVICE.contains("branch_summary_success_outcome")
            && BRANCH_SUMMARY_SERVICE.contains("branch_summary_failed_outcome"),
        "BranchSummaryService should delegate branch-summary outcome construction to flow-boundary helpers"
    );

    for variant in ["PromptTurnOutcome::Success", "PromptTurnOutcome::Failed"] {
        assert!(
            !BRANCH_SUMMARY_SERVICE.contains(variant),
            "BranchSummaryService should delegate outcome construction to the branch summary flow boundary instead of building {variant} inline"
        );
    }
}

#[test]
fn owner_delegates_prompt_transaction_finalization_to_services() {
    let owner_impl = PROMPT_EXECUTION
        .split("impl PromptOperation<'_> {")
        .nth(1)
        .expect("prompt execution owner impl should be present");
    let finalize_region = owner_impl
        .split("    fn finalize_prompt_transaction(")
        .nth(1)
        .expect("owner finalize_prompt_transaction should be present");

    for variant in [
        "PromptTurnOutcome::Success",
        "PromptTurnOutcome::Aborted",
        "PromptTurnOutcome::Failed",
    ] {
        assert!(
            !finalize_region.contains(variant),
            "PromptOperation::finalize_prompt_transaction should delegate {variant} handling to session/transient services"
        );
    }
}

#[test]
fn owner_does_not_rebuild_prompt_success_session_write_metadata() {
    let owner_helpers = SESSION_FACADE_SERVICE
        .split("fn apply_finalized_session_write(")
        .nth(1)
        .expect("apply_finalized_session_write helper should be present");

    assert!(
        !owner_helpers.contains("PromptTurnOutcome::Success"),
        "CodingAgentSession owner should delegate prompt success session/leaf metadata updates to PromptTurnOutcome helpers"
    );
    for caller in [
        PROMPT_EXECUTION,
        MANUAL_COMPACTION_SERVICE,
        BRANCH_SUMMARY_SERVICE,
    ] {
        assert!(caller.contains("apply_finalized_session_write("));
    }
}

#[test]
fn prompt_operation_uses_outcome_helper_for_success_branching() {
    let prompt_inner = PROMPT_EXECUTION
        .split("async fn run_inner(")
        .nth(1)
        .expect("prompt operation run_inner should be present")
        .split("    async fn execute_authorized_delegations(")
        .next()
        .expect("delegation execution should follow prompt_inner");

    assert!(
        !prompt_inner.contains("PromptTurnOutcome::Success"),
        "prompt operation should ask PromptTurnOutcome helpers about success state instead of matching the success variant inline"
    );
}

#[test]
fn interactive_projection_consumes_product_events() {
    assert!(
        INTERACTIVE_EVENT_BRIDGE.contains("UiProjection"),
        "interactive projection should use UiProjection"
    );
    assert!(
        INTERACTIVE_EVENT_BRIDGE.contains("product: Option<CodingAgentClientProjection>"),
        "interactive projection should compose the shared product reducer"
    );
    assert!(
        INTERACTIVE_EVENT_BRIDGE
            .contains("/tests/fixtures/client_projection/cross-adapter-events.json"),
        "interactive projection must consume the shared cross-adapter fixture"
    );
    assert!(
        INTERACTIVE_EVENT_BRIDGE
            .contains("shared_cross_adapter_fixture_matches_interactive_product_state_exactly"),
        "interactive projection must compare its complete product state with the shared reducer"
    );
    assert!(
        INTERACTIVE_EVENT_BRIDGE.contains("#[cfg(test)]\nmod cross_adapter_tests")
            && INTERACTIVE_EVENT_BRIDGE
                .contains("shared_cross_adapter_fixture_matches_interactive_product_state_exactly"),
        "interactive cross-adapter equality must execute as an enabled cli test"
    );
    assert_eq!(
        INTERACTIVE_EVENT_BRIDGE
            .matches("shared_cross_adapter_fixture_matches_interactive_product_state_exactly")
            .count(),
        1,
        "disabled legacy tests must not duplicate the cross-adapter evidence"
    );
    for required_matrix in [
        "live_event_bounds_matrix_caps_every_retained_collection_and_payload",
        "snapshot_and_bootstrap_bounds_matrix_caps_top_level_and_transcript_state",
        "invalid_event_matrix_preserves_all_retained_product_facts",
        "narrow_replacement_atomicity_matrix_rejects_without_partial_mutation",
    ] {
        assert!(
            PRODUCT_CLIENT_PROJECTION.contains(required_matrix),
            "shared projection closure must retain matrix `{required_matrix}`"
        );
    }
    assert!(
        INTERACTIVE_EVENT_BRIDGE.contains("push_product_event"),
        "interactive projection should consume product events through UiProjection"
    );
    assert!(
        INTERACTIVE_ROOT.contains("shared_projection: UiProjection"),
        "interactive root should own the sole ordered UiProjection"
    );
    assert!(
        INTERACTIVE_ROOT.contains("self.shared_projection = UiProjection::from_snapshot(snapshot)"),
        "interactive root should reset projection state from CodingAgentSnapshot"
    );
    assert!(
        INTERACTIVE_EVENT_BRIDGE.contains("context: CodingAgentContextSnapshot")
            && !INTERACTIVE_EVENT_BRIDGE.contains("context: UiContextProjection"),
        "interactive projection must retain public context facts rather than the legacy UI context DTO"
    );
    assert!(
        INTERACTIVE_ROOT.contains("self.shared_projection.apply_product_event(event)"),
        "interactive root should consume ProductEvent through its UiProjection"
    );
    assert!(
        !INTERACTIVE_LOOP.contains("let mut ui_projection"),
        "interactive loop must not maintain a second projection owner"
    );
    for forbidden in [
        "\n    last_sequence: ProductEventSequence",
        "\n    session: Option<CodingAgentSessionView>",
        "\n    capabilities: Option<crate::runtime::facade::CodingAgentCapabilities>",
        "CodingAgentProfileProductEvent::DefaultChanged",
    ] {
        assert!(
            !INTERACTIVE_EVENT_BRIDGE.contains(forbidden),
            "interactive projection must not reintroduce product-state mirror `{forbidden}`"
        );
    }
}

#[test]
fn stable_facade_and_interactive_adapter_reject_raw_event_projection() {
    let stable_api = region(
        CRATE_ROOT,
        "pub mod api {",
        "#[cfg(any(test, feature = \"test-support\"))]",
    );
    assert!(
        !stable_api.contains("CodingAgentEvent"),
        "the stable facade must not export the private raw admission event"
    );
    assert!(
        !INTERACTIVE_EVENT_BRIDGE.contains("pub fn handle(&mut self, event: &CodingAgentEvent)"),
        "the interactive bridge must not expose a public raw-event projection method"
    );
    assert!(
        !INTERACTIVE_EVENT_TESTS.contains("CodingAgentEvent"),
        "interactive adapter behavior tests must enter through typed product-event fixtures"
    );
}

fn workspace_path(relative: &str) -> std::path::PathBuf {
    let crate_root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repo_root = crate_root
        .parent()
        .and_then(std::path::Path::parent)
        .expect("crate should live under crates/coding-agent")
        .to_path_buf();
    repo_root.join(relative)
}

#[test]
fn first_party_code_does_not_consume_compatibility_event_subscription() {
    let scan_roots = [
        "crates/cli/src/protocol",
        "crates/cli/src/interactive",
        "crates/coding-agent/tests",
    ];
    let repo_root = workspace_path("");
    let allowed = ["crates/coding-agent/tests/events_snapshot/event_boundary_guards.rs"];
    let mut violations = Vec::new();

    for root in scan_roots {
        collect_source_violations(
            &repo_root,
            &repo_root.join(root),
            &allowed,
            &mut violations,
            |line| line.contains(".subscribe()") || line.contains("CodingAgentEventReceiver"),
        );
    }

    assert!(
        violations.is_empty(),
        "first-party code should consume ProductEvent or public product-event facades instead of compatibility CodingAgentEventReceiver:\n{}",
        violations.join("\n")
    );
}

#[test]
fn legacy_receiver_and_duplicate_broadcast_are_absent() {
    let session_source =
        std::fs::read_to_string(workspace_path("crates/coding-agent/src/runtime/facade.rs"))
            .expect("read coding session owner");
    let connection_source = std::fs::read_to_string(workspace_path(
        "crates/coding-agent/src/runtime/facade/connection.rs",
    ))
    .expect("read coding session connection owner");
    let owner_source = format!("{session_source}\n{connection_source}");
    let event_service_source =
        std::fs::read_to_string(workspace_path("crates/coding-agent/src/services/event.rs"))
            .expect("read event service");

    let owner_forbidden = [
        "pub use event_service::CodingAgentEventReceiver",
        "pub fn subscribe(&self) -> CodingAgentEventReceiver",
        "use subscribe_product_events_public instead",
        "#[allow(deprecated)]\n    pub fn subscribe(",
    ];
    let event_service_forbidden = [
        "struct CodingAgentEventReceiver",
        "impl CodingAgentEventReceiver",
        "pub(crate) fn subscribe(&self)",
        "Sender<CodingAgentEvent>",
        ".sender\n            .send(",
        "use ProductEventReceiver instead",
        "#[allow(deprecated)]\nmod tests",
    ];

    for forbidden in owner_forbidden {
        assert!(
            !owner_source.contains(forbidden),
            "coding session owner reintroduced legacy receiver/subscription fragment: {forbidden}"
        );
    }
    for forbidden in event_service_forbidden {
        assert!(
            !event_service_source.contains(forbidden),
            "EventService reintroduced legacy receiver/duplicate broadcast fragment: {forbidden}"
        );
    }

    assert!(owner_source.contains("pub fn subscribe_product_events_public(&self)"));
    assert!(event_service_source.contains("broadcast::Sender<ProductEvent>"));
    assert!(event_service_source.contains("self.product_sender.send(product_event.clone())"));
    assert!(event_service_source.contains("retained_product_events.push_back(event)"));
}

#[test]
fn production_event_runtime_has_no_raw_compatibility_storage_or_transport() {
    let repo_root = workspace_path("");
    let scan_roots = [
        "crates/coding-agent/src/runtime",
        "crates/cli/src/protocol",
        "crates/cli/src/interactive",
        "crates/coding-agent/src/lib.rs",
    ];
    let forbidden = [
        ["compatibility", "_event"].concat(),
        "CodingAgentEventReceiver".to_owned(),
        "Sender<CodingAgentEvent>".to_owned(),
        "Receiver<CodingAgentEvent>".to_owned(),
        "broadcast::channel::<CodingAgentEvent>".to_owned(),
        ["from_compat", "_event"].concat(),
    ];
    let mut violations = Vec::new();

    for root in scan_roots {
        collect_source_violations(
            &repo_root,
            &repo_root.join(root),
            &["crates/cli/src/protocol/events_tests.rs"],
            &mut violations,
            |line| forbidden.iter().any(|needle| line.contains(needle)),
        );
    }

    assert!(
        violations.is_empty(),
        "production event code reintroduced raw compatibility storage, accessors, receivers, broadcasts, or conversions:\n{}",
        violations.join("\n")
    );

    let public_event_source =
        std::fs::read_to_string(repo_root.join("crates/coding-agent/src/events/mod.rs"))
            .expect("read public event source");
    let client_projection_source = std::fs::read_to_string(
        repo_root.join("crates/coding-agent/src/runtime/client/projection.rs"),
    )
    .expect("read client projection source");
    let snapshot_source =
        std::fs::read_to_string(repo_root.join("crates/coding-agent/src/runtime/snapshot.rs"))
            .expect("read snapshot source");
    let outcome_source =
        std::fs::read_to_string(repo_root.join("crates/coding-agent/src/runtime/outcome.rs"))
            .expect("read operation outcome source");

    assert!(public_event_source.contains("pub struct CodingAgentProductEvent {"));
    assert!(public_event_source.contains("pub enum CodingAgentProductEventDeliveryClass {"));
    assert!(public_event_source.contains("pub fn delivery_class(&self)"));
    assert!(public_event_source.contains("pub(crate) type ProductEvent = CodingAgentProductEvent;"));
    assert!(public_event_source.contains("pub(crate) fn from_draft_for_tests("));
    assert!(!repo_root
        .join("crates/coding-agent/src/events/internal.rs")
        .exists());
    assert!(SESSION_EVENT.contains("pub(crate) enum SessionWriteEvent {"));
    assert!(SESSION_EVENT.contains("pub(crate) enum SessionLifecycleEvent {"));
    assert!(PROMPT_EVENT.contains("pub(crate) enum PromptEvent {"));
    assert!(PROFILE_EVENT.contains("pub(crate) enum ProfileEvent {"));
    assert!(DIAGNOSTIC_EVENT.contains("pub(crate) enum DiagnosticEvent {"));
    assert!(CAPABILITY_EVENT.contains("pub(crate) enum CapabilityEvent {"));
    assert!(AGENT_EVENT.contains("pub(crate) enum AgentInvocationEvent {"));
    assert!(TEAM_EVENT.contains("pub(crate) enum TeamEvent {"));
    assert!(AGENT_EVENT.contains("pub(crate) enum AgentStreamEvent {"));
    assert!(MESSAGE_EVENT.contains("pub(crate) enum MessageEvent {"));
    assert!(TOOL_EVENT.contains("pub(crate) enum ToolEvent {"));
    assert!(DELEGATION_EVENT.contains("pub(crate) enum DelegationEvent {"));
    assert!(RUNTIME_EVENT.contains("pub(crate) enum RuntimeEvent {"));
    assert!(SESSION_EVENT.contains("pub(crate) struct SessionCompactionEvent {"));
    assert!(WORKFLOW_EVENT.contains("pub(crate) enum SelfHealingEditEvent {"));
    assert!(RECOVERY_EVENT.contains("pub(crate) struct RecoveryEvent {"));
    assert!(!public_event_source.contains("fn from_internal("));
    assert!(!public_event_source.contains("fn terminal_operation_for("));
    assert!(!client_projection_source.contains("from_internal"));
    assert!(!snapshot_source.contains("fn root_evidence("));
    assert!(snapshot_source.contains("pub(crate) struct OperationEventContext {"));
    assert!(snapshot_source
        .contains("operation_event_contexts: HashMap<String, OperationEventContext>"));
    assert!(!snapshot_source.contains("operation_capability_generations:"));
    assert!(!snapshot_source.contains("operation_kinds:"));
    assert!(snapshot_source.contains("let Some(terminal_operation) = event.terminal_operation()"));
    assert!(outcome_source.contains("pub(crate) fn product_terminal_operation("));
    assert!(outcome_source.contains("pub(crate) enum OperationTerminalPolicy {"));
    assert!(outcome_source.contains("terminal_policy: OperationTerminalPolicy"));
    assert!(outcome_source.contains("fn validate_terminal_policy(self)"));
    assert!(!outcome_source.contains("OperationAssociationClass"));

    let event_service_source =
        std::fs::read_to_string(repo_root.join("crates/coding-agent/src/services/event.rs"))
            .expect("read event service source");
    assert!(event_service_source.contains("broadcast::Sender<ProductEvent>"));
    assert!(event_service_source.contains("retained_product_events.push_back(event)"));
    assert!(event_service_source.contains("state.operation_event_contexts.get(operation_id)"));
    assert!(event_service_source.contains("product_terminal_operation("));
    assert!(event_service_source.contains("event.into_product_draft()"));
    assert!(!event_service_source.contains("operation_capability_generations"));
    assert!(!event_service_source.contains("operation_kinds"));
    assert!(!event_service_source.contains("CodingAgentEvent"));
    assert!(!event_service_source.contains("CodingAgentProductEventKind::from(&event)"));
    let publish = region(
        &event_service_source,
        "fn publish(",
        "pub(crate) fn emit_agent_event",
    );
    assert!(publish.contains("ProductEvent::new("));
}

#[test]
fn compatibility_deletion_does_not_add_path_scoped_deprecation_suppressions() {
    let repo_root = workspace_path("");
    let guarded_files = [
        "crates/coding-agent/src/events/mod.rs",
        "crates/coding-agent/src/events/workflow.rs",
        "crates/coding-agent/src/events/recovery.rs",
        "crates/coding-agent/src/services/event.rs",
        "crates/coding-agent/src/runtime/facade.rs",
        "crates/cli/src/protocol/events.rs",
        "crates/cli/src/rpc/events.rs",
        "crates/cli/src/interactive/event_bridge.rs",
        "crates/cli/src/interactive/loop.rs",
    ];
    let mut violations = Vec::new();

    for relative in guarded_files {
        let source =
            std::fs::read_to_string(repo_root.join(relative)).expect("read guarded source");
        for (line_index, line) in source.lines().enumerate() {
            if line.contains("allow(deprecated)") {
                violations.push(format!("{relative}:{}: {}", line_index + 1, line.trim()));
            }
        }
    }

    assert!(
        violations.is_empty(),
        "compatibility deletion paths must not suppress deprecated APIs:\n{}",
        violations.join("\n")
    );
}

fn collect_source_violations(
    repo_root: &std::path::Path,
    path: &std::path::Path,
    allowed_files: &[&str],
    violations: &mut Vec<String>,
    is_violation: impl Copy + Fn(&str) -> bool,
) {
    let Ok(metadata) = std::fs::metadata(path) else {
        return;
    };
    if metadata.is_dir() {
        let mut entries = std::fs::read_dir(path)
            .expect("read source directory")
            .collect::<Result<Vec<_>, _>>()
            .expect("read source entries");
        entries.sort_by_key(|entry| entry.path());
        for entry in entries {
            collect_source_violations(
                repo_root,
                &entry.path(),
                allowed_files,
                violations,
                is_violation,
            );
        }
        return;
    }
    if path.extension().and_then(|extension| extension.to_str()) != Some("rs") {
        return;
    }
    let relative = path
        .strip_prefix(repo_root)
        .expect("scanned file should be under repo root")
        .to_string_lossy()
        .replace('\\', "/");
    if allowed_files.contains(&relative.as_str()) {
        return;
    }
    let content = std::fs::read_to_string(path).expect("read source file");
    for (line_index, line) in content.lines().enumerate() {
        if is_violation(line) {
            violations.push(format!("{}:{}: {}", relative, line_index + 1, line.trim()));
        }
    }
}

#[test]
fn startup_recovery_stays_session_service_owned() {
    let session_service_rs =
        std::fs::read_to_string(workspace_path("crates/coding-agent/src/session/service.rs"))
            .expect("read session service source");
    let rpc_sources = [
        workspace_path("crates/cli/src/rpc/commands.rs"),
        workspace_path("crates/cli/src/rpc/prompt.rs"),
        workspace_path("crates/cli/src/interactive/event_bridge.rs"),
    ];

    assert!(session_service_rs.contains("apply_startup_recovery"));
    assert!(session_service_rs.contains("take_startup_recovery_markers"));
    for source in rpc_sources {
        let text = std::fs::read_to_string(&source).expect("read adapter source");
        assert!(
            !text.contains("SessionEventData::OperationRecovered {"),
            "adapters must project recovery events but not write recovery session markers: {}",
            source.display()
        );
    }
}
