//! Conversation model unit tests: block hydration, bounded retention, release
//! fixtures, and streaming matrix.

use std::borrow::Cow;

use coding_agent::api::view::{CodingAgentSessionTranscriptItem, CodingAgentTranscriptSnapshot};

use super::*;
use crate::ui::conversation::markdown::bounded_markdown_preview;
use crate::ui::conversation::{
    ComposerState, ConversationRowRenderCache, ConversationRowRenderSource, ConversationViewport,
};

fn transcript(items: Vec<CodingAgentSessionTranscriptItem>) -> CodingAgentTranscriptSnapshot {
    CodingAgentTranscriptSnapshot::new("session-1", Some("leaf-1".into()), items)
}

fn user(index: usize) -> CodingAgentSessionTranscriptItem {
    CodingAgentSessionTranscriptItem::User {
        text: format!("message {index}"),
        started_at: None,
    }
}

#[test]
fn delegation_status_parses_the_tool_vocabulary() {
    for (raw, expected, label) in [
        ("requested", DelegationStatus::Requested, "Requested"),
        ("running", DelegationStatus::Running, "Running"),
        ("completed", DelegationStatus::Completed, "Completed"),
        ("failed", DelegationStatus::Failed, "Failed"),
        ("rejected", DelegationStatus::Rejected, "Rejected"),
        ("cancelled", DelegationStatus::Cancelled, "Cancelled"),
        (
            "confirmation_required",
            DelegationStatus::ConfirmationRequired,
            "Awaiting approval",
        ),
    ] {
        let parsed = DelegationStatus::parse(raw);
        assert_eq!(parsed, expected, "{raw}");
        assert_eq!(parsed.label(), label, "{raw}");
    }
    assert_eq!(
        DelegationStatus::parse("mystery-status"),
        DelegationStatus::Unknown
    );
}

#[test]
fn delegation_block_carries_target_and_status_metadata() {
    use coding_agent::api::view::{ProfileId, ProfileKind};
    let snapshot = transcript(vec![CodingAgentSessionTranscriptItem::Delegation {
        tool_call_id: "call-delegation".into(),
        requesting_profile_id: ProfileId::new("sa-main").unwrap(),
        target_kind: ProfileKind::Agent,
        target_id: ProfileId::new("sa_plan").unwrap(),
        task: "Implement the auth flow\nsecond line".into(),
        status: "running".into(),
        child_operation_id: Some("op-1".into()),
        summary: None,
    }]);
    let block = ConversationProjection::hydrate(snapshot).blocks()[0].clone();
    assert_eq!(block.title, "Delegation · Agent");
    assert_eq!(block.text, "Implement the auth flow\nsecond line");
    // No summary yet: the detail stays empty instead of falling back to
    // the raw status string, which the header shows on its own.
    assert_eq!(block.detail, "");
    let meta = block.delegation.expect("delegation metadata");
    assert_eq!(meta.target_id, "sa_plan");
    assert_eq!(meta.status, DelegationStatus::Running);
    assert!(block.done);
    assert!(!block.is_error);
}

#[test]
fn delegation_status_transitions_bump_the_block_revision() {
    use coding_agent::api::view::{ProfileId, ProfileKind};
    fn delegation_block(status: &str) -> ConversationBlock {
        let snapshot = transcript(vec![CodingAgentSessionTranscriptItem::Delegation {
            tool_call_id: "call-delegation".into(),
            requesting_profile_id: ProfileId::new("sa-main").unwrap(),
            target_kind: ProfileKind::Agent,
            target_id: ProfileId::new("sa_plan").unwrap(),
            task: "Implement the auth flow".into(),
            status: status.into(),
            child_operation_id: None,
            summary: None,
        }]);
        ConversationProjection::hydrate(snapshot).blocks()[0].clone()
    }
    // Same task and (empty) summary, only the status changes: the render
    // cache must see a new revision so the header chip re-renders.
    assert_ne!(
        delegation_block("running").source_revision,
        delegation_block("completed").source_revision
    );
    // Unrelated statuses stay stable across identical inputs.
    assert_eq!(
        delegation_block("running").source_revision,
        delegation_block("running").source_revision
    );
}

