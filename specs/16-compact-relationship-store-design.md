# 16 - Compact Relationship Store Design

Status: draft v1
Owner: Simple Zanzibar
Depends on: [12-relationship-store-design.md](./12-relationship-store-design.md), [13-revision-consistency-design.md](./13-revision-consistency-design.md), [14-evaluation-engine-design.md](./14-evaluation-engine-design.md)
Last updated: 2026-05-23

## 1. Problem

The current indexed in-memory store meets the latency goal, but misses the memory shape required for medium-to-large org authorization datasets. The `org_authorization` benchmark on 2026-05-23 measured roughly:

| Dataset | Max RSS | Approximate overhead |
| --- | ---: | ---: |
| lightweight Criterion baseline | 9.7 MiB | - |
| 1k relationships | 12.8 MiB | 3.1 MiB |
| 100k relationships | 324 MiB | 315 MiB |
| 1M relationships | 3.12 GiB | 3.11 GiB |

The hot-path latency stays flat from 1k to 1M relationships, but the resident set is approximately `3.1 KiB / relationship`. That is too high for an embedded authorization library. A 1M relationship local engine should fit comfortably in a service process without consuming multiple GiB before application state is loaded.

The primary cause is not the Zanzibar model. It is duplicated ownership and pointer-heavy indexes:

- `IndexedRelationshipStore` stores owned `Relationship` values in both `rows` and `uniqueness`.
- Every index key clones string-backed domain types.
- Posting lists use `BTreeSet<usize>`, allocating tree nodes for row ids.
- `publish_snapshot` stores one relationship copy in the writer-owned store and another clone inside `PublishedSnapshot`.
- The compatibility `InMemoryTupleStore` can retain another full legacy tuple representation.

## 2. Goals

| # | Goal | Measure |
| --- | --- | --- |
| G1 | Reduce steady-state RSS by one order of magnitude for large org datasets. | `org_authorization/1m_rules/check_direct_group_viewer` max RSS <= 400 MiB on the reference machine, excluding future Criterion variance. |
| G2 | Preserve current read latency. | Direct check p95 remains <= 10 us over 1M relationships; inherited check p95 remains <= 25 us. |
| G3 | Preserve public API compatibility. | Existing `Relationship`, `RelationshipFilter`, `SubjectFilter`, `ZanzibarEngine`, consistency, check, expand, and lookup tests keep passing. |
| G4 | Keep exact snapshot semantics. | Published snapshots remain immutable; exact-token reads never observe later writes. |
| G5 | Keep implementation simple enough to review. | Start with std collections, compact ids, and `Vec<RowId>` postings; defer compressed bitmaps until benchmarks prove a need. |

## 3. Non-Goals

- No persistent backend in this phase.
- No distributed dispatcher, remote cache, or watch API.
- No public API break for callers that use string/domain relationship types.
- No full query planner rewrite. The evaluator continues to call directional store readers.
- No dependency on a compressed bitmap crate in the first implementation. `roaring = 0.11.4` is a future candidate only if `Vec<RowId>` postings miss memory or lookup targets.

## 4. Current Memory Shape

```text
WriterState
  |
  +-- relationships: IndexedRelationshipStore
  |     |
  |     +-- HashSet<Relationship>              owned strings
  |     +-- Vec<Relationship>                  owned strings
  |     +-- HashMap<StringKey, BTreeSet<usize>>
  |     +-- HashMap<StringKey, BTreeSet<usize>>
  |     +-- ... several more cloned-key indexes
  |
  +-- current_snapshot: Arc<PublishedSnapshot>
  |     |
  |     +-- Arc<IndexedRelationshipStore>      cloned store
  |
  +-- snapshot_history: VecDeque<Arc<PublishedSnapshot>>
  |
  +-- store: Box<InMemoryTupleStore>
        |
        +-- HashSet<RelationTuple>             compatibility copy
```

The same logical relationship is represented as multiple independent allocations. `BTreeSet` postings are also expensive for append-heavy indexes because each row id is wrapped in a tree node. The store should instead be shaped as compact row ids and shared immutable snapshots.

## 5. Target Architecture

