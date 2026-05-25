#!/usr/bin/env python3
# /// script
# requires-python = ">=3.11"
# dependencies = []
# ///
"""Render phase-to-phase benchmark trend lines from recorded evidence."""

from __future__ import annotations

import html
from dataclasses import dataclass
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
OUTPUT = ROOT / "docs" / "perf" / "phase-15-phase-trends.svg"

WIDTH = 1800
HEIGHT = 1120
PADDING = 56
PANEL_RADIUS = 8

BG = "#f7f8fb"
PANEL = "#ffffff"
TEXT = "#172033"
MUTED = "#667085"
GRID = "#d9dee8"
GREEN = "#14845c"
BLUE = "#2f6fed"
RED = "#c2413a"
AMBER = "#b7791f"
PURPLE = "#7c3aed"
TEAL = "#0f766e"
GRAY = "#64748b"

PHASES = [
    "P12",
    "P13",
    "P14",
    "P15",
]

PHASE_DESCRIPTIONS = [
    "structural read baseline",
    "zstd/read first pass",
    "read completion baseline",
    "current follow-up",
]


@dataclass(frozen=True)
class Series:
    """One benchmark trend series."""

    label: str
    values: tuple[float, float, float, float]
    unit: str
    color: str


READ_SERIES = [
    Series("prepared check", (6.0009, 5.3424, 4.3065, 4.4088), "us", BLUE),
    Series("lookup resources", (3.1883, 2.7544, 2.3551, 2.3713), "ms", RED),
    Series("lookup subjects", (6.3451, 5.4722, 4.7426, 3.9911), "us", GREEN),
    Series("realworld inherited", (15.110, 14.833, 11.559, 11.754), "us", TEAL),
    Series("realworld mixed", (58.164, 53.489, 41.808, 41.537), "us", PURPLE),
    Series("heavy-write read", (17.844, 12.365, 12.712, 13.003), "us", AMBER),
]

SNAPSHOT_SERIES = [
    Series("full snapshot load", (589.75, 553.98, 573.95, 564.88), "ms", BLUE),
    Series("trusted fast load", (138.34, 173.52, 177.49, 172.57), "ms", AMBER),
    Series("Full zstd bytes", (33_116_811, 21_471_681, 21_471_681, 21_471_681), "bytes", GREEN),
    Series("CheckOnly raw bytes", (78_188_326, 59_078_231, 59_078_231, 59_078_231), "bytes", TEAL),
]


def esc(value: object) -> str:
    return html.escape(str(value), quote=True)


def fmt_value(value: float, unit: str) -> str:
    if unit == "bytes":
        return f"{value / 1_000_000:.1f} MB"
    if unit == "ms":
        return f"{value:.2f} ms"
    return f"{value:.3f} us"


def pct_change(before: float, current: float) -> float:
    return (current - before) / before * 100.0


