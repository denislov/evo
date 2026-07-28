#!/usr/bin/env python3
"""Validate external click-to-photon samples and emit a release-gate artifact."""

from __future__ import annotations

import argparse
import csv
import math
import re
import sys
from pathlib import Path


RUN_ID_PATTERN = r"[A-Za-z0-9][A-Za-z0-9._-]{0,127}"
APP_INPUT_PATTERN = re.compile(
    rf"click_to_photon_input_received\trun=(?P<run>{RUN_ID_PATTERN})\t"
    r"sample=(?P<sample>[0-9]+)\tbright=(?P<bright>true|false)"
)
APP_POST_RENDER_PATTERN = re.compile(
    rf"click_to_photon_post_render\trun=(?P<run>{RUN_ID_PATTERN})\t"
    r"sample=(?P<sample>[0-9]+)\tbright=(?P<bright>true|false)\t"
)


def percentile(samples: list[int], value: int) -> int:
    ordered = sorted(samples)
    index = math.ceil(len(ordered) * value / 100) - 1
    return ordered[index]


def read_samples(path: Path) -> list[tuple[str, int, int]]:
    with path.open(newline="", encoding="utf-8-sig") as source:
        reader = csv.DictReader(source)
        required = {"run_id", "sample_id", "latency_us"}
        if reader.fieldnames is None or not required.issubset(reader.fieldnames):
            raise ValueError("CSV must contain run_id, sample_id and latency_us columns")

        samples: list[tuple[str, int, int]] = []
        seen: set[tuple[str, int]] = set()
        for line_number, row in enumerate(reader, start=2):
            run_id = (row["run_id"] or "").strip()
            if re.fullmatch(RUN_ID_PATTERN, run_id) is None:
                raise ValueError(f"line {line_number}: run_id is invalid")
            try:
                sample_id = int(row["sample_id"])
                latency_us = int(row["latency_us"])
            except (TypeError, ValueError) as error:
                raise ValueError(
                    f"line {line_number}: sample_id and latency_us must be integers"
                ) from error
            if sample_id <= 0:
                raise ValueError(f"line {line_number}: sample_id must be positive")
            if latency_us <= 0:
                raise ValueError(f"line {line_number}: latency_us must be positive")
            sample_key = (run_id, sample_id)
            if sample_key in seen:
                raise ValueError(
                    f"line {line_number}: duplicate sample_id {sample_id} for run {run_id}"
                )
            seen.add(sample_key)
            samples.append((run_id, sample_id, latency_us))
    run_ids = {run_id for run_id, _, _ in samples}
    if len(run_ids) > 1:
        raise ValueError("external CSV contains mixed run_id values")
    return samples


def read_app_samples(path: Path) -> dict[tuple[str, int], bool]:
    input_samples: dict[tuple[str, int], bool] = {}
    post_render_samples: dict[tuple[str, int], bool] = {}
    for line_number, line in enumerate(
        path.read_text(encoding="utf-8", errors="replace").splitlines(), start=1
    ):
        input_match = APP_INPUT_PATTERN.search(line)
        if input_match:
            key = (input_match.group("run"), int(input_match.group("sample")))
            if key in input_samples:
                raise ValueError(
                    f"app log line {line_number}: duplicate input sample {key[1]}"
                )
            input_samples[key] = input_match.group("bright") == "true"

        post_render_match = APP_POST_RENDER_PATTERN.search(line)
        if post_render_match:
            key = (
                post_render_match.group("run"),
                int(post_render_match.group("sample")),
            )
            if key in post_render_samples:
                raise ValueError(
                    f"app log line {line_number}: duplicate post-render sample {key[1]}"
                )
            post_render_samples[key] = post_render_match.group("bright") == "true"

    if not post_render_samples:
        raise ValueError("app log contains no click_to_photon_post_render samples")
    run_ids = {run_id for run_id, _ in post_render_samples}
    if len(run_ids) != 1:
        raise ValueError("app log contains mixed click-to-photon run IDs")
    for key, bright in post_render_samples.items():
        input_bright = input_samples.get(key)
        if input_bright is None:
            raise ValueError(
                f"app post-render sample {key[1]} has no matching input sample"
            )
        if input_bright != bright:
            raise ValueError(
                f"app sample {key[1]} changed bright state before post-render"
            )
    return post_render_samples


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description=(
            "Validate physical key-actuation-to-visible-photon measurements using "
            "the nearest-rank percentile used by the desktop release gates."
        )
    )
    parser.add_argument(
        "samples", type=Path, help="CSV with run_id,sample_id,latency_us"
    )
    parser.add_argument(
        "--platform", required=True, choices=("linux", "macos", "windows")
    )
    parser.add_argument(
        "--app-log",
        type=Path,
        required=True,
        help="click-to-photon app log used to verify every physical sample ID",
    )
    parser.add_argument("--min-samples", type=int, default=50)
    parser.add_argument("--budget-us", type=int, default=50_000)
    parser.add_argument("--refresh-hz", type=float, required=True)
    parser.add_argument("--output", type=Path)
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    repository_root = Path(__file__).resolve().parent.parent
    output = args.output or (
        repository_root
        / "target"
        / "desktop-perf"
        / f"click-to-photon-{args.platform}.log"
    )
    resolved_output = output.resolve()
    if resolved_output in {args.samples.resolve(), args.app_log.resolve()}:
        raise ValueError("--output must not overwrite the external CSV or app log")
    output.unlink(missing_ok=True)

    if args.min_samples < 1:
        raise ValueError("--min-samples must be positive")
    if args.budget_us < 1:
        raise ValueError("--budget-us must be positive")
    if args.refresh_hz <= 0:
        raise ValueError("--refresh-hz must be positive")

    samples = read_samples(args.samples)
    if len(samples) < args.min_samples:
        raise ValueError(
            f"expected at least {args.min_samples} external samples, found {len(samples)}"
        )
    run_id = samples[0][0]

    app_samples = read_app_samples(args.app_log)
    app_run_ids = {app_run_id for app_run_id, _ in app_samples}
    if app_run_ids != {run_id}:
        app_run_id = next(iter(app_run_ids))
        raise ValueError(
            f"external run_id {run_id} does not match app log run_id {app_run_id}"
        )
    missing = sorted(
        sample_id
        for external_run_id, sample_id, _ in samples
        if (external_run_id, sample_id) not in app_samples
    )
    if missing:
        preview = ", ".join(str(sample) for sample in missing[:10])
        raise ValueError(f"external samples missing from app log: {preview}")

    latencies = [latency for _, _, latency in samples]
    p95 = percentile(latencies, 95)
    if p95 > args.budget_us:
        print(
            f"external click-to-photon P95 exceeded budget: {p95} us > {args.budget_us} us",
            file=sys.stderr,
        )
        return 1

    fields = [
        "desktop_perf",
        f"platform={args.platform}",
        "measurement=external_click_to_photon",
        f"run_id={run_id}",
        f"samples={len(latencies)}",
        "paired_app_log=true",
        f"p50_us={percentile(latencies, 50)}",
        f"p95_us={p95}",
        f"p99_us={percentile(latencies, 99)}",
        f"min_us={min(latencies)}",
        f"max_us={max(latencies)}",
        f"p95_budget_us={args.budget_us}",
    ]
    fields.append(f"refresh_hz={args.refresh_hz:g}")
    row = "\t".join(fields)

    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text(row + "\n", encoding="utf-8")
    print(row)
    print(f"wrote {output}")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (OSError, ValueError) as error:
        print(f"desktop click-to-photon report failed: {error}", file=sys.stderr)
        raise SystemExit(2) from error
