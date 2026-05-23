# Key Decisions

Status: draft v1
Owner: Simple Zanzibar
Last updated: 2026-05-23

Each decision is load-bearing. Supersede with a new decision entry rather than silently rewriting history.

## D1 - Rebuild the core inside the existing crate

- Context: The current implementation is a toy, but its tests and examples encode useful behavior.
- Alternatives considered: keep patching current internals; start a new repository; rebuild v2 in the same crate.
- Decision: rebuild v2 internals in the same crate and keep a compatibility facade until v2 covers legacy behavior.
- Why: preserves user-visible continuity while avoiding architectural debt from `NamespaceConfig + Vec scan + recursive eval`.
- Pinned by: [00-local-engine-prd.md](./00-local-engine-prd.md), [15-public-api-design.md](./15-public-api-design.md), [91-local-engine-impl-plan.md](./91-local-engine-impl-plan.md)
- Date: 2026-05-23

## D2 - Schema validation happens before runtime evaluation

- Context: SpiceDB validates schema references before serving requests in `vendors/spicedb/pkg/schema/typesystem_validation.go:37-288`.
- Alternatives considered: validate lazily during check; validate only parser syntax; full SpiceDB type system.
- Decision: compile and type-check schemas before publication with a smaller local rule set.
- Why: invalid policies must fail deterministically at apply time, not during an authorization decision.
- Pinned by: [11-schema-system-design.md](./11-schema-system-design.md), [12-relationship-store-design.md](./12-relationship-store-design.md), [14-evaluation-engine-design.md](./14-evaluation-engine-design.md)
- Date: 2026-05-23

## D3 - Use resource and subject indexes in the in-memory store

- Context: SpiceDB exposes both `QueryRelationships` and `ReverseQueryRelationships` in `vendors/spicedb/pkg/datastore/datastore.go:538-561`.
- Alternatives considered: keep `HashSet` scan; add only resource index; add resource and subject indexes.
- Decision: maintain both resource-side and subject-side indexes.
- Why: direct checks need resource lookup, lookup resources needs subject lookup, and keeping both in the first real store avoids redesign.
- Pinned by: [12-relationship-store-design.md](./12-relationship-store-design.md), [14-evaluation-engine-design.md](./14-evaluation-engine-design.md)
- Date: 2026-05-23

## D4 - Add local revision tokens without distributed consistency

- Context: SpiceDB tokens carry revision, datastore identity, and schema hash in `vendors/spicedb/pkg/zedtoken/zedtoken.go:85-111`.
- Alternatives considered: no tokens; raw revision number only; full SpiceDB zedtoken compatibility.
- Decision: use a local token containing revision, schema hash, and datastore ID.
- Why: deterministic snapshot reads are useful in-process, while full distributed consistency is outside scope.
- Pinned by: [13-revision-consistency-design.md](./13-revision-consistency-design.md), [15-public-api-design.md](./15-public-api-design.md)
- Date: 2026-05-23

## D5 - Keep the core synchronous

- Context: The target is an embedded local library with immutable snapshots and no network I/O.
- Alternatives considered: Tokio actor engine; async traits everywhere; synchronous core with optional future async wrapper.
- Decision: implement the v2 core as synchronous snapshot reads and serialized writes.
- Why: avoids runtime requirements and keeps hot-path checks cheap; AGENTS.md async guidance is marked N/A where no async work exists.
- Pinned by: [13-revision-consistency-design.md](./13-revision-consistency-design.md), [14-evaluation-engine-design.md](./14-evaluation-engine-design.md), [15-public-api-design.md](./15-public-api-design.md)
- Date: 2026-05-23

## D6 - Defer caveats, watch, distributed dispatch, and full query planner

- Context: The SpiceDB research memo identifies these as powerful but not necessary for the first serious local engine.
- Alternatives considered: port all SpiceDB subsystems; implement caveats early; keep v2 focused.
- Decision: reserve type shapes where useful, but defer these features.
- Why: typed schema, indexed storage, snapshots, and bounded evaluation are the foundations; adding advanced SpiceDB features first would obscure correctness work.
- Pinned by: [00-local-engine-prd.md](./00-local-engine-prd.md), [14-evaluation-engine-design.md](./14-evaluation-engine-design.md), [90-local-engine-roadmap.md](./90-local-engine-roadmap.md)
- Date: 2026-05-23

