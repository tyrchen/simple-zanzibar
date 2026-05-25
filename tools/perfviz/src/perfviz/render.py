"""Render SVG charts from benchmark history."""

from __future__ import annotations

from pathlib import Path

from perfviz.metadata import IMPORTANT_BENCHMARKS
from perfviz.model import BenchmarkRecord
from perfviz.svg import AMBER, BLUE, GRAY, GREEN, GRID, MUTED, RED, TEAL, TEXT, Svg, panel

PHASES = ("P12", "P13", "P14", "P15")
PHASE_LABELS = {
    "P12": "structural",
    "P13": "zstd/read",
    "P14": "read base",
    "P15": "follow-up",
}
COLORS = [BLUE, RED, GREEN, TEAL, AMBER, "#7c3aed", GRAY]
READ_BENCHMARKS = (
    "perf_optimization/check_prepared_1m",
    "perf_optimization/lookup_resources_streaming_1m",
    "perf_optimization/lookup_subjects_streaming_1m",
    "realworld_authorization/1m_rules/check_doc_inherited_workspace_member",
    "perf_optimization/read_heavy_heavy_write_batched_1m",
)
SNAPSHOT_BENCHMARKS = (
    "snapshot_load_compact/1m",
    "snapshot_load_trusted_fast/1m",
    "snapshot_section_size/full_1m/total_bytes:zstd",
    "snapshot_section_size/check_only_1m/total_bytes:raw",
)


def latest_by_phase(records: list[BenchmarkRecord]) -> dict[tuple[str, str], BenchmarkRecord]:
    """Return the latest record for each phase and benchmark."""
    latest: dict[tuple[str, str], BenchmarkRecord] = {}
    for record in records:
        key = (record.phase, record.benchmark)
        if key not in latest or record.timestamp >= latest[key].timestamp:
            latest[key] = record
    return latest


def latest_ci_records(records: list[BenchmarkRecord]) -> list[BenchmarkRecord]:
    """Return latest records from non-phase CI/local runs."""
    return [
        record
        for record in sorted(records, key=lambda item: (item.timestamp, item.run_id))
        if record.phase not in PHASES
    ][-80:]


def pct_change(before: float, current: float) -> float:
    """Return percent change."""
    return (current - before) / before * 100.0


def fmt_value(value: float, unit: str) -> str:
    """Format a normalized record value."""
    if unit == "bytes":
        return f"{value / 1_000_000:.1f} MB"
    if value >= 1_000_000_000:
        return f"{value / 1_000_000_000:.2f} s"
    if value >= 1_000_000:
        return f"{value / 1_000_000:.2f} ms"
    if value >= 1_000:
        return f"{value / 1_000:.3f} us"
    return f"{value:.0f} ns"


def render_all(records: list[BenchmarkRecord], output_dir: Path) -> dict[Path, str]:
    """Render all SVG outputs."""
    return {
        output_dir / "phase-15-phase-trends.svg": render_phase_trends(records),
        output_dir / "phase-15-performance-deltas.svg": render_phase_deltas(records),
        output_dir / "continuous-performance-dashboard.svg": render_continuous_dashboard(records),
    }


def render_phase_trends(records: list[BenchmarkRecord]) -> str:
    """Render normalized line charts across phase evidence."""
    svg = Svg(
        1800,
        1120,
        "Phase 15 benchmark trends",
        "Line charts comparing key benchmark upper estimates across phases 12 through 15.",
    )
    svg.text(56, 58, "Phase 15 benchmark trends against previous phases", size=34, weight=800)
    svg.text(
        56,
        88,
        "Values are recorded upper estimates. P12 is normalized to 100; lower is better.",
        size=16,
        fill=MUTED,
    )
    draw_trend_panel(
        svg,
        56,
        126,
        830,
        590,
        "Read path trends",
        "Latency upper estimate, normalized to P12",
        records,
        READ_BENCHMARKS,
        55,
        130,
    )
    draw_trend_panel(
        svg,
        914,
        126,
        830,
        590,
        "Snapshot and artifact trends",
        "Load latency and byte size, normalized to P12",
        records,
        SNAPSHOT_BENCHMARKS,
        55,
        140,
    )
    draw_takeaways(svg, records, 56, 754, 1688, 300)
    svg.text(
        56,
        1086,
        "Sources: docs/perf/history.ndjson, updated by CI benchmark runs.",
        size=13,
        fill=MUTED,
    )
    return svg.finish()


