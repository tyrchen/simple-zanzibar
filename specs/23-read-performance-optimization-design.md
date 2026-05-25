# 23 - Read Performance Optimization Design

Status: draft v1
Owner: Simple Zanzibar
Last updated: 2026-05-24
Depends on: [14](./14-evaluation-engine-design.md), [16](./16-compact-relationship-store-design.md), [20](./20-concurrent-engine-runtime-design.md), [21](./21-performance-optimization-design.md), [22](./22-snapshot-file-size-optimization-design.md), [71](./71-performance-budgets-design.md)

## 1. Purpose

Phase 12 removed write amplification and improved realistic checks, but the 1M realworld mixed-read
workload still misses the <= 55 us target at `[57.221 us, 57.733 us, 58.164 us]`. Snapshot file-size
work will reduce artifacts, but it will not automatically make hot reads faster because `check`
already uses the exact resource index retained by every profile.

This spec defines the next read-performance pass. It targets evaluator allocation, recursive
relation materialization, segment/delta lookup overhead after writes, and schema-expression
shortcuts that preserve Zanzibar semantics.

## 2. Current Evidence

Measured 2026-05-24 after Phase 12:

| Benchmark | Measurement | Status |
| --- | ---: | --- |
| `perf_optimization/check_prepared_1m` | `[5.8971 us, 5.9513 us, 6.0009 us]` | stable prepared-check baseline |
| `perf_optimization/lookup_resources_streaming_1m` | `[3.0446 ms, 3.1188 ms, 3.1883 ms]` | within existing lookup budget |
| `perf_optimization/lookup_subjects_streaming_1m` | `[6.2956 us, 6.3200 us, 6.3451 us]` | stable |
| `realworld_authorization/1m_rules/check_doc_inherited_workspace_member` | `[14.793 us, 14.966 us, 15.110 us]` | improved >10% versus prior baseline |
| `realworld_authorization/1m_rules/mixed_read_workload` | `[57.221 us, 57.733 us, 58.164 us]` | still misses <= 55 us |
| `read_heavy_*_1m` mixed write harnesses | mostly `13-18 us` | store cloning is no longer dominant |

The section-size spike also clarifies what not to expect: `CheckOnly` removes unused broad/reverse
indexes, but it retains the exact resource index used by `check`, so file-size wins are orthogonal
to evaluator hot-path latency.

M14 first-pass update on 2026-05-24: the low-risk recursion-state change from
[24](./24-zstd-aware-snapshot-load-design.md) replaced active-recursion `HashSet`s with bounded
stacks. This is a subset of the reusable-context design below, not the full schema ID IR. It moved
the mixed-read workload under budget and improved guardrails:

| Benchmark | Phase 13 | First pass | Status |
| --- | ---: | ---: | --- |
| `realworld_authorization/1m_rules/mixed_read_workload` | `[57.221 us, 57.733 us, 58.164 us]` | `[52.474 us, 52.895 us, 53.489 us]` | passes <= 55 us |
| `realworld_authorization/1m_rules/check_doc_inherited_workspace_member` | `[14.793 us, 14.966 us, 15.110 us]` | `[14.473 us, 14.655 us, 14.833 us]` | improved; still above 13.5 us stretch |
| `perf_optimization/check_prepared_1m` | `[5.8971 us, 5.9513 us, 6.0009 us]` | `[5.2677 us, 5.3115 us, 5.3424 us]` | improved |
| `perf_optimization/lookup_resources_streaming_1m` | `[3.0446 ms, 3.1188 ms, 3.1883 ms]` | `[2.6849 ms, 2.7143 ms, 2.7544 ms]` | improved |
| `perf_optimization/lookup_subjects_streaming_1m` | `[6.2956 us, 6.3200 us, 6.3451 us]` | `[5.4088 us, 5.4472 us, 5.4722 us]` | improved |

M14 completion update on 2026-05-24: schema rewrites now retain same-namespace compiled relation
ids, plain computed-userset edges use a conservative exact shortcut, lookup verification resets a
request-local reusable context between candidates, and segmented delta views mask checkpoint
tombstones with checkpoint-native row identities instead of materializing public relationships.

