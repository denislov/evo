use super::*;

#[gpui::test]
#[ignore = "release performance gate"]
fn desktop_release_gpui_headless_frame_and_input_replay(cx: &mut TestAppContext) {
    let _performance_guard = crate::allocation_probe::serial_guard();
    const SAMPLE_COUNT: usize = 200;
    const CPU_FRAME_BUDGET_MICROS: u128 = 16_700;
    const WINDOW_RSS_GROWTH_BUDGET: u64 = 64 * 1024 * 1024;

    initialize_visual_test(cx);
    let projection =
        visual_performance_projection(desktop::ui::conversation::model::MAX_TRANSCRIPT_BLOCKS);
    let window_rss_before = crate::allocation_probe::resident_bytes();
    let (shell, cx) = add_visual_shell(
        cx,
        DesktopRuntimeBridge::disconnected_for_test(),
        projection,
    );
    cx.simulate_resize(size(px(1_300.), px(900.)));
    cx.run_until_parked();
    let window_rss_after = crate::allocation_probe::resident_bytes();
    let window_rss_growth = match (window_rss_before, window_rss_after) {
        (Some(before), Some(after)) => Some(after.saturating_sub(before)),
        _ => None,
    };

    let mut frame_samples = Vec::with_capacity(SAMPLE_COUNT);
    for _ in 0..SAMPLE_COUNT {
        let started = Instant::now();
        cx.update(|window, _| window.refresh());
        cx.run_until_parked();
        frame_samples.push(started.elapsed().as_micros());
    }

    let mut input_roundtrip_samples = Vec::with_capacity(SAMPLE_COUNT);
    let mut input_to_render_samples = Vec::with_capacity(SAMPLE_COUNT);
    let keystroke =
        gpui::Keystroke::parse("a").expect("the headless composer input keystroke remains valid");
    for _ in 0..SAMPLE_COUNT {
        shell.read_with(cx, |shell, cx| {
            shell
                .views
                .composer_pane
                .read(cx)
                .latency_probe()
                .clear_last_observed();
        });
        let started = Instant::now();
        let dispatched = cx.update(|window, cx| window.dispatch_keystroke(keystroke.clone(), cx));
        assert!(dispatched, "headless composer accepts keyboard input");
        // The headless platform deliberately has no on_request_frame
        // callback. Dispatch through the real keyboard/InputEvent::Change
        // path, drain it, then force the requested ComposerPane frame.
        cx.run_until_parked();
        cx.update(|window, _| window.refresh());
        cx.run_until_parked();
        input_roundtrip_samples.push(started.elapsed().as_micros());
        let observed = shell
            .read_with(cx, |shell, cx| {
                shell
                    .views
                    .composer_pane
                    .read(cx)
                    .latency_probe()
                    .last_observed()
            })
            .expect("InputEvent::Change reaches the next ComposerPane render");
        input_to_render_samples.push(observed.as_micros());
    }

    let frame_p95_micros = test_percentile_95(&mut frame_samples);
    let input_roundtrip_p95_micros = test_percentile_95(&mut input_roundtrip_samples);
    let input_to_render_p95_micros = test_percentile_95(&mut input_to_render_samples);
    println!(
        "desktop_perf\tplatform={}\theadless_blocks={}\theadless_cpu_frame_p95_us={frame_p95_micros}\t\
             headless_input_roundtrip_p95_us={input_roundtrip_p95_micros}\t\
             input_change_to_render_p95_us={input_to_render_p95_micros}\t\
             window_rss_supported={}\twindow_rss_before_bytes={}\twindow_rss_after_bytes={}\t\
             window_rss_growth_bytes={}",
        std::env::consts::OS,
        desktop::ui::conversation::model::MAX_TRANSCRIPT_BLOCKS,
        window_rss_before.is_some() && window_rss_after.is_some(),
        window_rss_before.unwrap_or_default(),
        window_rss_after.unwrap_or_default(),
        window_rss_growth.unwrap_or_default()
    );
    assert!(
        frame_p95_micros <= CPU_FRAME_BUDGET_MICROS,
        "headless full-tree CPU frame P95 exceeded one frame: {frame_p95_micros} us"
    );
    assert!(
        input_roundtrip_p95_micros <= CPU_FRAME_BUDGET_MICROS,
        "headless Composer roundtrip P95 exceeded one frame: \
             {input_roundtrip_p95_micros} us"
    );
    assert!(
        input_to_render_p95_micros <= CPU_FRAME_BUDGET_MICROS,
        "Composer change-to-render P95 exceeded one frame: {input_to_render_p95_micros} us"
    );
    if let Some(window_rss_growth) = window_rss_growth {
        assert!(
            window_rss_growth <= WINDOW_RSS_GROWTH_BUDGET,
            "10k NativeShell window RSS growth exceeded 64 MiB: {window_rss_growth} bytes"
        );
    }
}

#[gpui::test]
#[ignore = "release performance gate"]
fn desktop_release_gpui_markdown_parser_matrix(cx: &mut TestAppContext) {
    let _performance_guard = crate::allocation_probe::serial_guard();
    const SAMPLE_COUNT: usize = 20;
    const MARKDOWN_PARSE_BUDGET_MICROS: u128 = 150_000;

    initialize_visual_test(cx);
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
        let preview = desktop::ui::conversation::markdown::bounded_markdown_preview(&payload);
        let bounded_bytes = preview.text.len();
        let mut samples = Vec::with_capacity(SAMPLE_COUNT);
        for _ in 0..SAMPLE_COUNT {
            let source = preview.text.clone();
            let started = Instant::now();
            let state =
                cx.update(|cx| cx.new(move |cx| TextViewState::markdown(source.as_str(), cx)));
            std::hint::black_box(state);
            samples.push(started.elapsed().as_micros());
        }
        let parse_p95_micros = test_percentile_95(&mut samples);
        println!(
            "desktop_perf\tcontent={name}\tinput_bytes={}\tbounded_bytes={bounded_bytes}\t\
                 markdown_parser_p95_us={parse_p95_micros}",
            payload.len()
        );
        assert!(
            parse_p95_micros <= MARKDOWN_PARSE_BUDGET_MICROS,
            "{name} GPUI Markdown parser P95 exceeded 150ms: {parse_p95_micros} us"
        );
    }
}
