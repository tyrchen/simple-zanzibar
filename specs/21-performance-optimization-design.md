# 21 - Performance Optimization Design

Status: draft v1
Owner: Simple Zanzibar
Last updated: 2026-05-24
Depends on: [16](./16-compact-relationship-store-design.md), [17](./17-compact-snapshot-format-design.md), [18](./18-trusted-fast-snapshot-load-design.md), [20](./20-concurrent-engine-runtime-design.md), [71](./71-performance-budgets-design.md)

## 1. Purpose

M10 made `ZanzibarEngine` the only public runtime, moved reads to immutable snapshots, and added
single-writer actor plus tenant sharding. The current engine is now correct and practical at
1M-rule scale, but the performance profile still has structural ceilings:

- writes copy too much unchanged state;
- some evaluator and lookup paths still bounce through legacy string/domain objects;
- full snapshot load repeats semantic proof that trusted build-pipeline artifacts can avoid;
- snapshot save/load still materializes full payloads in memory;
- every loaded store currently carries the full index set even when the application only needs
  checks.

This spec is the dependency-ordered plan for the next optimization pass. It keeps correctness and
the safe default trust boundary ahead of raw speed: the hard <= 200 ms 1M-rule load target remains
the explicit trusted fast-load path, while the default full loader continues to validate hostile
files.

## 2. Current Evidence

Measured 2026-05-23 on the reference macOS machine:

| Area | Current evidence | Notes |
| --- | ---: | --- |
| raw 1M full snapshot load | `[555.68 ms, 559.30 ms, 563.48 ms]` | safe default full validation |
| raw 1M trusted fast load | `[135.78 ms, 137.06 ms, 138.67 ms]` | `TrustedFastLoad + External` |
| zstd 1M direct load | `[625.35 ms, 628.92 ms, 632.45 ms]` | compressed transport, not fastest startup path |
| raw 1M `.szsnap` size | `124,422,241 bytes` | uncompressed v2 artifact |
| zstd 1M `.szsnap` size | `33,162,371 bytes` | 26.7% of raw bytes |
| 1M full-load max RSS | `436,076,544 bytes` | load-time peak, not steady state |
| realworld mixed read | `[63.188 us, 63.394 us, 63.577 us]` | 1M realistic SaaS sample |
| single-tenant heavy batched writes | `111,872 logical writes/s; p95 9,921 us` | 100k base concurrent benchmark |
| tenant-sharded heavy batched writes | `419,072 logical writes/s; p95 2,753 us` | sharding proves partitioning helps |

Concrete code bottlenecks observed in the current implementation:

| Finding | Code evidence | Structural issue |
| --- | --- | --- |
| full-store clone on write | `src/lib.rs:344`, `src/relationship.rs:448`, `src/relationship.rs:528` | each write batch can copy rows, symbols, uniqueness, and seven indexes |
| writer submit mutex spans blocking send | `src/api.rs:1113`, `src/api.rs:1169` | a full bounded queue can serialize submitter threads before the actor sees work |
| evaluator still creates owned keys and legacy relations | `src/eval.rs:194`, `src/eval.rs:280`, `src/eval.rs:344`, `src/eval.rs:721` | recursive checks allocate/convert on hot paths |
| lookup allocates and re-checks candidates | `src/eval.rs:615`, `src/eval.rs:672`, `src/eval.rs:689` | fresh contexts and owned objects per candidate |
| full load decodes indexes sequentially with per-index coverage buffers | `src/relationship.rs:2577`, `src/relationship.rs:2820` | safe but leaves parallel CPU and RSS reductions unused |
| full load keeps eager uniqueness | `src/relationship.rs:3247`, `src/relationship.rs:3292` | uniqueness is needed for writes, not read-only startup |
| snapshot save/load copies full payloads | `src/snapshot.rs:484`, `src/snapshot.rs:495`, `src/snapshot.rs:518`, `src/snapshot.rs:582` | whole-section and whole-file buffers increase transient memory |
| every store carries all seven indexes | `src/relationship.rs:419`, `src/relationship.rs:423` | check-only deployments pay lookup-index memory and disk cost |