| Benchmark | First pass | Completion pass | Status |
| --- | ---: | ---: | --- |
| `realworld_authorization/1m_rules/mixed_read_workload` | `[52.474 us, 52.895 us, 53.489 us]` | `[41.599 us, 42.085 us, 42.690 us]` | passes <= 55 us |
| `realworld_authorization/1m_rules/check_doc_inherited_workspace_member` | `[14.473 us, 14.655 us, 14.833 us]` | `[11.540 us, 11.599 us, 11.646 us]` | passes <= 13.5 us stretch |
| `perf_optimization/check_prepared_1m` | `[5.2677 us, 5.3115 us, 5.3424 us]` | `[4.3107 us, 4.3867 us, 4.4450 us]` | improved |
| `perf_optimization/lookup_resources_streaming_1m` | `[2.6849 ms, 2.7143 ms, 2.7544 ms]` | `[2.2853 ms, 2.3241 ms, 2.3551 ms]` | improved |
| `perf_optimization/lookup_subjects_streaming_1m` | `[5.4088 us, 5.4472 us, 5.4722 us]` | `[4.6735 us, 4.7194 us, 4.7426 us]` | improved |
| `perf_optimization/read_heavy_heavy_write_batched_1m` | `[10.810 us, 11.567 us, 12.365 us]` | `[10.820 us, 11.773 us, 12.712 us]` | no detected regression |
| `perf_optimization/read_heavy_delta_counters_1m` | not present | `[4.4792 us, 4.5157 us, 4.5475 us]`; `delta_segments_inspected=1400`, `tombstone_checks=300` over 100 checks | counters recorded |

## 3. Goals

| # | Goal | Measure |
| --- | --- | --- |
| G1 | Bring the realistic mixed-read workload under budget. | `realworld_authorization/1m_rules/mixed_read_workload` upper estimate <= 55 us. |
| G2 | Improve inherited check latency without API changes. | `realworld_authorization/1m_rules/check_doc_inherited_workspace_member` upper estimate <= 13.5 us, or profile evidence explains the remaining cost. |
| G3 | Preserve fast direct checks and lookup behavior. | `check_prepared_1m`, `lookup_subjects_streaming_1m`, and `lookup_resources_streaming_1m` regress by no more than 5%. |
| G4 | Keep post-write read overhead bounded. | Read-heavy mixed write benchmarks stay within 5% of their Phase 12 baselines after delta/native-key changes. |
| G5 | Make allocation reductions visible. | Bench reports or profiler captures show fewer owned `Object`/`Relation`/`Relationship` materializations on recursive read paths. |

## 4. Non-Goals

- No semantic cache that can return stale results across revisions.
- No skipping final verification for exclusion, intersection, fanout-limit, or depth-limit cases.
- No public API shape change for `check`, `expand`, `lookup_resources`, or `lookup_subjects`.
- No `unsafe`, no unchecked indexing, and no panic path reachable from user input.
- No full relationship-store scan as a fallback for missing profile indexes.

## 5. Target Read Path

```text
Public request
  |
  v
+--------------------------+
| Prepare IDs once         |
| - public strings -> ids  |
| - per-segment key plan   |
+------------+-------------+
             |
             v
+--------------------------+        +--------------------------+
| Compiled schema IR       |        | Reusable eval context    |
| - relation ids in nodes  |<------>| - generation counters    |
| - shortcut descriptors   |        | - visited check keys     |
+------------+-------------+        +------------+-------------+
             |                                   |
             v                                   v
+-------------------------------------------------------------+
| Evaluator core                                              |
| - exact direct relation fast path                           |
| - ID-native computed userset / tuple-to-userset recursion   |
| - safe shortcut only when proof is exact                    |
+------------------------------+------------------------------+
                               |
                               v
+-------------------------------------------------------------+
| RelationshipStoreView                                        |
| - checkpoint segment                                         |
| - bounded delta segments                                     |
| - segment-native keys and tombstone masking                  |
+-------------------------------------------------------------+
```

## 6. Design

### 6.1 Measurement First

Before changing evaluator structure, capture one CPU profile for the failing realworld mixed-read
case and one for inherited check. The profile must classify time into:

- public request preparation;
- schema expression dispatch;
- store key lookup;
- userset recursion;
- visited/context work;
- public model materialization.

