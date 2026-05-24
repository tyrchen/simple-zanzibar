# Spike: where do 1M snapshot bytes go?

Status: Done | Owner: Simple Zanzibar | Date: 2026-05-24 | Outcome: **PASS-with-caveat**

## Question

Phase 12 proved that `CheckOnly` snapshots are 37% smaller than `Full`, but the earlier evidence
only compared total file size. This spike answers which `.szsnap` v2 sections and index groups
account for the reduction, and which remaining sections form the next lower bound for file-size
optimization.

## Method

Runnable artifact: `make bench-snapshot-section-size`, which invokes the release benchmark target
added in `benches/snapshot_section_size.rs`. The benchmark builds the same 1M org dataset used by
the Phase 12 performance harness, saves raw and zstd snapshots for `Full`, `CheckOnly`, and
`CheckAndObjectAudit`, then parses the raw section directory and index directory. The Makefile entry
is `Makefile:46`.

The parser mirrors the local v2 format: section ids are defined by `src/snapshot.rs:336`, and the
benchmark maps them in `benches/snapshot_section_size.rs:134`. It parses the section directory in
`benches/snapshot_section_size.rs:323`, parses per-index directory entries in
`benches/snapshot_section_size.rs:356`, and sums per-group overflow row ids from posting ranges in
`benches/snapshot_section_size.rs:386`.

The writer-side index behavior being measured is in `src/relationship.rs:2945`: rows are inserted
into seven possible `SnapshotIndexGroups`, but `CheckOnly` returns after the exact resource index at
`src/relationship.rs:3038`. `Full` continues into broad resource indexes and subject reverse indexes
through `src/relationship.rs:3101`.

## Findings

1. `Full` raw size is 124,422,114 bytes. Of that, index payload is 63,867,328 bytes and non-index
   payload is 60,554,402 bytes. The zstd wrapper for distribution is 33,116,811 bytes.

2. `CheckOnly` raw size is 78,188,326 bytes. Its index payload is 17,633,540 bytes, all from the
   exact resource index plus the 140-byte index directory. It saves 46,233,788 bytes, or 37.15%, by
   omitting broad resource and subject reverse index groups. The zstd wrapper is 18,873,001 bytes.

3. `CheckAndObjectAudit` currently has the same byte shape as `CheckOnly`: 78,188,326 raw bytes and
   18,873,001 zstd bytes. It is a capability label today, not a larger index profile. The next spec
   must either codify that alias behavior or define the minimal extra index it needs.

4. The fixed non-index floor is already large:

   | Section | Bytes |
   | --- | ---: |
   | `relationship_rows` | 24,000,000 |
   | `symbol_bytes` | 16,153,374 |
   | `symbol_table` | 8,160,104 |
   | `symbol_hashes` | 8,160,104 |
   | `symbol_lookup` | 4,080,052 |
   | `schema + footer` | 768 |

5. Full index bytes are split as follows:

   | Index group | Payload bytes | Total postings |
   | --- | ---: | ---: |
   | `resource` | 17,633,400 | 1,000,000 |
   | `resource_object` | 17,633,380 | 1,000,000 |
   | `resource_type_relation` | 4,000,140 | 1,000,000 |
   | `resource_type` | 4,000,060 | 1,000,000 |
   | `subject` | 13,933,444 | 1,666,666 |
   | `subject_type_relation` | 2,666,704 | 666,666 |
   | `subject_type` | 4,000,060 | 1,000,000 |

6. The best first v3 targets are visible in the section data. `posting_row_ids` alone is
   22,426,556 bytes in `Full`; delta-varint encoding should attack this first. `index_keys` and
   `posting_ranges` are each 20,720,316 bytes in `Full`; singleton-key specialization and key
   prefix/width compression should attack these next.

## Decision

**GO-with-amendments** for two follow-up specs:

- A snapshot file-size spec must start with section-size gates, then prioritize index overflow
  delta-varint encoding, singleton/prefix key compression, and only then row/symbol compaction.
- A read-performance spec must not treat file-size wins as read-latency wins. The hot `check` path
  already uses the retained exact resource index, so read latency requires evaluator/store-view
  changes rather than only smaller snapshot artifacts.

## Risks Identified

- The Criterion timing for `snapshot_section_size/*/total_bytes` measures a constant value and is
  not a latency claim. The load-bearing evidence is the release-build report printed by the
  benchmark.
- This spike records file bytes, not steady-state RSS. `CheckOnly` memory savings still require a
  separate RSS benchmark.
- zstd sizes are useful for distribution, but direct zstd load remains a different startup profile
  from raw `TrustedFastLoad + External`.
