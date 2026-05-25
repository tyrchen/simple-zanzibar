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
| `public_api/apply_schema/small` | small schema | `[64.222 us, 65.553 us, 66.552 us]` | recorded actor-backed API baseline |
| `public_api/replace_schema/small` | small schema | `[79.059 us, 81.720 us, 83.377 us]` | recorded actor-backed API baseline |
| `public_api/delete_relation/small` | small schema | `[45.458 us, 47.419 us, 49.389 us]` | recorded actor-backed API baseline |
| `public_api/delete_namespace/small` | small schema | `[46.536 us, 47.614 us, 49.294 us]` | recorded actor-backed API baseline |
| `public_api/check/100k` | 100k org rules | `[2.7956 us, 2.8265 us, 2.8615 us]` | passes <= 10 us |
| `public_api/expand/100k` | 100k org rules | `[4.6261 us, 4.6535 us, 4.7004 us]` | recorded baseline |
| `public_api/lookup_resources/100k` | 100k org rules | `[3.2256 ms, 3.2559 ms, 3.2761 ms]` | passes <= 10 ms |
| `public_api/lookup_subjects/100k` | 100k org rules | `[6.3363 us, 6.4295 us, 6.4977 us]` | passes <= 10 ms |
| `public_api/lookup_permissions/100k` | 100k org rules | `[15.554 us, 15.681 us, 15.921 us]` | passes <= 250 us |
| `public_api/lookup_object_permissions/100k` | 100k org rules | `[13.676 us, 13.794 us, 13.928 us]` | passes <= 25 ms |
| `public_api/write_relationships/1k_batch` | 100k org rules base | `[7.3734 ms, 7.9690 ms, 8.5949 ms]` | recorded baseline |
| `public_api/apply_policy_text/1k` | 1k org rules | `[1.2749 ms, 1.3526 ms, 1.4315 ms]` | recorded baseline |
| `public_api/export_policy_text/100k` | 100k org rules | `[38.939 ms, 39.808 ms, 40.893 ms]` | recorded baseline |
| `public_api/export_policy_files/1k` | 1k org rules | `[1.0172 ms, 1.0568 ms, 1.0931 ms]` | recorded baseline |
| `public_api/snapshot_save_uncompressed/100k` | 100k org rules | `[47.025 ms, 47.823 ms, 48.712 ms]` | recorded baseline |
| `public_api/snapshot_load_uncompressed/100k` | 100k org rules | `[50.558 ms, 51.390 ms, 52.279 ms]` | recorded baseline |
| `public_api/snapshot_save_zstd/100k` | 100k org rules | `[62.498 ms, 63.345 ms, 64.432 ms]` | recorded baseline |
| `public_api/snapshot_load_zstd/100k` | 100k org rules | `[60.120 ms, 60.494 ms, 60.797 ms]` | recorded baseline |

Focused 1M regression checks after the public API additions:

| Operation | Dataset | Measurement | Target status |
| --- | --- | ---: | --- |
| `snapshot_build_from_relationships/1m` | 1M org rules | `[2.7622 s, 2.7858 s, 2.8144 s]` | recorded default full-build baseline |
| `snapshot_save_uncompressed/1m` | 1M org rules | `[541.60 ms, 555.85 ms, 569.02 ms]` | passes <= 1.5 s |
| `snapshot_load_compact/1m` full mode | 1M org rules | `[555.68 ms, 559.30 ms, 563.48 ms]` | passes <= 700 ms; no detected regression |
| `snapshot_load_trusted_fast/1m` | 1M org rules | `[135.78 ms, 137.06 ms, 138.67 ms]` | passes <= 200 ms |
| `snapshot_loaded_check_direct/1m` | 1M org rules | `[3.0067 us, 3.0236 us, 3.0453 us]` | passes <= 10 us |
| `snapshot_loaded_check_inherited/1m` | 1M org rules | `[7.1645 us, 7.2265 us, 7.2778 us]` | passes <= 25 us |
| `snapshot_loaded_lookup_resources/1m` | 1M org rules | `[3.8731 ms, 4.0003 ms, 4.2153 ms]` | passes <= 10 ms |
| `snapshot_file_size/1m` | 1M org rules | `124,422,241 bytes` | recorded v2 size |
| `snapshot_load_peak_rss/1m` | 1M org rules | `436,076,544-byte max RSS; 404,849,384-byte peak footprint` | passes <= 1.25x loaded RSS |
| `snapshot_save_zstd/1m` | 1M org rules | `[641.69 ms, 644.70 ms, 647.67 ms]` | distribution-size baseline |
| `snapshot_load_zstd/1m` | 1M org rules | `[625.35 ms, 628.92 ms, 632.45 ms]` | direct compressed-load baseline |
| `snapshot_file_size_zstd/1m` | 1M org rules | `33,162,371 bytes` | 26.7% of raw `.szsnap` |
| `org_authorization/1m_rules/check_denied_exclusion` | 1M org rules | `[981.46 ns, 990.13 ns, 995.71 ns]` | benefits from plain-exclusion short-circuit |

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