#[test]
fn adjacent_equivalent_diagnostics_coalesce_for_presentation() {
    let message = "invalid terminal tool-call name";
    let projection = ConversationProjection::hydrate(transcript(vec![
        CodingAgentSessionTranscriptItem::Diagnostic {
            message: message.into(),
        },
        CodingAgentSessionTranscriptItem::Diagnostic {
            message: message.into(),
        },
        CodingAgentSessionTranscriptItem::Diagnostic {
            message: format!("provider error: {message}"),
        },
        user(1),
        CodingAgentSessionTranscriptItem::Diagnostic {
            message: message.into(),
        },
    ]));

    assert_eq!(projection.blocks().len(), 3);
    let diagnostic = projection.blocks().front().unwrap();
    assert_eq!(diagnostic.title, "Diagnostic · 3 related events");
    assert_eq!(diagnostic.text, message);
    assert_eq!(projection.blocks().back().unwrap().title, "Diagnostic");
}

#[test]
fn long_transcript_retains_the_newest_ten_thousand_stable_blocks() {
    let projection =
        ConversationProjection::hydrate(transcript((0..10_500).map(user).collect::<Vec<_>>()));
    assert_eq!(projection.blocks().len(), MAX_TRANSCRIPT_BLOCKS);
    assert_eq!(projection.omitted_blocks(), 500);
    assert_eq!(projection.blocks().front().unwrap().text, "message 500");
    assert_eq!(projection.blocks().back().unwrap().text, "message 10499");
}

#[test]
fn release_fixture_retains_ten_thousand_blocks_and_at_least_ten_mib() {
    let payload = "x".repeat(1_280);
    let items = (0..MAX_TRANSCRIPT_BLOCKS)
        .map(|index| CodingAgentSessionTranscriptItem::User {
            text: format!("{index}:{payload}"),
            started_at: None,
        })
        .collect::<Vec<_>>();
    let fixture_bytes = items
        .iter()
        .map(|item| match item {
            CodingAgentSessionTranscriptItem::User { text, .. } => text.len(),
            _ => 0,
        })
        .sum::<usize>();
    assert!(fixture_bytes >= 10 * 1024 * 1024);

    let projection = ConversationProjection::hydrate(transcript(items));
    assert_eq!(projection.blocks().len(), MAX_TRANSCRIPT_BLOCKS);
    assert_eq!(projection.omitted_blocks(), 0);
    assert!(projection.retained_bytes() <= MAX_TRANSCRIPT_BYTES);
    assert!(projection.blocks().front().unwrap().text.starts_with("0:"));
    assert!(
        projection
            .blocks()
            .back()
            .unwrap()
            .text
            .starts_with("9999:")
    );
}

#[test]
#[ignore = "release performance gate"]
fn desktop_release_empty_conversation_baseline() {
    let _performance_guard = crate::allocation_probe::serial_guard();
    let projection = ConversationProjection::hydrate(transcript(Vec::new()));
    let viewport = ConversationViewport::new(30);
    let composer = ComposerState::default();
    std::hint::black_box((&projection, &viewport, &composer));
    println!("desktop_perf\tempty_blocks={}", projection.blocks().len());
}

