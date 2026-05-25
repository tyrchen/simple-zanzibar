# 31 - Schema-Aware Lookup Planner Design

Status: draft v1
Owner: Simple Zanzibar
Last updated: 2026-05-25
Depends on: [14](./14-evaluation-engine-design.md), [16](./16-compact-relationship-store-design.md), [23](./23-read-performance-optimization-design.md), [71](./71-performance-budgets-design.md)

## 1. Problem

Phase 14 made `check` and the direct lookup primitives faster, but the lookup APIs still use broad
candidate enumeration followed by repeated evaluator calls:

- `lookup_resources` starts from the requested subject, walks subject-side reverse relationships as
  a generic userset frontier, treats every reached object of the requested resource type as a
  candidate, and calls full `check(resource, permission, original_subject)` before returning it.
- `lookup_subjects` first builds an `ExpandedUserset` tree for the resource permission, recursively
  expands userset subjects, and calls full `check(resource, permission, candidate_subject)` before
  returning each candidate.

This is sound because `check` remains the final authority. It is also intentionally conservative:
candidate generation is not aware that `doc#can_view` may be an exclusion over only
`doc#viewer`, `doc#editor`, `doc#owner`, and `doc#parent -> folder#can_view`. The current
implementation can therefore explore relations that cannot add an allowed result, and it can repeat
the same positive proof during the final `check`.

This spec designs a schema-aware reverse lookup planner for `lookup_resources` and
`lookup_subjects`. The goal is to produce fewer candidates and, when the candidate already carries a
valid proof for part of the schema expression, verify only the residual guards instead of rerunning
the whole root expression.

## 2. Current Baseline

The relevant implementation points on the Phase 14 branch are:

- `src/eval.rs`:
  - `lookup_resources_with_snapshot` uses a `VecDeque<User>` subject frontier, calls
    `reverse_query_compact_relationships`, deduplicates resources, and verifies each resource with
    `EvaluationContext::check`.
  - `lookup_subjects_with_snapshot` calls `EvaluationContext::expand`, then
    `LookupSubjectCollector` recursively expands usersets and verifies each subject with
    `EvaluationContext::check`.
  - `EvaluationContext` now evaluates compiled schema expressions, uses compiled relation ids for
    same-namespace recursion, and supports reusable generation-counter state.
- `src/schema/mod.rs`:
  - `CompiledUsersetExpression` covers `This`, `ComputedUserset`, `TupleToUserset`, `Union`,
    `Intersection`, and `Exclusion`.
  - `TupleToUserset` keeps the computed userset relation as a name because the intermediate object
    namespace determines the target relation at runtime.
  - Explicit `AllowedSubjectTypes` can bound tuple-to-userset intermediate namespaces, but the
    current DSL mostly produces `Unspecified`.
- `src/relationship.rs`:
  - resource-side indexes support object+relation, object, type+relation, and type queries.
  - subject-side indexes support subject exact, subject type+relation, and subject type queries, but
    only `IndexProfile::Full` persists reverse lookup indexes.
  - there is no composite subject-plus-resource-type-plus-relation index yet.

Current performance evidence from [23](./23-read-performance-optimization-design.md) and
[71](./71-performance-budgets-design.md):

| Benchmark | Phase 14 follow-up upper estimate | Notes |
| --- | ---: | --- |
| `perf_optimization/lookup_resources_streaming_1m` | `2.3551 ms` | result-limited but still millisecond-scale |
| `perf_optimization/lookup_subjects_streaming_1m` | `4.7426 us` | already small on the synthetic fixture |
| `realworld_authorization/1m_rules/lookup_resources_target_user` | prior bounded baseline `6.3217 us` | realworld fixture is result-limited and favorable |
| `realworld_authorization/1m_rules/lookup_subjects_shared_doc` | prior bounded baseline `11.657 us` | uses group/userset recursion |