def draw_trend_panel(
    svg: Svg,
    x: float,
    y: float,
    width: float,
    height: float,
    title: str,
    subtitle: str,
    records: list[BenchmarkRecord],
    benchmarks: tuple[str, ...],
    y_min: float,
    y_max: float,
) -> None:
    """Draw one normalized line chart panel."""
    panel(svg, x, y, width, height, title, subtitle)
    latest = latest_by_phase(records)
    chart_x = x + 82
    chart_y = y + 114
    chart_w = width - 324
    chart_h = height - 190
    step_x = chart_w / (len(PHASES) - 1)

    def sx(index: int) -> float:
        return chart_x + step_x * index

    def sy(value: float) -> float:
        return chart_y + (y_max - value) / (y_max - y_min) * chart_h

    for tick in (60, 80, 100, 120, 140):
        if y_min <= tick <= y_max:
            ty = sy(tick)
            svg.line(chart_x, ty, chart_x + chart_w, ty)
            svg.text(chart_x - 14, ty + 5, tick, size=12, fill=MUTED, anchor="end")
    svg.line(chart_x, chart_y, chart_x, chart_y + chart_h, stroke="#cfd6e2")
    svg.line(chart_x, chart_y + chart_h, chart_x + chart_w, chart_y + chart_h, stroke="#cfd6e2")
    for phase_index, phase in enumerate(PHASES):
        px = sx(phase_index)
        svg.line(px, chart_y, px, chart_y + chart_h, stroke="#edf0f5")
        svg.text(px, chart_y + chart_h + 28, phase, size=14, weight=800, anchor="middle")
        svg.text(px, chart_y + chart_h + 48, PHASE_LABELS[phase], size=10, fill=MUTED, anchor="middle")

    for bench_index, benchmark in enumerate(benchmarks):
        phase_records = [latest.get((phase, benchmark)) for phase in PHASES]
        if any(record is None for record in phase_records):
            continue
        base = phase_records[0].upper  # type: ignore[union-attr]
        color = COLORS[bench_index % len(COLORS)]
        points = [
            (sx(index), sy(record.upper / base * 100.0))  # type: ignore[union-attr]
            for index, record in enumerate(phase_records)
        ]
        svg.polyline(points, stroke=color)
        for px, py in points:
            svg.circle(px, py, 6, fill=color)

    legend_x = x + width - 220
    legend_y = y + 112
    svg.text(legend_x, legend_y - 20, "Current value", size=13, weight=800, fill=MUTED)
    for index, benchmark in enumerate(benchmarks):
        current = latest.get(("P15", benchmark))
        previous = latest.get(("P14", benchmark))
        first = latest.get(("P12", benchmark))
        if current is None or previous is None or first is None:
            continue
        spec = IMPORTANT_BENCHMARKS[benchmark]
        ly = legend_y + index * 42
        change_14 = pct_change(previous.upper, current.upper)
        change_12 = pct_change(first.upper, current.upper)
        change_color = GREEN if change_14 <= 0 else RED
        color = COLORS[index % len(COLORS)]
        svg.circle(legend_x, ly - 5, 6, fill=color, stroke=color)
        svg.text(legend_x + 16, ly, spec.label, size=12, weight=700)
        svg.text(legend_x + 16, ly + 17, fmt_value(current.upper, current.unit), size=11, fill=MUTED)
        svg.text(legend_x + 118, ly + 17, f"{change_14:+.1f}% vs P14", size=11, fill=change_color)
        svg.text(legend_x + 16, ly + 31, f"{change_12:+.1f}% vs P12", size=11, fill=MUTED)


