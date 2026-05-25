# 29 - Adaptive Bitmap Index Design

Status: draft v1
Owner: Simple Zanzibar
Last updated: 2026-05-25
Depends on: [16](./16-compact-relationship-store-design.md), [17](./17-compact-snapshot-format-design.md), [22](./22-snapshot-file-size-optimization-design.md), [23](./23-read-performance-optimization-design.md), [71](./71-performance-budgets-design.md)

## 1. Purpose

This spec evaluates whether relationship-store postings and delta tombstone masks should move from
plain row-id vectors and row-identity hash sets to adaptive bitmap-like representations. The target
is not "use bitmaps everywhere". The target is to keep the current fast singleton and sparse-list
paths intact, while adding a denser representation only when it reduces memory, row predicate scans,
or tombstone membership cost on the hot read path.

The current implementation is intentionally simple and already fast for exact checks:

- `IndexedRelationshipStore` owns seven `PostingIndex<K>` groups in
  `src/relationship.rs:479`.
- Runtime postings are either mutable hash postings or loaded sorted arrays:
  `PostingIndex::{Hash, Sorted}` in `src/relationship.rs:2071`.
- Sorted postings store one inline `first_row_id` plus an overflow slice in
  `RuntimePostingRange` in `src/relationship.rs:2169`.
- Query iteration yields `CandidateRowIds` and then checks `live_rows` and row predicates in
  `CompactRelationshipIter` in `src/relationship.rs:1937`.
- Delta overlays currently mask checkpoint rows through
  `HashSet<RelationshipRow>` in `StoreDelta.deleted_rows` and
  `RelationshipRef::is_deleted_by` in `src/relationship.rs:952` and
  `src/relationship.rs:1988`.
- The store already has a dense bitset shape for live rows: `LiveRows` uses `Vec<u64>` words after
  leaving the all-live fast path in `src/relationship.rs:5266`.

## 2. Evidence and Current Bottleneck Shape

### 2.1 Existing performance budget

The latest read path evidence in [71](./71-performance-budgets-design.md) shows that the direct
check path is no longer the obvious bottleneck:

| Benchmark | Current follow-up evidence |
| --- | ---: |
| `perf_optimization/check_prepared_1m` | `[4.2240 us, 4.2590 us, 4.3065 us]` |
| `realworld_authorization/1m_rules/check_doc_inherited_workspace_member` | `[11.211 us, 11.341 us, 11.559 us]` |
| `realworld_authorization/1m_rules/mixed_read_workload` | `[40.921 us, 41.317 us, 41.808 us]` |

The remaining algorithmic room is mainly in lookup and post-write reads:

| Benchmark or counter | Current evidence |
| --- | ---: |
| `perf_optimization/lookup_resources_streaming_1m` | `[2.2853 ms, 2.3241 ms, 2.3551 ms]` |
| `perf_optimization/lookup_subjects_streaming_1m` | `[4.6735 us, 4.7194 us, 4.7426 us]` |
| `perf_optimization/read_heavy_delta_counters_1m` | `delta_segments_inspected=1400`, `tombstone_checks=300` over 100 checks |

Bitmap work should therefore prioritize lookup and delta-mask cost. It must not regress direct
checks to chase theoretical set-operation wins.

### 2.2 Where `Vec<RowId>` postings degrade

`Vec<RowId>` postings are the right representation for high-cardinality key spaces where most keys
have one or a few rows. They degrade when all of these are true:

1. One posting list has high cardinality.
2. The query cannot stop after the first candidate.
3. The query repeatedly applies the same row predicate or tombstone mask to that list.
4. The row-id universe is not much larger than the list, so a bitset is smaller than `cardinality * 4`
   bytes.

Concrete cases in the current fixture:

- Broad groups such as `resource_type_relation`, `resource_type`, `subject_type_relation`, and
  `subject_type` had 1M total postings each in the section-size spike, with only a few logical keys.
  Those are natural dense candidates.
