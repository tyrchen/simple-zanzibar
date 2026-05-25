# 26 - Request-Local Memoization Design

Status: draft v1
Owner: Simple Zanzibar
Last updated: 2026-05-25
Depends on: [14](./14-evaluation-engine-design.md), [16](./16-compact-relationship-store-design.md), [23](./23-read-performance-optimization-design.md), [25](./25-compiled-computed-userset-shortcut-design.md), [71](./71-performance-budgets-design.md)

## 1. Purpose

Phase 14 moved the read path under the current gates by making schema recursion ID-native,
querying resource/relation postings directly, keeping lookup verification contexts reusable, and
moving computed-userset shortcut facts into compiled schema IR. The remaining repeated work is
semantic rather than structural: one public read request can still prove the same
`object#relation@subject` check more than once while walking lookup candidates, shared parent
edges, or multiple permissions on one resource.

This spec defines a request-scoped check result cache owned by `EvaluationContext`. It is not a
global cache, not a revision cache, and not a cross-thread service cache. The target is to avoid
duplicate recursive checks inside one immutable snapshot read while preserving the existing cycle,
depth, fanout, and exact-revision semantics.

## 2. Decision

Request-local memoization is worth implementing as a benchmark-gated read-path follow-up, with one
important constraint: it should be enabled first only for multi-check request surfaces and lookup
verification contexts, not for the default single `check` hot path unless profiles show enough
duplicate recursion to pay for the extra hash lookup.

Rationale:

- `perf_optimization/lookup_resources_streaming_1m` is still millisecond-scale at
  `[2.2853 ms, 2.3241 ms, 2.3551 ms]`, while direct prepared checks are already microsecond-scale.
- `lookup_permissions` and `lookup_object_permissions` evaluate multiple relations over the same
  resource and subject. Real-world schemas often share `owner`, `editor`, `viewer`, and `banned`
  subrelations across permissions.
- Tuple-to-userset inheritance creates converging subchecks such as
  `workspace#member@user` from many documents or folders.
- SpiceDB prior art keeps caching outside graph expression evaluation and guards cached check
  results with depth requirements. The local design should adopt that placement lesson, but keep
  the cache request-local rather than service-wide.

This should be prioritized after the Phase 14 read-path work and before larger planner or bitmap
index work. Expected benefit is moderate: roughly 5-15 percent on lookup and mixed read scenarios
with repeated subchecks, higher on synthetic shared-parent fixtures, and near-zero or disabled
overhead on direct checks.

## 3. Current Read Path

Relevant current surfaces:

- `src/eval.rs` owns `EvaluationContext`, `CheckKey`, cycle stacks, generation counters,
  `check`, `check_prepared`, `lookup_resources_with_snapshot`, and
  `lookup_subjects_with_snapshot`.
- `lookup_resources_with_snapshot` already creates one `EvaluationContext` and calls
  `reset_for_reuse()` between candidate checks.
- `lookup_subjects_with_snapshot` already creates separate reusable expand and check contexts.
- `lookup_permissions` in `src/api.rs` currently calls `check_prepared_with_snapshot` once per
  relation, which creates a fresh context for every permission candidate.
- `lookup_object_permissions` currently calls `lookup_subjects_with_snapshot` once per relation,
  which prevents sharing check memo entries across permission groups.
- `src/relationship.rs` exposes immutable `RelationshipStoreView` snapshots, resource/relation
  direct reads, and checkpoint-plus-delta masking. A request-local cache can trust this snapshot to
  remain stable for the whole request.

The existing generation maps in `EvaluationContext` are active-recursion indexes. They are not a
completed-result cache. They answer "is this key currently on the stack?" rather than "has this
semantic check already completed?"

## 4. Ownership Flow

