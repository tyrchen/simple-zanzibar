# 27 - Compiled Evaluation Plan Design

Status: draft v1
Owner: Simple Zanzibar
Last updated: 2026-05-25
Depends on: [14](./14-evaluation-engine-design.md), [23](./23-read-performance-optimization-design.md), [25](./25-compiled-computed-userset-shortcut-design.md), [71](./71-performance-budgets-design.md)

## 1. Purpose

Phase 14 moved schema evaluation from public relation strings toward crate-private compiled schema
IR. The current hot evaluator still interprets a recursive enum tree:
`CompiledUsersetExpression` owns nested `Vec` and `Box` children, and `EvaluationContext` recursively
matches that tree for every `check` and `expand`.

This spec evaluates whether the next read optimization should compile each relation rewrite into a
compact opcode plan. The recommendation is yes, but as a flat arena plan with explicit child ranges,
not a general stack VM. That keeps the current evaluator semantics reviewable while improving
instruction locality, precomputing per-node strategy, and making future optimizer passes cheaper.

## 2. Current State

Relevant implementation points on PR #14 / branch `phase-14-read-performance-refinement`:

- `src/schema/mod.rs` stores `RelationDefinition::compiled_userset_rewrite:
  Option<CompiledUsersetExpression>`.
- `CompiledUsersetExpression::ComputedUserset` already carries same-namespace `SchemaRelationId`
  and `target_has_rewrite`.
- `CompiledUsersetExpression::TupleToUserset` carries the left-side `tupleset_relation_id`, but
  the right-side `computed_userset_relation` remains a name because the intermediate object's
  namespace determines the target relation at runtime.
- `src/eval.rs` evaluates the enum tree in `eval_compiled_schema_expression` and
  `expand_compiled_schema_expression`.
- Depth and cycle semantics are relation-frame based. Set-operation nodes do not spend depth;
  recursive relation checks, plain computed usersets, tuple-to-userset targets, and userset subjects
  do.
- `This` first checks exact direct membership, then scans userset subjects from the same
  resource/relation and recursively checks each userset subject.
- Check set algebra short-circuits: union exits on allowed, intersection exits on denied, and
  exclusion may evaluate a cheap exact exclude branch first.
- `expand` preserves tree shape and does not use check-style short-circuiting.

Current PR #14 follow-up evidence from [25](./25-compiled-computed-userset-shortcut-design.md):

| Benchmark | Current upper estimate |
| --- | ---: |
| `perf_optimization/check_prepared_1m` | `4.3065 us` |
| `realworld_authorization/1m_rules/check_doc_inherited_workspace_member` | `11.559 us` |
| `realworld_authorization/1m_rules/mixed_read_workload` | `41.808 us` |
| `perf_optimization/lookup_resources_streaming_1m` | `2.3551 ms` |
| `perf_optimization/lookup_subjects_streaming_1m` | `4.7426 us` |

## 3. Prior Art

[study-spicedb](../docs/research/study-spicedb.md) notes two useful patterns:

- SpiceDB separates request validation, schema metadata, graph dispatch, datastore iteration, and
  set algebra instead of letting one recursive function own every concern.
- SpiceDB's later query planner can represent check, subject iteration, and resource iteration from
  one plan tree, but the memo explicitly says Simple Zanzibar should not port that full iterator
  architecture until typed schemas and indexed snapshots are stable.

The proposed plan is deliberately smaller than SpiceDB's iterator planner. It compiles only the
already-validated relation rewrite expression used by `check` and `expand`; it does not introduce
pagination, recursive iterators, remote dispatch, caveat planners, optimizer registries, or
lookup-specific reverse planning.

## 4. Decision

Compile the enum tree into a compact per-relation evaluation plan.

Do not build a full bytecode VM with an operand stack and jump instructions in the first pass. The
hot work in this crate is not pure expression arithmetic: `This` and `TupleToUserset` enter store
iterators, fanout accounting, relation-frame depth checks, and recursive schema dispatch. A VM would
still need escape hatches for those operations and would make `expand` substantially harder to
review.

The first implementation should use:

- a flat `Arc<[PlanNode]>` node arena;
- a flat `Arc<[PlanNodeId]>` child-id arena;
- explicit `PlanChildRange` ranges for union, intersection, and exclusion children;
- precomputed opcode variants for `ComputedPlain`, `ComputedRewrite`, and exclusion evaluation
  strategy;
