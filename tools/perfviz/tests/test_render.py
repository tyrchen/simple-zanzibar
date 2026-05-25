"""Tests for SVG rendering from checked-in history."""

from __future__ import annotations

import tempfile
import unittest
from pathlib import Path

from perfviz.model import read_history
from perfviz.render import render_all


class RenderTests(unittest.TestCase):
    """Rendering smoke tests."""

    def test_should_render_line_and_bar_svg_outputs_from_history(self) -> None:
        root = Path(__file__).resolve().parents[3]
        records = read_history(root / "docs/perf/history.ndjson")

        with tempfile.TemporaryDirectory() as directory:
            output_dir = Path(directory)
            rendered = render_all(records, output_dir)

        names = {path.name for path in rendered}
        self.assertIn("phase-15-phase-trends.svg", names)
        self.assertIn("phase-15-performance-deltas.svg", names)
        self.assertIn("continuous-performance-dashboard.svg", names)
        for content in rendered.values():
            self.assertIn("<svg", content)
            self.assertIn("</svg>", content)


if __name__ == "__main__":
    unittest.main()