## 3. Goals

| # | Goal | Measure |
| --- | --- | --- |
| G1 | Reduce single-tenant write amplification. | 1M-base write benchmarks must show at least 3x lower p95 for small/mixed writes after the segmented store lands, or this spec must be recalibrated with profile evidence. |
| G2 | Improve read latency without weakening semantics. | Realworld 1M inherited checks and mixed-read workload should improve by >= 10% with no public API behavior change; no existing hot-path benchmark may regress by > 5%. |
| G3 | Keep the <= 200 ms load path explicit and safe. | `snapshot_load_trusted_fast/1m` remains <= 200 ms; default `Full` validation never silently skips semantic checks. |
| G4 | Lower default full-load cost where safe. | `snapshot_load_compact/1m` full mode target after loader optimization: upper estimate <= 450 ms, with load-time max RSS <= 400 MiB. |
| G5 | Reduce memory and disk for narrow use cases. | A check-only index profile must reduce 1M raw snapshot bytes and steady-state store memory by >= 20% versus `Full`, while preserving direct/inherited check budgets. |
| G6 | Make performance evidence reproducible. | Every phase adds or updates Makefile-discoverable benchmark targets and records Criterion estimates plus RSS where relevant. |

## 4. Non-Goals

- No `unsafe` and no mmap in this pass. Memory mapping can be reconsidered only with a safe wrapper,
  a separate threat model update, and evidence that read/copy cost is still dominant after this spec.
- No weakening of the default snapshot loader. Hostile files still use `SnapshotValidationMode::Full`.
- No distributed datastore, network watch protocol, or cross-process consistency.
- No fine-grained relationship locks inside one tenant. The write order remains linearized per
  engine; throughput comes from smaller write deltas, caller batching, and tenant sharding.
- No public global "dump every subject and every permission" query.
- No new dependency is required by this design. If implementation later needs a channel or
  parallelism crate, [60-crates-features-design.md](./60-crates-features-design.md) and
  [99-key-decisions.md](./99-key-decisions.md) must be updated after a current dependency review.

## 5. Optimization Map

```text
+--------------------------------------------------------------------------------+
| ZanzibarEngine public API                                                      |
|                                                                                |
|  Read calls                                                                    |
|    |                                                                           |
|    v                                                                           |
|  +-----------------------+      +-----------------------+                       |
|  | ArcSwap EngineState   |----->| ID-native evaluator   |                       |
|  | immutable snapshots   |      | reusable contexts     |                       |
|  +-----------+-----------+      +-----------+-----------+                       |
|              |                              |                                   |
|              v                              v                                   |
|  +-----------------------+      +-----------------------+                       |
|  | Store view            |<-----| Streaming lookup      |                       |
|  | checkpoint + deltas   |      | bounded candidates    |                       |
|  +-----------+-----------+      +-----------------------+                       |
|              ^                                                                  |
|              | publish immutable revision                                       |
|  +-----------+-----------+                                                      |
|  | Single writer actor  |                                                      |
|  | batch + delta write  |                                                      |
|  +-----------+-----------+                                                      |
|              |                                                                  |
|              v                                                                  |
|  +-----------------------+      +-----------------------+                       |
|  | Snapshot save/load    |<---->| Index profile policy |                       |
|  | full/trusted/zstd     |      | full/check-only/etc. |                       |
|  +-----------------------+      +-----------------------+                       |
|                                                                                |
+--------------------------------------------------------------------------------+
```

The design follows an rsync-like principle: avoid redoing work whose result can be proven once and
reused. For snapshots, trusted fast-load reuses build-time semantic proof. For writes, segmented
store deltas publish only the changed rows instead of cloning unchanged indexes. For lookup, reusable
contexts and streaming traversal avoid rebuilding the same temporary state per candidate. This is
not a network delta-sync protocol; it is the same optimization idea applied inside one process.