```text
WriterState
  |
  +-- current_snapshot: ArcSwap<PublishedSnapshot>
  |
  +-- snapshot_history: VecDeque<Arc<PublishedSnapshot>>
  |
  +-- pre_schema_compat_store: Option<InMemoryTupleStore>
        retained only until first schema publication

PublishedSnapshot
  |
  +-- schema: Arc<CompiledSchema>
  +-- relationships: Arc<CompactRelationshipSnapshot>

CompactRelationshipSnapshot
  |
  +-- interner: IdentifierInterner
  |     +-- strings: Vec<Arc<str>>
  |     +-- ids_by_string: HashMap<Arc<str>, SymbolId>
  |
  +-- rows: Vec<RelationshipRow>
  +-- live_rows: BitSet<RowId>
  +-- uniqueness: HashMap<RelationshipRow, RowId>
  |
  +-- by_resource: HashMap<ResourceRelationKey, Vec<RowId>>
  +-- by_resource_object: HashMap<ResourceObjectKey, Vec<RowId>>
  +-- by_resource_type_relation: HashMap<ResourceTypeRelationKey, Vec<RowId>>
  +-- by_resource_type: HashMap<ObjectTypeId, Vec<RowId>>
  |
  +-- by_subject: HashMap<SubjectKey, Vec<RowId>>
  +-- by_subject_type_relation: HashMap<SubjectTypeRelationKey, Vec<RowId>>
  +-- by_subject_type: HashMap<SubjectTypeId, Vec<RowId>>
```

The steady-state service owns exactly one relationship snapshot per retained revision. The current head and the newest retained history entry point at the same `Arc`, not two cloned stores.

## 6. Compact Identifiers

