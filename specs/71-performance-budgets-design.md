# 71 - Performance Budgets Design

Status: draft v1
Owner: Simple Zanzibar
Depends on: [12-relationship-store-design.md](./12-relationship-store-design.md), [14-evaluation-engine-design.md](./14-evaluation-engine-design.md), [60-crates-features-design.md](./60-crates-features-design.md)

## 1. Purpose

This spec sets measurable performance goals and benchmark gates. The first implementation phase must establish baselines before gates become blocking, but the architecture is designed around indexed reads and lock-free snapshot access.

## 2. Benchmark Matrix

Datasets:

| Dataset | Relationships | Shape |
| --- | ---: | --- |
| D1 | 10k | direct user grants, 10 object types |
| D2 | 100k | direct plus group usersets, 100 groups |
| D3 | 1M | mixed direct, userset, tuple-to-userset |
| D4 | 100k | adversarial fanout near configured limits |

Operations:

- direct `check`
- one-hop userset `check`
- tuple-to-userset `check`
- `expand` for bounded relation
- `lookup_resources` with 100, 1k, and 10k candidates
- exact-snapshot read after write
- schema compile/validate for small and large schemas
- max resident set size for the `org_authorization` 1k, 100k, and 1M datasets

## 3. Initial Targets

Targets are measured on release builds with criterion after Phase 0 calibration:

| Operation | Dataset | Initial target |
| --- | --- | ---: |
| direct check | D2 | p95 <= 10 us |
| one-hop userset check | D2 | p95 <= 50 us |
| tuple-to-userset check | D3 | p95 <= 250 us |
| latest snapshot acquisition | all | p95 <= 1 us |
| exact token validation | all retained snapshots | p95 <= 5 us |
| lookup 1k resources | D3 | p95 <= 10 ms |
| relationship touch write batch of 100 | D2 | p95 <= 2 ms |

Memory targets after [16-compact-relationship-store-design.md](./16-compact-relationship-store-design.md) lands:

| Dataset | Operation filter | Current measured max RSS | Target max RSS |
| --- | --- | ---: | ---: |
| 1k org rules | `org_authorization/1k_rules/check_direct_group_viewer` | 12.8 MiB | <= 16 MiB |
| 100k org rules | `org_authorization/100k_rules/check_direct_group_viewer` | 324 MiB | <= 80 MiB |
| 1M org rules | `org_authorization/1m_rules/check_direct_group_viewer` | 3.12 GiB | <= 400 MiB |

The target is steady-state process RSS measured with the release benchmark binary and `/usr/bin/time -l` on macOS, using the lightweight relationship-parse benchmark as the harness baseline. Linux CI may use `/usr/bin/time -v` and record `Maximum resident set size`.

If Phase 0 proves a target unrealistic on the reference machine, update this spec and [99-key-decisions.md](./99-key-decisions.md) with measured data before implementation proceeds.

## 3.1 M6 Compact Store Measurements

Measured on 2026-05-23 on the reference macOS machine with `make bench-org-memory`.
The org authorization cases build a single schema-backed compact `PublishedSnapshot` and then run
the Criterion operation filter under `/usr/bin/time -l`, so the RSS result reflects the compact
snapshot/evaluator shape rather than legacy tuple migration or multi-revision write history.

| Dataset | Operation filter | Pre-M6 max RSS | M6 max RSS | M6 peak footprint | Target |
| --- | --- | ---: | ---: | ---: | ---: |
| harness baseline | `building_blocks/relationship_parse` | 9.7 MiB | 16.0 MiB | 14.4 MiB | n/a |
| 1k org rules | `org_authorization/1k_rules/check_direct_group_viewer` | 12.8 MiB | 16.0 MiB | 13.9 MiB | <= 16 MiB |
| 100k org rules | `org_authorization/100k_rules/check_direct_group_viewer` | 324 MiB | 71.8 MiB | 29.3 MiB | <= 80 MiB |
| 1M org rules | `org_authorization/1m_rules/check_direct_group_viewer` | 3.12 GiB | 368 MiB | 207 MiB | <= 400 MiB |

Full `cargo bench --bench org_authorization -- --sample-size 10` after M6 shows the memory
reduction trades some CPU in the compact indexes. The 1M direct check remains in the same
microsecond range at 2.71 us, while inherited and lookup cases are still within the original
budgets: inherited folder viewer is 6.61 us and lookup resources is 3.42 ms.

## 3.2 Compact Snapshot Load Targets

After [17-compact-snapshot-format-design.md](./17-compact-snapshot-format-design.md) lands,
the benchmark matrix expands from steady-state query latency to cold-load behavior.