```text
+--------------------- public read API ----------------------+
| check / lookup_resources / lookup_subjects / permission    |
| enumeration resolves Consistency into one PublishedSnapshot |
+---------------------------+--------------------------------+
                            |
                            v
        +-------------------+-------------------+
        | EvaluationContext<'snapshot>          |
        | - immutable snapshot reference        |
        | - depth and fanout limits             |
        | - active recursion generation indexes |
        | - optional RequestCheckMemo           |
        +-------------+-------------------------+
                      |
          cache hit?  | no
          +-----------+-----------+
          |                       |
          v                       v
 +----------------+     +-------------------------+
 | return cached  |     | enter active stack      |
 | Membership     |     | evaluate compiled IR    |
 | if depth fits  |     | query RelationshipStore |
 +----------------+     +------------+------------+
                                      |
                                      v
                         +------------+------------+
                         | insert completed result |
                         | if cacheable and cap OK |
                         +-------------------------+

reset_for_reuse() clears active recursion state for the next candidate, but it does not clear the
request memo. Dropping the public read request drops both the context and the memo.
```

## 5. Cache Key Design

The memo key must identify the semantic check, not the current expression branch. It must also avoid
reintroducing public `Relation(String)` materialization on hot recursive paths.

Target private shape:

```rust
enum CheckMemoKey {
    Store(StoreCheckKey),
    Public {
        object: Object,
        relation: RelationName,
        user: User,
    },
}
```

Rules:

- Prefer `Store(StoreCheckKey)` when the current `RelationshipStoreView` can intern the exact
  resource, relation, and subject. This keeps the common compact-snapshot key small.
- Use `Public` only after the relation has been validated as `RelationName`. This avoids caching
  invalid public strings and lets top-level `Relation("viewer")` and recursive `RelationName`
  checks share the same fallback key.
- Include the full `Object` namespace/id and full `User`. This separates identical relation names
  across namespaces and userset subjects such as `group:eng#member`.
- Do not include revision or schema hash in the key. The memo is owned by
  `EvaluationContext<'snapshot>` and cannot outlive the immutable `PublishedSnapshot`.
- Future caveat or contextual conditions must extend the key with a typed caveat-context digest
  before conditional results become cacheable.

Do not reuse `CheckKey` directly as the memo key. `CheckKey::Public` stores an unvalidated public
`Relation`, and active-cycle keys have slightly different needs from completed-result keys.

## 6. Cache Entry Design

Target private shape:

```rust
struct CheckMemoEntry {
    membership: Membership,
    depth_required: NonZeroU32,
}
```

`depth_required` is mandatory. A completed check result is reusable only when the caller has enough
remaining depth to reproduce the proof without hiding a depth-limit error. The evaluator should
compute it by tracking the minimum remaining depth observed while the frame is active:

```text
entry_remaining_depth = remaining_depth before enter()
minimum_remaining_depth = lowest remaining_depth seen inside this frame
depth_required = entry_remaining_depth - minimum_remaining_depth
```

Every check consumes at least one depth unit, so `depth_required` must be non-zero. A cache hit is
valid only when `current_remaining_depth >= depth_required`.

Do not cache:

- active-cycle short-circuit denials;
- `DepthExceeded`, `FanoutExceeded`, store errors, validation errors, or unsupported-index errors;
- partial expression results;
- expanded usersets;
- results that required future caveat evaluation.

Caching both `Allowed` and `Denied` completed memberships is safe within one immutable snapshot when
the depth requirement is satisfied. Negative entries are important for lookup workloads that reject
many candidates through the same inherited relation path.

## 7. Request-Local and Revision-Safe Semantics

The cache is revision-safe by construction:

- A public API call resolves `Consistency` into one `Arc<PublishedSnapshot>` before creating the
  context.
- `RequestCheckMemo` is stored inside `EvaluationContext<'snapshot>`, not in `ZanzibarEngine`,
  `EngineState`, `RelationshipStoreView`, or a global static.
- Writes publish a new snapshot; existing request contexts keep reading their old snapshot and are
  dropped at request completion.
- `Consistency::Exact(token)` and `Consistency::Latest` never share a memo because they create
  distinct request contexts.
- `reset_for_reuse()` advances active-recursion generation state but keeps completed memo entries
  only for the same public request.

The first implementation should expose no public API and should add no shared synchronization
primitive. A `HashMap` inside the request context is enough.

## 8. Cycle, Depth, and Fanout Semantics

### 8.1 Active Cycle Order

The evaluator must check the active-recursion stack before checking the memo:

```text
if key is active in the current generation:
    return Denied without reading or writing the memo
else if memo has key and current depth can satisfy entry.depth_required:
    return entry.membership
else:
    evaluate normally and maybe insert completed result
```