2026-05-23 evidence:

| Scenario | Read ops/s | Write calls/s | Logical writes/s | Write p95 us |
| --- | ---: | ---: | ---: | ---: |
| `concurrent_runtime/read_heavy_light_write_batched` | 5,614,038 | 32 | 1,024 | 887 |
| `concurrent_runtime/read_heavy_medium_write_unbatched` | 5,920,998 | 1,906 | 1,906 | 1,191 |
| `concurrent_runtime/read_heavy_medium_write_batched` | 5,807,822 | 742 | 94,976 | 2,663 |
| `concurrent_runtime/read_heavy_heavy_write_unbatched` | 5,688,228 | 3,908 | 3,908 | 2,635 |
| `concurrent_runtime/read_heavy_heavy_write_batched` | 5,925,860 | 874 | 111,872 | 9,921 |
| `concurrent_runtime/tenant_sharded_heavy_write_batched` | 7,816,622 | 3,274 | 419,072 | 2,753 |

## 3.7 Real-World Authorization Benchmarks

The synthetic org benchmark remains the historical trend line for compact storage and snapshot
load. It is intentionally stable, but it over-represents a small set of hot objects and users. The
`realworld_authorization` benchmark adds a larger SaaS collaboration sample with:

- 128 tenants and workspace/project/folder/doc resources.
- group membership, direct users, inherited viewers/editors, auditors, owners, and deny lists.
- `check`, `lookup_resources`, `lookup_subjects`, `lookup_permissions`,
  `lookup_object_permissions`, `expand`, mixed-read, snapshot-load, and snapshot-size measurements.

2026-05-23 1M-rule evidence after the plain-exclusion short-circuit optimization:

| Operation | Dataset | Measurement | Target status |
| --- | --- | ---: | --- |
| `realworld_authorization/1m_rules/check_doc_inherited_workspace_member` | 1M realworld rules | `[17.428 us, 17.608 us, 17.852 us]` | recorded realistic inherited baseline |
| `realworld_authorization/1m_rules/check_doc_direct_user` | 1M realworld rules | `[3.7781 us, 3.8040 us, 3.8441 us]` | recorded direct baseline |
| `realworld_authorization/1m_rules/check_doc_denied_by_ban` | 1M realworld rules | `[1.1511 us, 1.1627 us, 1.1735 us]` | fixed from pre-optimization ~607 us |
| `realworld_authorization/1m_rules/check_doc_project_editor` | 1M realworld rules | `[6.3356 us, 6.3913 us, 6.4308 us]` | recorded editor inheritance baseline |
| `realworld_authorization/1m_rules/lookup_resources_target_user` | 1M realworld rules | `[6.2352 us, 6.2939 us, 6.3217 us]` | recorded bounded lookup baseline |
| `realworld_authorization/1m_rules/lookup_subjects_shared_doc` | 1M realworld rules | `[11.500 us, 11.581 us, 11.657 us]` | recorded subject lookup baseline |
| `realworld_authorization/1m_rules/lookup_permissions_shared_doc` | 1M realworld rules | `[12.862 us, 13.015 us, 13.147 us]` | recorded permission enumeration baseline |
| `realworld_authorization/1m_rules/lookup_object_permissions_shared_doc` | 1M realworld rules | `[28.118 us, 28.645 us, 29.035 us]` | recorded object audit baseline |
| `realworld_authorization/1m_rules/expand_shared_doc` | 1M realworld rules | `[2.7888 us, 2.8134 us, 2.8311 us]` | recorded expand baseline |
| `realworld_authorization/1m_rules/mixed_read_workload` | 1M realworld rules | `[63.188 us, 63.394 us, 63.577 us]` | fixed from pre-optimization ~679 us |
| `realworld_authorization/1m_rules/snapshot_load_compact` | 1M realworld rules | `[557.61 ms, 560.65 ms, 563.14 ms]` | consistent with org 1M full-load gate |
| `realworld_authorization/1m_rules/snapshot_load_trusted_fast` | 1M realworld rules | `[136.47 ms, 137.53 ms, 138.62 ms]` | passes <= 200 ms trusted gate |
| `realworld_authorization/1m_rules/snapshot_file_size` | 1M realworld rules | `123,983,263 bytes` | comparable to org 1M artifact |