The profile does not need to become a committed artifact, but [71](./71-performance-budgets-design.md)
must record the measured bottleneck if a target is recalibrated.

### 6.2 Complete Schema ID IR

Compiled schema expressions should carry relation ids instead of reconstructing public
`Relation(String)` values during recursion. Target internal shape:

```rust
enum CompiledUsersetExpression {
    This,
    ComputedUserset { relation: RelationId },
    TupleToUserset {
        tupleset: RelationId,
        computed_userset: RelationId,
    },
    Union(SmallVec<[CompiledUsersetExpression; 4]>),
    Intersection(SmallVec<[CompiledUsersetExpression; 2]>),
    Exclusion {
        base: Box<CompiledUsersetExpression>,
        subtract: Box<CompiledUsersetExpression>,
    },
}
```

The concrete container can use existing project dependencies only. If `SmallVec` is introduced, it
requires the dependency review process from [60](./60-crates-features-design.md). Without a new
dependency, use `Vec` first and optimize only with allocation evidence.

### 6.3 Segment-Aware ID-Native Store Keys

Segmented publication improved writes by allowing checkpoint plus delta stores. The remaining read
cost risk is that each segment may have its own interner. The evaluator should prepare a
`StoreLookupPlan` per request:

```rust
struct StoreLookupPlan {
    checkpoint: Option<SegmentLookupKeys>,
    deltas: Vec<SegmentLookupKeys>,
}

struct SegmentLookupKeys {
    segment_id: SegmentId,
    resource_key: ResourceIndexKey,
    subject_key: Option<SubjectIndexKey>,
}
```

The plan translates public identifiers into each segment's ids once. Recursive checks reuse the
segment-native keys instead of materializing public `Object`, `Relation`, or `Relationship` values
between segment lookups.

### 6.4 Reusable Evaluation Context

Lookup and mixed workloads run many candidate checks. A reusable context should use generation
counters:

```rust
struct ReusableEvaluationContext {
    generation: NonZeroU32,
    visited_checks: HashMap<EvalCheckKey, NonZeroU32>,
}
```

Reset increments `generation`; overflow clears the maps and restarts at one. This preserves cycle
detection while avoiding per-candidate allocation churn. The context is request-local and never
shared across threads.

### 6.5 Exact-Proof Shortcuts

Shortcuts are allowed only when they preserve the normal check proof:

- `This` on exact `object#relation@subject` may return allowed from an exact resource posting.
- A computed-userset recursion may return its recursive result directly when the same subject and
  exact object relation were evaluated in the current context.
- Tuple-to-userset may reuse the recursive computed relation proof for the intermediate userset
  object when the tupleset edge was read from the same snapshot revision.

Shortcuts are not allowed for exclusion, intersection, partial fanout, or depth-limit boundaries
unless the normal algebra has already proven the result.

### 6.6 Delta Read Boundaries

Each `RelationshipStoreView` query should expose how many delta segments were inspected and how many
tombstone checks were performed under benchmark-only instrumentation. Checkpoint thresholds should
use these counters:

- checkpoint when delta segment count exceeds the configured maximum;
- checkpoint when tombstone checks per returned candidate exceed a measured ratio;
- checkpoint before snapshot save if a compact artifact would otherwise need to merge many deltas.

The default thresholds should favor read latency because local authorization is read-heavy.

## 7. Correctness Invariants

- ID-native evaluation returns the same result as the public model evaluator for every existing
  schema expression.
- Cycle detection keys include enough segment/relation identity to prevent false negatives and false
  positives.
- Exact revision reads observe the same `StoreView` that was current at publication time.
- Delta deletes mask older rows, and touches do not produce duplicate candidates.
- Reusable contexts are request-local and cannot leak visited state between public calls.
- Unsupported index profiles still fail loudly instead of scanning.

## 8. Benchmarks and Gates

Required benchmark targets for the implementation phase:

```text
make bench-realworld
make bench-perf-optimization
make bench-snapshot-section-size
```

New or updated filters:

| Benchmark | Gate |
| --- | --- |
| `realworld_authorization/1m_rules/mixed_read_workload` | upper estimate <= 55 us |
| `realworld_authorization/1m_rules/check_doc_inherited_workspace_member` | upper estimate <= 13.5 us or profile-backed recalibration |
| `perf_optimization/check_prepared_1m` | no > 5% regression |
| `perf_optimization/lookup_subjects_streaming_1m` | no > 5% regression |
| `perf_optimization/read_heavy_heavy_write_batched_1m` | no > 5% regression |
| benchmark-only delta counters | segment and tombstone overhead recorded for read-heavy write cases |

## 9. Phasing

### M14.0 - Profile and Counter Baseline

- Capture CPU profile for inherited check and mixed read.
- Add benchmark-only counters for delta segment inspections and tombstone checks.
- Record any recalibrated bottleneck in [71](./71-performance-budgets-design.md).

Exit: the failing read budget has a measured dominant cost.

### M14.1 - Schema ID IR

- Store relation ids in compiled schema expression nodes.
- Remove recursive public `Relation(String)` materialization from computed-userset and
  tuple-to-userset paths.
- Add evaluator equivalence tests for every expression kind.

Exit: inherited check improves or profile shows store lookup dominates.

### M14.2 - Segment-Native Lookup Plan

- Prepare per-segment resource/subject keys once per request.
- Keep recursive checks segment-native across checkpoint and deltas.
- Add post-write exact-revision tests that compare against a reference relationship set.

Exit: read-heavy write benchmarks stay within gate after many deltas.

### M14.3 - Reusable Contexts

- Add generation-counter reset for lookup verification loops.
- Ensure overflow clears maps deterministically.
- Add tests proving visited state does not leak across candidates.

Exit: lookup and mixed-read allocation profiles improve without latency regression.

### M14.4 - Exact-Proof Shortcuts

- Add direct `This` and safe computed-userset shortcuts.
- Keep exclusion/intersection verification conservative.
- Add adversarial tests for deny, intersection, fanout limit, depth limit, and cycles.

Exit: mixed-read and inherited-check gates pass or the spec is recalibrated with profile evidence.

## 10. AGENTS.md Binding

- Error Handling: evaluator and profile failures remain typed; no string-only error erasure.
- Async & Concurrency: contexts are request-local; shared read state stays immutable behind
  `ArcSwap`/`Arc`.
- Type Design & API: internal ids must make illegal states unrepresentable; public API types remain
  stable.
- Safety & Security: no `unsafe`, no unchecked indexing, no panic on user-controlled schema or
  relationship data.
- Serialization: no snapshot-format change is required by this spec; any row/key encoding changes
  belong to [22](./22-snapshot-file-size-optimization-design.md).
- Testing: equivalence, exact revision, profile unsupported-operation, cycle, fanout, and deny-list
  tests are required.
- Performance: no shortcut lands without benchmark evidence and a correctness test for the branch
  it skips.
- Documentation: public docs only change if index-profile capability language changes.

## 11. Risks and Open Questions

- Schema ID IR may expose that store segment translation dominates. If so, M14.2 becomes the main
  performance phase.
- Reusable contexts reduce allocation but can make cycle bugs harder to inspect. Tests must include
  nested tuple-to-userset and repeated candidate checks.
- Exact-proof shortcuts are easy to over-apply. The first implementation should be conservative and
  add shortcuts one expression family at a time.
- Adding `SmallVec` may help expression trees, but it is not justified without allocation evidence
  and dependency review.

## 12. Cross-References

- <- Depends on: [14-evaluation-engine-design.md](./14-evaluation-engine-design.md), [16-compact-relationship-store-design.md](./16-compact-relationship-store-design.md), [20-concurrent-engine-runtime-design.md](./20-concurrent-engine-runtime-design.md), [21-performance-optimization-design.md](./21-performance-optimization-design.md), [22-snapshot-file-size-optimization-design.md](./22-snapshot-file-size-optimization-design.md), [71-performance-budgets-design.md](./71-performance-budgets-design.md)
- -> Consumed by: [72-testing-verification-plan.md](./72-testing-verification-plan.md), [91-local-engine-impl-plan.md](./91-local-engine-impl-plan.md)
- Related research: [../docs/research/spike-snapshot-section-size.md](../docs/research/spike-snapshot-section-size.md)