def draw_takeaways(
    svg: Svg,
    records: list[BenchmarkRecord],
    x: float,
    y: float,
    width: float,
    height: float,
) -> None:
    """Draw summary notes."""
    latest = latest_by_phase(records)
    lookup_subjects = latest[("P15", "perf_optimization/lookup_subjects_streaming_1m")]
    lookup_subjects_base = latest[("P12", "perf_optimization/lookup_subjects_streaming_1m")]
    notes = [
        (
            "Comparable reads",
            f"lookup_subjects is {abs(pct_change(lookup_subjects_base.upper, lookup_subjects.upper)):.1f}% faster than P12.",
            TEAL,
        ),
        (
            "Correctness cost",
            "real-world lookup_resources now covers tuple-to-userset reverse traversal.",
            RED,
        ),
        (
            "Micro path held",
            "prepared check is flat versus P14 while remaining materially faster than P12.",
            AMBER,
        ),
        (
            "Continuous evidence",
            "CI appends Criterion records to history.ndjson and regenerates the SVGs.",
            BLUE,
        ),
    ]
    panel(svg, x, y, width, height, "Phase 15 reading", "Lower is better; comparisons use upper estimates")
    for index, (title, body, color) in enumerate(notes):
        ty = y + 104 + index * 72
        svg.rect(x + 30, ty - 30, 8, 54, fill=color, radius=4)
        svg.text(x + 56, ty - 8, title, size=15, weight=800)
        svg.text(x + 56, ty + 16, body, size=13, fill=MUTED)


def render_phase_deltas(records: list[BenchmarkRecord]) -> str:
    """Render bar chart deltas versus P14 and P12."""
    svg = Svg(
        1600,
        980,
        "Phase 15 performance deltas",
        "Bar charts comparing Phase 15 upper estimates with Phase 14 and Phase 12 baselines.",
    )
    svg.text(56, 58, "Phase 15 performance delta bars", size=34, weight=800)
    svg.text(56, 88, "Negative means faster or smaller. Correctness-sensitive rows are separated.", size=16, fill=MUTED)
    draw_delta_panel(
        svg,
        56,
        126,
        710,
        730,
        "P15 vs P14",
        "Latest follow-up against confirmed Phase 14 baseline",
        records,
        "P14",
        "P15",
    )
    draw_delta_panel(
        svg,
        836,
        126,
        710,
        730,
        "P15 vs P12",
        "Longer trend since structural read baseline",
        records,
        "P12",
        "P15",
    )
    svg.text(56, 936, "Sources: docs/perf/history.ndjson. Lower latency and smaller size are better.", size=13, fill=MUTED)
    return svg.finish()


def draw_delta_panel(
    svg: Svg,
    x: float,
    y: float,
    width: float,
    height: float,
    title: str,
    subtitle: str,
    records: list[BenchmarkRecord],
    before_phase: str,
    after_phase: str,
) -> None:
    """Draw horizontal delta bars."""
    panel(svg, x, y, width, height, title, subtitle)
    latest = latest_by_phase(records)
    benchmarks = (*READ_BENCHMARKS, *SNAPSHOT_BENCHMARKS)
    chart_x = x + 260
    chart_y = y + 96
    chart_w = width - 340
    center = chart_x + chart_w / 2
    scale = chart_w / 2 / 45.0
    row_h = 54
    for tick in (-45, -30, -15, 0, 15, 30, 45):
        tx = center + tick * scale
        svg.line(tx, chart_y - 18, tx, chart_y + row_h * len(benchmarks), stroke="#cfd6e2" if tick == 0 else GRID)
        svg.text(tx, chart_y - 26, f"{tick:+d}%", size=11, fill=MUTED, anchor="middle")
    for index, benchmark in enumerate(benchmarks):
        before = latest.get((before_phase, benchmark))
        after = latest.get((after_phase, benchmark))
        if before is None or after is None:
            continue
        spec = IMPORTANT_BENCHMARKS[benchmark]
        change = pct_change(before.upper, after.upper)
        color = GREEN if change <= 0 else RED
        cy = chart_y + index * row_h
        svg.text(x + 24, cy + 18, spec.label, size=13, weight=700)
        svg.text(x + 24, cy + 36, fmt_value(after.upper, after.unit), size=11, fill=MUTED)
        if change < 0:
            bar_x = center + max(change, -45.0) * scale
            bar_w = -max(change, -45.0) * scale
        else:
            bar_x = center
            bar_w = min(change, 45.0) * scale
        svg.rect(bar_x, cy + 6, max(bar_w, 2), 18, fill=color, radius=4)
        svg.text(chart_x + chart_w + 16, cy + 20, f"{change:+.1f}%", size=12, weight=800, fill=color)