## 3.8 Structural Performance Optimization Targets

[21-performance-optimization-design.md](./21-performance-optimization-design.md) turns the
post-M10 performance review into explicit gates. The implementation must first record 1M baselines
for every new `perf_optimization` filter before claiming an improvement.

| Area | Benchmark | Target |
| --- | --- | ---: |
| prepared check / ID-native eval | `perf_optimization/check_prepared_1m` | no public check regression; allocation count lower than baseline |
| realistic inherited read | `realworld_authorization/1m_rules/check_doc_inherited_workspace_member` | >= 10% improvement or profile-backed recalibration |
| realistic mixed read | `realworld_authorization/1m_rules/mixed_read_workload` | upper estimate <= 55 us |
| streaming lookup | `perf_optimization/lookup_subjects_streaming_1m` and `perf_optimization/lookup_resources_streaming_1m` | no latency regression; lower allocation count |
| full snapshot load | `snapshot_load_compact/1m` | upper estimate <= 450 ms after loader optimization |
| load-time RSS | `snapshot_load_peak_rss/1m` | max RSS <= 400 MiB |
| trusted fast load | `snapshot_load_trusted_fast/1m` | upper estimate <= 200 ms |
| single write over 1M base | `perf_optimization/write_single_touch_1m` | p95 improves >= 3x versus pre-change 1M baseline |
| mixed batch write over 1M base | `perf_optimization/write_mixed_batch_1m` | p95 improves >= 3x versus pre-change 1M baseline |
| read-heavy heavy writes | `perf_optimization/read_heavy_heavy_write_batched_1m` | read throughput no > 5% regression; write p95 improves >= 2x |
| index profiles | `snapshot_file_size_check_only/1m` plus RSS equivalent | >= 20% reduction versus `Full` |

The hard <= 200 ms target is intentionally scoped to trusted artifacts. A default full loader that
continues to prove hostile-file row and index semantics at startup is not expected to hit <= 200 ms
without changing the trust boundary.

## 3.9 M11 Structural Optimization Measurements

Measured on 2026-05-24 on the reference macOS machine after the Phase 12 low-risk read-path,
snapshot-loader instrumentation, streaming raw writer, lazy uniqueness, segmented store
publication, and index-profile work.

| Operation | Dataset | Measurement | Target status |
| --- | --- | ---: | --- |
| `perf_optimization/check_prepared_1m` | 1M org rules | `[5.8971 us, 5.9513 us, 6.0009 us]` | no regression versus prior prepared-check baseline |
| `perf_optimization/lookup_resources_streaming_1m` | 1M org rules | `[3.0446 ms, 3.1188 ms, 3.1883 ms]` | no regression versus prior 1M lookup budget |
| `perf_optimization/lookup_subjects_streaming_1m` | 1M org rules | `[6.2956 us, 6.3200 us, 6.3451 us]` | no regression versus prior streaming lookup baseline |
| `perf_optimization/write_single_touch_1m` | 1M org rules | `[130.26 us, 192.30 us, 227.23 us]` | > 100x p95 improvement versus the pre-segmentation 27.861 ms upper estimate |
| `perf_optimization/write_mixed_batch_1m` | 1M org rules | `[982.19 us, 3.7111 ms, 7.7351 ms]` | > 3x p95 improvement versus the pre-segmentation 24.080 ms upper estimate |
| `perf_optimization/read_heavy_light_write_1m` | 1M org rules | `[13.610 us, 14.471 us, 15.338 us]` | write amplification removed from mixed harness |
| `perf_optimization/read_heavy_medium_write_unbatched_1m` | 1M org rules | `[13.699 us, 14.521 us, 15.171 us]` | write amplification removed from mixed harness |
| `perf_optimization/read_heavy_medium_write_batched_1m` | 1M org rules | `[16.942 us, 17.353 us, 17.884 us]` | write amplification removed from mixed harness |
| `perf_optimization/read_heavy_heavy_write_unbatched_1m` | 1M org rules | `[14.408 us, 15.769 us, 17.371 us]` | write amplification removed from mixed harness |
| `perf_optimization/read_heavy_heavy_write_batched_1m` | 1M org rules | `[15.181 us, 16.530 us, 17.844 us]` | > 2x improvement versus the pre-segmentation 1.6742 ms upper estimate |
| `perf_optimization/snapshot_load_phase_timers_1m` | 1M org rules | `[552.91 ms, 556.03 ms, 559.16 ms]` | phase evidence recorded; full-load <= 450 ms not yet met |
| `snapshot_load_compact/1m` | 1M org rules | `[556.47 ms, 572.87 ms, 589.75 ms]` | still above Phase 12 <= 450 ms target |
| `snapshot_load_peak_rss/1m` | 1M org rules | `[538.48 ms, 541.61 ms, 545.36 ms]` | timing filter recorded; RSS still requires external `/usr/bin/time` evidence |
| `snapshot_load_trusted_fast/1m` | 1M org rules | `[135.38 ms, 136.84 ms, 138.34 ms]` | passes <= 200 ms trusted gate |
| `realworld_authorization/1m_rules/check_doc_inherited_workspace_member` | 1M realworld rules | `[14.793 us, 14.966 us, 15.110 us]` | >= 10% improvement versus 17.852 us prior upper estimate |
| `realworld_authorization/1m_rules/mixed_read_workload` | 1M realworld rules | `[57.221 us, 57.733 us, 58.164 us]` | still above the <= 55 us target |
| `snapshot_file_size_check_only/1m` | 1M org rules | `78,188,326 bytes` vs `124,422,114 bytes` full | 37.2% reduction; passes >= 20% file-size target |

