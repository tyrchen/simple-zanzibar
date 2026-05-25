# 72 - Testing and Verification Plan

Status: draft v1
Owner: Simple Zanzibar
Depends on: [70-security-design.md](./70-security-design.md), [71-performance-budgets-design.md](./71-performance-budgets-design.md)

## 1. Purpose

This plan defines how each phase proves correctness, safety, compatibility, and performance. It is the implementation exit contract.

## 2. Test Layers

```text
+-------------------------+
| doctests                |
| public API examples     |
+------------+------------+
             |
+------------v------------+
| integration tests       |
| check/expand/lookup     |
+------------+------------+
             |
+------------v------------+
| unit tests              |
| parser/store/evaluator  |
+------------+------------+
             |
+------------v------------+
| property tests          |
| indexes, algebra, parse |
+------------+------------+
             |
+------------v------------+
| benchmarks and gates    |
| criterion, audit, deny  |
+-------------------------+
```

## 3. Required Test Suites

| Suite | Minimum coverage |
| --- | --- |
| Domain parsing | valid and invalid relationship strings, identifier caps, charset rejection. |
| Schema validation | missing relations, duplicate definitions, invalid tuple-to-userset, removal with existing relationships. |
| Relationship store | create/touch/delete, preconditions, duplicate mutation rejection, index equivalence. |
| Revision consistency | latest read, exact read, expired token, wrong datastore ID, schema hash mismatch. |
| Evaluation | every expression variant, cross-namespace usersets, recursion cycles, depth exceeded, fanout limit. |
| Expand/lookup | shared evaluator semantics, duplicate suppression, result limits. |
| Public runtime | existing tests pass through `ZanzibarEngine`; no legacy mutable facade remains public. |
| Security | no panics on malformed external input, no unchecked indexing in boundary modules. |
| Performance | criterion benchmarks for budgets in [71](./71-performance-budgets-design.md). |
| Memory | peak RSS checks for compact relationship store budgets in [16](./16-compact-relationship-store-design.md). |
| Snapshot artifact | save/load equivalence, corrupt file rejection, file size, load time, and load-time RSS for [17](./17-compact-snapshot-format-design.md) and trusted fast-load coverage for [18](./18-trusted-fast-snapshot-load-design.md). |
| Public API completeness | zstd snapshot round trips, policy text import/export, schema replacement/deletion, permission enumeration, and public API benchmarks for [19](./19-public-api-completeness-design.md). |
| Concurrent runtime | lock-free read acquisition, writer actor lifecycle, failed-write atomicity, concurrent read/write behaviour, tenant isolation, and mixed workload benchmarks for [20](./20-concurrent-engine-runtime-design.md). |
| Structural performance optimization | prepared-check equivalence, ID-native recursion, streaming lookup, segmented-store properties, index-profile support, snapshot phase timers, and 1M write/read benchmarks for [21](./21-performance-optimization-design.md). |
| Production readiness | serde boundary rejection, policy-text-to-zstd-snapshot e2e flow, current README/API documentation, and Makefile production gate for [94](./94-production-readiness-review.md). |

## 4. Command Gates

Every implementation phase closes only when these pass:

```text
cargo build
cargo test --all-features
cargo +nightly fmt --check
cargo clippy --all-targets --all-features -- -D warnings -W clippy::pedantic
cargo audit
cargo deny check
RUSTDOCFLAGS="-D warnings" cargo doc --all-features --no-deps
```

The Makefile equivalent for release-hardening and production-readiness reviews is:

```text
make prod-ready-check
```

For boundary-module hardening phases, also run:

```text
cargo clippy -- -D warnings -W clippy::unwrap_used -W clippy::expect_used -W clippy::panic -W clippy::indexing_slicing
```

## 5. Fixtures

Fixtures live under `tests/fixtures/`:

- `schemas/minimal.zed`
- `schemas/cross_namespace.zed`
- `schemas/invalid_missing_relation.zed`
- `relationships/documents.txt`
- `relationships/groups.txt`
- `relationships/fanout_limit.txt`

## 6. Property Tests

Property tests cover:

- relationship string parse/display round trip
- random mutation sequences keep `HashSet`, resource index, and subject index equivalent
- set algebra laws for allowed/denied membership
- canonical schema hash stability under source definition order changes
- exact snapshot isolation across random write/read interleavings
- compact store query equivalence against a reference `HashSet<Relationship>` after random create/touch/delete batches
- tombstone compaction preserves all live relationships and removes deleted relationships
- compact snapshot save/load preserves query results for random compact stores

## 7. Memory Verification

Memory-sensitive phases add a release-mode measurement script or Makefile target rather than ad hoc shell notes. The target records:

- command line
- relationship count
- benchmark filter
- maximum resident set size
- peak memory footprint where the platform reports it
- commit SHA

Minimum required filters:

```text
building_blocks/relationship_parse
org_authorization/1k_rules/check_direct_group_viewer
org_authorization/100k_rules/check_direct_group_viewer
org_authorization/1m_rules/check_direct_group_viewer
```