## D7 - Keep `pest` for M0 and migrate parser internals later

- Context: Phase 0 compared the current parser risk against the M0 requirement to compile the legacy DSL into validated v2 schema IR. `cargo search` on 2026-05-23 confirmed `winnow = 1.0.3`, matching [60-crates-features-design.md](./60-crates-features-design.md), and AGENTS.md prefers `winnow` for string grammars.
- Alternatives considered: migrate the parser to `winnow` before M0; keep `pest` permanently; keep `pest` for M0 and migrate after schema validation is stable.
- Decision: keep the current `pest` parser through M0, compile its output into the v2 schema IR, and defer parser-internal migration until after the schema validator and compatibility facade are green.
- Why: M0's risk is semantic validation, not syntax recognition. A parser rewrite before the IR exists would churn tests without retiring the invalid-reference risk the roadmap calls out.
- Pinned by: [11-schema-system-design.md](./11-schema-system-design.md), [60-crates-features-design.md](./60-crates-features-design.md), [91-local-engine-impl-plan.md](./91-local-engine-impl-plan.md)
- Date: 2026-05-23

## D8 - Use `arc-swap` for snapshot publication

- Context: Phase 0 validated the desired `ArcSwap<PublishedSnapshot>` shape with a dev-only probe test that publishes an `Arc`, stores a replacement, and loads the current snapshot with pointer identity preserved. `cargo search` on 2026-05-23 confirmed `arc-swap = 1.9.1`.
- Alternatives considered: `ArcSwap`; `RwLock<Arc<PublishedSnapshot>>`; cloning snapshots through a writer-owned field.
- Decision: adopt `arc-swap = 1.9.1` for the revision layer when Phase 3 adds `PublishedSnapshot`.
- Why: it directly supports the design's read path: one atomic load plus an `Arc` clone, without a read-path mutex.
- Pinned by: [13-revision-consistency-design.md](./13-revision-consistency-design.md), [60-crates-features-design.md](./60-crates-features-design.md), [91-local-engine-impl-plan.md](./91-local-engine-impl-plan.md)
- Date: 2026-05-23

## D9 - Use `blake3` for canonical schema hashes

- Context: Phase 0 compared a std-only fallback against a dependency-backed 32-byte digest. `cargo search` on 2026-05-23 confirmed `blake3 = 1.8.5`, matching [60-crates-features-design.md](./60-crates-features-design.md).
- Alternatives considered: std-only `DefaultHasher`; `blake3`; defer hash selection until consistency tokens.
- Decision: use `blake3 = 1.8.5` for `SchemaHash` once schema canonicalization lands.
- Why: consistency tokens require a stable digest across process runs and Rust versions. `DefaultHasher` is intentionally not stable, while BLAKE3 provides a fixed 32-byte output and a small API surface.
- Pinned by: [11-schema-system-design.md](./11-schema-system-design.md), [13-revision-consistency-design.md](./13-revision-consistency-design.md), [60-crates-features-design.md](./60-crates-features-design.md)
- Date: 2026-05-23

## D10 - Record the legacy scan baseline before indexed storage

- Context: Phase 0 added `benches/baseline.rs` to measure the current `HashSet` scan path before Phase 2 replaces it with indexed relationship reads. `cargo search` on 2026-05-23 confirmed `criterion = 0.8.2`.
- Alternatives considered: ad hoc timing in tests; Criterion benchmark harness; wait until indexed storage exists.
- Decision: keep a Criterion baseline benchmark for `legacy_direct_check_scan_100k` and `legacy_store_read_tuples_scan_100k`.
- Why: the benchmark makes the Phase 2 performance delta observable and prevents the project from inventing targets without measurement.
- Pinned by: [71-performance-budgets-design.md](./71-performance-budgets-design.md), [91-local-engine-impl-plan.md](./91-local-engine-impl-plan.md)
- Date: 2026-05-23
