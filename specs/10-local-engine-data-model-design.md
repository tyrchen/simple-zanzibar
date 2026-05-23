# 10 - Data Model Design

Status: draft v1
Owner: Simple Zanzibar
Depends on: [00-local-engine-prd.md](./00-local-engine-prd.md)

## 1. Purpose

This spec defines the validated domain types every downstream subsystem uses. It replaces raw `String`-heavy model structs with small newtypes and structured relationship shapes. The goal is to reject malformed external input once, at the boundary, and make valid domain values cheap to clone, hash, and compare.

## 2. Architecture

```text
+---------------------+        +-----------------------+        +----------------------+
| External Input      |        | Domain Constructors   |        | Engine Internals     |
| - DSL text          | -----> | - FromStr/TryFrom     | -----> | - validated IDs      |
| - tuple strings     |        | - length/charset caps |        | - relationship keys  |
| - request builders  |        | - typed errors        |        | - schema references  |
+----------+----------+        +-----------+-----------+        +----------+-----------+
           |                               |                               |
           | malformed                     | valid                         |
           v                               v                               v
    DomainError::InvalidInput       newtype values                  no revalidation
```

## 3. Core Types

Code-shaped contract:

```rust
pub struct ObjectType(SmolStr);
pub struct ObjectId(SmolStr);
pub struct RelationName(SmolStr);
pub struct SubjectType(SmolStr);
pub struct SubjectId(SmolStr);

pub struct ObjectRef {
    pub object_type: ObjectType,
    pub object_id: ObjectId,
}

pub enum SubjectRef {
    Object(ObjectRef),
    Userset {
        object: ObjectRef,
        relation: RelationName,
    },
}

pub struct Relationship {
    pub resource: ObjectRef,
    pub relation: RelationName,
    pub subject: SubjectRef,
}
```

`SmolStr` is a candidate compact string representation; [60-crates-features-design.md](./60-crates-features-design.md) decides dependency adoption. If no compact string crate is adopted in Phase 1, use private `String` fields with the same public API.

## 4. Validation Rules

All externally supplied names are validated in bytes:

| Type | Max bytes | Allowed charset | Notes |
| --- | ---: | --- | --- |
| `ObjectType` | 64 | `[A-Za-z][A-Za-z0-9_]{0,63}` | Namespace/type name. |
| `RelationName` | 64 | `[A-Za-z][A-Za-z0-9_]{0,63}` | Relation or permission name. |
| `SubjectType` | 64 | Same as `ObjectType` | Same grammar, separate type for API clarity. |
| `ObjectId` | 256 | Visible ASCII excluding `#`, `@`, `:`, control chars | Reject path separators and whitespace. |
| `SubjectId` | 256 | Same as `ObjectId` | Wildcards are a future feature and rejected now. |
| Relationship string | 768 | Derived from component limits | Enough for all component caps plus separators. |

The parser rejects invalid input rather than sanitizing it. This binds AGENTS.md § Input Validation and § Injection Prevention.

## 5. Relationship Grammar

Relationship strings accepted by `FromStr`:

```text
resource-object  = object-type ":" object-id
resource-relation = resource-object "#" relation-name
subject-object   = subject-type ":" subject-id
subject-userset  = subject-object "#" relation-name
relationship     = resource-relation "@" (subject-object | subject-userset)
```

Examples:

```text
doc:readme#viewer@user:alice
doc:readme#viewer@group:eng#member
folder:root#owner@user:alice
```

## 6. Schema Expression Model

The current `UsersetExpression` shape remains conceptually correct, but v2 uses validated relation names and reserves a typed target model:

```rust
pub enum UsersetExpression {
    This,
    ComputedUserset { relation: RelationName },
    TupleToUserset {
        tupleset_relation: RelationName,
        computed_userset_relation: RelationName,
    },
    Union(Vec<UsersetExpression>),
    Intersection(Vec<UsersetExpression>),
    Exclusion {
        base: Box<UsersetExpression>,
        exclude: Box<UsersetExpression>,
    },
}
```

Empty `Union` and empty `Intersection` are rejected during schema validation. They are not represented with `Option<Vec<_>>`; empty collections remain empty collections per AGENTS.md § Type Design & API.

## 7. Errors

`DomainError` is a `thiserror` enum:

```rust
#[derive(Debug, thiserror::Error)]
pub enum DomainError {
    #[error("identifier {kind} exceeds {max_bytes} bytes")]
    IdentifierTooLong { kind: IdentifierKind, max_bytes: usize },
    #[error("identifier {kind} contains invalid byte at offset {offset}")]
    InvalidIdentifierByte { kind: IdentifierKind, offset: usize },
    #[error("relationship is malformed: {reason}")]
    MalformedRelationship { reason: &'static str },
}
```

No production constructor may use `unwrap()` or `expect()`.

## 8. AGENTS Binding

- Error Handling: `thiserror` enums with precise variants and `#[source]` where wrapping parser or hashing errors.
- Async & Concurrency: N/A for domain types; they are immutable values.
- Type Design & API: newtype domain primitives, private fields, `FromStr`/`TryFrom`, `Debug`, `Clone`, `Eq`, `Hash`.
- Safety & Security: reject untrusted strings at boundary; length and charset caps are mandatory.
- Serialization: optional `serde` feature uses `camelCase`, `deny_unknown_fields`, and validation during deserialization.
- Testing: unit tests for every accepted/rejected grammar path plus property tests for round-trip relationship parsing.
- Logging & Observability: domain values may appear in traces only after control-character rejection.
- Performance: constructors avoid regex backtracking; use byte loops or a linear-time parser.
- Documentation: every public type has a doc comment with a valid example.

## 9. Cross-References

- <- Depends on: [00-local-engine-prd.md](./00-local-engine-prd.md)
- -> Consumed by: [11-schema-system-design.md](./11-schema-system-design.md), [12-relationship-store-design.md](./12-relationship-store-design.md), [14-evaluation-engine-design.md](./14-evaluation-engine-design.md), [15-public-api-design.md](./15-public-api-design.md)
- Related research: [../docs/research/study-spicedb.md § What Simple Zanzibar Should Adopt](../docs/research/study-spicedb.md#what-simple-zanzibar-should-adopt)