| Operation | Dataset | Initial target |
| --- | --- | ---: |
| save uncompressed compact snapshot | 1M org rules | Criterion upper estimate <= 1.5 s |
| load uncompressed compact snapshot, fast-load profile | 1M org rules | Criterion upper estimate <= 700 ms |
| load-time max RSS | 1M org rules | <= 1.25x loaded steady-state RSS |
| uncompressed snapshot file size | 1M org rules | <= 2x loaded steady-state RSS |
| direct check after load | 1M org rules | Criterion upper estimate <= 10 us |
| inherited check after load | 1M org rules | Criterion upper estimate <= 25 us |
| lookup resources after load | 1M org rules | Criterion upper estimate <= 10 ms |
| trusted fast-load compact snapshot (`TrustedFastLoad + External`) | 1M org rules | Criterion upper estimate <= 200 ms |

The current 1M `org_authorization/1m_rules/check_direct_group_viewer` wall time of roughly
2.32 s is not a load benchmark. It includes process startup, schema parse/compile, generated
relationship construction, compact snapshot construction, scenario validation, Criterion warmup,
measurement, and analysis. Phase M7 must add pure load benchmarks before claiming a load-speed
improvement.

## 3.3 M7 Compact Snapshot Measurements

Measured on 2026-05-23 on the reference macOS machine with the new pure snapshot benchmark
harness. Criterion values below are reported estimates or confidence intervals, not extracted p95
samples. The load measurement uses an already-created local `.szsnap` file and excludes
relationship generation, schema authoring, compact-store construction, and snapshot save time.

| Operation | Dataset | Measurement | Target status |
| --- | --- | ---: | --- |
| `snapshot_build_from_relationships/1m` | 1M org rules | 2.68 s | recorded baseline |
| `snapshot_save_uncompressed/1m` | 1M org rules | 0.44 s | passes <= 1.5 s |
| `snapshot_load_compact/1m` | 1M org rules | 0.58 s | passes <= 700 ms |
| `snapshot_file_size/1m` | 1M org rules | 112,182,029 bytes | passes <= 2x loaded RSS |
| `snapshot_load_peak_rss/1m` | 1M org rules | 402,259,968-byte max RSS; 392,446,600-byte peak footprint | passes <= 1.25x M6 loaded RSS |
| `snapshot_loaded_check_direct/1m` | 1M org rules | 2.99 us | passes <= 10 us |
| `snapshot_loaded_check_inherited/1m` | 1M org rules | 7.09 us | passes <= 25 us |
| `snapshot_loaded_lookup_resources/1m` | 1M org rules | 3.69 ms | passes <= 10 ms |

M8 revised the pre-release v2 file layout from interleaved `(hash, symbol_id)` lookup rows to
separate `symbol_hashes` and sorted `symbol_lookup` id permutation sections. This recovers the
default full-load path while giving trusted load a compact query table:

| Operation | Dataset | Measurement | Target status |
| --- | --- | ---: | --- |
| `snapshot_load_compact/1m` full mode | 1M org rules | `[575.82 ms, 580.38 ms, 585.11 ms]` | passes <= 700 ms; no meaningful regression versus M7 |
| `snapshot_file_size/1m` | 1M org rules | 124,422,241 bytes | recorded v2 size |
| `snapshot_loaded_check_direct/1m` | 1M org rules | `[2.9493 us, 2.9625 us, 2.9746 us]` | passes <= 10 us |
| `snapshot_loaded_check_inherited/1m` | 1M org rules | `[7.0179 us, 7.0895 us, 7.1458 us]` | passes <= 25 us |
| `snapshot_loaded_lookup_resources/1m` | 1M org rules | `[3.4836 ms, 3.7007 ms, 3.8729 ms]` | passes <= 10 ms |

The initial 500 ms fast-load target was optimistic for the first safe checked loader because the
loader validates every serialized index posting against compact rows and rejects incomplete or
mis-keyed indexes. The M7 gate is recalibrated to a Criterion upper estimate <= 700 ms for the
first version. A future
optimization pass can attempt to recover the original <= 500 ms target by making index validation
single-pass over grouped row ids or by adding a trusted-writer validation mode, but the default
untrusted-file loader keeps full validation.

## 3.4 Trusted Fast-Load Target

