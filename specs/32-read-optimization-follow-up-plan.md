# 32 - Read Optimization Follow-Up Plan

Status: draft v1
Owner: Simple Zanzibar
Last updated: 2026-05-25
Depends on: [23](./23-read-performance-optimization-design.md), [25](./25-compiled-computed-userset-shortcut-design.md), [26](./26-request-local-memoization-design.md), [27](./27-compiled-evaluation-plan-design.md), [28](./28-read-path-allocation-reduction-design.md), [29](./29-adaptive-bitmap-index-design.md), [30](./30-adaptive-delta-compaction-design.md), [31](./31-schema-aware-lookup-planner-design.md), [71](./71-performance-budgets-design.md)

## 1. Purpose

Six parallel deep reviews examined the remaining read-optimization directions after Phase 14 and
the compiled computed-userset shortcut follow-up. This spec consolidates those findings into one
implementation order.

The plan is deliberately evidence-gated. Phase 14 already brought mixed read and inherited check
under their gates, so the next work should not add broad architectural complexity unless counters
show the target cost and benchmarks prove the improvement.

## 2. Current Baseline

Use the Phase 14 follow-up measurements as the comparison point:

| Benchmark | Current upper estimate |
| --- | ---: |
| `perf_optimization/check_prepared_1m` | `4.3065 us` |
| `realworld_authorization/1m_rules/check_doc_inherited_workspace_member` | `11.559 us` |
| `realworld_authorization/1m_rules/mixed_read_workload` | `41.808 us` |
| `perf_optimization/lookup_resources_streaming_1m` | `2.3551 ms` |
| `perf_optimization/lookup_subjects_streaming_1m` | `4.7426 us` |
| `perf_optimization/read_heavy_heavy_write_batched_1m` | `12.712 us` |

Parser migration is not part of this plan. The latest timer puts schema parse/compile around
`51 us` inside a roughly `572 ms` full 1M snapshot load, and parsing is absent from steady-state
read APIs.

## 3. Consolidated Ranking

| Rank | Direction | Decision | Why |
| ---: | --- | --- | --- |
| 1 | Measurement foundation | Do first. | Every candidate needs hit-rate, allocation, candidate-count, delta, or posting-shape evidence. |
| 2 | Request-local memoization | Do, scoped to multi-check request surfaces. | Moderate expected gain, low API risk, directly targets repeated subchecks in lookup and permission enumeration. |
| 3 | Allocation reduction | Do the streaming collector slice after allocation counters. | Low semantic risk if `lookup_subjects` stops building an intermediate expand tree only for internal collection. |
| 4 | Adaptive delta compaction | Do after stats exist. | Protects Phase 14 write/read tradeoff under delete-heavy or touch-heavy bursts. |
| 5 | Compiled evaluation plan | Prototype after simpler wins. | Modest expected gain, useful foundation, but more migration surface than memoization. |
| 6 | Schema-aware lookup planner | Defer full planner; instrument first. | Highest lookup upside, highest soundness risk. |
| 7 | Adaptive bitmap index | Defer broad index change; start with tombstone mask and histograms only. | Needs set-aware queries or planner support to pay for complexity. |

## 4. Dependency Shape

```text
                     +-----------------------------+
                     | M15.0 Measurement Baseline  |
                     | counters + fixtures         |
                     +--------------+--------------+
                                    |
          +-------------------------+-------------------------+
          |                         |                         |
          v                         v                         v
+-------------------+     +---------------------+     +----------------------+
| M15.1 Request     |     | M15.2 Allocation    |     | M15.3 Delta         |
| Memoization       |     | Reduction           |     | Compaction Stats    |
+---------+---------+     +----------+----------+     +----------+-----------+
          |                          |                           |
          +--------------------------+---------------------------+
                                     |
                                     v
                         +------------------------+
                         | M15.4 Compiled Plan    |
                         | flat arena prototype   |
                         +-----------+------------+
                                     |
                     +---------------+----------------+
                     |                                |
                     v                                v
          +-----------------------+        +--------------------------+
          | M15.5 Lookup Planner  |        | M15.6 Adaptive Bitmap    |
          | producer pruning      |        | tombstone + histograms   |
          +-----------------------+        +--------------------------+
```

## 5. Implementation Plan

### M15.0 - Measurement Baseline

