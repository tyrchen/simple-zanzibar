# 90 - Roadmap: Local Zanzibar Engine

Status: draft v1
Owner: Simple Zanzibar
Last updated: 2026-05-23

## 1. Principles

- Every milestone leaves the crate green on the command gates in [72-testing-verification-plan.md](./72-testing-verification-plan.md).
- User-visible functionality lands only after its underlying contract is stable.
- Compatibility is maintained through a facade until v2 covers the legacy behaviour.
- Performance claims require benchmark evidence.

## 2. Milestones

### M0 - Schema-First Compatibility Engine

User-visible outcome: existing examples and tests still work, but requests run through a validated v2 schema model.

Specs touched: [10](./10-local-engine-data-model-design.md), [11](./11-schema-system-design.md), [15](./15-public-api-design.md), [72](./72-testing-verification-plan.md).

Exit criteria:

- legacy DSL compiles into v2 schema
- invalid relation references fail at schema application time
- existing check/expand tests pass through compatibility facade
- no production parser `unwrap()`/`expect()` remains

### M1 - Indexed Relationship Writes and Reads

User-visible outcome: relationship writes are batchable and direct checks no longer scan the full store.

Specs touched: [12](./12-relationship-store-design.md), [14](./14-evaluation-engine-design.md), [72](./72-testing-verification-plan.md).

Exit criteria:

- resource and subject indexes stay equivalent under property tests
- create/touch/delete/precondition semantics pass integration tests
- direct check benchmark proves indexed path over 100k relationships

### M2 - Snapshot Tokens and Deterministic Reads

User-visible outcome: write calls return consistency tokens; checks can run at exact snapshots.

Specs touched: [13](./13-revision-consistency-design.md), [15](./15-public-api-design.md), [72](./72-testing-verification-plan.md).

Exit criteria:

- exact snapshot tests prove old tokens do not observe later writes
- wrong datastore ID and expired revision tokens are rejected
- read path uses atomic snapshot acquisition

### M3 - Shared Graph Engine for Check and Expand

User-visible outcome: check and expand share typed schema, indexed store, revision snapshots, recursion policy, and membership algebra.

Specs touched: [14](./14-evaluation-engine-design.md), [71](./71-performance-budgets-design.md), [72](./72-testing-verification-plan.md).

Exit criteria:

- every expression variant has direct tests
- cross-namespace userset tests pass
- depth exceeded is distinguishable from denied
- performance budgets are either met or recalibrated with measured evidence

### M4 - Local Lookup APIs

User-visible outcome: applications can ask "which resources can this subject access?" and "which subjects can access this resource?" locally.

Specs touched: [14](./14-evaluation-engine-design.md), [15](./15-public-api-design.md), [72](./72-testing-verification-plan.md).

Exit criteria:

- `lookup_resources` and `lookup_subjects` are public and documented
- result limits are enforced
- duplicate suppression works
- lookup tests reuse check semantics

### M5 - Release Hardening

User-visible outcome: v2 is the default engine and ready for a crate release.

Specs touched: [60](./60-crates-features-design.md), [70](./70-security-design.md), [71](./71-performance-budgets-design.md), [72](./72-testing-verification-plan.md).

Exit criteria:

- Rust 2024 toolchain pinned
- docs and doctests complete
- strict clippy, audit, deny, and benchmarks pass
- legacy modules are removed or explicitly compatibility-only

## 3. Calendar Shape

One experienced Rust developer:

- M0: 1.5 to 2 weeks
- M1: 1.5 to 2 weeks
- M2: 1 week
- M3: 2 to 3 weeks
- M4: 1.5 to 2 weeks
- M5: 1 week

Total: 8.5 to 11 weeks, assuming no persistent backend and no caveats.

## 4. Cross-References

- Paired engineer plan: [91-local-engine-impl-plan.md](./91-local-engine-impl-plan.md)
- Verification gates: [72-testing-verification-plan.md](./72-testing-verification-plan.md)
