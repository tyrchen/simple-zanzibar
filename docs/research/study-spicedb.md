# Study: SpiceDB implementation patterns for Simple Zanzibar

Status: Done
Date: 2026-05-23
Vendor: `vendors/spicedb` at `9a71382960c2912f8debeaaeb98ae9288cb3f092`

## Purpose

This memo studies AuthZed SpiceDB as prior art for improving Simple Zanzibar. The focus is implementation structure, not product surface area: how SpiceDB validates schemas, stores relationships, evaluates checks, handles revisions, and exposes lookup-style APIs. The end result is a set of concrete design choices Simple Zanzibar should adopt, defer, or avoid.

Simple Zanzibar currently has the right core primitives for a first Zanzibar evaluator. It models objects, relations, direct users, userset subjects, relation tuples, namespace configs, and userset expressions in `src/model.rs:5-77`. It evaluates `check` and `expand` recursively in `src/eval.rs:8-223`, and it has a pluggable `TupleStore` trait with an in-memory `HashSet` backend in `src/store.rs:8-81`. The current limitations are visible in those same files: evaluation receives one `NamespaceConfig`, not a whole schema resolver, in `src/eval.rs:8-15`; tuple reads return an eager `Vec<RelationTuple>` in `src/store.rs:20-25`; and the in-memory backend scans the full set and clones every match in `src/store.rs:55-63`.

SpiceDB is much larger than this project should be today. The useful lesson is its separation of concerns: API validation, schema type checking, revision selection, snapshot reads, graph dispatch, relationship storage, and query iteration are distinct layers with narrow contracts.

## Architecture Map

SpiceDB's service path can be summarized as:

```text
gRPC/API request
  -> consistency middleware chooses a revision
  -> datalayer opens a snapshot reader
  -> schema reader and type system validate names and relations
  -> dispatch layer applies depth limits, caching, singleflight, chunking
  -> graph checker or query planner evaluates userset expressions
  -> datastore query/reverse-query APIs read indexed relationships
```

The same layering appears in the code. The API server's `CheckPermission` starts by resolving consistency to a revision and snapshot reader, then loads caveat context and schema, validates the namespace and relation, and calls `computed.ComputeCheck` with revision, schema hash, max depth, and dispatch chunk size in `vendors/spicedb/internal/services/v1/permissions.go:78-137`. The local graph dispatcher checks request depth, parses the revision, loads namespace metadata, validates relation existence, and delegates to the checker in `vendors/spicedb/internal/dispatch/graph/graph.go:274-361`. The datastore contract exposes snapshot readers, optimized/head revisions, revision validation, and watch support in `vendors/spicedb/pkg/datastore/datastore.go:693-760`.

Simple Zanzibar should preserve the core local-process scope from `specs/0001-design.md`, but adopt these boundaries. A single in-process engine can still use a schema resolver, revisioned snapshot reader, graph execution context, and indexed tuple reader without becoming a distributed system.

## Request Paths

### Check

SpiceDB's check path is explicit about each boundary crossing. `CheckPermission` obtains a snapshot reader for a revision in `vendors/spicedb/internal/services/v1/permissions.go:78-84`, builds schema and caveat helpers in `vendors/spicedb/internal/services/v1/permissions.go:85-93`, validates namespace and relation in `vendors/spicedb/internal/services/v1/permissions.go:95-109`, and dispatches a graph check with resource, subject, revision, max depth, schema hash, and chunk size in `vendors/spicedb/internal/services/v1/permissions.go:124-137`. The response is mapped from internal membership to API permissions in `vendors/spicedb/internal/services/v1/permissions.go:168-190`.

Below the API layer, the dispatcher protects evaluation. The dispatcher interface separates check, expand, lookup subjects, lookup resources, query planning, close, and readiness in `vendors/spicedb/internal/dispatch/dispatch.go:23-37`. It also centralizes depth validation in `vendors/spicedb/internal/dispatch/dispatch.go:102-115`. The local dispatcher loads the namespace from schema metadata and chooses the relation before calling the checker in `vendors/spicedb/internal/dispatch/graph/graph.go:233-361`.