- The exact `resource` group is singleton-heavy and retained by every profile. It should stay array
  backed except for rare objects or relations with very large fanout.
- The `subject` group is mixed: most subject keys are small, but hot userset subjects such as
  `group:target_team#member` can have a large posting. It is a good adaptive-per-posting target.
- Delta tombstones are currently bounded at 100,000 by `STORE_VIEW_MAX_DELTA_TOMBSTONES` in
  `src/relationship.rs:40`. When the tombstone set is large, hashing a six-field
  `RelationshipRow` for every checkpoint candidate is wasteful.

### 2.3 Snapshot size evidence

The v2 section-size spike in [../docs/research/spike-snapshot-section-size.md](../docs/research/spike-snapshot-section-size.md)
showed large broad-index payloads:

| Group | v2 payload bytes | Total postings |
| --- | ---: | ---: |
| `resource_type_relation` | `4,000,140` | `1,000,000` |
| `resource_type` | `4,000,060` | `1,000,000` |
| `subject_type_relation` | `2,666,704` | `666,666` |
| `subject_type` | `4,000,060` | `1,000,000` |

Phase 13 reduced raw snapshot bytes with v3 key and posting compression, but the logical shape did
not change: broad index groups still aggregate many rows behind a small number of keys.

## 3. External Crate Survey

Survey date: 2026-05-25. This survey does not approve a dependency. Any dependency still requires
the review process in [60](./60-crates-features-design.md), `cargo audit`, and `cargo deny check`.

| Candidate | Current version observed | Fit | Decision |
| --- | ---: | --- | --- |
| `roaring` | `0.11.4` on docs.rs | Pure Rust Roaring bitmap API with `RoaringBitmap`, set operations, serialization, `from_sorted_iter`, and `optimize`. | Best external candidate if runtime-only dense/sparse hybrid proves worthwhile after local benchmarks. |
| `croaring` | `2.6.0` on docs.rs | Rust wrapper around CRoaring, a C/C++ implementation. Provides compressed bitmaps and dense bitset types. | Do not start here. The FFI/build surface conflicts with the project's dependency minimization and safety posture. |
| `fixedbitset` | `0.5.7` on docs.rs | Simple fixed-size bitset with SIMD-aware set operations. | Useful reference, but `LiveRows` already provides the dense-word primitive needed for a first pass. Avoid adding for only dense masks. |
| `bitvec` | `1.0.1` on docs.rs | Rich bit-addressed memory API with many layout options. | Too general for row-id postings; not needed unless future packed snapshot sections need bit-level layout views. |

Roaring's useful design point is the adaptive container threshold. A 32-bit integer space is split
into 2^16-sized chunks; sparse chunks use sorted 16-bit arrays, dense chunks use fixed bitmaps, and
run containers can compress contiguous ranges. The common array-to-bitmap crossover is 4096 values
per 65536-row chunk because `4096 * 2 bytes == 8192 bytes`, the fixed bitmap size for one chunk.

## 4. Goals

| # | Goal | Measure |
| --- | --- | --- |
| G1 | Preserve exact-check speed. | `perf_optimization/check_prepared_1m` and inherited realworld check show no > 5% upper-estimate regression. |
| G2 | Improve broad lookup and dense subject/resource fanout. | `perf_optimization/lookup_resources_streaming_1m` upper estimate improves by >= 10% when a bitmap representation is enabled, or the phase exits with no runtime representation change. |
| G3 | Reduce broad-index runtime memory when density justifies it. | Bench-internal index histogram reports estimated runtime posting bytes before/after, with >= 5% reduction for `IndexProfile::Full` on the 1M fixture before shipping by default. |
| G4 | Reduce large tombstone-mask overhead. | A delete-heavy delta fixture shows tombstone membership cost or memory improves, while `read_heavy_heavy_write_batched_1m` has no > 5% regression. |
| G5 | Keep snapshot compatibility explicit. | First implementation is runtime-only and keeps v3 file bytes unchanged; any v4 encoding phase must pass the snapshot-size and load gates in [22](./22-snapshot-file-size-optimization-design.md). |

