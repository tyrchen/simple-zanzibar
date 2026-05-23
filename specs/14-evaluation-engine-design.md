# 14 - Evaluation Engine Design

Status: draft v1
Owner: Simple Zanzibar
Depends on: [11-schema-system-design.md](./11-schema-system-design.md), [12-relationship-store-design.md](./12-relationship-store-design.md), [13-revision-consistency-design.md](./13-revision-consistency-design.md)

## 1. Purpose

The evaluation engine answers `check`, `expand`, `lookup_resources`, and `lookup_subjects` from the same validated schema and relationship snapshot. It replaces the current single recursive function with a bounded execution context, direct-vs-userset relation evaluation, and shared membership algebra. SpiceDB's graph checker separates direct checks, non-terminal dispatch, tuple-to-userset, and set operations in `vendors/spicedb/internal/graph/check.go:304-596`; this design compresses that into a local synchronous engine.

## 2. Request Flow

```text
Public API request
  |
  v
+--------------------+
| Parse request      |
| relationship refs  |
+---------+----------+
          |
          v
+--------------------+
| Pick snapshot      |
| Latest or Exact    |
+---------+----------+
          |
          v
+--------------------+
| Schema validation  |
| object/relation    |
+---------+----------+
          |
          v
+--------------------+
| EvaluationContext  |
| depth, visited     |
+---------+----------+
          |
          v
+------------------------------+
| Expression evaluator         |
| - direct relation query      |
| - computed userset           |
| - tuple-to-userset           |
| - union/intersection/exclude |
+---------+--------------------+
          |
          v
Typed result
```

## 3. Execution Context

```rust
pub struct EvaluationContext<'a> {
    snapshot: &'a PublishedSnapshot,
    max_depth: NonZeroU32,
    remaining_depth: u32,
    visited: FxHashSet<CheckKey>,
    limits: EvaluationLimits,
}

pub struct EvaluationLimits {
    pub max_depth: NonZeroU32,
    pub max_fanout_per_step: NonZeroU32,
    pub max_lookup_results: NonZeroU32,
}
```

`FxHashSet` is a candidate internal set representation; [60-crates-features-design.md](./60-crates-features-design.md) decides whether to use `rustc-hash = 2.1.2` or std collections first.

## 4. Membership Algebra

```rust
pub enum Membership {
    Allowed,
    Denied,
    Conditional(ConditionExpression),
}
```

`Conditional` is reserved. v2 returns it only through internal tests that prove set operations are shape-compatible; public caveats are not accepted. This follows the research recommendation to keep caveat-aware algebra extensible while deferring caveat semantics.

Set operations:

- `union`: returns `Allowed` as soon as any branch is allowed for `check`.
- `intersection`: returns `Denied` as soon as any branch is denied.
- `exclusion`: evaluates base first; denied base short-circuits; allowed exclude turns result denied.
- conditional combination exists as internal enum plumbing, not public feature.

## 5. Direct Relation Algorithm

For `This`:

```text
1. query direct match: resource#relation@subject
2. if found, return Allowed
3. query userset subjects for resource#relation
4. group userset subjects by object type/relation
5. recursively check subject userset membership
6. return Allowed if any recursive check allows; otherwise Denied
```

This replaces the current scan-then-loop path with store-level directional queries. SpiceDB's direct path uses direct query plus non-terminal dispatch in `vendors/spicedb/internal/graph/check.go:406-512`.

## 6. Tuple-To-Userset Algorithm

For `TupleToUserset`:

```text
1. query resource#tupleset_relation@*
2. keep only userset subjects
3. for each intermediate object:
     check intermediate_object#computed_userset_relation@original_subject
4. union the results
```

Schema validation guarantees the target relation can exist before this runs. Fanout is bounded by `EvaluationLimits`.

## 7. Lookup APIs

`lookup_resources` and `lookup_subjects` are built on the same primitives:

- `lookup_resources(subject, permission, resource_type)` starts from subject-side indexes and validates candidate resources through `check`.
- `lookup_subjects(resource, permission, subject_type)` starts from resource-side indexes and validates candidate subjects through `check`.
- Both APIs support result limits.
- Cursor pagination is deferred until Phase 5 because local library callers can initially consume iterators.

SpiceDB's lookup resources path shares consistency, schema validation, duplicate suppression, cursor handling, and dispatch with check in `vendors/spicedb/internal/services/v1/permissions.go:492-653`. Simple Zanzibar adopts the shared-layer principle and defers cursor complexity.

## 8. Recursion Policy

Cycle and depth handling are explicit:

- repeated `CheckKey(resource, relation, subject)` in the active path returns `Denied` by default
- exceeding `max_depth` returns `EvaluationError::DepthExceeded`
- default max depth is `50`, matching SpiceDB's recursive query default in `vendors/spicedb/pkg/query/recursive.go:43`
- users can lower the depth through builder limits

The default cycle-denied behaviour preserves current Simple Zanzibar semantics while making depth exhaustion distinguishable.

## 9. AGENTS Binding

- Error Handling: `EvaluationError` is typed and does not collapse depth/schema/store errors into strings.
- Async & Concurrency: core evaluator is synchronous and side-effect free over an immutable snapshot.
- Type Design & API: limits are newtypes; invalid zero values cannot be built.
- Safety & Security: no panics on malformed requests; fanout and depth are bounded.
- Serialization: API result serialization is optional and camelCase.
- Testing: unit tests for every expression variant; property tests for set algebra; integration tests for cross-namespace usersets.
- Logging & Observability: `tracing` spans include operation, revision, depth, and result, not full subject IDs unless debug feature is enabled.
- Performance: hot-path direct checks avoid allocation where possible.
- Documentation: each public evaluation method documents consistency and errors.

## 10. Cross-References

- <- Depends on: [11-schema-system-design.md](./11-schema-system-design.md), [12-relationship-store-design.md](./12-relationship-store-design.md), [13-revision-consistency-design.md](./13-revision-consistency-design.md)
- -> Consumed by: [15-public-api-design.md](./15-public-api-design.md), [71-performance-budgets-design.md](./71-performance-budgets-design.md), [72-testing-verification-plan.md](./72-testing-verification-plan.md)
- Related research: [../docs/research/study-spicedb.md § Algorithms Worth Porting Conceptually](../docs/research/study-spicedb.md#algorithms-worth-porting-conceptually)
