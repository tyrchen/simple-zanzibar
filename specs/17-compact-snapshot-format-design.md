# 17 - Compact Snapshot File Format Design

Status: draft v1
Owner: Simple Zanzibar
Depends on: [13-revision-consistency-design.md](./13-revision-consistency-design.md), [16-compact-relationship-store-design.md](./16-compact-relationship-store-design.md), [71-performance-budgets-design.md](./71-performance-budgets-design.md)
Last updated: 2026-05-23

## 1. Problem

Phase M6 makes the in-memory relationship snapshot compact enough for 1M-rule local org authorization datasets: the measured max RSS drops from 3.12 GiB to 368 MiB on the reference machine. The remaining cold-start path still constructs that compact snapshot from logical relationships. A large deployment that wants to ship local authorization data with an application should not need to parse relationship text, allocate domain `Relationship` values, intern every string again, and rebuild all indexes on every process start.

The current measured 1M filtered benchmark runtime is about 2.32 s end to end for `org_authorization/1m_rules/check_direct_group_viewer`. That number is not pure load time: it includes process startup, schema parse/compile, generated relationship construction, compact snapshot construction, scenario validation, Criterion warmup, measurement, and analysis. Before implementing this spec, the project must add dedicated build/serialize/load benchmarks so snapshot load speed and load-time memory are measured directly.

This spec defines a versioned compact snapshot artifact that is close to the final in-memory representation. The first implementation prioritizes load speed and bounded load-time RSS over minimal disk size.

## 2. Goals

| # | Goal | Measure |
| --- | --- | --- |
| G1 | Load a 1M-rule compact snapshot substantially faster than rebuilding from logical rules. | `snapshot_load_compact/1m` p95 <= 500 ms on the reference machine after the file is already on local SSD. |
| G2 | Keep load-time peak RSS close to final steady-state RSS. | `snapshot_load_compact/1m` max RSS <= 1.25x the loaded snapshot RSS. |
| G3 | Keep disk size practical without slowing the fast path. | Uncompressed 1M snapshot artifact <= 2x final steady-state compact RSS; compressed artifact is optional and benchmark-gated. |
| G4 | Preserve correctness and compatibility. | Loaded snapshots produce identical check, expand, lookup, and exact consistency behavior to snapshots built from relationships. |
| G5 | Keep the format evolvable. | Header versioning and section table allow adding sections without breaking older readers that reject unsupported versions. |
| G6 | Stay within project safety rules. | No `unsafe` in crate code; first implementation uses std file reads and checked parsing, not direct memory mapping. |

## 3. Non-Goals

- No distributed datastore, watch API, or incremental remote replication.
- No append-only on-disk database in the first phase. The artifact is a frozen snapshot file.
- No direct serialization of Rust `HashMap` internals. HashMap layout, hasher state, and allocator details are not stable file format contracts.
- No text relationship import format replacement. Text/domain ingestion remains useful for authoring and tests.
- No memory-mapped zero-copy implementation in the first pass. `mmap` can be reconsidered only if a safe abstraction satisfies AGENTS.md's no-unsafe rule and benchmarks prove it is necessary.

## 4. Target Architecture

```text
Authoring / Build Time

  relationships + schema
          │
          ▼
┌──────────────────────────┐
│ CompactRelationshipStore │
│ - byte arena             │
│ - compact rows           │
│ - posting indexes        │
└────────────┬─────────────┘
             │ snapshot writer
             ▼
┌────────────────────────────────────────────────────────┐
│ .szsnap compact snapshot file                          │
│                                                        │
│  Header                                                │
│  Section directory                                     │
│  Schema section                                        │
│  Symbol byte arena + symbol table                      │
│  Relationship rows                                     │
│  Index key tables + posting ranges + posting row ids   │
│  Integrity footer                                      │
└────────────────────────────────────────────────────────┘

Runtime Load

  .szsnap file
       │
       ▼
┌──────────────────────────────┐
│ SnapshotLoader               │
│ - validate header/checksum   │
│ - bounds-check sections      │
│ - decode little-endian words │
└──────────────┬───────────────┘
               │
               ▼
┌──────────────────────────────┐
│ PublishedSnapshot            │
│ - compiled schema            │
│ - compact relationship store │
│ - revision/schema hash       │
└──────────────────────────────┘
```

