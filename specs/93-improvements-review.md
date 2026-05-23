# 93 - Improvements Review

Status: draft v1
Owner: Simple Zanzibar
Depends on: [91-local-engine-impl-plan.md](./91-local-engine-impl-plan.md)

## 1. Purpose

This file is the canonical backlog for valid review findings that are out of phase for the current
implementation slice. Each item stays here until a later phase implements it or the relevant spec is
updated.

## 2. Deferred Findings

### D1 - Public v2 Engine API

- Severity: P2
- Citation: [src/lib.rs](../src/lib.rs)
- Source: Phase 3 review against [15-public-api-design.md](./15-public-api-design.md)
- Finding: Phase 3 returns tokens through `ZanzibarService`, but the full `ZanzibarEngine`,
  `ZanzibarEngineBuilder`, `EngineError`, and request/response API from spec 15 are not yet present.
- Fix shape: introduce `ZanzibarEngine` as the owner of snapshot state, move `ZanzibarService` to a
  compatibility facade that delegates to the engine, and expose request/response APIs with
  doc-tested examples.
- Target phase: Phase 5 or Phase 6, when lookup APIs and release public surface are finalized.

### D2 - Concurrent Snapshot Publication Tests

- Severity: P2
- Citation: [tests/revision_tests.rs](../tests/revision_tests.rs)
- Source: Phase 3 review against [13-revision-consistency-design.md](./13-revision-consistency-design.md)
  and [72-testing-verification-plan.md](./72-testing-verification-plan.md)
- Finding: Exact snapshot tests cover write/delete/schema-change interleavings, wrong datastore,
  expired revision, future revision, and schema hash mismatch, but do not yet cover concurrent
  read/write publication through a writer gate.
- Fix shape: once `ZanzibarEngine` owns an explicit writer gate, add concurrent tests proving latest
  readers acquire one atomic published snapshot while writes publish newer snapshots.
- Target phase: Phase 6 release hardening, after the engine wrapper exists.

### D3 - Rust 2024 Toolchain and Crate Root Lints

- Severity: P3
- Citation: [Cargo.toml](../Cargo.toml), [src/lib.rs](../src/lib.rs)
- Source: Phase 3 review against AGENTS.md and [60-crates-features-design.md](./60-crates-features-design.md)
- Finding: The crate is still on edition 2021 and does not yet have `rust-toolchain.toml` or crate
  root lint attributes for unsafe, Rust 2024 compatibility, missing docs, and debug impls.
- Fix shape: pin latest stable in `rust-toolchain.toml`, migrate the crate to edition 2024, add
  `#![forbid(unsafe_code)]` and required warn lints, then fix all resulting diagnostics.
- Target phase: Phase 6 task 6.1 and 6.2.

### D4 - Legacy Evaluator Public Surface

- Severity: P2
- Citation: [src/eval.rs](../src/eval.rs)
- Source: Phase 4 review against [14-evaluation-engine-design.md](./14-evaluation-engine-design.md)
  and [15-public-api-design.md](./15-public-api-design.md)
- Finding: `check_with_configs`, `check_with_indexed_store`, and `expand_with_configs` remain public
  compatibility helpers, so callers can bypass the snapshot-backed `EvaluationContext` even though
  `ZanzibarService` now routes typed-schema checks and expands through the shared evaluator.
- Fix shape: when the v2 public API lands, move these helpers behind the compatibility facade or make
  them crate-private/test-only so the public surface cannot opt out of typed schema, snapshot,
  recursion, fanout, and membership semantics.
- Target phase: Phase 6 task 6.4 after `ZanzibarEngine` owns the public request/response API.

## 3. Cross-References

- Implementation plan: [91-local-engine-impl-plan.md](./91-local-engine-impl-plan.md)
- Roadmap: [90-local-engine-roadmap.md](./90-local-engine-roadmap.md)