- one root plan per relation, including a `This` root for relations without rewrites.

This converts the recursive enum tree into a compact opcode plan without changing public APIs,
snapshot files, or Zanzibar semantics.

## 5. Goals

| # | Goal | Measure |
| --- | --- | --- |
| G1 | Remove recursive expression-tree pointer chasing from hot `check` and `expand` paths. | Relation evaluation dispatches through `EvaluationPlan` nodes, not nested `CompiledUsersetExpression` matches. |
| G2 | Precompute per-node strategy once at schema compile time. | `ComputedPlain` vs `ComputedRewrite` and exclusion ordering do not inspect child enum shape at runtime. |
| G3 | Preserve depth, cycle, fanout, and set algebra semantics exactly. | Existing evaluator and performance optimization tests pass; new tree-vs-plan equivalence tests pass. |
| G4 | Avoid public or serialized format churn. | Public schema model and snapshot file size stay unchanged. |
| G5 | Produce measurable read-path value or a clear stop signal. | Key read benchmarks improve or remain within gates with profile evidence that store work dominates. |

## 6. Non-Goals

- No public API change.
- No snapshot format change.
- No `unsafe`.
- No parser rewrite. `pest` to `winnow` remains orthogonal because parser work is not on
  steady-state read paths.
- No request-local memoization, bitmap index, adaptive delta compaction, or reverse lookup planner.
- No SpiceDB-style general iterator planner in this phase.
- No cross-revision semantic cache.

## 7. Target Data Model

The plan is crate-private schema runtime state. It should live beside the existing compiled enum
during migration and later replace it if the plan evaluator fully covers check and expand.

```rust
pub(crate) struct EvaluationPlan {
    root: PlanNodeId,
    nodes: Arc<[PlanNode]>,
    children: Arc<[PlanNodeId]>,
}

pub(crate) struct PlanNode {
    opcode: PlanOpcode,
    children: PlanChildRange,
}

pub(crate) struct PlanNodeId {
    index: u32,
}

pub(crate) struct PlanChildRange {
    start: u32,
    len: u16,
}

pub(crate) enum PlanOpcode {
    This,
    ComputedPlain {
        relation: RelationName,
        relation_id: SchemaRelationId,
    },
    ComputedRewrite {
        relation: RelationName,
        relation_id: SchemaRelationId,
    },
    TupleToUserset {
        tupleset_relation: RelationName,
        tupleset_relation_id: SchemaRelationId,
        computed_userset_relation: RelationName,
    },
    Union,
    Intersection,
    Exclusion {
        strategy: ExclusionStrategy,
    },
}

pub(crate) enum ExclusionStrategy {
    BaseFirst,
    ExcludeFirstIfAllowedDenies,
}
```

Implementation notes:

- Use `u32` indexes only after checked conversion at compile time. The interpreter must access
  nodes and child ranges through checked helper methods that return `ZanzibarError` on internal
  invariant failure.
- `PlanNodeId` can be zero-based to avoid `Option<NonZeroU32>` complexity. It remains internal and
  all accesses are checked.
- Use `Arc<[T]>` to match existing immutable schema storage and avoid per-request allocation.
- Do not add `smallvec` for this pass. A flat child arena gives most of the locality benefit without
  a dependency.
- Keep `RelationName` in `ComputedPlain` and `ComputedRewrite` because store lookup and recursion
  keys still need the relation name. Keep `SchemaRelationId` for same-namespace schema dispatch.
- Keep the tuple-to-userset computed target as `RelationName`. Its target namespace is determined
  by each intermediate userset object at evaluation time.

### 7.1 Relation Storage

Target `RelationDefinition` shape:

```rust
pub struct RelationDefinition {
    name: RelationName,
    allowed_subject_types: AllowedSubjectTypes,
    userset_rewrite: Option<UsersetExpression>,
    compiled_userset_rewrite: Option<CompiledUsersetExpression>,
    evaluation_plan: Arc<EvaluationPlan>,
}
```

Every relation has an `evaluation_plan`. Plain relations use a single-node `This` plan. This lets
`check_prepared` skip the current `Option` dispatch after the migration completes.