This prevents a cached positive result from being used to prove a recursive self-dependency while
the same key is already active in the current proof.

### 8.2 Depth Requirement

Depth-limit semantics are observable because current code returns `DepthExceeded` rather than
silently denying. A memo hit must not bypass that error. The stored `depth_required` is the local
equivalent of SpiceDB's cached-depth guard.

Counterexample the implementation must reject:

```text
1. A request computes doc:a#can_view@user:u at the top level with depth 50.
2. The same key appears later under a branch with only depth 1 remaining.
3. Returning the cached result would skip a depth error if the proof needs more than one level.
```

The hit is valid only if the entry's `depth_required` is <= the current remaining depth.

### 8.3 Fanout

Fanout limits are per evaluator step. A cached nested result may skip the nested step's internal
fanout work, but the caller still pays the fanout increment for the edge that led to the nested
check. Errors are never cached, so `FanoutExceeded` remains observable.

The implementation must keep existing fanout increments in `eval_this`, `eval_tuple_to_userset`,
and expand paths. The memo applies only to `check` results, not relationship iteration itself.

## 9. Benefiting Scenarios

### 9.1 Tuple-to-Userset Inheritance

Many resources can point to the same parent userset:

```text
doc:1#parent@folder:a#viewer
doc:2#parent@folder:a#viewer
doc:3#parent@folder:a#viewer
folder:a#viewer@group:eng#member
group:eng#member@user:alice
```

`lookup_resources(user:alice, can_view, doc)` can verify many document candidates that converge on
`folder:a#viewer@user:alice` or `group:eng#member@user:alice`.

### 9.2 Permission Enumeration

Schemas often define permissions from shared direct relations:

```text
can_view  = owner + editor + viewer - banned
can_edit  = owner + editor - banned
can_share = owner + editor
```

`lookup_permissions(subject, doc)` can avoid rechecking `doc#owner@subject`,
`doc#editor@subject`, and `doc#banned@subject` for every permission.

### 9.3 Lookup Subject Verification

`lookup_subjects` expands candidate subjects and then reuses `check` semantics for final
verification. Completed negative checks are useful when expansion includes usersets that ultimately
do not satisfy exclusion or intersection constraints.

### 9.4 Expression Convergence

Unions, intersections, and exclusions can converge on the same computed userset through different
branches. Memoization can help after a subcheck completes. It must not cache an expression-local
intermediate value because set algebra still owns short-circuit order and error propagation.

## 10. Non-Benefiting and Deferred Scenarios

- A single direct check that hits `any_resource_relation_subject` has little repeated work. Memo
  lookup overhead can dominate; keep memo disabled there until profile evidence says otherwise.
- `expand` trees are not cached in this spec. `lookup_subjects` already deduplicates seen usersets,
  and expand result caching has different memory and ownership tradeoffs.
- Parser migration from `pest` to `winnow` is unrelated to this request-local read cache. Existing
  evidence in [25](./25-compiled-computed-userset-shortcut-design.md) shows schema parse/compile is
  not a steady-state read-path bottleneck.
- A service-wide cache is deferred. It requires revision/schema keying, caveat-context keying,
  eviction policy, metrics, and concurrent synchronization.
- Singleflight request coalescing is deferred. The current engine is local and in-memory; first
  prove request-local hit rates before adding cross-request coordination.

## 11. Implementation Plan

### 11.1 Private Memo Types

Add private types to `src/eval.rs`:

- `CheckMemoKey`
- `CheckMemoEntry`
- `RequestCheckMemo`
- `MemoizationMode` or equivalent constructor flag

`RequestCheckMemo` should use `HashMap<CheckMemoKey, CheckMemoEntry>`. It should be lazily enabled
for lookup and permission enumeration paths. No dependency addition is justified for the first pass.

### 11.2 Capacity Control

The cache must be bounded. Use a private cap derived from evaluation limits, for example:

```text
memo_cap = clamp(max_lookup_results * 4, 128, 16_384)
```

Use checked or saturating arithmetic. When the cap is reached, keep serving existing hits but stop
inserting new entries. Do not add LRU in the first implementation; eviction policy overhead is not
justified without evidence.

### 11.3 Frame Depth Accounting

Add frame-local depth tracking around check evaluation:

- capture `entry_remaining_depth` before `enter`;
- update the current frame's minimum remaining depth whenever `enter` succeeds;
- propagate a completed child frame's minimum remaining depth into its parent before popping it;
- compute `depth_required` only for successful completed results;
- do not insert on errors or active-cycle denials.

Keep the implementation small enough that `EvaluationContext` remains reviewable. If frame tracking
makes the existing methods hard to follow, extract a private helper that owns enter/cache/evaluate/
leave ordering.

### 11.4 Wiring

Enable memoization in this order:

1. `lookup_resources_with_snapshot`: keep one memoized `check_context` across candidate resets.
2. `lookup_subjects_with_snapshot`: keep one memoized `check_context` across final candidate
   verification resets.
3. `lookup_permissions`: replace repeated `check_prepared_with_snapshot` calls with one memoized
   `EvaluationContext` and `reset_for_reuse()` between relation candidates.
4. `lookup_object_permissions`: add an internal helper that lets repeated `lookup_subjects`
   evaluations share a request memo across permission groups, or defer this substep if the helper
   becomes too invasive.

Leave public `check`, `check_relation`, and `check_prepared_with_snapshot` memo-disabled by default
for the first implementation. Add a benchmark before enabling memoization there.

### 11.5 Bench-Only Counters

Behind `bench-internals`, add counters for:

- check memo hits;
- misses;
- inserts;
- capacity skips;
- active-cycle skips;
- depth-insufficient skips.

The counters should be resettable from benchmarks like the existing store-view read counters.

## 12. Correctness Tests

Required tests:

- `test_should_not_share_request_memo_across_latest_revisions`: write, read, write again, and prove
  the next public read observes the new snapshot rather than a previous memo entry.
- `test_should_keep_exact_revision_memo_snapshot_local`: an exact token read and a latest read
  after a write return different valid answers.
- `test_should_not_cache_active_cycle_denial`: a cycle branch returns denied without poisoning a
  completed result that can be allowed through another branch.
- `test_should_require_sufficient_depth_for_memo_hit`: a cached result with high
  `depth_required` is not used when the current branch has less remaining depth.
- `test_should_not_cache_fanout_errors`: after a fanout-limited error, a later request with higher
  limits still computes normally.
- `test_should_cache_negative_completed_membership_only_within_request`: repeated candidate
  verification can hit a denied result, but a separate public request recomputes.
- Existing Phase 14 tests for reusable context leakage, cycle denial, plain computed-user shortcuts,
  delta tombstone masking, and segmented delta reference behavior must keep passing.

## 13. Benchmarks and Gates

Compare against the Phase 14 follow-up evidence in [71](./71-performance-budgets-design.md):

| Benchmark | Baseline upper | Gate |
| --- | ---: | --- |
| `perf_optimization/check_prepared_1m` | `4.3065 us` | no > 3% regression while memo is disabled for direct checks |
| `perf_optimization/lookup_resources_streaming_1m` | `2.3551 ms` | no regression; >= 5% improvement expected only if hit rate is meaningful |
| `perf_optimization/lookup_subjects_streaming_1m` | `4.7426 us` | no > 5% regression |
| `realworld_authorization/1m_rules/check_doc_inherited_workspace_member` | `11.559 us` | no > 5% regression |
| `realworld_authorization/1m_rules/mixed_read_workload` | `41.808 us` | no > 5% regression; improvement expected from lookup substeps |
| `realworld_authorization/1m_rules/lookup_permissions_shared_doc` | current local rerun required | no regression; improvement expected |
| `realworld_authorization/1m_rules/lookup_object_permissions_shared_doc` | current local rerun required | no regression; improvement expected if shared helper lands |

Add one memo-specific synthetic benchmark before implementation is declared successful:

```text
perf_optimization/request_memo_lookup_resources_shared_parent_1m
```

Fixture shape: many documents point to a smaller set of folders or groups, and candidate
verification repeatedly checks the same parent/group membership. Gate: check memo hit rate >= 40%
and upper estimate improves by >= 10% versus the same fixture with memoization disabled.

Optional second benchmark:

```text
perf_optimization/request_memo_lookup_permissions_shared_edges_1m
```

