# 94 - Production Readiness Review

Status: draft v1
Owner: Simple Zanzibar
Last updated: 2026-05-25
Depends on: [15](./15-public-api-design.md), [19](./19-public-api-completeness-design.md), [20](./20-concurrent-engine-runtime-design.md), [70](./70-security-design.md), [71](./71-performance-budgets-design.md), [72](./72-testing-verification-plan.md)

## 1. Purpose

This review turns the production-readiness pass into an implementation contract. It focuses on the
four requested dimensions: test coverage, documentation freshness, public API ergonomics, and safety.
The target state is not a new feature phase; it is a hardening pass that keeps existing Zanzibar
semantics and performance budgets while closing concrete readiness gaps.

## 2. Review Scope

```text
                   ┌────────────────────────────────────────┐
                   │ Production readiness review            │
                   ├────────────────────────────────────────┤
                   │ 1. Tests: unit / integration / e2e /   │
                   │    benchmark coverage                  │
                   │ 2. Docs: README, specs, perf evidence  │
                   │ 3. Public API: validated DTOs and      │
                   │    discoverable verification commands  │
                   │ 4. Safety: hostile-input boundaries,   │
                   │    no unsafe, no production unwrap     │
                   └──────────────┬─────────────────────────┘
                                  │
                    fixes must preserve existing behavior
                                  │
                                  ▼
          ┌─────────────────────────────────────────────────┐
          │ Verification gate                               │
          │ cargo build / test --all-features / fmt /       │
          │ pedantic clippy / audit / deny / docs / smoke   │
          │ benchmarks                                      │
          └─────────────────────────────────────────────────┘
```

## 3. Findings and Fix Contract

| ID | Area | Finding | Fix shape | Exit evidence |
| --- | --- | --- | --- | --- |
| PRR-1 | Safety / serde | Public request DTOs derived `Deserialize` over raw `String` fields, so hostile JSON could build invalid `Object`, `Relation`, `User`, `LookupResourcesRequest.resource_type`, or `LookupSubjectsRequest.subject_type` values and defer rejection until later engine code. This violated [70](./70-security-design.md) and AGENTS.md boundary-validation rules. | Add validation methods to public DTOs, implement custom serde deserialization for domain-bearing DTOs, and validate requests before snapshot access in `ZanzibarEngine`. | New serde-boundary tests reject invalid object ids, resource types, and subject types before API execution. |
| PRR-2 | E2E tests | Existing tests were strong at unit/integration/property/snapshot levels but lacked one end-to-end public flow that starts from reviewable policy text, exercises exact consistency and Zanzibar lookup behavior, saves a zstd snapshot, loads it, and proves equivalent answers. | Add an e2e test that covers policy text import, exclusion semantics, tuple-to-userset inheritance, permission enumeration, zstd snapshot save/load, and equivalence after load. | `tests/e2e_prod_readiness_tests.rs` passes under `cargo test --all-features`. |
| PRR-3 | Documentation | README still described the older tuple-store architecture, `simple-zanzibar = "0.1.0"`, and hot-path `HashSet` storage, while the implementation now uses the concurrent engine, compact indexes, snapshot profiles, zstd, policy text, and Phase 15 perf evidence. | Rewrite README around the current public API, architecture, safety features, verification commands, and non-goals. | README no longer claims HashSet hot paths or missing benchmark support; it links the current performance report. |
| PRR-4 | Verification automation | The Makefile had build/test/fmt/clippy and benchmark targets, but no single production-readiness gate that included pedantic clippy, audit, deny, and docs. | Add discoverable `prod-ready-check`, `audit`, `deny`, `doc`, and focused `bench-prod-smoke` targets. | `make prod-ready-check` is the documented local gate; benchmark smoke target covers representative read and snapshot load filters. |

Known deferred performance backlog in [93](./93-improvements-review.md) remains valid and is not
reclassified as a release blocker by this review: default safe full snapshot load is still tracked as
an optimization item distinct from the trusted-fast cold-load path, which remains the production
startup path for pre-verified artifacts.

## 4. Test Coverage Assessment

| Layer | Current coverage | Readiness decision |
| --- | --- | --- |
| Unit | Domain validation, schema/evaluator helpers, request memo behavior. | Sufficient for current API after PRR-1 serde-boundary additions. |
| Integration | Check/expand/lookup semantics, consistency tokens, schema mutation atomicity, relationship preconditions. | Sufficient; PRR-2 adds a public e2e flow instead of duplicating existing integration cases. |
| Property / randomized | Relationship-store equivalence and compact snapshot random direct relationship round trips. | Sufficient for the compact in-memory store scope. |
| E2E | Previously scattered across public API and snapshot tests. | Add one explicit policy-to-zstd-snapshot e2e test. |
| Benchmarks | Synthetic org, real-world auth, concurrent runtime, public API, snapshot, section-size, and read-follow-up allocation harnesses. | Sufficient; PRR-4 adds a smoke target for fast regression checks while `make bench-all` remains the full gate. |

## 5. Verification Plan

Required before this review is complete:

1. `cargo build`
2. `cargo test --all-features`
3. `cargo +nightly fmt --check`
4. `cargo clippy --all-targets --all-features -- -D warnings -W clippy::pedantic`
5. `cargo audit`
6. `cargo deny check`
7. `RUSTDOCFLAGS="-D warnings" cargo doc --all-features --no-deps`
8. Focused benchmark smoke with representative read and snapshot-load filters.

## 6. Implementation Evidence

Local evidence captured on 2026-05-25 for this hardening pass:

| Command | Result |
| --- | --- |
| `CARGO_TARGET_DIR=target/codex-review make prod-ready-check` | Passed: build, 152 nextest tests, fmt, pedantic clippy, audit, deny, and rustdoc. `cargo deny` emitted pre-existing warning-level duplicate/unmatched-license diagnostics and exited successfully. |
| `CARGO_TARGET_DIR=target/codex-review PERF_SAMPLE_SIZE=10 make bench-prod-smoke` | Passed: `check_prepared_1m` high `4.4025 us`, `lookup_resources_streaming_1m` high `2.3653 ms`, `snapshot_load_trusted_fast/1m` high `179.98 ms` (still under the 200 ms trusted-fast gate). |
| `cargo bench --bench public_api -- public_api/check/100k --sample-size 10` | Passed: high `2.0057 us`, still well under the 10 us public check gate after request validation. |
| `cargo bench --bench public_api -- public_api/lookup_resources/100k --sample-size 10` | Passed: high `2.4869 ms`, still under the 10 ms public lookup gate. |

## 7. Cross-References

- Public API design: [15-public-api-design.md](./15-public-api-design.md)
- Public API completeness: [19-public-api-completeness-design.md](./19-public-api-completeness-design.md)
- Concurrent runtime: [20-concurrent-engine-runtime-design.md](./20-concurrent-engine-runtime-design.md)
- Security model: [70-security-design.md](./70-security-design.md)
- Performance budgets: [71-performance-budgets-design.md](./71-performance-budgets-design.md)
- Verification plan: [72-testing-verification-plan.md](./72-testing-verification-plan.md)
- Deferred findings: [93-improvements-review.md](./93-improvements-review.md)