During migration, keep `compiled_userset_rewrite` as the reference evaluator input until check and
expand plan execution are proven equivalent. After that, remove the enum tree if no remaining code
needs it.

## 8. Opcode Semantics

### 8.1 `This`

`This` calls the existing direct relation evaluator:

1. Convert the public `Object` to `DomainObjectRef`.
2. Convert the public `User` to `SubjectFilter`.
3. Query exact `resource#relation@subject` membership.
4. If allowed, return `Allowed`.
5. Iterate `resource#relation` userset subjects.
6. Spend one fanout unit only for each userset subject considered.
7. Recursively check `nested_object#nested_relation@original_subject`.
8. Return `Allowed` on the first allowed recursive result; otherwise `Denied`.

The opcode itself does not spend depth. Recursive relation checks created by step 7 do.

### 8.2 `ComputedPlain`

`ComputedPlain` represents same-object computed userset whose target relation has no rewrite.

Check execution must call the existing plain computed path:

- enter a check frame for `(object, relation, user)`;
- apply cycle detection before evaluation;
- spend depth for that frame;
- evaluate `This` for the target relation.

This preserves the Phase 14 depth fix. The plan must not inline `This` without entering the
relation frame.

Expand execution calls `expand_relation_name(object, relation)`, same as computed rewrite, because
expand recursion and cycle handling are relation-frame based.

### 8.3 `ComputedRewrite`

`ComputedRewrite` represents same-object computed userset whose target relation has its own rewrite.

Check execution calls `check_relation_id(object, relation, relation_id, user)`. This keeps the
schema lookup id-native and preserves the current active check key.

Expand execution calls `expand_relation_name(object, relation)`.

### 8.4 `TupleToUserset`

`TupleToUserset` execution:

1. Resolve or use the plan's `tupleset_relation` name for resource-side store lookup.
2. Iterate `object#tupleset_relation`.
3. Skip direct user subjects without spending fanout.
4. For each userset subject, spend one fanout unit.
5. For check, recursively check
   `intermediate_object#computed_userset_relation@original_subject`.
6. For expand, recursively expand `intermediate_object#computed_userset_relation` and push each
   child expansion into a union result.

The left relation is same-namespace and has a static relation id. The right relation is dynamic
because each intermediate object supplies the namespace. The first implementation should keep the
name-based right side. A later allowed-subject-type planner may compile a multi-target relation-id
table, but that requires stronger schema typing than this phase.

### 8.5 `Union`

Check execution evaluates child ids in source order and applies membership union:

- start with `Denied`;
- return `Allowed` as soon as any child returns `Allowed`;
- preserve `Conditional` algebra behavior for future caveats.

Expand execution evaluates all children and returns `ExpandedUserset::Union(children)`.

### 8.6 `Intersection`

Check execution evaluates child ids in source order and applies membership intersection:

- start with `Allowed`;
- return `Denied` as soon as any child returns `Denied`;
- preserve `Conditional` algebra behavior for future caveats.

Expand execution evaluates all children and returns `ExpandedUserset::Intersection(children)`.

### 8.7 `Exclusion`

The plan compiler computes `ExclusionStrategy` once:

- `ExcludeFirstIfAllowedDenies` when the exclude child is `This` or `ComputedPlain`;
- `BaseFirst` for tuple-to-userset, union, intersection, nested exclusion, and computed rewrite.

Check execution:

- `ExcludeFirstIfAllowedDenies`: evaluate exclude first; if it returns `Allowed`, return `Denied`;
  otherwise evaluate base and apply membership exclusion.
- `BaseFirst`: evaluate base first; if it returns `Denied`, return `Denied`; otherwise evaluate
  exclude and apply membership exclusion.

Expand execution always evaluates base and exclude, then returns `ExpandedUserset::Exclusion`.
Expand does not use the check-only exclusion strategy.

## 9. Execution Diagram

