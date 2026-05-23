# 91 - Implementation Plan: Local Zanzibar Engine

Status: draft v1
Owner: Simple Zanzibar
Last updated: 2026-05-23

## 1. Readiness Assessment

Ready:

- SpiceDB research memo exists at [../docs/research/study-spicedb.md](../docs/research/study-spicedb.md).
- SpiceDB source is vendored at `vendors/spicedb`.
- Current toy implementation provides behavior tests and examples.

Needs implementation:

- v2 domain model
- schema validation
- indexed store
- revisioned snapshots
- shared evaluator
- public v2 API
- benchmark and property-test suites

`~/.codex/AGENTS.md` was not present on this machine during spec creation. Project `AGENTS.md` is binding and encoded in the component specs.

## 2. Why Dependency Order Differs From Feature Order

Users want fast `check` first, but fast check cannot be correct until schema references and relationship writes are validated. Therefore schema and store contracts land before evaluator work.

Users want lookup APIs, but lookup without subject-side indexes becomes a scan-heavy API. Therefore relationship indexes land before lookup.

Users want consistency tokens after writes, but tokens are meaningless without snapshot publication. Therefore revision snapshots land before token-facing public APIs.

## 3. Phase 0 - Risk Retirement

| # | Deliverable | Lands in | Effort |
| --- | --- | --- | --- |
| 0.1 | Decide whether to keep pest for M0 or migrate parser internals to `winnow = 1.0.3`. | [11](./11-schema-system-design.md), [60](./60-crates-features-design.md) | 0.5 day |
| 0.2 | Benchmark current direct check and store scan baseline. | [71](./71-performance-budgets-design.md) | 0.5 day |
| 0.3 | Validate `arc-swap = 1.9.1` publication API against desired snapshot shape. | [13](./13-revision-consistency-design.md), [60](./60-crates-features-design.md) | 0.5 day |
| 0.4 | Choose schema hash dependency or std-only fallback. | [11](./11-schema-system-design.md), [60](./60-crates-features-design.md) | 0.5 day |

Exit gate: decisions recorded in [99-key-decisions.md](./99-key-decisions.md), benchmark baseline committed, no production code beyond measurement harness.

## 4. Phase 1 - Domain and Schema Spine

Closes M0 foundation.

| # | Task | Spec | Effort |
| --- | --- | --- | --- |
| 1.1 | Add `domain` module with validated newtypes and relationship parser. | [10](./10-local-engine-data-model-design.md) | 1 day |
| 1.2 | Add schema IR and resolver. | [11](./11-schema-system-design.md) | 1 day |
| 1.3 | Compile legacy DSL into schema IR. | [11](./11-schema-system-design.md), [15](./15-public-api-design.md) | 1 day |
| 1.4 | Add schema validator for duplicates and relation references. | [11](./11-schema-system-design.md) | 1.5 days |
| 1.5 | Wire `ZanzibarService` facade through v2 schema where possible. | [15](./15-public-api-design.md) | 1 day |
| 1.6 | Add schema/domain tests and doctests. | [72](./72-testing-verification-plan.md) | 1 day |

Exit criteria: M0 roadmap criteria pass.

## 5. Phase 2 - Indexed Store and Write Semantics

Closes M1.

| # | Task | Spec | Effort |
| --- | --- | --- | --- |
| 2.1 | Add relationship row/key model and uniqueness set. | [12](./12-relationship-store-design.md) | 0.5 day |
| 2.2 | Add resource-side and subject-side indexes. | [12](./12-relationship-store-design.md) | 1 day |
| 2.3 | Add query filters and iterator API. | [12](./12-relationship-store-design.md) | 1 day |
| 2.4 | Add create/touch/delete batch mutations. | [12](./12-relationship-store-design.md) | 1 day |
| 2.5 | Add precondition checks. | [12](./12-relationship-store-design.md) | 0.5 day |
| 2.6 | Add property tests for index consistency. | [72](./72-testing-verification-plan.md) | 1 day |
| 2.7 | Port direct check to indexed store path. | [14](./14-evaluation-engine-design.md) | 1 day |

Exit criteria: M1 roadmap criteria pass.

## 6. Phase 3 - Revisions and Tokens

