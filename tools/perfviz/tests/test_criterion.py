"""Tests for Criterion result ingestion."""

from __future__ import annotations

import json
import tempfile
import unittest
from pathlib import Path

from perfviz.criterion import ingest_criterion


class CriterionIngestTests(unittest.TestCase):
    """Criterion parser tests."""

    def test_should_ingest_nested_criterion_benchmark_directories(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            new_dir = (
                root
                / "target"
                / "criterion"
                / "perf_optimization"
                / "lookup_subjects_streaming_1m"
                / "new"
            )
            new_dir.mkdir(parents=True)
            write_json(
                new_dir / "benchmark.json",
                {"full_id": "perf_optimization/lookup_subjects_streaming_1m"},
            )
            write_json(
                new_dir / "estimates.json",
                {
                    "slope": {
                        "point_estimate": 9.0,
                        "confidence_interval": {
                            "lower_bound": 8.0,
                            "upper_bound": 11.0,
                        },
                    },
                    "mean": {
                        "point_estimate": 12.0,
                        "confidence_interval": {
                            "lower_bound": 10.0,
                            "upper_bound": 15.0,
                        },
                    },
                },
            )

            records = ingest_criterion(
                root=root,
                criterion_dir=root / "target" / "criterion",
                run_id="run-1",
                phase="ci",
                timestamp="2026-05-25T00:00:00Z",
            )

        self.assertEqual(1, len(records))
        self.assertEqual("perf_optimization/lookup_subjects_streaming_1m", records[0].benchmark)
        self.assertEqual(11.0, records[0].upper)


def write_json(path: Path, value: object) -> None:
    """Write a JSON file for a Criterion fixture."""
    path.write_text(json.dumps(value), encoding="utf-8")


if __name__ == "__main__":
    unittest.main()