The graph checker avoids treating every expression the same way. It deduplicates resources, rejects wildcard subjects for check, handles direct checks separately from rewrite checks, and tracks membership results in `vendors/spicedb/internal/graph/check.go:165-245`. Direct checks split exact user matches from non-terminal userset paths, query the datastore, then dispatch non-terminal subject checks in chunks in `vendors/spicedb/internal/graph/check.go:304-537`. Rewrite checks dispatch union, intersection, exclusion, computed-userset, tuple-to-userset, and functioned arrows through set-operation helpers in `vendors/spicedb/internal/graph/check.go:539-596`.

Simple Zanzibar currently performs the same conceptual work, but in one recursive function. `check_internal` fetches relation config, picks `This` as the default rewrite, and calls `eval_expression` in `src/eval.rs:21-36`. `eval_expression` handles direct tuples, indirect usersets, computed usersets, tuple-to-userset, union, intersection, and exclusion in `src/eval.rs:48-125`. The next version should keep the evaluator understandable, but split it into the same logical pieces SpiceDB has: request validation, schema lookup, execution context, direct relation query, userset dispatch, and set algebra.

### Relationship Writes

SpiceDB's relationship write path is a model for the write API Simple Zanzibar should grow toward. The service caps update counts and precondition counts in `vendors/spicedb/internal/services/v1/relationships.go:359-372`, rejects duplicate updates and invalid caveat/expiration combinations in `vendors/spicedb/internal/services/v1/relationships.go:374-404`, converts API updates into validated tuple updates in `vendors/spicedb/internal/services/v1/relationships.go:407-433`, checks preconditions in the same transaction in `vendors/spicedb/internal/services/v1/relationships.go:445-448`, writes relationships in `vendors/spicedb/internal/services/v1/relationships.go:450-452`, and returns a revision token in `vendors/spicedb/internal/services/v1/relationships.go:469-476`.

Preconditions are deliberately simple. Each precondition is converted to a datastore filter, queried with a limit of one, and interpreted as `MUST_MATCH` or `MUST_NOT_MATCH` in `vendors/spicedb/internal/services/v1/preconditions.go:17-55`. That is a good fit for Simple Zanzibar: it gives callers atomic "write only if this relationship exists/does not exist" behavior without exposing a general transaction language.

The memdb backend shows how writes should behave even in an in-memory engine. It serializes write transactions, creates a new revision, runs the caller's transaction, records changed relationships and schema, commits, and appends a snapshot with the schema hash in `vendors/spicedb/internal/datastore/memdb/memdb.go:175-368`. Individual relationship mutations distinguish create, touch, and delete, and return specific errors for duplicate creates or missing deletes in `vendors/spicedb/internal/datastore/memdb/readwrite.go:50-132`.

Simple Zanzibar's `TupleStore` writes one tuple at a time and returns `Result<(), String>` in `src/store.rs:27-39`. That is fine for a prototype, but the next store trait should use domain error types, batch updates, transactional preconditions, and a returned revision.

### Schema Writes

SpiceDB treats schema writes as compilation and validation, not just text storage. The schema service compiles DSL with feature options in `vendors/spicedb/internal/services/v1/schema.go:122-148`, validates type-system changes in `vendors/spicedb/internal/services/v1/schema.go:152-156`, applies changes in a transaction in `vendors/spicedb/internal/services/v1/schema.go:158-175`, and returns a revision token in `vendors/spicedb/internal/services/v1/schema.go:180-187`.

The shared schema layer validates caveats and definitions, builds a resolver-backed type system, and validates every definition in `vendors/spicedb/internal/services/shared/schema.go:40-79`. It also compares the new schema against the old one, checks removed caveat parameters and type changes, prevents deleting namespaces that still have relationships, writes the full stored schema, and returns operation counts plus schema hash in `vendors/spicedb/internal/services/shared/schema.go:136-313`.

The compiler stage is also isolated. `Compile` parses source, validates and resolves imports, translates definitions, builds caveat type references, and returns a `CompiledSchema` in `vendors/spicedb/pkg/schemadsl/compiler/compiler.go:126-190`. Parse errors are mapped into source-aware compiler errors in `vendors/spicedb/pkg/schemadsl/compiler/compiler.go:193-201` and `vendors/spicedb/pkg/schemadsl/compiler/compiler.go:244-278`.

Simple Zanzibar already has `NamespaceConfig`, `RelationConfig`, and `UsersetExpression` in `src/model.rs:39-77`, but it should stop treating the parsed result as automatically valid. The schema pipeline should be `parse -> compile -> type-check -> apply`, where the type-check phase verifies references before any tuple write or check can use the schema.

