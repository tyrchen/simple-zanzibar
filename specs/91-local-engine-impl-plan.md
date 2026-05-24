# 91 - Implementation Plan: Local Zanzibar Engine

Status: draft v1
Owner: Simple Zanzibar
Last updated: 2026-05-24

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
| 1.5 | Wire legacy mutable facade through v2 schema where possible. | [15](./15-public-api-design.md) | 1 day |
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

## 10. Phase 7 - Compact Relationship Store

Closes M6.

| # | Task | Spec | Effort |
| --- | --- | --- | --- |
| 7.1 | Add memory measurement Makefile target for org authorization RSS baselines. | [71](./71-performance-budgets-design.md), [72](./72-testing-verification-plan.md) | 0.5 day |
| 7.2 | Remove duplicate relationship-store clone during snapshot publication. | [13](./13-revision-consistency-design.md), [16](./16-compact-relationship-store-design.md) | 1 day |
| 7.3 | Drain and clear the compatibility tuple store after first schema publication; materialize legacy tuples only on demand. | [15](./15-public-api-design.md), [16](./16-compact-relationship-store-design.md) | 1 day |
| 7.4 | Replace `BTreeSet<usize>` posting lists with `Vec<RowId>` and live-row tombstones. | [12](./12-relationship-store-design.md), [16](./16-compact-relationship-store-design.md) | 2 days |
| 7.5 | Add delete compaction thresholds and property tests for tombstone-heavy workloads. | [16](./16-compact-relationship-store-design.md), [72](./72-testing-verification-plan.md) | 1.5 days |
| 7.6 | Add `IdentifierInterner`, compact row ids, and compact index keys. | [16](./16-compact-relationship-store-design.md) | 3 days |
| 7.7 | Move evaluator hot paths to borrowed `RelationshipRef<'_>` accessors and avoid owned relationship materialization. | [14](./14-evaluation-engine-design.md), [16](./16-compact-relationship-store-design.md) | 2 days |
| 7.8 | Re-run full latency and memory benchmarks at 1k, 100k, and 1M rules; recalibrate spec only with measured evidence. | [71](./71-performance-budgets-design.md) | 1 day |

Exit criteria:

- M6 roadmap criteria pass.
- Existing public API, compatibility, exact snapshot, lookup, expand, and property tests remain green.
- `cargo build --workspace --all-targets`, `cargo test --workspace --all-features`, `cargo +nightly fmt --check`, strict clippy, `cargo audit`, and `cargo deny check` pass.

## 11. Phase 8 - Compact Snapshot File Format

Closes M7.

| # | Task | Spec | Effort |
| --- | --- | --- | --- |
| 8.1 | Add pure snapshot build/save/load benchmarks and `bench-snapshot` / `bench-snapshot-memory` Makefile targets. | [17](./17-compact-snapshot-format-design.md), [71](./71-performance-budgets-design.md), [72](./72-testing-verification-plan.md) | 1 day |
| 8.2 | Implement deterministic uncompressed `.szsnap` writer with header, section directory, schema, symbols, rows, indexes, and BLAKE3 footer. | [17](./17-compact-snapshot-format-design.md), [60](./60-crates-features-design.md), [70](./70-security-design.md) | 3 days |
| 8.3 | Implement safe checked loader for fast-load sorted-array indexes; reject malformed untrusted files without panics. | [17](./17-compact-snapshot-format-design.md), [70](./70-security-design.md) | 4 days |
| 8.4 | Add equivalence tests, corrupt fixture tests, golden snapshot fixture, and exact-consistency tests after load and subsequent writes. | [13](./13-revision-consistency-design.md), [17](./17-compact-snapshot-format-design.md), [72](./72-testing-verification-plan.md) | 2 days |
| 8.5 | Add public `save_snapshot` / `load_snapshot` APIs and docs for artifact versioning, trust boundary, and token semantics. | [15](./15-public-api-design.md), [17](./17-compact-snapshot-format-design.md) | 1.5 days |
| 8.6 | Run 1k/100k/1M load, file-size, load-RSS, and loaded-query benchmarks; update performance evidence and recalibrate only with measured data. | [17](./17-compact-snapshot-format-design.md), [71](./71-performance-budgets-design.md) | 1 day |

