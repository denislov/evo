#!/usr/bin/env python3
"""Validate external click-to-photon samples and emit a release-gate artifact."""

from __future__ import annotations

import argparse
import csv
import math
import re
import sys
from pathlib import Path


APP_SAMPLE_PATTERN = re.compile(
    r"click_to_photon_post_render\tsample=(?P<sample>[0-9]+)\t"
)


def percentile(samples: list[int], value: int) -> int:
    ordered = sorted(samples)
    index = math.ceil(len(ordered) * value / 100) - 1
    return ordered[index]


def read_samples(path: Path) -> list[tuple[int, int]]:
    with path.open(newline="", encoding="utf-8-sig") as source:
        reader = csv.DictReader(source)
        required = {"sample_id", "latency_us"}
        if reader.fieldnames is None or not required.issubset(reader.fieldnames):
            raise ValueError("CSV must contain sample_id and latency_us columns")

        samples: list[tuple[int, int]] = []
        seen: set[int] = set()
        for line_number, row in enumerate(reader, start=2):
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
            if sample_id in seen:
                raise ValueError(f"line {line_number}: duplicate sample_id {sample_id}")
            seen.add(sample_id)
            samples.append((sample_id, latency_us))
    return samples


def read_app_samples(path: Path) -> set[int]:
    samples: set[int] = set()
    for line in path.read_text(encoding="utf-8", errors="replace").splitlines():
        match = APP_SAMPLE_PATTERN.search(line)
        if match:
            samples.add(int(match.group("sample")))
    if not samples:
        raise ValueError("app log contains no click_to_photon_post_render samples")
    return samples


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description=(
            "Validate physical key-actuation-to-visible-photon measurements using "
            "the nearest-rank percentile used by the desktop release gates."
        )
    )
    parser.add_argument("samples", type=Path, help="CSV with sample_id,latency_us")
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
    parser.add_argument("--refresh-hz", type=float)
    parser.add_argument("--output", type=Path)
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    if args.min_samples < 1:
        raise ValueError("--min-samples must be positive")
    if args.budget_us < 1:
        raise ValueError("--budget-us must be positive")
    if args.refresh_hz is not None and args.refresh_hz <= 0:
        raise ValueError("--refresh-hz must be positive")

    samples = read_samples(args.samples)
    if len(samples) < args.min_samples:
        raise ValueError(
            f"expected at least {args.min_samples} external samples, found {len(samples)}"
        )

    app_samples = read_app_samples(args.app_log)
    missing = sorted(sample_id for sample_id, _ in samples if sample_id not in app_samples)
    if missing:
        preview = ", ".join(str(sample) for sample in missing[:10])
        raise ValueError(f"external samples missing from app log: {preview}")

    latencies = [latency for _, latency in samples]
    fields = [
        "desktop_perf",
        f"platform={args.platform}",
        "measurement=external_click_to_photon",
        f"samples={len(latencies)}",
        "paired_app_log=true",
        f"p50_us={percentile(latencies, 50)}",
        f"p95_us={percentile(latencies, 95)}",
        f"p99_us={percentile(latencies, 99)}",
        f"min_us={min(latencies)}",
        f"max_us={max(latencies)}",
        f"p95_budget_us={args.budget_us}",
    ]
    if args.refresh_hz is not None:
        fields.append(f"refresh_hz={args.refresh_hz:g}")
    row = "\t".join(fields)

    repository_root = Path(__file__).resolve().parent.parent
    output = args.output or (
        repository_root
        / "target"
        / "desktop-perf"
        / f"click-to-photon-{args.platform}.log"
    )
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text(row + "\n", encoding="utf-8")
    print(row)
    print(f"wrote {output}")

    p95 = percentile(latencies, 95)
    if p95 > args.budget_us:
        print(
            f"external click-to-photon P95 exceeded budget: {p95} us > {args.budget_us} us",
            file=sys.stderr,
        )
        return 1
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (OSError, ValueError) as error:
        print(f"desktop click-to-photon report failed: {error}", file=sys.stderr)
        raise SystemExit(2) from error
