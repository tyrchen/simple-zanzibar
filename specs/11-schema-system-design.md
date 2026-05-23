# 11 - Schema System Design

Status: draft v1
Owner: Simple Zanzibar
Depends on: [10-local-engine-data-model-design.md](./10-local-engine-data-model-design.md)

## 1. Purpose

The schema system owns parsing, compiling, validating, hashing, and publishing schema definitions. It moves invariants out of `check` and into a schema application boundary. SpiceDB validates computed usersets, tuple-to-userset relations, wildcard constraints, and relation type references before serving requests in `vendors/spicedb/pkg/schema/typesystem_validation.go:37-288`; Simple Zanzibar adopts the same timing with a smaller feature set.

## 2. Pipeline

```text
Client schema text
       |
       v
+------------------+
| Parser           |
| DSL -> AST       |
+--------+---------+
         |
         v
+------------------+
| Compiler         |
| AST -> Schema IR |
+--------+---------+
         |
         v
+------------------+        invalid        +----------------------+
| Type Validator   | --------------------> | SchemaError          |
| resolver checks  |                       | source span included |
+--------+---------+                       +----------------------+
         |
         v
+------------------+
| Schema Snapshot  |
| hash + resolver  |
+--------+---------+
         |
         v
Published by revision layer
```

## 3. Public Interface

```rust
pub struct SchemaSource<'a> {
    pub name: Option<&'a str>,
    pub text: &'a str,
}

pub struct CompiledSchema {
    definitions: Arc<[NamespaceDefinition]>,
    resolver: SchemaResolver,
    hash: SchemaHash,
}

pub struct SchemaResolver { /* private */ }

impl SchemaResolver {
    pub fn namespace(&self, object_type: &ObjectType) -> Result<&NamespaceDefinition, SchemaError>;
    pub fn relation(
        &self,
        object_type: &ObjectType,
        relation: &RelationName,
    ) -> Result<&RelationDefinition, SchemaError>;
}
```

`CompiledSchema` is immutable and shareable. The revision layer publishes it with relationship snapshots.

## 4. DSL Scope

M0 parser scope:

```text
definition <object_type> {
  relation <name>: <allowed_subjects>
  permission <name> = <expression>
}
```

Expression scope:

- `this`
- relation reference on same object, represented as computed userset
- tuple-to-userset arrow syntax or current DSL equivalent
- `+` union
- `&` intersection
- `-` exclusion
- parentheses

The exact surface syntax may preserve the current pest grammar during M0. A parser migration to `winnow` is a Phase 0 risk-retirement task because AGENTS.md prefers `winnow` and the 2026-05-23 crate survey found `winnow = 1.0.3`.

## 5. Validation Invariants

The validator rejects:

- duplicate namespace definitions
- duplicate relation or permission names in a namespace
- computed userset references to missing relations
- tuple-to-userset left relation references to missing relations
- tuple-to-userset target relation references that cannot exist on allowed subject object types
- empty union/intersection/exclusion operands
- relation cycles that exceed the explicit recursion policy in [14-evaluation-engine-design.md](./14-evaluation-engine-design.md)
- relationship writes for object/relation/subject combinations not allowed by schema

Allowed direct subject types are required for relations that accept stored relationships. This gives the store enough type information to validate writes without asking the evaluator.

## 6. Schema Hash

`SchemaHash` is a stable 32-byte digest of the canonical compiled schema. It is included in every consistency token per [13-revision-consistency-design.md](./13-revision-consistency-design.md). SpiceDB includes schema hash in revision tokens in `vendors/spicedb/pkg/zedtoken/zedtoken.go:85-111`.

Canonicalization rules:

- sorted namespace definitions
- sorted relation definitions
- sorted allowed subject types
- deterministic expression serialization
- no source-location fields in the hash

## 7. Behaviour

- Parser errors include line/column and a short reason.
- Validation errors include the namespace/relation path and source span when available.
- Applying a new schema is atomic with respect to revision publication.
- Removing a namespace or relation is rejected while matching relationships exist, following the design lesson from `vendors/spicedb/internal/services/shared/schema.go:213-223`.
- Existing compatibility `NamespaceConfig` values can be converted through `TryFrom` and then validated. The conversion is not the authoritative model.

## 8. AGENTS Binding

- Error Handling: `SchemaError` is a `thiserror` enum; parser errors use `#[source]`.
- Async & Concurrency: schema compilation is synchronous and CPU-bound; no Tokio runtime is required for core library use.
- Type Design & API: immutable compiled schema, private fields, explicit resolver methods, no raw `String` public fields.
- Safety & Security: source length cap is enforced before parsing; recursion depth during parse/validation is bounded.
- Serialization: schema export under optional `serde` feature uses canonical order.
- Testing: same-file unit tests for validator rules, integration tests for apply/remove behaviour, property tests for canonical hash stability.
- Logging & Observability: validation spans include schema source name, not full schema text by default.
- Performance: resolver lookup is `O(1)` average through precomputed maps.
- Documentation: every public schema type documents invariants and error cases.

## 9. Cross-References

- <- Depends on: [10-local-engine-data-model-design.md](./10-local-engine-data-model-design.md)
- -> Consumed by: [12-relationship-store-design.md](./12-relationship-store-design.md), [13-revision-consistency-design.md](./13-revision-consistency-design.md), [14-evaluation-engine-design.md](./14-evaluation-engine-design.md)
- Related research: `vendors/spicedb/pkg/schemadsl/compiler/compiler.go:126-190`, `vendors/spicedb/pkg/schema/typesystem.go:20-36`, `vendors/spicedb/pkg/schema/typesystem_validation.go:37-288`