Exit criteria:

- M7 roadmap criteria pass.
- Loaded snapshots are behaviorally equivalent to build-from-relationships snapshots for check, expand, lookup, and exact consistency.
- Corrupt snapshot files are rejected with typed errors and no panics.
- `snapshot_load_compact/1m` p95 <= 500 ms or target recalibration is documented with benchmark evidence.
- `cargo build --workspace --all-targets`, `cargo test --workspace --all-features`, `cargo +nightly fmt --check`, strict clippy including boundary lints, `cargo audit`, and `cargo deny check` pass.

## 12. Correctness of the Order

The order is correct because:

- schema validation blocks relationship validation
- relationship indexes block fast check and lookup
- snapshot publication blocks consistency tokens
- membership algebra blocks shared check/expand/lookup semantics
- benchmark gates are meaningful only after indexed store and evaluator exist
- compact storage is valuable only after the full indexed/snapshot/evaluator path exists and benchmark evidence identifies memory as the limiting resource
- compact snapshot serialization is valuable only after the in-memory compact representation is stable and memory evidence identifies cold load as the next bottleneck
- trusted fast-load is valuable only after the fully validating snapshot format exists and profiling identifies repeated semantic validation as the dominant cost
- structural optimization is valuable only after the complete public API and concurrent runtime exist, because otherwise benchmarks would optimize a provisional surface
- snapshot file-size optimization is valuable only after section-size evidence separates index bytes
  from row/symbol bytes
- read-performance refinement is valuable only after write amplification is removed, because
  otherwise mixed-read results conflate reader work with writer clone cost

## 12a. Phase 9 - Trusted Fast Snapshot Load

Closes M8.

| # | Task | Spec | Effort |
| --- | --- | --- | --- |
| 9.1 | Rev the pre-release `.szsnap` format to v2 and add serialized `symbol_hashes` and `symbol_lookup` sections. | [17](./17-compact-snapshot-format-design.md), [18](./18-trusted-fast-snapshot-load-design.md) | 1 day |
| 9.2 | Add `SnapshotValidationMode` and `SnapshotIntegrityMode` with safe defaults and explicit trusted/external public docs. | [18](./18-trusted-fast-snapshot-load-design.md), [15](./15-public-api-design.md) | 0.5 day |
| 9.3 | Keep full validation semantics while using v2 structural validation and symbol lookup validation. | [18](./18-trusted-fast-snapshot-load-design.md), [70](./70-security-design.md) | 1 day |
| 9.4 | Implement trusted fast-load row/index adoption and lazy relationship uniqueness construction on first write. | [18](./18-trusted-fast-snapshot-load-design.md), [16](./16-compact-relationship-store-design.md) | 1.5 days |
| 9.5 | Add trusted-mode equivalence, subsequent-write, and structural-corruption tests. | [18](./18-trusted-fast-snapshot-load-design.md), [72](./72-testing-verification-plan.md) | 1 day |
| 9.6 | Add trusted fast-load benchmarks and update measured performance evidence. | [18](./18-trusted-fast-snapshot-load-design.md), [71](./71-performance-budgets-design.md) | 1 day |

Exit criteria:

- M8 roadmap criteria pass.
- `SnapshotValidationMode::Full` remains the default and corrupt semantic-file tests still pass.
- `snapshot_load_trusted_fast/1m` Criterion upper estimate <= 200 ms on the reference machine with trusted fast-load and external integrity.
- trusted loaded query benchmarks pass the loaded-query budgets.
- `cargo build --workspace --all-targets`, `cargo test --workspace --all-features`, `cargo +nightly fmt --check`, strict clippy including boundary lints, `cargo audit`, and `cargo deny check` pass.

## 12b. Phase 10 - Public API Completeness

Closes M9.