```text
Schema compile / replace_schema
    |
    v
+---------------------------------------------------------------+
| CompiledSchema                                                |
|                                                               |
|  NamespaceDefinition[]                                        |
|      |                                                        |
|      v                                                        |
|  RelationDefinition                                           |
|      - public userset_rewrite             (reference shape)   |
|      - compiled_userset_rewrite           (migration oracle)  |
|      - evaluation_plan                    (new hot path)      |
+-------------------------------+-------------------------------+
                                |
                                | immutable Arc
                                v
+---------------------------------------------------------------+
| PublishedSnapshot                                             |
|  schema resolver + relation plans                             |
|  relationship store view                                      |
+-------------------------------+-------------------------------+
                                |
                                v
+---------------------------------------------------------------+
| EvaluationContext                                             |
|  remaining_depth, fanout limit, active check/expand stacks    |
|                                                               |
|  check relation frame                                         |
|      |                                                        |
|      v                                                        |
|  execute_plan_check(root)                                     |
|      |                                                        |
|      +--> This ---------------------> store exact + usersets   |
|      |                                   |                    |
|      |                                   v                    |
|      |                              recursive check frame     |
|      |                                                        |
|      +--> ComputedPlain ------------> plain check frame       |
|      |                                                        |
|      +--> ComputedRewrite ----------> id-native check frame   |
|      |                                                        |
|      +--> TupleToUserset -----------> store tupleset scan     |
|      |                                   |                    |
|      |                                   v                    |
|      |                              dynamic relation check    |
|      |                                                        |
|      +--> Union / Intersection -----> ordered child ids       |
|      |                                   short-circuit check  |
|      |                                                        |
|      +--> Exclusion ----------------> precomputed strategy    |
|                                                               |
|  expand relation frame                                        |
|      |                                                        |
|      v                                                        |
|  execute_plan_expand(root)                                    |
|      same opcodes, no check short-circuit tree collapse       |
+---------------------------------------------------------------+
```

## 10. Plan Compiler

The compiler runs after schema reference validation and after relation ids are known.

Algorithm:

1. For a plain relation, emit one `This` node and set it as root.
2. For a relation with a rewrite, recursively lower the existing `CompiledUsersetExpression` into
   the flat plan arena.
3. Append leaf nodes directly.
4. For `Union` and `Intersection`, lower children left to right, append their ids into the child-id
   arena, then append the parent node with the child range.
5. For `Exclusion`, lower base and exclude, append exactly two child ids, compute
   `ExclusionStrategy`, then append the parent node.
6. Validate plan invariants before storing:
   - root id resolves;
   - every child range resolves;
   - leaves have zero children;
   - union and intersection have at least one child;
   - exclusion has exactly two children;
   - node and child counts fit in `u32` and `u16` fields.

The compiler must not accept invalid schema input that the existing schema validator rejects. It
must not add a public `SchemaError` variant for impossible internal plan states. If an internally
generated plan fails validation, use an existing internal invariant error path or a crate-private
error type that is converted before crossing the public API boundary.

## 11. Interpreter Contract

Add two internal entry points:

```rust
impl EvaluationContext<'_> {
    fn eval_plan_check(
        &mut self,
        object: &Object,
        relation_name: &RelationName,
        user: &User,
        plan: &EvaluationPlan,
    ) -> Result<Membership, ZanzibarError>;

    fn eval_plan_expand(
        &mut self,
        object: &Object,
        relation_name: &RelationName,
        plan: &EvaluationPlan,
    ) -> Result<ExpandedUserset, ZanzibarError>;
}
```

Helper methods must be small and explicit:

- `plan.node(id) -> Result<&PlanNode, ZanzibarError>`;
- `plan.children(node) -> Result<&[PlanNodeId], ZanzibarError>`;
- `eval_plan_node_check(...)`;
- `eval_plan_node_expand(...)`.

Avoid unchecked indexing and `unwrap` / `expect`. Even though plans are internally generated, the
project's safety posture treats corrupted snapshots and internal invariant drift as explicit errors.

## 12. Semantic Invariants

The plan evaluator must preserve these invariants:

- Public `check`, `expand`, `lookup_resources`, and `lookup_subjects` results do not change.
- Repeated active check key returns `Denied`, not `DepthExceeded`.
- Exceeding `max_depth` returns `EvaluationError::DepthExceeded`.
- Set-operation expression nodes do not spend depth.
- `ComputedPlain` still spends depth because it enters a target relation frame.
- `This` direct exact hits do not spend indirect fanout.
- `This` userset subjects spend fanout one per userset subject considered.
- `TupleToUserset` direct user subjects on the tupleset relation are skipped without spending
  fanout.
