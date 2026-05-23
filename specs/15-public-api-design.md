# 15 - Public API Design

Status: draft v1
Owner: Simple Zanzibar
Depends on: [14-evaluation-engine-design.md](./14-evaluation-engine-design.md)

## 1. Purpose

This spec defines the crate-facing API and migration shape. The new engine is strict and typed.
[20-concurrent-engine-runtime-design.md](./20-concurrent-engine-runtime-design.md) supersedes the
early compatibility plan and removes the legacy mutable facade from the public API before stabilization.

## 2. Public Surface

```rust
pub struct ZanzibarEngine { /* private */ }

pub struct ZanzibarEngineBuilder { /* private */ }

impl ZanzibarEngine {
    pub fn builder() -> ZanzibarEngineBuilder;
    pub fn check(&self, request: CheckRequest) -> Result<CheckResponse, EngineError>;
    pub fn expand(&self, request: ExpandRequest) -> Result<ExpandResponse, EngineError>;
    pub fn lookup_resources(
        &self,
        request: LookupResourcesRequest,
    ) -> Result<LookupResources, EngineError>;
    pub fn lookup_subjects(
        &self,
        request: LookupSubjectsRequest,
    ) -> Result<LookupSubjects, EngineError>;
    pub fn write_relationships(
        &self,
        mutations: impl IntoIterator<Item = RelationshipMutation>,
    ) -> Result<ConsistencyToken, EngineError>;
    pub fn apply_schema(&self, source: SchemaSource<'_>) -> Result<ConsistencyToken, EngineError>;
}
```

Request builders are provided for multi-field requests. Per AGENTS.md, `typed-builder` is used only for structs with more than five fields.

## 3. Compatibility Facade

The early M0 compatibility facade is retired by
[20-concurrent-engine-runtime-design.md](./20-concurrent-engine-runtime-design.md). Public examples,
tests, and benchmarks use `ZanzibarEngine` directly. Legacy model structs that remain public for
request DTOs are converted into validated domain types at the engine boundary.

## 4. Error Model

```rust
pub enum EngineError {
    Domain(DomainError),
    Schema(SchemaError),
    Store(StoreError),
    Consistency(ConsistencyError),
    Evaluation(EvaluationError),
}
```

Every variant uses `#[source]` where it wraps a lower-level error. Public API methods return `Result<T, EngineError>`.

## 5. Example Contract

Every public API gets a doc-tested example:

- compile/apply schema
- write relationships with touch
- write relationships with precondition
- latest check
- exact snapshot check
- expand
- lookup resources
- lookup subjects

## 6. AGENTS Binding

- Error Handling: typed errors, no `anyhow` in library surface.
- Async & Concurrency: public API is synchronous in v2; optional async wrapper may be added later without changing core.
- Type Design & API: explicit return types, builders for request structs with more than five fields.
- Safety & Security: string-based convenience constructors validate before engine access.
- Serialization: optional serde feature for request/response DTOs, camelCase, deny unknown fields.
- Testing: examples are doctests; public behavior tests target `ZanzibarEngine` directly.
- Logging & Observability: public API starts tracing spans when `tracing` feature is enabled.
- Performance: string convenience APIs are not used inside hot evaluator loops.
- Documentation: public API docs are complete before release.

## 7. Cross-References

- <- Depends on: [14-evaluation-engine-design.md](./14-evaluation-engine-design.md)
- -> Consumed by: [60-crates-features-design.md](./60-crates-features-design.md), [72-testing-verification-plan.md](./72-testing-verification-plan.md)
- Related research: [../docs/research/study-spicedb.md § Request Paths](../docs/research/study-spicedb.md#request-paths)
- Superseded runtime details: [20-concurrent-engine-runtime-design.md](./20-concurrent-engine-runtime-design.md)
