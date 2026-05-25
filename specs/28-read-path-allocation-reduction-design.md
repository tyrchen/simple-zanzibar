# 28 - Read Path Allocation Reduction Design

Status: draft v1
Owner: Simple Zanzibar
Last updated: 2026-05-25
Depends on: [14](./14-evaluation-engine-design.md), [16](./16-compact-relationship-store-design.md), [21](./21-performance-optimization-design.md), [23](./23-read-performance-optimization-design.md), [25](./25-compiled-computed-userset-shortcut-design.md), [60](./60-crates-features-design.md), [71](./71-performance-budgets-design.md)

## 1. Purpose

Phase 14 brought the read path under the latency gates: mixed read is now
`[40.921 us, 41.317 us, 41.808 us]`, inherited check is
`[11.211 us, 11.341 us, 11.559 us]`, and `check_prepared_1m` is
`[4.2240 us, 4.2590 us, 4.3065 us]` after the compiled computed-userset
follow-up. The next allocation-reduction pass should therefore avoid speculative
container churn in code that is already fast.

This spec scopes one evidence-driven pass over expression traversal and lookup
collection. The goal is to reduce heap allocation in paths that still build
public-model temporary structures, especially `lookup_subjects` and
`lookup_resources`, while preserving the current public API and the read
latency wins from [23](./23-read-performance-optimization-design.md) and
[25](./25-compiled-computed-userset-shortcut-design.md).

## 2. Current Allocation Map

The current evaluator already avoids the most obvious per-expression allocation
for check evaluation: union and intersection checks iterate over compiled
expression slices and short-circuit without building child result vectors
(`src/eval.rs:603`, `src/eval.rs:625`). The remaining likely allocation points
are concentrated in recursion keys, public expand trees, and lookup collectors.

| Area | Code evidence | Allocation shape | Priority |
| --- | --- | --- | --- |
| Recursion keys | `src/eval.rs:56`, `src/eval.rs:82`, `src/eval.rs:122`, `src/eval.rs:275`, `src/eval.rs:322`, `src/eval.rs:374` | `CheckKey` / `ExpandKey` clone public `Object`, `RelationName`, and `User` when a store-native key is not available; `key.clone()` is also used for depth errors and active-index insertion. | Medium; mostly solved for compact store-native checks, but delta/fallback paths can still allocate. |
| Active recursion state | `src/eval.rs:216`, `src/eval.rs:218`, `src/eval.rs:816`, `src/eval.rs:825`, `src/eval.rs:855` | Stack `Vec`s retain capacity, while `HashMap` indexes allocate only after depth threshold. | Low; current generation-counter design is already acceptable unless allocation counters prove deep recursion churn. |
| Tuple-to-userset relation lookup | `src/eval.rs:492`, `src/eval.rs:706` | `RelationName::clone()` when resolving compiled tupleset ids back to names. | Low-medium; avoidable by keeping borrowed relation names through the loop, but not expected to dominate. |
| Public `expand` result tree | `src/eval.rs:369`, `src/eval.rs:687`, `src/eval.rs:703`, `src/eval.rs:731`, `src/eval.rs:742`, `src/eval.rs:753`, `src/eval.rs:765` | `ExpandedUserset::Union(Vec<_>)`, `Intersection(Vec<_>)`, and `Exclusion { Box<_> }` allocate an owned public response tree. | High only when the caller is `lookup_subjects`; unavoidable for the public `expand` API response. |
| `lookup_resources` frontier and output | `src/eval.rs:981`, `src/eval.rs:982`, `src/eval.rs:983`, `src/eval.rs:984`, `src/eval.rs:993`, `src/eval.rs:994`, `src/eval.rs:1000`, `src/eval.rs:1006` | Request-local `VecDeque`, two `HashSet`s, public `Object` materialization, and userset-subject clones while traversing reverse edges. | Medium-high; candidate IDs can stay internal until final output. |
| `lookup_subjects` expand-then-collect | `src/eval.rs:1039`, `src/eval.rs:1041`, `src/eval.rs:1042`, `src/eval.rs:1043`, `src/eval.rs:1085`, `src/eval.rs:1124`, `src/eval.rs:1139` | Builds a full `ExpandedUserset` tree, then walks it into `HashSet`s and output `Vec`, cloning public users and usersets. Nested usersets trigger more public expand trees. | Highest; streaming collection can remove a whole transient tree for lookup. |
| Schema expression containers | `src/schema/mod.rs:286`, `src/schema/mod.rs:288`, `src/schema/mod.rs:340`, `src/schema/mod.rs:342`, `src/schema/mod.rs:630`, `src/schema/mod.rs:701` | `Vec` allocations during schema parse/compile and one heap allocation per union/intersection children vector in compiled IR. | Low for steady-state reads; consider only with schema-compile or cache-locality evidence. |
| Benchmark request cloning | `benches/perf_optimization.rs:81`, `benches/perf_optimization.rs:95`, `benches/realworld_authorization.rs:260`, `benches/realworld_authorization.rs:274`, `benches/realworld_authorization.rs:326`, `benches/realworld_authorization.rs:397` | Bench harness clones request objects and mixed-read inputs per iteration. | Do not optimize engine based solely on this; separate harness overhead from engine allocation counts. |

