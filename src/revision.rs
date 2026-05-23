//! Revision, consistency token, and published snapshot types.

use std::collections::HashMap;
use std::fmt;
use std::num::{NonZeroU64, NonZeroUsize};
use std::str::FromStr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use thiserror::Error;

use crate::model::NamespaceConfig;
use crate::relationship::IndexedRelationshipStore;
use crate::schema::{
    AllowedSubjectTypes, CompiledSchema, NamespaceDefinition, RelationDefinition, UsersetExpression,
};

const TOKEN_VERSION: &str = "sz1";
const MAX_CONSISTENCY_TOKEN_BYTES: usize = 122;
const DEFAULT_RETAINED_SNAPSHOTS: usize = 32;
static NEXT_DATASTORE_COUNTER: AtomicU64 = AtomicU64::new(1);

/// Errors produced by revision and consistency-token handling.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ConsistencyError {
    /// A token string does not match the supported grammar.
    #[error("invalid consistency token: {reason}")]
    InvalidToken {
        /// Static parse failure reason.
        reason: &'static str,
    },

    /// A token came from a different datastore instance.
    #[error("consistency token datastore id does not match this engine")]
    WrongDatastore,

    /// A token references a revision older than retained history.
    #[error("revision {revision} is no longer retained")]
    RevisionExpired {
        /// Expired revision.
        revision: Revision,
    },

    /// A token references a revision that has not been published by this engine.
    #[error("revision {revision} is not available")]
    RevisionUnavailable {
        /// Unavailable revision.
        revision: Revision,
    },

    /// A token revision exists but the schema hash does not match the retained snapshot.
    #[error("consistency token schema hash does not match retained revision {revision}")]
    SchemaHashMismatch {
        /// Revision with mismatched schema hash.
        revision: Revision,
    },

    /// The engine cannot publish another revision without overflowing.
    #[error("revision counter overflowed")]
    RevisionOverflow,
}

/// Monotonic non-zero revision identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Revision(NonZeroU64);

impl Revision {
    /// Returns the first publishable revision.
    #[must_use]
    pub const fn first() -> Self {
        Self(NonZeroU64::MIN)
    }

    /// Creates a revision from a non-zero value.
    #[must_use]
    pub const fn new(value: NonZeroU64) -> Self {
        Self(value)
    }

    /// Returns the revision as a primitive integer.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0.get()
    }

    /// Returns the next revision.
    ///
    /// # Errors
    ///
    /// Returns [`ConsistencyError::RevisionOverflow`] when incrementing would overflow `u64`.
    pub fn next(self) -> Result<Self, ConsistencyError> {
        let value = self
            .get()
            .checked_add(1)
            .ok_or(ConsistencyError::RevisionOverflow)?;
        NonZeroU64::new(value)
            .map(Self)
            .ok_or(ConsistencyError::RevisionOverflow)
    }
}

impl fmt::Display for Revision {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.get())
    }
}

impl FromStr for Revision {
    type Err = ConsistencyError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let parsed = value
            .parse::<u64>()
            .map_err(|_| ConsistencyError::InvalidToken {
                reason: "revision must be a non-zero unsigned integer",
            })?;
        NonZeroU64::new(parsed)
            .map(Self)
            .ok_or(ConsistencyError::InvalidToken {
                reason: "revision must be non-zero",
            })
    }
}

/// Per-engine datastore identifier used to reject foreign tokens.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DatastoreId([u8; 16]);

impl DatastoreId {
    /// Creates a best-effort unique datastore id for an in-memory engine instance.
    #[must_use]
    pub fn new_unique() -> Self {
        let counter = NEXT_DATASTORE_COUNTER.fetch_add(1, Ordering::Relaxed);
        let elapsed = match SystemTime::now().duration_since(UNIX_EPOCH) {
            Ok(duration) => duration.as_nanos(),
            Err(error) => error.duration().as_nanos(),
        };
        let mut hasher = blake3::Hasher::new();
        hasher.update(&counter.to_le_bytes());
        hasher.update(&elapsed.to_le_bytes());
        hasher.update(&u64::from(std::process::id()).to_le_bytes());
        let hash = hasher.finalize();
        let mut bytes = [0_u8; 16];
        for (target, source) in bytes.iter_mut().zip(hash.as_bytes().iter()) {
            *target = *source;
        }
        Self(bytes)
    }

    /// Creates an id from raw bytes.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 16]) -> Self {
        Self(bytes)
    }

    /// Returns raw id bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }
}

/// Canonical hash of a compiled schema.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SchemaHash([u8; 32]);

