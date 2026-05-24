# 24 - Zstd-Aware Snapshot Load Design

Status: draft v1
Owner: Simple Zanzibar
Last updated: 2026-05-24
Depends on: [17](./17-compact-snapshot-format-design.md), [18](./18-trusted-fast-snapshot-load-design.md), [22](./22-snapshot-file-size-optimization-design.md), [23](./23-read-performance-optimization-design.md), [71](./71-performance-budgets-design.md)

## 1. Purpose

Phase 13 made raw v3 snapshots much smaller, but it also proved that raw byte minimization and
zstd byte minimization are not the same objective. Compact 24-bit row and symbol fields remove raw
bytes, while zstd often prefers fixed-width fields because repeated zero bytes and stable alignment
compress well. Loader CPU has the opposite tradeoff: every variable-width field is another decode
branch on the cold-load path.

This spec defines a measured follow-up that keeps the raw `.szsnap` compact while allowing
zstd-wrapped snapshots to use a compression-friendly inner layout. It also records the load/read
optimizations that are safe to land without a new snapshot format version.

## 2. Current Evidence

Phase 13 baseline on the 1M org fixture:

| Benchmark | Phase 13 |
| --- | ---: |
| `snapshot_file_size/1m` | `77,573,646 bytes` |
| `snapshot_file_size_zstd/1m` | `22,384,838 bytes` |
| `snapshot_load_compact/1m` | `[579.68 ms, 585.81 ms, 593.91 ms]` |
| `snapshot_load_trusted_fast/1m` | `[183.45 ms, 185.11 ms, 186.84 ms]` |
| `snapshot_load_zstd/1m` | `[625.59 ms, 629.45 ms, 633.10 ms]` |
| `snapshot_load_phase_timers_1m` | `[578.05 ms, 586.63 ms, 597.34 ms]`; rows `314.50 ms` |
| `realworld_authorization/1m_rules/mixed_read_workload` | `[57.221 us, 57.733 us, 58.164 us]` |

The zstd crate dependency is already at `0.13.3`, the current docs.rs latest release at the time of
this design, so this phase does not change dependencies.

## 3. Goals

| # | Goal | Measure |
| --- | --- | --- |
| G1 | Reduce zstd snapshot size without increasing raw `.szsnap` size. | `snapshot_file_size_zstd/1m` improves versus Phase 13; `snapshot_file_size/1m` unchanged. |
| G2 | Improve raw and zstd load CPU. | `snapshot_load_compact/1m`, trusted load, and zstd load improve versus Phase 13. |
| G3 | Keep zstd as an outer transport wrapper. | No public API change and no v4 format bump. |
| G4 | Bring realistic mixed reads under budget. | `mixed_read_workload` upper estimate <= 55 us. |
| G5 | Preserve corruption safety. | Existing malformed-section, zstd cap, cycle, fanout, and full test gates pass. |

## 4. Design

### 4.1 Compression-Friendly Inner Layout

`SnapshotCompression::None` continues to write compact v3 sections:

- `RelationshipRows`: minimal symbol-id width.
- `SymbolTable`: minimal start and length widths.
- `SymbolLookup`: minimal symbol-id width.
- Index sections: compact v3 keys, singleton/multi split, and delta-varint overflow row ids.

`SnapshotCompression::Zstd` writes the same v3 format but uses fixed `u32` widths for
`RelationshipRows`, `SymbolTable`, and `SymbolLookup` before applying the outer zstd frame. The
existing per-section flags already describe these widths, so every v3 reader can parse both layouts
without a format bump. Index sections stay compact because Phase 13 evidence showed they dominate
the full-profile raw savings and still compress well under zstd.

This deliberately makes the decompressed inner zstd payload larger:

| Profile | Raw compact bytes | Zstd inner raw bytes | Why accepted |
| --- | ---: | ---: | --- |
| `Full` | `77,573,519` | `88,673,584` | outer zstd shrinks from `22,317,770` to `21,471,681` |
| `CheckOnly` | `59,078,231` | `70,178,296` | outer zstd shrinks from `19,031,692` to `18,182,828` |

### 4.2 Streaming Zstd Decode

Direct zstd load validates the compressed file length from metadata, then streams zstd
decompression from the file into the bounded inner snapshot buffer. The loader no longer holds the
entire compressed frame and decompressed payload at the same time. This keeps direct-zstd RSS under
the 400 MiB snapshot-load budget even though the zstd inner layout is larger than the compact raw
artifact.

### 4.3 Row-Chunk Decode

The loader decodes `RelationshipRows` as fixed-size row chunks. Each row contains six symbol-id
fields, so the decoder turns one exact row slice into six `u32` values for the active width. This
removes six cursor reads per row, keeps bounds checks at the row boundary, and remains safe for
hostile input because section length and row length are validated before chunk iteration.

The measured row phase drops from the Phase 13 `314.50 ms` representative timer to `277.82 ms`.

### 4.4 Stack-Based Recursion State

The evaluator used request-local `HashSet`s to detect active check and expand recursion. The active
recursion set is a stack, not an arbitrary set: only ancestors matter, depth is capped, and hot
paths are shallow. Replacing the recursion `HashSet`s with `Vec` stacks preserves cycle detection
while reducing hash work in `check`, lookup verification, and mixed-read workloads.

This is a subset of [23](./23-read-performance-optimization-design.md)'s reusable context work. It
does not replace the later ID-native schema IR or segment-native lookup plan.

## 5. Alternatives Rejected