## Load-Bearing Data Structures

### DataLayer, Datastore, and Revision

SpiceDB uses a `DataLayer` wrapper around the datastore instead of passing raw storage everywhere. The wrapper exposes snapshot readers, read-write transactions, optimized/head revision methods, revision parsing, watch, features, IDs, metrics, and close in `vendors/spicedb/pkg/datalayer/datalayer.go:27-44`. Its schema and revisioned reader interfaces keep schema reads, relationship queries, reverse relationship queries, and counter reads separate in `vendors/spicedb/pkg/datalayer/datalayer.go:52-106`.

The datastore interface is revision-centric. A snapshot reader is opened at a revision in `vendors/spicedb/pkg/datastore/datastore.go:693-701`, optimized and head revisions are separate methods in `vendors/spicedb/pkg/datastore/datastore.go:703-711`, and revision validation/parsing is explicit in `vendors/spicedb/pkg/datastore/datastore.go:713-722`. A `Revision` must be comparable, stringifiable, byte-sortable, and cloneable in `vendors/spicedb/pkg/datastore/datastore.go:986-1002`. Schema hash can travel with a revision in `vendors/spicedb/pkg/datastore/datastore.go:1026-1032`.

Simple Zanzibar should introduce a small Rust version of this boundary:

```text
Engine
  owns SchemaStore + RelationshipStore
  resolves Check/Expand/Lookup requests

SnapshotReader
  reads schema and relationships at Revision

ReadWriteTransaction
  validates and writes schema/relationship mutations
```

This does not require persistence. SpiceDB's memdb backend proves revisioned snapshots are useful even in memory: it keeps database snapshots with revisions and schema hashes in `vendors/spicedb/internal/datastore/memdb/memdb.go:86-100`, selects a valid snapshot for a requested revision in `vendors/spicedb/internal/datastore/memdb/memdb.go:116-152`, and rejects revisions outside the GC window or in the future in `vendors/spicedb/internal/datastore/memdb/revisions.go:100-132`.

### Schema Type System

SpiceDB's `TypeSystem` is a cached view over validated definitions and a resolver in `vendors/spicedb/pkg/schema/typesystem.go:20-36`. It validates relation rewrites by checking computed-userset relation existence, tuple-to-userset left relation existence, permission restrictions on tuple-to-userset, wildcard rules, and functioned tuple-to-userset semantics in `vendors/spicedb/pkg/schema/typesystem_validation.go:37-172`. It validates direct subject relation types by checking duplicate allowed relations, namespace existence, relation existence, transitive wildcard constraints, and caveat references in `vendors/spicedb/pkg/schema/typesystem_validation.go:204-288`.

The immediate lesson for Simple Zanzibar is to move invariants out of `check`. A `Schema` type should hold every namespace, relations should be resolved through a typed resolver, and invalid relation references should be rejected during schema application rather than discovered during a user request. This is especially important because Simple Zanzibar's evaluator receives one `NamespaceConfig` in `src/eval.rs:8-15`, but tuple-to-userset and userset subjects naturally cross namespaces through `User::Userset(Object, Relation)` in `src/model.rs:24-27`.

### Query Filters and Indexes

SpiceDB's datastore reader supports both resource-oriented and subject-oriented access. The reader exposes `QueryRelationships` and `ReverseQueryRelationships` in `vendors/spicedb/pkg/datastore/datastore.go:538-561`. Memdb chooses indexes for resource filters in `vendors/spicedb/internal/datastore/memdb/readonly.go:360-389` and filters tuples with resource, relation, subject, caveat, and expiration predicates in `vendors/spicedb/internal/datastore/memdb/readonly.go:391-457`. Reverse relationship queries use subject-side indexes in `vendors/spicedb/internal/datastore/memdb/readonly.go:168-232`.

Simple Zanzibar's `read_tuples` can express only object plus optional relation/user filters and returns a vector in `src/store.rs:20-25`. Its implementation linearly scans a single `HashSet` in `src/store.rs:55-63`. The next in-memory store should maintain at least two indexes:

```text
by_resource: (object_type, object_id, relation) -> relationships
by_subject:  (subject_type, subject_id, optional relation) -> relationships
```

Those indexes enable fast direct checks, userset expansion, `LookupResources`, and `LookupSubjects`.

### Membership Results