impl SchemaHash {
    /// Computes a canonical schema hash.
    #[must_use]
    pub fn for_schema(schema: &CompiledSchema) -> Self {
        let mut hasher = blake3::Hasher::new();
        let mut namespaces = schema.definitions().iter().collect::<Vec<_>>();
        namespaces.sort_by(|left, right| left.name().as_str().cmp(right.name().as_str()));

        for namespace in namespaces {
            update_namespace(&mut hasher, namespace);
        }

        Self(*hasher.finalize().as_bytes())
    }

    /// Creates a schema hash from raw bytes.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// Returns raw hash bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

/// Stable exact-snapshot token returned by writes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConsistencyToken {
    revision: Revision,
    schema_hash: SchemaHash,
    datastore_id: DatastoreId,
}

impl ConsistencyToken {
    /// Creates a token from validated parts.
    #[must_use]
    pub const fn new(
        revision: Revision,
        schema_hash: SchemaHash,
        datastore_id: DatastoreId,
    ) -> Self {
        Self {
            revision,
            schema_hash,
            datastore_id,
        }
    }

    /// Returns the token revision.
    #[must_use]
    pub const fn revision(&self) -> Revision {
        self.revision
    }

    /// Returns the token schema hash.
    #[must_use]
    pub const fn schema_hash(&self) -> SchemaHash {
        self.schema_hash
    }

    /// Returns the token datastore id.
    #[must_use]
    pub const fn datastore_id(&self) -> DatastoreId {
        self.datastore_id
    }
}

impl fmt::Display for ConsistencyToken {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{TOKEN_VERSION}:{}:", self.revision)?;
        write_hex(formatter, self.schema_hash.as_bytes())?;
        formatter.write_str(":")?;
        write_hex(formatter, self.datastore_id.as_bytes())
    }
}

impl FromStr for ConsistencyToken {
    type Err = ConsistencyError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value.len() > MAX_CONSISTENCY_TOKEN_BYTES {
            return Err(ConsistencyError::InvalidToken {
                reason: "token exceeds maximum byte length",
            });
        }

        let mut parts = value.split(':');
        let version = parts.next().ok_or(ConsistencyError::InvalidToken {
            reason: "missing token version",
        })?;
        if version != TOKEN_VERSION {
            return Err(ConsistencyError::InvalidToken {
                reason: "unsupported token version",
            });
        }
        let revision = parts
            .next()
            .ok_or(ConsistencyError::InvalidToken {
                reason: "missing revision",
            })?
            .parse()?;
        let schema_hash = SchemaHash::from_bytes(decode_hex(parts.next().ok_or(
            ConsistencyError::InvalidToken {
                reason: "missing schema hash",
            },
        )?)?);
        let datastore_id = DatastoreId::from_bytes(decode_hex(parts.next().ok_or(
            ConsistencyError::InvalidToken {
                reason: "missing datastore id",
            },
        )?)?);
        if parts.next().is_some() {
            return Err(ConsistencyError::InvalidToken {
                reason: "too many token fields",
            });
        }
        Ok(Self::new(revision, schema_hash, datastore_id))
    }
}

/// Read consistency mode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Consistency {
    /// Read the latest published snapshot.
    Latest,
    /// Read exactly at a previously returned token.
    Exact(ConsistencyToken),
}

/// Published immutable snapshot.
#[derive(Debug, Clone)]
pub struct PublishedSnapshot {
    revision: Revision,
    schema_hash: SchemaHash,
    configs: Arc<HashMap<String, NamespaceConfig>>,
    schema: Arc<CompiledSchema>,
    relationships: Arc<IndexedRelationshipStore>,
}

impl PublishedSnapshot {
    /// Creates a published snapshot from immutable state.
    #[must_use]
    pub fn new(
        revision: Revision,
        schema_hash: SchemaHash,
        configs: Arc<HashMap<String, NamespaceConfig>>,
        schema: Arc<CompiledSchema>,
        relationships: Arc<IndexedRelationshipStore>,
    ) -> Self {
        Self {
            revision,
            schema_hash,
            configs,
            schema,
            relationships,
        }
    }

    /// Returns the snapshot revision.
    #[must_use]
    pub const fn revision(&self) -> Revision {
        self.revision
    }

    /// Returns the snapshot schema hash.
    #[must_use]
    pub const fn schema_hash(&self) -> SchemaHash {
        self.schema_hash
    }

    /// Returns legacy namespace configs retained for compatibility evaluation.
    #[must_use]
    pub fn configs(&self) -> &HashMap<String, NamespaceConfig> {
        &self.configs
    }

    /// Returns the compiled schema.
    #[must_use]
    pub fn schema(&self) -> &CompiledSchema {
        &self.schema
    }