## 3. Target Data Flow

```text
+-----------------------------------------------------------------------------------+
| Public read request                                                               |
|                                                                                   |
|  check / check_prepared                                                           |
|    |                                                                              |
|    v                                                                              |
|  +----------------------+     +----------------------+     +-------------------+  |
|  | EvaluationContext    |---->| Compiled schema IR   |---->| StoreView iterators|  |
|  | - reusable stacks    |     | - borrowed slices    |     | - compact refs     |  |
|  | - active index maps  |     | - relation ids       |     | - no result Vec    |  |
|  +----------------------+     +----------------------+     +-------------------+  |
|                                                                                   |
|  lookup_resources                                                                 |
|    |                                                                              |
|    v                                                                              |
|  +----------------------+     +----------------------+     +-------------------+  |
|  | Request scratch      |---->| Reverse edge stream  |---->| Final Vec<Object> |  |
|  | - frontier           |     | - internal objects   |     | - allocate only at |  |
|  | - visited / seen     |     | - verify with check  |     |   public boundary  |  |
|  +----------------------+     +----------------------+     +-------------------+  |
|                                                                                   |
|  lookup_subjects                                                                  |
|    |                                                                              |
|    v                                                                              |
|  +----------------------+     +----------------------+     +-------------------+  |
|  | Streaming collector  |<----| Expand traversal     |---->| Final Vec<User>   |  |
|  | - seen subjects      |     | - callback events    |     | - no transient    |  |
|  | - seen usersets      |     | - depth/fanout same  |     |   ExpandedUserset |  |
|  | - reusable contexts  |     | - public expand kept |     +-------------------+  |
|  +----------------------+     |   unchanged          |                           |
|                               +----------------------+                           |
+-----------------------------------------------------------------------------------+
```

The key distinction is boundary ownership. Public `expand` must return an owned
`ExpandedUserset` tree. `lookup_subjects` only needs to discover and verify
subjects, so it should not allocate that public tree as an intermediate format.

## 4. Design Options

### 4.1 SmallVec or ArrayVec in Compiled Expression Trees

`CompiledUsersetExpression::Union(Vec<_>)` and `Intersection(Vec<_>)` are natural
small-vector candidates because real schemas usually have two to four operands.
However, the allocation occurs during schema compilation or snapshot load, not
per steady-state check. Runtime check evaluation already borrows the slice.

Current dependency review:

- `smallvec` latest observed through `cargo info` on 2026-05-25 is
  `2.0.0-alpha.12`, MIT OR Apache-2.0, Rust 1.83.
- [60](./60-crates-features-design.md) already prefers `arrayvec` over
  `smallvec` initially because the latest observed `smallvec` release is alpha.
- `arrayvec` latest observed is `0.7.6`, MIT OR Apache-2.0, but it is fixed
  capacity and cannot represent arbitrary user schema operand counts unless the
  schema language adds a new maximum.

Decision: do not add `smallvec` in the first allocation-reduction pass. If
allocation profiles show schema compile or expand-tree child vectors dominate,
prefer one of:

- use `smallvec` only after a dependency review accepts the alpha latest release
  or a stable `1.x` line is deliberately pinned;
- use a local enum such as `OneOrMany<T>` for compiled IR only, preserving
  arbitrary fanout without a dependency;
- keep `Vec` and focus on lookup streaming if steady-state read profiles do not
  show expression-container allocation.

### 4.2 Request-Local Scratch Collections

Request-local scratch is the lowest-risk allocation reduction because it keeps
ownership and lifetimes simple:

```rust
struct LookupScratch {
    frontier: VecDeque<User>,
    visited_subjects: HashSet<User>,
    seen_resources: HashSet<Object>,
    seen_subjects: HashSet<User>,
    seen_usersets: HashSet<(Object, Relation)>,
}
```

This shape does not require a new dependency and can be phased in behind helper
methods. The first implementation should pre-size scratch collections from the
configured `max_lookup_results` with conservative caps. It should avoid large
preallocations from untrusted configuration by bounding initial capacity, for
example `min(max_lookup_results, 1024)`.

Scratch collections are request-local and must not be stored in `PublishedSnapshot`
or shared between threads. Reusing them across public API calls risks stale data
and is out of scope.

### 4.3 Streaming Collector for `lookup_subjects`

This is the recommended first real implementation. Today `lookup_subjects`
constructs `expanded = expand(...)` and then traverses it. A streaming collector
should introduce an internal traversal mode:

```rust
trait ExpandedUsersetVisitor {
    fn visit_user(&mut self, id: &str) -> Result<(), ZanzibarError>;
    fn visit_userset(&mut self, object: &Object, relation: &Relation) -> Result<(), ZanzibarError>;
    fn enter_union(&mut self) -> Result<(), ZanzibarError>;
    fn leave_union(&mut self);
}
```

The exact trait shape is not binding; the contract is. `lookup_subjects` should
walk the same schema expression semantics as `expand`, but emit candidates into
`LookupSubjectCollector` without allocating an `ExpandedUserset` tree. Public
`expand` keeps the existing owned tree builder.

Correctness requirements:

- reuse the same depth and fanout accounting as `expand`;
- preserve cycle denial behavior;
- preserve lookup result limit enforcement after every collected subject;
- keep final check verification for usersets and subjects unless an existing
  exact-proof shortcut applies;
- do not change the public `ExpandedUserset` shape.

### 4.4 Arena Allocation

A bump arena is not recommended for the first pass. It would help only for
transient internal trees, but the better design is to avoid building those trees
for lookup. Public `expand` cannot return arena-borrowed data, so an arena-backed
public tree would still need a copy into owned response values. Adding `bumpalo`
or a similar crate would also add a dependency for an avoidable lifetime problem.

The only arena-like idea worth keeping is a simple request-local scratch owner:
ordinary `Vec`, `VecDeque`, and `HashSet` fields whose capacity is retained for
the duration of one public lookup request.

### 4.5 No Dependency First

The default implementation path is no new dependency:

- keep compiled expression containers as `Vec` until profiles show otherwise;
- add allocation counters before changing containers;
- stream `lookup_subjects` traversal into the collector;
- retain scratch capacities inside one lookup call;
- pre-size final output vectors with a capped capacity derived from
  `max_lookup_results`.

This path aligns with AGENTS.md dependency discipline and keeps rollback simple.

## 5. Paths Not Worth Optimizing First

- Parser migration for this goal. [25](./25-compiled-computed-userset-shortcut-design.md) records
  schema parse/compile at about `51 us` inside a roughly `572 ms` full load and
  outside steady-state reads.
- Check union/intersection child storage. Check evaluation already uses borrowed
  slices and short-circuits without child vectors.
- Public `expand` output allocation. It is the API result. Optimize only the
  internal lookup traversal that currently uses public expand as an intermediate.
- Benchmark request cloning. Keep it visible in allocation reports, but do not
  change engine internals to compensate for harness-only cloning.
- Snapshot row/index load allocation. That belongs to snapshot-load specs, not
  this read-path lookup/expression pass.

## 6. Allocation Measurement Plan

The implementation phase must establish allocation baselines before changing
containers. CPU-only Criterion improvements are not sufficient evidence for this
spec.

