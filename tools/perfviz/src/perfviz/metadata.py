"""Benchmark chart metadata.

Historical measurements live in `docs/perf/history.ndjson`; this module intentionally contains
only labels, chart grouping, and display policy.
"""

from __future__ import annotations

from dataclasses import dataclass


@dataclass(frozen=True)
class BenchmarkSpec:
    """Chart metadata for one benchmark."""

    benchmark: str
    label: str
    scenario: str
    metric: str
    lower_is_better: bool
    importance: int
    tags: tuple[str, ...]


IMPORTANT_BENCHMARKS: dict[str, BenchmarkSpec] = {
    "perf_optimization/check_prepared_1m": BenchmarkSpec(
        "perf_optimization/check_prepared_1m",
        "prepared check",
        "Prepared check micro hot path",
        "latency",
        True,
        100,
        ("read", "core"),
    ),
    "perf_optimization/lookup_resources_streaming_1m": BenchmarkSpec(
        "perf_optimization/lookup_resources_streaming_1m",
        "lookup resources",
        "Streaming resource lookup micro fixture",
        "latency",
        True,
        100,
        ("read", "lookup"),
    ),
    "perf_optimization/lookup_subjects_streaming_1m": BenchmarkSpec(
        "perf_optimization/lookup_subjects_streaming_1m",
        "lookup subjects",
        "Streaming subject lookup micro fixture",
        "latency",
        True,
        95,
        ("read", "lookup"),
    ),
    "realworld_authorization/1m_rules/check_doc_inherited_workspace_member": BenchmarkSpec(
        "realworld_authorization/1m_rules/check_doc_inherited_workspace_member",
        "realworld inherited",
        "Realistic inherited permission check",
        "latency",
        True,
        90,
        ("read", "realworld"),
    ),
    "perf_optimization/read_heavy_heavy_write_batched_1m": BenchmarkSpec(
        "perf_optimization/read_heavy_heavy_write_batched_1m",
        "heavy-write read",
        "Read latency while heavy batched writes publish",
        "latency",
        True,
        85,
        ("read", "write-mix"),
    ),
    "snapshot_load_compact/1m": BenchmarkSpec(
        "snapshot_load_compact/1m",
        "full snapshot load",
        "Full compact snapshot load",
        "latency",
        True,
        80,
        ("snapshot", "load"),
    ),
    "snapshot_load_trusted_fast/1m": BenchmarkSpec(
        "snapshot_load_trusted_fast/1m",
        "trusted fast load",
        "Trusted-fast snapshot load",
        "latency",
        True,
        80,
        ("snapshot", "load"),
    ),
    "snapshot_section_size/full_1m/total_bytes:zstd": BenchmarkSpec(
        "snapshot_section_size/full_1m/total_bytes:zstd",
        "Full zstd bytes",
        "Full-profile compressed artifact size",
        "size",
        True,
        70,
        ("snapshot", "size"),
    ),
    "snapshot_section_size/check_only_1m/total_bytes:raw": BenchmarkSpec(
        "snapshot_section_size/check_only_1m/total_bytes:raw",
        "CheckOnly raw bytes",
        "CheckOnly raw artifact size",
        "size",
        True,
        70,
        ("snapshot", "size"),
    ),
    "realworld_authorization/1m_rules/lookup_resources_target_user": BenchmarkSpec(
        "realworld_authorization/1m_rules/lookup_resources_target_user",
        "realworld lookup resources",
        "Tuple-to-userset resource lookup",
        "latency",
        True,
        70,
        ("read", "realworld", "correctness-sensitive"),
    ),
    "realworld_authorization/1m_rules/mixed_read_workload": BenchmarkSpec(
        "realworld_authorization/1m_rules/mixed_read_workload",
        "realworld mixed read",
        "Mixed check/lookup/expand workload",
        "latency",
        True,
        70,
        ("read", "realworld", "correctness-sensitive"),
    ),
}