The largest exposed target is `lookup_resources_streaming_1m`, where repeated candidate
verification dominates more than direct relationship lookup.

## 3. Goals

| # | Goal | Measure |
| --- | --- | --- |
| G1 | Generate lookup candidates from schema-positive producer relations instead of every reached relation. | Candidate count and full-root check count fall in benchmark instrumentation. |
| G2 | Preserve exact `check` semantics for every returned result. | Existing lookup integration tests and new adversarial algebra tests pass. |
| G3 | Reduce candidate re-check cost using residual verification. | `lookup_resources_streaming_1m` improves by at least 25% upper estimate, or profile evidence explains the limit. |
| G4 | Keep low-risk paths fast. | `check_prepared_1m`, inherited check, and `lookup_subjects_streaming_1m` regress by no more than 5%. |
| G5 | Keep fallback behavior explicit. | Unsupported or unsafe plan shapes use the current lookup algorithm or full-root `check`, never an unsound shortcut. |

## 4. Non-Goals

- No public API change for `check`, `expand`, `lookup_resources`, or `lookup_subjects`.
- No cursor pagination in this phase.
- No new vendor checkout or dependency addition.
- No parser migration. The `pest -> winnow` question is not on this hot path.
- No distributed dispatcher, batching service, or SpiceDB query engine port.
- No snapshot format change in the first implementation slice. Composite index additions are
  specified as an optional later phase.

## 5. Planner Model

The planner is built from compiled schema expressions. It should be cached on the compiled schema or
built lazily per `(namespace, relation)` and reused by snapshot readers.

```rust
struct LookupPlan {
    root: SchemaRelationId,
    root_namespace: ObjectType,
    root_relation: RelationName,
    node: PlanNode,
    exactness: PlanExactness,
    cost: PlanCost,
}

enum PlanNode {
    This {
        namespace: ObjectType,
        relation: RelationName,
        relation_id: SchemaRelationId,
    },
    Computed {
        relation: RelationName,
        relation_id: SchemaRelationId,
        child: Box<PlanNode>,
    },
    TupleToUserset {
        tupleset_relation: RelationName,
        tupleset_relation_id: SchemaRelationId,
        computed_relation: RelationName,
        intermediate_types: IntermediateTypeSet,
        child_by_type: Vec<(ObjectType, PlanNode)>,
    },
    Union(Vec<PlanNode>),
    Intersection {
        seed: Box<PlanNode>,
        guards: Vec<PlanNode>,
    },
    Exclusion {
        base: Box<PlanNode>,
        exclude_guard: Box<PlanNode>,
    },
    Fallback {
        reason: FallbackReason,
    },
}

enum PlanExactness {
    Exact,
    SupersetRequiresResidual,
    CurrentAlgorithm,
}
```

The concrete implementation can use the current `CompiledUsersetExpression` directly at first. The
important contract is not the exact enum layout; it is that each candidate carries a proof state:

```rust
struct LookupCandidate<T> {
    value: T,
    proof: CandidateProof,
}

enum CandidateProof {
    ProvenRoot,
    ProvenNode { residual: ResidualCheck },
    Superset { verify: VerifyPolicy },
}

enum ResidualCheck {
    None,
    All(Vec<PlanNode>),
    ExcludeDenied(Box<PlanNode>),
    FullRoot,
}
```

`ProvenRoot` can be returned after deduplication. `ProvenNode` skips the already-proven positive
part and evaluates only the residual guard. `Superset` must use full-root `check` or fall back to
the current implementation.

## 6. Planner Flow

