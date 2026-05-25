# Simple Zanzibar perfviz

`perfviz` keeps benchmark evidence in `docs/perf/history.ndjson` and renders deterministic SVG
charts from that history.

Common commands:

```bash
uv run --project tools/perfviz perfviz ingest-criterion --criterion-dir target/criterion
uv run --project tools/perfviz perfviz render
uv run --project tools/perfviz perfviz render --check
```

Historical phase records are data in `docs/perf/history.ndjson`, not Python code. CI uses
`render --check` on pull requests and the scheduled performance workflow appends Criterion results
before regenerating the SVGs.