class Svg:
    def __init__(self) -> None:
        self.parts = [
            f'<svg xmlns="http://www.w3.org/2000/svg" width="{WIDTH}" height="{HEIGHT}" '
            f'viewBox="0 0 {WIDTH} {HEIGHT}" role="img" aria-labelledby="title desc">',
            '<title id="title">Phase 15 benchmark trends</title>',
            '<desc id="desc">Line charts comparing key benchmark upper estimates across phases 12 through 15.</desc>',
            f'<rect width="{WIDTH}" height="{HEIGHT}" fill="{BG}"/>',
        ]

    def add(self, value: str) -> None:
        self.parts.append(value)

    def text(
        self,
        x: float,
        y: float,
        value: object,
        *,
        size: int = 20,
        weight: int = 400,
        fill: str = TEXT,
        anchor: str = "start",
    ) -> None:
        self.add(
            f'<text x="{x:.1f}" y="{y:.1f}" font-size="{size}" font-weight="{weight}" '
            'font-family="Inter, ui-sans-serif, system-ui, -apple-system, BlinkMacSystemFont, '
            f'Segoe UI, sans-serif" fill="{fill}" text-anchor="{anchor}">{esc(value)}</text>'
        )

    def rect(
        self,
        x: float,
        y: float,
        width: float,
        height: float,
        *,
        fill: str,
        stroke: str | None = None,
        radius: float = 0,
    ) -> None:
        stroke_attr = "" if stroke is None else f' stroke="{stroke}"'
        self.add(
            f'<rect x="{x:.1f}" y="{y:.1f}" width="{width:.1f}" height="{height:.1f}" '
            f'rx="{radius:.1f}" fill="{fill}"{stroke_attr}/>'
        )

    def line(
        self,
        x1: float,
        y1: float,
        x2: float,
        y2: float,
        *,
        stroke: str = GRID,
        width: float = 1,
    ) -> None:
        self.add(
            f'<line x1="{x1:.1f}" y1="{y1:.1f}" x2="{x2:.1f}" y2="{y2:.1f}" '
            f'stroke="{stroke}" stroke-width="{width:.1f}"/>'
        )

    def polyline(self, points: list[tuple[float, float]], *, stroke: str) -> None:
        point_text = " ".join(f"{x:.1f},{y:.1f}" for x, y in points)
        self.add(
            f'<polyline points="{point_text}" fill="none" stroke="{stroke}" '
            'stroke-width="4" stroke-linecap="round" stroke-linejoin="round"/>'
        )

    def circle(self, x: float, y: float, radius: float, *, fill: str, stroke: str = "#ffffff") -> None:
        self.add(
            f'<circle cx="{x:.1f}" cy="{y:.1f}" r="{radius:.1f}" fill="{fill}" '
            f'stroke="{stroke}" stroke-width="2"/>'
        )

    def finish(self) -> str:
        self.parts.append("</svg>")
        return "\n".join(self.parts) + "\n"


def panel(svg: Svg, x: float, y: float, width: float, height: float, title: str, subtitle: str) -> None:
    svg.rect(x, y, width, height, fill=PANEL, stroke="#e7eaf0", radius=PANEL_RADIUS)
    svg.text(x + 24, y + 38, title, size=24, weight=800)
    svg.text(x + 24, y + 66, subtitle, size=14, fill=MUTED)


def normalized(value: float, base: float) -> float:
    return value / base * 100.0


def draw_trend_panel(
    svg: Svg,
    x: float,
    y: float,
    width: float,
    height: float,
    title: str,
    subtitle: str,
    series: list[Series],
    *,
    y_min: float,
    y_max: float,
) -> None:
    panel(svg, x, y, width, height, title, subtitle)

    chart_x = x + 82
    chart_y = y + 114
    chart_w = width - 324
    chart_h = height - 190
    step_x = chart_w / (len(PHASES) - 1)

    def sx(index: int) -> float:
        return chart_x + step_x * index

    def sy(value: float) -> float:
        return chart_y + (y_max - value) / (y_max - y_min) * chart_h

    for tick in [60, 80, 100, 120, 140]:
        if y_min <= tick <= y_max:
            ty = sy(tick)
            svg.line(chart_x, ty, chart_x + chart_w, ty)
            svg.text(chart_x - 14, ty + 5, f"{tick}", size=12, fill=MUTED, anchor="end")

    svg.line(chart_x, chart_y, chart_x, chart_y + chart_h, stroke="#cfd6e2")
    svg.line(chart_x, chart_y + chart_h, chart_x + chart_w, chart_y + chart_h, stroke="#cfd6e2")
    for index, (phase, description) in enumerate(zip(PHASES, PHASE_DESCRIPTIONS, strict=True)):
        px = sx(index)
        svg.line(px, chart_y, px, chart_y + chart_h, stroke="#edf0f5")
        svg.text(px, chart_y + chart_h + 28, phase, size=14, weight=800, anchor="middle")
        svg.text(px, chart_y + chart_h + 48, description, size=10, fill=MUTED, anchor="middle")

    for item in series:
        points = [(sx(index), sy(normalized(value, item.values[0]))) for index, value in enumerate(item.values)]
        svg.polyline(points, stroke=item.color)
        for px, py in points:
            svg.circle(px, py, 6, fill=item.color)

    legend_x = x + width - 220
    legend_y = y + 112
    svg.text(legend_x, legend_y - 20, "Current value", size=13, weight=800, fill=MUTED)
    for index, item in enumerate(series):
        ly = legend_y + index * 42
        change_14 = pct_change(item.values[-2], item.values[-1])
        change_12 = pct_change(item.values[0], item.values[-1])
        change_color = GREEN if change_14 <= 0 else RED
        svg.circle(legend_x, ly - 5, 6, fill=item.color, stroke=item.color)
        svg.text(legend_x + 16, ly, item.label, size=12, weight=700)
        svg.text(legend_x + 16, ly + 17, fmt_value(item.values[-1], item.unit), size=11, fill=MUTED)
        svg.text(legend_x + 118, ly + 17, f"{change_14:+.1f}% vs P14", size=11, fill=change_color)
        svg.text(legend_x + 16, ly + 31, f"{change_12:+.1f}% vs P12", size=11, fill=MUTED)


