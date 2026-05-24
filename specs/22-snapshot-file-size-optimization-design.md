# 22 - Snapshot File Size Optimization Design

Status: draft v1
Owner: Simple Zanzibar
Last updated: 2026-05-24
Depends on: [17](./17-compact-snapshot-format-design.md), [18](./18-trusted-fast-snapshot-load-design.md), [21](./21-performance-optimization-design.md), [71](./71-performance-budgets-design.md)

## 1. Purpose

Phase 12 reduced 1M `CheckOnly` snapshot size by omitting unused index groups, but the remaining
artifact is still 78.19 MB raw and 18.87 MB zstd. This spec defines the next file-size pass for
`.szsnap` artifacts. The goal is to reduce raw and compressed bytes without weakening the default
safe loader, without adding `unsafe`, and without making the hot read path depend on decompression.

This spec is deliberately section-driven: every format change must name the section bytes it reduces
and must rerun `make bench-snapshot-section-size`.

## 2. Evidence

Measured 2026-05-24 with `make bench-snapshot-section-size`; details are captured in
[../docs/research/spike-snapshot-section-size.md](../docs/research/spike-snapshot-section-size.md).

| Profile | Raw bytes | Zstd bytes | Index payload | Non-index payload | Saved vs Full |
| --- | ---: | ---: | ---: | ---: | ---: |
| `Full` | 124,422,114 | 33,116,811 | 63,867,328 | 60,554,402 | baseline |
| `CheckOnly` | 78,188,326 | 18,873,001 | 17,633,540 | 60,554,402 | 46,233,788 bytes / 37.15% |
| `CheckAndObjectAudit` | 78,188,326 | 18,873,001 | 17,633,540 | 60,554,402 | 46,233,788 bytes / 37.15% |

Full index payload by group:

| Group | Bytes | Main cause |
| --- | ---: | --- |
| `resource` | 17,633,400 | high-cardinality exact keys retained by every profile |
| `resource_object` | 17,633,380 | mostly duplicates exact resource object ids without relation |
| `resource_type_relation` | 4,000,140 | few keys, large overflow lists |
| `resource_type` | 4,000,060 | few keys, large overflow lists |
| `subject` | 13,933,444 | reverse lookup plus userset wildcard postings |
| `subject_type_relation` | 2,666,704 | userset-subject relation postings |
| `subject_type` | 4,000,060 | few keys, large overflow lists |

The current non-index lower bound is dominated by rows and symbols:

| Section | Bytes |
| --- | ---: |
| `relationship_rows` | 24,000,000 |
| `symbol_bytes` | 16,153,374 |
| `symbol_table` | 8,160,104 |
| `symbol_hashes` | 8,160,104 |
| `symbol_lookup` | 4,080,052 |

## 3. Goals

| # | Goal | Measure |
| --- | --- | --- |
| G1 | Reduce `Full` raw snapshot bytes materially. | 1M `Full` raw size <= 100 MB, or a section-size report explains why the next safe change is elsewhere. |
| G2 | Reduce `CheckOnly` raw bytes beyond index-profile omission. | 1M `CheckOnly` raw size <= 65 MB while preserving `check`, `expand`, and object-bounded audit behavior. |
| G3 | Keep startup profiles explicit. | `snapshot_load_trusted_fast/1m` remains <= 200 ms; default full validation regresses by no more than 5% unless a profile explains the tradeoff. |
| G4 | Preserve compatibility and failure clarity. | Unsupported snapshot versions, encodings, or profile requirements fail with typed `SnapshotIoError` / `EngineError` variants. |
| G5 | Keep measurement reproducible. | `make bench-snapshot-section-size` reports raw/zstd size, section bytes, index group bytes, postings, and saved percent for every profile. |

## 4. Non-Goals

- No mmap or `unsafe` in this pass.
- No changing default `Full` loader trust semantics to hit size or load targets.
- No replacing raw `TrustedFastLoad + External` with direct zstd loading as the fastest startup
  recommendation.
- No full-scan fallback for index profiles that omit required lookup indexes.
- No external compression dependency beyond existing `zstd` unless [60](./60-crates-features-design.md)
  and [99](./99-key-decisions.md) are updated after dependency review.

## 5. Target Shape

```text
+--------------------------------------------------------------+
| .szsnap v3                                                   |
|                                                              |
|  Header: version + index profile + section encoding profile  |
|  Directory: fixed bounds for every section                   |
|                                                              |
|  Rows                                                        |
|    - v2 fixed-width fallback                                 |
|    - optional columnar/width-compressed v3 payload            |
|                                                              |
|  Symbols                                                     |
|    - symbol bytes/table                                      |
|    - optional width-compressed lookup ids                    |
|                                                              |
|  Indexes                                                     |
|    - exact resource index retained by all profiles            |
|    - optional broad/reverse groups by profile                 |
|    - v3 posting overflow delta-varint stream                  |
|    - optional singleton/prefix key encodings                  |
|                                                              |
|  Footer checksum over raw payload                            |
+--------------------------------------------------------------+
```