- `TupleToUserset` userset subjects spend fanout one per intermediate userset object considered.
- Check `Union`, `Intersection`, and `Exclusion` preserve current short-circuit order.
- Expand evaluates every required child and preserves `ExpandedUserset` shape.
- Tuple-to-userset right-side relation remains namespace-dynamic.
- Plan invariant errors are impossible for valid compiled schemas; if detected, return a typed
  internal storage/schema error rather than panicking.

## 13. Migration Plan

### M27.0 - Plan Data Model and Compiler

- Add crate-private plan types under the schema module or a new `schema::plan` module.
- Compile `evaluation_plan` for every relation during `CompiledSchema::from_definitions`.
- Keep `compiled_userset_rewrite` unchanged.
- Add tests that assert simple plan shapes for:
  - plain relation;
  - computed plain;
  - computed rewrite;
  - tuple-to-userset;
  - union;
  - intersection;
  - exclusion with both strategies.

Exit: schema compile produces validated plans and no evaluator uses them yet.

### M27.1 - Check Plan Interpreter in Shadow Tests

- Implement `eval_plan_check`.
- Add test-only reference helpers that evaluate the old enum tree and the new plan for the same
  request.
- Cover all existing `tests/eval_tests.rs` and Phase 14 performance-adversarial cases.
- Add a `proptest` generator for small valid same-namespace expression trees and direct
  relationship sets. Keep the generated grammar small enough to avoid invalid dynamic
  tuple-to-userset typing.

Exit: tree-vs-plan check equivalence is proven for existing examples and randomized small schemas.

### M27.2 - Switch Check Hot Path

- Change `check_prepared`, `check_relation_name_entered_with_type`, and `check_relation_id_entered`
  to execute `relation_definition.evaluation_plan()`.
- Keep the old enum tree behind tests for one release cycle of the branch.
- Run the full correctness and benchmark gate set.

Exit: check benches pass gates and no correctness test regresses.

### M27.3 - Expand Plan Interpreter

- Implement `eval_plan_expand`.
- Switch `expand_relation_name_entered` to execute the plan.
- Add expand tree-vs-plan tests for:
  - direct relation;
  - computed userset cycle;
  - tuple-to-userset with direct users skipped;
  - union/intersection/exclusion shape preservation.

Exit: expand tests pass and lookup-subjects behavior remains stable.

### M27.4 - Cleanup

- Remove `compiled_userset_rewrite` only after check and expand no longer need it as a reference.
- Keep public `userset_rewrite` for schema export, debugging, and compatibility.
- Update [23](./23-read-performance-optimization-design.md) and
  [71](./71-performance-budgets-design.md) with implementation evidence in the implementation PR,
  not during this spec-only worker task.

Exit: relation evaluation uses one compiled runtime representation.

## 14. Verification

Required correctness tests:

- Existing `tests/eval_tests.rs`.
- Existing `tests/performance_optimization_tests.rs`.
- New unit tests for plan compiler shape and invariant checks.
- New equivalence tests that compare old enum-tree and new plan evaluators before the old evaluator
  is removed.
- New regression tests for:
  - `ComputedPlain` depth spending;
  - recursion cycle denial with relation ids;
  - exclusion cheap-exclude ordering;
  - tuple-to-userset dynamic namespace target;
  - expand shape preservation;
  - fanout accounting for direct users versus userset subjects.

Required command gates:

```text
cargo build --workspace --all-targets
cargo test --workspace --all-targets
cargo test --workspace --all-features
cargo +nightly fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings -W clippy::pedantic -W clippy::unwrap_used -W clippy::expect_used -W clippy::indexing_slicing -W clippy::panic
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps
cargo audit
cargo deny check
```

## 15. Benchmark Gates

Compare against PR #14 follow-up numbers from [25](./25-compiled-computed-userset-shortcut-design.md).

