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