[18-trusted-fast-snapshot-load-design.md](./18-trusted-fast-snapshot-load-design.md) adds an
explicit trusted load mode for build-pipeline artifacts. Its gate is
`snapshot_load_trusted_fast/1m` Criterion upper estimate <= 200 ms while the default full loader
keeps the <= 700 ms gate. The benchmark uses `SnapshotValidationMode::TrustedFastLoad` with
`SnapshotIntegrityMode::External`; deployments choosing `Checksum` keep an in-process BLAKE3 rehash
and are expected to land just above the hard 200 ms gate. Trusted loaded direct/inherited/lookup
benchmarks must still satisfy the loaded-query budgets in § 3.2.

2026-05-23 evidence:

| Operation | Dataset | Measurement | Target status |
| --- | --- | ---: | --- |
| `snapshot_load_trusted_fast/1m` | 1M org rules | `[151.06 ms, 152.23 ms, 153.35 ms]` | passes <= 200 ms |
| `snapshot_trusted_loaded_check_direct/1m` | 1M org rules | `[3.0610 us, 3.1232 us, 3.1971 us]` | passes <= 10 us |
| `snapshot_trusted_loaded_check_inherited/1m` | 1M org rules | `[7.2171 us, 7.2732 us, 7.3198 us]` | passes <= 25 us |
| `snapshot_trusted_loaded_lookup_resources/1m` | 1M org rules | `[3.8150 ms, 3.9764 ms, 4.1401 ms]` | passes <= 10 ms |

## 3.5 Public API Completeness Benchmarks

[19-public-api-completeness-design.md](./19-public-api-completeness-design.md) adds a public API
benchmark harness that measures the crate-facing surface, including zstd snapshot wrappers and
policy text export.

| Operation | Dataset | Target |
| --- | --- | ---: |
| `public_api/check/100k` | 100k org rules | Criterion upper estimate <= 10 us |
| `public_api/expand/100k` | 100k org rules | recorded baseline |
| `public_api/lookup_resources/100k` | 100k org rules | Criterion upper estimate <= 10 ms |
| `public_api/lookup_subjects/100k` | 100k org rules | Criterion upper estimate <= 10 ms |
| `public_api/lookup_permissions/100k` | 100k org rules | Criterion upper estimate <= 250 us |
| `public_api/lookup_object_permissions/100k` | 100k org rules | Criterion upper estimate <= 25 ms |
| `public_api/write_relationships/1k_batch` | 100k org rules base | recorded baseline |
| `public_api/export_policy_text/100k` | 100k org rules | recorded baseline |
| `public_api/snapshot_save_zstd/100k` | 100k org rules | recorded baseline |
| `public_api/snapshot_load_zstd/100k` | 100k org rules | recorded baseline |

The zstd numbers describe storage/distribution tradeoffs. They do not replace the trusted raw
snapshot fast-load gate in § 3.4.

2026-05-23 evidence:

| Operation | Dataset | Measurement | Target status |
| --- | --- | ---: | --- |
| `public_api/apply_schema/small` | small schema | `[40.358 us, 40.742 us, 41.332 us]` | recorded baseline |
| `public_api/replace_schema/small` | small schema | `[40.574 us, 41.285 us, 42.260 us]` | recorded baseline |
| `public_api/delete_relation/small` | small schema | `[2.7558 us, 2.8226 us, 2.8841 us]` | recorded baseline |
| `public_api/delete_namespace/small` | small schema | `[2.3846 us, 2.4320 us, 2.4550 us]` | recorded baseline |
| `public_api/check/100k` | 100k org rules | `[2.8479 us, 2.8662 us, 2.8904 us]` | passes <= 10 us |
| `public_api/expand/100k` | 100k org rules | `[4.6743 us, 4.7234 us, 4.8001 us]` | recorded baseline |
| `public_api/lookup_resources/100k` | 100k org rules | `[3.2315 ms, 3.2529 ms, 3.2751 ms]` | passes <= 10 ms |
| `public_api/lookup_subjects/100k` | 100k org rules | `[6.4236 us, 6.4700 us, 6.5226 us]` | passes <= 10 ms |
| `public_api/lookup_permissions/100k` | 100k org rules | `[15.683 us, 15.799 us, 15.917 us]` | passes <= 250 us |
| `public_api/lookup_object_permissions/100k` | 100k org rules | `[14.019 us, 14.090 us, 14.148 us]` | passes <= 25 ms |
| `public_api/write_relationships/1k_batch` | 100k org rules base | `[7.3189 ms, 7.9381 ms, 8.5944 ms]` | recorded baseline |
| `public_api/apply_policy_text/1k` | 1k org rules | `[1.0222 ms, 1.0575 ms, 1.1059 ms]` | recorded baseline |
| `public_api/export_policy_text/100k` | 100k org rules | `[41.733 ms, 42.442 ms, 43.150 ms]` | recorded baseline |
| `public_api/export_policy_files/1k` | 1k org rules | `[1.1863 ms, 1.2307 ms, 1.2975 ms]` | recorded baseline |
| `public_api/snapshot_save_uncompressed/100k` | 100k org rules | `[49.547 ms, 50.476 ms, 51.422 ms]` | recorded baseline |
| `public_api/snapshot_load_uncompressed/100k` | 100k org rules | `[52.936 ms, 53.320 ms, 53.705 ms]` | recorded baseline |
| `public_api/snapshot_save_zstd/100k` | 100k org rules | `[64.139 ms, 64.937 ms, 65.745 ms]` | recorded baseline |
| `public_api/snapshot_load_zstd/100k` | 100k org rules | `[62.064 ms, 63.144 ms, 64.369 ms]` | recorded baseline |