## 6. Priority 1 - Low-Risk Fixes

These changes are intentionally small and should land before larger architecture work.

### 6.1 Writer Submit Without Blocking Mutex

`WriterActor` should keep a cloneable bounded sender directly:

```rust
struct WriterActor {
    sender: SyncSender<WriterCommand>,
    handle: Mutex<Option<JoinHandle<()>>>,
    shutdown: AtomicBool,
}
```

`send` clones or borrows the sender without holding a mutex across `SyncSender::send`. Drop uses
`shutdown.swap(true, Ordering::AcqRel)` and sends `WriterCommand::Shutdown` at most once, then joins
the handle.

Invariants:

- a full queue still applies backpressure to callers;
- submitter threads are not serialized by a sender mutex while blocked;
- actor shutdown still returns `EngineError::WriterUnavailable`;
- no write command is accepted after shutdown starts.

### 6.2 Duplicate Schema Lookup Removal

Public `check` validates the object relation, and `EvaluationContext::check_entered` validates it
again. The read path should validate once and pass a prepared relation handle into the evaluator:

```rust
struct PreparedCheck<'a> {
    object: &'a Object,
    relation: &'a Relation,
    subject: &'a User,
    relation_definition: &'a SchemaRelationDefinition,
}
```

The public API keeps the same signature. Internally, `check_with_snapshot` gains a prepared variant
used by `ZanzibarEngine::check`, `lookup_permissions`, and lookup verification loops.

### 6.3 Lazy Write Uniqueness After Full Load

Full snapshot load should use a temporary duplicate detector for row validation and then drop it
before publishing read-only state. The store records:

```rust
enum UniquenessState {
    Ready(RelationshipIdentityIndex),
    KnownUniqueButNotIndexed,
    UntrustedNotIndexed,
}
```

`Full` load publishes `KnownUniqueButNotIndexed`; `TrustedFastLoad` publishes
`UntrustedNotIndexed`. The first mutation builds the index once. If `UntrustedNotIndexed` discovers
duplicates during lazy build, the mutation fails with a typed store invariant error and publishes no
new revision.

### 6.4 Sorted Relation Cache

Permission enumeration currently sorts schema relations per call. `CompiledSchema` should store
relation definitions in both canonical map form and stable sorted order per namespace. The cache is
rebuilt only when schema changes, so `lookup_permissions` and `lookup_object_permissions` can
iterate without allocation or sort.

### 6.5 All-Live Row Fast Path

`LiveRows::full(row_count)` means no tombstones exist. Iterators should carry a fast-path state that
skips `live_rows.contains(row_id)` until the first delete/compaction introduces tombstones. The
fast path must preserve the checked `rows.get(row_id.index())` access.

## 7. Priority 2 - ID-Native Evaluation

The evaluator should stop converting typed schema relations back into legacy `Relation(String)` and
then reparsing them. The internal check key becomes compact and ID-native:

```rust
struct EvalObject {
    object_type: ObjectTypeId,
    object_id: ObjectIdId,
}

struct EvalSubject {
    subject_type: SubjectTypeId,
    subject_id: SubjectIdId,
    subject_relation: Option<RelationId>,
}

struct EvalCheckKey {
    object: EvalObject,
    relation: RelationId,
    subject: EvalSubject,
}
```

Public `Object`, `Relation`, and `User` remain the public API. The snapshot prepares internal IDs
once per request, and recursive computed-userset / tuple-to-userset evaluation passes IDs directly.
Materializing legacy `Object` and `Relation` values is allowed only at public response boundaries.

Required behavior:

- recursion detection uses `EvalCheckKey`, not cloned public model structs;
- computed userset and tuple-to-userset relations store `RelationId` in compiled schema metadata;
- `RelationshipRef::subject_userset_legacy` is removed from hot check recursion and replaced with
  an ID-native subject userset accessor;