#[test]
#[ignore = "release performance gate"]
fn desktop_release_ten_mib_interaction_baseline() {
    let _performance_guard = crate::allocation_probe::serial_guard();
    const SAMPLE_COUNT: usize = 500;
    const VISIBLE_BLOCKS: usize = 30;
    const FRAME_BUDGET_MICROS: u128 = 16_700;
    const HYDRATION_ALLOCATION_BUDGET: u64 = 40_064;
    const HYDRATION_ALLOCATED_BYTE_BUDGET: u64 = 8 * 1024 * 1024;
    const HYDRATION_RSS_GROWTH_BUDGET: u64 = 64 * 1024 * 1024;

    let payload = format!(
        "# streamed message\n\n{}\n\n```text\n{}\n```",
        "x".repeat(1_152),
        "tool progress".repeat(8)
    );
    let items = (0..MAX_TRANSCRIPT_BLOCKS)
        .map(|index| CodingAgentSessionTranscriptItem::User {
            text: format!("{index}:{payload}"),
            started_at: None,
        })
        .collect::<Vec<_>>();
    let fixture_bytes = items
        .iter()
        .map(|item| match item {
            CodingAgentSessionTranscriptItem::User { text, .. } => text.len(),
            _ => 0,
        })
        .sum::<usize>();
    assert!(fixture_bytes >= 10 * 1024 * 1024);

    let rss_before = crate::allocation_probe::resident_bytes();
    let allocations_before = crate::allocation_probe::snapshot();
    let hydration_started = std::time::Instant::now();
    let projection = ConversationProjection::hydrate(transcript(items));
    let hydration_micros = hydration_started.elapsed().as_micros();
    let hydration_allocations = crate::allocation_probe::snapshot().delta_since(allocations_before);
    let rss_after = crate::allocation_probe::resident_bytes();
    let rss_growth = resident_growth(rss_before, rss_after);
    assert_eq!(projection.blocks().len(), MAX_TRANSCRIPT_BLOCKS);

    let mut viewport = ConversationViewport::new(VISIBLE_BLOCKS);
    viewport.on_blocks_changed(projection.blocks().len());
    let mut scroll_samples = Vec::with_capacity(SAMPLE_COUNT);
    for sample in 0..SAMPLE_COUNT {
        let first_visible = (sample * 97) % (MAX_TRANSCRIPT_BLOCKS.saturating_sub(VISIBLE_BLOCKS));
        let started = std::time::Instant::now();
        viewport.user_scrolled(first_visible, projection.blocks().len());
        let visible = projection
            .blocks()
            .iter()
            .skip(viewport.first_visible())
            .take(VISIBLE_BLOCKS)
            .map(|block| bounded_markdown_preview(&block.text))
            .collect::<Vec<_>>();
        std::hint::black_box(visible);
        scroll_samples.push(started.elapsed().as_micros());
    }

    let mut composer = ComposerState::default();
    let mut input_samples = Vec::with_capacity(SAMPLE_COUNT);
    for sample in 0..SAMPLE_COUNT {
        let draft = format!("ordinary desktop input {sample} {}", "x".repeat(256));
        let started = std::time::Instant::now();
        composer.edit(draft);
        std::hint::black_box(composer.draft());
        input_samples.push(started.elapsed().as_nanos());
    }

    let scroll_p95_micros = percentile_95(&mut scroll_samples);
    let input_p95_micros = percentile_95(&mut input_samples).div_ceil(1_000);
    println!(
        "desktop_perf\tplatform={}\tfixture_bytes={fixture_bytes}\thydration_us={hydration_micros}\t\
             hydration_allocations={}\thydration_allocated_bytes={}\tretained_bytes={}\t\
             rss_supported={}\trss_before_bytes={}\trss_after_bytes={}\trss_growth_bytes={}\t\
             scroll_render_p95_us={scroll_p95_micros}\tinput_p95_us={input_p95_micros}",
        std::env::consts::OS,
        hydration_allocations.count(),
        hydration_allocations.bytes(),
        projection.retained_bytes(),
        rss_before.is_some() && rss_after.is_some(),
        rss_before.unwrap_or_default(),
        rss_after.unwrap_or_default(),
        rss_growth.unwrap_or_default()
    );
    assert!(
        scroll_p95_micros <= FRAME_BUDGET_MICROS,
        "10k transcript scroll/render preparation P95 exceeded one frame: \
             {scroll_p95_micros} us"
    );
    assert!(
        input_p95_micros <= FRAME_BUDGET_MICROS,
        "composer input P95 exceeded one frame: {input_p95_micros} us"
    );
    assert!(
        hydration_allocations.count() <= HYDRATION_ALLOCATION_BUDGET,
        "10k transcript hydration allocation count exceeded the linear budget: {}",
        hydration_allocations.count()
    );
    assert!(
        hydration_allocations.bytes() <= HYDRATION_ALLOCATED_BYTE_BUDGET,
        "10 MiB transcript hydration copied too much retained content: {} bytes",
        hydration_allocations.bytes()
    );
    if let Some(rss_growth) = rss_growth {
        assert!(
            rss_growth <= HYDRATION_RSS_GROWTH_BUDGET,
            "10 MiB transcript hydration RSS growth exceeded 64 MiB: {rss_growth} bytes"
        );
    }
}