Recommended measurement order:

1. Add a benchmark-only allocation counter using a custom global allocator in a
   dedicated benchmark binary or `bench-internals` build. The counter should
   report allocations/op, deallocations/op, allocated bytes/op, and peak live
   bytes/op for selected filters. Use `iter_custom` or equivalent manual loops
   so counters can be reset around exactly the engine call.
2. Cross-check one representative run with `dhat` (`0.3.3`, MIT OR Apache-2.0)
   or macOS Instruments Allocations. `dhat` is acceptable as a dev-only profiling
   dependency if `cargo audit` and `cargo deny check` pass.
3. Use `samply` or Instruments Time Profiler only to confirm that allocation
   reduction does not move CPU time into hashing, branching, or extra traversal.
4. Report allocation counts alongside existing Criterion latency tables in the
   PR comment.

Required filters:

| Filter | What it proves |
| --- | --- |
| `perf_optimization/check_prepared_1m` | Recursion-key changes do not add allocations to the fast prepared-check path. |
| `perf_optimization/lookup_resources_streaming_1m` | Frontier/seen/output scratch changes reduce or at least do not increase allocations. |
| `perf_optimization/lookup_subjects_streaming_1m` | Streaming collector removes transient expand-tree allocation. |
| `realworld_authorization/1m_rules/lookup_subjects_shared_doc` | Realistic subject lookup benefits from the streaming collector. |
| `realworld_authorization/1m_rules/expand_shared_doc` | Public expand allocation and latency do not regress. |
| `realworld_authorization/1m_rules/mixed_read_workload` | End-to-end mixed read remains under current gate. |

Allocation reports must distinguish benchmark harness allocation from engine
allocation. If the current public API forces request cloning in benchmarks,
measure an internal snapshot function where possible to isolate the engine.

## 7. Benchmark Gates

Compare against the latest Phase 14 follow-up where available, and capture fresh
baselines for filters not recorded in [71](./71-performance-budgets-design.md)
§3.14.

| Benchmark | Latency gate | Allocation gate |
| --- | --- | --- |
| `perf_optimization/check_prepared_1m` | upper <= `4.5218 us` (`4.3065 us * 1.05`) | no increase in allocations/op; expected near-zero engine allocations on store-native direct paths. |
| `realworld_authorization/1m_rules/check_doc_inherited_workspace_member` | upper <= `12.137 us` (`11.559 us * 1.05`) and still <= `13.5 us` stretch | no increase in allocations/op. |
| `realworld_authorization/1m_rules/mixed_read_workload` | upper <= `43.899 us` (`41.808 us * 1.05`) and still <= `55 us` hard gate | allocations/op reduced or profile explains why lookup is not allocation-bound. |
| `perf_optimization/lookup_resources_streaming_1m` | upper <= `2.4729 ms` (`2.3551 ms * 1.05`) | >= 10% fewer engine allocations/op or no latency regression with documented allocation floor. |
| `perf_optimization/lookup_subjects_streaming_1m` | upper <= `4.9797 us` (`4.7426 us * 1.05`) | >= 25% fewer engine allocations/op after streaming collector, unless baseline is already <= 2 allocations/op. |
| `realworld_authorization/1m_rules/lookup_subjects_shared_doc` | no > 5% regression versus fresh pre-change baseline | >= 25% fewer engine allocations/op after streaming collector. |
| `realworld_authorization/1m_rules/expand_shared_doc` | no > 5% regression versus fresh pre-change baseline | no required reduction; public response allocation is allowed. |

If an allocation gate fails but latency improves, keep the change only with a
profile showing allocator time was not material and the added complexity is
small. If latency regresses by more than 5%, rollback unless the user explicitly
accepts a recalibrated budget.

## 8. Implementation Plan

### P0 - Allocation Baseline

- Add benchmark-only allocation measurement for the required filters.
- Record baseline allocation counts and Criterion numbers in the PR discussion.
- Do not change runtime containers in this phase.

Exit: the team can name the top two allocation sources for
`lookup_subjects_streaming_1m` and `mixed_read_workload`.

### P1 - Stream `lookup_subjects` Collection