One representative `snapshot_load_phase_timers_1m` run produced:

| Phase | Duration |
| --- | ---: |
| `file_read` | `12.007417 ms` |
| `decompression` | `41 ns` |
| `header_and_sections` | `4.001 us` |
| `checksum` | `53.784833 ms` |
| `schema_parse_compile` | `42.375 us` |
| `symbols` | `80.906917 ms` |
| `rows` | `299.240541 ms` |
| `indexes` | `106.616042 ms` |
| `publish` | `2.042 us` |

The evidence shows the read-side allocation fixes are effective, segmented publication removes the
1M-base write-copy ceiling, trusted fast-load remains within budget, and check-only artifacts exceed
the disk-size reduction target. The remaining full-load target is dominated by safe row validation
plus symbol/index decoding. The remaining realistic mixed-read target needs another evaluator
profile pass because the synthetic mixed read/write harness now spends negligible time cloning the
relationship store.

## 3.10 Snapshot Section-Size Measurements

[22-snapshot-file-size-optimization-design.md](./22-snapshot-file-size-optimization-design.md)
turns the Phase 12 file-size result into a section-driven format plan. The benchmark target is
`make bench-snapshot-section-size`; it reports raw/zstd bytes, section bytes, index group bytes, and
profile deltas. The Criterion timings for `snapshot_section_size/*/total_bytes` are not latency
evidence because the benchmark reports constant byte counts after generating the artifacts.

Measured 2026-05-24 on the 1M org fixture:

| Profile | Raw bytes | Zstd bytes | Index payload | Non-index payload | Saved vs `Full` |
| --- | ---: | ---: | ---: | ---: | ---: |
| `Full` | 124,422,114 | 33,116,811 | 63,867,328 | 60,554,402 | baseline |
| `CheckOnly` | 78,188,326 | 18,873,001 | 17,633,540 | 60,554,402 | 46,233,788 bytes / 37.15% |
| `CheckAndObjectAudit` | 78,188,326 | 18,873,001 | 17,633,540 | 60,554,402 | 46,233,788 bytes / 37.15% |

Full-profile index group payload:

| Index group | Payload bytes | Total postings |
| --- | ---: | ---: |
| `resource` | 17,633,400 | 1,000,000 |
| `resource_object` | 17,633,380 | 1,000,000 |
| `resource_type_relation` | 4,000,140 | 1,000,000 |
| `resource_type` | 4,000,060 | 1,000,000 |
| `subject` | 13,933,444 | 1,666,666 |
| `subject_type_relation` | 2,666,704 | 666,666 |
| `subject_type` | 4,000,060 | 1,000,000 |

## 3.11 M12 Snapshot File-Size Measurements

Measured 2026-05-24 on the same 1M org fixture after the v3 posting delta-varint stream,
singleton/multi index split, group-specific compact key widths, row-id width encoding, compact
symbol table entries, and compact symbol lookup ids.

| Profile | Raw bytes | Zstd bytes | Index payload | Non-index payload | Target status |
| --- | ---: | ---: | ---: | ---: | --- |
| `Full` | 77,573,519 | 22,317,770 | 28,118,798 | 49,454,337 | passes <= 100 MB |
| `CheckOnly` | 59,078,231 | 19,031,692 | 9,623,510 | 49,454,337 | passes <= 65 MB |
| `CheckAndObjectAudit` | 59,078,231 | 19,031,693 | 9,623,510 | 49,454,337 | same capability alias as `CheckOnly` |