## 5. Non-Goals

- No bitmap representation for all postings.
- No dependency addition in the measurement phase.
- No snapshot v4 format change until a runtime-only implementation proves useful.
- No `croaring` or other FFI bitmap dependency in the first implementation.
- No changing public API output ordering. Deterministic tests should continue sorting materialized
  results explicitly where needed.
- No replacing the existing `LiveRows` all-live fast path.

## 6. Representation Selection

### 6.1 Runtime representation

Introduce a private adaptive posting enum for runtime indexes:

```rust
enum PostingSet {
    One(RowId),
    RowIds {
        first: RowId,
        overflow_start: u32,
        overflow_len: u32,
    },
    DenseBitset(DenseRowIdBitset),
    ChunkedBitmap(ChunkedRowIdBitmap),
}
```

`One` and `RowIds` preserve the current representation. `DenseBitset` is a simple `Vec<u64>` over
the full checkpoint row universe, using the same checked row-id conversion discipline as `LiveRows`.
`ChunkedBitmap` is a local Roaring-like shape used only if dense bitsets waste too much space for
mixed sparse/dense postings:

```rust
enum RowIdChunk {
    Array16(Vec<u16>),
    Bitmap([u64; 1024]), // 65536 bits
    Run(Vec<RowIdRun16>),
}
```

The first implementation should ship `One`, `RowIds`, and `DenseBitset` only. `ChunkedBitmap` or the
external `roaring` crate should be a second implementation phase gated by histogram evidence.

### 6.2 Selection diagram

```text
                         +---------------------------+
                         | posting list for one key |
                         +-------------+-------------+
                                       |
                                       v
                           cardinality == 1 ?
                              /        \
                            yes        no
                             |          |
                             v          v
                         +-------+   compute:
                         |  One  |   - cardinality
                         +-------+   - row_count
                                     - density = cardinality / row_count
                                     - max per-65536 chunk cardinality
                                     - estimated bytes
                                           |
                                           v
                 +--------------------------------------------------+
                 | exact singleton-heavy group and no set operation |
                 | planned for this query path?                     |
                 +----------------------+---------------------------+
                                        | yes
                                        v
                                  +-----------+
                                  |  RowIds   |
                                  +-----------+
                                        ^
                                        |
                    no                  |
                    |                   |
                    v                   |
       +----------------------------+   |
       | dense_bytes <= 0.75 *      | no|
       | row_ids_bytes and          +---+
       | cardinality >= 4096 ?      |
       +-------------+--------------+
                     | yes
                     v
              +--------------+
              | DenseBitset  |
              +------+-------+
                     |
                     v
       +----------------------------+
       | mixed chunks or sparse     |
       | large list still costly?   |
       +-------------+--------------+
                     | yes, future phase only
                     v
              +---------------+
              | ChunkedBitmap |
              | or roaring    |
              +---------------+
```

### 6.3 Initial thresholds

The initial thresholds should be constants in `relationship.rs`, not runtime configuration. They are
compile-time tuning points and must be changed only with benchmark evidence.

Definitions:

- `row_count`: checkpoint row universe.
- `cardinality`: rows in the posting.
- `row_ids_bytes = cardinality * size_of::<RowId>()`.
- `dense_bytes = ceil(row_count / 64) * 8`.
- `dense_break_even = row_count / 32`, because `RowId` is 4 bytes and a dense bitset is 1 bit per
  row.

Recommended first thresholds:

| Representation | Use when | Rationale |
| --- | --- | --- |
| `One` | `cardinality == 1` | Current fastest path. |
| `RowIds` | `cardinality < 4096` | Avoid bitset setup and zero scanning for short lists. |
| `RowIds` | exact `resource` or `resource_object` posting unless selected by a query-specific intersection plan | Exact-resource checks usually exit early; arrays are cache-friendly. |
| `DenseBitset` | `cardinality >= 4096` and `dense_bytes * 4 <= row_ids_bytes * 3` | Requires at least 25% estimated memory win before changing iteration behavior. This is roughly `cardinality >= row_count / 24`. |
| `DenseBitset` | broad group with `cardinality >= dense_break_even` and lookup benchmark proves faster | Allows a lower memory threshold for broad lookup only when measured. |
| `ChunkedBitmap` or `roaring` | many 2^16 chunks where some chunks are dense and others sparse, and runtime-only `DenseBitset` loses memory or CPU | Defer until histograms prove dense bitsets are too coarse. |

For a 1M-row checkpoint:

- Dense bitset size is about 125 KiB per posting.
- Dense bitset memory breaks even with `Vec<RowId>` at about 31,250 rows.
- The stricter first threshold picks dense bitsets at about 41,667 rows.
- Roaring's per-chunk array/bitmap threshold is 4096 values per 65536-row chunk.

## 7. Query Complexity and Planner Changes

### 7.1 Current complexity

For sorted fast-load indexes:

- Key lookup is `O(log key_count)`.
- Candidate iteration is `O(candidate_count)`.
- Each candidate may pay one `live_rows.contains`, one row fetch, row predicate checks, and possibly
  one tombstone membership check.

For hash latency indexes:

- Key lookup is expected `O(1)`.
- Candidate iteration is still `O(candidate_count)`.

Changing only the storage representation does not automatically improve query latency if the iterator
still yields every candidate and runs the same row predicates. The implementation must add set-aware
query paths where a bitmap representation gives a real algorithmic win.

### 7.2 Required set-aware paths

The first useful paths are:

1. `resource_relation_subject`
   - Current path starts from the resource/relation posting and filters by subject.
   - New path may compute `resource_relation_posting INTERSECT subject_posting` when both are
     available and at least one side is bitmap-backed or the subject side is smaller.
   - If the intersection is non-empty, `any_resource_relation_subject` can return without scanning a
     large resource fanout.

2. `lookup_resources`
   - Current broad reverse traversal can produce many resource candidates and then verify checks.
   - New path should use bitmap cardinality and `is_disjoint` style checks to skip empty branches
     before materializing candidates.

3. Delta checkpoint masking
   - Current path checks every checkpoint candidate against `HashSet<RelationshipRow>`.
   - New path should mask by row id: `deleted_mask.contains(row.row_id)`.

### 7.3 Iterator contract

`CandidateRowIds` should become a wrapper over adaptive posting iterators:

```rust
enum CandidateRowIds<'a> {
    Empty,
    One(Option<RowId>),
    RowIds(RowIdSliceIter<'a>),
    Dense(DenseRowIdBitsetIter<'a>),
    Intersection(PostingIntersectionIter<'a>),
}
```

The iterator must preserve these invariants:

- It never yields a row id outside `1..=row_count`.
- It yields strictly increasing row ids for snapshot-loaded sorted profiles.
- It may stop early when `QueryLimit` reaches zero.
- It does not allocate on `next`.
- It exposes optional `cardinality_hint()` and `contains(RowId)` for query planning.

## 8. Tombstone Mask Design

Tombstone masks are a stronger candidate than postings because they are pure membership tests.
The checkpoint row id is stable for the lifetime of the checkpoint, and exact-revision snapshots
keep that checkpoint immutable.

Replace `StoreDelta.deleted_rows: Arc<HashSet<RelationshipRow>>` with a private adaptive mask:

```rust
enum TombstoneMask {
    Empty,
    Small(Vec<RowId>),
    Hash(HashSet<RowId>),
    Dense(DenseRowIdBitset),
}
```

Recommended thresholds:

| Representation | Use when | Membership cost |
| --- | --- | --- |
| `Empty` | no checkpoint deletes | no branch after `Option` check |
| `Small(Vec<RowId>)` | `1..=32` tombstones | linear or binary search over cache-local row ids |
| `Hash(HashSet<RowId>)` | `33..8191` tombstones | expected `O(1)`, lower setup cost than dense bitset |
| `Dense(DenseRowIdBitset)` | `>= 8192` tombstones or `dense_bytes <= hash_estimated_bytes / 2` | one word load and mask |

The mask must be built from checkpoint row ids, not from public `Relationship` values. Mutation
application already requires checkpoint uniqueness for delta writes. The future implementation should
look up the checkpoint `RelationshipRow`, retain its `row_id`, and use that row id in the tombstone
mask. `RelationshipRow` equality intentionally ignores `row_id` today; this spec changes tombstone
masking, not row identity semantics.

Snapshot files should not serialize tombstone masks in this phase. `RelationshipStoreView::encode_snapshot_sections`
already canonicalizes a delta view into a compact store before writing. A saved snapshot starts with
all live rows and no runtime delta tombstones.

## 9. Snapshot Encoding Impact

### 9.1 Runtime-only phase

The first implementation must not change `.szsnap` bytes. It should load the existing v3 arrays and
select adaptive runtime posting sets while decoding indexes.

Expected effects:

- Raw snapshot size: unchanged.
- Zstd snapshot size: unchanged.
- Full-validation load time: may regress if bitmap construction adds work; gate at <= 5%.
- Trusted-fast load time: must remain <= 200 ms.
- Runtime RSS: should improve for `IndexProfile::Full` only if broad postings are dense enough.

### 9.2 Optional v4 format phase

Only after runtime-only representation proves useful, a v4 snapshot may encode posting
representations directly. The preferred shape is an index-local representation directory, not a new
global bitmap section that loses key locality:

```text
IndexDirectoryEntryV4
  kind
  key_start
  key_count
  posting_descriptor_start
  posting_descriptor_count

PostingDescriptor
  repr_tag: one | row_ids_delta_varint | dense_words | roaring_chunks
  cardinality: u32
  first_row_id: u32
  payload_start: u32
  payload_len: u32

PostingPayloads
  row-id delta varints, dense u64 words, or chunk payloads
```

This keeps key lookup local: binary search key, fetch one descriptor, then iterate or test membership
from one payload span.

### 9.3 Raw and zstd size expectations

Dense bitsets trade row-id count for universe size:

- For 1M rows, one dense posting costs about 125 KiB raw.
- A 100k-row posting is about 400 KiB as `Vec<RowId>` and about 125 KiB as dense bits before
  metadata.
- A 1k-row posting is about 4 KiB as `Vec<RowId>` and about 125 KiB as dense bits, so arrays must
  remain the default for sparse postings.

Zstd impact is data-dependent:

- Dense bitsets for contiguous or periodic generated data can compress well.
- Random half-full bitsets can compress poorly compared with delta-varints.
- Roaring or chunked encodings reduce raw bytes before zstd, but zstd may then have less redundancy
  left to exploit.

The v4 phase must therefore publish both raw and zstd section-size reports and must not assume that
a smaller runtime structure produces a smaller distribution artifact.

## 10. Migration Plan

### M29.0 - Histogram and Cost Model

- Add bench-internal instrumentation that reports posting cardinality histograms by index kind.
- Report estimated bytes for current row-id arrays, dense bitsets, and chunked bitmap candidates.
- Report tombstone count and tombstone membership checks in delete-heavy read benchmarks.
- Do not change runtime behavior in this phase.

Exit criteria:

- A 1M `IndexProfile::Full` report identifies which posting lists exceed 4096 rows, which exceed
  `row_count / 32`, and which are exact-resource singletons.
- A delete-heavy delta fixture records tombstone checks and tombstone count.

### M29.1 - Row-Id Tombstone Mask

- Change delta masking from `HashSet<RelationshipRow>` to `TombstoneMask`.
- Preserve public `RelationshipRow` equality and uniqueness behavior.
- Add tests for delete, recreate, touch-after-delete, exact revision reads, and snapshot save from a
  delta view.

