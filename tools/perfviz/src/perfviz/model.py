"""Perf history data model and persistence."""

from __future__ import annotations

import json
from dataclasses import asdict, dataclass
from pathlib import Path
from typing import Any


SCHEMA_VERSION = 1


@dataclass(frozen=True)
class BenchmarkRecord:
    """One normalized benchmark observation."""

    schema_version: int
    run_id: str
    phase: str
    timestamp: str
    source: str
    benchmark: str
    scenario: str
    metric: str
    unit: str
    lower: float
    estimate: float
    upper: float
    lower_is_better: bool
    importance: int
    tags: tuple[str, ...]
    git_sha: str | None = None

    @classmethod
    def from_json(cls, data: dict[str, Any]) -> "BenchmarkRecord":
        """Build a record from one `ndjson` object."""
        schema_version = int(data["schema_version"])
        if schema_version != SCHEMA_VERSION:
            msg = f"unsupported schema_version {schema_version}"
            raise ValueError(msg)
        lower_is_better = data["lower_is_better"]
        if not isinstance(lower_is_better, bool):
            msg = "lower_is_better must be a JSON boolean"
            raise ValueError(msg)
        return cls(
            schema_version=schema_version,
            run_id=str(data["run_id"]),
            phase=str(data["phase"]),
            timestamp=str(data["timestamp"]),
            source=str(data["source"]),
            benchmark=str(data["benchmark"]),
            scenario=str(data["scenario"]),
            metric=str(data["metric"]),
            unit=str(data["unit"]),
            lower=float(data["lower"]),
            estimate=float(data["estimate"]),
            upper=float(data["upper"]),
            lower_is_better=lower_is_better,
            importance=int(data["importance"]),
            tags=tuple(str(tag) for tag in data.get("tags", [])),
            git_sha=data.get("git_sha"),
        )

    def to_json(self) -> dict[str, Any]:
        """Return a stable JSON object."""
        data = asdict(self)
        data["tags"] = list(self.tags)
        return data


def read_history(path: Path) -> list[BenchmarkRecord]:
    """Read benchmark history from an `ndjson` file."""
    if not path.exists():
        return []
    records: list[BenchmarkRecord] = []
    for line_number, line in enumerate(path.read_text(encoding="utf-8").splitlines(), start=1):
        stripped = line.strip()
        if not stripped:
            continue
        try:
            records.append(BenchmarkRecord.from_json(json.loads(stripped)))
        except (KeyError, TypeError, ValueError, json.JSONDecodeError) as error:
            msg = f"{path}:{line_number}: invalid benchmark history record: {error}"
            raise ValueError(msg) from error
    return records


def write_history(path: Path, records: list[BenchmarkRecord]) -> None:
    """Write benchmark history as stable `ndjson`."""
    path.parent.mkdir(parents=True, exist_ok=True)
    ordered = sorted(records, key=lambda item: (item.timestamp, item.run_id, item.benchmark))
    lines = [
        json.dumps(record.to_json(), sort_keys=True, separators=(",", ":")) for record in ordered
    ]
    path.write_text("\n".join(lines) + "\n", encoding="utf-8")


def upsert_records(
    existing: list[BenchmarkRecord],
    incoming: list[BenchmarkRecord],
) -> list[BenchmarkRecord]:
    """Replace records with the same run and benchmark, append new ones."""
    by_key = {(record.run_id, record.benchmark): record for record in existing}
    for record in incoming:
        by_key[(record.run_id, record.benchmark)] = record
    return list(by_key.values())