The disk artifact is a deployment artifact, not a mutable runtime store. A service loads it into a normal immutable `PublishedSnapshot`; subsequent relationship writes still publish new in-memory snapshots unless a future spec adds checkpointing.

## 5. File Envelope

All multi-byte integers are little-endian. All offsets are byte offsets from the beginning of the file. Every section offset and length must be checked before slicing.

```text
File
  Header
  SectionDirectory[count]
  Section payloads
  Footer
```

Header fields:

| Field | Type | Purpose |
| --- | --- | --- |
| magic | `[u8; 8]` | `SZSNAP\0\1` for quick file identification. |
| format_version | `u16` | Initial version is `1`. Unsupported versions are rejected. |
| flags | `u16` | Compression/checksum/index-mode flags. Initial writer sets `0`. |
| header_len | `u32` | Allows extending the header. |
| section_count | `u32` | Number of directory entries. |
| file_len | `u64` | Expected total file length. |
| schema_hash | `[u8; 32]` | Canonical hash of the schema section. |
| relationship_count | `u32` | Number of compact rows. |
| symbol_count | `u32` | Number of interned identifiers. |
| created_revision | `u64` | Revision to install into the loaded `PublishedSnapshot`. |

Section directory entry:

| Field | Type | Purpose |
| --- | --- | --- |
| kind | `u16` | Known section id. |
| flags | `u16` | Section-local flags. |
| offset | `u64` | Payload offset. |
| len | `u64` | Payload length. |
| row_count | `u64` | Optional element count for fixed-width sections. |

Required section ids:

| ID | Name | Required | Description |
| ---: | --- | --- | --- |
| 1 | `schema` | yes | Canonical schema representation sufficient to reconstruct `CompiledSchema`. |
| 2 | `symbol_bytes` | yes | Contiguous UTF-8 bytes for interned identifiers. |
| 3 | `symbol_table` | yes | `start: u32, len: u32` entries into `symbol_bytes`. |
| 4 | `relationship_rows` | yes | Fixed-width compact relationship rows. |
| 5 | `index_directory` | yes | Metadata for each serialized index. |
| 6 | `index_keys` | yes | Fixed-width compact keys grouped by index kind. |
| 7 | `posting_ranges` | yes | `start: u32, len: u32` ranges into `posting_row_ids`. |
| 8 | `posting_row_ids` | yes | Contiguous row ids for non-singleton postings. |
| 9 | `footer` | yes | Checksum and optional writer metadata. |

Unknown required sections are rejected. Unknown optional sections are skipped only when a future section flag explicitly marks them optional.

## 6. Section Layouts

### 6.1 Schema Section

The first implementation should store the existing DSL text or a canonical JSON-like schema representation, whichever is already stable when implemented. Load speed priority favors a compact canonical schema IR eventually, but schema compile time is tiny compared with 1M relationship ingestion and must not dominate the first phase.

Rules:

- The `schema_hash` in the header must match `SchemaHash::for_schema` after loading/compiling the schema.
- The schema section has an explicit maximum length, initially 4 MiB.
- Invalid schema fails the entire load with a typed error.

### 6.2 Symbol Sections

The symbol representation matches the M6 byte-arena interner:

```rust
#[repr(C)]
struct DiskSymbol {
    start: u32,
    len: u32,
}
```

Load rules:

- `start + len` must be in bounds of `symbol_bytes`.
- Every symbol byte range must be valid UTF-8.
- Identifier domain validation is skipped only if the writer records a trusted `validated` flag and the checksum passes. The first implementation should revalidate in debug/test builds and may revalidate in release until benchmarks show it matters.
- Duplicate symbols are a writer bug and must be rejected unless the reader can prove row semantics remain equivalent. The first reader rejects duplicates to keep invariants simple.

### 6.3 Relationship Rows

Rows are stored without `RowId`; row id is implicit `index + 1`.

```rust
#[repr(C)]
struct DiskRelationshipRow {
    resource_type: u32,
    resource_id: u32,
    relation: u32,
    subject_type: u32,
    subject_id: u32,
    subject_relation: u32, // 0 means None; otherwise SymbolId.get()
}
```