```text
Public lookup request
  |
  v
+-------------------------+
| Validate public request |
| - object/resource type  |
| - permission relation   |
| - subject type          |
+------------+------------+
             |
             v
+-------------------------+       no plan / unsafe shape
| LookupPlan cache        |------------------------------+
| key: namespace#relation |                              |
+------------+------------+                              |
             |                                           |
             v                                           v
+-------------------------+                +-----------------------------+
| Compile plan from schema|                | Current lookup implementation|
| expression              |                | + full check verification    |
+------------+------------+                +--------------+--------------+
             |                                            |
             v                                            |
+-------------------------+                               |
| Normalize algebra       |                               |
| - union producers       |                               |
| - intersection seed     |                               |
| - exclusion base only   |                               |
| - tuple intermediate    |                               |
|   type set              |                               |
+------------+------------+                               |
             |                                            |
             v                                            |
+-------------------------+                               |
| Execute reverse plan    |                               |
| resources or subjects   |                               |
| from snapshot indexes   |                               |
+------------+------------+                               |
             |                                            |
             v                                            |
+-------------------------+                               |
| Candidate proof         |                               |
| - ProvenRoot            |                               |
| - ProvenNode+Residual   |                               |
| - Superset+FullRoot     |                               |
+------------+------------+                               |
             |                                            |
             v                                            |
+-------------------------+                               |
| Dedup + limit + verify  |<------------------------------+
| on same snapshot        |
+------------+------------+
             |
             v
Lookup result
```

## 7. Expression Strategy

### 7.1 `This`

For `lookup_subjects(resource, relation, subject_type)`, `This` is exact:

1. Query `resource_relation(resource, relation, fanout_limit)`.
2. Yield direct `user` subjects matching `subject_type == "user"`.
3. Yield userset subjects matching the requested userset object type.
4. For userset subjects that need expansion to `user`, recursively execute the plan for the
   userset object's relation and carry `ProvenNode`.

For `lookup_resources(subject, relation, resource_type)`, `This` uses the current subject frontier
as a reachability primitive, but filters candidate resources by the planned positive
`resource_type#relation` pair before verification. This first slice avoids adding a composite
subject/resource index. A later slice can add
`reverse_resource_relation(subject_filter, resource_type, relation)` and pick the shorter posting
list between subject and resource-type-relation indexes.

Exactness:

- Direct exact subject matches are `ProvenNode`.
- Userset-subject matches are `ProvenNode` only when the nested userset membership proof is exact.
- Otherwise the candidate is `Superset` and uses residual or full-root verification.

### 7.2 `ComputedUserset`

Computed usersets are aliases to another relation on the same object. The planner resolves the
compiled `relation_id` and reuses that relation's plan.

If the target relation has no rewrite, the plan lowers directly to `This(namespace, target)`. This
matches the Phase 14 `target_has_rewrite == false` fast path and lets lookup candidates carry a
direct proof instead of rechecking the computed relation.

Cycle policy:

- A relation cycle already handled by evaluator recursion must not be unrolled indefinitely.
- The planner stores a `PlanningStack` of relation ids. A repeated relation becomes
  `Fallback { reason: RecursiveRelation }` for that branch.
- The executor then uses the current lookup algorithm or full-root `check`, preserving existing
  depth and cycle behavior.

### 7.3 `Union`

Union is the best case for lookup planning.

- Candidate producers are the union of child producers.
- A candidate proven by any exact child is proven for the union.
- Duplicates are removed by the existing result dedupe key.
- Children that are conservative can still produce candidates, but those candidates carry their
  residual/full-root verification policy.

No child should force another child to run for the same candidate. This preserves the normal
short-circuit behavior of `check`.

### 7.4 `Intersection`

Intersection is exact only if every child proves the same candidate. Materializing every child set
can be expensive, so the first implementation should use a seed-and-guard strategy:

1. Pick the cheapest child as the seed. Cost is estimated from static shape first:
   `This` < plain computed userset < tuple-to-userset < union < nested intersection/exclusion.
2. Generate candidates only from the seed.
3. For each candidate, evaluate the remaining children as residual guards on the same snapshot.
4. Return the candidate only if all guards allow.

