# 18 - Trusted Fast Snapshot Load Design

Status: draft v1
Owner: Simple Zanzibar
Last updated: 2026-05-23
Depends on: [17](./17-compact-snapshot-format-design.md), [70](./70-security-design.md), [71](./71-performance-budgets-design.md)

## 1. Purpose

The first compact snapshot loader is safe for untrusted files and validates the semantic shape of
every row and index posting on every process start. That is the right default boundary, but the
measured 1M-rule load cost is dominated by repeated semantic proof work, not file I/O. Deployments
that generate snapshots in a trusted build pipeline need a second path: validate the artifact once
at build time, bind the bytes with an external content-address or signature layer, and let startup
perform only structural checks plus cheap adoption of precomputed runtime tables.

This spec adds a trusted fast-load profile for `.szsnap` v2. The goal is not to weaken the default
loader; it is to move expensive validation to the artifact producer and make the runtime trust
boundary explicit in the API.

## 2. Goals

| # | Goal | Measure |
| --- | --- | --- |
| G1 | Load a trusted 1M-rule snapshot in serverless/container cold-start budgets. | `snapshot_load_trusted_fast/1m` Criterion upper estimate <= 200 ms on the reference machine. |
| G2 | Keep the default loader safe for hostile files. | `SnapshotValidationMode::Full` remains the default and continues to reject corrupt semantic data. |
| G3 | Keep trusted mode bounded and memory safe. | Trusted mode still validates section bounds, row-id bounds, sorted ranges, and UTF-8 symbol bytes before publishing. Checksum remains available, while the <= 200 ms path uses explicit external integrity. |
| G4 | Preserve post-load behavior. | Trusted-loaded snapshots pass check, expand, lookup, exact-token, and subsequent-write tests. |
| G5 | Make trust explicit. | Public load options name the mode and docs state that trusted mode is for build-pipeline artifacts only. |

## 3. Non-Goals

- No cryptographic signing implementation in this phase. `SnapshotIntegrityMode::External` is an
  API boundary for deployments that already verify artifact bytes by an OCI digest, signed manifest,
  release checksum, or equivalent supply-chain layer.
- No memory mapping. The format remains std-file-read based under AGENTS.md's no-unsafe rule.
- No compression. The 200 ms budget is for uncompressed local artifacts.
- No silent fallback from trusted to full validation. Mode choice is explicit and observable.

## 4. Measured Bottleneck

The 2026-05-23 phase timer on the 1M-rule artifact showed:

| Stage | Baseline |
| --- | ---: |
| file read | ~10 ms |
| checksum/envelope | ~52 ms |
| symbol interner maps | ~85 ms |
| relationship rows | ~313 ms |
| indexes | ~113 ms |
| total | ~572 ms |

The row stage is almost entirely repeated semantic proof:

| Experiment | Row stage |
| --- | ---: |
| full row validation | ~313 ms |
| skip domain validation only | ~123 ms |
| skip duplicate-row uniqueness only | ~200 ms |
| skip both row semantic checks | ~31 ms |

The measured viable lower bound trusts row/index semantics and avoids eager Rust `HashMap`
construction for symbols. Keeping the in-process checksum stayed close but above the hard gate
after full format validation (~205 ms). Explicit external integrity removes the extra whole-file
hash during startup and measured `[151.06 ms, 152.23 ms, 153.35 ms]`.

## 5. Trust Model

```text
                 Build / CI Trust Boundary                 Runtime Trust Boundary
        ┌──────────────────────────────────────┐     ┌──────────────────────────────┐
        │ Source DSL + relationship data       │     │ App process loads .szsnap    │
        │                                      │     │                              │
        │ 1. Build compact store               │     │ 1. Read bounded regular file │
        │ 2. Full semantic validation          │     │ 2. Validate header/sections  │
        │ 3. Emit canonical rows/indexes       │────▶│ 3. Verify external identity  │
        │ 4. Emit symbol hash/lookup sections  │     │ 4. Adopt prevalidated rows   │
        │ 5. Run snapshot validation tests     │     │ 5. Publish service snapshot  │
        └──────────────────────────────────────┘     └──────────────────────────────┘
```

`SnapshotValidationMode::Full` assumes the file is hostile and proves all semantic invariants at
load time.

`SnapshotValidationMode::TrustedFastLoad` assumes the artifact came from a trusted build pipeline
that already ran full validation. The loader still rejects malformed binary structure because files
remain external input. It does not re-prove row domain grammar, duplicate-row absence, or that every
posting semantically matches its key.

## 6. Format Changes

`.szsnap` moves to format version 2 before the artifact is public. Version 2 adds two required
sections:

| Section | Kind | Row | Purpose |
| --- | ---: | --- | --- |
| `symbol_hashes` | 9 | `u64 hash` | Hashes in symbol-id order. Full validation computes hashes while scanning `symbol_bytes` and compares them. |
| `symbol_lookup` | 10 | `u32 symbol_id` | Stable sorted permutation by `(symbol_hashes[id], symbol_id)` for trusted fast-load queries. |