#[test]
#[ignore = "release performance gate"]
fn desktop_release_scale_content_and_streaming_matrix() {
    let _performance_guard = crate::allocation_probe::serial_guard();
    const FRAME_BUDGET_MICROS: u128 = 16_700;
    const FINAL_PARSE_BUDGET_MICROS: u128 = 150_000;
    const HYDRATION_RSS_GROWTH_BUDGET: u64 = 64 * 1024 * 1024;

    for block_count in [1, 100, 1_000, MAX_TRANSCRIPT_BLOCKS] {
        let items = (0..block_count)
            .map(user)
            .collect::<Vec<CodingAgentSessionTranscriptItem>>();
        let rss_before = crate::allocation_probe::resident_bytes();
        let allocations_before = crate::allocation_probe::snapshot();
        let started = std::time::Instant::now();
        let projection = ConversationProjection::hydrate(transcript(items));
        let hydration_micros = started.elapsed().as_micros();
        let hydration_allocations =
            crate::allocation_probe::snapshot().delta_since(allocations_before);
        let rss_after = crate::allocation_probe::resident_bytes();
        let rss_growth = resident_growth(rss_before, rss_after);
        println!(
            "desktop_perf\tplatform={}\tscale_blocks={block_count}\thydration_us={hydration_micros}\t\
                 hydration_allocations={}\thydration_allocated_bytes={}\tretained_bytes={}\t\
                 rss_supported={}\trss_before_bytes={}\trss_after_bytes={}\trss_growth_bytes={}",
            std::env::consts::OS,
            hydration_allocations.count(),
            hydration_allocations.bytes(),
            projection.retained_bytes(),
            rss_before.is_some() && rss_after.is_some(),
            rss_before.unwrap_or_default(),
            rss_after.unwrap_or_default(),
            rss_growth.unwrap_or_default()
        );
        assert_eq!(projection.blocks().len(), block_count);
        assert!(
            hydration_allocations.count() <= block_count as u64 * 4 + 64,
            "{block_count}-block hydration allocation count exceeded the linear budget"
        );
        assert!(
            hydration_allocations.bytes() <= block_count as u64 * 512 + 4_096,
            "{block_count}-block hydration allocated-byte count exceeded the linear budget"
        );
        if let Some(rss_growth) = rss_growth {
            assert!(
                rss_growth <= HYDRATION_RSS_GROWTH_BUDGET,
                "{block_count}-block hydration RSS growth exceeded 64 MiB"
            );
        }
    }

    let table_row = format!(
        "| {} |\n",
        (0..32).map(|_| "cell").collect::<Vec<_>>().join(" | ")
    );
    let content_cases = [
        (
            "markdown_256k",
            format!(
                "# heading\n\n{}",
                "paragraph **bold** `code`\n".repeat(10_000)
            ),
        ),
        ("reasoning_512k", "reasoning step 中文 🧠\n".repeat(24_000)),
        (
            "bash_output",
            format!("```text\n{}\n```", "build output\n".repeat(80_000)),
        ),
        ("table", table_row.repeat(1_000)),
        (
            "code_cjk_emoji",
            format!(
                "```rust\n{}\n```\n{}",
                "fn main() {}\n".repeat(12_000),
                "中文🙂🚀\n".repeat(12_000)
            ),
        ),
    ];
    for (name, payload) in content_cases {
        let mut samples = Vec::with_capacity(20);
        for _ in 0..20 {
            let started = std::time::Instant::now();
            let preview = bounded_markdown_preview(&payload);
            std::hint::black_box(preview);
            samples.push(started.elapsed().as_micros());
        }
        let preview_p95_micros = percentile_95(&mut samples);
        println!(
            "desktop_perf\tcontent={name}\tinput_bytes={}\tpreview_sanitize_p95_us={preview_p95_micros}",
            payload.len()
        );
        assert!(
            preview_p95_micros <= FINAL_PARSE_BUDGET_MICROS,
            "{name} bounded preview sanitize P95 exceeded 150ms: {preview_p95_micros} us"
        );
    }

    let streaming_text = "streaming 中文 🙂 output ".repeat(128);
    for events_per_second in [10, 50, 200] {
        let mut cache = ConversationRowRenderCache::default();
        let mut samples = Vec::with_capacity(events_per_second);
        for revision in 1..=events_per_second {
            cache.begin_frame();
            let started = std::time::Instant::now();
            let row = cache.resolve(
                ConversationRowRenderSource {
                    item_key: ConversationItemKey::new(
                        "performance-session",
                        ConversationItemKind::LiveMessage,
                        "live:assistant:performance",
                    ),
                    source_revision: revision as u64,
                    title: Cow::Borrowed("Assistant"),
                    text: &streaming_text,
                    detail: "",
                    kind: ConversationBlockKind::Assistant,
                    done: false,
                    is_error: false,
                    image_count: 0,
                    reasoning_duration_millis: None,
                    truncated: false,
                    durable: false,
                    delegation: None,
                    turn: None,
                    model: None,
                },
                900,
            );
            std::hint::black_box(row);
            cache.finish_incremental();
            samples.push(started.elapsed().as_micros());
        }
        let event_p95_micros = percentile_95(&mut samples);
        println!(
            "desktop_perf\tstream_events_per_s={events_per_second}\t\
                 event_p95_us={event_p95_micros}\tcache_bytes={}",
            cache.retained_bytes()
        );
        assert!(
            event_p95_micros <= FRAME_BUDGET_MICROS,
            "{events_per_second} events/s row preparation P95 exceeded one frame: \
                 {event_p95_micros} us"
        );
    }
}