`CheckOnly` and `CheckAndObjectAudit` save 18,495,288 raw bytes versus `Full`, or 23.84%.

Largest remaining sections:

| Section | Bytes |
| --- | ---: |
| `relationship_rows` | 18,000,000 |
| `symbol_bytes` | 16,153,374 |
| `symbol_hashes` | 8,160,104 |
| `symbol_table` | 4,080,052 |
| `symbol_lookup` | 3,060,039 |

Full-profile index payload by group:

| Index group | Payload bytes | Total postings |
| --- | ---: | ---: |
| `resource` | 9,623,370 | 1,000,000 |
| `resource_object` | 7,578,359 | 1,000,000 |
| `resource_type_relation` | 1,000,083 | 1,000,000 |
| `resource_type` | 1,000,036 | 1,000,000 |
| `subject` | 7,250,082 | 1,666,666 |
| `subject_type_relation` | 666,692 | 666,666 |
| `subject_type` | 1,000,036 | 1,000,000 |

Phase 13 gate evidence:

| Benchmark | Evidence | Status |
| --- | --- | --- |
| `snapshot_load_compact/1m` | `[579.68 ms, 585.81 ms, 593.91 ms]` | passes <= 700 ms; within 5% of the M11 full-load upper estimate |
| `snapshot_load_trusted_fast/1m` | `[183.45 ms, 185.11 ms, 186.84 ms]` | passes <= 200 ms |
| `snapshot_load_zstd/1m` | `[625.59 ms, 629.45 ms, 633.10 ms]` | no detected regression |
| `snapshot_file_size/1m` | `77,573,646 bytes` | recorded v3 full-size artifact in the snapshot bench fixture |
| `snapshot_file_size_zstd/1m` | `22,384,838 bytes` | recorded zstd artifact in the snapshot bench fixture |
| `snapshot_file_size_check_only/1m` | `full=77,573,519 bytes check_only=59,078,231 bytes` | 23.84% smaller than `Full`; passes >= 20% |
| `snapshot_load_peak_rss/1m` | `343,851,008-byte max RSS; 312,705,672-byte peak footprint` | passes <= 400 MiB RSS target |
| `perf_optimization/snapshot_load_phase_timers_1m` | `[578.05 ms, 586.63 ms, 597.34 ms]`; `file_read=7.85 ms`, `checksum=32.79 ms`, `symbols=87.53 ms`, `rows=314.50 ms`, `indexes=145.07 ms` | recorded post-v3 load phase costs |

## 3.12 M13 Read/Load and Zstd-Aware Layout Measurements

Measured 2026-05-24 after [24](./24-zstd-aware-snapshot-load-design.md)'s zstd-aware inner layout,
row-chunk relationship decode, and evaluator recursion-stack optimization. Diffs below compare to
Phase 13 evidence, not to Criterion's previous local run cache.

| Benchmark | Phase 13 | Current | Diff |
| --- | ---: | ---: | ---: |
| `snapshot_file_size/1m` | `77,573,646 bytes` | `77,573,646 bytes` | unchanged |
| `snapshot_file_size_zstd/1m` | `22,384,838 bytes` | `21,512,241 bytes` | `-872,597 bytes / -3.90%` |
| section-size `Full` zstd | `22,317,770 bytes` | `21,471,681 bytes` | `-846,089 bytes / -3.79%` |
| section-size `CheckOnly` zstd | `19,031,692 bytes` | `18,182,828 bytes` | `-848,864 bytes / -4.46%` |
| `snapshot_load_compact/1m` | `[579.68 ms, 585.81 ms, 593.91 ms]` | `[547.67 ms, 550.62 ms, 553.98 ms]` | upper `-6.72%` |
| `snapshot_load_trusted_fast/1m` | `[183.45 ms, 185.11 ms, 186.84 ms]` | `[171.85 ms, 172.70 ms, 173.52 ms]` | upper `-7.13%` |
| `snapshot_load_zstd/1m` | `[625.59 ms, 629.45 ms, 633.10 ms]` | `[610.46 ms, 614.59 ms, 618.59 ms]` | upper `-2.29%` |
| `perf_optimization/snapshot_load_phase_timers_1m` | `[578.05 ms, 586.63 ms, 597.34 ms]` | `[546.45 ms, 548.01 ms, 549.63 ms]` | upper `-7.99%` |
| `snapshot_load_peak_rss/1m` raw | `343,851,008-byte max RSS; 312,705,672-byte peak footprint` | `354,959,360-byte max RSS; 315,638,456-byte peak footprint` | max RSS `+3.23%`; still under 400 MiB |
| `snapshot_load_peak_rss/1m` zstd | no Phase 13 zstd RSS baseline | `415,055,872-byte max RSS; 367,100,648-byte peak footprint` | direct-zstd load remains under 400 MiB |
| `realworld_authorization/1m_rules/mixed_read_workload` | `[57.221 us, 57.733 us, 58.164 us]` | `[52.474 us, 52.895 us, 53.489 us]` | upper `-8.04%`; passes <=55 us |
| `realworld_authorization/1m_rules/check_doc_inherited_workspace_member` | `[14.793 us, 14.966 us, 15.110 us]` | `[14.473 us, 14.655 us, 14.833 us]` | upper `-1.83%`; still above 13.5 us stretch |
| `perf_optimization/check_prepared_1m` | `[5.8971 us, 5.9513 us, 6.0009 us]` | `[5.2677 us, 5.3115 us, 5.3424 us]` | upper `-10.97%` |
| `perf_optimization/lookup_resources_streaming_1m` | `[3.0446 ms, 3.1188 ms, 3.1883 ms]` | `[2.6849 ms, 2.7143 ms, 2.7544 ms]` | upper `-13.61%` |
| `perf_optimization/lookup_subjects_streaming_1m` | `[6.2956 us, 6.3200 us, 6.3451 us]` | `[5.4088 us, 5.4472 us, 5.4722 us]` | upper `-13.75%` |
| `perf_optimization/read_heavy_heavy_write_batched_1m` | `[15.181 us, 16.530 us, 17.844 us]` | `[10.810 us, 11.567 us, 12.365 us]` | upper `-30.70%` |