SpiceDB does not reduce every intermediate result to `bool`. Its membership set tracks members and caveat expressions in `vendors/spicedb/internal/graph/membershipset.go:39-44`, combines caveats from direct and parent paths in `vendors/spicedb/internal/graph/membershipset.go:46-93`, merges duplicate members in `vendors/spicedb/internal/graph/membershipset.go:95-118`, and implements union, intersection, and subtraction in `vendors/spicedb/internal/graph/membershipset.go:120-173`.

Simple Zanzibar can start with `Allowed` and `Denied`, but should shape its internal result as an enum instead of plain `bool`:

```text
Membership
  Allowed
  Denied
  Conditional(expression)   # later, only if caveats are added
```

That keeps future caveats from forcing a rewrite of every set-operation function.

### Dispatch Sets and Chunking

SpiceDB batches non-terminal checks. `checkDispatchSet` groups resources by subject type and subject relation in `vendors/spicedb/internal/graph/checkdispatchset.go:13-24`, records relationships to check in `vendors/spicedb/internal/graph/checkdispatchset.go:67-90`, and emits sorted chunks by subject type and caveat priority in `vendors/spicedb/internal/graph/checkdispatchset.go:92-135`. The graph checker's direct path uses this to dispatch userset checks in chunks in `vendors/spicedb/internal/graph/check.go:472-512`.

Simple Zanzibar does not need distributed dispatch, but it should introduce an `EvaluationContext` with max depth, visited state, and future chunk size. Today it passes a mutable visited `HashSet` through recursive calls in `src/eval.rs:8-18` and removes the key on unwind in `src/eval.rs:32-36`. That is a reasonable start, but depth should become an explicit bounded resource, and userset-subject checks should be grouped before recursive evaluation when a relation has many userset subjects.

### Query Iterator Plans

SpiceDB has an iterator-based query planner that can serve check, subject iteration, and resource iteration from one plan tree. A `Plan` contains check, subject iteration, resource iteration, and explain functions in `vendors/spicedb/pkg/query/types.go:12-32`. The `Iterator` interface exposes shape, clone, subiterator replacement, canonicalization, and resource/subject type metadata in `vendors/spicedb/pkg/query/types.go:34-81`. The query context carries the executor, snapshot reader, caveat runner, recursion bounds, operation kind, batched-arrow support, pagination, observers, and recursive frontier state in `vendors/spicedb/pkg/query/context.go:13-56`.

The outline builder converts schema expressions into iterator trees for arrow, nil, self, relation references, union, intersection, exclusion, and functioned arrows in `vendors/spicedb/pkg/query/build_tree.go:180-282`. It detects recursive outlines and wraps them with recursive iterators in `vendors/spicedb/pkg/query/build_tree.go:58-146`. Datastore iterators support `Check`, `IterSubjects`, and `IterResources` over direct stored relationships in `vendors/spicedb/pkg/query/datastore.go:47-300`.

This is not the first refactor Simple Zanzibar needs. It is the architecture to target once the project has typed schemas and indexed snapshot reads. The reason to keep it in view now is API coherence: `check`, `expand`, `lookup_resources`, and `lookup_subjects` should not grow four unrelated evaluators.

## Algorithms Worth Porting Conceptually

### Direct Check, Then Non-Terminal Usersets

SpiceDB's direct relation check first queries direct membership, then separately handles userset subjects that require recursive dispatch. It builds a resource filter, queries relationships, records direct member matches, then builds dispatch work for non-terminal subjects in `vendors/spicedb/internal/graph/check.go:406-512`. This split is sharper than Simple Zanzibar's current `This` evaluation, which first checks the exact tuple and then scans every tuple for the object relation in `src/eval.rs:49-68`.

Simple Zanzibar should make `This` evaluation a store-level operation:

```text
query direct subject match
if found: return Allowed
query non-terminal userset subjects for object#relation
for each grouped userset subject: recursively check subject object#relation
```

That preserves readability while allowing indexes and batching to work.

### Tuple-To-Userset

SpiceDB's tuple-to-userset path queries the tupleset relation on the left side, builds dispatch requests for the computed relation on each intermediate object, and maps successful responses back to the original resource in `vendors/spicedb/internal/graph/check.go:942-1023`. It has a specialized intersection variant that requires all intermediate subjects to satisfy the right-side permission in `vendors/spicedb/internal/graph/check.go:802-940`.

