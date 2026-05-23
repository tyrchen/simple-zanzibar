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

If Phase 0 proves a target unrealistic on the reference machine, update this spec and [99-key-decisions.md](./99-key-decisions.md) with measured data before implementation proceeds.

## 4. Design Constraints

- No full relationship-store scans in direct `check`.
- No read-path mutex.
- No clone of all matched relationships for hot checks.
- No string parsing inside the evaluator hot loop.
- Bounded fanout at each recursive step.
- Benchmark-only counters can be behind `bench-internals`.

## 5. Profiling Rules

- Optimize only after a failing benchmark or profile evidence.
- Use `criterion` for repeatable microbenchmarks.
- Use `samply` or `cargo flamegraph` for CPU profiles when a benchmark misses by more than 20 percent.
- Keep allocation counts visible for direct and one-hop checks.

## 6. Cross-References

- <- Depends on: [12-relationship-store-design.md](./12-relationship-store-design.md), [14-evaluation-engine-design.md](./14-evaluation-engine-design.md), [60-crates-features-design.md](./60-crates-features-design.md)
- -> Consumed by: [72-testing-verification-plan.md](./72-testing-verification-plan.md), [91-local-engine-impl-plan.md](./91-local-engine-impl-plan.md)
- Related research: [../docs/research/study-spicedb.md § Query Filters and Indexes](../docs/research/study-spicedb.md#query-filters-and-indexes)