The lightweight parse benchmark is the process/harness baseline. The org-rule measurements are compared against [71-performance-budgets-design.md § 3](./71-performance-budgets-design.md#3-initial-targets).

## 8. Snapshot Artifact Verification

Compact snapshot file format phases add release-mode benchmarks and corrupt-input tests before exposing public load APIs.

Required benchmark filters:

```text
snapshot_build_from_relationships/1m
snapshot_save_uncompressed/1m
snapshot_load_compact/1m
snapshot_load_trusted_fast/1m
snapshot_trusted_loaded_check_direct/1m
snapshot_trusted_loaded_check_inherited/1m
snapshot_trusted_loaded_lookup_resources/1m
snapshot_load_peak_rss/1m
snapshot_file_size/1m
snapshot_save_zstd/1m
snapshot_load_zstd/1m
snapshot_file_size_zstd/1m
public_api/check/100k
public_api/lookup_resources/100k
public_api/lookup_subjects/100k
public_api/lookup_permissions/100k
public_api/lookup_object_permissions/100k
public_api/export_policy_text/100k
public_api/snapshot_save_zstd/100k
public_api/snapshot_load_zstd/100k
concurrent_runtime/read_heavy_light_write
concurrent_runtime/read_heavy_medium_write_unbatched
concurrent_runtime/read_heavy_medium_write_batched
concurrent_runtime/read_heavy_heavy_write_unbatched
concurrent_runtime/read_heavy_heavy_write_batched
concurrent_runtime/tenant_sharded_heavy_write
realworld_authorization/1m_rules/check_doc_inherited_workspace_member
realworld_authorization/1m_rules/check_doc_denied_by_ban
realworld_authorization/1m_rules/mixed_read_workload
realworld_authorization/1m_rules/snapshot_load_compact
realworld_authorization/1m_rules/snapshot_load_trusted_fast
perf_optimization/writer_submit_queue_full
perf_optimization/check_prepared_1m
perf_optimization/lookup_subjects_streaming_1m
perf_optimization/lookup_resources_streaming_1m
perf_optimization/write_single_touch_1m
perf_optimization/write_mixed_batch_1m
perf_optimization/read_heavy_light_write_1m
perf_optimization/read_heavy_medium_write_unbatched_1m
perf_optimization/read_heavy_medium_write_batched_1m
perf_optimization/read_heavy_heavy_write_unbatched_1m
perf_optimization/read_heavy_heavy_write_batched_1m
snapshot_file_size_check_only/1m
```

Required corrupt-input tests:

- bad magic/version/header length
- duplicate or missing required section
- overlapping or out-of-bounds section
- checksum mismatch
- malformed UTF-8 symbol bytes
- invalid symbol id or row id
- unsorted index keys
- posting range outside the posting row id section
- malformed symbol lookup section for v2 artifacts
- external integrity accepted only with explicit trusted fast-load and never as the default

Loaded snapshots must pass check, expand, lookup, and exact-consistency equivalence tests against a
snapshot built from the same relationship set. Trusted fast-load snapshots must additionally prove
that subsequent create/touch/delete writes preserve relationship uniqueness semantics after the lazy
uniqueness index is built.

## 9. Public API Completeness Verification

Required tests for [19](./19-public-api-completeness-design.md):

- raw and zstd snapshot artifacts round trip through `ZanzibarEngine`;
- zstd load rejects decompressed payloads beyond `SnapshotLoadOptions::max_file_bytes`;
- `PolicyText` import/export preserves check, expand, lookup, and permission enumeration behavior;
- exported policy files are deterministic, sorted, and grouped by resource namespace;
- schema replacement, namespace deletion, and relation deletion publish revisions on success and
  leave the prior state observable after failed deletion;
- `lookup_permissions` and `lookup_object_permissions` cover direct, computed userset,
  tuple-to-userset, and exclusion behavior.

## 10. Concurrent Runtime Verification

Required tests for [20](./20-concurrent-engine-runtime-design.md):

- public API no longer exports the legacy mutable service facade;
- latest read APIs work while a writer actor is processing write traffic;
- failed relationship, schema, and policy writes publish no state;
- exact consistency tokens remain valid for retained snapshots and reject foreign tenant tokens;
- actor shutdown/drop does not leak writer threads in tests;
- `ZanzibarTenantShards` returns the same engine for the same tenant, different engines for
  different tenants, and isolates schema/relationship state by tenant.

## 11. Structural Performance Optimization Verification

Required tests for [21](./21-performance-optimization-design.md):

- writer submit queue-full behavior does not hold a sender mutex while blocked;
- full-load lazy uniqueness supports subsequent create/touch/delete and publishes no state on lazy
  uniqueness failure;
- prepared-check and ID-native evaluator paths are equivalent to current public `check`, including
  computed userset, tuple-to-userset, intersection, exclusion, cycles, and depth/fanout errors;
- lookup internals stream bounded candidates, reuse evaluation context state, and preserve result
  ordering/limits;
- segmented store random mutation sequences match a reference `HashSet<Relationship>`;
- checkpoint boundaries preserve exact-revision tokens and query equivalence;
- index profiles load/save round trip and return typed unsupported-operation errors for unsupported
  APIs instead of scanning.

Required benchmark target:

```text
make bench-perf-optimization
```

The target must be added when the `perf_optimization` benchmark binary lands.

## 12. Cross-References

- <- Depends on: [70-security-design.md](./70-security-design.md), [71-performance-budgets-design.md](./71-performance-budgets-design.md)
- -> Consumed by: [90-local-engine-roadmap.md](./90-local-engine-roadmap.md), [91-local-engine-impl-plan.md](./91-local-engine-impl-plan.md)
- Memory layout: [16-compact-relationship-store-design.md](./16-compact-relationship-store-design.md)
- Snapshot artifact format: [17-compact-snapshot-format-design.md](./17-compact-snapshot-format-design.md)
- Public API completeness: [19-public-api-completeness-design.md](./19-public-api-completeness-design.md)
- Performance optimization design: [21-performance-optimization-design.md](./21-performance-optimization-design.md)