Both sections have exactly `symbol_count` rows. Full validation checks `symbol_hashes` against the
symbol bytes while building normal lookup maps. Trusted mode validates lookup length, strict sorted
order, and symbol-id bounds, then adopts `symbol_hashes + symbol_lookup` without rebuilding Rust
`HashMap`s. A strict sorted `symbol_count`-length in-bounds permutation proves every symbol id is
covered without a separate seen-id bitmap.

The section directory stays fixed-width. Version 2 readers reject version 1 artifacts because no
public compatibility promise has been made yet.

## 7. Loader Modes

```rust
pub enum SnapshotValidationMode {
    Full,
    TrustedFastLoad,
}
```

`SnapshotLoadOptions` gains `validation: SnapshotValidationMode`, defaulting to `Full`.

```rust
pub enum SnapshotIntegrityMode {
    Checksum,
    External,
}
```

`SnapshotLoadOptions` also gains `integrity: SnapshotIntegrityMode`, defaulting to `Checksum`.
`External` is accepted only with `TrustedFastLoad`; it preserves footer placement checks but skips
the BLAKE3 rehash because byte identity is assumed to have been proven before process startup.

Trusted mode is valid only with `SnapshotLoadProfile::FastLoad`. Latency-profile loading rebuilds
hash indexes and is intentionally outside the 200 ms target.

## 8. Trusted Runtime Behavior

Trusted mode performs:

- regular-file check and byte cap;
- header, version, flags, file length, section count, section bounds, duplicate-section, overlap,
  and footer placement checks;
- checksum verification when `SnapshotIntegrityMode::Checksum` is selected, or explicit reliance on
  an external byte-identity proof when `SnapshotIntegrityMode::External` is selected;
- schema parse/compile and schema hash check;
- symbol table length/range/UTF-8 checks;
- symbol hash length checks;
- symbol lookup length, sorted-order, row-count, and symbol-id bounds checks;
- relationship row length and symbol-id bounds checks;
- index directory/key/range/posting-row-id length and bounds checks.

Trusted mode skips:

- per-row domain grammar validation;
- duplicate relationship row detection;
- per-posting row/key semantic matching;
- per-index full coverage proof;
- eager symbol `HashMap` construction.

For subsequent writes, trusted-loaded stores build the relationship uniqueness index lazily on the
first mutation. If that lazy build discovers duplicate rows, the mutation returns a typed
`StoreError::InternalInvariant` through the existing snapshot/store error chain.

## 9. Correctness Requirements

- Full mode and trusted mode must produce identical check/expand/lookup results for valid artifacts.
- Exact consistency tokens from the writer process remain rejected after load.
- Writes after trusted load must preserve create/touch/delete semantics.
- Full mode corrupt-file tests continue to cover duplicate symbols, bad symbol ids, bad row ids,
  bad posting ranges, bad index ordering, bad checksums, and missing sections.
- External integrity mode must be explicit, must be rejected outside trusted fast-load, and must not
  be the default.
- Trusted mode gets separate tests proving that structural corruptions are still rejected.

## 10. Performance Requirements

| Benchmark | Dataset | Gate |
| --- | --- | ---: |
| `snapshot_load_trusted_fast/1m` (`TrustedFastLoad + External`) | 1M org rules | Criterion upper estimate <= 200 ms |
| `snapshot_load_compact/1m` full mode | 1M org rules | Criterion upper estimate <= 700 ms |
| trusted direct check after load | 1M org rules | <= 10 us |
| trusted inherited check after load | 1M org rules | <= 25 us |
| trusted lookup resources after load | 1M org rules | <= 10 ms |

If trusted query latency regresses versus full mode but remains inside query budgets, the load-speed
tradeoff is acceptable and must be documented in [71](./71-performance-budgets-design.md).

2026-05-23 implementation evidence:

| Benchmark | Criterion estimate |
| --- | ---: |
| `snapshot_load_trusted_fast/1m` | `[151.06 ms, 152.23 ms, 153.35 ms]` |
| `snapshot_load_compact/1m` full mode | `[575.82 ms, 580.38 ms, 585.11 ms]` |
| `snapshot_trusted_loaded_check_direct/1m` | `[3.0610 us, 3.1232 us, 3.1971 us]` |
| `snapshot_trusted_loaded_check_inherited/1m` | `[7.2171 us, 7.2732 us, 7.3198 us]` |
| `snapshot_trusted_loaded_lookup_resources/1m` | `[3.8150 ms, 3.9764 ms, 4.1401 ms]` |

## 11. AGENTS.md Binding

- Error Handling: `SnapshotIoError` remains the public loader error type; store invariant failures
  are propagated with `#[source]`.
- Safety & Security: no `unsafe`; no `unwrap`/`expect`/unchecked indexing in production loader code;
  all external byte ranges and counts are checked before use.
- Type Design: trusted mode is an enum value, not a boolean, so the trust boundary is explicit.
- Testing: add integration, corrupt-file, property, and benchmark coverage for both validation modes.
- Performance: use Criterion; update specs only with measured evidence.

## 12. Cross-References

- Extends format design: [17](./17-compact-snapshot-format-design.md)
- Threat model: [70](./70-security-design.md)
- Performance gates: [71](./71-performance-budgets-design.md)
- Verification plan: [72](./72-testing-verification-plan.md)
