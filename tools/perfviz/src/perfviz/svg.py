"""Small deterministic SVG helper."""

from __future__ import annotations

import html

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
PANEL_RADIUS = 8
FONT = "Inter, ui-sans-serif, system-ui, -apple-system, BlinkMacSystemFont, Segoe UI, sans-serif"


def esc(value: object) -> str:
    """Escape SVG text."""
    return html.escape(str(value), quote=True)


class Svg:
    """Collects SVG fragments."""

    def __init__(self, width: int, height: int, title: str, desc: str) -> None:
        self.width = width
        self.height = height
        self.parts = [
            f'<svg xmlns="http://www.w3.org/2000/svg" width="{width}" height="{height}" '
            f'viewBox="0 0 {width} {height}" role="img" aria-labelledby="title desc">',
            f'<title id="title">{esc(title)}</title>',
            f'<desc id="desc">{esc(desc)}</desc>',
            f'<rect width="{width}" height="{height}" fill="{BG}"/>',
        ]

    def add(self, value: str) -> None:
        """Append raw SVG."""
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
        """Append a text node."""
        self.add(
            f'<text x="{x:.1f}" y="{y:.1f}" font-size="{size}" font-weight="{weight}" '
            f'font-family="{FONT}" fill="{fill}" text-anchor="{anchor}">{esc(value)}</text>'
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
        """Append a rectangle."""
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
        """Append a line."""
        self.add(
            f'<line x1="{x1:.1f}" y1="{y1:.1f}" x2="{x2:.1f}" y2="{y2:.1f}" '
            f'stroke="{stroke}" stroke-width="{width:.1f}"/>'
        )

    def polyline(self, points: list[tuple[float, float]], *, stroke: str) -> None:
        """Append a polyline."""
        point_text = " ".join(f"{x:.1f},{y:.1f}" for x, y in points)
        self.add(
            f'<polyline points="{point_text}" fill="none" stroke="{stroke}" '
            'stroke-width="4" stroke-linecap="round" stroke-linejoin="round"/>'
        )

    def circle(
        self,
        x: float,
        y: float,
        radius: float,
        *,
        fill: str,
        stroke: str = "#ffffff",
    ) -> None:
        """Append a circle."""
        self.add(
            f'<circle cx="{x:.1f}" cy="{y:.1f}" r="{radius:.1f}" fill="{fill}" '
            f'stroke="{stroke}" stroke-width="2"/>'
        )

    def finish(self) -> str:
        """Return the complete SVG document."""
        return "\n".join([*self.parts, "</svg>"]) + "\n"


def panel(svg: Svg, x: float, y: float, width: float, height: float, title: str, subtitle: str) -> None:
    """Draw a titled panel."""
    svg.rect(x, y, width, height, fill=PANEL, stroke="#e7eaf0", radius=PANEL_RADIUS)
    svg.text(x + 24, y + 38, title, size=24, weight=800)
    svg.text(x + 24, y + 66, subtitle, size=14, fill=MUTED)