This is sound because intersection cannot add candidates that are absent from any child. The seed
choice affects completeness only if the seed plan is not exact. If the seed is conservative, the
candidate uses full-root verification. If no exact seed exists, the branch falls back to the current
lookup algorithm.

Later optimization: if two or more exact child plans have cheap bounded sets, execute them as sorted
or hash-set intersections and emit `ProvenRoot` candidates. That should be gated by measured
candidate counts, not added speculatively.

### 7.5 `Exclusion`

Exclusion is where the current `lookup_subjects` behavior does the most unnecessary work. The
exclude side can never add an allowed candidate.

Plan rule:

- Generate candidates from `base` only.
- Attach `ResidualCheck::ExcludeDenied(exclude)` to every candidate unless the exclude plan can be
  evaluated as a cheap exact set subtraction.
- Do not traverse the exclude side for candidate generation.

This preserves semantics:

- If the base denies, the candidate is never produced.
- If the base allows and the exclude allows, residual verification drops the candidate.
- If the exclude denies, the candidate is returned.

For `lookup_resources`, this avoids treating relations such as `banned` as resource candidates for
`can_view`. For `lookup_subjects`, it avoids expanding subjects that appear only in the deny side.

### 7.6 `TupleToUserset`

Tuple-to-userset is the highest-risk expression because the computed relation is resolved against
the runtime intermediate object namespace.

For `lookup_subjects(resource, tuple_to_userset(parent, computed), subject_type)`:

1. Query `resource_relation(resource, parent, fanout_limit)`.
2. For each userset subject row, take the intermediate object and ignore the row's subject relation,
   matching current evaluator semantics.
3. Execute the plan for `(intermediate_object.namespace, computed)`.
4. Yield nested subjects with a proof that includes the tuple edge and the nested computed proof.

For `lookup_resources(subject, tuple_to_userset(parent, computed), resource_type)`:

1. Execute resource lookup for each possible intermediate type that can evaluate `computed`.
2. For every proven intermediate object, query relationships where the intermediate object is the
   subject id, any subject relation is accepted, and the resource side is
   `resource_type#parent`.
3. Yield each resource with a tuple proof plus the nested computed proof.

Intermediate type policy:

- If `AllowedSubjectTypes::Explicit` is present on the tupleset relation, use exactly that object
  type list and require each type to define `computed`.
- If subject types are `Unspecified`, use a conservative policy:
  - exact proof is allowed only for rows whose intermediate object namespace defines `computed` and
    whose nested computed proof is exact;
  - candidates that cannot prove this carry `FullRoot`;
  - if preserving runtime errors from unrelated malformed tuple edges is required, fall back to the
    current lookup algorithm for the tuple branch.

The first implementation should prefer `FullRoot` or current-algorithm fallback over clever
shortcuts for unspecified tuple types. This keeps tuple-to-userset correct while still allowing
union/exclusion candidate pruning around it.

## 8. Reducing Candidate Re-Check

The core optimization is residual verification:

| Root shape | Candidate source | Verification |
| --- | --- | --- |
| `this` | exact direct or exact userset proof | none |
| `computed_userset(r)` | plan for `r` | inherited from `r` |
| `union(a, b, ...)` | any child | child residual only |
| `intersection(a, b, ...)` | cheapest exact child | remaining children only |
| `exclusion(base, exclude)` | base only | evaluate exclude as denied |
| `tuple_to_userset(t, c)` | tuple edge + intermediate `c` proof | none if exact, full root if conservative |

This turns many `check(root)` calls into cheaper targeted checks:

- `exclusion(union(viewer, editor, owner, parent->can_view), banned)` no longer re-evaluates the
  positive union after the candidate already came from it; it only checks `banned`.
- `intersection(viewer, active)` can seed from the smaller side and check only the other side.
- direct `this` lookup can return without calling `check` when there is no parent guard.

The executor must record benchmark-only counters:

- produced candidates;
- candidates dropped by schema relation pruning;
- full-root checks;
- residual checks by kind;
- candidates returned without check;
- tuple-to-userset fallbacks.

These counters are required before claiming a win, because raw latency can improve while candidate
count is unchanged due unrelated cache effects.

## 9. Store API Extensions

The first implementation can use existing iterators and filter rows in the executor. If profiling
shows subject postings are the remaining cost, add store-native methods without changing public API:

```rust
impl RelationshipStoreView {
    fn reverse_resource_relation(
        &self,
        subject: &SubjectFilter,
        resource_type: &ObjectType,
        relation: &RelationName,
        limit: QueryLimit,
    ) -> StoreViewCompactIter<'_>;

    fn reverse_resource_relation_any_subject_relation(
        &self,
        subject_type: &SubjectType,
        subject_id: &SubjectId,
        resource_type: &ObjectType,
        relation: &RelationName,
        limit: QueryLimit,
    ) -> StoreViewCompactIter<'_>;
}
```

Implementation options:

- v1: query by exact subject and filter `resource_type#relation`.
- v2: if posting lengths are available, choose the shorter of subject posting and
  resource-type-relation posting, then filter the other predicate.
- v3: add an optional persisted composite index only if v2 still leaves lookup resources
  millisecond-scale.

`IndexProfile::Full` is required for subject reverse lookup. If a loaded snapshot lacks reverse
indexes, the planner must fail over to the current supported behavior for that profile. It must not
silently return empty lookup results.

## 10. Soundness Proof Sketch

The planner is sound if every returned candidate is allowed by the same snapshot and schema that
would be used by `check`.

Definitions:

- `Eval(E, object, subject)` is the current evaluator result for expression `E`.
- `Plan(E)` produces candidates with proof state `P`.
- `Verify(P)` is either no-op, residual guard evaluation, or full-root `check`.

Induction by expression:

- `This`: a direct relationship row `object#relation@subject` is exactly the direct branch of
  `eval_this`. A userset subject row is exact only if the nested userset membership proof is exact;
  otherwise verification falls back to `check`.
- `ComputedUserset`: the schema definition says `computed_userset(r)` delegates to relation `r` on
  the same object. The plan uses the compiled same-namespace relation id, so the proof for `r` is a
  proof for the computed node.
- `TupleToUserset`: a tuple edge `object#tupleset@intermediate#_` plus an exact proof of
  `computed` on `intermediate` is exactly the evaluator's tuple-to-userset allowance rule. Runtime
  namespace uncertainty downgrades the proof to residual/full-root verification.
- `Union`: if any child is allowed, the union is allowed. A child proof is therefore a parent proof.
- `Intersection`: a candidate seeded from one child is returned only after every other child allows.
  Therefore every child allows and the intersection allows.
- `Exclusion`: a candidate comes from the base and is returned only if the exclude guard denies.
  Therefore `base - exclude` allows.

No false negatives for exact plan branches:

- `This` enumerates all matching direct rows and recursively all exact userset members.
- `ComputedUserset` delegates to the complete target relation plan.
- `TupleToUserset` enumerates every tupleset row and every exact intermediate computed member.
- `Union` emits from all children.
- `Intersection` seeds from a child that every allowed subject/resource must satisfy.
- `Exclusion` seeds from the base that every allowed subject/resource must satisfy.

Conservative branches do not claim no-false-negative proof. They must use the current algorithm or
full-root verification as specified in the fallback policy.

## 11. Fallback Policy

Fallback must be branch-local where possible and whole-request only when necessary.

Use current lookup implementation when:

- planner construction detects a recursive relation cycle that cannot be represented with existing
  depth semantics;
- the snapshot index profile does not support required reverse indexes;
- tuple-to-userset with unspecified intermediate types would otherwise skip rows that the current
  evaluator could inspect and error on;
- a residual guard would exceed fanout/depth semantics different from `check`;
- benchmark counters show the plan produces more full-root checks than the current algorithm for a
  known workload.