Load rules:

- Every non-zero symbol id must be `1..=symbol_count`.
- `subject_relation == 0` represents a direct object subject.
- `relationship_count` must match section length / row width.
- The loaded store starts compacted: all rows are live and there are no tombstones.

### 6.4 Index Sections

The file format stores stable sorted index arrays, not runtime `HashMap` internals.

Each serialized index is represented by:

```text
IndexDirectoryEntry
  index_kind
  key_start
  key_count
  posting_range_start
  posting_range_count

IndexKey[i]
PostingRange[i]
PostingRowIds[range.start..range.start + range.len]
```

`PostingRange` uses inline singleton storage to keep disk and load memory small:

```rust
#[repr(C)]
struct DiskPostingRange {
    first_row_id: u32,
    overflow_start: u32,
    overflow_len: u32,
}
```

If `overflow_len == 0`, the posting list contains exactly `first_row_id`. If `overflow_len > 0`, the posting list contains `first_row_id` followed by `posting_row_ids[overflow_start..overflow_start + overflow_len]`.

Required index kinds:

| Kind | Key shape | Query use |
| --- | --- | --- |
| resource | `resource_type, resource_id, relation` | exact resource/relation checks |
| resource_object | `resource_type, resource_id` | resource wildcard relation |
| resource_type_relation | `resource_type, relation` | type+relation scans |
| resource_type | `resource_type` | broad resource type scans |
| subject | `subject_type, subject_id, subject_relation_or_zero` | exact reverse lookup |
| subject_type_relation | `subject_type, relation` | reverse relation scans |
| subject_type | `subject_type` | broad subject type scans |

Keys must be sorted lexicographically by their integer fields. The loader can choose one of two runtime profiles:

| Profile | Load behavior | Runtime behavior | Use case |
| --- | --- | --- | --- |
| `fast-load` | Keep sorted key/range arrays and binary-search on query. | Slightly slower point lookups, lowest load time/RSS. | CLI/serverless cold starts. |
| `latency` | Rebuild M6 `PostingIndex<K>` from sorted arrays. | Faster point lookups, higher load time/RSS. | Long-running service process. |

The first implementation should ship `fast-load` first and add `latency` only if benchmarks show binary search misses check budgets.

## 7. Reader and Writer APIs

Public API shape:

```rust
pub struct SnapshotSaveOptions {
    pub compression: SnapshotCompression,
    pub include_indexes: bool,
}

pub enum SnapshotCompression {
    None,
}

pub struct SnapshotLoadOptions {
    pub profile: SnapshotLoadProfile,
    pub max_file_bytes: NonZeroU64,
}

pub enum SnapshotLoadProfile {
    FastLoad,
    Latency,
}

impl ZanzibarService {
    pub fn save_snapshot(
        &self,
        path: impl AsRef<Path>,
        options: SnapshotSaveOptions,
    ) -> Result<(), SnapshotIoError>;

    pub fn load_snapshot(
        path: impl AsRef<Path>,
        options: SnapshotLoadOptions,
    ) -> Result<Self, SnapshotIoError>;
}
```

Internal API shape:

```rust
impl IndexedRelationshipStore {
    pub(crate) fn encode_snapshot_sections(&self, writer: &mut SnapshotWriter)
        -> Result<(), SnapshotIoError>;

    pub(crate) fn decode_snapshot_sections(reader: &SnapshotReader)
        -> Result<Self, SnapshotIoError>;
}
```

The public API returns a fully usable `ZanzibarService` with a current `PublishedSnapshot`. The loaded service uses the artifact revision and datastore id policy defined below.

## 8. Revision and Consistency Semantics

A snapshot file is a point-in-time local datastore image.

Load policy:

- The loaded service gets a new `DatastoreId` unless the caller explicitly requests preserving the file's datastore id in a future option. First implementation always mints a new id.
- `created_revision` becomes the loaded service's `last_revision`.
- The current snapshot and history contain exactly one snapshot unless the caller later writes mutations.
- Existing consistency tokens from the writer process are not valid against the newly loaded service because the datastore id changes.

This avoids accidental token reuse across machines and matches [13-revision-consistency-design.md](./13-revision-consistency-design.md)'s local datastore identity model.

