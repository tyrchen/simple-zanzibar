# 30 - Adaptive Delta Compaction Design

Status: draft v1
Owner: Simple Zanzibar
Last updated: 2026-05-25
Depends on: [13-revision-consistency-design.md](./13-revision-consistency-design.md), [16-compact-relationship-store-design.md](./16-compact-relationship-store-design.md), [20-concurrent-engine-runtime-design.md](./20-concurrent-engine-runtime-design.md), [21-performance-optimization-design.md](./21-performance-optimization-design.md), [23-read-performance-optimization-design.md](./23-read-performance-optimization-design.md), [71-performance-budgets-design.md](./71-performance-budgets-design.md)

## 1. Purpose

Phase 14 made post-write reads cheaper by masking checkpoint tombstones with checkpoint-native row
identities and by recording benchmark-only `delta_segments_inspected` and `tombstone_checks`
counters. The remaining risk is that the current write optimization shifts cost from writer
publication into later reads: `RelationshipStoreView` is a checkpoint plus a bounded delta overlay,
and every query against a delta-backed view checks inserted rows first and then masks checkpoint
rows through the delta tombstone set.

This spec defines an adaptive compaction policy for relationship-store views after writes. The
first implementation must fit the current PR #14 code shape: one checkpoint plus one cumulative
delta overlay. The design also leaves room for the older multi-delta architecture described in
[21](./21-performance-optimization-design.md#10-priority-5---segmented-store-and-delta-publication),
but does not require it for the first phase.

The target outcome is simple: let normal writes publish quickly, schedule checkpoint merge before
delta read overhead becomes visible, and preserve exact-revision semantics by replacing only
immutable `Arc` snapshots with logically equivalent compacted snapshots.

## 2. Current Implementation Facts

The current relationship view is:

```rust
pub struct RelationshipStoreView {
    checkpoint: Arc<IndexedRelationshipStore>,
    delta: Option<StoreDelta>,
}

struct StoreDelta {
    inserted: Arc<IndexedRelationshipStore>,
    deleted_rows: Arc<HashSet<RelationshipRow>>,
    deleted: Arc<HashSet<Relationship>>,
    mutation_count: NonZeroUsize,
}
```

Important behavior:

- `RelationshipStoreView::apply_mutations` clones the previous cumulative delta, applies the new
  write batch, publishes a new view, and synchronously checkpoints only when the hard
  `STORE_VIEW_MAX_DELTA_MUTATIONS` or `STORE_VIEW_MAX_DELTA_TOMBSTONES` threshold trips.
- Query methods inspect the delta inserted store if present, then iterate the checkpoint while
  testing checkpoint rows against `deleted_rows`.
- Benchmark-only counters currently count delta-overlay inspections and checkpoint tombstone tests.
- `WriterState::publish_snapshot` publishes immutable `Arc<PublishedSnapshot>` values through
  `ArcSwapOption<EngineState>` and retains exact snapshot history.
- Exact reads are defined by logical revision contents, not by physical store layout. The code must
  still avoid mutating an already published `PublishedSnapshot` in place.

Phase 14 completion evidence in [71](./71-performance-budgets-design.md#313-m13-read-path-completion-measurements)
recorded:

| Benchmark | Evidence |
| --- | ---: |
| `perf_optimization/read_heavy_delta_counters_1m` | `[4.4792 us, 4.5157 us, 4.5475 us]` |
| Counter sample | `delta_segments_inspected=1400`, `tombstone_checks=300` over 100 checks |
| `perf_optimization/read_heavy_heavy_write_batched_1m` | `[10.820 us, 11.773 us, 12.712 us]` |

These counters are the minimum observability needed for adaptive compaction, but they are global
bench counters. The implementation needs per-view statistics for decisions.

## 3. Goals

| # | Goal | Measure |
| --- | --- | --- |
| G1 | Keep normal writes off the compaction path. | Soft compaction builds checkpoints outside the writer actor; writer p95 for read-heavy heavy batched writes regresses by no more than 5% versus Phase 14 unless a documented hard cap trips. |
| G2 | Bound delta read overhead after writes. | Latest reads after soft compaction return to zero delta-overlay inspections for compacted views, and pre-compaction reads remain within the configured inspected-segment and tombstone-check caps. |
| G3 | Preserve exact-revision semantics. | Exact reads at tokens issued before, during, and after compaction return the same logical results as a reference relationship set. |
| G4 | Make compaction decisions measurable and tunable. | The policy records delta segment count, mutation count, tombstone count, tombstone ratio, read inspection rates, and write batch size. |
| G5 | Avoid runaway background work. | At most one background compaction job per engine is active by default; stale or duplicate requests are coalesced or dropped. |

## 4. Non-Goals

- No public API shape change for checks, lookup, expand, writes, or consistency tokens.
- No multi-version persistent datastore.
- No compaction of every retained historical snapshot. The first implementation targets latest
  head-state read cost.
- No compaction-only user-visible revision in the first implementation.
- No full relationship scan on the read path.
- No new dependency is required. If a later implementation adds a scheduler/channel/config crate,
  it must follow the dependency review process in [60-crates-features-design.md](./60-crates-features-design.md).

## 5. Trigger Metrics

The policy evaluates triggers after every successful relationship write and after benchmark-only
read-counter sampling windows.

### 5.1 View Metrics

Each `RelationshipStoreView` should expose crate-private stats:

```rust
#[derive(Debug, Clone, Copy)]
pub(crate) struct StoreViewDeltaStats {
    pub checkpoint_rows: usize,
    pub delta_segments: usize,
    pub delta_inserted_rows: usize,
    pub delta_deleted_rows: usize,
    pub delta_mutations: usize,
    pub tombstone_ratio_bps: u16,
}
```

For the current single-overlay implementation:

- `delta_segments` is `0` or `1`.
- `delta_inserted_rows` is `delta.inserted.rows().len()` or a cheaper internal row-count accessor.
- `delta_deleted_rows` is `delta.deleted_rows.len()`.
- `delta_mutations` is `delta.mutation_count.get()`.
- `tombstone_ratio_bps` is
  `delta_deleted_rows * 10_000 / max(1, checkpoint_rows + delta_inserted_rows)`, expressed in basis
  points to avoid floating point in policy code.

For a future multi-delta implementation, the same struct keeps the public decision contract:
`delta_segments` becomes the number of retained delta segments, and the other counts become totals
across all segments.

### 5.2 Read Metrics

The current global bench counters should be refined into a scoped sample that can be reset and read
per benchmark window:

```rust
#[derive(Debug, Clone, Copy)]
pub(crate) struct StoreViewReadSample {
    pub checks: u64,
    pub delta_segments_inspected: u64,
    pub tombstone_checks: u64,
}
```

Derived ratios:

- `inspected_segments_per_check = delta_segments_inspected / max(1, checks)`
- `tombstone_checks_per_check = tombstone_checks / max(1, checks)`

The implementation does not need to maintain these counters in production by default. The first
phase can keep them under `bench-internals` and use write-side stats for runtime decisions. A later
runtime metrics feature can expose them through `tracing` or a stats callback.

### 5.3 Write Metrics

The writer actor knows the write batch shape before publication:

- `write_batch_size`: number of submitted mutations.
- `delete_count`: number of delete mutations.
- `touch_count`: number of touch mutations.
- `create_count`: number of create mutations.
- `precondition_count`: number of preconditions, useful for interpreting write latency but not a
  compaction trigger by itself.

Large write batches should lower the soft threshold for scheduling compaction because one batch can
create a delta large enough to affect all later reads. Tiny writes should be coalesced to avoid
thrashing the compactor.

## 6. Trigger Policy

The policy has soft and hard triggers.

Soft triggers schedule background compaction and return the user write immediately. Hard triggers
are emergency caps that may compact synchronously in the writer actor to keep memory and read cost
bounded.

```rust
#[derive(Debug, Clone, Copy)]
pub(crate) struct DeltaCompactionPolicy {
    pub mode: DeltaCompactionMode,
    pub soft_max_delta_segments: NonZeroUsize,
    pub hard_max_delta_segments: NonZeroUsize,
    pub soft_max_delta_mutations: NonZeroUsize,
    pub hard_max_delta_mutations: NonZeroUsize,
    pub soft_max_delta_tombstones: NonZeroUsize,
    pub hard_max_delta_tombstones: NonZeroUsize,
    pub soft_max_tombstone_ratio_bps: u16,
    pub hard_max_tombstone_ratio_bps: u16,
    pub soft_max_inspected_segments_per_check_x100: NonZeroU32,
    pub soft_max_tombstone_checks_per_check_x100: NonZeroU32,
    pub large_write_batch_threshold: NonZeroUsize,
    pub min_revisions_between_compactions: NonZeroU64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DeltaCompactionMode {
    Off,
    SynchronousOnly,
    Background,
}
```

Recommended defaults for the first implementation:

| Field | Default | Reason |
| --- | ---: | --- |
| `mode` | `Background` | Normal writes should not run checkpoint merge. |
| `soft_max_delta_segments` | `8` | Future multi-delta trigger; current single-overlay implementation cannot trip this by segment count alone. |
| `hard_max_delta_segments` | `16` | Future multi-delta cap; current implementation cannot exceed `1`. |
| `soft_max_delta_mutations` | `16_384` | Schedules well before current `100_000` hard cap. |
| `hard_max_delta_mutations` | `100_000` | Preserves current hard behavior. |
| `soft_max_delta_tombstones` | `8_192` | Tombstones affect every matching checkpoint candidate. |
| `hard_max_delta_tombstones` | `100_000` | Preserves current hard behavior. |
| `soft_max_tombstone_ratio_bps` | `100` | 1% deleted rows is enough to start background cleanup. |
| `hard_max_tombstone_ratio_bps` | `500` | 5% deleted rows should not stay in the latest read path. |
| `soft_max_inspected_segments_per_check_x100` | `1_600` | Matches the Phase 14 counter scale: 16.00 inspections/check leaves headroom above the 14.00 sample. |
| `soft_max_tombstone_checks_per_check_x100` | `800` | 8.00 tombstone checks/check is above the Phase 14 3.00 sample but catches delete-heavy reads. |
| `large_write_batch_threshold` | `10_000` | Matches the current mutation batch cap and avoids repeated soft scheduling during seed batches. |
| `min_revisions_between_compactions` | `4` | Prevents compact-reschedule churn under tiny writes. |

Decision order:

1. If mode is `Off`, do nothing.
2. If any hard trigger trips, compact synchronously before publishing the write unless mode is
   `Background` and an already completed fresh background checkpoint is installable.
3. If any soft trigger trips, publish the write as a delta-backed view, then enqueue a background
   compaction request with the latest `Arc<PublishedSnapshot>`.
4. If only read-sample ratios trip, enqueue background compaction unless a newer compaction request
   is already pending for the same latest revision.
5. If a large write batch is the trigger but the resulting delta is tiny because most touches were
   already present, do not compact.

## 7. Background Compaction Architecture

The writer actor remains the only owner of mutable engine state. The background compactor receives
immutable snapshots and returns immutable compacted relationship views.

```text
┌─────────────────────────────── ZanzibarEngine ───────────────────────────────┐
│                                                                              │
│  Public writes                                                               │
│      │                                                                       │
│      ▼                                                                       │
│  ┌─────────────────────┐        try_send         ┌───────────────────────┐   │
│  │ WriterActor         │────────────────────────▶│ DeltaCompactorWorker  │   │
│  │ - validates writes  │                         │ - owns no writer state│   │
│  │ - publishes Arc     │◀────────────────────────│ - builds checkpoint   │   │
│  │ - installs fresh    │   InstallCompaction     │ - returns Result      │   │
│  │   compacted views   │                         └───────────────────────┘   │
│  └─────────┬───────────┘                                                     │
│            │ atomic store                                                    │
│            ▼                                                                 │
│  ┌─────────────────────┐                                                     │
│  │ ArcSwap EngineState │─── lock-free latest/exact reads                     │
│  └─────────────────────┘                                                     │
│                                                                              │
└──────────────────────────────────────────────────────────────────────────────┘
```

The first implementation should use one bounded worker queue per engine:

- queue capacity defaults to `1`;
- only one active job per engine is allowed;
- `try_send` failure is not a write failure;
- newer requests replace older pending requests if the older one has not started;
- worker failure disables only the current compaction attempt and leaves published state unchanged.

No background thread may hold references that require the writer actor to stay alive during drop.
Shutdown must close the queue, let the worker finish or discard its result, and join the worker
without panicking.

## 8. Lifecycle and Sequence

```text
Client                WriterActor              CompactorWorker          ArcSwap EngineState
  │                       │                           │                          │
  │ 1. write mutations    │                           │                          │
  │──────────────────────▶│                           │                          │
  │                       │ 2. validate schema,       │                          │
  │                       │    preconditions, batch   │                          │
  │                       │                           │                          │
  │                       │ 3. apply delta and        │                          │
  │                       │    compute stats          │                          │
  │                       │                           │                          │
  │                       │ 4. publish revision R ──────────────────────────────▶│
  │                       │    checkpoint + delta     │                          │
  │                       │                           │                          │
  │ 5. token R ◀───────── │                           │                          │
  │                       │                           │                          │
  │                       │ 6. soft trigger:          │                          │
  │                       │    enqueue snapshot R ───▶│                          │
  │                       │                           │ 7. build canonical       │
  │                       │                           │    checkpoint from       │
  │                       │                           │    immutable view        │
  │                       │                           │                          │
  │                       │ 8. result R ◀─────────────│                          │
  │                       │                           │                          │
  │                       │ 9. if latest is still R,  │                          │
  │                       │    physical republish ─────────────────────────────▶│
  │                       │    same revision R,       │                          │
  │                       │    compacted relationships│                          │
  │                       │                           │                          │
  │                       │ 10. if latest moved to    │                          │
  │                       │     R+n, discard result   │                          │
  │                       │     and maybe reschedule  │                          │
```

Step 9 is a physical republish, not a logical revision. It creates a new `Arc<PublishedSnapshot>`
with the same revision, schema hash, configs, schema, and logical relationships as the snapshot
that was compacted. It updates the latest `EngineState` and the matching snapshot-history entry for
that revision. Existing readers that already cloned the old `Arc` keep reading the old physical
view; later latest or exact reads may see the compacted physical view for the same token. This is
compatible because no published snapshot is mutated in place and query results are equivalent.

If this same-revision physical republish is considered too subtle during implementation review, the
fallback is to install background compaction only on the next successful write by merging the write
against the prepared checkpoint. With the current cumulative single-delta implementation this
fallback is useful only when no newer writes happened while the worker ran, so physical republish is
the recommended first design.

## 9. Exact Revision Compatibility

Required invariants:

- A compactor input is an `Arc<PublishedSnapshot>`; it never reads mutable writer fields.
- The worker produces a canonical `RelationshipStoreView::from_checkpoint` that is logically
  equivalent to the input view's `rows()`.
- Installation succeeds only when the writer actor's latest revision and schema hash still match
  the compactor input.
- Installation never mutates the old snapshot in place.
- Exact-token lookup may return either old physical view or compacted physical view for the same
  revision, but both must return identical check, lookup, expand, and snapshot-save results.
- If a schema write occurs while compaction runs, the result is stale and must be discarded.
- If a relationship write occurs while compaction runs, the result is stale and must be discarded
  for the current single-overlay implementation.

Property tests should model the physical republish as a no-op over the logical relationship set.

## 10. Writer Isolation

The writer actor may do only bounded work on the soft path:

- compute cheap delta stats;
- decide whether to enqueue;
- publish the user write;
- return the user's token.

The writer actor must not call `canonical_store()` for soft triggers. It may call synchronous
checkpoint merge only for hard triggers. Hard-trigger synchronous compaction is allowed because it
replaces the current hard cap behavior and protects memory/read latency when the background worker
cannot keep up.

To prevent soft compaction from becoming accidental backpressure:

- enqueue with non-blocking `try_send`;
- record a skipped-compaction counter when the queue is full;
- apply revision cooldown before scheduling another job;
- discard stale results without scanning relationships;
- cap active compaction memory to one extra canonical checkpoint per engine by default.

## 11. Configuration Surface

The first public configuration should be builder-based, consistent with existing
`retained_snapshots`, `evaluation_limits`, and `writer_queue_capacity`:

```rust
impl ZanzibarEngineBuilder {
    pub fn delta_compaction_policy(mut self, policy: DeltaCompactionPolicy) -> Self;
}
```

If the project later adds YAML runtime configuration, these names should map directly:

```yaml
deltaCompaction:
  mode: background
  softMaxDeltaMutations: 16384
  hardMaxDeltaMutations: 100000
  softMaxDeltaTombstones: 8192
  hardMaxDeltaTombstones: 100000
  softMaxTombstoneRatioBps: 100
  hardMaxTombstoneRatioBps: 500
  softMaxInspectedSegmentsPerCheckX100: 1600
  softMaxTombstoneChecksPerCheckX100: 800
  largeWriteBatchThreshold: 10000
  minRevisionsBetweenCompactions: 4
```

Validation rules:

- hard thresholds must be greater than or equal to soft thresholds;
- ratio basis points must be `<= 10_000`;
- mode `Off` ignores all thresholds but still validates them;
- queue capacity is fixed at `1` for v1 unless implementation evidence proves a benefit from more.

## 12. Benchmark Gates

Required new or extended benchmarks:

| Benchmark | Requirement |
| --- | --- |
| `perf_optimization/read_heavy_delta_counters_1m` | Extend output with view stats before and after compaction. After a fresh background install, latest reads should report `delta_segments_inspected=0` and `tombstone_checks=0` for the same operation shape. |
| `perf_optimization/adaptive_delta_compaction_burst_1m` | Apply a delete/touch burst that crosses soft thresholds; report write latency, compaction install latency, stale result count, and post-install read counters. |
| `perf_optimization/read_heavy_heavy_write_batched_1m` | Upper estimate regresses by no more than 5% versus Phase 14 completion when hard compaction does not trigger. |
| `concurrent_runtime/read_heavy_heavy_write_batched` | Read ops/s regresses by no more than 5%; write p95 regresses by no more than 10% in background mode with hard compactions reported separately. |
| exact-revision test harness | Tokens from before compaction, at the compacted revision, and after later writes all return reference-equivalent results. |
| snapshot save/load canonicalization | Saving a delta-backed view and saving the compacted physical view produce logically equivalent loaded engines. Byte-for-byte equality is not required unless snapshot ordering remains identical. |

PR benchmark comments for the implementation must compare against Phase 14 completion evidence from
[71](./71-performance-budgets-design.md#313-m13-read-path-completion-measurements), especially:

- `perf_optimization/read_heavy_delta_counters_1m`;
- `perf_optimization/read_heavy_heavy_write_batched_1m`;
- `realworld_authorization/1m_rules/mixed_read_workload` if read counters suggest user-visible
  mixed-read impact.

## 13. Failure Modes

| Failure mode | Required behavior |
| --- | --- |
| Worker queue full | Skip or replace the pending request; do not fail the write. Increment a skipped counter under bench/runtime metrics. |
| Worker panic or channel close | Mark the current job failed, keep current published state, and allow later writes to reschedule. Writer APIs continue returning normal results. |
| Compaction build returns `StoreError` | Discard result and keep current view. This should be treated as an internal invariant failure in tests because input was already published. |
| Result is stale because latest revision changed | Discard in O(1) by comparing revision/schema hash. Do not attempt to patch a cumulative single delta. |
| Continuous writes make every result stale | Apply cooldown/backoff and rely on hard thresholds as a bounded fallback. A future multi-delta suffix replay can reduce stale discards. |
| Hard threshold trips repeatedly | Synchronous compaction is allowed; benchmark comments must report hard compaction count and write p95 impact. |
| Exact history retains old delta-backed snapshots | This is expected. Retention bounds memory; old exact snapshots expire per existing `retained_snapshots`. |
| Snapshot save races with compaction | Save clones one `PublishedSnapshot` first. It writes whichever physical view it cloned, both of which are canonicalized through `encode_snapshot_sections`. |
| Shutdown during background compaction | Drop closes the queue and joins the worker. An unfinished result is discarded; no panic reaches user code. |

## 14. Implementation Phases

### M30.1 - Stats and Policy Types

- Add crate-private `StoreViewDeltaStats`.
- Add `DeltaCompactionPolicy` and validated defaults.
- Add builder plumbing but keep mode effectively `Off` until tests cover installation.
- Extend bench-internals counters to print view stats.

Exit criteria:

- Existing tests and benchmarks pass unchanged in default behavior.
- `perf_optimization/read_heavy_delta_counters_1m` reports view stats.

### M30.2 - Background Worker and Physical Republish

- Add one compaction worker per `ZanzibarEngine`.
- Enqueue latest snapshot after soft triggers using non-blocking send.
- Build canonical checkpoint outside the writer actor.
- Install only when latest revision/schema hash still matches input.
- Republish the same revision with a compacted physical view and updated snapshot-history entry.

Exit criteria:

- Exact revision tests prove logical equivalence before and after physical republish.
- Writer p95 in read-heavy heavy batched mode regresses by no more than 5% when hard compaction does
  not trigger.

### M30.3 - Adaptive Trigger Calibration

- Enable default `Background` mode.
- Calibrate soft thresholds with 1M delete/touch bursts and read-heavy mixed-write benchmarks.
- Add cooldown/backoff for stale compaction results.
- Keep hard thresholds at current `100_000` mutation/tombstone caps unless evidence supports lower
  values.

Exit criteria:

- PR comment includes Phase 14 comparison table.
- Stale result count, skipped enqueue count, and hard compaction count are reported.
- Post-compaction latest read counters return to zero delta overlay overhead for compacted views.

### M30.4 - Future Multi-Delta Suffix Replay

This phase is optional and should not start unless continuous-write benchmarks show too many stale
single-overlay compactions.

- Store per-publication delta segments instead of one cumulative delta.
- Let background compaction of revision `R` install against current revision `R+n` by replaying the
  suffix deltas `R+1..R+n` inside the writer actor.
- Recalibrate `soft_max_delta_segments` and `hard_max_delta_segments`.

Exit criteria:

- Continuous-write stale compaction discard rate drops materially.
- Read-heavy write benchmarks remain within gates.

## 15. AGENTS.md Binding

- Error Handling: compaction worker failures map to typed store/runtime errors internally; public
  write APIs do not erase errors into strings.
- Async & Concurrency: mutable engine state remains owned by the writer actor; background workers
  receive immutable `Arc` snapshots and communicate by bounded channels.
- Type Design & API: thresholds use `NonZeroUsize`, `NonZeroU32`, `NonZeroU64`, or basis-point
  integer newtypes; no `Option<T>` for values with defaults.
- Safety & Security: no `unsafe`, no unchecked indexing, no panic on user-controlled relationships,
  and checked arithmetic for ratios/counters.
- Serialization: no snapshot format change is required.
- Testing: exact-revision equivalence, stale-result discard, queue-full skip, shutdown, hard-cap,
  and delete-heavy compaction tests are required.
- Logging & Observability: metrics report counts, ratios, revisions, and durations; relationship
  payloads are not logged.
- Performance: soft compaction must not call canonical merge on the writer thread.
- Documentation: public docs mention the builder policy only after it becomes a supported public
  API.

## 16. Recommendation

Priority: high after the current read-path PR, but below request-local memoization and compiled
evaluation-plan work for pure read latency. Adaptive compaction directly protects the Phase 14
write optimization from degrading read-heavy services after delete/touch bursts.

Expected benefit:

- Latest reads after a write burst can return to checkpoint-only overhead without waiting for the
  hard `100_000` mutation/tombstone caps.
- Delete-heavy workloads avoid accumulating tombstone checks in common reads.
- Writer latency stays near Phase 14 behavior for soft triggers because checkpoint merge runs
  outside the writer actor.

Main risks:

- Same-revision physical republish is subtle and needs focused exact-token tests.
- Current cumulative single-delta implementation makes background results stale under continuous
  writes; cooldown and hard caps are mandatory.
- Background compaction temporarily holds an extra canonical checkpoint, so memory peaks can rise by
  roughly one store view per active job.

## 17. Cross-References

- <- Depends on: [13-revision-consistency-design.md](./13-revision-consistency-design.md), [16-compact-relationship-store-design.md](./16-compact-relationship-store-design.md), [20-concurrent-engine-runtime-design.md](./20-concurrent-engine-runtime-design.md), [21-performance-optimization-design.md](./21-performance-optimization-design.md), [23-read-performance-optimization-design.md](./23-read-performance-optimization-design.md), [71-performance-budgets-design.md](./71-performance-budgets-design.md)
- -> Intended consumer: future implementation-plan update after worker research is merged.
