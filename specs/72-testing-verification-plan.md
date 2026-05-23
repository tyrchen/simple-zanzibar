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
| Compatibility | existing tests pass through `ZanzibarService` facade. |
| Security | no panics on malformed external input, no unchecked indexing in boundary modules. |
| Performance | criterion benchmarks for budgets in [71](./71-performance-budgets-design.md). |
| Memory | peak RSS checks for compact relationship store budgets in [16](./16-compact-relationship-store-design.md). |
| Snapshot artifact | save/load equivalence, corrupt file rejection, file size, load time, and load-time RSS for [17](./17-compact-snapshot-format-design.md). |

## 4. Command Gates

Every implementation phase closes only when these pass:

```text
cargo build
cargo test
cargo +nightly fmt --check
cargo clippy -- -D warnings -W clippy::pedantic
cargo audit
cargo deny check
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
snapshot_load_peak_rss/1m
snapshot_file_size/1m
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

Loaded snapshots must pass check, expand, lookup, and exact-consistency equivalence tests against a snapshot built from the same relationship set.

## 9. Cross-References

- <- Depends on: [70-security-design.md](./70-security-design.md), [71-performance-budgets-design.md](./71-performance-budgets-design.md)
- -> Consumed by: [90-local-engine-roadmap.md](./90-local-engine-roadmap.md), [91-local-engine-impl-plan.md](./91-local-engine-impl-plan.md)
- Memory layout: [16-compact-relationship-store-design.md](./16-compact-relationship-store-design.md)
- Snapshot artifact format: [17-compact-snapshot-format-design.md](./17-compact-snapshot-format-design.md)