- every public error remains the same typed error category as before.

## 8. Priority 3 - Streaming Lookup

Lookup APIs must keep the existing bounded Vec-returning public surface, but internals should avoid
collecting candidates and allocating fresh evaluation contexts for every candidate.

### 8.1 Reusable Evaluation Contexts

`EvaluationContext` should support per-candidate reset with generation counters:

```rust
struct VisitGeneration(NonZeroU32);

struct ReusableEvaluationContext<'a> {
    snapshot: &'a PublishedSnapshot,
    limits: EvaluationLimits,
    generation: VisitGeneration,
    visited_checks: HashMap<EvalCheckKey, VisitGeneration>,
    expanded: HashMap<EvalExpandKey, VisitGeneration>,
}
```

Reset increments the generation; when it overflows, the maps are cleared and generation restarts at
one. This preserves cycle detection while reducing allocation churn in lookup loops.

### 8.2 Candidate Streaming

`lookup_subjects` should stream candidates from expansion into verification and output limits. It
must not build an unbounded intermediate `Vec<User>`.

`lookup_resources` should reuse one context for the BFS and verification loop. It should also keep
direct-resource candidates as borrowed/ID-native objects until the final response vector.

### 8.3 Safe Shortcuts

The evaluator may skip the final `check` only when the candidate proof is exact under the compiled
schema expression:

- direct `this` relation candidate on the requested object/relation;
- computed userset where the recursive check proof already returned allowed for the same subject;
- tuple-to-userset proof whose intermediate edge and computed relation were both evaluated in the
  same context.

Exclusion, intersection, fanout-limit, and depth-limit cases must continue to verify through the
normal check algebra.

## 9. Priority 4 - Snapshot Load and Save

### 9.1 Phase Timers

Snapshot benchmarks must expose these phase timings behind benchmark-only instrumentation:

```text
file_read
decompression
header_and_sections
checksum
schema_parse_compile
symbols
rows
indexes
publish
```

The timers are not a public runtime API. They exist so future PR comments cannot conflate
relationship generation, Criterion harness overhead, compressed transport, and pure load time.

### 9.2 Parallel Index Decode

Full validation may decode independent index groups in parallel using `std::thread::scope` and a
bounded worker count from `std::thread::available_parallelism`. The implementation must cap workers
to the number of index groups and preserve deterministic error reporting by returning the first
index in fixed section order that fails.

Parallel decoding may allocate one coverage buffer per active index group. For 1M rows this is
small relative to current load RSS, but the worker cap must be configurable internally so memory
benchmarks can compare sequential and parallel modes.

### 9.3 Fixed Section Lookup

`SnapshotReader` should replace generic section lookup maps with a fixed section table keyed by
`SectionKind`. The current section set is small and versioned; an array avoids hash work during
every load and makes duplicate/missing-section checks direct.

### 9.4 Streaming Snapshot Writer

The writer should stop building all section payloads plus a final encoded Vec before `fs::write`.
The target shape:

1. precompute section lengths and offsets;
2. write header and section directory to a temp file through `BufWriter`;
3. stream schema, symbols, rows, index keys, posting ranges, posting row ids, and optional lookup
   sections;
4. update BLAKE3 while bytes are written;
5. write footer and atomically rename the temp file into place.

For `SnapshotCompression::Zstd`, the raw payload may be streamed through a zstd encoder, but the
inner `.szsnap` footer checksum still covers the raw payload. The fastest startup recommendation
remains: distribute zstd, decompress once into a content-addressed raw cache, then load the raw
artifact with `TrustedFastLoad + External`.

## 10. Priority 5 - Segmented Store and Delta Publication

The current copy-on-write strategy clones the entire `IndexedRelationshipStore` before a write. The
next store architecture should publish immutable views made of a checkpoint plus bounded deltas.

