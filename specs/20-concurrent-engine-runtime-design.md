# 20 - Concurrent Engine Runtime Design

Status: implemented v1
Owner: Simple Zanzibar
Depends on: [13-revision-consistency-design.md](./13-revision-consistency-design.md), [15-public-api-design.md](./15-public-api-design.md), [19-public-api-completeness-design.md](./19-public-api-completeness-design.md)

## 1. Purpose

This spec removes the legacy mutable public facade and makes `ZanzibarEngine`
the only crate-facing runtime. Reads must be lock-free over immutable published snapshots. Writes
must be serialized by a single writer actor, optimized for batch mutation, and horizontally scaled
by tenant-level sharding rather than fine-grained relationship locks.

The design makes the existing [13-revision-consistency-design.md](./13-revision-consistency-design.md)
contract true in code: latest reads are one atomic snapshot load plus graph evaluation, and write
publication is the only place that mutates engine state.

## 2. Architecture

```text
                         ┌───────────────────────────────────────────────┐
                         │              ZanzibarEngine                  │
                         │                                               │
                         │  ┌─────────────────────────────────────────┐  │
Read APIs ───────────────┼─▶│ ArcSwapOption<EngineState>              │  │
check / expand / lookup  │  │ - latest PublishedSnapshot              │  │
export / save snapshot   │  │ - retained exact-snapshot history       │  │
                         │  │ - datastore id + latest revision        │  │
                         │  │ - evaluation limits                     │  │
                         │  └─────────────────────────────────────────┘  │
                         │                         ▲                     │
Write APIs ──────────────┼──────────────┐          │ atomic publish      │
schema / relationships   │              │          │                     │
policy import            │              ▼          │                     │
                         │  ┌─────────────────────────────────────────┐  │
                         │  │ Single Writer Actor                    │  │
                         │  │ - bounded command queue                │  │
                         │  │ - owns mutable WriterState             │  │
                         │  │ - batches caller-supplied mutations    │  │
                         │  │ - validates preconditions atomically   │  │
                         │  └─────────────────────────────────────────┘  │
                         └───────────────────────────────────────────────┘

              ┌────────────────────────────────────────────────────────┐
              │ ZanzibarTenantShards                                  │
              │                                                        │
              │ ArcSwap<HashMap<TenantId, Arc<ZanzibarEngine>>>        │
              │ - existing tenant lookup is lock-free                  │
              │ - tenant creation clones the map under a short gate    │
              │ - each tenant has independent writer actor/revisions   │
              └────────────────────────────────────────────────────────┘
```

## 3. Public Interface

The legacy mutable facade is removed from the public API. The public runtime surface is:

```rust
pub struct ZanzibarEngine { /* private */ }
pub struct ZanzibarEngineBuilder { /* private */ }

impl ZanzibarEngine {
    pub fn builder() -> ZanzibarEngineBuilder;
    pub fn check(&self, request: CheckRequest) -> Result<CheckResponse, EngineError>;
    pub fn check_relation(&self, object: &Object, relation: &Relation, user: &User) -> Result<bool, EngineError>;
    pub fn expand(&self, request: ExpandRequest) -> Result<ExpandResponse, EngineError>;
    pub fn expand_relation(&self, object: &Object, relation: &Relation) -> Result<ExpandedUserset, EngineError>;
    pub fn lookup_resources(&self, request: impl Borrow<LookupResourcesRequest>) -> Result<LookupResources, EngineError>;
    pub fn lookup_subjects(&self, request: impl Borrow<LookupSubjectsRequest>) -> Result<LookupSubjects, EngineError>;
    pub fn lookup_permissions(&self, request: impl Borrow<LookupPermissionsRequest>) -> Result<LookupPermissions, EngineError>;
    pub fn lookup_object_permissions(&self, request: impl Borrow<LookupObjectPermissionsRequest>) -> Result<LookupObjectPermissions, EngineError>;
    pub fn add_dsl(&self, dsl: &str) -> Result<(), EngineError>;
    pub fn apply_schema(&self, source: SchemaSource<'_>) -> Result<ConsistencyToken, EngineError>;
    pub fn write_relationships(&self, mutations: impl IntoIterator<Item = RelationshipMutation>) -> Result<ConsistencyToken, EngineError>;
    pub fn write_relationships_with_preconditions(
        &self,
        mutations: impl IntoIterator<Item = RelationshipMutation>,
        preconditions: impl IntoIterator<Item = Precondition>,
    ) -> Result<ConsistencyToken, EngineError>;
    pub fn save_snapshot(&self, path: impl AsRef<Path>, options: SnapshotSaveOptions) -> Result<(), SnapshotIoError>;
    pub fn load_snapshot(path: impl AsRef<Path>, options: SnapshotLoadOptions) -> Result<Self, SnapshotIoError>;
}

pub struct ZanzibarTenantShards { /* private */ }
pub struct TenantId { /* private */ }

impl ZanzibarTenantShards {
    pub fn new(builder: ZanzibarEngineBuilder) -> Self;
    pub fn get(&self, tenant: &TenantId) -> Option<Arc<ZanzibarEngine>>;
    pub fn get_or_create(&self, tenant: TenantId) -> Arc<ZanzibarEngine>;
    pub fn tenants(&self) -> Vec<TenantId>;
}
```

`write_relationships` remains the primary write API and accepts batches. Single-relationship
helpers remain convenience wrappers and are not optimized as a separate write path.

