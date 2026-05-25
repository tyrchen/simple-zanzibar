"""Criterion result ingestion."""

from __future__ import annotations

import json
import os
import subprocess
from datetime import UTC, datetime
from pathlib import Path
from typing import Any

from perfviz.metadata import IMPORTANT_BENCHMARKS
from perfviz.model import SCHEMA_VERSION, BenchmarkRecord


def git_sha(root: Path) -> str | None:
    """Return current git sha when available."""
    try:
        sha = subprocess.check_output(
            ["git", "rev-parse", "HEAD"],
            cwd=root,
            text=True,
            stderr=subprocess.DEVNULL,
        ).strip()
        status = subprocess.check_output(
            ["git", "status", "--short"],
            cwd=root,
            text=True,
            stderr=subprocess.DEVNULL,
        ).strip()
        return f"{sha}-dirty" if status else sha
    except (OSError, subprocess.CalledProcessError):
        return None


def default_run_id(root: Path) -> str:
    """Return a stable run id for the current execution context."""
    github_run_id = os.environ.get("GITHUB_RUN_ID")
    github_attempt = os.environ.get("GITHUB_RUN_ATTEMPT")
    if github_run_id:
        suffix = f"-{github_attempt}" if github_attempt else ""
        return f"github-{github_run_id}{suffix}"
    sha = git_sha(root)
    if sha is not None:
        return f"local-{sha[:12]}"
    return f"local-{datetime.now(UTC).strftime('%Y%m%d%H%M%S')}"


def ingest_criterion(
    *,
    root: Path,
    criterion_dir: Path,
    run_id: str,
    phase: str,
    timestamp: str | None = None,
) -> list[BenchmarkRecord]:
    """Read Criterion `estimates.json` files and normalize important benchmarks."""
    timestamp = datetime.now(UTC).replace(microsecond=0).isoformat().replace("+00:00", "Z") if timestamp is None else timestamp
    sha = git_sha(root)
    records: list[BenchmarkRecord] = []
    for estimate_path in sorted(criterion_dir.rglob("new/estimates.json")):
        benchmark_path = estimate_path.parent / "benchmark.json"
        if not benchmark_path.exists():
            continue
        benchmark = read_benchmark_id(benchmark_path)
        if benchmark not in IMPORTANT_BENCHMARKS:
            continue
        estimates = read_json(estimate_path)
        estimate = display_estimate(estimates)
        if estimate is None:
            continue
        interval = estimate.get("confidence_interval")
        if not isinstance(interval, dict):
            continue
        spec = IMPORTANT_BENCHMARKS[benchmark]
        records.append(
            BenchmarkRecord(
                schema_version=SCHEMA_VERSION,
                run_id=run_id,
                phase=phase,
                timestamp=timestamp,
                source=criterion_source(root, criterion_dir, estimate_path),
                benchmark=benchmark,
                scenario=spec.scenario,
                metric=spec.metric,
                unit="ns",
                lower=float(interval["lower_bound"]),
                estimate=float(estimate["point_estimate"]),
                upper=float(interval["upper_bound"]),
                lower_is_better=spec.lower_is_better,
                importance=spec.importance,
                tags=spec.tags,
                git_sha=sha,
            )
        )
    return records


def display_estimate(estimates: dict[str, Any]) -> dict[str, Any] | None:
    """Return the estimate Criterion prints in the benchmark summary."""
    slope = estimates.get("slope")
    if isinstance(slope, dict):
        return slope
    mean = estimates.get("mean")
    if isinstance(mean, dict):
        return mean
    return None


def criterion_source(root: Path, criterion_dir: Path, estimate_path: Path) -> str:
    """Return a repository-stable source label for one Criterion estimate file."""
    if estimate_path.is_relative_to(root):
        return str(estimate_path.relative_to(root))
    if estimate_path.is_relative_to(criterion_dir):
        return f"criterion/{estimate_path.relative_to(criterion_dir)}"
    return estimate_path.name


def read_benchmark_id(path: Path) -> str:
    """Read Criterion benchmark id."""
    data = read_json(path)
    full_id = data.get("full_id")
    if not isinstance(full_id, str):
        msg = f"{path}: missing string full_id"
        raise ValueError(msg)
    return full_id


def read_json(path: Path) -> dict[str, Any]:
    """Read one JSON object."""
    data = json.loads(path.read_text(encoding="utf-8"))
    if not isinstance(data, dict):
        msg = f"{path}: expected JSON object"
        raise ValueError(msg)
    return data