```text
                 exact revision N
        +--------------------------------+
        | StoreView                      |
        | checkpoint: Arc<StoreSegment>  |
        | deltas: Arc<[StoreDelta]>      |
        +---------------+----------------+
                        |
      query             | merge candidates, mask tombstones
        v               v
+---------------+  +----------------+  +----------------+
| checkpoint    |  | delta N-2      |  | delta N-1      |
| full indexes  |  | inserts/deletes|  | inserts/deletes|
+---------------+  +----------------+  +----------------+

writer command
        |
        v
validate against StoreView -> build one new immutable delta -> publish new StoreView
```

### 10.1 StoreDelta

`StoreDelta` owns only changes from one successful write publication:

```rust
struct StoreDelta {
    inserted: IndexedRelationshipStore,
    deleted: RelationshipIdentitySet,
    mutation_count: NonZeroUsize,
}
```

The inserted store may use its own small interner. Querying a `StoreView` asks each segment for
matching candidates and filters any candidate whose relationship identity is deleted by a newer
delta. Segment count stays bounded by checkpoint thresholds.

### 10.2 Checkpointing

The writer actor creates a new checkpoint when any threshold trips:

- `deltas.len() > max_delta_segments`;
- total delta mutations exceed `max_delta_mutations`;
- tombstone checks exceed a measured ratio of returned candidates;
- snapshot save requests a compact full artifact and no recent checkpoint exists.

Checkpointing merges checkpoint + deltas into one `IndexedRelationshipStore`, compacts tombstones,
and publishes a new `StoreView` with no deltas. Exact revisions retain their older `Arc<StoreView>`
until revision-history retention expires.

### 10.3 Correctness Invariants

- Query results from `StoreView` are equivalent to applying every retained delta in revision order
  to the checkpoint.
- Newer deletes mask older inserted/base rows; newer touches must not produce duplicates.
- Preconditions evaluate against the writer's current `StoreView` before the new delta is created.
- A failed write creates no delta and publishes no revision.
- Exact tokens continue to observe the exact `StoreView` that was current at publication time.
- Snapshot save always writes a canonical compact artifact equivalent to the view, not the physical
  delta layout.

### 10.4 Tradeoff

Segmented deltas shift cost from writes to reads. Reads may inspect a small number of delta
segments and tombstone sets before checkpointing. This is acceptable only while delta thresholds
keep the read overhead bounded and benchmarks prove no > 5% regression in read-heavy workloads.

## 11. Priority 6 - Index Profiles

The full seven-index store is correct for the complete API, but not every deployment needs every
query family. Add explicit index profiles:

```rust
pub enum IndexProfile {
    Full,
    CheckOnly,
    CheckAndObjectAudit,
}
```

Rules:

- `Full` is the default and supports every public API plus writes.
- `CheckOnly` keeps resource-side indexes needed for `check`, `expand`, tuple-to-userset, and
  object-bounded permission checks. It does not support `lookup_resources`.
- `CheckAndObjectAudit` keeps resource-side indexes and the minimum subject-type traversal needed
  for `lookup_subjects` / `lookup_object_permissions`; it may still reject `lookup_resources` if no
  subject reverse index exists.
- Unsupported operations return a typed `EngineError::UnsupportedIndexProfile` instead of scanning.
- Snapshot artifacts record the index profile in the header or a required options section.
- `SnapshotLoadOptions` may reject loading an artifact whose profile cannot satisfy requested
  builder capabilities.

The default public experience remains hard to misuse: users who do not choose a profile get `Full`.
Profiles are an explicit memory/disk optimization for applications with a narrow query surface.

## 12. Performance Budgets

