# 25 - Compiled Computed-Userset Shortcut Design

Status: implemented v1
Owner: Simple Zanzibar
Last updated: 2026-05-24
Depends on: [11](./11-schema-system-design.md), [14](./14-evaluation-engine-design.md), [23](./23-read-performance-optimization-design.md), [71](./71-performance-budgets-design.md)

## 1. Purpose

The Phase 14 completion pass made read evaluation ID-native for schema relation edges, but a deep
follow-up review found one remaining runtime rediscovery in the hot evaluator path:
`ComputedUserset` expressions still ask the schema resolver whether their target relation has a
rewrite every time they run. That fact is known during schema compilation.

This spec moves the plain-target proof into the compiled schema IR so `check` can take the exact
computed-userset shortcut without another resolver lookup. It is intentionally narrower than a
parser migration: the latest Phase 14 timer records schema parse/compile at about 51 us inside a
~572 ms full snapshot load, and parsing is not on steady-state read paths.

## 2. Deep Review Findings

| Finding | Decision |
| --- | --- |
| `pest` remains a transitive snapshot-load cost, but schema parse/compile is ~0.01% of full 1M snapshot load. | Defer `pest` -> `winnow` until parser work is user-facing or parse benchmarks become material. |
| `CompiledUsersetExpression::ComputedUserset` stores a relation id but not whether the target relation is plain. | Add a compiled boolean so exact computed-userset shortcuts do not call the resolver. |
| Exclusion ordering asks `computed_userset_is_plain_relation` at runtime. | Reuse the compiled boolean and remove the helper. |
| Tuple-to-userset target relation remains name-based because the intermediate object namespace is dynamic. | Keep it unchanged; this spec only handles same-namespace computed-userset edges. |

## 3. Target Flow

```text
Schema compile
  |
  v
+-----------------------------------------+
| ComputedUserset compiled node           |
| - relation name for store lookup        |
| - relation id for recursive schema eval |
| - target_has_rewrite bool               |
+--------------------+--------------------+
                     |
                     v
+-----------------------------------------+
| Evaluator                               |
| - if !target_has_rewrite: eval_this     |
| - else: recurse by relation id          |
| - exclusion ordering uses same flag     |
+-----------------------------------------+
```

## 4. Goals

| # | Goal | Measure |
| --- | --- | --- |
| G1 | Remove runtime plain-target resolver checks from computed-userset evaluation. | No call to `relation_by_id` just to test `compiled_userset_rewrite().is_none()`. |
| G2 | Preserve exact Zanzibar semantics. | Existing computed-userset, exclusion, depth, cycle, and cross-namespace tests pass. |
| G3 | Avoid read-path regression versus Phase 14 completion. | `check_prepared_1m`, inherited check, and mixed read stay within 5% of Phase 14 completion upper estimates. |

## 5. Non-Goals

- No public API or snapshot-format change.
- No parser migration in this follow-up.
- No shortcut for tuple-to-userset computed targets whose namespace is determined at runtime.
- No semantic cache across revisions.

## 6. Design

Extend `CompiledUsersetExpression::ComputedUserset` with `target_has_rewrite: bool`. The schema
compiler fills it by resolving the same-namespace target relation once, using the uncompiled
relation definition that already passed schema validation. The evaluator then:

- calls `eval_plain_computed_userset` immediately when `target_has_rewrite` is false;
- recurses through `check_relation_id` when `target_has_rewrite` is true;
- uses `!target_has_rewrite` for conservative exclusion ordering.

This preserves the existing relation id for recursive evaluation and keeps the relation name for
store lookup. It also removes `computed_userset_is_plain_relation`, which reparsed the object
namespace and repeated a schema hash lookup in a path that already has compiled expression data.

## 7. Benchmarks and Gates

Compare against the Phase 14 completion evidence:

| Benchmark | Phase 14 completion upper | Follow-up gate |
| --- | ---: | --- |
| `perf_optimization/check_prepared_1m` | `4.4450 us` | no >5% regression; improvement expected |
| `realworld_authorization/1m_rules/check_doc_inherited_workspace_member` | `11.646 us` | no >5% regression; improvement expected |
| `realworld_authorization/1m_rules/mixed_read_workload` | `42.690 us` | no >5% regression |

Record final numbers in [71](./71-performance-budgets-design.md) and post the comparison to the
Phase 14 PR.

## 8. AGENTS.md Binding

- Error Handling: compiled invariant failures stay typed as `ZanzibarError`.
- Type Design: the new flag is crate-private compiled IR, not a public schema API.
- Safety & Security: no `unsafe`, no unchecked indexing, no panic path from user schema input.
- Testing: existing adversarial computed-userset and exclusion tests remain mandatory.
- Performance: no dependency additions; benchmark before claiming the follow-up complete.

## 9. Implementation Evidence

Implemented 2026-05-24 in PR #14 after the Phase 14 completion commit. Benchmarks compare against
the Phase 14 completion evidence from [71](./71-performance-budgets-design.md).

| Benchmark | Phase 14 completion | Follow-up | Result |
| --- | ---: | ---: | --- |
| `perf_optimization/check_prepared_1m` | `[4.3107 us, 4.3867 us, 4.4450 us]` | `[4.2240 us, 4.2590 us, 4.3065 us]` | upper `-3.12%` |
| `realworld_authorization/1m_rules/check_doc_inherited_workspace_member` | `[11.540 us, 11.599 us, 11.646 us]` | `[11.211 us, 11.341 us, 11.559 us]` | upper `-0.75%` |
| `realworld_authorization/1m_rules/mixed_read_workload` | `[41.599 us, 42.085 us, 42.690 us]` | `[40.921 us, 41.317 us, 41.808 us]` | upper `-2.07%` |

Validation:

- `cargo test --workspace --all-targets`
- `cargo +nightly fmt --all -- --check`
- `cargo clippy --workspace --all-targets --all-features -- -D warnings -W clippy::pedantic -W clippy::unwrap_used -W clippy::expect_used -W clippy::indexing_slicing -W clippy::panic`

## 10. Cross-References

- <- Depends on: [11-schema-system-design.md](./11-schema-system-design.md), [14-evaluation-engine-design.md](./14-evaluation-engine-design.md), [23-read-performance-optimization-design.md](./23-read-performance-optimization-design.md), [71-performance-budgets-design.md](./71-performance-budgets-design.md)