Representative phase timer:

| Phase | Duration |
| --- | ---: |
| `file_read` | `7.600375 ms` |
| `decompression` | `42 ns` |
| `header_and_sections` | `4.333 us` |
| `checksum` | `32.422542 ms` |
| `schema_parse_compile` | `42 us` |
| `symbols` | `86.127084 ms` |
| `rows` | `277.822584 ms` |
| `indexes` | `135.971417 ms` |
| `publish` | `1.75 us` |

## 3.13 M13 Read-Path Completion Measurements

Measured 2026-05-24 after the remaining read-path refinement work in
[23](./23-read-performance-optimization-design.md): compiled relation-id schema rewrites,
conservative exact computed-userset shortcuts, reusable lookup verification contexts, and
checkpoint-native delta tombstone masking.

| Benchmark | Previous M13 evidence | Completion evidence | Target status |
| --- | ---: | ---: | --- |
| `realworld_authorization/1m_rules/mixed_read_workload` | `[52.474 us, 52.895 us, 53.489 us]` | `[41.599 us, 42.085 us, 42.690 us]` | passes <= 55 us |
| `realworld_authorization/1m_rules/check_doc_inherited_workspace_member` | `[14.473 us, 14.655 us, 14.833 us]` | `[11.540 us, 11.599 us, 11.646 us]` | passes <= 13.5 us stretch |
| `perf_optimization/check_prepared_1m` | `[5.2677 us, 5.3115 us, 5.3424 us]` | `[4.3107 us, 4.3867 us, 4.4450 us]` | improved |
| `perf_optimization/lookup_resources_streaming_1m` | `[2.6849 ms, 2.7143 ms, 2.7544 ms]` | `[2.2853 ms, 2.3241 ms, 2.3551 ms]` | improved |
| `perf_optimization/lookup_subjects_streaming_1m` | `[5.4088 us, 5.4472 us, 5.4722 us]` | `[4.6735 us, 4.7194 us, 4.7426 us]` | improved |
| `perf_optimization/read_heavy_heavy_write_batched_1m` | `[10.810 us, 11.567 us, 12.365 us]` | `[10.820 us, 11.773 us, 12.712 us]` | no detected regression |
| `perf_optimization/read_heavy_delta_counters_1m` | not present | `[4.4792 us, 4.5157 us, 4.5475 us]`; `delta_segments_inspected=1400`, `tombstone_checks=300` over 100 checks | counters recorded |
| `snapshot_file_size/1m` | `77,573,646 bytes` | `77,573,646 bytes` | raw artifact unchanged |
| `snapshot_file_size_zstd/1m` | `21,512,241 bytes` | `21,512,241 bytes` | zstd artifact unchanged |
| `snapshot_load_compact/1m` | `[547.67 ms, 550.62 ms, 553.98 ms]` | `[571.24 ms, 572.58 ms, 573.95 ms]` | still under 700 ms; no read-path dependency |
| `snapshot_load_trusted_fast/1m` | `[171.85 ms, 172.70 ms, 173.52 ms]` | `[174.79 ms, 176.08 ms, 177.49 ms]` | passes <= 200 ms |
| `snapshot_load_zstd/1m` | `[610.46 ms, 614.59 ms, 618.59 ms]` | `[639.00 ms, 643.41 ms, 650.15 ms]` | distribution-load baseline recorded |

