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

### M6 - Large Org Memory Efficiency

User-visible outcome: applications can load medium-to-large local org authorization datasets without multi-GiB resident memory while preserving the current microsecond-level check latency.

Specs touched: [12](./12-relationship-store-design.md), [13](./13-revision-consistency-design.md), [16](./16-compact-relationship-store-design.md), [71](./71-performance-budgets-design.md), [72](./72-testing-verification-plan.md).

Exit criteria:

- current service head and newest published snapshot share relationship storage instead of cloning it
- compatibility tuple-store mirror is cleared after schema publication
- relationship indexes use compact `Vec<RowId>` postings, not `BTreeSet<usize>`
- relationship rows use interned identifier ids on the hot path
- 1M-rule org authorization benchmark max RSS <= 400 MiB
- direct and inherited check latency budgets in [71](./71-performance-budgets-design.md) still pass

### M7 - Fast Compact Snapshot Load

User-visible outcome: applications can ship a prebuilt local authorization snapshot and load it quickly at startup without parsing relationship text or rebuilding all compact indexes from domain objects.

Specs touched: [13](./13-revision-consistency-design.md), [16](./16-compact-relationship-store-design.md), [17](./17-compact-snapshot-format-design.md), [71](./71-performance-budgets-design.md), [72](./72-testing-verification-plan.md).

Exit criteria:

- compact snapshots can be saved to a versioned `.szsnap` artifact
- snapshot files are treated as untrusted input and reject corrupt headers, sections, checksums, symbol ids, and posting ranges
- loaded snapshots produce equivalent check, expand, lookup, and exact consistency behavior to build-from-relationships snapshots
- pure 1M snapshot load benchmark is measured separately from relationship generation and Criterion harness time
- 1M uncompressed fast-load p95 <= 500 ms or the target is recalibrated with measured evidence
- 1M load-time max RSS <= 1.25x loaded steady-state RSS
- loaded 1M direct, inherited, and lookup latency budgets in [71](./71-performance-budgets-design.md) pass

### M8 - Trusted 200 ms Snapshot Load

User-visible outcome: applications that ship build-pipeline validated `.szsnap` artifacts can opt
into a trusted fast-load mode that targets sub-200 ms 1M-rule cold starts.

Specs touched: [17](./17-compact-snapshot-format-design.md), [18](./18-trusted-fast-snapshot-load-design.md), [70](./70-security-design.md), [71](./71-performance-budgets-design.md), [72](./72-testing-verification-plan.md).

Exit criteria:

- `.szsnap` v2 includes stable serialized symbol hash and lookup permutation sections
- `SnapshotValidationMode::Full` remains the default and keeps corrupt semantic-file rejection
- `SnapshotValidationMode::TrustedFastLoad` is explicit in public load options and documented as a build-pipeline trust boundary
- `SnapshotIntegrityMode::External` is explicit, restricted to trusted fast-load, and documented as requiring a prior content-address or signature proof
- trusted loaded snapshots produce equivalent check, expand, lookup, and exact consistency behavior for valid artifacts
- subsequent writes after trusted load preserve create/touch/delete uniqueness semantics
- `snapshot_load_trusted_fast/1m` Criterion upper estimate <= 200 ms with trusted fast-load and external integrity
- trusted loaded direct, inherited, and lookup latency budgets in [71](./71-performance-budgets-design.md) pass

### M9 - Complete Public API Surface

User-visible outcome: applications can use Simple Zanzibar as a complete local policy package:
load/save raw or zstd snapshots, import/export reviewable policy text, replace/delete schema
policy, and answer permission-audit queries without hand-enumerating schema relations.

Specs touched: [15](./15-public-api-design.md), [19](./19-public-api-completeness-design.md), [71](./71-performance-budgets-design.md), [72](./72-testing-verification-plan.md).

Exit criteria:

- raw and zstd snapshot save/load APIs round trip equivalent services
- `PolicyText` import/export round trips schema and relationships with deterministic sorted output
- `export_policy_files` writes `schema.zed` and grouped relationship files suitable for review
- schema replacement, namespace deletion, and relation deletion publish revisions only when existing relationships remain valid
- `lookup_permissions` and `lookup_object_permissions` return stable sorted audit results
- public API benchmarks record check, lookup, permission enumeration, policy export, and zstd snapshot costs
- full build, test, fmt, clippy, audit, and deny gates pass

## 3. Calendar Shape

One experienced Rust developer:

- M0: 1.5 to 2 weeks
- M1: 1.5 to 2 weeks
- M2: 1 week
- M3: 2 to 3 weeks
- M4: 1.5 to 2 weeks
- M5: 1 week
- M6: 2 to 3 weeks
- M7: 2 to 3 weeks
- M8: 1 week
- M9: 1 week

Total through M5: 8.5 to 11 weeks, assuming no persistent backend and no caveats.

Total through M6: 10.5 to 14 weeks.

Total through M7: 12.5 to 17 weeks.

Total through M8: 13.5 to 18 weeks.

Total through M9: 14.5 to 19 weeks.

## 4. Cross-References

- Paired engineer plan: [91-local-engine-impl-plan.md](./91-local-engine-impl-plan.md)
- Verification gates: [72-testing-verification-plan.md](./72-testing-verification-plan.md)