## 4. Invariants

- I1: Public reads never acquire a `Mutex` or `RwLock`; they clone `Arc<EngineState>` from
  `ArcSwapOption`.
- I2: Every successful write publishes exactly one new `PublishedSnapshot`, increments the local
  revision by one, and returns a token for that revision.
- I3: Failed writes publish no state and leave the previously loaded `EngineState` observable.
- I4: Exact consistency tokens are scoped to one tenant engine because each tenant has its own
  datastore id.
- I5: The writer actor is the only owner of mutable schema, relationship, and snapshot-history
  state for one engine.
- I6: Tenant sharding is a runtime partitioning boundary. Relationships are not tagged with tenant
  id inside one engine.

## 5. Behaviour

### Reads

Latest reads fail with `SchemaRequired` if no snapshot has been published. Exact reads validate
datastore id, revision range, retained-history presence, and schema hash using the immutable
`EngineState` loaded at the beginning of the request.

Policy export and snapshot save clone the latest `PublishedSnapshot` first and perform sorting,
serialization, compression, and file I/O after the atomic load. They do not hold a writer gate.

### Writes

Every write command travels through a bounded sync channel to the writer actor. The public call
waits on a one-shot response channel and returns the typed result. If the actor has shut down or
panicked, the call returns `EngineError::WriterUnavailable`.

The writer actor processes one command at a time:

```text
Client               Writer Actor                 ArcSwap EngineState
  │                        │                                  │
  │ 1. command + response  │                                  │
  │───────────────────────▶│                                  │
  │                        │ 2. validate against owned state  │
  │                        │ 3. build candidate store/schema  │
  │                        │ 4. assign revision/token         │
  │                        │ 5. publish Arc<EngineState> ────▶│
  │                        │                                  │
  │ 6. typed result ◀──────│                                  │
```

Batch writes are the throughput path. Callers with high write rates should accumulate related
relationship mutations into one `write_relationships` call so the engine pays one queue handoff,
one validation pass, and one snapshot publication.

### Tenant Shards

`ZanzibarTenantShards` is a convenience owner for many independent engines. Existing tenant lookups
load an immutable map through `ArcSwap`, so routing a request to a tenant has no lock in the hot
path. Creating a missing tenant takes a short creation gate, clones the small tenant map, inserts a
new `Arc<ZanzibarEngine>`, and atomically publishes the new map.

This is the only write-scaling strategy in this milestone. Fine-grained locks inside one tenant are
deferred because preconditions, schema changes, global revision tokens, and snapshot publication all
require a single linearized write order.

## 6. Performance Verification

The benchmark suite adds `concurrent_runtime` with these scenarios:

| Scenario | Shape | Required output |
| --- | --- | --- |
| read-heavy/light-write | many reader threads, one small batched writer | read throughput, write p50/p95 |
| read-heavy/medium-write unbatched | many reader threads, writers submit one mutation at a time | read throughput, write p50/p95 |
| read-heavy/medium-write batched | same logical write volume batched by 100 | read throughput, write p50/p95 |
| read-heavy/heavy-write unbatched | many readers and sustained single-mutation writers | read throughput, write p50/p95 |
| read-heavy/heavy-write batched | same logical write volume batched by 100 or 1k | read throughput, write p50/p95 |
| tenant-sharded heavy-write | same total logical write volume split across tenants | per-tenant and aggregate write throughput |

The benchmark must report results in the PR comment. The first implementation records baselines
rather than setting hard gates; future gates are allowed only after the new runtime has evidence.

## 7. AGENTS Binding

- Error Handling: actor send/receive failures map to typed `EngineError`; no `anyhow` in library.
- Async & Concurrency: the runtime uses message passing and an actor that owns mutable state. The
  public API stays synchronous per [99-key-decisions.md D5](./99-key-decisions.md#d5---keep-the-core-synchronous).
- Type Design & API: `TenantId` is a validated newtype; writer queue capacity is `NonZeroUsize`.
- Safety & Security: no unsafe; no unchecked indexing; file, policy, schema, and relationship
  inputs keep existing validation boundaries.
- Serialization: unchanged request/response serde feature behaviour.
- Testing: actor lifecycle, failed-write atomicity, concurrent read/write, and tenant isolation
  tests are required.
- Logging & Observability: existing `tracing` API spans remain at the public method boundary.
- Performance: reads must not take locks; benchmark evidence must cover write batching and sharding.
- Documentation: public docs and examples use `ZanzibarEngine`, not the legacy mutable facade.

## 8. Cross-References

- <- Depends on: [13-revision-consistency-design.md](./13-revision-consistency-design.md), [15-public-api-design.md](./15-public-api-design.md), [19-public-api-completeness-design.md](./19-public-api-completeness-design.md)
- -> Consumed by: [71-performance-budgets-design.md](./71-performance-budgets-design.md), [72-testing-verification-plan.md](./72-testing-verification-plan.md), [90-local-engine-roadmap.md](./90-local-engine-roadmap.md), [91-local-engine-impl-plan.md](./91-local-engine-impl-plan.md)
- Related research: [../docs/research/study-spicedb.md § Relationship Writes](../docs/research/study-spicedb.md#relationship-writes), [../docs/research/study-spicedb.md § Revision Tokens](../docs/research/study-spicedb.md#revision-tokens)