## 9. Integrity and Trust Boundary

Snapshot files are untrusted input. The loader is a boundary module and must follow AGENTS.md safety/security rules.

Validation requirements:

- Reject files larger than `SnapshotLoadOptions::max_file_bytes`.
- Reject bad magic, unsupported version, duplicate required sections, overlapping sections, out-of-bounds sections, malformed UTF-8, invalid symbol ids, invalid row ids, unsorted keys, and posting ranges outside `posting_row_ids`.
- Reject arithmetic overflow with checked arithmetic.
- Compute a BLAKE3 checksum over all bytes except the footer checksum field. The footer stores the expected digest.
- Do not log full relationship identifiers by default.
- No `unsafe`, no unchecked indexing, no `unwrap()`/`expect()` in production loader code.

Error model:

```rust
#[derive(Debug, thiserror::Error)]
pub enum SnapshotIoError {
    #[error("snapshot io failed")]
    Io { #[source] source: std::io::Error },

    #[error("snapshot format error: {reason}")]
    Format { reason: &'static str },

    #[error("snapshot limit exceeded: {component}")]
    LimitExceeded { component: &'static str },

    #[error("snapshot schema failed")]
    Schema { #[source] source: ZanzibarError },
}
```

Exact variant names can change during implementation, but failures must be typed and must preserve source errors where useful.

## 10. Load Algorithms

### 10.1 Fast Load

```text
load_snapshot(path)
  |
  +-- read file bytes with size cap
  +-- validate header and section directory
  +-- verify checksum
  +-- compile schema section
  +-- borrow-parse symbol table and rows
  +-- copy symbol bytes, symbol table, and rows into store vectors
  +-- validate index key/range sections
  +-- build runtime sorted-array indexes
  +-- publish one PublishedSnapshot into ZanzibarService
```

The first implementation may still copy the file sections into owned vectors. The important property is that it avoids per-relationship domain object allocation and avoids re-interning strings from text.

### 10.2 Latency Profile

```text
fast load sections
  |
  +-- for each serialized index:
        for each key/range:
          insert key -> first row id into PostingIndex
          append overflow row ids if present
```

This profile trades extra load CPU and memory for the current hash-based runtime lookup shape. It is optional in the first implementation if fast-load sorted arrays pass latency budgets.

## 11. Disk Size Strategy

Priority order:

1. Fast uncompressed load.
2. Compact fixed-width sections and no duplicated strings.
3. Optional compression only after uncompressed load benchmarks are green.

Expected uncompressed size components for 1M rules:

| Component | Approximate shape |
| --- | --- |
| rows | `relationship_count * 24 bytes` before section alignment |
| symbols | byte arena plus `symbol_count * 8 bytes` |
| index keys | high-cardinality singleton-heavy keys dominate |
| postings | `first_row_id` inline plus overflow row ids only |
| schema/header/footer | negligible |

Compression can be useful for distribution but may hurt load speed. If added, it must be per-section and optional so deployments can choose:

- uncompressed for fastest local startup
- compressed for lower disk/network footprint

No compression crate is selected in this spec. Adding one requires updating [60-crates-features-design.md](./60-crates-features-design.md), `cargo audit`, and `cargo deny check`.

## 12. Benchmarks and Gates

Add benchmark targets:

```text
snapshot_build_from_relationships/1k
snapshot_build_from_relationships/100k
snapshot_build_from_relationships/1m
snapshot_save_uncompressed/1m
snapshot_load_compact/1k
snapshot_load_compact/100k
snapshot_load_compact/1m
snapshot_load_and_reindex/1m
snapshot_file_size/1m
snapshot_load_peak_rss/1m
```

Add Makefile targets:

```text
bench-snapshot
bench-snapshot-memory
```

Initial targets on the reference machine:

| Operation | Dataset | Target |
| --- | --- | ---: |
| build compact snapshot from generated relationships | 1M | p95 recorded, not gate for first phase |
| save uncompressed snapshot | 1M | p95 <= 1.5 s |
| load fast-load uncompressed snapshot | 1M | p95 <= 500 ms |
| load-time max RSS | 1M | <= 1.25x loaded steady-state RSS |
| file size uncompressed | 1M | <= 2x loaded steady-state RSS |
| direct check after load | 1M | p95 <= 10 us |
| inherited check after load | 1M | p95 <= 25 us |
| lookup resources after load | 1M | p95 <= 10 ms |