Exit criteria:

- Existing delta correctness tests pass.
- New delete-heavy benchmark shows no regression for small deltas and improvement for large
  tombstone counts.

### M29.2 - Runtime Adaptive Postings

- Introduce `PostingSet` and keep `One`/`RowIds` as the default.
- Select `DenseBitset` only for measured high-density postings.
- Keep snapshot v3 encoding unchanged.
- Add `contains`, `iter`, `cardinality_hint`, and optional `intersects` methods.

Exit criteria:

- Direct check and inherited check have no > 5% regression.
- Runtime memory estimate or RSS improves for `IndexProfile::Full`, or the phase remains disabled by
  default.

### M29.3 - Set-Aware Query Paths

- Add an intersection path for `resource_relation_subject`.
- Add broad-lookup skips using `intersects` or `is_disjoint` where both sides have posting sets.
- Keep fallback row iteration for every query shape.

Exit criteria:

- `perf_optimization/lookup_resources_streaming_1m` improves by >= 10% on the 1M fixture.
- `perf_optimization/lookup_subjects_streaming_1m` and `check_prepared_1m` have no > 5% regression.

### M29.4 - Optional Roaring or Chunked Bitmap Decision

- Compare local `DenseBitset` against either local `ChunkedBitmap` or `roaring`.
- If using `roaring`, add the dependency only after documenting crate audit, features, license, and
  `cargo deny` status.
- If using local chunks, keep the implementation small: array16 and bitmap chunks first; add run
  chunks only if the histogram proves long runs.

Exit criteria:

- The chosen representation beats `DenseBitset` on either memory or query latency for mixed
  sparse/dense large postings.
- The rejected alternative is documented with benchmark numbers.

### M29.5 - Optional Snapshot v4 Encoding

- Add v4 posting descriptors only if runtime adaptive postings are enabled by default and load-time
  construction cost is measurable.
- Keep v3 loader support or reject unsupported versions with typed `SnapshotIoError` according to
  the compatibility policy already used by snapshot specs.
- Extend `snapshot_section_size` to report representation counts and bitmap payload bytes.

Exit criteria:

- `snapshot_load_compact/1m` has no > 5% regression.
- `snapshot_load_trusted_fast/1m` remains <= 200 ms.
- Raw and zstd file-size deltas are recorded for `Full`, `CheckOnly`, and `CheckAndObjectAudit`.

## 11. Testing and Benchmark Gates

Correctness tests:

- Property-test adaptive posting iteration against the current row-id vector for random sorted row-id
  sets across sparse, dense, chunk-boundary, and all-live cases.
- Test `contains`, `intersects`, and intersection iterators against `HashSet<RowId>` reference
  results.
- Test tombstone masks for all threshold boundaries: 0, 1, 32, 33, 8191, 8192, and 100000.
- Test exact revision visibility after checkpoint deletes and recreates.
- Test snapshot save/load equivalence from a view with a non-empty delta.

Bench gates:

| Benchmark | Gate |
| --- | --- |
| `perf_optimization/check_prepared_1m` | no > 5% regression |
| `realworld_authorization/1m_rules/check_doc_inherited_workspace_member` | no > 5% regression |
| `realworld_authorization/1m_rules/mixed_read_workload` | no > 5% regression |
| `perf_optimization/lookup_resources_streaming_1m` | >= 10% improvement before enabling adaptive postings by default |
| `perf_optimization/lookup_subjects_streaming_1m` | no > 5% regression |
| `perf_optimization/read_heavy_heavy_write_batched_1m` | no > 5% regression |
| delete-heavy tombstone benchmark | improvement for large tombstone masks and no regression for small masks |
| `snapshot_load_compact/1m` | no > 5% regression |
| `snapshot_load_trusted_fast/1m` | upper estimate <= 200 ms |
| `snapshot_section_size/full_1m` | report raw, zstd, index payload, and representation counts if v4 lands |

