# PRD - High Performance Local Rust Zanzibar Engine

Status: draft v1
Owner: Simple Zanzibar
Last updated: 2026-05-23

## 1. Problem

Simple Zanzibar currently demonstrates the Zanzibar authorization model, but its internals are still a toy architecture. The evaluator accepts one `NamespaceConfig`, which prevents correct cross-namespace userset handling. The tuple store scans a `HashSet` and returns cloned `Vec` results for every query. There is no schema type validation, revision token, snapshot read, transactional write, or lookup API. These gaps are documented in [../docs/research/study-spicedb.md](../docs/research/study-spicedb.md#purpose).

The target user needs an embedded Rust library that can answer authorization questions in-process with predictable latency, strong local invariants, and a path to future persistence. The library should borrow SpiceDB's layering, but not its distributed deployment machinery. SpiceDB separates request validation, schema type checks, revision selection, snapshot reads, dispatch, graph evaluation, and datastore queries; the research memo identifies those as the patterns to compress into Simple Zanzibar.

## 2. Vision

Simple Zanzibar v2 is a small, strict, high-performance local authorization engine:

```rust
let engine = ZanzibarEngine::builder()
    .compile_schema(schema_source)?
    .build()?;

let token = engine.write_relationships([
    RelationshipMutation::touch("doc:readme#viewer@user:alice")?,
])?;

let allowed = engine
    .at_exact_snapshot(token)
    .check("doc:readme#viewer@user:alice")?;

assert!(allowed.is_allowed());
```

The public API remains ergonomic, but internally every request flows through validated domain types, typed schema resolution, indexed relationships, revisioned snapshots, bounded graph evaluation, and typed error handling.

## 3. Goals

| # | Goal | Measure |
| --- | --- | --- |
| G1 | Replace the toy core with a schema-first local engine. | All existing behavior tests pass through the v2 engine facade, and invalid schema references fail at schema application time. |
| G2 | Make reads fast by design. | Direct checks over 100k relationships avoid full-store scans and use resource/subject indexes. Benchmark gates are defined in [71-performance-budgets-design.md](./71-performance-budgets-design.md). |
| G3 | Add deterministic local consistency. | Every schema or relationship write returns a token; exact-token checks read a stable snapshot. |
| G4 | Support core Zanzibar APIs as a library. | `check`, `expand`, `lookup_resources`, and `lookup_subjects` are backed by one engine and share validation, snapshots, and membership algebra. |
| G5 | Preserve a compatibility path. | Existing `ZanzibarService` examples and tests either keep working or fail with documented migration errors. |
| G6 | Meet project engineering policy. | `cargo build`, `cargo test`, `cargo +nightly fmt --check`, `cargo clippy -- -D warnings -W clippy::pedantic`, `cargo audit`, and `cargo deny check` pass before each implementation phase closes. |

## 4. Non-Goals

- No distributed service, gRPC server, remote dispatcher, replica coordination, or global consistency protocol.
- No SpiceDB caveat expression engine in v2 core. The membership type keeps a conditional variant reserved for a future phase.
- No watch API until an external consumer or persistent secondary index needs change streams.
- No production database backend in the initial rebuild. The first backend is an indexed, revisioned in-memory store.
- No full SpiceDB DSL compatibility promise. The parser should be SpiceDB-inspired and internally typed, but public compatibility is scoped by tests.
- No authn/authz HTTP middleware. This crate is an engine library used by applications that already authenticate callers.

## 5. Users

Primary users:

- Rust services that want embedded Zanzibar-style authorization without running SpiceDB.
- Library authors building local-first apps, test harnesses, or single-node control planes.
- Engineers learning Zanzibar internals from a smaller Rust implementation.

Secondary users:

- Future persistence-backend authors who need clean store traits.
- Benchmark and research users comparing graph-evaluation strategies.

Anti-personas:

- Teams needing multi-region consistency, centralized authorization-as-a-service, or production-grade change streams. They should use SpiceDB/OpenFGA rather than this local library.

## 6. Success Metrics

- All legacy tests pass through the v2 compatibility facade.
- At least 20 schema-validation negative tests reject invalid computed-userset, tuple-to-userset, and duplicate relation definitions.
- At least 10 property tests cover relationship-store index consistency.
- Direct-check benchmark demonstrates indexed lookup rather than scan behavior at 10k, 100k, and 1M relationship scales.
- Exact snapshot read test proves a pre-write token cannot observe later writes.
- Public docs include one compile-tested example for each core API.

## 7. Naming Conventions

- Product name: `Simple Zanzibar`.
- New public engine type: `ZanzibarEngine`.
- Internal v2 modules use explicit names: `domain`, `schema`, `relationship`, `revision`, `engine`, `api`.
- External string syntax remains Zanzibar-like: `object_type:object_id#relation@subject_type:subject_id` and `object_type:object_id#relation@subject_type:subject_id#subject_relation`.
- Internal identifiers are newtypes, not raw `String`.
- Public fallible constructors use `TryFrom` or `FromStr` where parsing is involved.

## 8. Prior-Art Commitments

- Adopt SpiceDB's separation between schema validation, snapshot reader, relationship queries, and graph evaluation. See `vendors/spicedb/internal/services/v1/permissions.go:78-137` and `vendors/spicedb/internal/dispatch/graph/graph.go:274-361`.
- Adopt resource-side and subject-side relationship query contracts. See `vendors/spicedb/pkg/datastore/datastore.go:538-561`.
- Adopt local revision tokens with schema hash and datastore identity, compressed for a single-process library. See `vendors/spicedb/pkg/zedtoken/zedtoken.go:70-111`.
- Defer distributed dispatcher, caching, singleflight, caveats, watch, and full query planner per [../docs/research/study-spicedb.md § What Simple Zanzibar Should Defer or Avoid](../docs/research/study-spicedb.md#what-simple-zanzibar-should-defer-or-avoid).

## 9. Cross-References

- Depends on: [../docs/research/study-spicedb.md](../docs/research/study-spicedb.md), [0001-design.md](./0001-design.md)
- Consumed by: [10-local-engine-data-model-design.md](./10-local-engine-data-model-design.md), [90-local-engine-roadmap.md](./90-local-engine-roadmap.md), [91-local-engine-impl-plan.md](./91-local-engine-impl-plan.md)