def render_continuous_dashboard(records: list[BenchmarkRecord]) -> str:
    """Render a compact dashboard for phase and CI history."""
    svg = Svg(
        1600,
        900,
        "Continuous performance dashboard",
        "Dashboard generated from benchmark history ndjson.",
    )
    svg.text(56, 58, "Continuous performance benchmark dashboard", size=34, weight=800)
    svg.text(56, 88, "History is append-only by run id and rendered by the uv perfviz project.", size=16, fill=MUTED)
    draw_latest_table(svg, 56, 126, 710, 640, records)
    draw_ci_strip(svg, 836, 126, 710, 640, records)
    svg.text(56, 856, "CI path: cargo bench -> perfviz ingest-criterion -> docs/perf/history.ndjson -> SVG dashboards.", size=13, fill=MUTED)
    return svg.finish()


def draw_latest_table(svg: Svg, x: float, y: float, width: float, height: float, records: list[BenchmarkRecord]) -> None:
    """Draw latest P15 table."""
    panel(svg, x, y, width, height, "Important Phase 15 values", "Upper estimates from history.ndjson")
    latest = latest_by_phase(records)
    benchmarks = (*READ_BENCHMARKS, *SNAPSHOT_BENCHMARKS)
    row_y = y + 96
    for index, benchmark in enumerate(benchmarks):
        record = latest.get(("P15", benchmark))
        previous = latest.get(("P14", benchmark))
        if record is None or previous is None:
            continue
        spec = IMPORTANT_BENCHMARKS[benchmark]
        cy = row_y + index * 55
        change = pct_change(previous.upper, record.upper)
        color = GREEN if change <= 0 else RED
        svg.text(x + 28, cy, spec.label, size=13, weight=800)
        svg.text(x + 28, cy + 20, spec.scenario, size=11, fill=MUTED)
        svg.text(x + width - 170, cy, fmt_value(record.upper, record.unit), size=13, weight=800, anchor="end")
        svg.text(x + width - 28, cy, f"{change:+.1f}%", size=13, weight=800, fill=color, anchor="end")


def draw_ci_strip(svg: Svg, x: float, y: float, width: float, height: float, records: list[BenchmarkRecord]) -> None:
    """Draw CI run status strip."""
    panel(svg, x, y, width, height, "Continuous run history", "Most recent non-phase records")
    ci_records = latest_ci_records(records)
    if not ci_records:
        svg.text(x + 28, y + 126, "No CI benchmark run has been appended yet.", size=18, weight=700, fill=TEXT)
        svg.text(x + 28, y + 160, "The scheduled workflow will add records after it runs on master.", size=13, fill=MUTED)
        return
    row_y = y + 96
    for index, record in enumerate(ci_records[-9:]):
        spec = IMPORTANT_BENCHMARKS.get(record.benchmark)
        label = record.benchmark if spec is None else spec.label
        cy = row_y + index * 58
        svg.text(x + 28, cy, label, size=13, weight=800)
        svg.text(x + 28, cy + 20, f"{record.phase} / {record.run_id}", size=11, fill=MUTED)
        svg.text(x + width - 28, cy, fmt_value(record.upper, record.unit), size=13, weight=800, anchor="end")
