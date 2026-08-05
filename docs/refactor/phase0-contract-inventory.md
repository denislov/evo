# Phase 0 跨层契约清单

这份文档是 ARC-002 的发布契约索引。原则是：fixture 固定 wire shape，transition table 固定状态机，真实临时存储测试固定 crash/reopen 语义，facade test 固定 crate 对外边界。测试名和命令属于可审查的 release inventory，不依赖人工记忆。

## 一键入口

```bash
scripts/release-api-snapshots.sh
cargo test --locked -p cli
cargo test --locked -p coding-agent --all-features
cargo test --locked -p desktop --all-features
```

`scripts/release-api-snapshots.sh` 当前固定运行：

| Package | Contract target | Tests |
| --- | --- | ---: |
| `agent-core` | root facade only exposes `api` | 1 |
| `ai` | root facade only exposes `api` | 1 |
| `coding-agent` | public facade and non-exhaustive DTO construction | 2 |
| `coding-agent` | ProductEvent / cross-adapter golden | 2 |
| `coding-agent` | operation descriptor table | 5 |
| `coding-agent` | child/delegated capability snapshots | 2 |
| `desktop` | dependency boundary | 11 |
| `tui` | API/UI boundary contract | 5 |

## Public facade

| Owner | Evidence | Frozen rule |
| --- | --- | --- |
| `agent-core` | `crates/agent-core/tests/api_contract.rs` | crate root 的唯一 public module 是 `api` |
| `ai` | `crates/ai/tests/api_contract.rs` | crate root 的唯一 public module 是 `api` |
| `coding-agent` | `crates/coding-agent/tests/api_contract.rs` | 下游只通过稳定 facade；演进中的 session response DTO 不能被 downstream struct literal 锁死 |
| `desktop` | `crates/desktop/tests/dependency_boundary.rs` | Desktop 是 adapter，只依赖 `coding_agent::api`，并登记性能/视觉 Gate |
| `tui` | `crates/tui/tests/api_contract.rs` | TUI render/input API 与 UI boundary 保持 deterministic |

此外，`scripts/architecture-gate.sh` 对 CLI/Desktop 执行源码级 facade 检查，任何 `coding_agent::` 非 `api` 引用都会失败。

## ProductEvent 与 projection

| Fixture / test | 固定内容 |
| --- | --- |
| `all-product-event-families.json` | 每个 ProductEvent family 的 schema、序列化与 round-trip |
| `cross-adapter-events.json` | CLI/Desktop 共同消费的事件序列 |
| `cross-adapter-projection.json` | 共同事件序列折叠后的 client projection truth |
| `product_event_schema_golden_covers_every_family_and_round_trips` | family coverage 与反序列化闭包 |
| `shared_cross_adapter_events_match_the_client_projection_golden` | domain projection 与 fixture 完全一致 |
| Desktop `shared_cross_adapter_fixture_matches_desktop_product_state_exactly` | Desktop adapter 不重解释 product truth |
| CLI `protocol::events_tests::*` | ProductEvent 到 JSONL protocol event 的 typed mapping |

Fixture 的 bytes 和 SHA-256 见 `docs/refactor/phase0-baseline.md`。任何 intentional wire change 必须同时修改 producer、所有 adapter、fixture 和 release inventory。

## CLI RPC wire

当前 CLI RPC 没有维护一份覆盖所有 command 的巨大手写 JSON 文件；wire 由 typed serde tests、JSONL framing tests 和端到端 command tests共同固定。

| Contract | Representative evidence |
| --- | --- |
| JSONL framing | `jsonl_reader_accepts_the_exact_frame_limit`、`discards_one_byte_over_and_recovers_at_lf`、chunk boundary、CRLF、oversized EOF |
| Lifecycle enum values | `lifecycle_wire_values_are_additive_and_exact` |
| Product event mapping | `coding_event_adapter_maps_*` family tests |
| Failure/recovery payload | prompt provider failure、session write failure、event stream lag typed payload tests |
| Protocol negotiation | RPC loop requires `hello` before non-hello commands and returns typed recovery guidance |
| Input limits | frame bytes、JSON depth、container items、identifier、image 和 repair-attempt limits |

Phase 1/6 若重写 CLI protocol ownership，应先把 command/response canonical samples抽成 fixture；在旧 RPC 模块删除前，新旧 serializer 必须对同一 fixture 完全一致。

## Desktop replay 与视觉契约

| Contract | Evidence |
| --- | --- |
| Projection recovery | `desktop_projection_rejects_gaps_and_association_mismatches_atomically` |
| Typed recovery replacement | `typed_recovery_reasons_replace_the_projection_atomically` |
| Recovery identity | `recovery_actions_are_identity_bound_and_stale_facts_fail_closed` |
| Authorization projection | `authorization_projection_preserves_identity_and_bounds_display_payloads` |
| Overlay behavior | authorization modal focus trap、recovery action、file review command smoke tests |
| Responsive layout | narrow/medium/wide deterministic GPUI tests |
| Visual output | `crates/desktop/tests/goldens/native/*.png` 与 `REVIEW.md` |
| Runtime performance | `scripts/desktop-perf-gate.sh` |
| Native frame/RSS | `scripts/desktop-native-perf-gate.sh` |