Representative completion-phase timer:

| Phase | Duration |
| --- | ---: |
| `file_read` | `7.828875 ms` |
| `decompression` | `42 ns` |
| `header_and_sections` | `3.833 us` |
| `checksum` | `32.892083 ms` |
| `schema_parse_compile` | `51.083 us` |
| `symbols` | `87.499833 ms` |
| `rows` | `292.607208 ms` |
| `indexes` | `145.074458 ms` |
| `publish` | `1.834 us` |

## 3.14 Compiled Computed-Userset Shortcut Follow-Up

Measured 2026-05-24 after the deep follow-up review in
[25](./25-compiled-computed-userset-shortcut-design.md). The parser migration idea was deferred
because schema parse/compile is ~51 us inside the ~572 ms full snapshot-load path and is absent from
steady-state reads. The implemented follow-up instead moves computed-userset plain-target knowledge
into compiled schema IR.

| Benchmark | Phase 14 completion | Follow-up | Result |
| --- | ---: | ---: | --- |
| `perf_optimization/check_prepared_1m` | `[4.3107 us, 4.3867 us, 4.4450 us]` | `[4.2240 us, 4.2590 us, 4.3065 us]` | upper `-3.12%`; no regression |
| `realworld_authorization/1m_rules/check_doc_inherited_workspace_member` | `[11.540 us, 11.599 us, 11.646 us]` | `[11.211 us, 11.341 us, 11.559 us]` | upper `-0.75%`; no regression |
| `realworld_authorization/1m_rules/mixed_read_workload` | `[41.599 us, 42.085 us, 42.690 us]` | `[40.921 us, 41.317 us, 41.808 us]` | upper `-2.07%`; no regression |

## 3.15 M14 Measurement Baseline Counters

Phase 15.0 adds benchmark-only measurement foundations for
[32](./32-read-optimization-follow-up-plan.md). The production read path is unchanged; the new
counters and fixture helpers are compiled only with `bench-internals`.

Phase 14 follow-up remains the comparison point for later M14 optimization slices:

| Benchmark | Phase 14 follow-up upper estimate | Later-slice gate |
| --- | ---: | --- |
| `perf_optimization/check_prepared_1m` | `4.3065 us` | no > 2% regression for memoization; no > 5% regression for other read-path slices |
| `realworld_authorization/1m_rules/check_doc_inherited_workspace_member` | `11.559 us` | no > 5% regression and stay <= `13.5 us` stretch |
| `realworld_authorization/1m_rules/mixed_read_workload` | `41.808 us` | no > 5% regression and stay <= `55 us` hard gate |
| `perf_optimization/lookup_resources_streaming_1m` | `2.3551 ms` | planner slice must improve by >= 25% or stop with counter/profile evidence |
| `perf_optimization/lookup_subjects_streaming_1m` | `4.7426 us` | allocation slice must cut allocations materially with no > 5% latency regression |
| `perf_optimization/read_heavy_heavy_write_batched_1m` | `12.712 us` | delta/bitmap slices must show no > 5% regression unless a hard compaction cap triggers |

New Phase 15.0 fixture filters:

| Filter | Counter evidence | Follow-up decision gate |
| --- | --- | --- |
| `perf_optimization/phase15_memo_shared_parent` + `read_followup_allocations/phase15_memo_shared_parent` | completed-check repeat opportunities across shared parent/group proofs plus allocation count/bytes | Proceed with 15.1 only if hit opportunities are non-zero and the repeated-subcheck fixture can target >= 10% improvement. |
| `perf_optimization/phase15_lookup_subjects_allocation` + `read_followup_allocations/phase15_lookup_subjects_allocation` | allocation count/bytes plus lookup-subject candidate/full-root counts | Proceed with 15.2 streaming only if transient allocation is visible and public `expand` remains separately measured. |
| `perf_optimization/phase15_delete_heavy_delta` | view delta stats, query calls, delta inspections, and tombstone checks | Proceed with 15.3 once delete-heavy latest reads show tombstone overhead above the Phase 14 delta sample. |
| `perf_optimization/phase15_high_fanout_posting` + `read_followup_allocations/phase15_high_fanout_posting` | posting length histograms by index group and allocation count/bytes | Proceed with 15.6 dense/tombstone representation only for groups with histogram-backed dense or high-cardinality postings. |
| `perf_optimization/phase15_lookup_planner_pruning` | lookup candidate count and full-root check count for exclusion-only producer noise | Proceed with 15.5 producer pruning only if counters show candidate/full-check reduction potential. |

