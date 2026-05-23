# 13 - Revision and Consistency Design

Status: draft v1
Owner: Simple Zanzibar
Depends on: [11-schema-system-design.md](./11-schema-system-design.md), [12-relationship-store-design.md](./12-relationship-store-design.md)

## 1. Purpose

The revision layer gives a local library deterministic snapshot semantics. It does not emulate Spanner, but it does let a caller write relationships, receive a token, and later check exactly at that token. SpiceDB's datastore exposes snapshot readers, head revisions, optimized revisions, revision parsing, and revision validation in `vendors/spicedb/pkg/datastore/datastore.go:693-722`; this design keeps the local subset.

## 2. Runtime State

```text
ZanzibarEngine
  |
  +-- ArcSwap<PublishedSnapshot>        read path: lock-free clone of Arc
  |
  +-- writer gate                       write path: serializes schema/store publication
        |
        v
     build new PublishedSnapshot
        |
        v
     assign Revision + ConsistencyToken
        |
        v
     publish Arc<PublishedSnapshot>
```

`PublishedSnapshot` contains:

```rust
pub struct PublishedSnapshot {
    pub revision: Revision,
    pub schema_hash: SchemaHash,
    pub schema: Arc<CompiledSchema>,
    pub relationships: Arc<RelationshipSnapshot>,
}
```

Reads clone the current `Arc<PublishedSnapshot>` and never block writes except for atomic pointer publication.

## 3. Revision and Token Types

```rust
pub struct Revision(NonZeroU64);

pub struct DatastoreId([u8; 16]);

pub struct SchemaHash([u8; 32]);

pub struct ConsistencyToken {
    revision: Revision,
    schema_hash: SchemaHash,
    datastore_id: DatastoreId,
}
```

Tokens support:

- `Display` for stable external string representation
- `FromStr` for exact-snapshot requests
- constant-time equality only if token secrecy is later required; tokens are not secrets in v2
- rejection when `datastore_id` does not match the engine instance
- rejection when revision is older than retained history

SpiceDB encodes datastore ID and schema hash in zedtokens in `vendors/spicedb/pkg/zedtoken/zedtoken.go:85-111`; v2 mirrors the concept with a smaller local token.

## 4. Consistency Modes

```rust
pub enum Consistency {
    Latest,
    Exact(ConsistencyToken),
}
```

`Latest` clones the current published snapshot. `Exact` looks up the retained snapshot by revision and validates token metadata. `AtLeastAsFresh` is deferred because a local library can usually keep exact tokens cheap, and adding another mode before consumers need it expands the API surface.

## 5. Retention

The in-memory engine retains the latest `N` published snapshots. Default `N = 32`. `N` is configured by builder and must be `NonZeroUsize`. When a token references an evicted revision, requests return `ConsistencyError::RevisionExpired`.

This is the local equivalent of SpiceDB memdb's revision window checks in `vendors/spicedb/internal/datastore/memdb/revisions.go:100-132`.

## 6. Write Publication Sequence

```text
Client              Engine Writer Gate             Snapshot History
  |                         |                              |
  | 1. write command        |                              |
  |------------------------>|                              |
  |                         | 2. validate against current  |
  |                         |    schema and relationships |
  |                         |                              |
  |                         | 3. build next snapshot       |
  |                         |----------------------------->|
  |                         | 4. assign revision           |
  |                         |    update history            |
  |                         |<-----------------------------|
  | 5. token                |                              |
  |<------------------------|                              |
```

Failures before step 4 publish nothing.

## 7. AGENTS Binding

- Error Handling: `ConsistencyError` is a `thiserror` enum and is wrapped by top-level `EngineError`.
- Async & Concurrency: use `ArcSwap` for lock-free read publication; writer serialization is explicit and outside the read path.
- Type Design & API: `Revision` is `NonZeroU64`; retention count is `NonZeroUsize`.
- Safety & Security: checked arithmetic for revision increment; overflow returns a domain error.
- Serialization: token string format is versioned.
- Testing: exact snapshot, expired revision, wrong datastore ID, and concurrent read/write tests.
- Logging & Observability: spans include revision and token status only.
- Performance: latest read path is one atomic load plus `Arc` clone.
- Documentation: consistency modes document stale-read behaviour and token lifetime.

## 8. Cross-References

- <- Depends on: [11-schema-system-design.md](./11-schema-system-design.md), [12-relationship-store-design.md](./12-relationship-store-design.md)
- -> Consumed by: [14-evaluation-engine-design.md](./14-evaluation-engine-design.md), [15-public-api-design.md](./15-public-api-design.md)
- Related research: [../docs/research/study-spicedb.md § Revision Tokens](../docs/research/study-spicedb.md#revision-tokens), `vendors/spicedb/internal/datastore/memdb/memdb.go:116-152`
