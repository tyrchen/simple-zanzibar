# Simple Zanzibar

[![Crates.io](https://img.shields.io/crates/v/simple-zanzibar.svg)](https://crates.io/crates/simple-zanzibar)
[![Documentation](https://docs.rs/simple-zanzibar/badge.svg)](https://docs.rs/simple-zanzibar)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)
[![Build Status](https://github.com/tyrchen/simple-zanzibar/workflows/CI/badge.svg)](https://github.com/tyrchen/simple-zanzibar/actions)

Simple Zanzibar is a local, in-memory Rust authorization engine inspired by Google's Zanzibar paper.
It provides relationship-based access control (ReBAC), a small policy DSL, consistency tokens,
lock-free snapshot reads, compact snapshot artifacts, and public APIs for check, expand, lookup, and
policy review workflows.

The crate is intentionally a local library, not a distributed Zanzibar service. It is useful when an
application wants Zanzibar-style semantics in-process and can distribute policy/relationship data as
text or prebuilt `.szsnap` artifacts.

## Current Capabilities

- Schema-first DSL with direct relations, computed usersets, tuple-to-userset inheritance, union,
  intersection, and exclusion.
- Validated relationship strings such as `doc:readme#viewer@group:eng#member`.
- Single-writer actor with bounded queue; readers use immutable published snapshots through
  `arc-swap` and do not take a service-level lock.
- Consistency tokens for exact-snapshot reads across retained revisions.
- Indexed compact relationship storage for resource-side and subject-side lookup paths.
- `check`, `expand`, `lookup_resources`, `lookup_subjects`, `lookup_permissions`, and
  `lookup_object_permissions` APIs.
- Deterministic policy text import/export and raw or zstd-compressed snapshot save/load.
- Optional `serde` feature with validated public request/response DTO deserialization.
- Optional `tracing` feature for structured API spans.

## Architecture

```text
Client / application
        │
        ▼
┌───────────────────────────────┐
│ ZanzibarEngine public API      │
│ - validates request DTOs       │
│ - starts tracing spans         │
└───────────────┬───────────────┘
                │
    ┌───────────┴───────────┐
    │                       │
    ▼                       ▼
Read path               Write path
ArcSwap snapshot        bounded writer actor
check/expand/lookup     schema + relationship mutation
    │                       │
    ▼                       ▼
Compiled schema IR      publish new revision token
Indexed store view      retain exact snapshots
    │                       │
    └───────────┬───────────┘
                ▼
        `.szsnap` save/load
        raw or zstd wrapper
```

## Quick Start

Add the crate to your `Cargo.toml`:

```toml
[dependencies]
simple-zanzibar = "0.2.1"
```

Basic permission check:

```rust
use simple_zanzibar::{
    ZanzibarEngine,
    model::{Object, Relation, User},
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let engine = ZanzibarEngine::builder().build();

    engine.add_dsl(r#"
        namespace doc {
            relation owner {}
            relation viewer {
                rewrite union(this, computed_userset(relation: "owner"))
            }
        }
    "#)?;

    engine.touch_relationship("doc:readme#owner@user:alice")?;

    let doc = Object::new("doc", "readme");
    let viewer = Relation::new("viewer");
    let alice = User::user_id("alice");

    assert!(engine.check_relation(&doc, &viewer, &alice)?);
    Ok(())
}
```

Exact consistency after a write:

```rust
use simple_zanzibar::{
    ZanzibarEngine,
    model::{CheckRequest, Object, Relation, User},
    revision::Consistency,
};

# fn main() -> Result<(), Box<dyn std::error::Error>> {
let engine = ZanzibarEngine::builder().build();
engine.add_dsl("namespace doc { relation viewer {} }")?;
let token = engine.touch_relationship("doc:readme#viewer@user:alice")?;
let response = engine.check(CheckRequest::new(
    Object::new("doc", "readme"),
    Relation::new("viewer"),
    User::user_id("alice"),
    Consistency::Exact(token),
))?;
assert!(response.allowed);
# Ok(())
# }
```

## DSL Reference

```text
namespace <namespace_name> {
    relation <relation_name> {
        rewrite <userset_expression>
    }
}
```

Supported userset expressions:

- `this`: direct relationships for the current object relation.
- `computed_userset(relation: "owner")`: another relation on the same object.
- `tuple_to_userset(tupleset: "parent", computed_userset: "viewer")`: follow related objects and
  evaluate a relation on each related object.
- `union(expr1, expr2, ...)`: any expression may allow access.
- `intersection(expr1, expr2, ...)`: all expressions must allow access.
- `exclusion(base, exclude)`: allow `base` except subjects in `exclude`.

Example:

```text
namespace group {
    relation member {}
}

namespace folder {
    relation viewer {}
}

namespace doc {
    relation owner {}
    relation parent {}
    relation banned {}
    relation viewer {
        rewrite exclusion(
            union(
                computed_userset(relation: "owner"),
                tuple_to_userset(tupleset: "parent", computed_userset: "viewer")
            ),
            computed_userset(relation: "banned")
        )
    }
}
```

## Public API Overview

```rust
use simple_zanzibar::{
    ZanzibarEngine,
    model::{
        CheckRequest, ExpandRequest, LookupObjectPermissionsRequest,
        LookupPermissionsRequest, LookupResourcesRequest, LookupSubjectsRequest,
    },
    relationship::{Precondition, RelationshipMutation},
    revision::ConsistencyToken,
    schema::SchemaSource,
};

impl ZanzibarEngine {
    pub fn builder() -> simple_zanzibar::ZanzibarEngineBuilder;
    pub fn apply_schema(&self, source: SchemaSource<'_>) -> Result<ConsistencyToken, simple_zanzibar::EngineError>;
    pub fn write_relationships(
        &self,
        mutations: impl IntoIterator<Item = RelationshipMutation>,
    ) -> Result<ConsistencyToken, simple_zanzibar::EngineError>;
    pub fn write_relationships_with_preconditions(
        &self,
        mutations: impl IntoIterator<Item = RelationshipMutation>,
        preconditions: impl IntoIterator<Item = Precondition>,
    ) -> Result<ConsistencyToken, simple_zanzibar::EngineError>;
    pub fn check(&self, request: CheckRequest) -> Result<simple_zanzibar::model::CheckResponse, simple_zanzibar::EngineError>;
    pub fn expand(&self, request: ExpandRequest) -> Result<simple_zanzibar::model::ExpandResponse, simple_zanzibar::EngineError>;
    pub fn lookup_resources(&self, request: impl std::borrow::Borrow<LookupResourcesRequest>) -> Result<simple_zanzibar::model::LookupResources, simple_zanzibar::EngineError>;
    pub fn lookup_subjects(&self, request: impl std::borrow::Borrow<LookupSubjectsRequest>) -> Result<simple_zanzibar::model::LookupSubjects, simple_zanzibar::EngineError>;
    pub fn lookup_permissions(&self, request: impl std::borrow::Borrow<LookupPermissionsRequest>) -> Result<simple_zanzibar::model::LookupPermissions, simple_zanzibar::EngineError>;
    pub fn lookup_object_permissions(&self, request: impl std::borrow::Borrow<LookupObjectPermissionsRequest>) -> Result<simple_zanzibar::model::LookupObjectPermissions, simple_zanzibar::EngineError>;
}
```

String convenience methods are available for ergonomic setup:

- `add_dsl` / `add_dsl_with_token`
- `create_relationship`
- `touch_relationship`
- `delete_relationship`
- `check_relation`
- `expand_relation`

## Policy Text and Snapshot Artifacts

Reviewable policy text is deterministic and grouped by resource type:

```rust
use simple_zanzibar::{PolicyText, ZanzibarEngine};

# fn main() -> Result<(), Box<dyn std::error::Error>> {
let policy = PolicyText::from_single_relationship_file(
    "namespace doc { relation viewer {} }".to_string(),
    "doc:readme#viewer@user:alice\n".to_string(),
);
let engine = ZanzibarEngine::from_policy_text(&policy)?;
let exported = engine.export_policy_text()?;
assert!(exported.schema.contains("namespace doc"));
# Ok(())
# }
```

Snapshots are the fastest whole-state distribution format:

```rust
use simple_zanzibar::{SnapshotLoadOptions, SnapshotSaveOptions, ZanzibarEngine};

# fn main() -> Result<(), Box<dyn std::error::Error>> {
# let path = std::env::temp_dir().join("simple-zanzibar-readme.szsnap.zst");
let engine = ZanzibarEngine::builder().build();
engine.add_dsl("namespace doc { relation viewer {} }")?;
engine.touch_relationship("doc:readme#viewer@user:alice")?;
engine.save_snapshot(&path, SnapshotSaveOptions::zstd())?;
let loaded = ZanzibarEngine::load_snapshot(&path, SnapshotLoadOptions::zstd())?;
# std::fs::remove_file(path).ok();
# let _ = loaded;
# Ok(())
# }
```

## Testing and Verification

Common checks:

```bash
cargo build
cargo test --all-features
cargo +nightly fmt --check
cargo clippy --all-targets --all-features -- -D warnings -W clippy::pedantic
cargo audit
cargo deny check
```

The Makefile keeps common automation discoverable:

```bash
make check                 # build + nextest + fmt-check + clippy
make bench-all             # full benchmark suite
make bench-perf-continuous # focused continuous performance filters
make perf-charts           # regenerate docs/perf SVGs from history.ndjson
```

The test suite includes unit, integration, property, snapshot-corruption, public API completeness,
concurrent-runtime, e2e policy-to-snapshot, and benchmark harness coverage. The current performance
report is in [`docs/perf/phase-15-complete-benchmark-2026-05-25.md`](docs/perf/phase-15-complete-benchmark-2026-05-25.md).

## Performance Notes

- Relationship data is stored in compact indexed snapshots, not a linear `HashSet` scan on hot
  paths.
- Reads acquire a published immutable snapshot through `arc-swap` and reuse request-local evaluator
  state where possible.
- Writes are serialized through one bounded actor per engine; batch writes are much faster than many
  single-relationship writes.
- Raw `.szsnap` files optimize load speed. Zstd-wrapped snapshots optimize distribution size and are
  decoded under a configured byte cap.
- `IndexProfile::CheckOnly` can reduce artifact size when subject-side reverse lookup APIs are not
  needed.

## Repository Map

- `src/api.rs`: public engine, writer actor, tenant sharding, public error model.
- `src/domain.rs`: validated identifiers and relationship grammar.
- `src/schema/`: schema compiler and resolver.
- `src/relationship.rs`: compact relationship store and snapshot index encoding.
- `src/eval.rs`: check, expand, lookup, memoization, and lookup planning.
- `src/snapshot.rs`: raw and zstd snapshot save/load with validation.
- `specs/`: product, design, performance, verification, and implementation specs.
- `docs/perf/`: recorded benchmark evidence and generated charts.

## Non-Goals

- No persistent database backend in the current crate.
- No network server or gRPC API.
- No distributed consistency protocol.
- No cryptographic signature implementation for snapshots; callers can use external integrity and
  then select `SnapshotIntegrityMode::External` where appropriate.

## License

This project is licensed under the MIT License. See [LICENSE.md](LICENSE.md).

## Acknowledgments

- [Google's Zanzibar paper](https://research.google/pubs/pub48190/) for the authorization model.
- [SpiceDB](https://github.com/authzed/spicedb) for production implementation patterns studied in
  [`docs/research/study-spicedb.md`](docs/research/study-spicedb.md).