| Phase | Benchmark | Gate |
| --- | --- | --- |
| P1 quick fixes | existing `public_api/check/100k`, `snapshot_load_compact/1m`, concurrent runtime suite | no > 5% regression; writer submit contention recorded |
| P2 ID-native eval | `realworld_authorization/1m_rules/check_doc_inherited_workspace_member` | >= 10% improvement or profile-backed recalibration |
| P2 ID-native eval | `realworld_authorization/1m_rules/mixed_read_workload` | upper estimate <= 55 us |
| P3 lookup | `public_api/lookup_resources/100k` | no regression; allocation count lower than baseline |
| P4 full loader | `snapshot_load_compact/1m` | upper estimate <= 450 ms |
| P4 full loader | `snapshot_load_peak_rss/1m` | max RSS <= 400 MiB |
| P4 trusted loader | `snapshot_load_trusted_fast/1m` | upper estimate <= 200 ms |
| P5 segmented writes | `perf_optimization/write_single_touch_1m` | p95 improves >= 3x versus pre-change 1M baseline |
| P5 segmented writes | `perf_optimization/read_heavy_heavy_write_batched_1m` | read throughput no > 5% regression; write p95 improves >= 2x |
| P6 index profiles | `snapshot_file_size_check_only/1m` and RSS equivalent | >= 20% reduction versus `Full` |

Every target is measured in release builds. If a target fails, the implementation must include a
profile or phase timing that explains whether the design assumption was wrong or the implementation
is incomplete.

## 13. Verification

Required new tests:

- writer actor shutdown and queue-full behavior without sender mutex serialization;
- full-load lazy uniqueness followed by create/touch/delete;
- trusted-load lazy uniqueness failure on a crafted duplicate-row fixture;
- prepared-check equivalence against the public `check` path;
- ID-native recursion, computed userset, tuple-to-userset, intersection, exclusion, and cycle tests;
- streaming lookup result equivalence and limit enforcement;
- segmented store property tests comparing random mutation sequences to a reference `HashSet`;
- checkpoint property tests preserving exact revision behavior;
- index-profile operation support and typed unsupported-operation errors;
- snapshot save/load round trips for every index profile.

Required benchmark additions:

```text
perf_optimization/writer_submit_queue_full
perf_optimization/check_prepared_1m
perf_optimization/lookup_subjects_streaming_1m
perf_optimization/lookup_resources_streaming_1m
perf_optimization/write_single_touch_1m
perf_optimization/write_mixed_batch_1m
perf_optimization/read_heavy_light_write_1m
perf_optimization/read_heavy_medium_write_unbatched_1m
perf_optimization/read_heavy_medium_write_batched_1m
perf_optimization/read_heavy_heavy_write_unbatched_1m
perf_optimization/read_heavy_heavy_write_batched_1m
snapshot_load_compact/1m with phase timers
snapshot_load_peak_rss/1m
snapshot_file_size_check_only/1m
```

Add a Makefile target named `bench-perf-optimization` when the benchmark binary lands. Do not add ad
hoc shell scripts.

## 14. Phasing

### Phase 12.0 - Measurement Spine

- add phase timers for snapshot load;
- add 1M write and mixed read/write benchmarks;
- record pre-change baseline in [71](./71-performance-budgets-design.md).

Exit gate: baseline evidence exists for every P1-P6 target.

### Phase 12.1 - Low-Risk Fixes

- remove writer sender mutex from the submit path;
- remove duplicate schema lookup in prepared checks;
- add sorted relation cache;
- add all-live row iterator fast path;
- switch full-load uniqueness to lazy retained state.

Exit gate: existing correctness tests pass; no benchmark regresses by > 5%.

### Phase 12.2 - ID-Native Evaluation and Streaming Lookup

- add `EvalObject`, `EvalSubject`, and `EvalCheckKey`;
- compile relation IDs into schema expression nodes;
- remove legacy relation/object materialization from recursive check;
- add reusable lookup evaluation contexts and candidate streaming.

Exit gate: evaluator equivalence tests pass and realworld read targets in section 12 are met or
recalibrated with profile evidence.

### Phase 12.3 - Snapshot Loader/Writer Optimization

