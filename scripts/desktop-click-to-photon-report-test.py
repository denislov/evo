#!/usr/bin/env python3
"""Executable failure-path tests for the external click-to-photon report."""

from __future__ import annotations

import csv
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


REPOSITORY_ROOT = Path(__file__).resolve().parent.parent
REPORT = REPOSITORY_ROOT / "scripts" / "desktop-click-to-photon-report.py"


class ClickToPhotonReportTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary_directory = tempfile.TemporaryDirectory()
        self.directory = Path(self.temporary_directory.name)
        self.samples = self.directory / "samples.csv"
        self.app_log = self.directory / "app.log"
        self.output = self.directory / "report.log"

    def tearDown(self) -> None:
        self.temporary_directory.cleanup()

    def write_samples(
        self,
        run_id: str,
        *,
        count: int = 50,
        latency_us: int = 12_000,
    ) -> None:
        with self.samples.open("w", newline="", encoding="utf-8") as destination:
            writer = csv.DictWriter(
                destination, fieldnames=("run_id", "sample_id", "latency_us")
            )
            writer.writeheader()
            for sample_id in range(1, count + 1):
                writer.writerow(
                    {
                        "run_id": run_id,
                        "sample_id": sample_id,
                        "latency_us": latency_us + sample_id,
                    }
                )

    def write_app_log(
        self,
        run_id: str,
        *,
        count: int = 50,
        duplicate_post_render: bool = False,
        omit_input_sample: int | None = None,
    ) -> None:
        lines = [f"desktop_trace\tclick_to_photon_run\trun={run_id}"]
        for sample_id in range(1, count + 1):
            bright = "true" if sample_id % 2 else "false"
            if sample_id != omit_input_sample:
                lines.append(
                    "desktop_trace\tclick_to_photon_input_received\t"
                    f"run={run_id}\tsample={sample_id}\tbright={bright}"
                )
            lines.append(
                "desktop_trace\tclick_to_photon_post_render\t"
                f"run={run_id}\tsample={sample_id}\tbright={bright}\t"
                "input_received_to_post_render_us=8000"
            )
        if duplicate_post_render:
            lines.append(
                "desktop_trace\tclick_to_photon_post_render\t"
                f"run={run_id}\tsample=1\tbright=true\t"
                "input_received_to_post_render_us=8000"
            )
        self.app_log.write_text("\n".join(lines) + "\n", encoding="utf-8")

    def run_report(
        self,
        *extra_arguments: str,
        output: Path | None = None,
        refresh_hz: str | None = "60",
    ) -> subprocess.CompletedProcess[str]:
        arguments = [
            sys.executable,
            str(REPORT),
            str(self.samples),
            "--platform",
            "linux",
            "--app-log",
            str(self.app_log),
        ]
        if refresh_hz is not None:
            arguments.extend(("--refresh-hz", refresh_hz))
        arguments.extend(("--output", str(output or self.output), *extra_arguments))
        return subprocess.run(
            arguments,
            check=False,
            capture_output=True,
            text=True,
        )

    def test_current_run_writes_a_paired_passing_artifact(self) -> None:
        self.write_samples("run-current")
        self.write_app_log("run-current")

        result = self.run_report()

        self.assertEqual(result.returncode, 0, result.stderr)
        artifact = self.output.read_text(encoding="utf-8")
        self.assertIn("measurement=external_click_to_photon", artifact)
        self.assertIn("run_id=run-current", artifact)
        self.assertIn("samples=50", artifact)
        self.assertIn("paired_app_log=true", artifact)
        self.assertIn("refresh_hz=60", artifact)

    def test_stale_run_rejects_and_removes_an_old_artifact(self) -> None:
        self.write_samples("run-old")
        self.write_app_log("run-current")
        self.output.write_text("stale passing artifact\n", encoding="utf-8")

        result = self.run_report()

        self.assertEqual(result.returncode, 2)
        self.assertIn("does not match app log run_id", result.stderr)
        self.assertFalse(self.output.exists())

    def test_duplicate_or_unpaired_app_samples_fail_closed(self) -> None:
        self.write_samples("run-current")
        for duplicate, omitted, expected in (
            (True, None, "duplicate post-render sample"),
            (False, 17, "has no matching input sample"),
        ):
            with self.subTest(expected=expected):
                self.write_app_log(
                    "run-current",
                    duplicate_post_render=duplicate,
                    omit_input_sample=omitted,
                )
                self.output.write_text("stale passing artifact\n", encoding="utf-8")

                result = self.run_report()

                self.assertEqual(result.returncode, 2)
                self.assertIn(expected, result.stderr)
                self.assertFalse(self.output.exists())

    def test_over_budget_fails_without_writing_an_artifact(self) -> None:
        self.write_samples("run-current", latency_us=60_000)
        self.write_app_log("run-current")
        self.output.write_text("stale passing artifact\n", encoding="utf-8")

        result = self.run_report()

        self.assertEqual(result.returncode, 1)
        self.assertIn("P95 exceeded budget", result.stderr)
        self.assertFalse(self.output.exists())

    def test_output_cannot_overwrite_input_evidence(self) -> None:
        self.write_samples("run-current")
        self.write_app_log("run-current")

        for evidence in (self.samples, self.app_log):
            with self.subTest(evidence=evidence.name):
                before = evidence.read_bytes()

                result = self.run_report(output=evidence)

                self.assertEqual(result.returncode, 2)
                self.assertIn("must not overwrite", result.stderr)
                self.assertEqual(evidence.read_bytes(), before)

    def test_refresh_rate_is_required_and_positive(self) -> None:
        self.write_samples("run-current")
        self.write_app_log("run-current")

        missing = self.run_report(refresh_hz=None)
        self.assertEqual(missing.returncode, 2)
        self.assertIn("--refresh-hz", missing.stderr)

        self.output.write_text("stale passing artifact\n", encoding="utf-8")
        non_positive = self.run_report(refresh_hz="0")
        self.assertEqual(non_positive.returncode, 2)
        self.assertIn("--refresh-hz must be positive", non_positive.stderr)
        self.assertFalse(self.output.exists())

    def test_short_duplicate_and_mixed_external_samples_are_rejected(self) -> None:
        self.write_app_log("run-current")
        cases = (
            (
                [
                    ("run-current", sample_id, 12_000)
                    for sample_id in range(1, 50)
                ],
                "expected at least 50 external samples",
            ),
            (
                [
                    ("run-current", sample_id, 12_000)
                    for sample_id in range(1, 51)
                ]
                + [("run-current", 1, 12_000)],
                "duplicate sample_id 1",
            ),
            (
                [
                    (
                        "run-current" if sample_id < 50 else "run-other",
                        sample_id,
                        12_000,
                    )
                    for sample_id in range(1, 51)
                ],
                "mixed run_id values",
            ),
        )
        for rows, expected in cases:
            with self.subTest(expected=expected):
                with self.samples.open(
                    "w", newline="", encoding="utf-8"
                ) as destination:
                    writer = csv.writer(destination)
                    writer.writerow(("run_id", "sample_id", "latency_us"))
                    writer.writerows(rows)
                self.output.write_text("stale passing artifact\n", encoding="utf-8")

                result = self.run_report()

                self.assertEqual(result.returncode, 2)
                self.assertIn(expected, result.stderr)
                self.assertFalse(self.output.exists())


if __name__ == "__main__":
    unittest.main()
