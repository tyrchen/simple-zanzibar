#!/usr/bin/env python3
# /// script
# requires-python = ">=3.11"
# dependencies = []
# ///
"""Render a deterministic multi-chart performance impact dashboard."""

from __future__ import annotations

import html
import math
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
OUTPUT = ROOT / "docs" / "perf" / "phase-12-impact.svg"

WIDTH = 1800
HEIGHT = 1360
PADDING = 48
PANEL_RADIUS = 8

BG = "#f7f8fb"
PANEL = "#ffffff"
TEXT = "#172033"
MUTED = "#667085"
GRID = "#d9dee8"
GREEN = "#14845c"
GREEN_LIGHT = "#dff4ea"
BLUE = "#2f6fed"
BLUE_LIGHT = "#dfe9ff"
RED = "#c2413a"
RED_LIGHT = "#fde8e7"
AMBER = "#b7791f"
AMBER_LIGHT = "#fff3d6"
PURPLE = "#7c3aed"
PURPLE_LIGHT = "#ede9fe"


def fmt_pct(value: float) -> str:
    return f"{value:+.1f}%"


def pct_change(before: float, current: float) -> float:
    return (current - before) / before * 100.0


def reduction(before: float, current: float) -> float:
    return (before - current) / before * 100.0


def esc(value: object) -> str:
    return html.escape(str(value), quote=True)


