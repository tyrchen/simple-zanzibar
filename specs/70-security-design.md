# 70 - Security Design

Status: draft v1
Owner: Simple Zanzibar
Depends on: [10-local-engine-data-model-design.md](./10-local-engine-data-model-design.md), [15-public-api-design.md](./15-public-api-design.md)

## 1. Purpose

This spec defines security and safety requirements for a local authorization engine. The engine is not an authentication system and does not secure a network boundary by itself, but it must be robust against hostile input passed through its library API.

## 2. Threat Model

In scope:

- malformed schema text
- malformed relationship strings
- excessive identifier lengths
- fanout-heavy relationship graphs
- recursive schema and relationship structures
- untrusted serialized request DTOs when `serde` feature is enabled
- log injection through object IDs or subject IDs

Out of scope:

- network TLS
- OAuth/session auth
- multi-tenant service isolation
- persistent database compromise

## 3. Security Controls

| Risk | Control | Spec |
| --- | --- | --- |
| parser memory exhaustion | schema source and identifier byte caps | [10](./10-local-engine-data-model-design.md), [11](./11-schema-system-design.md) |
| malformed names | allowlist validation, reject not sanitize | [10](./10-local-engine-data-model-design.md) |
| graph DoS | max depth, fanout limits, lookup limits | [14](./14-evaluation-engine-design.md) |
| stale/wrong token use | datastore ID and schema hash validation | [13](./13-revision-consistency-design.md) |
| panic on hostile input | no `unwrap`/`expect` in production paths | all implementation phases |
| log injection | reject control chars, structured tracing | [10](./10-local-engine-data-model-design.md), [15](./15-public-api-design.md) |

## 4. Unsafe Policy

The crate root uses `#![forbid(unsafe_code)]`. Dependencies containing unsafe are allowed only if cargo-deny permits them and the dependency is justified in [99-key-decisions.md](./99-key-decisions.md).

## 5. AGENTS Binding

This spec directly binds AGENTS.md § Safety & Security:

- validate at API/deserialization boundaries
- byte length caps on every external string
- charset allowlists, not blocklists
- bounded collections for external inputs
- numeric ranges for limits
- no panics reachable from user data
- checked arithmetic for revisions and fanout counters

## 6. Verification

Security verification lives in [72-testing-verification-plan.md](./72-testing-verification-plan.md) and includes:

- parser rejection tests
- relationship string fuzz/property tests
- graph depth exhaustion tests
- token mismatch tests
- `rg` gate for `unwrap(`, `expect(`, unimplemented macros, `unreachable!`, and indexing in boundary modules
- `cargo audit`
- `cargo deny check`

## 7. Cross-References

- <- Depends on: [10-local-engine-data-model-design.md](./10-local-engine-data-model-design.md), [15-public-api-design.md](./15-public-api-design.md)
- -> Consumed by: [72-testing-verification-plan.md](./72-testing-verification-plan.md), [91-local-engine-impl-plan.md](./91-local-engine-impl-plan.md)
