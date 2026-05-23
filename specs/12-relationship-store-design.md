# 12 - Relationship Store Design

Status: draft v1
Owner: Simple Zanzibar
Depends on: [10-local-engine-data-model-design.md](./10-local-engine-data-model-design.md), [11-schema-system-design.md](./11-schema-system-design.md)

## 1. Purpose

The relationship store owns relationship uniqueness, indexed query, mutation validation, preconditions, and snapshot materialization. It replaces the current `TupleStore::read_tuples(...) -> Vec<RelationTuple>` scan API with directional filters and iterators. SpiceDB exposes resource-side and subject-side readers in `vendors/spicedb/pkg/datastore/datastore.go:538-561`; this design adopts that split for a local in-memory backend.

## 2. Store Shape

```text
RelationshipStore
  |
  +-- uniqueness: HashSet<RelationshipKey>
  |
  +-- by_resource:
  |     (object_type, object_id, relation) -> Vec<RelationshipId>
  |
  +-- by_subject:
  |     (subject_type, subject_id, optional subject_relation) -> Vec<RelationshipId>
  |
  +-- rows:
        RelationshipId -> Relationship
```

Rows are immutable inside a snapshot. Write transactions build a new snapshot or structurally shared snapshot, then the revision layer publishes it.

Memory layout note: this file defines the logical store contract. The compact physical layout, identifier interning, `Vec<RowId>` postings, tombstones, and duplicate-ownership cleanup are specified in [16-compact-relationship-store-design.md](./16-compact-relationship-store-design.md). Implementations must satisfy both specs: this one for semantics, `16` for large-dataset memory shape.

## 3. Query Interface

```rust
pub struct RelationshipFilter {
    pub resource_type: ObjectType,
    pub optional_resource_id: Option<ObjectId>,
    pub optional_relation: Option<RelationName>,
    pub optional_subject: Option<SubjectFilter>,
    pub limit: QueryLimit,
}

pub struct SubjectFilter {
    pub subject_type: SubjectType,
    pub optional_subject_id: Option<SubjectId>,
    pub optional_relation: Option<RelationName>,
}

pub trait RelationshipReader {
    type Iter<'a>: Iterator<Item = &'a Relationship>
    where
        Self: 'a;

    fn query_relationships(&self, filter: &RelationshipFilter) -> Result<Self::Iter<'_>, StoreError>;
    fn reverse_query_relationships(&self, filter: &SubjectFilter) -> Result<Self::Iter<'_>, StoreError>;
}
```

The trait uses associated iterator types to avoid boxing in the default in-memory backend. Object-safe wrappers may be added later for extension backends.

## 4. Mutations

```rust
pub enum RelationshipMutation {
    Create(Relationship),
    Touch(Relationship),
    Delete(Relationship),
}

pub enum Precondition {
    MustMatch(RelationshipFilter),
    MustNotMatch(RelationshipFilter),
}
```

Semantics:

- `Create`: succeeds only if absent.
- `Touch`: inserts if absent and is idempotent if present.
- `Delete`: succeeds only if present.
- `MustMatch`: succeeds when at least one relationship matches the filter.
- `MustNotMatch`: succeeds when no relationship matches the filter.

This follows SpiceDB's transactional precondition shape in `vendors/spicedb/internal/services/v1/preconditions.go:17-55` and mutation distinction in `vendors/spicedb/internal/datastore/memdb/readwrite.go:50-132`.

## 5. Validation

Before mutations are applied:

- every relationship component is already a validated domain type
- schema resolver confirms resource relation exists
- schema resolver confirms subject type/relation is allowed
- mutation batch length is capped
- precondition count is capped
- duplicate mutations for the same key in a batch are rejected
- deletes for non-existent relationships return a typed error

Validation uses the schema snapshot paired with the write transaction's base revision.

## 6. Snapshot Publication

The store does not decide revisions. It returns a `RelationshipSnapshot` candidate to [13-revision-consistency-design.md](./13-revision-consistency-design.md), which assigns the revision and publishes the pair `(schema_snapshot, relationship_snapshot)`.

## 7. AGENTS Binding

- Error Handling: `StoreError` is a `thiserror` enum; no string errors.
- Async & Concurrency: core reader is synchronous and immutable. Writes are serialized by the revision publisher; no read-path locks.
- Type Design & API: filters use typed fields and capped `QueryLimit` newtypes.
- Safety & Security: all external filter builders enforce length/range caps before store access.
- Serialization: optional serde feature serializes request/response filters in camelCase.
- Testing: property tests assert uniqueness and index equivalence after random mutation batches.
- Logging & Observability: write spans include mutation counts and revision, not full relationship payloads by default.
- Performance: direct resource/relation/subject queries must not scan all rows.
- Documentation: public mutation semantics are documented with examples.

## 8. Cross-References

- <- Depends on: [10-local-engine-data-model-design.md](./10-local-engine-data-model-design.md), [11-schema-system-design.md](./11-schema-system-design.md)
- -> Consumed by: [13-revision-consistency-design.md](./13-revision-consistency-design.md), [14-evaluation-engine-design.md](./14-evaluation-engine-design.md)
- Refined by: [16-compact-relationship-store-design.md](./16-compact-relationship-store-design.md)
- Related research: [../docs/research/study-spicedb.md § Query Filters and Indexes](../docs/research/study-spicedb.md#query-filters-and-indexes), `vendors/spicedb/internal/datastore/memdb/readonly.go:108-232`