Local Phase 15.0 baseline snapshot from `make bench-read-followup-baseline`:
allocation samples run in the dedicated `read_followup_allocations` bench binary so
the `perf_optimization` latency filters stay on the normal allocator. Allocation
sample inputs are cloned before the measured allocator region.

| Filter | Local timing | Counter snapshot |
| --- | ---: | --- |
| `perf_optimization/phase15_memo_shared_parent` | `3.3874..3.5147 ms` | `5000` checks, `2997` memo-hit opportunities, `1000` lookup candidates/full-root checks; allocation companion: `1072865` allocations, `38573712` bytes |
| `perf_optimization/phase15_lookup_subjects_allocation` | `604.03..608.86 ms` | `502500` checks, `1000` subject candidates/usersets/full-root checks; allocation companion: `193219344` allocations, `6881911104` bytes |
| `perf_optimization/phase15_delete_heavy_delta` | `7.1309..7.5606 us` | `1` query, `1` inspected delta segment, `4096` deleted rows, `9997` tombstone ratio bps |
| `perf_optimization/phase15_high_fanout_posting` | `2.4394..2.5346 ms` | resource indexes have one `16384`-row posting; exact-subject index has `16384` singleton postings; allocation companion: `646752` allocations, `41070704` bytes |
| `perf_optimization/phase15_lookup_planner_pruning` | `95.482..99.013 ms` | `100000` checks, `50000` lookup candidates, `50000` full-root checks |

Local Phase 15.1, 15.2, and 15.5 follow-up snapshot from
`cargo bench --features bench-internals --bench perf_optimization -- phase15 --sample-size 10`
and `cargo bench --features bench-internals --bench read_followup_allocations`:

| Filter | Follow-up timing | Counter snapshot |
| --- | ---: | --- |
| `perf_optimization/phase15_memo_shared_parent` | `2.7524..2.9980 ms` | `3002` evaluated checks, `999` memo hits, `2003` memo misses/inserts, `1000` lookup candidates/full-root checks; allocation companion: `929633` allocations, `57763312` bytes |
| `perf_optimization/phase15_lookup_subjects_allocation` | `683.77..707.74 us` | positive-only proof shortcut removed full-root verification: `0` checks, `1000` subject candidates, `1000` userset candidates, `0` full-root checks; allocation companion: `272672` allocations, `13003712` bytes |
| `perf_optimization/phase15_delete_heavy_delta` | `7.1991..7.8377 us` | unchanged non-target fixture: `1` query, `1` inspected delta segment, `4096` deleted rows, `9997` tombstone ratio bps |
| `perf_optimization/phase15_high_fanout_posting` | `224.27..259.58 us` | positive-only `This` lookup-subject proof skips full-root verification; allocation companion: `32400` allocations, `6350288` bytes |
| `perf_optimization/phase15_lookup_planner_pruning` | `1.0150..1.0501 ms` | schema producer pruning removed `50000` exclusion-only candidates: `0` checks, `0` lookup candidates, `50000` schema-pruned relationships, `0` full-root checks |

Gate outcome:

- 15.1 memoization is kept for `lookup_resources` and permission enumeration, where shared
  subchecks exist. It is intentionally not enabled for `lookup_subjects`; the first measured pass
  had `0` memo hits and a `~21%` latency regression on the allocation fixture.
- 15.2 streaming plus the 15.5 exact positive-proof shortcut materially reduces
  `lookup_subjects` allocation and latency while leaving public `expand` unchanged.
- 15.5 producer pruning keeps full-root verification for returned resources and prunes only
  positive-producer-safe root candidates. Lookup-subject full-check skipping is limited to exact
  positive proof shapes and is recomputed for nested userset relations; exclusion, intersection,
  tuple-to-userset, and recursive fallbacks still use full verification.

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
- Real-world benchmark coverage must include deny-list, inheritance, group userset, and audit helper
  cases; synthetic hot-path benchmarks alone are not sufficient evidence.
- No full relationship-store clone per successful write after [21](./21-performance-optimization-design.md) Phase 12.4.
- Index-profile optimizations must return typed unsupported-operation errors instead of silently
  falling back to full scans.

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
- Performance optimization roadmap: [21-performance-optimization-design.md](./21-performance-optimization-design.md)
- Related research: [../docs/research/study-spicedb.md § Query Filters and Indexes](../docs/research/study-spicedb.md#query-filters-and-indexes)