Every validated identifier string is interned once per relationship snapshot and referenced by a small typed id:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
struct SymbolId(NonZeroU32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
struct ObjectTypeId(SymbolId);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
struct ObjectIdId(SymbolId);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
struct RelationId(SymbolId);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
struct SubjectTypeId(SymbolId);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
struct SubjectIdId(SymbolId);
```

`SymbolId` uses `NonZeroU32` so `Option<RelationId>` remains compact. If a snapshot would exceed `u32::MAX - 1` interned strings or rows, the write fails with a typed `StoreError::CapacityExceeded` before publication.

The interner is internal. External APIs continue accepting `ObjectType`, `ObjectId`, `RelationName`, `SubjectType`, `SubjectId`, and `Relationship`. Query filter construction validates strings at the boundary per [10-local-engine-data-model-design.md](./10-local-engine-data-model-design.md).

Dependency policy:

- M6 starts with a std-only interner implemented for this store.
- `lasso = 0.7.3` and `string-interner = 0.20.0` are candidates only if the local interner becomes a maintenance burden or misses memory targets.
- Any adoption must update [60-crates-features-design.md](./60-crates-features-design.md), run `cargo audit`, and pass `cargo deny check`.

## 7. Row Model

Rows are fixed-width, copyable internal records:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct RelationshipRow {
    resource_type: ObjectTypeId,
    resource_id: ObjectIdId,
    relation: RelationId,
    subject_type: SubjectTypeId,
    subject_id: SubjectIdId,
    subject_relation: Option<RelationId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
struct RowId(NonZeroU32);
```

This row is the uniqueness key. It replaces storing whole `Relationship` values in both `HashSet` and `Vec`. Materializing a public `Relationship` is done only at API boundaries, tests, or compatibility paths.

The compact store reader exposes an internal borrowed row view:

```rust
pub(crate) struct RelationshipRef<'a> {
    snapshot: &'a CompactRelationshipSnapshot,
    row_id: RowId,
}
```

`RelationshipRef` provides accessors equivalent to the current domain surface:

- `resource_type() -> &str`
- `resource_id() -> &str`
- `relation() -> &str`
- `subject_type() -> &str`
- `subject_id() -> &str`
- `subject_relation() -> Option<&str>`
- `to_relationship() -> Relationship` for public materialization

The evaluator should use `RelationshipRef` accessors and avoid allocating `Relationship` on hot paths.

The existing public `RelationshipReader` trait that yields `&Relationship` is not the hot-path contract for compact storage. M6 adds a crate-private compact reader for the evaluator and keeps public compatibility through materializing wrappers at API/test boundaries. If a later crate release removes or changes the public trait, that is a separate API design change in [15-public-api-design.md](./15-public-api-design.md).

## 8. Posting Lists

Indexes map compact keys to `Vec<RowId>`, not `BTreeSet<usize>`.

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct ResourceRelationKey {
    resource_type: ObjectTypeId,
    resource_id: ObjectIdId,
    relation: RelationId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct SubjectKey {
    subject_type: SubjectTypeId,
    subject_id: SubjectIdId,
    subject_relation: Option<RelationId>,
}
```

Posting list rules:

- Row ids are appended once when a relationship is inserted.
- Duplicate insertion is prevented by `uniqueness`, not by posting-list set semantics.
- Query iteration skips tombstoned row ids using `live_rows`.
- No ordering guarantee is exposed publicly. If deterministic test output needs ordering, tests sort materialized values explicitly.
- `QueryLimit` is enforced by the iterator, not by pre-allocating result vectors.

Why `Vec<RowId>` first:

- It is the smallest simple representation for append-heavy postings.
- It has better cache locality than `BTreeSet`.
- The direct-check path usually reads one exact posting list and exits after the first live match.
- Reverse lookup scans contiguous row ids and can stop at `max_lookup_results`.

`roaring = 0.11.4` remains a future candidate for very large sparse postings that need set operations, but current check/lookup operations are better served by contiguous postings and row-level predicates.

## 9. Deletes and Compaction

Deleting a row removes it from `uniqueness` and clears its bit in `live_rows`. It does not eagerly remove the row id from every posting list. This keeps delete cost bounded and avoids expensive posting-list searches.

```text
delete relationship
  |
  +-- lookup RelationshipRow in uniqueness
  +-- remove uniqueness entry
  +-- clear live_rows[row_id]
  +-- increment dead_row_count
```

Queries must check `live_rows` before yielding. This adds one bit test per candidate and avoids stale results.

Compaction rebuilds rows and posting lists when either condition holds:

- `dead_row_count > 100_000`
- `dead_row_count * 4 > rows.len()`

Compaction is a write-path operation. It builds a fresh `CompactRelationshipSnapshot` from live rows and publishes it as the next revision. Exact snapshots retained in history continue pointing to the old compact snapshot until evicted.

## 10. Snapshot Publication

Publication must stop cloning full relationship stores.

Current publication duplicates:

```text
self.relationships = relationships
snapshot.relationships = Arc::new(relationships.clone())
```

Target publication:

```text
builder.freeze() -> Arc<CompactRelationshipSnapshot>
PublishedSnapshot.relationships = Arc::clone(&relationships)
self.current_snapshot.store(Arc<PublishedSnapshot>)
self.snapshot_history.push_back(Arc<PublishedSnapshot>)
```

The writer state should not own a separate relationship store outside the current `PublishedSnapshot`. When a write starts, it loads the current snapshot and builds the next compact snapshot from it. For M6, copying compact rows/postings during a write is acceptable because the steady-state memory target is the release gate; a later persistent delta store can optimize write amplification if benchmark evidence requires it.

Compatibility store policy:

- Before any schema exists, `write_tuple` may store tuples in the legacy `InMemoryTupleStore` so existing compatibility flows still work.
- On first successful schema publication, the service drains legacy tuples into `CompactRelationshipSnapshot` and clears the legacy store.
- After schema publication, all writes go only through compact relationships. The legacy tuple mirror must not be rebuilt from compact rows.
- Compatibility methods that need legacy `RelationTuple` values materialize them on demand from the compact snapshot.

## 11. Query Algorithms

### Exact Direct Check

```text
input RelationshipFilter(resource, relation, exact subject)
  |
  +-- convert filter strings to existing interned ids
  |     missing id => empty iterator
  |
  +-- build ResourceRelationKey
  |
  +-- read posting Vec<RowId>
  |
  +-- for row_id in posting:
        if !live_rows[row_id]: continue
        if row.subject == filter.subject: yield RelationshipRef
        stop at QueryLimit
```

The direct check path remains independent of total row count when the exact posting list is small.

### Resource-Side Wildcard Queries

Resource-side queries use the narrowest available key:

| Filter shape | Index |
| --- | --- |
| type + id + relation | `by_resource` |
| type + id | `by_resource_object` |
| type + relation | `by_resource_type_relation` |
| type only | `by_resource_type` |

The iterator applies remaining predicates, live-row checks, and `QueryLimit`.

### Subject-Side Queries

Subject-side queries use:

| Filter shape | Index |
| --- | --- |
| subject type + id + optional relation | `by_subject` |
| subject type + relation | `by_subject_type_relation` |
| subject type only | `by_subject_type` |

`lookup_resources` starts from this index and validates candidates through the shared evaluator in [14-evaluation-engine-design.md § 7](./14-evaluation-engine-design.md#7-lookup-apis).

## 12. Migration Plan

### M6.1 - Remove Duplicate Snapshot Ownership

- Change `PublishedSnapshot.relationships` and service head state so the current snapshot and service point at the same `Arc`.
- Remove `IndexedRelationshipStore` clone in `publish_snapshot`.
- Clear the compatibility tuple store after first schema publication.
- Add a memory benchmark gate for 1M org rules.

Expected result: significant RSS drop without changing row/index representation.

### M6.2 - Replace `BTreeSet` Postings with `Vec<RowId>`

- Introduce `RowId`.
- Replace all posting sets with append-only `Vec<RowId>`.
- Add live-row tombstones for deletes.
- Add compaction on delete-heavy workloads.
- Preserve property tests for index equivalence.

Expected result: lower index overhead and better reverse-query locality.

### M6.3 - Intern Identifiers and Store Compact Rows

- Add `IdentifierInterner`.
- Replace owned `Relationship` rows with `RelationshipRow`.
- Convert filters into compact keys at query start.
- Add `RelationshipRef<'_>` for evaluator access.
- Materialize public `Relationship` only at API/test/compatibility boundaries.

Expected result: final 1M org-rule RSS <= 400 MiB.

### M6.4 - Clean Up and Recalibrate

- Remove now-unused legacy storage paths.
- Re-run latency, memory, audit, deny, and strict clippy gates.
- Update [71-performance-budgets-design.md](./71-performance-budgets-design.md) with actual before/after measurements.
- Record any target misses in [93-improvements-review.md](./93-improvements-review.md) with profile evidence.

## 13. Risks and Mitigations

| Risk | Mitigation |
| --- | --- |
| Public `RelationshipReader` currently returns `&Relationship`. | Keep it out of compact hot paths. Add a crate-private compact reader for evaluator use and materialize public relationships only in wrappers/tests. |
| Tombstoned row ids grow after many deletes. | Compaction threshold rebuilds live rows as a new revision. Property tests cover delete-heavy sequences. |
| Interned ids obscure debugging. | `Debug` for compact rows resolves ids to redacted or full strings depending on feature/test context. |
| Snapshot writes copy compact arrays. | M6 accepts compact snapshot copy for write simplicity; add persistent delta pages only if write benchmarks fail after M6. |
| `u32` ids overflow in pathological datasets. | Return typed capacity errors before publication; document limits. |

## 14. AGENTS Binding

- Error Handling: new capacity, interner, and compaction failures are typed `StoreError` variants using `thiserror`.
- Async & Concurrency: core remains synchronous. Snapshot reads use `Arc` publication and no read-path mutex.
- Type Design & API: all ids are newtypes; `RowId` and `SymbolId` use non-zero/ranged constructors.
- Safety & Security: no `unsafe`; no unchecked indexing from external values; all row lookup goes through bounds-checked helpers.
- Serialization: compact internals are not serialized publicly. Public request/response DTOs keep existing camelCase behavior.
- Testing: property tests compare compact store results with a reference `HashSet<Relationship>` model after random create/touch/delete/precondition batches.
- Logging & Observability: metrics/logs expose counts and bytes, not full relationship payloads by default.
- Performance: no full scans for direct checks; no hot-path relationship materialization.
- Documentation: public docs describe memory expectations and retained snapshot impact.

## 15. Cross-References

- <- Depends on: [12-relationship-store-design.md](./12-relationship-store-design.md), [13-revision-consistency-design.md](./13-revision-consistency-design.md), [14-evaluation-engine-design.md](./14-evaluation-engine-design.md)
- -> Consumed by: [71-performance-budgets-design.md](./71-performance-budgets-design.md), [72-testing-verification-plan.md](./72-testing-verification-plan.md), [90-local-engine-roadmap.md](./90-local-engine-roadmap.md), [91-local-engine-impl-plan.md](./91-local-engine-impl-plan.md)
- Related research: [../docs/research/study-spicedb.md § Query Filters and Indexes](../docs/research/study-spicedb.md#query-filters-and-indexes)