- fixed section lookup table;
- optional bounded parallel index decode;
- streaming snapshot writer;
- zstd streaming decode/write only if it reduces measured RSS without hurting safety.

Exit gate: full-load, trusted-load, zstd-load, and RSS benchmarks are recorded.

### Phase 12.4 - Segmented Store

- introduce `StoreView`, `StoreDelta`, and checkpoint thresholds internally;
- preserve exact revisions with immutable `Arc<StoreView>`;
- make writer publication append one delta instead of cloning the full store;
- keep snapshot save canonical and independent of physical delta layout.

Exit gate: random mutation property tests pass and 1M write benchmarks meet section 12.

### Phase 12.5 - Index Profiles

- add profile metadata to runtime state and `.szsnap`;
- save/load `Full`, `CheckOnly`, and `CheckAndObjectAudit`;
- return typed unsupported-operation errors for unsupported lookup APIs;
- document builder and snapshot-option ergonomics.

Exit gate: profile-specific tests pass and memory/disk reduction is measured.

## 15. AGENTS.md Binding

- Error Handling: all new failure modes use typed library errors with `thiserror`; no string-only
  error erasure.
- Async & Concurrency: the public core remains synchronous per [99-key-decisions.md D5](./99-key-decisions.md#d5---keep-the-core-synchronous);
  concurrency is actor/message-passing plus immutable snapshots, not shared mutable locks.
- Type Design & API: profile and validation modes are enums, not booleans; builder fields with more
  than five settings use `typed-builder` if they become public structs.
- Safety & Security: no `unsafe`, no `unwrap`/`expect`, no unchecked indexing in boundary or
  snapshot code; every external byte count and range stays checked.
- Serialization: `.szsnap` version/profile changes remain explicit and reject unsupported versions.
- Testing: unit, integration, property, corrupt-file, exact-revision, and benchmark gates in section 13
  are required before implementation is complete.
- Logging & Observability: benchmark-only timers must not leak into stable public API; public spans
  may include phase names but not relationship data.
- Performance: no optimization claim without Criterion/RSS evidence; no speculative dependency
  additions without a dependency review.
- Documentation: public docs must explain when `TrustedFastLoad`, zstd, and index profiles are
  appropriate.

## 16. Risks and Open Questions

- Segmented deltas may hurt read-heavy latency if checkpoint thresholds are too high. Thresholds
  must be benchmark-driven and conservative by default.
- ID-native evaluation and segmented local interners interact. The first segmented implementation
  may translate filters per segment while segment counts are low; a chunked global symbol arena is a
  future option if profiles prove translation cost dominates.
- Parallel index decode may reduce CPU time but increase transient allocations. RSS gates decide
  whether it stays enabled by default.
- Check-only profiles complicate API support. The default remains `Full`; narrow profiles must fail
  unsupported operations loudly.
- A default full loader with hostile-file semantic validation is unlikely to reach <= 200 ms without
  moving proof out of startup, adding mmap/zero-copy, or changing the safety boundary. This spec
  deliberately keeps <= 200 ms as the trusted artifact path.

## 17. Cross-References

- <- Depends on: [16-compact-relationship-store-design.md](./16-compact-relationship-store-design.md), [17-compact-snapshot-format-design.md](./17-compact-snapshot-format-design.md), [18-trusted-fast-snapshot-load-design.md](./18-trusted-fast-snapshot-load-design.md), [20-concurrent-engine-runtime-design.md](./20-concurrent-engine-runtime-design.md), [71-performance-budgets-design.md](./71-performance-budgets-design.md)
- -> Consumed by: [72-testing-verification-plan.md](./72-testing-verification-plan.md), [90-local-engine-roadmap.md](./90-local-engine-roadmap.md), [91-local-engine-impl-plan.md](./91-local-engine-impl-plan.md)
- Related research: [../docs/research/study-spicedb.md](../docs/research/study-spicedb.md)