Use full-root `check` for a candidate when:

- the candidate came from a conservative producer;
- a tuple-to-userset proof depends on runtime namespace resolution;
- an intersection seed was not exact;
- a future optimization cannot produce a mechanical proof for `ProvenRoot`.

Return without check only when:

- the candidate proof covers the root expression;
- all residual guards are empty;
- the proof was produced from the same immutable `PublishedSnapshot`;
- normal depth and fanout limits were applied during proof construction.

## 12. Bench Gates

Add benchmark filters and counters before implementation claims success.

Required latency gates:

| Benchmark | Gate |
| --- | --- |
| `perf_optimization/lookup_resources_streaming_1m` | upper estimate improves by >= 25% versus Phase 14 follow-up, or profile-backed explanation |
| `perf_optimization/lookup_subjects_streaming_1m` | no > 5% regression |
| `realworld_authorization/1m_rules/lookup_resources_target_user` | no > 5% regression; record candidate and check counters |
| `realworld_authorization/1m_rules/lookup_subjects_shared_doc` | no > 5% regression; avoid exclude-only traversal |
| `realworld_authorization/1m_rules/mixed_read_workload` | no > 5% regression |
| `perf_optimization/check_prepared_1m` | no > 5% regression |
| `realworld_authorization/1m_rules/check_doc_inherited_workspace_member` | no > 5% regression |

Required instrumentation:

| Counter | Purpose |
| --- | --- |
| `lookup_candidates_produced` | raw candidates emitted by plan |
| `lookup_candidates_schema_pruned` | candidates avoided by relation/exclusion planning |
| `lookup_full_root_checks` | expensive verification calls remaining |
| `lookup_residual_checks` | targeted guard checks |
| `lookup_proven_without_check` | exact proof returns |
| `lookup_tuple_fallbacks` | tuple-to-userset conservative paths |

Acceptance requires showing Phase 14 follow-up comparison tables in the PR comment, not just local
Criterion output.

## 13. Implementation Plan

### M31.0 - Instrument Current Lookup

- Add benchmark-only counters to current `lookup_resources` and `lookup_subjects`.
- Count frontier rows, candidate resources/subjects, full-root checks, and result-limit exits.
- Record Phase 14 baseline counters in the PR comment.

Exit: the dominant lookup cost is visible without behavior changes.

### M31.1 - Positive Producer Plan

- Build a per-relation plan from `CompiledUsersetExpression`.
- Extract positive producer relations for `This`, plain computed usersets, union, and exclusion base.
- Keep the current subject frontier for `lookup_resources`, but only candidate resources from
  positive producer relations for the requested root.
- For `lookup_subjects`, stream base-side candidates instead of building a full `ExpandedUserset`
  tree for exclude-only branches.
- Verify every candidate with full-root `check`.

Exit: candidate count falls and all existing lookup tests pass.

### M31.2 - Residual Verification

- Attach `CandidateProof` to plan results.
- Return exact `This`/positive union candidates without full-root check when no residual guard
  exists.
- For exclusion, verify only the exclude guard after a positive base proof.
- For intersection, seed from the cheapest exact child and verify only remaining guards.
- Add adversarial tests for union, intersection, exclusion, cycles, depth, fanout, and duplicate
  paths.

Exit: `lookup_resources_streaming_1m` hits the 25% improvement target or the PR includes profile
evidence showing the remaining store iterator cost.

### M31.3 - Tuple-To-Userset Planning

- Implement exact `lookup_subjects` tuple-to-userset streaming for explicit or proven-safe
  intermediate types.
- Implement `lookup_resources` tuple-to-userset join through intermediate resources.
- Keep unspecified intermediate types conservative until runtime error behavior is proven.
- Add tests where tuple edge subject relation differs from the computed relation, because the
  current evaluator ignores the tuple row's subject relation.

