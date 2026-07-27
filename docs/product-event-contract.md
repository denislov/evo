# Product event contract

This document is the release inventory for the typed events exposed by the
`coding-agent` product facade. The Rust owner types and their executable test
fixture remain authoritative; the snapshot test requires this inventory to
match them exactly so an event addition, removal, family move, or wire-kind
rename is a reviewed contract change.

Product events are ordered by their typed product-event sequence. Consumers
must reject gaps and association mismatches and recover through a fresh typed
snapshot; they must not infer event identity from debug strings or accept raw
compatibility events. Event-specific payloads, terminal status, durability,
operation identity, session identity, and capability generation retain the
serialization rules declared by the public Rust facade.

## Families

| Family | Responsibility |
|---|---|
| Session | Session lifecycle, writes, and compaction |
| Profile | Agent-profile selection |
| Agent | Invocation, turn, and provider-request lifecycle |
| Team | Multi-agent team lifecycle |
| Message | Assistant text and reasoning content |
| Tool | Tool execution and authorization lifecycle |
| Runtime | Runtime-owned compaction and shutdown |
| Delegation | Delegated-operation confirmation and execution |
| Workflow | Prompt, self-healing edit, and recovery lifecycle |
| Diagnostic | Product-safe diagnostics |
| Capability | Capability-generation changes |

## Event inventory

The family and kind columns are the stable serialized identifiers. The variant
column names the normalized public Rust event used by the facade and fixtures.

<!-- product-event-inventory:start -->
| Variant | Family | Kind |
|---|---|---|
| `SessionOpened` | `session` | `opened` |
| `SessionWritePending` | `session` | `write_pending` |
| `SessionWriteCommitted` | `session` | `write_committed` |
| `SessionWriteSkipped` | `session` | `write_skipped` |
| `SessionCompactionCompleted` | `session` | `compaction_completed` |
| `DefaultAgentProfileChanged` | `profile` | `default_changed` |
| `AgentInvocationStarted` | `agent` | `invocation_started` |
| `AgentInvocationCompleted` | `agent` | `invocation_completed` |
| `AgentInvocationFailed` | `agent` | `invocation_failed` |
| `AgentInvocationAborted` | `agent` | `invocation_aborted` |
| `AgentTurnStarted` | `agent` | `turn_started` |
| `ProviderRequestStarted` | `agent` | `provider_request_started` |
| `AgentTeamStarted` | `team` | `started` |
| `AgentTeamMemberStarted` | `team` | `member_started` |
| `AgentTeamMemberCompleted` | `team` | `member_completed` |
| `AgentTeamCompleted` | `team` | `completed` |
| `AgentTeamFailed` | `team` | `failed` |
| `AgentTeamAborted` | `team` | `aborted` |
| `AssistantMessageStarted` | `message` | `started` |
| `AssistantMessageDelta` | `message` | `delta` |
| `AssistantThinkingDelta` | `message` | `thinking_delta` |
| `AssistantMessageCompleted` | `message` | `completed` |
| `ToolCallStarted` | `tool` | `started` |
| `ToolCallUpdated` | `tool` | `updated` |
| `ToolCallCompleted` | `tool` | `completed` |
| `ToolCallFailed` | `tool` | `failed` |
| `RuntimeCompactionCompleted` | `runtime` | `compaction_completed` |
| `RuntimeShutDown` | `runtime` | `shut_down` |
| `DelegationRequested` | `delegation` | `requested` |
| `DelegationRejected` | `delegation` | `rejected` |
| `DelegationApproved` | `delegation` | `approved` |
| `DelegationConfirmationRequired` | `delegation` | `confirmation_required` |
| `DelegationStarted` | `delegation` | `started` |
| `DelegationCompleted` | `delegation` | `completed` |
| `DelegationFailed` | `delegation` | `failed` |
| `SelfHealingEditStarted` | `workflow` | `self_healing_edit_started` |
| `SelfHealingEditRepairAttempted` | `workflow` | `self_healing_edit_repair_attempted` |
| `SelfHealingEditCompleted` | `workflow` | `self_healing_edit_completed` |
| `SelfHealingEditFailed` | `workflow` | `self_healing_edit_failed` |
| `PromptStarted` | `workflow` | `prompt_started` |
| `PromptCompleted` | `workflow` | `prompt_completed` |
| `PromptFailed` | `workflow` | `prompt_failed` |
| `PromptAborted` | `workflow` | `prompt_aborted` |
| `OperationRecovered` | `workflow` | `operation_recovered` |
| `Diagnostic` | `diagnostic` | `diagnostic` |
| `CapabilityChanged` | `capability` | `changed` |
| `SelfHealingEditAborted` | `workflow` | `self_healing_edit_aborted` |
| `SessionWriteFailed` | `session` | `write_failed` |
| `ToolCallAuthorizationRequired` | `tool` | `authorization_required` |
| `ToolCallAuthorizationApproved` | `tool` | `authorization_approved` |
| `ToolCallAuthorizationDenied` | `tool` | `authorization_denied` |
| `ToolCallAuthorizationCancelled` | `tool` | `authorization_cancelled` |
<!-- product-event-inventory:end -->

## Operation outcomes

Every public operation has one top-level outcome variant. Payload-level success,
failure, cancellation, or no-op detail stays inside the paired typed outcome;
the facade must not return an untyped value as an alternative path.

<!-- operation-outcome-matrix:start -->
| Operation | Outcome |
|---|---|
| `Prompt` | `Prompt` |
| `Compact` | `Compact` |
| `BranchSummary` | `BranchSummary` |
| `SelfHealingEdit` | `SelfHealingEdit` |
| `InvokeAgent` | `AgentInvocation` |
| `InvokeTeam` | `AgentTeam` |
| `SetDefaultAgentProfile` | `DefaultAgentProfileChanged` |
| `ApproveDelegation` | `DelegationApproved` |
| `RejectDelegation` | `DelegationRejected` |
| `ForkSession` | `SessionForked` |
| `SwitchActiveLeaf` | `ActiveLeafSwitched` |
| `SetSessionTreeLabel` | `SessionTreeLabelChanged` |
| `ExportCurrent` | `Export` |
| `ExportCurrentHtml` | `ExportHtml` |
<!-- operation-outcome-matrix:end -->

## Change policy

- Additive payload fields must have backward-compatible serde defaults where
  older stored or transported events can omit them.
- A new event requires an owner variant, fixture construction, an inventory row,
  projection handling, and an explicit protocol compatibility decision.
- Renaming or removing a family, kind, operation, or outcome is a breaking
  contract change and requires a versioned migration rather than a silent edit.
- Adapters consume the categorized public facade. Raw internal events and debug
  representations are never a supported transport or persistence format.