Simple Zanzibar's tuple-to-userset implementation already follows the basic two-step shape: read tuples from the left relation, extract userset objects, and recursively check the computed relation in `src/eval.rs:76-96`. The missing pieces are schema validation that the tupleset relation can point to the expected target type, datastore filters that avoid scanning, and bounded execution context for fanout.

### Set Algebra and Short-Circuiting

SpiceDB's union, intersection, and difference helpers carry metadata, handle caveats, run subproblems concurrently, and short-circuit when possible. Union returns early for a single-resource allowed result in `vendors/spicedb/internal/graph/check.go:1072-1117`. Intersection returns early when the working result set is empty in `vendors/spicedb/internal/graph/check.go:1119-1171`. Difference evaluates the base set, subtracts exclusions, and combines caveat state in `vendors/spicedb/internal/graph/check.go:1174-1254`.

Simple Zanzibar's `Union`, `Intersection`, and `Exclusion` branches short-circuit for booleans in `src/eval.rs:97-124`. That behavior is good for `check`, but `expand` accumulates expression trees without deduplication in `src/eval.rs:200-221`. A shared membership algebra would make `check`, `expand`, and lookup operations more consistent.

### Revision Tokens

SpiceDB's consistency middleware chooses revisions from client consistency options. It uses optimized revisions for minimum latency, head revisions for full consistency, "at least as fresh" comparisons, and exact snapshot token decoding in `vendors/spicedb/pkg/middleware/consistency/consistency.go:123-225`. It chooses between requested and current revisions while handling datastore ID mismatch policies in `vendors/spicedb/pkg/middleware/consistency/consistency.go:284-330`.

The token carries more than a number. `zedtoken.NewFromRevision` builds a token from revision, datastore unique ID prefix, and schema hash in `vendors/spicedb/pkg/zedtoken/zedtoken.go:70-111`. `DecodeRevision` supports legacy tokens, datastore mismatch status, and schema hash extraction in `vendors/spicedb/pkg/zedtoken/zedtoken.go:141-220`.

Simple Zanzibar should add a small local token once writes become transactional:

```text
ConsistencyToken {
  revision: u64,
  schema_hash: [u8; 32],
  datastore_id: Uuid,
}
```

This is useful even without distributed consistency. It gives tests deterministic snapshot reads and gives callers a way to say "check at the revision returned by my write".

### Caching and Singleflight

SpiceDB layers caching and request coalescing outside the graph checker. The caching dispatcher computes a check key, reads cached responses only when the request has enough remaining depth for the cached depth requirement, and adjusts dispatch counts before returning in `vendors/spicedb/internal/dispatch/caching/caching.go:174-231`. The singleflight dispatcher canonicalizes requests and avoids coalescing likely recursive-loop requests using traversal bloom state in `vendors/spicedb/internal/dispatch/singleflight/singleflight.go:47-93`.

Simple Zanzibar should not implement this immediately. The useful design lesson is placement: if caching is added, make it a wrapper around an evaluator or dispatcher interface, not a feature baked into expression evaluation.

### Reachability and Planner Optimizations

SpiceDB builds reachability information from schema rewrites. It computes reachability for definitions in `vendors/spicedb/pkg/schema/reachabilitygraphbuilder.go:20-43`, walks rewrites in `vendors/spicedb/pkg/schema/reachabilitygraphbuilder.go:45-148`, and expands tuple-to-userset reachability through allowed direct relation types in `vendors/spicedb/pkg/schema/reachabilitygraphbuilder.go:150-216`.

The query planner then applies optimizers through a registry in `vendors/spicedb/pkg/query/queryopt/registry.go:46-121`. One optimizer prunes leaves that cannot reach the requested subject type in `vendors/spicedb/pkg/query/queryopt/reachability_pruning.go:21-70`.

Simple Zanzibar should use reachability first for validation and explainability. Planner pruning can wait until lookup APIs need performance.

## What Simple Zanzibar Should Adopt

### 1. Whole-Schema Resolver

Replace single-namespace evaluation with a `Schema` that owns all namespace definitions. Keep `NamespaceConfig` as a component, but give the evaluator a resolver. This is necessary because userset subjects already carry object namespaces in `src/model.rs:24-27`, while the evaluator can only look up relations inside one `NamespaceConfig` in `src/eval.rs:21-24`.

### 2. Typed Schema Validation Before Use