def draw_takeaways(svg: Svg, x: float, y: float, width: float, height: float) -> None:
    panel(svg, x, y, width, height, "Phase 15 reading", "Lower is better; comparisons use upper estimates")
    notes = [
        ("Best continuation", "lookup_subjects is 15.8% faster than P14 and 37.1% faster than P12.", GREEN),
        ("Realistic reads", "inherited and mixed real-world reads are within 1.7% of P14 and >22% faster than P12.", TEAL),
        ("Regression fixed", "lookup_resources_streaming is within 0.7% of P14 and 25.6% faster than P12.", RED),
        ("Micro path fixed", "prepared check is within 2.4% of P14 and 26.5% faster than P12.", AMBER),
        ("Snapshot", "full load is 1.6% faster than P14; compact zstd size holds the P14 result.", BLUE),
    ]
    for index, (title, body, color) in enumerate(notes):
        ty = y + 104 + index * 76
        svg.rect(x + 30, ty - 30, 8, 54, fill=color, radius=4)
        svg.text(x + 56, ty - 8, title, size=15, weight=800)
        svg.text(x + 56, ty + 16, body, size=13, fill=MUTED)


def main() -> None:
    svg = Svg()
    svg.text(PADDING, 58, "Phase 15 benchmark trends against previous phases", size=34, weight=800)
    svg.text(
        PADDING,
        88,
        "Values are recorded upper estimates from specs/71 and the current Phase 15 full benchmark run. P12 is normalized to 100.",
        size=16,
        fill=MUTED,
    )
    svg.text(WIDTH - PADDING, 58, "lower is better", size=16, weight=800, fill=GRAY, anchor="end")

    col_w = (WIDTH - PADDING * 2 - 28) / 2
    left = PADDING
    right = PADDING + col_w + 28
    draw_trend_panel(
        svg,
        left,
        126,
        col_w,
        590,
        "Read path trends",
        "Latency upper estimate, normalized to P12 = 100",
        READ_SERIES,
        y_min=60,
        y_max=130,
    )
    draw_trend_panel(
        svg,
        right,
        126,
        col_w,
        590,
        "Snapshot and artifact trends",
        "Load latency and byte size, normalized to P12 = 100",
        SNAPSHOT_SERIES,
        y_min=55,
        y_max=135,
    )
    draw_takeaways(svg, PADDING, 754, WIDTH - PADDING * 2, 300)
    svg.text(
        PADDING,
        HEIGHT - 34,
        "Sources: specs/71-performance-budgets-design.md and docs/perf/phase-15-complete-benchmark-2026-05-25.md.",
        size=13,
        fill=MUTED,
    )

    OUTPUT.parent.mkdir(parents=True, exist_ok=True)
    OUTPUT.write_text(svg.finish(), encoding="utf-8")
    print(OUTPUT.relative_to(ROOT))


if __name__ == "__main__":
    main()