Focused 1M regression checks after the public API additions:

| Operation | Dataset | Measurement | Target status |
| --- | --- | ---: | --- |
| `snapshot_load_compact/1m` full mode | 1M org rules | `[574.45 ms, 578.91 ms, 583.77 ms]` | passes <= 700 ms; no detected regression |
| `snapshot_load_trusted_fast/1m` | 1M org rules | `[138.30 ms, 139.35 ms, 140.46 ms]` | passes <= 200 ms; improved on this run |
| `snapshot_loaded_check_direct/1m` | 1M org rules | `[2.9807 us, 2.9895 us, 3.0075 us]` | passes <= 10 us; change within noise |
| `snapshot_trusted_loaded_check_direct/1m` | 1M org rules | `[3.0595 us, 3.0873 us, 3.1159 us]` | passes <= 10 us; no detected regression |

## 3.6 Concurrent Runtime Benchmarks

[20-concurrent-engine-runtime-design.md](./20-concurrent-engine-runtime-design.md) adds a mixed
read/write benchmark suite. The first implementation records evidence rather than enforcing hard
gates because write throughput depends heavily on caller batching and tenant partitioning.

| Operation | Dataset | Target |
| --- | --- | ---: |
| `concurrent_runtime/read_heavy_light_write` | 100k base + small batches | record read ops/s and write p95 |
| `concurrent_runtime/read_heavy_medium_write_unbatched` | 100k base + single writes | record read ops/s and write p95 |
| `concurrent_runtime/read_heavy_medium_write_batched` | same logical writes batched by 100 | record read ops/s and write p95 |
| `concurrent_runtime/read_heavy_heavy_write_unbatched` | 100k base + sustained single writes | record read ops/s and write p95 |
| `concurrent_runtime/read_heavy_heavy_write_batched` | same logical writes batched by 100 or 1k | record read ops/s and write p95 |
| `concurrent_runtime/tenant_sharded_heavy_write` | same logical write volume split across tenants | record aggregate write ops/s |

## 4. Design Constraints

- No full relationship-store scans in direct `check`.
- No read-path mutex.
- No clone of all matched relationships for hot checks.
- No string parsing inside the evaluator hot loop.
- Bounded fanout at each recursive step.
- Benchmark-only counters can be behind `bench-internals`.
- No duplicate relationship-store ownership between service head and newest published snapshot.
- No compatibility tuple-store mirror after schema publication.
- No `BTreeSet` posting lists in the compact store.
- No hot-path materialization of owned `Relationship` values during `check`.
- No service-level `RwLock` on public read APIs after [20](./20-concurrent-engine-runtime-design.md).
- Write throughput guidance must distinguish unbatched, batched, and tenant-sharded cases.

## 5. Profiling Rules

- Optimize only after a failing benchmark or profile evidence.
- Use `criterion` for repeatable microbenchmarks.
- Use `samply` or `cargo flamegraph` for CPU profiles when a benchmark misses by more than 20 percent.
- Keep allocation counts visible for direct and one-hop checks.
- Record peak RSS for large org benchmarks whenever store representation changes.

## 6. Cross-References

- <- Depends on: [12-relationship-store-design.md](./12-relationship-store-design.md), [14-evaluation-engine-design.md](./14-evaluation-engine-design.md), [60-crates-features-design.md](./60-crates-features-design.md)
- -> Consumed by: [72-testing-verification-plan.md](./72-testing-verification-plan.md), [91-local-engine-impl-plan.md](./91-local-engine-impl-plan.md)
- Memory layout: [16-compact-relationship-store-design.md](./16-compact-relationship-store-design.md)
- Related research: [../docs/research/study-spicedb.md § Query Filters and Indexes](../docs/research/study-spicedb.md#query-filters-and-indexes)