If the first measured build shows a different bottleneck, update this spec and [71-performance-budgets-design.md](./71-performance-budgets-design.md) with evidence before changing the format.

## 13. Testing Plan

Required tests:

- golden file round trip for a tiny schema and relationship set
- corrupt magic/version/header/section bounds/checksum tests
- malformed UTF-8 and invalid symbol id rejection
- duplicate symbol rejection
- unsorted index key rejection
- posting range out-of-bounds rejection
- loaded snapshot check/expand/lookup equivalence against build-from-relationships snapshot
- exact consistency behavior after loading and after a subsequent write
- property test: random compact stores save/load to equivalent query results
- compatibility test: loaded service public APIs match normal `ZanzibarService` APIs

Slow 1M load/save tests are benchmark/ignored tests, not regular unit tests.

## 14. Implementation Plan

### M7.1 - Measurement First

- Add pure build/save/load benchmarks for current generated org scenarios.
- Add `bench-snapshot` and `bench-snapshot-memory` Makefile targets.
- Record baseline in [71-performance-budgets-design.md](./71-performance-budgets-design.md).

Exit: pure 1M compact build time, save time, load baseline, file size baseline, and RSS are visible.

### M7.2 - Format Encoder

- Add header, section directory, symbol sections, row section, index sections, footer checksum.
- Save uncompressed `.szsnap` from an existing compact snapshot.
- Add tiny golden fixture.

Exit: writer produces deterministic bytes for the same snapshot.

### M7.3 - Safe Fast Loader

- Add checked parser and validation.
- Load schema, symbols, rows, and sorted-array indexes into a usable `PublishedSnapshot`.
- No `unsafe`, no unchecked indexing, no production `unwrap()`/`expect()`.

Exit: loaded tiny and medium fixtures pass check/expand/lookup equivalence.

### M7.4 - Large Dataset Gates

- Run 1k/100k/1M save/load/file-size/RSS benchmarks.
- Tune sorted index layout only with benchmark evidence.
- Add optional latency-profile reindexing only if fast-load misses check budgets.

Exit: 1M load <= 500 ms or documented target recalibration with evidence; loaded query budgets pass.

### M7.5 - Public API and Docs

- Add `ZanzibarService::save_snapshot` and `ZanzibarService::load_snapshot`.
- Document token/datastore semantics.
- Add examples and update verification docs.

Exit: public API docs build with examples and all gates pass.

## 15. AGENTS Binding

- Error Handling: `SnapshotIoError` uses `thiserror` and preserves `std::io::Error`/schema sources.
- Async & Concurrency: N/A for first implementation; APIs are synchronous local file operations.
- Type Design & API: file offsets, counts, versions, and limits use bounded newtypes where exposed.
- Safety & Security: no `unsafe`; all untrusted file data is length/range checked before use.
- Serialization: fixed little-endian section format; no serde for binary hot path unless benchmarked.
- Testing: corrupt-input tests and equivalence tests are required before public API exposure.
- Logging & Observability: only counts, bytes, format versions, and timing are logged; no full relationship payload logs by default.
- Performance: load benchmarks are release-mode gates; no compression or mmap until measured.
- Dependencies: first implementation uses std plus existing `blake3`; new compression/mmap dependencies require spec and deny/audit updates.
- Documentation: public docs explain artifact versioning, trust boundary, token behavior, and compatibility guarantees.

## 16. Cross-References

- <- Depends on: [13-revision-consistency-design.md](./13-revision-consistency-design.md), [16-compact-relationship-store-design.md](./16-compact-relationship-store-design.md), [71-performance-budgets-design.md](./71-performance-budgets-design.md)
- -> Consumed by: [72-testing-verification-plan.md](./72-testing-verification-plan.md), [90-local-engine-roadmap.md](./90-local-engine-roadmap.md), [91-local-engine-impl-plan.md](./91-local-engine-impl-plan.md)
- Related decision: [99-key-decisions.md § D13](./99-key-decisions.md#d13---serialize-compact-snapshots-as-stable-sectioned-artifacts)