- Split public expand tree building from internal expansion traversal.
- Add an internal collector path that visits users and usersets directly.
- Preserve final check verification and result limits.
- Keep public `expand_with_snapshot` unchanged.

Exit: `lookup_subjects_streaming_1m` allocation gate passes with no latency
regression; public expand tests and lookup equivalence tests pass.

### P2 - Request-Local Lookup Scratch

- Introduce a small internal scratch owner for lookup collections.
- Pre-size with conservative caps from `max_lookup_results`.
- Reuse existing `EvaluationContext::reset_for_reuse` for candidate checks.
- Avoid cross-request reuse.

Exit: `lookup_resources_streaming_1m` allocation gate passes or profile proves
remaining allocations are final public output only.

### P3 - Optional Expression Container Experiment

- Run a local experiment with `SmallVec`, `arrayvec`, or a local `OneOrMany<T>`
  for compiled expression operands.
- Keep the experiment out of the main branch unless allocation/cpu evidence shows
  a measurable steady-state read benefit.
- If a dependency is proposed, update dependency docs and run `cargo audit` and
  `cargo deny check`.

Exit: either defer container changes with evidence, or land the smallest proven
container change.

## 9. Testing and Correctness

Required tests:

- `lookup_subjects` equivalence against the current expand-then-collect behavior
  for `This`, computed userset, tuple-to-userset, union, intersection, and
  exclusion.
- Result limit tests proving streaming traversal stops after the configured
  maximum.
- Cycle tests for recursive usersets in both public `expand` and streaming
  lookup traversal.
- Fanout-limit tests for streaming traversal.
- Exact-revision tests after writes proving lookup scratch does not leak state
  across candidates.
- Regression tests for public `ExpandedUserset` shape and ordering where existing
  behavior is observable.

The implementation must preserve all AGENTS.md safety rules: no `unsafe`, no
unchecked indexing on user-controlled data, no panic path from schema or
relationship input, and typed errors for evaluator limits.

## 10. Rollback Standards

Rollback the allocation-reduction change if any of these occur:

- a required latency benchmark regresses by more than 5% without an accepted
  budget recalibration;
- allocation counts do not improve on the targeted lookup path and the change
  adds non-trivial complexity;
- public `expand` output or lookup result semantics change;
- stack usage grows materially from inline storage experiments;
- a new dependency fails `cargo audit`, `cargo deny check`, or project dependency
  policy;
- the implementation requires arena lifetimes that make error paths or public
  ownership harder to review.

Rollback should prefer removing the narrow change that caused the regression,
not reverting unrelated concurrent work from other branches or agents.

## 11. Recommendation

Recommended priority:

1. Baseline allocation counters.
2. Streaming `lookup_subjects` collector.
3. Request-local lookup scratch and conservative pre-sizing.
4. Optional expression container experiment only if profiles still point there.

Expected benefit:

- `lookup_subjects` should see the largest allocation-count reduction because it
  can avoid an entire transient `ExpandedUserset` tree.
- `mixed_read_workload` may see a smaller but measurable improvement because it
  includes `lookup_subjects` and `lookup_resources`.
- `check_prepared_1m` is expected to remain mostly unchanged; that is acceptable.

Main risks:

- streaming traversal can accidentally diverge from public `expand` semantics;
- allocation counters can be polluted by benchmark request cloning;
- SmallVec-style changes may improve allocation counts at schema compile time but
  not steady-state read latency;
- inline small containers can increase stack size and hurt instruction cache if
  applied broadly.

## 12. Cross-References

- <- Depends on: [14-evaluation-engine-design.md](./14-evaluation-engine-design.md), [16-compact-relationship-store-design.md](./16-compact-relationship-store-design.md), [21-performance-optimization-design.md](./21-performance-optimization-design.md), [23-read-performance-optimization-design.md](./23-read-performance-optimization-design.md), [25-compiled-computed-userset-shortcut-design.md](./25-compiled-computed-userset-shortcut-design.md), [60-crates-features-design.md](./60-crates-features-design.md), [71-performance-budgets-design.md](./71-performance-budgets-design.md)
- -> Intended consumer: the next read-path implementation plan assembled from the six optimization-direction specs.