| # | Task | Spec | Effort |
| --- | --- | --- | --- |
| 10.1 | Add zstd snapshot compression options, bounded decompression, and raw/zstd load-save tests. | [19](./19-public-api-completeness-design.md), [17](./17-compact-snapshot-format-design.md), [70](./70-security-design.md) | 1 day |
| 10.2 | Add `PolicyText`, policy import/export helpers, deterministic file export, and snapshot-from-policy helpers. | [19](./19-public-api-completeness-design.md), [15](./15-public-api-design.md) | 1.5 days |
| 10.3 | Add schema replacement, namespace deletion, and relation deletion APIs with atomic revalidation. | [19](./19-public-api-completeness-design.md), [11](./11-schema-system-design.md), [13](./13-revision-consistency-design.md) | 1 day |
| 10.4 | Add `lookup_permissions` and `lookup_object_permissions` request/response APIs on service and engine. | [19](./19-public-api-completeness-design.md), [14](./14-evaluation-engine-design.md), [15](./15-public-api-design.md) | 1 day |
| 10.5 | Add integration tests for zstd, policy round trips, schema deletion failure atomicity, permission enumeration, and engine wrappers. | [19](./19-public-api-completeness-design.md), [72](./72-testing-verification-plan.md) | 1 day |
| 10.6 | Add `public_api` Criterion benchmark target and Makefile target; run and publish results. | [19](./19-public-api-completeness-design.md), [71](./71-performance-budgets-design.md) | 1 day |

Exit criteria:

- M9 roadmap criteria pass.
- Existing snapshot full/trusted validation tests still pass.
- zstd load applies the configured byte cap to both compressed and decompressed bytes.
- Policy export/import is deterministic and behaviorally equivalent for check, expand, lookup, and permission enumeration.
- Public API benchmark results are posted to the PR comment.
- `cargo build --workspace --all-targets`, `cargo test --workspace --all-features`, `cargo +nightly fmt --check`, strict clippy including boundary lints, `cargo audit`, and `cargo deny check` pass.

## 13. Phase 11 - Concurrent Runtime and Tenant Shards

Closes M10.

| # | Task | Spec | Effort |
| --- | --- | --- | --- |
| 11.1 | Remove public legacy mutable facade and port examples/tests/benches to `ZanzibarEngine`. | [20](./20-concurrent-engine-runtime-design.md), [15](./15-public-api-design.md) | 1 day |
| 11.2 | Add immutable `EngineState` and move read APIs to `ArcSwapOption<EngineState>`. | [20](./20-concurrent-engine-runtime-design.md), [13](./13-revision-consistency-design.md) | 1 day |
| 11.3 | Add bounded single writer actor owning mutable writer state and publishing snapshots. | [20](./20-concurrent-engine-runtime-design.md) | 1.5 days |
| 11.4 | Add actor lifecycle, writer failure, failed-write atomicity, and concurrent read/write tests. | [20](./20-concurrent-engine-runtime-design.md), [72](./72-testing-verification-plan.md) | 1 day |
| 11.5 | Add `TenantId` and `ZanzibarTenantShards` with lock-free existing-tenant reads. | [20](./20-concurrent-engine-runtime-design.md) | 1 day |
| 11.6 | Add concurrent runtime Criterion benchmarks for read/write mix, batching, and tenant sharding. | [20](./20-concurrent-engine-runtime-design.md), [71](./71-performance-budgets-design.md) | 1 day |
| 11.7 | Run full correctness gates and post benchmark evidence to the PR. | [72](./72-testing-verification-plan.md) | 1 day |

Exit criteria:

- M10 roadmap criteria pass.
- Existing public snapshot, policy, schema, check, expand, lookup, and permission enumeration
  behavior remains equivalent through `ZanzibarEngine`.
- `cargo build --workspace --all-targets`, `cargo test --workspace --all-features`,
  `cargo +nightly fmt --check`, strict clippy including boundary lints,
  `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps`, `cargo audit`, and
  `cargo deny check` pass.

## 14. Phase 12 - Structural Performance Optimization

Closes M11.