Add a validation pass that checks computed-userset and tuple-to-userset references before schema application. SpiceDB validates computed usersets, tuple-to-userset left relations, permission restrictions, wildcard constraints, and functioned arrows in `vendors/spicedb/pkg/schema/typesystem_validation.go:37-172`. Simple Zanzibar can start with a smaller rule set:

- every referenced relation exists in the same namespace for `ComputedUserset`
- every tuple-to-userset left relation exists
- every tuple-to-userset target relation exists on allowed target namespaces
- relation rewrite cycles are either rejected or bounded by explicit recursion rules

### 3. Revisioned In-Memory Store

Introduce revisions and immutable snapshots before adding disk persistence. SpiceDB's memdb opens snapshot readers by revision in `vendors/spicedb/internal/datastore/memdb/memdb.go:116-152` and creates new revisions for write transactions in `vendors/spicedb/internal/datastore/memdb/memdb.go:175-368`. Simple Zanzibar can implement this with `Arc<Snapshot>` values and an append-only revision counter.

### 4. Relationship Query API With Directional Indexes

Replace `read_tuples(object, relation, user) -> Vec<RelationTuple>` with query objects and iterators. SpiceDB separates resource-side and subject-side queries in `vendors/spicedb/pkg/datastore/datastore.go:538-561`. That is the foundation for efficient check and lookup. Simple Zanzibar can expose:

```text
query_relationships(RelationshipFilter) -> RelationshipIter
reverse_query_relationships(SubjectFilter) -> RelationshipIter
```

The in-memory implementation can preserve a `HashSet` for uniqueness and add `HashMap` indexes for resource and subject lookup.

### 5. Transactional Batch Writes With Preconditions

Move from one-tuple writes to atomic relationship mutations. SpiceDB caps request sizes, validates updates, checks preconditions, writes relationships, and returns a token in `vendors/spicedb/internal/services/v1/relationships.go:359-476`. Simple Zanzibar should support `Create`, `Touch`, and `Delete` mutations plus `must_match` and `must_not_match` preconditions. Return a typed error enum instead of `String`, since current store errors are plain strings in `src/store.rs:31-39`.

### 6. Execution Context

Create an `EvaluationContext` that owns recursion depth, visited state, revision, schema hash, trace metadata, and future limits. SpiceDB validates depth in dispatch metadata in `vendors/spicedb/internal/dispatch/dispatch.go:102-115`, while Simple Zanzibar currently passes only a visited set through recursive evaluation in `src/eval.rs:8-18`. Bounded depth gives clearer failure behavior than cycle-only detection.

### 7. Shared Membership Algebra

Use a membership result type internally, even if the public API remains `bool`. SpiceDB's membership set implements union, intersection, and subtraction with caveat-aware state in `vendors/spicedb/internal/graph/membershipset.go:120-173`. A simplified enum-based version will let Simple Zanzibar reuse one algebra for `check`, `expand`, and later lookup APIs.

### 8. Lookup APIs Designed Around the Same Engine

SpiceDB's lookup resources path shares the same consistency, snapshot, schema validation, cursor, duplicate suppression, and dispatch layers as check in `vendors/spicedb/internal/services/v1/permissions.go:492-653`. Simple Zanzibar should add lookup APIs only after indexed reads and execution context exist, so they reuse the graph engine instead of scanning all tuples separately.

### 9. Query Planner as a Later Internal API

Keep the iterator architecture in mind, but do not start there. SpiceDB's plan and iterator contracts are powerful because they support check, subject iteration, and resource iteration in one tree in `vendors/spicedb/pkg/query/types.go:12-81`. Simple Zanzibar should first stabilize typed schema, snapshot reads, and graph evaluation; then introduce a planner if lookup behavior becomes important enough to justify the complexity.

## What Simple Zanzibar Should Defer or Avoid

### Distributed Dispatch

SpiceDB's combined dispatcher can wrap local graph dispatch with caching, concurrency controls, upstream dispatch, secondary dispatch, and stream timeouts in `vendors/spicedb/internal/dispatch/combined/combined.go:51-260`. Simple Zanzibar should avoid remote dispatch until it has a real multi-node requirement.

### Full Caveat Semantics

SpiceDB carries caveat context through check requests in `vendors/spicedb/internal/services/v1/permissions.go:85-93`, validates caveat schema changes in `vendors/spicedb/internal/services/shared/schema.go:316-350`, and combines caveats in membership sets in `vendors/spicedb/internal/graph/membershipset.go:46-93`. Simple Zanzibar should leave result types open for conditional membership, but keep caveat evaluation out of the next core refactor.