| Alternative | Rejection reason |
| --- | --- |
| Increase zstd level only | It would not reduce raw load CPU and risks slower save/load without addressing layout entropy. |
| Use fixed-width sections for raw `.szsnap` too | It regresses raw Full from `77.6 MB` toward `88.7 MB`, violating the Phase 13 raw-size goal. |
| Compress individual sections | It complicates the parser, integrity proof, and max decompressed byte accounting without evidence that the outer frame is insufficient. |
| Add v4 for zstd-aware layout | Existing v3 flags already encode the needed widths. A version bump would add compatibility cost with no reader benefit. |
| Keep recursion `HashSet`s | The recursion frontier is tiny and ordered; hashing every active key is unnecessary hot-path work. |

## 6. Correctness Invariants

- Raw and zstd snapshots decode through the same validated v3 reader after any zstd decompression.
- `max_file_bytes` applies to both compressed bytes and decompressed inner bytes.
- Zstd inner fixed-width sections must be self-described by section flags.
- Full validation still checks symbol ids, row semantics, uniqueness, posting ranges, and indexes.
- Trusted fast-load remains explicit and may only skip semantic proof under the documented trust
  boundary.
- Stack recursion state denies cycles exactly when the previous active-key set denied them.
- No public API shape changes for `check`, `expand`, lookup, snapshot save/load, or index profiles.
  Tight `SnapshotLoadOptions::max_file_bytes` settings must account for the decompressed zstd inner
  payload, which can be larger than an independently saved compact raw artifact.

## 7. Benchmark Evidence

Measured 2026-05-24 after the selected implementation:

| Benchmark | Phase 13 | This phase | Diff |
| --- | ---: | ---: | ---: |
| `snapshot_file_size/1m` | `77,573,646 bytes` | `77,573,646 bytes` | unchanged |
| `snapshot_file_size_zstd/1m` | `22,384,838 bytes` | `21,512,241 bytes` | `-872,597 bytes / -3.90%` |
| section-size `Full` zstd | `22,317,770 bytes` | `21,471,681 bytes` | `-846,089 bytes / -3.79%` |
| section-size `CheckOnly` zstd | `19,031,692 bytes` | `18,182,828 bytes` | `-848,864 bytes / -4.46%` |
| `snapshot_load_compact/1m` upper | `593.91 ms` | `553.98 ms` | `-6.72%` |
| `snapshot_load_trusted_fast/1m` upper | `186.84 ms` | `173.52 ms` | `-7.13%` |
| `snapshot_load_zstd/1m` upper | `633.10 ms` | `618.59 ms` | `-2.29%` |
| `snapshot_load_phase_timers_1m` upper | `597.34 ms` | `549.63 ms` | `-7.99%` |
| `snapshot_load_peak_rss/1m` raw max RSS | `343,851,008 bytes` | `354,959,360 bytes` | `+11,108,352 bytes / +3.23%`; still under 400 MiB |
| `snapshot_load_peak_rss/1m` zstd max RSS | no Phase 13 baseline | `415,055,872 bytes` | new direct-zstd evidence; under 400 MiB |
| `mixed_read_workload` upper | `58.164 us` | `53.489 us` | `-8.04%` |
| `check_prepared_1m` upper | `6.0009 us` | `5.3424 us` | `-10.97%` |
| `lookup_resources_streaming_1m` upper | `3.1883 ms` | `2.7544 ms` | `-13.61%` |
| `lookup_subjects_streaming_1m` upper | `6.3451 us` | `5.4722 us` | `-13.75%` |

## 8. Verification Gates

Required before declaring this phase done:

```text
make bench-snapshot-section-size
make bench-snapshot-memory
make bench-snapshot-zstd-memory
cargo bench --bench snapshot -- snapshot_load_compact/1m --sample-size 10
cargo bench --bench snapshot -- snapshot_load_trusted_fast/1m --sample-size 10
cargo bench --bench snapshot -- snapshot_load_zstd/1m --sample-size 10
cargo bench --bench snapshot -- snapshot_file_size/1m --sample-size 10
cargo bench --bench snapshot -- snapshot_file_size_zstd/1m --sample-size 10
cargo bench --features bench-internals --bench perf_optimization -- perf_optimization/snapshot_load_phase_timers_1m --sample-size 10
cargo bench --bench realworld_authorization -- realworld_authorization/1m_rules/mixed_read_workload --sample-size 10
cargo bench --bench realworld_authorization -- realworld_authorization/1m_rules/check_doc_inherited_workspace_member --sample-size 10
cargo build --workspace --all-targets
cargo test --workspace --all-features
cargo +nightly fmt --check
cargo clippy --workspace --all-targets --all-features -- -D warnings -W clippy::pedantic -W clippy::unwrap_used -W clippy::expect_used -W clippy::indexing_slicing -W clippy::panic
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps
cargo audit
cargo deny check
```

## 9. Cross-References

- <- Depends on: [17-compact-snapshot-format-design.md](./17-compact-snapshot-format-design.md), [18-trusted-fast-snapshot-load-design.md](./18-trusted-fast-snapshot-load-design.md), [22-snapshot-file-size-optimization-design.md](./22-snapshot-file-size-optimization-design.md), [23-read-performance-optimization-design.md](./23-read-performance-optimization-design.md), [71-performance-budgets-design.md](./71-performance-budgets-design.md)
- -> Consumed by: [72-testing-verification-plan.md](./72-testing-verification-plan.md), [91-local-engine-impl-plan.md](./91-local-engine-impl-plan.md)