Fixture shape: one resource has multiple permissions that share plain computed relations and an
exclusion relation. Gate: hit rate >= 30%, no correctness drift, and no direct-check regression.

## 14. Exit Criteria

Implementation is complete only when all of these hold:

- The cache is request-local and owned by `EvaluationContext`; no global or engine-level cache is
  introduced.
- Cache hits are guarded by active-cycle status and `depth_required`.
- Errors and active-cycle denials are not cached.
- Cache memory is capped and cap overflow degrades by skipping inserts rather than changing results.
- `lookup_resources`, `lookup_subjects`, and `lookup_permissions` use memoized contexts where
  practical.
- Required correctness tests pass.
- Required benchmarks are recorded with Phase 14 comparison tables.
- Existing strict gates pass: `cargo build`, `cargo test`, `cargo +nightly fmt`, strict clippy,
  `cargo audit`, and `cargo deny check`.

If the memo-specific benchmark cannot show at least a 10 percent win on a repeated-subcheck fixture,
do not enable this optimization broadly. Keep the spec as deferred evidence and move priority to
compiled evaluation plans or lookup planning.

## 15. Risks and Counterexamples

| Risk | Counterexample | Mitigation |
| --- | --- | --- |
| Direct-check regression | `check_prepared_1m` pays a hash lookup with no repeated work. | Keep memo disabled for single checks initially and gate regression at 3%. |
| Cycle unsoundness | A cached `Allowed` result proves a key that is currently active on the stack. | Active-cycle check must run before memo lookup. |
| Hidden depth errors | A result computed with depth 50 is reused from a branch with depth 1. | Store `depth_required` and hit only when current depth is sufficient. |
| Unbounded memory | Wide lookup sees thousands of unique denied candidates. | Cap entries and skip inserts after the cap. |
| Stale latest read | A write changes the relation after an earlier latest read. | Cache is dropped at public request completion and never stored in engine state. |
| Incorrect negative caching | A denied branch from an exclusion poisons a later allowed branch. | Cache only completed check results, never expression-local branch values. |
| Future caveats | Conditional membership depends on request context not in the key. | Treat caveat support as a key extension requirement before caching conditional results. |
| Overfitting synthetic benchmark | Shared-parent fixture improves but realworld mixed read is flat. | Require both memo-specific hit-rate evidence and no regression on realworld benches. |

## 16. AGENTS.md Binding

- Error Handling: use existing `ZanzibarError`/`EvaluationError`; do not cache errors or erase
  error types.
- Async & Concurrency: the memo is request-local, not shared, and needs no mutex.
- Type Design: use private typed keys and `NonZeroU32` for depth requirements.
- Safety & Security: no `unsafe`, no unchecked indexing, no panic path from user-controlled schema
  or relationship data.
- Resource Limits: bound memo entries and use checked or saturating arithmetic for capacity math.
- Serialization: no snapshot or public API format change.
- Testing: add adversarial cycle, depth, fanout, revision, and negative-cache tests.
- Performance: land only with benchmark evidence and keep direct-check gates tight.
- Documentation: no public docs are required unless memoization becomes configurable.

## 17. Recommendation

Recommended priority: P1.5 for the next read-performance phase. It is less invasive than bitmap
indexes or a reverse lookup planner and directly builds on the reusable context already present in
Phase 14. It should be abandoned or kept internal-only if the memo-specific fixture cannot produce
a clear hit rate and at least a 10 percent win.

Expected benefit: 5-15 percent on repeated-subcheck lookup and permission enumeration workloads,
with a possible larger win on high-convergence tuple-to-userset graphs. Direct check benefit is not
expected and should not be claimed.

Main risk: preserving depth and cycle semantics. The optimization is only elegant if memo lookup is
a narrow wrapper around completed check frames; pushing caching into expression evaluation would
make the correctness story weaker and should be avoided.

## 18. Cross-References

- <- Depends on: [14-evaluation-engine-design.md](./14-evaluation-engine-design.md), [16-compact-relationship-store-design.md](./16-compact-relationship-store-design.md), [23-read-performance-optimization-design.md](./23-read-performance-optimization-design.md), [25-compiled-computed-userset-shortcut-design.md](./25-compiled-computed-userset-shortcut-design.md), [71-performance-budgets-design.md](./71-performance-budgets-design.md)
