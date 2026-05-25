"""Command line interface for continuous performance history."""

from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
from pathlib import Path

from perfviz.criterion import default_run_id, ingest_criterion
from perfviz.metadata import IMPORTANT_BENCHMARKS
from perfviz.model import read_history, upsert_records, write_history
from perfviz.render import render_all


def main(argv: list[str] | None = None) -> int:
    """Run the `perfviz` command line interface."""
    parser = build_parser()
    args = parser.parse_args(argv)
    try:
        return int(args.func(args))
    except ValueError as error:
        print(f"perfviz: {error}", file=sys.stderr)
        return 2


def build_parser() -> argparse.ArgumentParser:
    """Build the top-level parser."""
    parser = argparse.ArgumentParser(
        prog="perfviz",
        description="Manage benchmark history.ndjson and render SVG performance charts.",
    )
    parser.add_argument(
        "--root",
        type=Path,
        default=None,
        help="Repository root. Defaults to git rev-parse or the nearest Cargo.toml parent.",
    )
    subcommands = parser.add_subparsers(required=True)

    render_parser = subcommands.add_parser("render", help="Render all SVG charts from history.")
    render_parser.add_argument("--history", type=Path, default=None, help="History ndjson path.")
    render_parser.add_argument("--output-dir", type=Path, default=None, help="SVG output directory.")
    render_parser.add_argument(
        "--check",
        action="store_true",
        help="Fail if generated SVG content differs from files on disk.",
    )
    render_parser.set_defaults(func=render_command)

    ingest_parser = subcommands.add_parser(
        "ingest-criterion",
        help="Append or replace one Criterion run in history.ndjson.",
    )
    ingest_parser.add_argument(
        "--criterion-dir",
        type=Path,
        default=None,
        help="Criterion output directory. Defaults to Cargo's active target directory.",
    )
    ingest_parser.add_argument("--history", type=Path, default=None, help="History ndjson path.")
    ingest_parser.add_argument("--phase", default="ci", help="Phase/run label for ingested records.")
    ingest_parser.add_argument("--run-id", default=None, help="Stable run id. Defaults to GitHub/local sha.")
    ingest_parser.add_argument("--timestamp", default=None, help="UTC ISO-8601 timestamp override.")
    ingest_parser.add_argument(
        "--allow-empty",
        action="store_true",
        help="Do not fail when no important Criterion records are found.",
    )
    ingest_parser.set_defaults(func=ingest_command)

    list_parser = subcommands.add_parser("list-important", help="List tracked benchmark ids.")
    list_parser.set_defaults(func=list_important_command)
    return parser


def render_command(args: argparse.Namespace) -> int:
    """Render SVGs and optionally verify checked-in files are current."""
    root = resolve_root(args.root)
    history_path = resolve_path(root, args.history, "docs/perf/history.ndjson")
    output_dir = resolve_path(root, args.output_dir, "docs/perf")
    records = read_history(history_path)
    if not records:
        msg = f"{history_path}: no benchmark records found"
        raise ValueError(msg)

    rendered = render_all(records, output_dir)
    if args.check:
        changed = [
            path
            for path, content in rendered.items()
            if not path.exists() or path.read_text(encoding="utf-8") != content
        ]
        if changed:
            changed_list = "\n".join(str(path) for path in changed)
            msg = f"rendered SVGs are stale:\n{changed_list}"
            raise ValueError(msg)
        return 0

    for path, content in rendered.items():
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(content, encoding="utf-8")
        print(path)
    return 0


def ingest_command(args: argparse.Namespace) -> int:
    """Ingest Criterion output into history.ndjson."""
    root = resolve_root(args.root)
    criterion_dir = (
        default_criterion_dir(root)
        if args.criterion_dir is None
        else resolve_path(root, args.criterion_dir, "target/criterion")
    )
    history_path = resolve_path(root, args.history, "docs/perf/history.ndjson")
    run_id = args.run_id or default_run_id(root)
    incoming = ingest_criterion(
        root=root,
        criterion_dir=criterion_dir,
        run_id=run_id,
        phase=args.phase,
        timestamp=args.timestamp,
    )
    if not incoming and not args.allow_empty:
        msg = f"{criterion_dir}: no tracked Criterion records found"
        raise ValueError(msg)
    records = upsert_records(read_history(history_path), incoming)
    write_history(history_path, records)
    print(f"ingested {len(incoming)} records into {history_path}")
    return 0


def list_important_command(args: argparse.Namespace) -> int:
    """Print tracked benchmark ids in chart priority order."""
    del args
    for benchmark in sorted(
        IMPORTANT_BENCHMARKS.values(),
        key=lambda spec: (-spec.importance, spec.benchmark),
    ):
        print(f"{benchmark.benchmark}\t{benchmark.scenario}")
    return 0


def resolve_root(value: Path | None) -> Path:
    """Resolve the repository root."""
    if value is not None:
        return value.resolve()

    cwd = Path.cwd()
    root = git_root(cwd)
    if root is not None:
        return root
    for candidate in (cwd, *cwd.parents):
        if (candidate / "Cargo.toml").exists():
            return candidate
    return cwd


def git_root(cwd: Path) -> Path | None:
    """Return the current git root, if this process is inside one."""
    try:
        output = subprocess.check_output(
            ["git", "rev-parse", "--show-toplevel"],
            cwd=cwd,
            text=True,
            stderr=subprocess.DEVNULL,
        ).strip()
    except (OSError, subprocess.CalledProcessError):
        return None
    if not output:
        return None
    return Path(output).resolve()


def resolve_path(root: Path, value: Path | None, default: str) -> Path:
    """Resolve a CLI path relative to the repository root."""
    path = Path(default) if value is None else value
    if path.is_absolute():
        return path
    return root / path


def default_criterion_dir(root: Path) -> Path:
    """Return the Criterion directory for the active Cargo target directory."""
    env_target_dir = os.environ.get("CARGO_TARGET_DIR")
    if env_target_dir:
        return Path(env_target_dir).expanduser().resolve() / "criterion"

    try:
        output = subprocess.check_output(
            ["cargo", "metadata", "--format-version", "1", "--no-deps"],
            cwd=root,
            text=True,
            stderr=subprocess.DEVNULL,
        )
        metadata = json.loads(output)
        target_directory = metadata.get("target_directory")
        if isinstance(target_directory, str) and target_directory:
            return Path(target_directory).resolve() / "criterion"
    except (OSError, subprocess.CalledProcessError, json.JSONDecodeError):
        pass

    return root / "target" / "criterion"
