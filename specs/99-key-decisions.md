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

## D11 - Compact relationship storage before advanced query planning

- Context: The `org_authorization` benchmark showed direct checks remain around 2-7 us from 1k to 1M rules, but 1M rules consume roughly 3.12 GiB max RSS. The limiting factor is memory, not check latency.
- Alternatives considered: add a query planner first; add caching; use compressed roaring bitmaps immediately; compact the existing indexed store.
- Decision: compact the existing store first by removing duplicate ownership, replacing `BTreeSet<usize>` postings with `Vec<RowId>`, and interning identifiers into fixed-width rows.
- Why: the evaluator and indexes already provide the right asymptotic read path. Query planning and caching would not remove cloned strings, cloned snapshots, legacy tuple mirrors, or pointer-heavy posting lists.
- Pinned by: [16-compact-relationship-store-design.md](./16-compact-relationship-store-design.md), [71-performance-budgets-design.md](./71-performance-budgets-design.md), [91-local-engine-impl-plan.md](./91-local-engine-impl-plan.md)
- Date: 2026-05-23

## D12 - Use append-only `Vec<RowId>` postings with tombstones before compressed bitmaps

- Context: Current indexes use `BTreeSet<usize>` postings. This gives uniqueness and deletion support but allocates heavily and has poor locality. Store-level uniqueness already prevents duplicate live rows.
- Alternatives considered: keep `BTreeSet`; switch directly to `roaring = 0.11.4`; use append-only `Vec<RowId>` plus tombstones and periodic compaction.
- Decision: use `Vec<RowId>` postings, `HashMap<RelationshipRow, RowId>` uniqueness, a compact live-row bitset, and write-path compaction for tombstone-heavy workloads.
- Why: direct check and lookup need fast iteration over candidate rows, not general set algebra over posting lists. Contiguous vectors are simpler, smaller, and likely faster. Roaring remains a future option if measured sparse-posting workloads need it.
- Pinned by: [16-compact-relationship-store-design.md](./16-compact-relationship-store-design.md), [72-testing-verification-plan.md](./72-testing-verification-plan.md)
- Date: 2026-05-23

## D13 - Serialize compact snapshots as stable sectioned artifacts

- Context: After M6, 1M-rule steady-state RSS meets the compact-store target, but cold startup still has to construct the compact shape from logical relationships unless a prebuilt artifact exists. The latest 1M filtered benchmark takes about 2.32 s end to end, but that is not pure load time because it includes process startup, schema parse/compile, generated relationship construction, compact snapshot construction, validation, Criterion warmup, measurement, and analysis.
- Alternatives considered: keep rebuilding from text/domain relationships; serialize Rust structs with `serde`/`bincode`; serialize runtime `HashMap` internals; define a stable sectioned compact snapshot format.
- Decision: define a versioned `.szsnap` artifact with explicit header, section directory, schema, byte-arena symbols, compact rows, stable sorted index sections, posting ranges, and a checksum footer.
- Why: stable sectioned bytes can be loaded with checked parsing and minimal per-relationship allocation, while avoiding unstable Rust collection internals and preserving future format evolution. The first implementation prioritizes load speed and bounded load-time RSS; compression and mmap are deferred until benchmark evidence justifies their complexity and safety tradeoffs.
- Pinned by: [17-compact-snapshot-format-design.md](./17-compact-snapshot-format-design.md), [71-performance-budgets-design.md](./71-performance-budgets-design.md), [91-local-engine-impl-plan.md](./91-local-engine-impl-plan.md)
- Date: 2026-05-23

## D14 - Split full validation from trusted fast-load

- Context: Profiling the first `.szsnap` loader showed the 1M-rule load path spends roughly 313 ms
  in row semantic validation, 113 ms in index semantic validation, and 85 ms rebuilding symbol
  lookup maps. A controlled experiment that kept checksum and structural parsing but trusted
  row/index semantics measured about 188 ms, proving the 200 ms goal is viable only when repeated
  semantic proof moves out of process startup.
- Alternatives considered: weaken the default loader; unconditionally skip checksum; add mmap; keep
  one loader and accept ~600 ms; add an explicit trusted mode with serialized lookup tables.
- Decision: keep full validation as the default for hostile files and add
  `SnapshotValidationMode::TrustedFastLoad` for build-pipeline validated artifacts. `.szsnap` v2
  includes serialized symbol hash and lookup permutation sections so trusted mode can query without
  rebuilding Rust `HashMap` internals.
- Why: this matches the rsync-like principle of doing expensive proof once and reusing a stable
  artifact identity, while keeping the runtime trust boundary explicit. Checksum remains the safe
  default; external byte identity is a separate explicit choice.
- Pinned by: [18-trusted-fast-snapshot-load-design.md](./18-trusted-fast-snapshot-load-design.md), [70-security-design.md](./70-security-design.md), [71-performance-budgets-design.md](./71-performance-budgets-design.md), [91-local-engine-impl-plan.md](./91-local-engine-impl-plan.md)
- Date: 2026-05-23

## D15 - Use external artifact integrity for the hard 200 ms load path

- Context: After moving symbol lookup to stable v2 sections, trusted fast-load with checksum stayed
  close to but above the hard 200 ms gate because BLAKE3 still rehashed the whole artifact at
  startup. The measured `TrustedFastLoad + External` path reached `[151.06 ms, 152.23 ms, 153.35 ms]`.
