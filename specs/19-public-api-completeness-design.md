# 19 - Public API Completeness Design

Status: draft v1
Owner: Simple Zanzibar
Last updated: 2026-05-23
Depends on: [15](./15-public-api-design.md), [17](./17-compact-snapshot-format-design.md), [18](./18-trusted-fast-snapshot-load-design.md)

## 1. Purpose

The engine can already load uncompressed compact snapshots, mutate relationships, and answer core
Zanzibar queries. The remaining product gap is the boundary around those capabilities: callers need
direct zstd snapshot support, explicit policy import/export APIs for reviewable text artifacts,
schema replacement/deletion APIs, and audit-style query helpers that avoid forcing every user to
hand-roll schema enumeration.

This spec completes the crate-facing API without changing the internal `.szsnap` v2 payload layout.
Compression is an outer transport wrapper; canonical policy text is a review artifact; permission
enumeration is implemented by composing schema relation enumeration with the existing evaluator.

## 2. Goals

| # | Goal | Measure |
| --- | --- | --- |
| G1 | Load and save both raw `.szsnap` and zstd-compressed snapshot files. | Public API supports `SnapshotCompression::{None, Zstd}` for save and load; zstd round trips pass equivalence tests. |
| G2 | Import policy from raw text and export policy to reviewable text. | `PolicyText` can rebuild an equivalent service; exported files are deterministic, sorted, and grouped by resource namespace. |
| G3 | Support add, replace, and delete policy operations. | Public schema APIs can merge DSL, replace DSL, delete a namespace, and delete one relation while revalidating existing relationships. |
| G4 | Add audit-style Zanzibar helpers. | Public APIs answer "what permissions does this subject have on this object" and "who has which permissions on this object" without callers enumerating schema relations manually. |
| G5 | Preserve hot-path performance. | Existing `check`, lookup, full snapshot load, and trusted fast-load benchmark gates remain valid; zstd overhead is measured separately. |
| G6 | Keep safety and review ergonomics. | No `unsafe`, no production `unwrap`/`expect`, bounded decompression, typed errors, deterministic output ordering. |

## 3. Non-Goals

- No change to the `.szsnap` v2 internal section layout.
- No mmap or zero-copy compressed loading.
- No cryptographic signing implementation. External integrity remains a caller-supplied boundary.
- No unbounded global "list every subject and every permission in the datastore" API.
- No semantic distinction between "relation" and "permission" in the first enumeration API; every schema relation can be checked.
- No incremental rsync-like delta protocol. The rsync metaphor applies to API shape: export/import should make review and distribution cheap by canonicalizing data, while the binary snapshot remains the fastest whole-state artifact.

## 4. API Additions

### 4.1 Snapshot Compression

```rust
pub enum SnapshotCompression {
    None,
    Zstd,
}

pub struct SnapshotSaveOptions {
    pub compression: SnapshotCompression,
    pub zstd_level: i32,
    pub include_indexes: bool,
}

pub struct SnapshotLoadOptions {
    pub compression: SnapshotCompression,
    pub profile: SnapshotLoadProfile,
    pub validation: SnapshotValidationMode,
    pub integrity: SnapshotIntegrityMode,
    pub max_file_bytes: NonZeroU64,
}
```

Rules:

- `SnapshotCompression::None` writes and reads the raw `.szsnap` v2 bytes.
- `SnapshotCompression::Zstd` writes and reads a single zstd frame whose decompressed payload is the
  same raw `.szsnap` v2 bytes.
- Save validates `zstd_level` against the zstd crate's accepted compression-level range.
- Load applies `max_file_bytes` to both the compressed file size and decompressed output size.
- `SnapshotIntegrityMode::Checksum` verifies the inner `.szsnap` footer after decompression.
- `SnapshotIntegrityMode::External` continues to mean the caller has already verified artifact byte
  identity. For compressed snapshots, the external proof is assumed to bind the compressed bytes.