Desktop 不拥有 session、operation 或 ProductEvent 语义；它只维护 adapter projection、view state 和 render lifecycle。

## Session durability 与 recovery

| Invariant | Evidence |
| --- | --- |
| Bounded reopen | `session::repository::bounded::tests::hundred_thousand_event_hydration_read_is_time_and_memory_bounded` |
| Torn final frame repair | `test_support::tests::temp_session_env_repairs_a_partial_commit_on_reopen` |
| Fsync failure injection | `test_support::tests::temp_session_env_can_fail_the_fsync_boundary` |
| Failed/skipped prompt transaction | `session::service::finalize::tests::*_prompt_transaction_transition_table` |
| Recovery retry/backoff | `session::service::recovery::tests::recovery_*_transition_table` |
| Recovery blocks new submission | `application::snapshot::submission_escape_hatch_tests::running_or_recovery_pending_submissions_still_block_prepare` |
| Durable outbox ownership | repository/store + transaction writer tests exercised by `coding-agent --all-features` |
| Client reconnect boundary | `services::event::transition_table_tests::recovery_boundary_transition_table` |

持久化重构必须遵守：先迁移并验证，再删除旧 reader/writer；禁止长期 dual-write。Torn-tail repair、outbox replay、recovery identity 与 committed sequence 必须作为一个一致性协议迁移。

## Authorization 与执行时重校验

| Contract | Evidence |
| --- | --- |
| Mode serialization/default | `authorization::mode_tests::*` |
| Decision state machine | `authorization_decision_transition_table` |
| Capability generation | `authorization_generation_transition_table` |
| Persistence transition | `authorization_persistence_transition_table` |
| Runtime mode propagation | `runtime_mode_switch_updates_the_interactive_waiter_policy` |
| Operation-bound permit cleanup | `every_terminal_exit_discards_all_and_only_its_operation_bindings` |
| Authorization-bound filesystem target | `application::operation::permit` tests plus filesystem capability target equality checks |
| Symlink/path escape rejection | `platform::fs::capability::tests_file::symlink_escape_tests` |

授权是 operation-scoped capability，不是 UI confirmation flag。任何新 tool runtime 都必须在执行点重新验证 authorization-bound target，不能只相信展示给用户时的 path 字符串。

## Operation descriptor table

`application::operation::tests` 固定以下维度：

| Dimension | Contract |
| --- | --- |
| Exhaustiveness | 每个 `OperationKind` variant 都必须进入 descriptor table |
| Dispatch | 每个 variant 解析到声明的 runner/dispatch mode |
| Admission | session access、capacity、durability 从 descriptor 推导 |
| Cancellation | priority、cancellation 与 child policy 与 kind/dispatch 一致 |
| Export normalization | export variants 归一化到正确 runner mode |

新增 operation 时，编译通过但 descriptor coverage 未更新仍应失败。

## Child 与 delegated capability 基线

Phase 0 固定的是当前实现，不代表 Phase 3 的最终目标：

| Capability | 当前 child operation | 当前 delegated profile |
| --- | --- | --- |
| Actor | `ChildOperation(parent_operation_id)` | 调用方提供的 child actor |
| Model profile | 继承 parent snapshot | 替换为 delegated profile id |
| Explicit tools | 继承 parent | parent 与 profile 的交集 |
| Server-side tools | 继承 parent | profile 至少获一个显式工具时，继承 parent 已有的 server tools |
| Filesystem | 继承 parent workspace handle | 有 filesystem tool 时继承 parent handle |
| Shell | 继承 parent shell | profile 明确允许 `bash` 时继承 parent shell |
| Session read/write | 删除 | 删除 |
| UI | 删除 | 删除 |

Phase 3 会把 filesystem/shell 从“父 workspace handle”改成独立 child worktree capability。修改这两组 snapshot test 必须作为 intentional contract migration，并同时加入 worktree cleanup/recovery tests。

## 第三方 provenance

统一协议与来源记录位于：

```text
docs/refactor/provenance/README.md
docs/refactor/provenance/grok-build.md
docs/refactor/provenance/codex.md
docs/refactor/provenance/opencode.md
```

Phase 0 没有复制第三方 production code。后续任何移植必须记录 upstream revision、源路径、license/notice、携带测试、Evo 目标路径、本地修改和同步策略。

## Intentional change 协议

跨层契约发生预期变化时，变更必须在同一个提交中包含：

1. 新旧行为差异和迁移原因。
2. producer 与所有 consumer adapter 的实现修改。
3. golden/fixture 或 transition table 更新。
4. 持久化兼容或一次性 migration；不能只留 TODO。
5. provenance 更新（若来自第三方）。
6. `scripts/release-api-snapshots.sh` 和相关性能 Gate 全绿。
