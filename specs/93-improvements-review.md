# 93 - Improvements Review

Status: draft v1
Owner: Simple Zanzibar
Depends on: [91-local-engine-impl-plan.md](./91-local-engine-impl-plan.md)

## 1. Purpose

This file is the canonical backlog for valid review findings that are out of phase for the current
implementation slice. Each item stays here until a later phase implements it or the relevant spec is
updated.

## 2. Deferred Findings

- P2 - Default full snapshot load still misses the Phase 12 <= 450 ms target. Phase timings show
  `src/relationship.rs:3837` row decoding/semantic validation around 299 ms and
  `src/relationship.rs:3182` sequential index decoding around 106 ms on the 1M fixture. Fix shape:
  split safe row validation and independent index-group decode into bounded parallel workers with
  deterministic first-error reporting, then re-run `snapshot_load_compact/1m` and RSS gates.
- P2 - `realworld_authorization/1m_rules/mixed_read_workload` remains above the <= 55 us target at
  `[57.221 us, 57.733 us, 58.164 us]`. `src/eval.rs:360` still materializes legacy relations for
  recursive schema checks in some paths after the first ID-native pass. Fix shape: compile relation
  ids into schema expression nodes and extend the reusable evaluation context so recursive computed
  userset and tuple-to-userset checks stay segment/id-native across the whole expression tree.
- P3 - The Phase 12 index-profile evidence currently proves disk reduction only; steady-state RSS
  reduction for `CheckOnly` is not recorded alongside `snapshot_file_size_check_only/1m`. Fix
  shape: add a Makefile-discoverable RSS measurement for `Full`, `CheckOnly`, and
  `CheckAndObjectAudit` loaded snapshots and update [71](./71-performance-budgets-design.md) with
  the memory delta.

## 3. Cross-References

- Implementation plan: [91-local-engine-impl-plan.md](./91-local-engine-impl-plan.md)
- Roadmap: [90-local-engine-roadmap.md](./90-local-engine-roadmap.md)
