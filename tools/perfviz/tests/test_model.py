"""Tests for benchmark history persistence."""

from __future__ import annotations

import tempfile
import unittest
from pathlib import Path

from perfviz.model import BenchmarkRecord, read_history, upsert_records, write_history


class HistoryModelTests(unittest.TestCase):
    """Unit tests for NDJSON history storage."""

    def test_should_round_trip_history_as_ndjson(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "history.ndjson"
            record = sample_record("run-1", "bench/a", 10.0)

            write_history(path, [record])

            self.assertEqual([record], read_history(path))
            self.assertEqual(1, len(path.read_text(encoding="utf-8").splitlines()))

    def test_should_upsert_by_run_and_benchmark(self) -> None:
        original = sample_record("run-1", "bench/a", 10.0)
        replacement = sample_record("run-1", "bench/a", 8.0)
        other = sample_record("run-1", "bench/b", 5.0)

        records = upsert_records([original], [replacement, other])

        self.assertCountEqual([replacement, other], records)

    def test_should_reject_non_boolean_lower_is_better(self) -> None:
        data = sample_record("run-1", "bench/a", 10.0).to_json()
        data["lower_is_better"] = "true"

        with self.assertRaises(ValueError):
            BenchmarkRecord.from_json(data)


def sample_record(run_id: str, benchmark: str, upper: float) -> BenchmarkRecord:
    """Build a minimal benchmark history record."""
    return BenchmarkRecord(
        schema_version=1,
        run_id=run_id,
        phase="test",
        timestamp="2026-05-25T00:00:00Z",
        source="test",
        benchmark=benchmark,
        scenario="test scenario",
        metric="latency",
        unit="ns",
        lower=upper,
        estimate=upper,
        upper=upper,
        lower_is_better=True,
        importance=1,
        tags=("test",),
        git_sha=None,
    )


if __name__ == "__main__":
    unittest.main()