```text
raw .szsnap load
  file bytes ───────────────▶ .szsnap v2 parser ─▶ PublishedSnapshot

zstd .szsnap.zst load
  compressed bytes ─▶ bounded zstd decode ─▶ .szsnap v2 parser ─▶ PublishedSnapshot
```

Compression is intentionally outside the trusted fast-load budget. The recommended deployment path
for the fastest cold start remains: verify/download zstd artifact, decompress once into a
content-addressed local cache, then load cached raw `.szsnap` with `TrustedFastLoad + External`.

### 4.2 Policy Text Import/Export

```rust
pub struct PolicyText {
    pub schema: String,
    pub relationship_files: Vec<PolicyTextFile>,
}

pub struct PolicyTextFile {
    pub path: String,
    pub contents: String,
}

impl ZanzibarEngine {
    pub fn from_policy_text(policy: &PolicyText) -> Result<Self, EngineError>;
    pub fn apply_policy_text(&self, policy: &PolicyText) -> Result<ConsistencyToken, EngineError>;
    pub fn export_policy_text(&self) -> Result<PolicyText, EngineError>;
    pub fn export_policy_files(&self, directory: impl AsRef<Path>) -> Result<(), PolicyIoError>;
    pub fn save_snapshot_from_policy_text(
        path: impl AsRef<Path>,
        policy: &PolicyText,
        options: SnapshotSaveOptions,
    ) -> Result<(), PolicyIoError>;
}
```

Export rules:

- `schema.zed` contains canonical DSL sorted by namespace name, then relation name.
- Relationships are grouped by resource object type under `relationships/<type>.zedtuples`.
- Relationship lines are sorted by their canonical string form.
- Exported relationship files end with a trailing newline when non-empty.
- Empty relationship groups are omitted.
- Relationship import accepts blank lines plus full-line `#` and `//` comments.

The output is optimized for review and diff stability, not load speed. Snapshot artifacts remain the
fast runtime loading format.

### 4.3 Policy Mutation APIs

Existing APIs:

- `add_dsl` / `add_dsl_with_token`: merge or overwrite namespaces from DSL.
- `add_config` / `add_config_with_token`: merge or overwrite one namespace config.

New APIs:

```rust
impl ZanzibarEngine {
    pub fn replace_schema(&self, source: SchemaSource<'_>) -> Result<ConsistencyToken, EngineError>;
    pub fn delete_namespace(&self, namespace: &str) -> Result<ConsistencyToken, EngineError>;
    pub fn delete_relation(&self, namespace: &str, relation: &str) -> Result<ConsistencyToken, EngineError>;
}
```

Deletion semantics:

- Deleting a missing namespace returns `NamespaceNotFound`.
- Deleting a missing relation returns `RelationNotFound`.
- After any policy change, all existing relationships are revalidated against the candidate schema.
- If existing relationships reference a deleted namespace/relation, the operation fails and the
  current engine state is unchanged. Callers can delete relationships first, then delete policy.

This keeps illegal states unrepresentable after a successful schema mutation.

### 4.4 Permission Enumeration APIs

```rust
pub struct LookupPermissionsRequest {
    pub subject: User,
    pub resource: Object,
    pub consistency: Consistency,
}

pub struct LookupPermissions {
    pub permissions: Vec<Relation>,
}

pub struct LookupObjectPermissionsRequest {
    pub resource: Object,
    pub subject_type: String,
    pub consistency: Consistency,
}

pub struct LookupObjectPermissions {
    pub permissions: Vec<PermissionSubjects>,
}

pub struct PermissionSubjects {
    pub permission: Relation,
    pub subjects: Vec<User>,
}
```

Service and engine APIs:

```rust
pub fn lookup_permissions(request: LookupPermissionsRequest) -> Result<LookupPermissions, _>;
pub fn lookup_object_permissions(
    request: LookupObjectPermissionsRequest,
) -> Result<LookupObjectPermissions, _>;
```

Semantics:

- `lookup_permissions` enumerates the resource namespace's schema relations in sorted order and
  runs the shared `check` evaluator for each relation.
- `lookup_object_permissions` enumerates sorted schema relations and runs `lookup_subjects` for the
  requested subject type. Relations with no subjects are omitted.
- Subject type remains explicit to keep the API bounded and aligned with Zanzibar lookup semantics.
- The APIs inherit evaluator depth, fanout, and lookup-result limits.

## 5. Error Model

`ZanzibarError`, `EngineError`, and `SnapshotIoError` remain the primary existing public errors.
Policy file IO and snapshot-from-policy helpers use:

```rust
pub enum PolicyIoError {
    Io { source: std::io::Error },
    Zanzibar { source: ZanzibarError },
    Snapshot { source: SnapshotIoError },
    InvalidExportPath { reason: &'static str },
}
```

No helper hides parse, schema, store, consistency, or snapshot validation failures behind strings.

## 6. Performance Budgets

New benchmark filters:

| Benchmark | Dataset | Gate |
| --- | --- | ---: |
| `public_api/check/100k` | 100k org rules | <= 10 us |
| `public_api/lookup_resources/100k` | 100k org rules | <= 10 ms |
| `public_api/lookup_subjects/100k` | 100k org rules | <= 10 ms |
| `public_api/lookup_permissions/100k` | 100k org rules | <= 250 us |
| `public_api/lookup_object_permissions/100k` | 100k org rules | <= 25 ms |
| `public_api/export_policy_text/100k` | 100k org rules | recorded baseline |
| `public_api/snapshot_save_zstd/100k` | 100k org rules | recorded baseline |
| `public_api/snapshot_load_zstd/100k` | 100k org rules | recorded baseline |

The zstd benchmarks are not hard gates for cold-start load speed because the fast path should load a
cached raw snapshot after decompression. They are still reported in PR comments for distribution-size
and startup tradeoff visibility.

## 7. Testing Requirements

- zstd snapshot save/load equivalence through `ZanzibarEngine`.
- zstd decompression size cap rejects oversized decompressed output.
- `PolicyText` export/import round trip preserves check, expand, lookup, and permission enumeration.
- `export_policy_files` creates deterministic `schema.zed` plus grouped sorted relationship files.
- `replace_dsl`, `delete_namespace`, and `delete_relation` publish new revisions on success and leave
  state unchanged on invalid deletion.
- `lookup_permissions` and `lookup_object_permissions` cover direct, computed, tuple-to-userset, and
  exclusion cases.
- Existing snapshot corrupt-file validation and trusted fast-load tests continue to pass.

## 8. AGENTS.md Binding

- Error Handling: `thiserror` enums; no `anyhow` in public library APIs.
- Async & Concurrency: APIs stay synchronous; engine methods acquire existing write/read locks.
- Type Design & API: policy export structs are owned DTOs with optional serde support; query helpers
  use request/response types.
- Safety & Security: bounded decompression; reject path traversal for export file names by deriving
  filenames from validated resource object types only.
- Serialization: serde-compatible public DTOs use camelCase and `deny_unknown_fields` where structs
  are deserialized.
- Testing: integration tests cover every new public method and error path.
- Logging & Observability: no relationship payload logging in production code.
- Performance: no new string parsing in evaluator hot loops; permission enumeration composes existing
  `check` and lookup APIs.
- Dependencies: zstd uses `zstd = 0.13.3` after crate-version survey; audit and deny must pass.
- Documentation: public items get doc comments and examples where concise.

## 9. Cross-References

- Extends public API: [15](./15-public-api-design.md)
- Uses compact snapshot payload: [17](./17-compact-snapshot-format-design.md)
- Preserves trusted fast-load boundary: [18](./18-trusted-fast-snapshot-load-design.md)
- Performance gates: [71](./71-performance-budgets-design.md)
- Verification gates: [72](./72-testing-verification-plan.md)