### Watch API

SpiceDB's datastore watch API has checkpoint, buffering, timeout, content, byte cap, and emission strategy options in `vendors/spicedb/pkg/datastore/datastore.go:622-668`. Memdb implements watch by tracking changelog entries and emitting relationship, schema, and checkpoint changes in `vendors/spicedb/internal/datastore/memdb/watch.go:17-148`. Simple Zanzibar should add watch only when it has secondary indexes or external consumers that need change streams.

### Legacy Compatibility Paths

SpiceDB supports legacy zookies and compatibility modes in token decoding in `vendors/spicedb/pkg/zedtoken/zedtoken.go:141-220`, and it applies schema over old/new storage paths in `vendors/spicedb/internal/services/shared/schema.go:136-294`. Simple Zanzibar has no compatibility burden yet. Its implementation should be clean and direct.

### Early Planner Complexity

SpiceDB's query planner has recursive sentinels, optimizer registries, batched arrows, pagination state, and observers in `vendors/spicedb/pkg/query/context.go:13-56`, `vendors/spicedb/pkg/query/build_tree.go:58-146`, and `vendors/spicedb/pkg/query/queryopt/registry.go:46-121`. Simple Zanzibar should not port that whole system until typed schemas and indexed storage are in place.

## Recommended Build Order

1. **Schema package**: create `Schema`, `NamespaceDefinition`, relation definitions, a resolver, and a validation pass over `UsersetExpression`.
2. **Store package**: replace eager tuple reads with query filters, iterators, resource index, subject index, and typed errors.
3. **Revision package**: add monotonic revisions, schema hash, datastore ID, snapshot readers, and consistency tokens.
4. **Write API**: add batch relationship mutations with preconditions and atomic revision returns.
5. **Evaluator package**: refactor `check_internal` into request validation, direct check, userset dispatch, tuple-to-userset, and set algebra modules.
6. **Expand and lookup**: implement them on top of shared membership/query primitives instead of separate scans.
7. **Planner spike**: evaluate whether an iterator plan tree is warranted after lookup APIs exist.

This order follows the smallest useful dependency chain. Schema validation makes relationship validation possible. Indexed snapshots make bounded evaluation possible. Transactional writes make consistency tokens useful. Only after those are stable should lookup and planner work start.

## Open Design Questions

### Snapshot Representation

SpiceDB memdb stores revisioned snapshots and selects an appropriate snapshot for reads in `vendors/spicedb/internal/datastore/memdb/memdb.go:116-152`. Simple Zanzibar should choose between full immutable snapshots, structural sharing with `Arc`, or append-only log replay. Full snapshots are simplest; structural sharing is probably the right second step if memory use becomes visible.

### Parser Direction

SpiceDB has a full schema compiler pipeline in `vendors/spicedb/pkg/schemadsl/compiler/compiler.go:126-190`. Simple Zanzibar currently has a parser module, and the repo's project instructions prefer `winnow` for grammar parsing. Before touching the DSL, compare the current parser against a minimal SpiceDB-compatible grammar and decide whether to migrate parser internals while keeping public types stable.

### Recursion Policy

SpiceDB's query planner has explicit recursive iterators and a default max recursion depth of 50 in `vendors/spicedb/pkg/query/recursive.go:43-68`. Simple Zanzibar currently detects exact repeated `(object, relation, user)` triples and returns false in `src/eval.rs:16-19`. The next evaluator should make recursion semantics explicit: cycle rejection, max-depth error, or SpiceDB-style bounded recursive expansion.

### Conditional Permissions

SpiceDB's caveat-aware membership set shows that conditional results affect every set operation in `vendors/spicedb/internal/graph/membershipset.go:46-173`. Simple Zanzibar should not add caveats casually. If conditional permissions are added later, they should enter through the membership algebra and schema validator, not as special cases in `check`.

## Bottom Line

The main thing to borrow from SpiceDB is not its distributed machinery. It is the discipline of treating authorization as a layered engine:

```text
validated schema
  + revisioned indexed relationships
  + bounded graph execution
  + shared membership algebra
  + consistency tokens
```

That is the shortest path from the current Simple Zanzibar prototype to a small but serious Rust authorization engine.