fn percentile_95(samples: &mut [u128]) -> u128 {
    samples.sort_unstable();
    let index = (samples.len() * 95).div_ceil(100).saturating_sub(1);
    samples[index]
}

fn resident_growth(before: Option<u64>, after: Option<u64>) -> Option<u64> {
    Some(after?.saturating_sub(before?))
}

#[test]
fn long_unicode_text_is_truncated_on_a_scalar_boundary() {
    let projection =
        ConversationProjection::hydrate(transcript(vec![CodingAgentSessionTranscriptItem::User {
            text: "界".repeat(MAX_BLOCK_TEXT_BYTES),
            started_at: None,
        }]));
    let block = projection.blocks().front().unwrap();
    assert!(block.truncated);
    assert!(block.text.len() <= MAX_BLOCK_TEXT_BYTES);
    assert!(block.text.is_char_boundary(block.text.len()));
}

#[test]
fn tool_duration_uses_compact_stable_units() {
    assert_eq!(compact_duration(0), "0 ms");
    assert_eq!(compact_duration(999), "999 ms");
    assert_eq!(compact_duration(1_049), "1.0 s");
    assert_eq!(compact_duration(1_050), "1.1 s");
    assert_eq!(compact_duration(59_949), "59.9 s");
    assert_eq!(compact_duration(59_950), "1m 00s");
    assert_eq!(compact_duration(60_000), "1m 00s");
    assert_eq!(compact_duration(125_600), "2m 06s");
}

#[test]
fn assistant_hydration_preserves_reasoning_duration_for_disclosure() {
    let projection = ConversationProjection::hydrate(transcript(vec![
        CodingAgentSessionTranscriptItem::Assistant {
            id: "message-reasoning".into(),
            text: "answer".into(),
            thinking: "reasoning".into(),
            images: Vec::new(),
            done: true,
            reasoning_duration_millis: Some(2_430),
            model_id: None,
            completed_at: None,
        },
    ]));
    let block = projection.blocks().front().unwrap();
    assert_eq!(block.reasoning_duration_millis, Some(2_430));
    assert_eq!(
        compact_duration(block.reasoning_duration_millis.unwrap()),
        "2.4 s"
    );
}

