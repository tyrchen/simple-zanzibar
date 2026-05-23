# 60 - Crates and Features Design

Status: draft v1
Owner: Simple Zanzibar
Depends on: [15-public-api-design.md](./15-public-api-design.md)

## 1. Purpose

This spec defines crate layout, feature flags, and dependency policy for the local engine rebuild. The project stays a single crate until component boundaries prove they need separate crates.

## 2. Module Layout

```text
src/
  lib.rs
  api.rs
  domain.rs
  schema/
    mod.rs
    parser.rs
    compiler.rs
    validator.rs
  relationship/
    mod.rs
    filter.rs
    memory.rs
    mutation.rs
  revision.rs
  engine/
    mod.rs
    check.rs
    expand.rs
    lookup.rs
    membership.rs
  error.rs
```

The existing `model.rs`, `store.rs`, `eval.rs`, and `parser.rs` are retired once the compatibility facade delegates fully to v2 modules.

## 3. Feature Flags

| Feature | Default | Purpose |
| --- | --- | --- |
| `std` | yes | Standard library support. Required for v2. |
| `serde` | no | Serialize request/response/domain types. |
| `tracing` | no | Emit structured spans. |
| `compat` | yes during v2 migration | Keep legacy `ZanzibarService` facade. |
| `bench-internals` | no | Expose benchmark-only constructors and counters. |

No network or runtime feature is part of v2.

## 4. Dependency Survey

Dependency versions checked with `cargo search` on 2026-05-23:

| Crate | Latest observed | Intended use | Decision |
| --- | ---: | --- | --- |
| `winnow` | `1.0.3` | DSL parser migration | Candidate for Phase 0 parser spike. |
| `arc-swap` | `1.9.1` | lock-free snapshot publication | Adopt for revision layer unless Phase 0 finds a blocker. |
| `blake3` | `1.8.5` | schema hash | Candidate; pure Rust and fast. |
| `uuid` | `1.23.1` | datastore ID | Candidate with explicit features only. |
| `smol_str` | `0.3.6` | compact validated identifiers | Candidate only if benchmarks show clone/storage pressure. |
| `arrayvec` | `0.7.6` | bounded small arrays | Prefer over `smallvec` initially because latest `smallvec` observed is alpha. |
| `rustc-hash` | `2.1.2` | fast internal hash maps | Candidate for benchmarked hot paths only. |
| `lasso` | `0.7.3` | optional string interner | Defer; compact store starts with a std-only interner. |
| `string-interner` | `0.20.0` | optional string interner | Defer; candidate only if local interner becomes maintenance-heavy. |
| `roaring` | `0.11.4` | optional compressed postings | Defer; use `Vec<RowId>` postings first. |
| `criterion` | `0.8.2` | performance benchmarks | Adopt as dev-dependency when benchmark phase starts. |
| `proptest` | `1.11.0` | property tests | Adopt as dev-dependency for store/schema invariants. |
| `rstest` | `0.26.1` | parameterized tests | Adopt as dev-dependency for validation matrices. |

Any dependency added during implementation must pass `cargo audit` and `cargo deny check`.

## 5. Rust Edition and Toolchain

- Rust edition: 2024.
- `rust-toolchain.toml` pins latest stable at implementation time.
- Crate root forbids unsafe code.
- Crate root enables rustc lints required by AGENTS.md: `rust_2024_compatibility`, `missing_docs`, and `missing_debug_implementations`.

## 6. Cross-References

- <- Depends on: [15-public-api-design.md](./15-public-api-design.md)
- -> Consumed by: [71-performance-budgets-design.md](./71-performance-budgets-design.md), [72-testing-verification-plan.md](./72-testing-verification-plan.md), [91-local-engine-impl-plan.md](./91-local-engine-impl-plan.md)
- Related research: [../docs/research/study-spicedb.md](../docs/research/study-spicedb.md)