class Svg:
    def __init__(self) -> None:
        self.parts: list[str] = [
            f'<svg xmlns="http://www.w3.org/2000/svg" width="{WIDTH}" height="{HEIGHT}" '
            f'viewBox="0 0 {WIDTH} {HEIGHT}" role="img" aria-labelledby="title desc">',
            '<title id="title">Simple Zanzibar performance impact dashboard</title>',
            '<desc id="desc">Multi-chart summary of performance benchmark changes across recent '
            "optimization PRs.</desc>",
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
        family: str = "Inter, ui-sans-serif, system-ui, -apple-system, BlinkMacSystemFont, Segoe UI, sans-serif",
    ) -> None:
        self.add(
            f'<text x="{x:.1f}" y="{y:.1f}" font-size="{size}" font-weight="{weight}" '
            f'font-family="{family}" fill="{fill}" text-anchor="{anchor}">{esc(value)}</text>'
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
        opacity: float | None = None,
    ) -> None:
        stroke_attr = "" if stroke is None else f' stroke="{stroke}"'
        opacity_attr = "" if opacity is None else f' opacity="{opacity:.3f}"'
        self.add(
            f'<rect x="{x:.1f}" y="{y:.1f}" width="{width:.1f}" height="{height:.1f}" '
            f'rx="{radius:.1f}" fill="{fill}"{stroke_attr}{opacity_attr}/>'
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

    def circle(self, x: float, y: float, radius: float, *, fill: str) -> None:
        self.add(f'<circle cx="{x:.1f}" cy="{y:.1f}" r="{radius:.1f}" fill="{fill}"/>')

    def finish(self) -> str:
        self.parts.append("</svg>")
        return "\n".join(self.parts) + "\n"


def panel(svg: Svg, x: float, y: float, width: float, height: float, title: str, subtitle: str) -> None:
    svg.rect(x, y, width, height, fill=PANEL, stroke="#e7eaf0", radius=PANEL_RADIUS)
    svg.text(x + 24, y + 36, title, size=22, weight=700)
    svg.text(x + 24, y + 62, subtitle, size=14, fill=MUTED)


def draw_speedup_bars(svg: Svg, x: float, y: float, width: float, height: float) -> None:
    data = [
        ("single write", 122.61),
        ("mixed batch", 3.11),
        ("light R/W", 130.59),
        ("medium unbatched", 138.49),
        ("medium batched", 98.47),
        ("heavy unbatched", 99.93),
        ("heavy batched", 93.82),
    ]
    panel(svg, x, y, width, height, "Phase 12 write amplification removed", "Upper estimate speedup vs pre-segmentation baseline")
    chart_x = x + 210
    chart_y = y + 90
    chart_w = width - 270
    row_h = 39
    max_log = math.log10(150.0)
    for tick in [1, 3, 10, 30, 100]:
        tx = chart_x + math.log10(tick) / max_log * chart_w
        svg.line(tx, chart_y - 12, tx, chart_y + row_h * len(data) - 8)
        svg.text(tx, chart_y - 20, f"{tick}x", size=12, fill=MUTED, anchor="middle")
    for index, (label, speedup) in enumerate(data):
        cy = chart_y + index * row_h
        bar_w = math.log10(speedup) / max_log * chart_w
        svg.text(x + 24, cy + 21, label, size=14, fill=TEXT)
        svg.rect(chart_x, cy + 3, chart_w, 20, fill="#f1f4f9", radius=5)
        svg.rect(chart_x, cy + 3, bar_w, 20, fill=GREEN, radius=5)
        svg.text(chart_x + bar_w + 8, cy + 19, f"{speedup:.1f}x", size=13, weight=700, fill=GREEN)


def draw_delta_bars(svg: Svg, x: float, y: float, width: float, height: float) -> None:
    data = [
        ("prepared check", -4.52),
        ("lookup resources", -2.45),
        ("lookup subjects", -1.59),
        ("phase-timed load", -2.39),
        ("full load", 4.46),
        ("trusted load", 2.12),
        ("realworld inherited", -2.98),
        ("realworld mixed", 1.83),
    ]
    panel(svg, x, y, width, height, "Latency delta by benchmark family", "Upper estimate change. Negative is faster")
    chart_x = x + 190
    chart_y = y + 86
    chart_w = width - 240
    center = chart_x + chart_w / 2
    row_h = 31
    scale = chart_w / 2 / 6.0
    for tick in [-6, -3, 0, 3, 6]:
        tx = center + tick * scale
        svg.line(tx, chart_y - 10, tx, chart_y + row_h * len(data) - 6, stroke="#cfd6e2" if tick == 0 else GRID)
        svg.text(tx, chart_y - 18, f"{tick:+d}%", size=11, fill=MUTED, anchor="middle")
    for index, (label, change) in enumerate(data):
        cy = chart_y + index * row_h
        color = GREEN if change <= 0 else RED
        svg.text(x + 24, cy + 18, label, size=13, fill=TEXT)
        if change < 0:
            bar_x = center + change * scale
            bar_w = -change * scale
        else:
            bar_x = center
            bar_w = change * scale
        svg.rect(bar_x, cy + 4, max(bar_w, 2), 17, fill=color, radius=4)
        svg.text(chart_x + chart_w + 10, cy + 18, fmt_pct(change), size=12, weight=700, fill=color)


def draw_snapshot_bars(svg: Svg, x: float, y: float, width: float, height: float) -> None:
    data = [
        ("full load before", 564.57, BLUE_LIGHT),
        ("full load current", 589.75, RED),
        ("phase timer before", 572.88, BLUE_LIGHT),
        ("phase timer current", 559.16, BLUE),
        ("trusted before", 135.47, GREEN_LIGHT),
        ("trusted current", 138.34, GREEN),
    ]
    panel(svg, x, y, width, height, "Snapshot load impact", "Upper estimate in ms; hard trusted gate is 200 ms")
    chart_x = x + 36
    chart_y = y + 98
    chart_w = width - 76
    chart_h = 230
    label_w = 150
    bar_area_x = chart_x + label_w
    bar_area_w = chart_w - label_w
    max_value = 620.0
    for tick in [0, 200, 450, 600]:
        tx = bar_area_x + tick / max_value * bar_area_w
        svg.line(tx, chart_y - 12, tx, chart_y + chart_h)
        svg.text(tx, chart_y + chart_h + 22, f"{tick} ms", size=12, fill=MUTED, anchor="middle")
    svg.line(bar_area_x + 200 / max_value * bar_area_w, chart_y - 16, bar_area_x + 200 / max_value * bar_area_w, chart_y + chart_h, stroke=GREEN, width=2)
    svg.line(bar_area_x + 450 / max_value * bar_area_w, chart_y - 16, bar_area_x + 450 / max_value * bar_area_w, chart_y + chart_h, stroke=AMBER, width=2)
    row_h = 34
    for index, (label, value, color) in enumerate(data):
        cy = chart_y + index * row_h
        bar_w = value / max_value * bar_area_w
        svg.text(chart_x, cy + 18, label, size=13, fill=TEXT)
        svg.rect(bar_area_x, cy + 3, bar_area_w, 18, fill="#f1f4f9", radius=4)
        svg.rect(bar_area_x, cy + 3, max(bar_w, 2), 18, fill=color, radius=4)
        svg.text(bar_area_x + max(bar_w, 2) + 8, cy + 18, f"{value:.2f}", size=12, weight=700, fill=TEXT)


def draw_reduction_tiles(svg: Svg, x: float, y: float, width: float, height: float) -> None:
    data = [
        ("M6 100k RSS", reduction(324.0, 71.8), "324 MiB -> 71.8 MiB"),
        ("M6 1M RSS", reduction(3.12 * 1024, 368.0), "3.12 GiB -> 368 MiB"),
        ("raw -> zstd", reduction(124_422_241, 33_162_371), "124.4 MB -> 33.2 MB"),
        ("Full -> CheckOnly", reduction(124_422_114, 78_188_326), "124.4 MB -> 78.2 MB"),
    ]
    panel(svg, x, y, width, height, "Earlier PR size and memory wins", "Percent reduction from recorded spec evidence")
    tile_w = (width - 72) / 2
    tile_h = 92
    for index, (label, value, detail) in enumerate(data):
        tx = x + 24 + (index % 2) * (tile_w + 24)
        ty = y + 92 + (index // 2) * (tile_h + 24)
        svg.rect(tx, ty, tile_w, tile_h, fill=GREEN_LIGHT, stroke="#b7e7cf", radius=8)
        svg.text(tx + 18, ty + 28, label, size=15, weight=700, fill=TEXT)
        svg.text(tx + 18, ty + 60, f"{value:.1f}% smaller", size=24, weight=800, fill=GREEN)
        svg.text(tx + 18, ty + 80, detail, size=12, fill=MUTED)


def draw_phase_stack(svg: Svg, x: float, y: float, width: float, height: float) -> None:
    phases = [
        ("file read", 12.007417, BLUE),
        ("checksum", 53.784833, PURPLE),
        ("symbols", 80.906917, AMBER),
        ("rows", 299.240541, RED),
        ("indexes", 106.616042, GREEN),
        ("other", 0.048459, "#8a94a6"),
    ]
    total = sum(value for _, value, _ in phases)
    panel(svg, x, y, width, height, "Full-load bottleneck breakdown", "Representative phase timer. Rows plus indexes dominate")
    bar_x = x + 36
    bar_y = y + 110
    bar_w = width - 72
    start = bar_x
    for label, value, color in phases:
        width_part = value / total * bar_w
        svg.rect(start, bar_y, width_part, 48, fill=color, radius=4)
        if width_part > 58:
            svg.text(start + width_part / 2, bar_y + 30, f"{value:.0f}", size=13, weight=700, fill="#ffffff", anchor="middle")
        start += width_part
    legend_x = x + 36
    legend_y = bar_y + 88
    for index, (label, value, color) in enumerate(phases):
        lx = legend_x + (index % 3) * 205
        ly = legend_y + (index // 3) * 30
        svg.circle(lx, ly - 5, 6, fill=color)
        svg.text(lx + 14, ly, f"{label}: {value:.1f} ms", size=13, fill=TEXT)
    svg.text(x + width - 36, y + height - 30, f"total tracked: {total:.1f} ms", size=14, weight=700, fill=TEXT, anchor="end")


def draw_gate_tiles(svg: Svg, x: float, y: float, width: float, height: float) -> None:
    data = [
        ("single write 3x", "PASS", GREEN, GREEN_LIGHT),
        ("mixed batch 3x", "PASS", GREEN, GREEN_LIGHT),
        ("trusted load <200ms", "PASS", GREEN, GREEN_LIGHT),
        ("CheckOnly size -20%", "PASS", GREEN, GREEN_LIGHT),
        ("full load <450ms", "MISS", RED, RED_LIGHT),
        ("mixed read <55us", "MISS", RED, RED_LIGHT),
        ("profile RSS evidence", "TODO", AMBER, AMBER_LIGHT),
    ]
    panel(svg, x, y, width, height, "Gate status", "Measured pass/fail after PR #12")
    tile_w = (width - 72) / 2
    tile_h = 50
    for index, (label, status, color, fill) in enumerate(data):
        tx = x + 24 + (index % 2) * (tile_w + 24)
        ty = y + 86 + (index // 2) * (tile_h + 14)
        svg.rect(tx, ty, tile_w, tile_h, fill=fill, stroke=color, radius=8)
        svg.text(tx + 14, ty + 31, label, size=13, fill=TEXT)
        svg.text(tx + tile_w - 14, ty + 31, status, size=13, weight=800, fill=color, anchor="end")


def main() -> None:
    svg = Svg()
    svg.text(PADDING, 58, "Simple Zanzibar perf impact across recent optimization PRs", size=34, weight=800)
    svg.text(
        PADDING,
        88,
        "Exact SVG rendered from recorded Criterion/spec values. Phase 12 data uses upper estimates for comparisons.",
        size=16,
        fill=MUTED,
    )
    svg.text(WIDTH - PADDING, 58, "PR #12 / Phase 12", size=18, weight=700, fill=BLUE, anchor="end")

    col_w = (WIDTH - PADDING * 2 - 28) / 2
    left = PADDING
    right = PADDING + col_w + 28
    draw_speedup_bars(svg, left, 124, col_w, 390)
    draw_delta_bars(svg, right, 124, col_w, 390)
    draw_snapshot_bars(svg, left, 542, col_w, 370)
    draw_reduction_tiles(svg, right, 542, col_w, 370)
    draw_phase_stack(svg, left, 940, col_w, 340)
    draw_gate_tiles(svg, right, 940, col_w, 340)

    svg.text(
        PADDING,
        HEIGHT - 34,
        "Sources: specs/71-performance-budgets-design.md and PR #12 benchmark runs. Lower latency and smaller size are better.",
        size=13,
        fill=MUTED,
    )

    OUTPUT.parent.mkdir(parents=True, exist_ok=True)
    OUTPUT.write_text(svg.finish(), encoding="utf-8")
    print(OUTPUT.relative_to(ROOT))


if __name__ == "__main__":
    main()