| Benchmark | Baseline upper | Hard gate | Stretch target |
| --- | ---: | ---: | ---: |
| `perf_optimization/check_prepared_1m` | `4.3065 us` | no > 5% regression (`<= 4.5218 us`) | `<= 4.10 us` |
| `realworld_authorization/1m_rules/check_doc_inherited_workspace_member` | `11.559 us` | no > 5% regression (`<= 12.137 us`) | `<= 11.10 us` |
| `realworld_authorization/1m_rules/mixed_read_workload` | `41.808 us` | no > 5% regression (`<= 43.899 us`) | `<= 40.50 us` |
| `perf_optimization/lookup_resources_streaming_1m` | `2.3551 ms` | no > 5% regression (`<= 2.4729 ms`) | recorded |
| `perf_optimization/lookup_subjects_streaming_1m` | `4.7426 us` | no > 5% regression (`<= 4.9797 us`) | recorded |

Additional benchmark requirements:

- Record `snapshot_load_compact/1m` and `snapshot_load_trusted_fast/1m` if schema compile or
  snapshot load timing changes. Plan compilation happens during schema load, so a large schema
  compile regression must be visible.
- Record plan node counts for the realworld and org schemas under benchmark-only diagnostics.
- If check benchmarks do not improve, include profile evidence that relationship-store work, not
  expression dispatch, is dominant.

Expected benefit:

- `check_prepared_1m`: 2-5% upper-estimate improvement.
- inherited check: 1-4% upper-estimate improvement.
- mixed read: 1-3% upper-estimate improvement.
- lookup resources: likely neutral unless candidate verification dominates.

These estimates are intentionally modest. Current relation rewrites are small, and Phase 14 already
removed the highest-impact resolver lookups.

## 16. Risks and Mitigations

| Risk | Impact | Mitigation |
| --- | --- | --- |
| Plan interpreter accidentally inlines `ComputedPlain` and skips depth/cycle accounting. | Incorrect authorization result or missing depth error. | Keep `ComputedPlain` as a relation-frame opcode and add explicit depth tests. |
| Tuple-to-userset right side is incorrectly compiled to same-namespace relation id. | Cross-namespace permissions break. | Keep right side as `RelationName`; add dynamic namespace regression test. |
| Checked plan access erases locality gains. | No measurable improvement. | Keep helper methods small; profile before considering more compact encoding. |
| Expand plan execution collapses result shape. | Public `expand` output changes. | Separate check and expand interpreters; add shape-preservation tests. |
| Duplicating enum tree and plan increases schema memory. | Small RSS increase. | Migration-only duplication; remove enum runtime copy after equivalence is proven. |
| Full bytecode VM temptation expands scope. | High correctness risk. | Defer jump/stack VM until flat plan evidence shows dispatch remains dominant. |

## 17. AGENTS.md Binding

- Error Handling: plan invariant failures return typed errors; no `unwrap`, `expect`, or panics in
  production evaluator code.
- Async & Concurrency: plans are immutable schema state behind `Arc`; evaluation remains
  synchronous over a `PublishedSnapshot`.
- Type Design & API: plan types are crate-private; public schema and engine APIs do not change.
- Safety & Security: no `unsafe`, no unchecked indexing, no unbounded allocation from external
  input, and no panic path reachable from user schema or relationships.
- Serialization: no snapshot or JSON format change.
- Testing: equivalence tests must exist before switching hot paths.
- Logging & Observability: no new per-request logging on hot paths. Benchmark-only diagnostics may
  expose plan node counts.
- Performance: profile and benchmark before removing the reference evaluator.
- Documentation: public docs do not need new API examples because this is an internal runtime
  representation.

## 18. Recommendation

Recommended priority: P2.

This is worth doing after request-local memoization, or in parallel if the worker can keep the
change isolated. It is lower risk than bitmap indexes and reverse lookup planning, but the expected
latency win is also smaller because Phase 14 already moved the largest costs out of the evaluator.

Proceed if the goal is to keep shaving steady-state check latency while preparing the codebase for
future schema-aware optimizer passes. Do not proceed as a standalone large VM rewrite.

## 19. Cross-References

- <- Depends on: [14-evaluation-engine-design.md](./14-evaluation-engine-design.md),
  [23-read-performance-optimization-design.md](./23-read-performance-optimization-design.md),
  [25-compiled-computed-userset-shortcut-design.md](./25-compiled-computed-userset-shortcut-design.md),
  [71-performance-budgets-design.md](./71-performance-budgets-design.md)
- Related research:
  [../docs/research/study-spicedb.md](../docs/research/study-spicedb.md)