Version 2 remains the current stable internal artifact. Version 3 may reject v2 when the caller
requires v3 encoding features, but loaders must keep a clear typed error rather than interpreting a
new section as v2 data.

## 6. Design

### 6.1 Section-Size Benchmark as a Gate

The new `snapshot_section_size` benchmark is not a latency benchmark. It is a format-accounting
gate. Each file-size PR must record:

- total raw and zstd bytes per profile;
- section bytes and row counts;
- per-index group key/range/overflow bytes;
- bytes saved versus the previous measurement.

The benchmark implementation parses the section directory and index directory rather than relying
on in-memory implementation details. This keeps it useful after runtime index structures change.

### 6.2 Posting Overflow Delta-Varint Encoding

Current `posting_row_ids` stores every overflow row id as fixed `u32`. In `Full`, this section is
22,426,556 bytes. Many large posting lists are monotonic by row id, so v3 should encode each
posting list as:

```text
PostingRangeV3
  first_row_id: u32
  encoded_start: u32
  encoded_len: u32

encoded overflow stream
  delta(row_id[1] - row_id[0])
  delta(row_id[2] - row_id[1])
  ...
```

The stream uses unsigned LEB128-style varints implemented locally. Decoding must reject zero deltas,
row ids outside `1..=relationship_count`, non-monotonic lists, and byte ranges outside the overflow
section. The existing `PostingRange` width stays 12 bytes, so random key lookup still slices one
range and decodes only that posting list.

### 6.3 Singleton and Multi-Posting Split

The high-cardinality `resource` and `resource_object` groups spend most bytes on `key + range`
overhead. A singleton entry currently costs 24 bytes before directory overhead: 12-byte key plus
12-byte range. v3 should split index groups into:

```text
SingletonEntry
  key: encoded key
  row_id: u32

MultiEntry
  key: encoded key
  range: PostingRangeV3
```

This is useful only for groups with high singleton ratios. The writer must record group-level counts
so the loader can binary-search singleton and multi-key arrays independently without scanning.

### 6.4 Key Prefix and Width Compression

`DiskIndexKey` is always three `u32` values. Several groups carry fixed zeros or repeated prefixes:

- `resource_object` has no relation;
- `resource_type` has one live value and two zero columns;
- `resource_type_relation`, `subject_type_relation`, and `subject_type` have very few distinct
  first/second columns.

v3 should allow group-specific key encodings:

| Group family | Candidate encoding |
| --- | --- |
| exact resource / subject | three width-selected integer columns |
| object-only / type-relation | two width-selected columns plus implicit zero |
| type-only | one width-selected column |
| low-cardinality groups | prefix table plus local ids |

The width selector is per section and chosen from `u8`, `u16`, `u24`, or `u32`. The loader validates
that every decoded symbol id is in bounds before publishing.

### 6.5 Row and Symbol Floor Reduction

After index compression, the floor becomes rows and symbols. A row is six `u32` ids, or 24 MB for
1M relationships. Since the measured symbol count is 1,020,013, all ids fit in 20 bits for this
dataset. v3 may add a columnar row section with per-column width selection:

```text
RowColumns
  resource_type_ids
  resource_id_ids
  relation_ids
  subject_type_ids
  subject_id_ids
  subject_relation_ids
```

This is a later phase because it touches row decode, validation, and all index row-match proofs.
Symbol acceleration sections also deserve measurement: `symbol_hashes + symbol_lookup` cost
12,240,156 bytes. Dropping them is not valid for normal public string-to-id lookup, but width
compressing `symbol_lookup` from `u32` to `u24` can be evaluated once row/id width support exists.

### 6.6 Profile Semantics

`CheckOnly` and `CheckAndObjectAudit` currently serialize the same index shape. This spec keeps that
behavior until object-audit support demonstrably needs a distinct index. If a future
`CheckAndObjectAudit` adds a minimal broad resource group, the section-size benchmark must show the
exact byte cost and the API spec must explain which operation it unlocks.

## 7. Correctness Invariants

- Decoded v3 rows and indexes must produce exactly the same relationship set as v2 for the same
  source snapshot.
- Posting lists remain strictly sorted by row id and point only to live relationship rows.
- Singleton and multi-posting tables must not contain duplicate keys within one group.
- Width-compressed symbol ids must reject zero when the field requires a symbol and reject values
  greater than `symbol_count`.