    /// Returns the indexed relationships.
    #[must_use]
    pub fn relationships(&self) -> &IndexedRelationshipStore {
        &self.relationships
    }
}

/// Returns the default snapshot retention.
#[must_use]
pub fn default_retained_snapshots() -> NonZeroUsize {
    match NonZeroUsize::new(DEFAULT_RETAINED_SNAPSHOTS) {
        Some(value) => value,
        None => NonZeroUsize::MIN,
    }
}

fn update_namespace(hasher: &mut blake3::Hasher, namespace: &NamespaceDefinition) {
    update_str(hasher, "namespace");
    update_str(hasher, namespace.name().as_str());
    let mut relations = namespace.relations().iter().collect::<Vec<_>>();
    relations.sort_by(|left, right| left.name().as_str().cmp(right.name().as_str()));
    for relation in relations {
        update_relation(hasher, relation);
    }
}

fn update_relation(hasher: &mut blake3::Hasher, relation: &RelationDefinition) {
    update_str(hasher, "relation");
    update_str(hasher, relation.name().as_str());
    match relation.allowed_subject_types() {
        AllowedSubjectTypes::Unspecified => update_str(hasher, "subjects_unspecified"),
        AllowedSubjectTypes::Explicit(subjects) => {
            update_str(hasher, "subjects_explicit");
            let mut subject_types = subjects.iter().collect::<Vec<_>>();
            subject_types.sort_by(|left, right| left.as_str().cmp(right.as_str()));
            for subject_type in subject_types {
                update_str(hasher, subject_type.as_str());
            }
        }
    }
    match relation.userset_rewrite() {
        Some(expression) => update_expression(hasher, expression),
        None => update_str(hasher, "rewrite_none"),
    }
}

fn update_expression(hasher: &mut blake3::Hasher, expression: &UsersetExpression) {
    match expression {
        UsersetExpression::This => update_str(hasher, "this"),
        UsersetExpression::ComputedUserset { relation } => {
            update_str(hasher, "computed_userset");
            update_str(hasher, relation.as_str());
        }
        UsersetExpression::TupleToUserset {
            tupleset_relation,
            computed_userset_relation,
        } => {
            update_str(hasher, "tuple_to_userset");
            update_str(hasher, tupleset_relation.as_str());
            update_str(hasher, computed_userset_relation.as_str());
        }
        UsersetExpression::Union(expressions) => {
            update_str(hasher, "union");
            for child in expressions {
                update_expression(hasher, child);
            }
        }
        UsersetExpression::Intersection(expressions) => {
            update_str(hasher, "intersection");
            for child in expressions {
                update_expression(hasher, child);
            }
        }
        UsersetExpression::Exclusion { base, exclude } => {
            update_str(hasher, "exclusion");
            update_expression(hasher, base);
            update_expression(hasher, exclude);
        }
    }
}

fn update_str(hasher: &mut blake3::Hasher, value: &str) {
    hasher.update(&value.len().to_le_bytes());
    hasher.update(value.as_bytes());
}

fn write_hex(formatter: &mut fmt::Formatter<'_>, bytes: &[u8]) -> fmt::Result {
    for byte in bytes {
        write!(formatter, "{byte:02x}")?;
    }
    Ok(())
}

fn decode_hex<const N: usize>(value: &str) -> Result<[u8; N], ConsistencyError> {
    if value.len() != N.saturating_mul(2) {
        return Err(ConsistencyError::InvalidToken {
            reason: "hex field length is invalid",
        });
    }

    let mut decoded = [0_u8; N];
    for (target, pair) in decoded.iter_mut().zip(value.as_bytes().chunks_exact(2)) {
        let high = pair
            .first()
            .copied()
            .ok_or(ConsistencyError::InvalidToken {
                reason: "hex field length is invalid",
            })?;
        let low = pair.get(1).copied().ok_or(ConsistencyError::InvalidToken {
            reason: "hex field length is invalid",
        })?;
        *target = decode_nibble(high)?
            .checked_mul(16)
            .and_then(|left| left.checked_add(decode_nibble(low).ok()?))
            .ok_or(ConsistencyError::InvalidToken {
                reason: "hex field contains invalid digit",
            })?;
    }
    Ok(decoded)
}

fn decode_nibble(value: u8) -> Result<u8, ConsistencyError> {
    match value {
        b'0'..=b'9' => Ok(value - b'0'),
        b'a'..=b'f' => Ok(value - b'a' + 10),
        b'A'..=b'F' => Ok(value - b'A' + 10),
        _ => Err(ConsistencyError::InvalidToken {
            reason: "hex field contains invalid digit",
        }),
    }
}