Exit: inherited lookup cases improve without changing tuple-to-userset semantics.

### M31.4 - Store-Native Reverse Relation Filter

- Add `reverse_resource_relation` and any-subject-relation tuple join helpers if profiling still
  shows subject posting scans.
- Prefer runtime posting length selection before adding a persisted composite index.
- Re-run snapshot size benchmarks if a new persisted index is introduced.

Exit: store-side scan cost is bounded or explicitly deferred with counter evidence.

## 14. Why This Is High-Reward and High-Risk

This is likely the largest remaining lookup gain because it changes the algorithmic shape:

- current `lookup_resources` is approximately `O(reachable_subject_edges + candidates * full_check)`;
- planned lookup aims for `O(schema-positive_edges + candidates * residual_guard)`;
- planned exclusion and intersection avoid work that cannot add results;
- exact candidates can bypass full-root check entirely.

It is high-risk because lookup is where the evaluator's algebra, recursion policy, schema
resolution, snapshot consistency, and storage indexes meet:

- a wrong union/intersection/exclusion rule creates silent authorization errors;
- tuple-to-userset has dynamic namespace resolution and currently weak subject-type metadata;
- skipping final `check` is safe only with a mechanical proof from the same snapshot;
- planner recursion must preserve existing cycle-denied and depth-exceeded behavior;
- store index changes can improve latency while increasing snapshot size or load time.

The implementation should therefore land in the phased order above: measure, prune positive
producers with full-root verification, add residual proof only where the proof is obvious, then
tackle tuple-to-userset and store-native indexes.

## 15. Tests

Add tests beside the existing lookup and evaluator tests:

- `test_should_lookup_resources_from_union_positive_relations_only`
- `test_should_not_candidate_lookup_resources_from_exclusion_only_relation`
- `test_should_lookup_subjects_without_expanding_exclusion_only_subjects`
- `test_should_verify_intersection_guards_after_seed_candidate`
- `test_should_preserve_tuple_to_userset_subject_relation_ignored_semantics`
- `test_should_fallback_for_recursive_lookup_plan_without_depth_change`
- `test_should_preserve_exact_consistency_with_planned_lookup`
- `test_should_match_reference_lookup_for_generated_small_schemas`

The generated-schema test should compare planned lookup against the current implementation for
small acyclic schemas with `this`, computed userset, tuple-to-userset, union, intersection, and
exclusion.

## 16. AGENTS.md Binding

- Error Handling: planner construction and execution failures must use typed errors or existing
  `ZanzibarError` conversions; no `unwrap`, `expect`, or panic on schema/user data.
- Async & Concurrency: the planner is immutable after schema publication; request execution state is
  local to one lookup call.
- Type Design & API: relation ids, node ids, and proof states are internal newtypes/enums. Public
  lookup request and response types do not change.
- Safety & Security: result limits, fanout, depth, and exact snapshot semantics remain enforced
  before a candidate can be returned.
- Serialization: no public serialization change.
- Testing: every expression kind needs positive, negative, and fallback coverage before any
  no-final-check shortcut lands.
- Logging & Observability: counters stay benchmark-only unless a later observability spec promotes
  them to tracing fields.
- Performance: do not add a persisted index until counters prove iterator scanning remains the
  bottleneck after residual verification.
- Documentation: public docs continue to describe lookup in terms of `check` semantics; internal
  docs explain which proofs can skip full-root verification.

## 17. Cross-References

- Depends on: [14-evaluation-engine-design.md](./14-evaluation-engine-design.md),
  [16-compact-relationship-store-design.md](./16-compact-relationship-store-design.md),
  [23-read-performance-optimization-design.md](./23-read-performance-optimization-design.md),
  [71-performance-budgets-design.md](./71-performance-budgets-design.md)
- Related research: [../docs/research/study-spicedb.md](../docs/research/study-spicedb.md),
  especially the reachability and query-planner notes.