## 12. Risks

| Risk | Impact | Mitigation |
| --- | --- | --- |
| Dense bitsets speed membership but slow sparse iteration. | Direct checks or small lookups regress. | Keep `RowIds` default below thresholds and require no-regression gates. |
| Bitmap representation reduces memory but not latency because row predicates still run per row. | Complexity without user-visible gain. | Add set-aware query paths before enabling default adaptive postings. |
| Runtime-only construction increases snapshot load time. | Cold-start regression. | Gate load benchmarks and defer v4 descriptors unless construction cost is measurable. |
| Roaring dependency adds audit and maintenance surface. | Larger dependency graph and possible unsafe transitive code. | Start with local dense bitset; approve `roaring` only with benchmark and dependency evidence. |
| Tombstone mask by row id can be wrong if built from the wrong checkpoint. | Exact-revision correctness bug. | Build mask only from checkpoint uniqueness lookups and test exact revision reads after deletes/recreates. |
| Snapshot v4 can hurt zstd artifacts. | Distribution size regression. | Keep v4 optional and record raw plus zstd section-size data before shipping. |

## 13. Recommendation

Priority: medium-high after request-local memoization and compiled evaluation plan work. The direct
check path is already under budget, but bitmap-style representations target the next largest class:
lookup and delete-heavy post-write reads.

Recommended execution order:

1. Ship `TombstoneMask` first. It is simpler than adaptive postings, does not affect snapshot files,
   and directly removes six-field hash checks from delta checkpoint candidates.
2. Add posting histograms and runtime `DenseBitset` behind a private default-off selection gate.
3. Enable adaptive postings only for broad groups and measured large subject/resource fanout.
4. Consider `roaring` or local chunked bitmaps only if dense bitsets waste memory on mixed
   sparse/dense postings.
5. Consider snapshot v4 only after runtime construction cost or runtime memory wins justify making
   representation persistent.

Expected benefit:

- Small or no benefit for direct exact checks.
- Moderate benefit for `lookup_resources_streaming_1m` if broad lookup can use intersection or
  disjoint tests before row materialization.
- Meaningful memory reduction for dense broad postings in `IndexProfile::Full`.
- Clearer win for large delta tombstone masks, especially near the current 100k tombstone threshold.

Primary risk:

- A bitmap-only storage rewrite would add complexity without improving latency. The implementation
  must be query-planner aware: representation selection, membership/intersection APIs, and benchmark
  gates are part of the same feature.

## 14. AGENTS.md Binding

- Error Handling: all snapshot format changes, if any, use typed `SnapshotIoError`; runtime
  construction failures use existing `StoreError::CapacityExceeded` or a specific store invariant
  error.
- Async & Concurrency: adaptive postings and tombstone masks are immutable after publication and
  shared through existing `Arc` snapshots; no read-path mutex is introduced.
- Type Design & API: row ids remain private newtypes; representation choices are private enums.
- Safety & Security: no crate-local `unsafe`; all row-id, byte-offset, length, and cardinality math
  uses checked arithmetic.
- Serialization: first phase has no serialization change; any v4 phase uses explicit section tags
  and rejects malformed payloads.
- Testing: property tests and threshold-boundary tests are required before enabling adaptive
  postings or tombstone masks by default.
- Performance: every default-on threshold must cite Criterion and section-size evidence.
- Documentation: public docs need no API change; internal specs must record the enabled thresholds
  and rejected alternatives.

## 15. Cross-References

- Builds on compact store row-id postings from [16](./16-compact-relationship-store-design.md).
- Preserves v3 snapshot format from [17](./17-compact-snapshot-format-design.md) and
  [22](./22-snapshot-file-size-optimization-design.md) until the optional v4 phase.
- Extends delta-read instrumentation from [23](./23-read-performance-optimization-design.md).
- Bench gates are anchored in [71](./71-performance-budgets-design.md).