#[test]
fn assistant_hydration_carries_the_message_level_model() {
    let projection = ConversationProjection::hydrate(transcript(vec![
        CodingAgentSessionTranscriptItem::Assistant {
            id: "message-model".into(),
            text: "answer".into(),
            thinking: String::new(),
            images: Vec::new(),
            done: true,
            reasoning_duration_millis: None,
            model_id: Some("deepseek-v4-flash".into()),
            completed_at: None,
        },
    ]));
    let block = projection.blocks().front().unwrap();
    assert_eq!(block.model.as_deref(), Some("deepseek-v4-flash"));
}

#[test]
fn turn_metadata_lands_on_the_final_assistant_row_of_a_completed_turn() {
    let items = vec![
        CodingAgentSessionTranscriptItem::User {
            text: "do it".into(),
            started_at: Some("2026-01-01T00:00:00Z".into()),
        },
        CodingAgentSessionTranscriptItem::Assistant {
            id: "message-1".into(),
            text: "step one".into(),
            thinking: String::new(),
            images: Vec::new(),
            done: true,
            reasoning_duration_millis: None,
            model_id: Some("deepseek-v4-flash".into()),
            completed_at: Some("2026-01-01T00:00:10Z".into()),
        },
        CodingAgentSessionTranscriptItem::Tool {
            call_id: "call-1".into(),
            name: "shell".into(),
            args: serde_json::json!({}),
            result: Some("done".into()),
            is_error: false,
            duration_millis: None,
        },
        CodingAgentSessionTranscriptItem::Assistant {
            id: "message-2".into(),
            text: "step two".into(),
            thinking: String::new(),
            images: Vec::new(),
            done: true,
            reasoning_duration_millis: None,
            model_id: Some("deepseek-v4-flash".into()),
            completed_at: Some("2026-01-01T00:00:30Z".into()),
        },
    ];
    let projection = ConversationProjection::hydrate(transcript(items));
    let blocks = projection.blocks();
    // Interior assistant rows of the turn carry no turn metadata.
    assert!(blocks[1].turn.is_none(), "interior row must stay bare");
    let turn = blocks[3]
        .turn
        .as_ref()
        .expect("the turn's final assistant row carries the summary");
    assert_eq!(turn.model, "deepseek-v4-flash");
    // Submit at 00:00:00 to completion at 00:00:30, tool call included.
    assert_eq!(turn.duration_millis, Some(30_000));
}

#[test]
fn turn_metadata_omits_duration_for_unfinished_or_legacy_turns() {
    // The last assistant row never completed: the model is known but the
    // whole-turn duration is not.
    let items = vec![
        CodingAgentSessionTranscriptItem::User {
            text: "do it".into(),
            started_at: Some("2026-01-01T00:00:00Z".into()),
        },
        CodingAgentSessionTranscriptItem::Assistant {
            id: "message-3".into(),
            text: "streaming…".into(),
            thinking: String::new(),
            images: Vec::new(),
            done: false,
            reasoning_duration_millis: None,
            model_id: Some("deepseek-v4-flash".into()),
            completed_at: None,
        },
    ];
    let projection = ConversationProjection::hydrate(transcript(items));
    let turn = projection.blocks().back().unwrap().turn.as_ref().unwrap();
    assert_eq!(turn.model, "deepseek-v4-flash");
    assert_eq!(turn.duration_millis, None);

    // Legacy session without submit timestamps: no duration either.
    let items = vec![
        CodingAgentSessionTranscriptItem::User {
            text: "legacy".into(),
            started_at: None,
        },
        CodingAgentSessionTranscriptItem::Assistant {
            id: "message-4".into(),
            text: "old answer".into(),
            thinking: String::new(),
            images: Vec::new(),
            done: true,
            reasoning_duration_millis: None,
            model_id: Some("claude-sonnet-4-5".into()),
            completed_at: None,
        },
    ];
    let projection = ConversationProjection::hydrate(transcript(items));
    let turn = projection.blocks().back().unwrap().turn.as_ref().unwrap();
    assert_eq!(turn.model, "claude-sonnet-4-5");
    assert_eq!(turn.duration_millis, None);
}