Specs: [26](./26-request-local-memoization-design.md), [28](./28-read-path-allocation-reduction-design.md), [29](./29-adaptive-bitmap-index-design.md), [30](./30-adaptive-delta-compaction-design.md), [31](./31-schema-aware-lookup-planner-design.md), [71](./71-performance-budgets-design.md)

Tasks:

- Add benchmark-only counters for check memo hit opportunities, lookup candidate count, full-root
  check count, allocation count/bytes, delta stats, tombstone checks, and posting length
  histograms.
- Add targeted fixtures:
  - shared parent/group memoization fixture;
  - lookup-subjects allocation fixture;
  - delete-heavy delta fixture;
  - high-fanout posting fixture;
  - lookup planner producer-pruning fixture.
- Record Phase 14 follow-up comparison tables in [71](./71-performance-budgets-design.md).

Exit:

- Each later milestone has a baseline counter and a failing or improvable fixture.
- No production behavior changes.
- Full gates pass.

### M15.1 - Request-Local Memoization

Specs: [26](./26-request-local-memoization-design.md), [14](./14-evaluation-engine-design.md), [71](./71-performance-budgets-design.md)

Tasks:

- Add an optional request-local completed-check memo inside `EvaluationContext`.
- Enable it only for lookup and permission-enumeration surfaces in the first pass.
- Keep active-cycle detection before memo lookup.
- Store `depth_required` on cache entries so hits cannot hide `DepthExceeded`.
- Do not enable the memo for default single `check` unless benchmarks prove no regression.

Exit:

- Memo-specific repeated-subcheck fixture improves by at least 10% upper estimate or hit-rate
  evidence explains why not.
- `check_prepared_1m` regresses by no more than 2%.
- Existing cycle, depth, fanout, deny, exact-token, and lookup tests pass.

Implementation note: 15.1 kept memoization out of `lookup_subjects` after measurement showed
`0` hits and a `~21%` regression on the allocation fixture. `lookup_resources` and permission
enumeration remain memo-enabled.

### M15.2 - Allocation Reduction

Specs: [28](./28-read-path-allocation-reduction-design.md), [14](./14-evaluation-engine-design.md), [71](./71-performance-budgets-design.md)

Tasks:

- Convert internal `lookup_subjects` collection to stream from relation expansion instead of
  always building an intermediate `ExpandedUserset` tree.
- Add request-local scratch collections for candidate verification where counters show repeated
  allocation.
- Defer `SmallVec` or `arrayvec` until allocation counters prove local container churn dominates.

Exit:

- `lookup_subjects_streaming_1m` allocation count drops materially and latency regresses by no more
  than 5%.
- Public `expand` output shape and lookup semantics remain unchanged.
- No new dependency unless a measured `SmallVec` experiment beats local scratch storage.

Implementation note: the large `lookup_subjects` allocation win landed with streaming plus the
15.5 exact positive-proof shortcut. Exclusion, intersection, tuple-to-userset, and recursive
fallbacks still run full root verification, and nested userset relations recompute their own
verification requirement.

### M15.3 - Adaptive Delta Compaction

Specs: [30](./30-adaptive-delta-compaction-design.md), [16](./16-compact-relationship-store-design.md), [20](./20-concurrent-engine-runtime-design.md), [71](./71-performance-budgets-design.md)

Tasks:

- Add crate-private `StoreViewDeltaStats` and configurable soft/hard compaction policy.
- Preserve current hard cap as the safety fallback.
- Add a background compaction path that builds a logically equivalent checkpoint without blocking
  normal writes.
- Install compacted latest snapshots only when revision/state identity is still compatible.

Exit:

- Delete-heavy fixture returns latest reads to checkpoint-like delta counters after compaction.
- `read_heavy_heavy_write_batched_1m` writer/read workload regresses by no more than 5% when hard
  compaction does not trigger.
- Exact tokens before, during, and after compaction read identical logical relationship sets.
- Peak RSS impact is measured and documented.

### M15.4 - Compiled Evaluation Plan Prototype

Specs: [27](./27-compiled-evaluation-plan-design.md), [25](./25-compiled-computed-userset-shortcut-design.md), [71](./71-performance-budgets-design.md)

Tasks:

- Add flat `EvaluationPlan` node and child arenas beside the current compiled enum tree.
- Build plans at schema compile time.
- Run check interpreter in shadow/equivalence tests before switching the hot path.
- Keep tuple-to-userset computed targets name-based because intermediate namespace is dynamic.
- Do not implement a full stack VM in this phase.

Exit:

- Tree-vs-plan check equivalence passes for existing and randomized small schemas.
- `check_prepared_1m`, inherited check, and mixed read do not regress by more than 5%; expected win
  is only 1-5%.
- Expand remains on the old tree until a separate equivalence pass proves shape preservation.

### M15.5 - Schema-Aware Lookup Planner

Specs: [31](./31-schema-aware-lookup-planner-design.md), [14](./14-evaluation-engine-design.md), [16](./16-compact-relationship-store-design.md), [71](./71-performance-budgets-design.md)

Tasks:

- Start with instrumentation only: candidate count, full-root check count, relation producer count,
  residual verification count.
- Add positive producer pruning for union and plain computed-userset paths while retaining final
  full `check`.
- Add residual verification only for proof shapes with a clear soundness argument.
- Keep conservative fallback for intersection, exclusion, tuple-to-userset dynamic namespace, and
  `AllowedSubjectTypes::Unspecified` until tests prove exactness.

Exit:

- `lookup_resources_streaming_1m` improves by at least 25% upper estimate or profile evidence
  explains the remaining bottleneck.
- No returned lookup result can skip full check unless the residual proof is exact and covered by
  adversarial tests.
- `CheckOnly` unsupported-operation behavior remains typed and unchanged.

### M15.6 - Adaptive Bitmap and Tombstone Representations

Specs: [29](./29-adaptive-bitmap-index-design.md), [16](./16-compact-relationship-store-design.md), [22](./22-snapshot-file-size-optimization-design.md), [71](./71-performance-budgets-design.md)

Tasks:

- Implement posting histograms and a row-id tombstone-mask abstraction first.
- Add runtime-only `DenseBitset` only for dense/high-cardinality row-id sets where counters justify
  it.
- Defer `roaring` and snapshot v4 encoding until runtime-only evidence proves the representation
  pays for itself.
- Pair bitmap work with set-aware query paths; replacing `Vec<RowId>` alone is not enough.

Exit:

- Delete-heavy tombstone checks drop on the targeted fixture.
- Snapshot load and file-size baselines do not regress unless a documented v4 design explains the
  tradeoff.
- Direct check does not regress.

## 6. Rejected or Deferred Work

| Direction | Decision |
| --- | --- |
| `pest` -> `winnow` parser migration | Deferred; not on steady-state read path and negligible in current full-load timer. |
| Full bytecode VM | Deferred; flat plan is enough to test locality and precomputed strategy without obscuring evaluator semantics. |
| Global semantic cache | Rejected for this local engine phase; request-local memoization is revision-safe and simpler. |
| Immediate `roaring` dependency | Deferred until histograms and runtime-only bitset evidence justify dependency and snapshot-format costs. |
| Skipping final lookup verification broadly | Rejected unless residual proof is exact and adversarial tests cover the expression family. |

## 7. PR and Benchmark Reporting

Every implementation PR in this plan must post:

- Phase 14 follow-up baseline comparison table.
- Target benchmark table with lower/estimate/upper values.
- Counter deltas for the relevant feature.
- Statement of whether direct check, inherited check, mixed read, lookup resources, lookup subjects,
  snapshot load, and raw/zstd file sizes regressed.
- Full gate list, including strict clippy, docs, audit, and deny.

## 8. Cross-References

- Memoization: [26-request-local-memoization-design.md](./26-request-local-memoization-design.md)
- Compiled plan: [27-compiled-evaluation-plan-design.md](./27-compiled-evaluation-plan-design.md)
- Allocation reduction: [28-read-path-allocation-reduction-design.md](./28-read-path-allocation-reduction-design.md)
- Adaptive bitmap: [29-adaptive-bitmap-index-design.md](./29-adaptive-bitmap-index-design.md)
- Delta compaction: [30-adaptive-delta-compaction-design.md](./30-adaptive-delta-compaction-design.md)
- Lookup planner: [31-schema-aware-lookup-planner-design.md](./31-schema-aware-lookup-planner-design.md)
- Performance budgets: [71-performance-budgets-design.md](./71-performance-budgets-design.md)