| # | Task | Spec | Effort |
| --- | --- | --- | --- |
| 12.1 | Add `perf_optimization` benchmark harness, 1M write/mixed read baselines, snapshot phase timers, and `bench-perf-optimization` Makefile target. | [21](./21-performance-optimization-design.md), [71](./71-performance-budgets-design.md), [72](./72-testing-verification-plan.md) | 1 day |
| 12.2 | Remove writer submit mutex blocking, add sorted relation cache, add all-live row fast path, and switch full-load uniqueness to lazy retained state. | [20](./20-concurrent-engine-runtime-design.md), [21](./21-performance-optimization-design.md) | 2 days |
| 12.3 | Add prepared-check path and ID-native evaluator keys for recursive check, computed userset, and tuple-to-userset. | [14](./14-evaluation-engine-design.md), [21](./21-performance-optimization-design.md) | 3 days |
| 12.4 | Rework lookup internals to stream bounded candidates and reuse evaluation contexts without changing public response types. | [14](./14-evaluation-engine-design.md), [19](./19-public-api-completeness-design.md), [21](./21-performance-optimization-design.md) | 2 days |
| 12.5 | Optimize snapshot load/save with fixed section lookup, optional bounded parallel index decode, and streaming writer. | [17](./17-compact-snapshot-format-design.md), [18](./18-trusted-fast-snapshot-load-design.md), [21](./21-performance-optimization-design.md) | 3 days |
| 12.6 | Introduce internal segmented `StoreView` / `StoreDelta` publication with checkpoint thresholds and exact-revision retention. | [13](./13-revision-consistency-design.md), [16](./16-compact-relationship-store-design.md), [20](./20-concurrent-engine-runtime-design.md), [21](./21-performance-optimization-design.md) | 5 days |
| 12.7 | Add `IndexProfile::{Full, CheckOnly, CheckAndObjectAudit}` to runtime/snapshot options with typed unsupported-operation errors. | [17](./17-compact-snapshot-format-design.md), [19](./19-public-api-completeness-design.md), [21](./21-performance-optimization-design.md) | 3 days |
| 12.8 | Run full correctness gates, 1M perf benchmarks, RSS checks, and post detailed benchmark evidence to the PR. | [71](./71-performance-budgets-design.md), [72](./72-testing-verification-plan.md) | 1 day |

Exit criteria:

- M11 roadmap criteria pass.
- Public check, expand, lookup, permission enumeration, policy import/export, snapshot load/save,
  and exact consistency behavior remain equivalent unless an unsupported index profile is explicitly
  selected.
- Segmented store property tests pass against a reference relationship set for random mutation
  sequences and checkpoint boundaries.
- `snapshot_load_compact/1m`, `snapshot_load_trusted_fast/1m`, `snapshot_load_peak_rss/1m`, 1M
  read latency, 1M write latency, concurrent runtime, and index-profile size/RSS benchmarks are
  recorded in [71](./71-performance-budgets-design.md).
- `cargo build --workspace --all-targets`, `cargo test --workspace --all-features`,
  `cargo +nightly fmt --check`, strict clippy including boundary lints,
  `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps`, `cargo audit`, and
  `cargo deny check` pass.

## 15. Phase 13 - Snapshot File Size Optimization

Closes M12.

| # | Task | Spec | Effort |
| --- | --- | --- | --- |
| 13.1 | Keep `snapshot_section_size` benchmark and record baseline section/index group bytes. | [22](./22-snapshot-file-size-optimization-design.md), [71](./71-performance-budgets-design.md) | 0.5 day |
| 13.2 | Add v3 posting overflow delta-varint encoding with corrupt-varint and monotonicity tests. | [22](./22-snapshot-file-size-optimization-design.md), [17](./17-compact-snapshot-format-design.md), [70](./70-security-design.md) | 3 days |
| 13.3 | Add singleton/multi-posting split for groups where section-size evidence justifies it. | [22](./22-snapshot-file-size-optimization-design.md), [16](./16-compact-relationship-store-design.md) | 3 days |
| 13.4 | Add group-specific key width/prefix encoding without changing profile support semantics. | [22](./22-snapshot-file-size-optimization-design.md), [17](./17-compact-snapshot-format-design.md) | 3 days |
| 13.5 | Evaluate row and symbol width encoding after index compression; land only if section-size and load benchmarks justify it. | [22](./22-snapshot-file-size-optimization-design.md), [71](./71-performance-budgets-design.md) | 2 days |
| 13.6 | Run snapshot load, trusted load, section-size, zstd size, RSS, and full correctness gates. | [22](./22-snapshot-file-size-optimization-design.md), [72](./72-testing-verification-plan.md) | 1 day |