- Alternatives considered: weaken full validation; drop checksum for every trusted load; accept
  ~205 ms; require mmap; add a separate integrity mode restricted to trusted artifacts.
- Decision: add `SnapshotIntegrityMode::{Checksum, External}`. `Checksum` remains the default.
  `External` is accepted only with `SnapshotValidationMode::TrustedFastLoad` and means byte identity
  was already proven by a content-address, signed manifest, release checksum, or equivalent layer.
- Why: the application should not repeat an O(file-size) proof if deployment has already pinned the
  exact artifact. Keeping this as an explicit option preserves the untrusted-file default and makes
  the supply-chain tradeoff visible in code review.
- Pinned by: [18-trusted-fast-snapshot-load-design.md](./18-trusted-fast-snapshot-load-design.md), [71-performance-budgets-design.md](./71-performance-budgets-design.md), [72-testing-verification-plan.md](./72-testing-verification-plan.md)
- Date: 2026-05-23

## D16 - Treat zstd as an outer snapshot transport wrapper

- Context: The `.szsnap` v2 payload is already close to the in-memory compact representation and
  meets the trusted 200 ms raw load target. zstd is valuable for distribution and disk footprint,
  but decoding it during startup adds CPU and transient memory that should not redefine the raw
  fast-load contract.
- Alternatives considered: compress individual `.szsnap` sections; add a new compressed file
  version; keep compression entirely outside the crate; wrap the raw `.szsnap` bytes in one zstd
  frame.
- Decision: add `SnapshotCompression::Zstd` as a public save/load option that compresses or
  decompresses the entire raw `.szsnap` payload before the existing v2 parser runs.
- Why: this preserves the stable binary format, keeps corrupt-file validation in one parser, and
  lets deployments choose between direct compressed load and the faster content-addressed cache
  pattern of decompress-once then raw trusted fast-load.
- Pinned by: [19-public-api-completeness-design.md](./19-public-api-completeness-design.md),
  [17-compact-snapshot-format-design.md](./17-compact-snapshot-format-design.md),
  [18-trusted-fast-snapshot-load-design.md](./18-trusted-fast-snapshot-load-design.md)
- Date: 2026-05-23

## D17 - Add bounded audit helpers instead of an unbounded global permission matrix

- Context: Users need "what permissions does this subject have on this object?" and "who has which
  permissions on this object?" as public API calls. A full global matrix over every object, subject,
  and relation would be expensive and easy to misuse in an embedded library.
- Alternatives considered: require callers to enumerate schema relations manually; add a global
  audit scan; add object/subject-bounded helpers that compose existing evaluators.
- Decision: add `lookup_permissions` for one subject/object pair and `lookup_object_permissions`
  for one object plus one subject type. Both enumerate only the target object's schema relations and
  reuse the existing `check` / `lookup_subjects` semantics.
- Why: the helpers cover common product questions while preserving Zanzibar's explicit subject-type
  lookup boundary and existing evaluator limits.
- Pinned by: [19-public-api-completeness-design.md](./19-public-api-completeness-design.md),
  [14-evaluation-engine-design.md](./14-evaluation-engine-design.md),
  [71-performance-budgets-design.md](./71-performance-budgets-design.md)
- Date: 2026-05-23

## D18 - Remove the legacy mutable service facade before API stabilization

- Context: the legacy mutable facade kept early compatibility while the strict engine API matured, but the
  crate API is not yet stable and the mutable facade now encourages service-level locking and
  duplicate examples.
- Alternatives considered: keep the legacy mutable facade indefinitely; hide it behind a `compat` feature;
  remove it and make `ZanzibarEngine` the only public runtime.
- Decision: remove the legacy mutable facade from the public API, examples, tests, and benchmarks.
- Why: the runtime contract should be one coherent engine: lock-free snapshot reads, typed request
  APIs, actor-backed writes, and policy/snapshot import/export on the same facade. Keeping a second
  mutable public type would force future compatibility work around a shape we already know is not
  the intended API.
- Pinned by: [20-concurrent-engine-runtime-design.md](./20-concurrent-engine-runtime-design.md),
  [15-public-api-design.md](./15-public-api-design.md),
  [72-testing-verification-plan.md](./72-testing-verification-plan.md)
- Date: 2026-05-23

## D19 - Scale writes by batching and tenant sharding before fine-grained locks

- Context: Relationship writes, preconditions, schema replacement, consistency tokens, and snapshot
  publication all require a single linearized write order inside one authorization state.
- Alternatives considered: per-object locks; per-namespace locks; optimistic CAS publish retries;
  a bounded single writer actor with caller batching; tenant-level sharding.
- Decision: use a single writer actor per `ZanzibarEngine`, make batch writes the throughput path,
  and add `ZanzibarTenantShards` for applications with independent tenant authorization states.
- Why: fine-grained locks would still need a global publish point and would complicate precondition
  correctness. Batching reduces per-write fixed cost, and tenant sharding moves true independence
  into separate revision/token spaces instead of pretending one tenant's global revision can be
  partitioned by object.
- Pinned by: [20-concurrent-engine-runtime-design.md](./20-concurrent-engine-runtime-design.md),
  [13-revision-consistency-design.md](./13-revision-consistency-design.md),
  [71-performance-budgets-design.md](./71-performance-budgets-design.md)
- Date: 2026-05-23