- `Full`, `CheckOnly`, and `CheckAndObjectAudit` must keep the same supported/unsupported public API
  behavior as their v2 equivalents.
- The default loader treats files as hostile input and validates every encoded bound before slicing.

## 8. Benchmarks and Gates

Required targets:

```text
make bench-snapshot-section-size
make bench-perf-optimization
make bench-snapshot
```

Required benchmark evidence after every format phase:

| Benchmark | Gate |
| --- | --- |
| `snapshot_section_size/full_1m` report | raw bytes, zstd bytes, section bytes, index group bytes recorded |
| `snapshot_section_size/check_only_1m` report | raw bytes <= previous phase unless documented as a deliberate capability tradeoff |
| `snapshot_load_compact/1m` | no > 5% regression for default full validation |
| `snapshot_load_trusted_fast/1m` | upper estimate <= 200 ms |
| `snapshot_file_size_check_only/1m` | remains at least 20% smaller than `Full` |

## 9. Phasing

### M13.0 - Measurement Lock

- Keep `bench-snapshot-section-size` as the canonical section-size report.
- Record the baseline from this spec in [71](./71-performance-budgets-design.md).
- Add RSS comparison for `Full`, `CheckOnly`, and `CheckAndObjectAudit`.

Exit: section bytes and RSS deltas are both visible.

### M13.1 - Posting Overflow Encoding

- Add v3 overflow varint stream.
- Keep range lookup O(log keys + decoded posting list).
- Add corrupt-varint, non-monotonic, out-of-bounds, and truncated-stream tests.

Exit: `Full` raw bytes improve materially and load targets do not regress beyond gate.

### M13.2 - Singleton and Key Encoding

- Split singleton and multi-posting groups where benchmarked singleton ratio justifies it.
- Add group-specific key widths and implicit-zero encodings.
- Keep deterministic sorted order and duplicate-key rejection.

Exit: exact resource index bytes fall below the v2 17.63 MB baseline.

### M13.3 - Row and Symbol Width Encoding

- Add columnar row encoding behind a v3 section kind.
- Width-compress `symbol_lookup` ids when `symbol_count` permits.
- Keep v2 fixed-width rows supported until the migration decision is explicit.

Exit: non-index payload falls below 50 MB on the 1M fixture.

### M13.4 - Profile Refinement

- Decide whether `CheckAndObjectAudit` remains an alias of `CheckOnly` or gains one minimal
  object-audit index group.
- Record exact byte and API support impact.

Exit: profile semantics are documented in public API docs and tests.

## 10. AGENTS.md Binding

- Error Handling: all format failures use typed `SnapshotIoError` variants with source errors where
  applicable.
- Async & Concurrency: N/A; snapshot save/load remains synchronous local file work.
- Type Design & API: encoding/profile choices are enums or versioned section tags, not booleans.
- Safety & Security: no `unsafe`; all offsets, lengths, counts, and arithmetic use checked parsing.
- Serialization: v3 changes are explicit section encodings; no serde or Rust layout serialization.
- Testing: corrupt-file, round-trip, profile-support, and benchmark gates are required before a
  format phase is complete.
- Performance: every byte-saving claim cites section-size output; every startup claim cites
  Criterion load output.
- Documentation: public docs must explain raw vs zstd, v2 vs v3, and profile capability tradeoffs.

## 11. Risks and Open Questions

- Varint decoding may save disk but add CPU to lookup of large posting lists. The range-local design
  bounds the cost to the queried key, but load and query benchmarks decide whether it stays default.
- Singleton/key split adds format complexity. It is justified only if exact resource bytes fall
  enough to offset implementation risk.
- Row columnar encoding may reduce file size while increasing full validation time. It must come
  after index compression, where the current largest waste is already proven.
- `CheckOnly` RSS reduction is still unmeasured. File-size wins must not be reported as memory wins
  until the RSS target exists.

## 12. Cross-References

- <- Depends on: [17-compact-snapshot-format-design.md](./17-compact-snapshot-format-design.md), [18-trusted-fast-snapshot-load-design.md](./18-trusted-fast-snapshot-load-design.md), [21-performance-optimization-design.md](./21-performance-optimization-design.md), [71-performance-budgets-design.md](./71-performance-budgets-design.md)
- -> Consumed by: [72-testing-verification-plan.md](./72-testing-verification-plan.md), [91-local-engine-impl-plan.md](./91-local-engine-impl-plan.md)
- Related research: [../docs/research/spike-snapshot-section-size.md](../docs/research/spike-snapshot-section-size.md)