Closes M2.

| # | Task | Spec | Effort |
| --- | --- | --- | --- |
| 3.1 | Add `Revision`, `SchemaHash`, `DatastoreId`, `ConsistencyToken`. | [13](./13-revision-consistency-design.md) | 1 day |
| 3.2 | Add `PublishedSnapshot` and current-snapshot publication. | [13](./13-revision-consistency-design.md) | 1 day |
| 3.3 | Add snapshot history and token validation. | [13](./13-revision-consistency-design.md) | 1 day |
| 3.4 | Return tokens from schema and relationship writes. | [15](./15-public-api-design.md) | 0.5 day |
| 3.5 | Add exact snapshot tests. | [72](./72-testing-verification-plan.md) | 0.5 day |

Exit criteria: M2 roadmap criteria pass.

## 7. Phase 4 - Shared Evaluation Engine

Closes M3.

| # | Task | Spec | Effort |
| --- | --- | --- | --- |
| 4.1 | Add `EvaluationContext`, depth handling, and visited keys. | [14](./14-evaluation-engine-design.md) | 1 day |
| 4.2 | Add membership algebra. | [14](./14-evaluation-engine-design.md) | 1 day |
| 4.3 | Implement computed userset and tuple-to-userset over snapshot reader. | [14](./14-evaluation-engine-design.md) | 1.5 days |
| 4.4 | Implement union/intersection/exclusion over membership algebra. | [14](./14-evaluation-engine-design.md) | 1 day |
| 4.5 | Rebuild expand on shared evaluator primitives. | [14](./14-evaluation-engine-design.md) | 1 day |
| 4.6 | Add recursion, fanout, and cross-namespace tests. | [72](./72-testing-verification-plan.md) | 1 day |
| 4.7 | Add criterion benchmarks and calibrate gates. | [71](./71-performance-budgets-design.md) | 1 day |

Exit criteria: M3 roadmap criteria pass.

## 8. Phase 5 - Lookup APIs

Closes M4.

| # | Task | Spec | Effort |
| --- | --- | --- | --- |
| 5.1 | Add lookup request/response types. | [15](./15-public-api-design.md) | 0.5 day |
| 5.2 | Implement `lookup_resources` from subject-side index candidates. | [14](./14-evaluation-engine-design.md) | 1 day |
| 5.3 | Implement `lookup_subjects` from resource-side index candidates. | [14](./14-evaluation-engine-design.md) | 1 day |
| 5.4 | Add duplicate suppression and result limits. | [14](./14-evaluation-engine-design.md) | 0.5 day |
| 5.5 | Add lookup integration tests and docs. | [72](./72-testing-verification-plan.md) | 1 day |

Exit criteria: M4 roadmap criteria pass.

## 9. Phase 6 - Release Hardening

Closes M5.

| # | Task | Spec | Effort |
| --- | --- | --- | --- |
| 6.1 | Pin Rust 2024 toolchain. | [60](./60-crates-features-design.md) | 0.5 day |
| 6.2 | Add crate root lint policy and forbid unsafe. | [60](./60-crates-features-design.md), [70](./70-security-design.md) | 0.5 day |
| 6.3 | Complete public docs and doctests. | [15](./15-public-api-design.md), [72](./72-testing-verification-plan.md) | 1 day |
| 6.4 | Remove retired modules or mark compatibility-only. | [15](./15-public-api-design.md) | 1 day |
| 6.5 | Run full gates and fix findings. | [72](./72-testing-verification-plan.md) | 1 day |

Exit criteria: M5 roadmap criteria pass.

## 10. Correctness of the Order

The order is correct because:

- schema validation blocks relationship validation
- relationship indexes block fast check and lookup
- snapshot publication blocks consistency tokens
- membership algebra blocks shared check/expand/lookup semantics
- benchmark gates are meaningful only after indexed store and evaluator exist

## 11. Cross-References

- Stakeholder roadmap: [90-local-engine-roadmap.md](./90-local-engine-roadmap.md)
- Key decisions: [99-key-decisions.md](./99-key-decisions.md)
- Verification gates: [72-testing-verification-plan.md](./72-testing-verification-plan.md)
