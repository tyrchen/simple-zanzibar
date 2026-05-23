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

The initial 500 ms fast-load target was optimistic for the first safe checked loader because the
loader validates every serialized index posting against compact rows and rejects incomplete or
mis-keyed indexes. The M7 gate is recalibrated to a Criterion upper estimate <= 700 ms for the
first version. A future
optimization pass can attempt to recover the original <= 500 ms target by making index validation
single-pass over grouped row ids or by adding a trusted-writer validation mode, but the default
untrusted-file loader keeps full validation.

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