Exit criteria:

- M12 roadmap criteria pass.
- v3 reader rejects malformed encodings with typed errors and no panics.
- `Full`, `CheckOnly`, and `CheckAndObjectAudit` keep their documented operation support.
- `make bench-snapshot-section-size` evidence is recorded in [71](./71-performance-budgets-design.md).
- `cargo build --workspace --all-targets`, `cargo test --workspace --all-features`,
  `cargo +nightly fmt --check`, strict clippy including boundary lints,
  `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps`, `cargo audit`, and
  `cargo deny check` pass.

## 16. Phase 14 - Read Performance Refinement

Closes M13.

| # | Task | Spec | Effort |
| --- | --- | --- | --- |
| 14.1 | Capture profiles/counters for inherited check, mixed read, delta segment scans, and tombstone checks. | [23](./23-read-performance-optimization-design.md), [71](./71-performance-budgets-design.md) | 1 day |
| 14.2 | Compile relation ids into schema expression nodes and remove recursive public relation materialization. | [14](./14-evaluation-engine-design.md), [23](./23-read-performance-optimization-design.md) | 3 days |
| 14.3 | Add segment-native lookup plans for checkpoint plus bounded deltas. | [16](./16-compact-relationship-store-design.md), [20](./20-concurrent-engine-runtime-design.md), [23](./23-read-performance-optimization-design.md) | 3 days |
| 14.4 | Add generation-counter reusable evaluation contexts for lookup verification loops. | [14](./14-evaluation-engine-design.md), [23](./23-read-performance-optimization-design.md) | 2 days |
| 14.5 | Add exact-proof shortcuts one expression family at a time with adversarial correctness tests. | [14](./14-evaluation-engine-design.md), [23](./23-read-performance-optimization-design.md), [72](./72-testing-verification-plan.md) | 2 days |
| 14.6 | Run realworld, perf-optimization, snapshot-profile, and full correctness gates. | [71](./71-performance-budgets-design.md), [72](./72-testing-verification-plan.md) | 1 day |

Exit criteria:

- M13 roadmap criteria pass.
- Read behavior remains equivalent for check, expand, lookup, permission enumeration, exact tokens,
  and unsupported index profiles.
- `realworld_authorization/1m_rules/mixed_read_workload` upper estimate is <= 55 us or the target is
  recalibrated with profile evidence.
- `perf_optimization/check_prepared_1m`, streaming lookup, and read-heavy write benchmarks regress
  by no more than the gates in [23](./23-read-performance-optimization-design.md).
- `cargo build --workspace --all-targets`, `cargo test --workspace --all-features`,
  `cargo +nightly fmt --check`, strict clippy including boundary lints,
  `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps`, `cargo audit`, and
  `cargo deny check` pass.

## 17. Cross-References

- Stakeholder roadmap: [90-local-engine-roadmap.md](./90-local-engine-roadmap.md)
- Key decisions: [99-key-decisions.md](./99-key-decisions.md)
- Verification gates: [72-testing-verification-plan.md](./72-testing-verification-plan.md)
- Compact store design: [16-compact-relationship-store-design.md](./16-compact-relationship-store-design.md)
- Compact snapshot format: [17-compact-snapshot-format-design.md](./17-compact-snapshot-format-design.md)
- Public API completeness: [19-public-api-completeness-design.md](./19-public-api-completeness-design.md)
- Concurrent engine runtime: [20-concurrent-engine-runtime-design.md](./20-concurrent-engine-runtime-design.md)
- Performance optimization design: [21-performance-optimization-design.md](./21-performance-optimization-design.md)
- Snapshot file-size optimization design: [22-snapshot-file-size-optimization-design.md](./22-snapshot-file-size-optimization-design.md)
- Read performance optimization design: [23-read-performance-optimization-design.md](./23-read-performance-optimization-design.md)
