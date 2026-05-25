//! Indexed in-memory relationship store and mutation semantics.

#[cfg(feature = "bench-internals")]
use std::sync::atomic::{AtomicU64, Ordering};
use std::{
    collections::{
        BTreeMap, HashMap, HashSet,
        hash_map::{DefaultHasher, Entry},
    },
    hash::{Hash, Hasher},
    num::{NonZeroU32, NonZeroUsize},
    ops::Range,
    str,
    sync::Arc,
    time::Instant,
};

use thiserror::Error;

use crate::{
    domain::{
        DomainError, ObjectId, ObjectRef, ObjectType, RelationName, Relationship, SubjectId,
        SubjectRef, SubjectType,
    },
    error::ZanzibarError,
    model::{Object, Relation, User},
    snapshot::{
        BinaryCursor, IndexProfile, SectionKind, SnapshotEncodingLayout, SnapshotFormatVersion,
        SnapshotIoError, SnapshotLoadPhaseTimings, SnapshotLoadProfile, SnapshotReader,
        SnapshotSectionWriter, SnapshotValidationMode, checked_add_usize, checked_mul_usize,
        checked_u32_from_usize, checked_usize_from_u32, checked_usize_from_u64, insert_unique,
    },
};

const DEFAULT_QUERY_LIMIT: usize = 1_000;
const MAX_MUTATIONS_PER_BATCH: usize = 10_000;
const MAX_PRECONDITIONS_PER_BATCH: usize = 100;
const COMPACT_DEAD_ROWS: usize = 100_000;
const STORE_VIEW_MAX_DELTA_MUTATIONS: usize = 100_000;
const STORE_VIEW_MAX_DELTA_TOMBSTONES: usize = 100_000;
const DISK_SYMBOL_HASH_LEN: usize = 8;
const DISK_INDEX_DIRECTORY_LEN: usize = 20;
const DISK_INDEX_KEY_LEN: usize = 12;
const DISK_POSTING_RANGE_LEN: usize = 12;
const DISK_ROW_ID_LEN: usize = 4;
const SECTION_WIDTH_MASK: u16 = 0b11;
const SYMBOL_TABLE_LEN_WIDTH_SHIFT: u16 = 2;
const SYMBOL_TABLE_LEN_WIDTH_MASK: u16 = 0b1100;
const INDEX_DIRECTORY_FLAG_V3_COMPACT: u16 = 1;
const INDEX_DIRECTORY_KEY_WIDTH_SHIFT: u16 = 1;
const INDEX_DIRECTORY_KEY_WIDTH_MASK: u16 = 0b110;
const SNAPSHOT_INDEX_KIND_COUNT: usize = 7;
const SNAPSHOT_INDEX_KIND_COUNT_U64: u64 = SNAPSHOT_INDEX_KIND_COUNT as u64;
#[cfg(feature = "bench-internals")]
static DELTA_SEGMENTS_INSPECTED: AtomicU64 = AtomicU64::new(0);
#[cfg(feature = "bench-internals")]
static TOMBSTONE_CHECKS: AtomicU64 = AtomicU64::new(0);
#[cfg(feature = "bench-internals")]
static STORE_VIEW_QUERY_CALLS: AtomicU64 = AtomicU64::new(0);

/// Benchmark-only read counters for segmented relationship views.
#[cfg(feature = "bench-internals")]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct StoreViewReadCounters {
    /// Number of store-view query calls sampled by read benchmarks.
    pub query_calls: u64,
    /// Number of delta overlays inspected by read queries.
    pub delta_segments_inspected: u64,
    /// Number of checkpoint rows tested against the delta tombstone set.
    pub tombstone_checks: u64,
}

/// Benchmark-only delta shape for one published relationship-store view.
#[cfg(feature = "bench-internals")]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct StoreViewDeltaStats {
    /// Number of rows in the checkpoint store.
    pub checkpoint_rows: usize,
    /// Number of delta overlays retained by the view.
    pub delta_segments: usize,
    /// Number of inserted rows in the current delta overlay.
    pub delta_inserted_rows: usize,
    /// Number of checkpoint rows masked by delta tombstones.
    pub delta_deleted_rows: usize,
    /// Number of mutations represented by the current delta overlay.
    pub delta_mutations: usize,
    /// Deleted-row ratio in basis points over checkpoint plus inserted rows.
    pub tombstone_ratio_bps: u16,
}

/// Benchmark-only posting length histogram for one index group.
#[cfg(feature = "bench-internals")]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct StorePostingHistogram {
    /// Number of distinct index keys.
    pub keys: u64,
    /// Number of row ids across all postings.
    pub total_postings: u64,
    /// Longest posting list length.
    pub max_posting_len: u64,
    /// Number of singleton posting lists.
    pub singleton_keys: u64,
    /// Number of posting lists with 2 to 4 row ids.
    pub keys_2_to_4: u64,
    /// Number of posting lists with 5 to 16 row ids.
    pub keys_5_to_16: u64,
    /// Number of posting lists with 17 to 64 row ids.
    pub keys_17_to_64: u64,
    /// Number of posting lists with 65 to 256 row ids.
    pub keys_65_to_256: u64,
    /// Number of posting lists with 257 to 1024 row ids.
    pub keys_257_to_1024: u64,
    /// Number of posting lists with 1025 to 4096 row ids.
    pub keys_1025_to_4096: u64,
    /// Number of posting lists with more than 4096 row ids.
    pub keys_over_4096: u64,
    /// Estimated bytes for the current row-id array representation.
    pub estimated_row_id_bytes: u64,
}

/// Benchmark-only posting histograms for all runtime index groups.
#[cfg(feature = "bench-internals")]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct StorePostingHistograms {
    /// Exact resource index.
    pub resource: StorePostingHistogram,
    /// Resource object index.
    pub resource_object: StorePostingHistogram,
    /// Resource type and relation index.
    pub resource_type_relation: StorePostingHistogram,
    /// Resource type index.
    pub resource_type: StorePostingHistogram,
    /// Exact subject index.
    pub subject: StorePostingHistogram,
    /// Subject type and relation index.
    pub subject_type_relation: StorePostingHistogram,
    /// Subject type index.
    pub subject_type: StorePostingHistogram,
}

#[cfg(feature = "bench-internals")]
impl StorePostingHistogram {
    fn record_posting_len(&mut self, len: usize) {
        let len = u64_from_usize_saturating(len);
        self.keys = self.keys.saturating_add(1);
        self.total_postings = self.total_postings.saturating_add(len);
        self.max_posting_len = self.max_posting_len.max(len);
        self.estimated_row_id_bytes = self.estimated_row_id_bytes.saturating_add(
            len.saturating_mul(u64_from_usize_saturating(std::mem::size_of::<RowId>())),
        );
        match len {
            0 => {}
            1 => self.singleton_keys = self.singleton_keys.saturating_add(1),
            2..=4 => self.keys_2_to_4 = self.keys_2_to_4.saturating_add(1),
            5..=16 => self.keys_5_to_16 = self.keys_5_to_16.saturating_add(1),
            17..=64 => self.keys_17_to_64 = self.keys_17_to_64.saturating_add(1),
            65..=256 => self.keys_65_to_256 = self.keys_65_to_256.saturating_add(1),
            257..=1024 => self.keys_257_to_1024 = self.keys_257_to_1024.saturating_add(1),
            1025..=4096 => {
                self.keys_1025_to_4096 = self.keys_1025_to_4096.saturating_add(1);
            }
            _ => self.keys_over_4096 = self.keys_over_4096.saturating_add(1),
        }
    }
}

#[cfg(feature = "bench-internals")]
#[derive(Debug, Default)]
struct ActivePostingHistogramBuilder {
    resource: HashMap<ResourceIndexKey, usize>,
    resource_object: HashMap<ResourceObjectIndexKey, usize>,
    resource_type_relation: HashMap<ResourceTypeRelationIndexKey, usize>,
    resource_type: HashMap<ObjectTypeId, usize>,
    subject: HashMap<SubjectIndexKey, usize>,
    subject_type_relation: HashMap<SubjectTypeRelationIndexKey, usize>,
    subject_type: HashMap<SubjectTypeId, usize>,
}

#[cfg(feature = "bench-internals")]
impl ActivePostingHistogramBuilder {
    fn record_row(&mut self, row: &RelationshipRow) {
        increment_posting(&mut self.resource, ResourceIndexKey::from(row));
        increment_posting(&mut self.resource_object, ResourceObjectIndexKey::from(row));
        increment_posting(
            &mut self.resource_type_relation,
            ResourceTypeRelationIndexKey::from(row),
        );
        increment_posting(&mut self.resource_type, row.resource_type);
        for key in SubjectIndexKey::from_row(row) {
            increment_posting(&mut self.subject, key);
        }
        if let Some(key) = SubjectTypeRelationIndexKey::from_row(row) {
            increment_posting(&mut self.subject_type_relation, key);
        }
        increment_posting(&mut self.subject_type, row.subject_type);
    }

    fn finish(self) -> StorePostingHistograms {
        StorePostingHistograms {
            resource: histogram_from_posting_lengths(self.resource.into_values()),
            resource_object: histogram_from_posting_lengths(self.resource_object.into_values()),
            resource_type_relation: histogram_from_posting_lengths(
                self.resource_type_relation.into_values(),
            ),
            resource_type: histogram_from_posting_lengths(self.resource_type.into_values()),
            subject: histogram_from_posting_lengths(self.subject.into_values()),
            subject_type_relation: histogram_from_posting_lengths(
                self.subject_type_relation.into_values(),
            ),
            subject_type: histogram_from_posting_lengths(self.subject_type.into_values()),
        }
    }
}

#[cfg(feature = "bench-internals")]
fn increment_posting<K>(postings: &mut HashMap<K, usize>, key: K)
where
    K: Eq + Hash,
{
    let count = postings.entry(key).or_default();
    *count = count.saturating_add(1);
}

#[cfg(feature = "bench-internals")]
fn histogram_from_posting_lengths(
    lengths: impl IntoIterator<Item = usize>,
) -> StorePostingHistogram {
    let mut histogram = StorePostingHistogram::default();
    for len in lengths {
        histogram.record_posting_len(len);
    }
    histogram
}

#[cfg(feature = "bench-internals")]
fn u64_from_usize_saturating(value: usize) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}

/// Resets benchmark-only segmented-store read counters.
#[cfg(feature = "bench-internals")]
pub fn reset_store_view_read_counters() {
    STORE_VIEW_QUERY_CALLS.store(0, Ordering::Relaxed);
    DELTA_SEGMENTS_INSPECTED.store(0, Ordering::Relaxed);
    TOMBSTONE_CHECKS.store(0, Ordering::Relaxed);
}

/// Returns benchmark-only segmented-store read counters.
#[cfg(feature = "bench-internals")]
#[must_use]
pub fn store_view_read_counters() -> StoreViewReadCounters {
    StoreViewReadCounters {
        query_calls: STORE_VIEW_QUERY_CALLS.load(Ordering::Relaxed),
        delta_segments_inspected: DELTA_SEGMENTS_INSPECTED.load(Ordering::Relaxed),
        tombstone_checks: TOMBSTONE_CHECKS.load(Ordering::Relaxed),
    }
}

#[cfg(feature = "bench-internals")]
fn record_store_view_query_call() {
    STORE_VIEW_QUERY_CALLS.fetch_add(1, Ordering::Relaxed);
}

#[cfg(not(feature = "bench-internals"))]
fn record_store_view_query_call() {}

#[cfg(feature = "bench-internals")]
fn record_delta_segment_inspected() {
    DELTA_SEGMENTS_INSPECTED.fetch_add(1, Ordering::Relaxed);
}

#[cfg(not(feature = "bench-internals"))]
fn record_delta_segment_inspected() {}

#[cfg(feature = "bench-internals")]
fn record_tombstone_check() {
    TOMBSTONE_CHECKS.fetch_add(1, Ordering::Relaxed);
}

#[cfg(not(feature = "bench-internals"))]
fn record_tombstone_check() {}

/// Errors produced by the indexed relationship store.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum StoreError {
    /// A create mutation attempted to insert an existing relationship.
    #[error("relationship already exists: {relationship}")]
    RelationshipAlreadyExists {
        /// Duplicate relationship.
        relationship: Box<Relationship>,
    },

    /// A delete mutation targeted a missing relationship.
    #[error("relationship not found: {relationship}")]
    RelationshipNotFound {
        /// Missing relationship.
        relationship: Box<Relationship>,
    },

    /// A mutation batch contains conflicting mutations for the same relationship.
    #[error("duplicate mutation for relationship: {relationship}")]
    DuplicateMutation {
        /// Duplicated relationship key.
        relationship: Box<Relationship>,
    },

    /// A precondition failed.
    #[error("precondition failed: {precondition:?}")]
    PreconditionFailed {
        /// Failed precondition.
        precondition: Box<Precondition>,
    },

    /// A mutation batch exceeded the configured in-memory store cap.
    #[error("mutation batch too large: {actual} exceeds limit {limit}")]
    MutationBatchTooLarge {
        /// Maximum allowed mutations.
        limit: usize,
        /// Submitted mutation count.
        actual: usize,
    },

    /// A precondition batch exceeded the configured in-memory store cap.
    #[error("precondition batch too large: {actual} exceeds limit {limit}")]
    PreconditionBatchTooLarge {
        /// Maximum allowed preconditions.
        limit: usize,
        /// Submitted precondition count.
        actual: usize,
    },

    /// A compact store id space was exhausted.
    #[error("compact store capacity exceeded for {component}")]
    CapacityExceeded {
        /// Component that reached its representable capacity.
        component: &'static str,
    },

    /// A compact store invariant was violated.
    #[error("compact store internal invariant failed: {reason}")]
    InternalInvariant {
        /// Static invariant failure reason.
        reason: &'static str,
    },

    /// Validated compact data failed to materialize back to a domain value.
    #[error("compact store domain materialization failed: {message}")]
    DomainMaterialization {
        /// Domain validation error message.
        message: String,
    },
}

impl From<DomainError> for StoreError {
    fn from(value: DomainError) -> Self {
        Self::DomainMaterialization {
            message: value.to_string(),
        }
    }
}

/// Compact evaluator recursion key built from interned store identifiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct StoreCheckKey {
    object_type: NonZeroU32,
    object_id: NonZeroU32,
    relation: NonZeroU32,
    subject_type: NonZeroU32,
    subject_id: NonZeroU32,
    subject_relation: Option<NonZeroU32>,
}

/// Maximum number of query results.
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QueryLimit(NonZeroUsize);

impl QueryLimit {
    /// Creates a query limit.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] indirectly through callers when zero cannot be represented.
    #[must_use]
    pub const fn new(limit: NonZeroUsize) -> Self {
        Self(limit)
    }

    /// Returns the default query limit.
    #[must_use]
    pub fn default_limit() -> Self {
        match NonZeroUsize::new(DEFAULT_QUERY_LIMIT) {
            Some(limit) => Self(limit),
            None => Self(NonZeroUsize::MIN),
        }
    }

    fn get(self) -> usize {
        self.0.get()
    }
}

impl Default for QueryLimit {
    fn default() -> Self {
        Self::default_limit()
    }
}

/// Subject-side relationship filter.
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize, serde::Deserialize),
    serde(rename_all = "camelCase", deny_unknown_fields)
)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubjectFilter {
    subject_type: SubjectType,
    optional_subject_id: Option<SubjectId>,
    optional_relation: Option<RelationName>,
}

impl SubjectFilter {
    /// Creates a subject filter.
    #[must_use]
    pub fn new(
        subject_type: SubjectType,
        optional_subject_id: Option<SubjectId>,
        optional_relation: Option<RelationName>,
    ) -> Self {
        Self {
            subject_type,
            optional_subject_id,
            optional_relation,
        }
    }

    /// Creates an exact subject filter.
    #[must_use]
    pub fn exact(
        subject_type: SubjectType,
        subject_id: SubjectId,
        relation: Option<RelationName>,
    ) -> Self {
        Self::new(subject_type, Some(subject_id), relation)
    }

    /// Returns the subject type.
    #[must_use]
    pub fn subject_type(&self) -> &SubjectType {
        &self.subject_type
    }

    /// Returns the optional subject id.
    #[must_use]
    pub fn optional_subject_id(&self) -> Option<&SubjectId> {
        self.optional_subject_id.as_ref()
    }

    /// Returns the optional subject relation.
    #[must_use]
    pub fn optional_relation(&self) -> Option<&RelationName> {
        self.optional_relation.as_ref()
    }
}

impl TryFrom<&User> for SubjectFilter {
    type Error = ZanzibarError;

    fn try_from(value: &User) -> Result<Self, Self::Error> {
        match value {
            User::UserId(id) => Ok(Self::exact(
                SubjectType::try_from("user")?,
                SubjectId::try_from(id.as_str())?,
                None,
            )),
            User::Userset(object, relation) => Ok(Self::exact(
                SubjectType::try_from(object.namespace.as_str())?,
                SubjectId::try_from(object.id.as_str())?,
                Some(RelationName::try_from(relation.0.as_str())?),
            )),
        }
    }
}

/// Resource-side relationship filter.
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize, serde::Deserialize),
    serde(rename_all = "camelCase", deny_unknown_fields)
)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelationshipFilter {
    resource_type: ObjectType,
    optional_resource_id: Option<ObjectId>,
    optional_relation: Option<RelationName>,
    optional_subject: Option<SubjectFilter>,
    limit: QueryLimit,
}

impl RelationshipFilter {
    /// Creates a relationship filter.
    #[must_use]
    pub fn new(
        resource_type: ObjectType,
        optional_resource_id: Option<ObjectId>,
        optional_relation: Option<RelationName>,
        optional_subject: Option<SubjectFilter>,
        limit: QueryLimit,
    ) -> Self {
        Self {
            resource_type,
            optional_resource_id,
            optional_relation,
            optional_subject,
            limit,
        }
    }

    /// Creates a filter for an exact resource, relation, and subject.
    #[must_use]
    pub fn for_exact_subject(
        resource: &ObjectRef,
        relation: RelationName,
        subject: SubjectFilter,
    ) -> Self {
        Self::new(
            resource.object_type().clone(),
            Some(resource.object_id().clone()),
            Some(relation),
            Some(subject),
            QueryLimit::default(),
        )
    }

    /// Returns the resource object type.
    #[must_use]
    pub fn resource_type(&self) -> &ObjectType {
        &self.resource_type
    }

    /// Returns the optional resource id.
    #[must_use]
    pub fn optional_resource_id(&self) -> Option<&ObjectId> {
        self.optional_resource_id.as_ref()
    }

    /// Returns the optional resource relation.
    #[must_use]
    pub fn optional_relation(&self) -> Option<&RelationName> {
        self.optional_relation.as_ref()
    }

    /// Returns the optional subject filter.
    #[must_use]
    pub fn optional_subject(&self) -> Option<&SubjectFilter> {
        self.optional_subject.as_ref()
    }
}

/// Relationship mutation.
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize, serde::Deserialize),
    serde(rename_all = "camelCase")
)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RelationshipMutation {
    /// Insert only if absent.
    Create(Relationship),
    /// Insert if absent and succeed if present.
    Touch(Relationship),
    /// Remove only if present.
    Delete(Relationship),
}

impl RelationshipMutation {
    /// Creates an insert-if-absent mutation from relationship text.
    ///
    /// # Errors
    ///
    /// Returns [`DomainError`] when `relationship` is not a valid `object#relation@subject`
    /// relationship.
    pub fn create(relationship: impl AsRef<str>) -> Result<Self, DomainError> {
        Ok(Self::Create(relationship.as_ref().parse()?))
    }

    /// Creates an idempotent insert mutation from relationship text.
    ///
    /// # Errors
    ///
    /// Returns [`DomainError`] when `relationship` is not a valid `object#relation@subject`
    /// relationship.
    pub fn touch(relationship: impl AsRef<str>) -> Result<Self, DomainError> {
        Ok(Self::Touch(relationship.as_ref().parse()?))
    }

    /// Creates a remove-only-if-present mutation from relationship text.
    ///
    /// # Errors
    ///
    /// Returns [`DomainError`] when `relationship` is not a valid `object#relation@subject`
    /// relationship.
    pub fn delete(relationship: impl AsRef<str>) -> Result<Self, DomainError> {
        Ok(Self::Delete(relationship.as_ref().parse()?))
    }

    /// Returns the relationship targeted by this mutation.
    #[must_use]
    pub fn relationship(&self) -> &Relationship {
        match self {
            Self::Create(relationship) | Self::Touch(relationship) | Self::Delete(relationship) => {
                relationship
            }
        }
    }
}

/// Mutation precondition.
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize, serde::Deserialize),
    serde(rename_all = "camelCase")
)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Precondition {
    /// At least one relationship must match.
    MustMatch(RelationshipFilter),
    /// No relationships may match.
    MustNotMatch(RelationshipFilter),
}

/// Read-only relationship query interface.
pub trait RelationshipReader {
    /// Iterator type returned by resource-side and subject-side queries.
    type Iter<'a>: Iterator<Item = &'a Relationship>
    where
        Self: 'a;

    /// Queries relationships from the resource side.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] when a reader implementation cannot evaluate the query.
    fn query_relationships(
        &self,
        filter: &RelationshipFilter,
    ) -> Result<Self::Iter<'_>, StoreError>;

    /// Queries relationships from the subject side.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] when a reader implementation cannot evaluate the query.
    fn reverse_query_relationships(
        &self,
        filter: &SubjectFilter,
    ) -> Result<Self::Iter<'_>, StoreError>;
}

/// Indexed immutable-row in-memory relationship store.
#[derive(Debug)]
pub struct IndexedRelationshipStore {
    index_profile: IndexProfile,
    interner: IdentifierInterner,
    rows: Vec<RelationshipRow>,
    live_rows: LiveRows,
    dead_row_count: usize,
    uniqueness: UniquenessState,
    by_resource: PostingIndex<ResourceIndexKey>,
    by_resource_object: PostingIndex<ResourceObjectIndexKey>,
    by_resource_type_relation: PostingIndex<ResourceTypeRelationIndexKey>,
    by_resource_type: PostingIndex<ObjectTypeId>,
    by_subject: PostingIndex<SubjectIndexKey>,
    by_subject_type_relation: PostingIndex<SubjectTypeRelationIndexKey>,
    by_subject_type: PostingIndex<SubjectTypeId>,
}

impl Default for IndexedRelationshipStore {
    fn default() -> Self {
        Self {
            index_profile: IndexProfile::Full,
            interner: IdentifierInterner::default(),
            rows: Vec::new(),
            live_rows: LiveRows::default(),
            dead_row_count: 0,
            uniqueness: UniquenessState::Ready(RelationshipIdentityIndex::default()),
            by_resource: PostingIndex::default(),
            by_resource_object: PostingIndex::default(),
            by_resource_type_relation: PostingIndex::default(),
            by_resource_type: PostingIndex::default(),
            by_subject: PostingIndex::default(),
            by_subject_type_relation: PostingIndex::default(),
            by_subject_type: PostingIndex::default(),
        }
    }
}

impl Clone for IndexedRelationshipStore {
    fn clone(&self) -> Self {
        Self {
            index_profile: self.index_profile,
            interner: self.interner.clone(),
            rows: self.rows.clone(),
            live_rows: self.live_rows.clone(),
            dead_row_count: self.dead_row_count,
            uniqueness: self.uniqueness.clone(),
            by_resource: self.by_resource.clone(),
            by_resource_object: self.by_resource_object.clone(),
            by_resource_type_relation: self.by_resource_type_relation.clone(),
            by_resource_type: self.by_resource_type.clone(),
            by_subject: self.by_subject.clone(),
            by_subject_type_relation: self.by_subject_type_relation.clone(),
            by_subject_type: self.by_subject_type.clone(),
        }
    }
}

/// Immutable relationship view published with each exact revision.
///
/// A view is a full indexed checkpoint plus an optional bounded delta overlay. Write publication
/// clones and updates the overlay instead of cloning the full checkpoint, while readers continue to
/// see an immutable `Arc` for the exact revision they requested.
#[derive(Debug, Clone)]
pub struct RelationshipStoreView {
    checkpoint: Arc<IndexedRelationshipStore>,
    delta: Option<StoreDelta>,
}

impl Default for RelationshipStoreView {
    fn default() -> Self {
        Self::from_checkpoint(Arc::new(IndexedRelationshipStore::default()))
    }
}

impl RelationshipStoreView {
    /// Creates a view from one fully indexed checkpoint and no delta overlay.
    #[must_use]
    pub fn from_checkpoint(checkpoint: Arc<IndexedRelationshipStore>) -> Self {
        Self {
            checkpoint,
            delta: None,
        }
    }

    pub(crate) fn apply_mutations(
        &self,
        mutations: impl IntoIterator<Item = RelationshipMutation>,
        preconditions: impl IntoIterator<Item = Precondition>,
    ) -> Result<Arc<Self>, StoreError> {
        let preconditions = preconditions.into_iter().collect::<Vec<_>>();
        if preconditions.len() > MAX_PRECONDITIONS_PER_BATCH {
            return Err(StoreError::PreconditionBatchTooLarge {
                limit: MAX_PRECONDITIONS_PER_BATCH,
                actual: preconditions.len(),
            });
        }
        for precondition in &preconditions {
            self.check_precondition(precondition)?;
        }

        let mutations = mutations.into_iter().collect::<Vec<_>>();
        if mutations.len() > MAX_MUTATIONS_PER_BATCH {
            return Err(StoreError::MutationBatchTooLarge {
                limit: MAX_MUTATIONS_PER_BATCH,
                actual: mutations.len(),
            });
        }
        let mut seen = HashSet::with_capacity(mutations.len());
        for mutation in &mutations {
            let relationship = mutation.relationship();
            if !seen.insert(relationship.clone()) {
                return Err(StoreError::DuplicateMutation {
                    relationship: Box::new(relationship.clone()),
                });
            }
        }
        if mutations.is_empty() {
            return Ok(Arc::new(self.clone()));
        }

        let mut base = self.clone();
        base.ensure_checkpoint_mutation_ready()?;

        let mut inserted = base
            .delta
            .as_ref()
            .map_or_else(IndexedRelationshipStore::default, |delta| {
                (*delta.inserted).clone()
            });
        let mut deleted = base
            .delta
            .as_ref()
            .map_or_else(HashSet::new, |delta| (*delta.deleted).clone());

        for mutation in mutations.iter().cloned() {
            base.apply_delta_mutation(&mut inserted, &mut deleted, mutation)?;
        }

        let previous_mutations = base
            .delta
            .as_ref()
            .map_or(0, |delta| delta.mutation_count.get());
        let mutation_count = previous_mutations
            .checked_add(mutations.len())
            .and_then(NonZeroUsize::new)
            .ok_or(StoreError::CapacityExceeded {
                component: "store view delta mutations",
            })?;
        let candidate = Self {
            checkpoint: Arc::clone(&base.checkpoint),
            delta: Some(StoreDelta {
                inserted: Arc::new(inserted),
                deleted_rows: Arc::new(base.deleted_relationship_rows(&deleted)),
                deleted: Arc::new(deleted),
                mutation_count,
            }),
        };
        if candidate.should_checkpoint() {
            return Ok(Arc::new(candidate.checkpointed()?));
        }
        Ok(Arc::new(candidate))
    }

    /// Returns true when at least one resource-side relationship matches.
    #[must_use]
    pub fn any_resource_match(&self, filter: &RelationshipFilter) -> bool {
        self.query_compact_relationships(filter).next().is_some()
    }

    /// Returns all live relationships in deterministic order.
    #[must_use]
    pub fn rows(&self) -> Vec<Relationship> {
        let Some(delta) = &self.delta else {
            return self.checkpoint.rows();
        };
        let mut relationships = self
            .checkpoint
            .rows()
            .into_iter()
            .filter(|relationship| !delta.deleted.contains(relationship))
            .collect::<Vec<_>>();
        relationships.extend(delta.inserted.rows());
        relationships
    }

    pub(crate) fn encode_snapshot_sections(
        &self,
        writer: &mut SnapshotSectionWriter,
        index_profile: IndexProfile,
        layout: SnapshotEncodingLayout,
    ) -> Result<(), SnapshotIoError> {
        self.canonical_store()?
            .encode_snapshot_sections(writer, index_profile, layout)
    }

    pub(crate) fn query_compact_relationships(
        &self,
        filter: &RelationshipFilter,
    ) -> StoreViewCompactIter<'_> {
        record_store_view_query_call();
        let inserted = self
            .delta
            .as_ref()
            .map(|delta| delta.inserted.query_compact_relationships(filter));
        if inserted.is_some() {
            record_delta_segment_inspected();
        }
        StoreViewCompactIter {
            inserted,
            checkpoint: self.checkpoint.query_compact_relationships(filter),
            deleted: self.delta.as_ref().map(|delta| delta.deleted_rows.as_ref()),
            phase: StoreViewIterPhase::Inserted,
        }
    }

    pub(crate) fn reverse_query_compact_relationships(
        &self,
        filter: &SubjectFilter,
    ) -> StoreViewCompactIter<'_> {
        record_store_view_query_call();
        let inserted = self
            .delta
            .as_ref()
            .map(|delta| delta.inserted.reverse_query_compact_relationships(filter));
        if inserted.is_some() {
            record_delta_segment_inspected();
        }
        StoreViewCompactIter {
            inserted,
            checkpoint: self.checkpoint.reverse_query_compact_relationships(filter),
            deleted: self.delta.as_ref().map(|delta| delta.deleted_rows.as_ref()),
            phase: StoreViewIterPhase::Inserted,
        }
    }

    pub(crate) fn resource_relation(
        &self,
        resource: &ObjectRef,
        relation: &RelationName,
        limit: QueryLimit,
    ) -> StoreViewCompactIter<'_> {
        record_store_view_query_call();
        let inserted = self
            .delta
            .as_ref()
            .map(|delta| delta.inserted.resource_relation(resource, relation, limit));
        if inserted.is_some() {
            record_delta_segment_inspected();
        }
        StoreViewCompactIter {
            inserted,
            checkpoint: self.checkpoint.resource_relation(resource, relation, limit),
            deleted: self.delta.as_ref().map(|delta| delta.deleted_rows.as_ref()),
            phase: StoreViewIterPhase::Inserted,
        }
    }

    pub(crate) fn any_resource_relation_subject(
        &self,
        resource: &ObjectRef,
        relation: &RelationName,
        subject: &SubjectFilter,
    ) -> bool {
        self.resource_relation_subject(resource, relation, subject)
            .next()
            .is_some()
    }

    fn resource_relation_subject(
        &self,
        resource: &ObjectRef,
        relation: &RelationName,
        subject: &SubjectFilter,
    ) -> StoreViewCompactIter<'_> {
        record_store_view_query_call();
        let inserted = self.delta.as_ref().map(|delta| {
            delta
                .inserted
                .resource_relation_subject(resource, relation, subject)
        });
        if inserted.is_some() {
            record_delta_segment_inspected();
        }
        StoreViewCompactIter {
            inserted,
            checkpoint: self
                .checkpoint
                .resource_relation_subject(resource, relation, subject),
            deleted: self.delta.as_ref().map(|delta| delta.deleted_rows.as_ref()),
            phase: StoreViewIterPhase::Inserted,
        }
    }

    pub(crate) fn index_profile(&self) -> IndexProfile {
        self.checkpoint.index_profile()
    }

    /// Returns benchmark-only delta stats for this view.
    #[cfg(feature = "bench-internals")]
    #[must_use]
    pub fn delta_stats(&self) -> StoreViewDeltaStats {
        let checkpoint_rows = self.checkpoint.live_row_count();
        let Some(delta) = &self.delta else {
            return StoreViewDeltaStats {
                checkpoint_rows,
                ..StoreViewDeltaStats::default()
            };
        };
        let delta_inserted_rows = delta.inserted.live_row_count();
        let delta_deleted_rows = delta.deleted_rows.len();
        let denominator = checkpoint_rows.saturating_add(delta_inserted_rows).max(1);
        let ratio = delta_deleted_rows.saturating_mul(10_000) / denominator;
        StoreViewDeltaStats {
            checkpoint_rows,
            delta_segments: 1,
            delta_inserted_rows,
            delta_deleted_rows,
            delta_mutations: delta.mutation_count.get(),
            tombstone_ratio_bps: u16::try_from(ratio).unwrap_or(u16::MAX),
        }
    }

    /// Returns benchmark-only logical posting histograms for the active view.
    #[cfg(feature = "bench-internals")]
    #[must_use]
    pub fn posting_histograms(&self) -> StorePostingHistograms {
        let mut builder = ActivePostingHistogramBuilder::default();
        let deleted = self.delta.as_ref().map(|delta| delta.deleted_rows.as_ref());
        self.checkpoint
            .record_active_postings(&mut builder, deleted);
        if let Some(delta) = &self.delta {
            delta.inserted.record_active_postings(&mut builder, None);
        }
        builder.finish()
    }

    pub(crate) fn store_check_key(
        &self,
        object: &Object,
        relation: &Relation,
        user: &User,
    ) -> Option<StoreCheckKey> {
        if self.delta.is_some() {
            return None;
        }
        self.checkpoint.store_check_key(object, relation, user)
    }

    pub(crate) fn store_check_key_for_relation_name(
        &self,
        object: &Object,
        relation: &RelationName,
        user: &User,
    ) -> Option<StoreCheckKey> {
        if self.delta.is_some() {
            return None;
        }
        self.checkpoint
            .store_check_key_for_relation_name(object, relation, user)
    }

    fn check_precondition(&self, precondition: &Precondition) -> Result<(), StoreError> {
        match precondition {
            Precondition::MustMatch(filter) if !self.any_resource_match(filter) => {
                Err(StoreError::PreconditionFailed {
                    precondition: Box::new(precondition.clone()),
                })
            }
            Precondition::MustNotMatch(filter) if self.any_resource_match(filter) => {
                Err(StoreError::PreconditionFailed {
                    precondition: Box::new(precondition.clone()),
                })
            }
            _ => Ok(()),
        }
    }

    fn ensure_checkpoint_mutation_ready(&mut self) -> Result<(), StoreError> {
        if self.checkpoint.has_ready_uniqueness() {
            return Ok(());
        }
        let mut checkpoint = (*self.checkpoint).clone();
        checkpoint.ensure_uniqueness_index()?;
        self.checkpoint = Arc::new(checkpoint);
        Ok(())
    }

    fn apply_delta_mutation(
        &self,
        inserted: &mut IndexedRelationshipStore,
        deleted: &mut HashSet<Relationship>,
        mutation: RelationshipMutation,
    ) -> Result<(), StoreError> {
        match mutation {
            RelationshipMutation::Create(relationship) => {
                self.create_delta_relationship(inserted, deleted, relationship)
            }
            RelationshipMutation::Touch(relationship) => {
                self.touch_delta_relationship(inserted, deleted, &relationship)
            }
            RelationshipMutation::Delete(relationship) => {
                self.delete_delta_relationship(inserted, deleted, relationship)
            }
        }
    }

    fn create_delta_relationship(
        &self,
        inserted: &mut IndexedRelationshipStore,
        deleted: &mut HashSet<Relationship>,
        relationship: Relationship,
    ) -> Result<(), StoreError> {
        let location = self.relationship_location(inserted, deleted, &relationship);
        match location {
            RelationshipLocation::Inserted | RelationshipLocation::CheckpointLive => {
                Err(StoreError::RelationshipAlreadyExists {
                    relationship: Box::new(relationship),
                })
            }
            RelationshipLocation::CheckpointDeleted => {
                deleted.remove(&relationship);
                Ok(())
            }
            RelationshipLocation::Absent => inserted.insert(&relationship),
        }
    }

    fn touch_delta_relationship(
        &self,
        inserted: &mut IndexedRelationshipStore,
        deleted: &mut HashSet<Relationship>,
        relationship: &Relationship,
    ) -> Result<(), StoreError> {
        match self.relationship_location(inserted, deleted, relationship) {
            RelationshipLocation::Inserted | RelationshipLocation::CheckpointLive => Ok(()),
            RelationshipLocation::CheckpointDeleted => {
                deleted.remove(relationship);
                Ok(())
            }
            RelationshipLocation::Absent => inserted.insert(relationship),
        }
    }

    fn delete_delta_relationship(
        &self,
        inserted: &mut IndexedRelationshipStore,
        deleted: &mut HashSet<Relationship>,
        relationship: Relationship,
    ) -> Result<(), StoreError> {
        match self.relationship_location(inserted, deleted, &relationship) {
            RelationshipLocation::Inserted => inserted.delete(&relationship),
            RelationshipLocation::CheckpointLive => {
                deleted.insert(relationship);
                Ok(())
            }
            RelationshipLocation::CheckpointDeleted | RelationshipLocation::Absent => {
                Err(StoreError::RelationshipNotFound {
                    relationship: Box::new(relationship),
                })
            }
        }
    }

    fn relationship_location(
        &self,
        inserted: &IndexedRelationshipStore,
        deleted: &HashSet<Relationship>,
        relationship: &Relationship,
    ) -> RelationshipLocation {
        if inserted.contains_relationship(relationship) {
            return RelationshipLocation::Inserted;
        }
        if self.checkpoint.contains_relationship(relationship) {
            if deleted.contains(relationship) {
                RelationshipLocation::CheckpointDeleted
            } else {
                RelationshipLocation::CheckpointLive
            }
        } else {
            RelationshipLocation::Absent
        }
    }

    fn should_checkpoint(&self) -> bool {
        self.delta.as_ref().is_some_and(|delta| {
            delta.mutation_count.get() >= STORE_VIEW_MAX_DELTA_MUTATIONS
                || delta.deleted.len() >= STORE_VIEW_MAX_DELTA_TOMBSTONES
        })
    }

    fn checkpointed(&self) -> Result<Self, StoreError> {
        Ok(Self::from_checkpoint(Arc::new(self.canonical_store()?)))
    }

    fn canonical_store(&self) -> Result<IndexedRelationshipStore, StoreError> {
        let mut store = IndexedRelationshipStore::default();
        for relationship in self.rows() {
            store.insert(&relationship)?;
        }
        Ok(store)
    }

    fn deleted_relationship_rows(
        &self,
        deleted: &HashSet<Relationship>,
    ) -> HashSet<RelationshipRow> {
        deleted
            .iter()
            .filter_map(|relationship| {
                RelationshipRow::from_existing_relationship(relationship, &self.checkpoint.interner)
            })
            .collect()
    }
}

#[derive(Debug, Clone)]
struct StoreDelta {
    inserted: Arc<IndexedRelationshipStore>,
    deleted_rows: Arc<HashSet<RelationshipRow>>,
    deleted: Arc<HashSet<Relationship>>,
    mutation_count: NonZeroUsize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RelationshipLocation {
    Inserted,
    CheckpointLive,
    CheckpointDeleted,
    Absent,
}

#[derive(Debug)]
pub(crate) struct StoreViewCompactIter<'a> {
    inserted: Option<CompactRelationshipIter<'a>>,
    checkpoint: CompactRelationshipIter<'a>,
    deleted: Option<&'a HashSet<RelationshipRow>>,
    phase: StoreViewIterPhase,
}

impl<'a> Iterator for StoreViewCompactIter<'a> {
    type Item = RelationshipRef<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            match self.phase {
                StoreViewIterPhase::Inserted => {
                    if let Some(inserted) = &mut self.inserted
                        && let Some(relationship) = inserted.next()
                    {
                        return Some(relationship);
                    }
                    self.phase = StoreViewIterPhase::Checkpoint;
                }
                StoreViewIterPhase::Checkpoint => {
                    let relationship = self.checkpoint.next()?;
                    match self.deleted {
                        Some(deleted) => {
                            record_tombstone_check();
                            if !relationship.is_deleted_by(deleted) {
                                return Some(relationship);
                            }
                        }
                        None => return Some(relationship),
                    }
                }
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StoreViewIterPhase {
    Inserted,
    Checkpoint,
}

impl IndexedRelationshipStore {
    /// Applies preconditions and mutations as one batch.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] when a precondition fails, a create/delete semantic fails, or the
    /// batch contains duplicate relationship keys.
    pub fn apply_mutations(
        &mut self,
        mutations: impl IntoIterator<Item = RelationshipMutation>,
        preconditions: impl IntoIterator<Item = Precondition>,
    ) -> Result<(), StoreError> {
        let preconditions = preconditions.into_iter().collect::<Vec<_>>();
        if preconditions.len() > MAX_PRECONDITIONS_PER_BATCH {
            return Err(StoreError::PreconditionBatchTooLarge {
                limit: MAX_PRECONDITIONS_PER_BATCH,
                actual: preconditions.len(),
            });
        }
        for precondition in &preconditions {
            self.check_precondition(precondition)?;
        }

        let mut mutations = mutations.into_iter().collect::<Vec<_>>();
        if mutations.len() > MAX_MUTATIONS_PER_BATCH {
            return Err(StoreError::MutationBatchTooLarge {
                limit: MAX_MUTATIONS_PER_BATCH,
                actual: mutations.len(),
            });
        }
        if !mutations.is_empty() {
            self.ensure_uniqueness_index()?;
        }
        let mut seen = HashSet::with_capacity(mutations.len());
        for mutation in &mutations {
            let relationship = mutation.relationship();
            if !seen.insert(relationship.clone()) {
                return Err(StoreError::DuplicateMutation {
                    relationship: Box::new(relationship.clone()),
                });
            }
        }

        if mutations.len() == 1
            && let Some(mutation) = mutations.pop()
        {
            self.apply_single_mutation(mutation)?;
            self.compact_if_needed()?;
            return Ok(());
        }

        if mutations
            .iter()
            .all(|mutation| matches!(mutation, RelationshipMutation::Touch(_)))
        {
            self.apply_touch_batch_in_place(&mutations)?;
            self.compact_if_needed()?;
            return Ok(());
        }

        let mut candidate = self.clone();
        for mutation in mutations {
            match mutation {
                RelationshipMutation::Create(relationship) => candidate.create(relationship)?,
                RelationshipMutation::Touch(relationship) => {
                    if !candidate.contains_relationship(&relationship) {
                        candidate.insert(&relationship)?;
                    }
                }
                RelationshipMutation::Delete(relationship) => candidate.delete(&relationship)?,
            }
        }

        candidate.compact_if_needed()?;
        *self = candidate;
        Ok(())
    }

    fn apply_touch_batch_in_place(
        &mut self,
        mutations: &[RelationshipMutation],
    ) -> Result<(), StoreError> {
        let new_relationships = mutations
            .iter()
            .filter(|mutation| !self.contains_relationship(mutation.relationship()))
            .collect::<Vec<_>>();
        let mut additional_symbols = 0_usize;
        let mut additional_bytes = 0_usize;
        for mutation in &new_relationships {
            relationship_identifier_values(mutation.relationship(), |value| {
                if self.interner.lookup(value).is_none() {
                    additional_symbols = additional_symbols.saturating_add(1);
                    additional_bytes = additional_bytes.saturating_add(value.len());
                }
            });
        }
        self.ensure_insert_capacity(
            new_relationships.len(),
            additional_symbols,
            additional_bytes,
        )?;
        for mutation in mutations {
            let relationship = mutation.relationship();
            if !self.contains_relationship(relationship) {
                self.insert(relationship)?;
            }
        }
        Ok(())
    }

    fn ensure_insert_capacity(
        &self,
        additional_rows: usize,
        additional_symbols: usize,
        additional_bytes: usize,
    ) -> Result<(), StoreError> {
        if additional_rows == 0 {
            return Ok(());
        }
        let last_row_len = self
            .rows
            .len()
            .checked_add(additional_rows)
            .and_then(|value| value.checked_sub(1))
            .ok_or(StoreError::CapacityExceeded {
                component: "relationship rows",
            })?;
        RowId::from_len(last_row_len)?;

        if additional_symbols > 0 {
            let last_symbol_index = self
                .interner
                .len()
                .checked_add(additional_symbols)
                .and_then(|value| value.checked_sub(1))
                .ok_or(StoreError::CapacityExceeded {
                    component: "identifier interner",
                })?;
            SymbolId::from_index(last_symbol_index)?;
        }
        self.interner
            .byte_len()
            .checked_add(additional_bytes)
            .and_then(|value| u32::try_from(value).ok())
            .ok_or(StoreError::CapacityExceeded {
                component: "identifier interner bytes",
            })?;
        Ok(())
    }

    fn ensure_uniqueness_index(&mut self) -> Result<(), StoreError> {
        let reason = match self.uniqueness {
            UniquenessState::Ready(_) => return Ok(()),
            UniquenessState::KnownUniqueButNotIndexed => {
                "full snapshot duplicate detector was dropped after validation"
            }
            UniquenessState::UntrustedNotIndexed => {
                "trusted snapshot contains duplicate relationship rows"
            }
        };
        let mut uniqueness = RelationshipIdentityIndex::default();
        for row in self
            .rows
            .iter()
            .filter(|row| self.live_rows.contains(row.row_id))
        {
            if uniqueness.find(&self.rows, row).is_some() {
                return Err(StoreError::InternalInvariant { reason });
            }
            uniqueness.insert(&self.rows, row.row_id, row);
        }
        self.uniqueness = UniquenessState::Ready(uniqueness);
        Ok(())
    }

    /// Returns true when at least one resource-side relationship matches.
    #[must_use]
    pub fn any_resource_match(&self, filter: &RelationshipFilter) -> bool {
        self.query_compact_relationships(filter).next().is_some()
    }

    /// Returns all rows. Intended for tests and migration checks.
    #[must_use]
    pub fn rows(&self) -> Vec<Relationship> {
        self.rows
            .iter()
            .filter(|row| self.live_rows.contains(row.row_id))
            .filter_map(|row| self.relationship_from_row(row).ok())
            .collect()
    }

    #[cfg(feature = "bench-internals")]
    fn live_row_count(&self) -> usize {
        self.rows.len().saturating_sub(self.dead_row_count)
    }

    #[cfg(feature = "bench-internals")]
    fn record_active_postings(
        &self,
        builder: &mut ActivePostingHistogramBuilder,
        deleted: Option<&HashSet<RelationshipRow>>,
    ) {
        for row in self
            .rows
            .iter()
            .filter(|row| self.live_rows.contains(row.row_id))
        {
            if deleted.is_some_and(|deleted| deleted.contains(row)) {
                continue;
            }
            builder.record_row(row);
        }
    }

    pub(crate) fn encode_snapshot_sections(
        &self,
        writer: &mut SnapshotSectionWriter,
        index_profile: IndexProfile,
        layout: SnapshotEncodingLayout,
    ) -> Result<(), SnapshotIoError> {
        let (symbol_table, symbol_table_flags) = encode_v3_symbol_table(
            &self.interner.entries,
            checked_u32_from_usize(self.interner.bytes.len())?,
            layout,
        )?;
        writer.add_section(
            SectionKind::SymbolBytes,
            self.interner.bytes.clone(),
            u64::try_from(self.interner.bytes.len()).map_err(|_| {
                SnapshotIoError::LimitExceeded {
                    component: "symbol bytes",
                }
            })?,
        )?;
        writer.add_section_with_flags(
            SectionKind::SymbolTable,
            symbol_table_flags,
            symbol_table,
            u64::try_from(self.interner.entries.len()).map_err(|_| {
                SnapshotIoError::LimitExceeded {
                    component: "symbol table",
                }
            })?,
        )?;
        let (symbol_hashes, symbol_lookup, symbol_lookup_flags) =
            self.interner.encode_symbol_acceleration(layout)?;
        let symbol_count = u64::try_from(self.interner.entries.len()).map_err(|_| {
            SnapshotIoError::LimitExceeded {
                component: "symbol lookup",
            }
        })?;
        writer.add_section(SectionKind::SymbolHashes, symbol_hashes, symbol_count)?;
        writer.add_section_with_flags(
            SectionKind::SymbolLookup,
            symbol_lookup_flags,
            symbol_lookup,
            symbol_count,
        )?;

        let disk_rows = self.live_disk_rows();
        let row_symbol_width =
            snapshot_symbol_width(checked_u32_from_usize(self.interner.entries.len())?, layout);
        let mut rows = Vec::with_capacity(
            disk_rows
                .len()
                .checked_mul(checked_mul_usize(row_symbol_width.byte_len(), 6)?)
                .ok_or(SnapshotIoError::Format {
                    reason: "relationship rows length overflowed",
                })?,
        );
        for row in &disk_rows {
            row.encode_width(row_symbol_width, &mut rows);
        }
        writer.add_section_with_flags(
            SectionKind::RelationshipRows,
            row_symbol_width.flag_bits(),
            rows,
            u64::try_from(disk_rows.len()).map_err(|_| SnapshotIoError::LimitExceeded {
                component: "relationship rows",
            })?,
        )?;

        let indexes = EncodedSnapshotIndexes::from_rows(&disk_rows, index_profile)?;
        writer.add_section(
            SectionKind::IndexDirectory,
            indexes.directory,
            SNAPSHOT_INDEX_KIND_COUNT_U64,
        )?;
        writer.add_section(SectionKind::IndexKeys, indexes.keys, indexes.key_count)?;
        writer.add_section(
            SectionKind::PostingRanges,
            indexes.ranges,
            indexes.range_count,
        )?;
        writer.add_section(
            SectionKind::PostingRowIds,
            indexes.posting_row_ids,
            indexes.posting_row_id_count,
        )?;
        Ok(())
    }

    pub(crate) fn decode_snapshot_sections(
        reader: &SnapshotReader<'_>,
        profile: SnapshotLoadProfile,
        validation: SnapshotValidationMode,
    ) -> Result<Self, SnapshotIoError> {
        Self::decode_snapshot_sections_inner(reader, profile, validation, None)
    }

    #[cfg(feature = "bench-internals")]
    pub(crate) fn decode_snapshot_sections_with_timings(
        reader: &SnapshotReader<'_>,
        profile: SnapshotLoadProfile,
        validation: SnapshotValidationMode,
        timings: &mut SnapshotLoadPhaseTimings,
    ) -> Result<Self, SnapshotIoError> {
        Self::decode_snapshot_sections_inner(reader, profile, validation, Some(timings))
    }

    fn decode_snapshot_sections_inner(
        reader: &SnapshotReader<'_>,
        profile: SnapshotLoadProfile,
        validation: SnapshotValidationMode,
        mut timings: Option<&mut SnapshotLoadPhaseTimings>,
    ) -> Result<Self, SnapshotIoError> {
        let phase_start = Instant::now();
        let interner = IdentifierInterner::decode_snapshot_sections(reader, validation)?;
        record_relationship_decode_phase(
            &mut timings,
            |timings, elapsed| {
                timings.symbols = elapsed;
            },
            phase_start,
        );
        let phase_start = Instant::now();
        let decoded_rows = decode_snapshot_rows(reader, &interner, validation)?;
        record_relationship_decode_phase(
            &mut timings,
            |timings, elapsed| {
                timings.rows = elapsed;
            },
            phase_start,
        );
        let phase_start = Instant::now();
        let decoded_indexes =
            DecodedSnapshotIndexes::decode(reader, &decoded_rows.rows, profile, validation)?;
        record_relationship_decode_phase(
            &mut timings,
            |timings, elapsed| {
                timings.indexes = elapsed;
            },
            phase_start,
        );
        Ok(Self {
            interner,
            index_profile: reader.header().index_profile,
            rows: decoded_rows.rows,
            live_rows: decoded_rows.live_rows,
            dead_row_count: 0,
            uniqueness: decoded_rows.uniqueness,
            by_resource: decoded_indexes.resource,
            by_resource_object: decoded_indexes.resource_object,
            by_resource_type_relation: decoded_indexes.resource_type_relation,
            by_resource_type: decoded_indexes.resource_type,
            by_subject: decoded_indexes.subject,
            by_subject_type_relation: decoded_indexes.subject_type_relation,
            by_subject_type: decoded_indexes.subject_type,
        })
    }

    fn live_disk_rows(&self) -> Vec<DiskRelationshipRow> {
        let mut rows = Vec::with_capacity(self.rows.len().saturating_sub(self.dead_row_count));
        for row in self
            .rows
            .iter()
            .filter(|row| self.live_rows.contains(row.row_id))
        {
            rows.push(DiskRelationshipRow::from(row));
        }
        rows
    }

    pub(crate) fn query_compact_relationships(
        &self,
        filter: &RelationshipFilter,
    ) -> CompactRelationshipIter<'_> {
        let matcher = self.resource_matcher(filter);
        let candidates = matcher.as_ref().map_or(CandidateRowIds::Empty, |matcher| {
            self.resource_candidate_row_ids(matcher)
        });
        CompactRelationshipIter {
            store: self,
            all_rows_live: self.live_rows.is_all_live(),
            candidates,
            matcher: matcher.map(CompactRelationshipMatcher::Resource),
        }
    }

    pub(crate) fn reverse_query_compact_relationships(
        &self,
        filter: &SubjectFilter,
    ) -> CompactRelationshipIter<'_> {
        let matcher = self.subject_matcher(filter);
        let candidates = matcher.as_ref().map_or(CandidateRowIds::Empty, |matcher| {
            self.subject_candidate_row_ids(matcher)
        });
        CompactRelationshipIter {
            store: self,
            all_rows_live: self.live_rows.is_all_live(),
            candidates,
            matcher: matcher.map(CompactRelationshipMatcher::Subject),
        }
    }

    pub(crate) fn resource_relation(
        &self,
        resource: &ObjectRef,
        relation: &RelationName,
        limit: QueryLimit,
    ) -> CompactRelationshipIter<'_> {
        let matcher = self.resource_relation_matcher(resource, relation, limit);
        let candidates = matcher.as_ref().map_or(CandidateRowIds::Empty, |matcher| {
            self.resource_candidate_row_ids(matcher)
        });
        CompactRelationshipIter {
            store: self,
            all_rows_live: self.live_rows.is_all_live(),
            candidates,
            matcher: matcher.map(CompactRelationshipMatcher::Resource),
        }
    }

    fn resource_relation_subject(
        &self,
        resource: &ObjectRef,
        relation: &RelationName,
        subject: &SubjectFilter,
    ) -> CompactRelationshipIter<'_> {
        let matcher = self.resource_relation_subject_matcher(resource, relation, subject);
        let candidates = matcher.as_ref().map_or(CandidateRowIds::Empty, |matcher| {
            self.resource_candidate_row_ids(matcher)
        });
        CompactRelationshipIter {
            store: self,
            all_rows_live: self.live_rows.is_all_live(),
            candidates,
            matcher: matcher.map(CompactRelationshipMatcher::Resource),
        }
    }

    pub(crate) const fn index_profile(&self) -> IndexProfile {
        self.index_profile
    }

    pub(crate) fn store_check_key(
        &self,
        object: &Object,
        relation: &Relation,
        user: &User,
    ) -> Option<StoreCheckKey> {
        self.store_check_key_for_relation(
            object,
            self.interner.lookup(relation.0.as_str()).map(RelationId)?,
            user,
        )
    }

    pub(crate) fn store_check_key_for_relation_name(
        &self,
        object: &Object,
        relation: &RelationName,
        user: &User,
    ) -> Option<StoreCheckKey> {
        self.store_check_key_for_relation(
            object,
            self.interner.lookup(relation.as_str()).map(RelationId)?,
            user,
        )
    }

    fn store_check_key_for_relation(
        &self,
        object: &Object,
        relation: RelationId,
        user: &User,
    ) -> Option<StoreCheckKey> {
        let subject = self.store_subject_key(user)?;
        Some(StoreCheckKey {
            object_type: self.interner.lookup(object.namespace.as_str())?.0,
            object_id: self.interner.lookup(object.id.as_str())?.0,
            relation: relation.0.0,
            subject_type: subject.subject_type.0.0,
            subject_id: subject.subject_id.0.0,
            subject_relation: subject.relation.map(|relation| relation.0.0),
        })
    }

    fn store_subject_key(&self, user: &User) -> Option<SubjectIndexKey> {
        match user {
            User::UserId(id) => Some(SubjectIndexKey {
                subject_type: SubjectTypeId(self.interner.lookup("user")?),
                subject_id: SubjectIdId(self.interner.lookup(id.as_str())?),
                relation: None,
            }),
            User::Userset(object, relation) => Some(SubjectIndexKey {
                subject_type: SubjectTypeId(self.interner.lookup(object.namespace.as_str())?),
                subject_id: SubjectIdId(self.interner.lookup(object.id.as_str())?),
                relation: Some(RelationId(self.interner.lookup(relation.0.as_str())?)),
            }),
        }
    }

    fn check_precondition(&self, precondition: &Precondition) -> Result<(), StoreError> {
        match precondition {
            Precondition::MustMatch(filter) if !self.any_resource_match(filter) => {
                Err(StoreError::PreconditionFailed {
                    precondition: Box::new(precondition.clone()),
                })
            }
            Precondition::MustNotMatch(filter) if self.any_resource_match(filter) => {
                Err(StoreError::PreconditionFailed {
                    precondition: Box::new(precondition.clone()),
                })
            }
            _ => Ok(()),
        }
    }

    fn create(&mut self, relationship: Relationship) -> Result<(), StoreError> {
        if self.contains_relationship(&relationship) {
            return Err(StoreError::RelationshipAlreadyExists {
                relationship: Box::new(relationship),
            });
        }
        self.insert(&relationship)?;
        Ok(())
    }

    fn apply_single_mutation(&mut self, mutation: RelationshipMutation) -> Result<(), StoreError> {
        match mutation {
            RelationshipMutation::Create(relationship) => self.create(relationship),
            RelationshipMutation::Touch(relationship) => {
                if !self.contains_relationship(&relationship) {
                    self.insert(&relationship)?;
                }
                Ok(())
            }
            RelationshipMutation::Delete(relationship) => self.delete(&relationship),
        }
    }

    fn insert(&mut self, relationship: &Relationship) -> Result<(), StoreError> {
        let row_id = RowId::from_len(self.rows.len())?;
        let row = RelationshipRow::from_relationship(row_id, relationship, &mut self.interner)?;
        let uniqueness = Self::ready_uniqueness_mut(&mut self.uniqueness)?;
        uniqueness.insert(&self.rows, row_id, &row);
        self.index_relationship(row_id, &row);
        self.rows.push(row);
        self.live_rows.insert(row_id);
        Ok(())
    }

    fn delete(&mut self, relationship: &Relationship) -> Result<(), StoreError> {
        let row = self.lookup_relationship_row(relationship).ok_or_else(|| {
            StoreError::RelationshipNotFound {
                relationship: Box::new(relationship.clone()),
            }
        })?;
        let uniqueness = Self::ready_uniqueness_mut(&mut self.uniqueness)?;
        let row_id = uniqueness.remove(&self.rows, &row).ok_or_else(|| {
            StoreError::RelationshipNotFound {
                relationship: Box::new(relationship.clone()),
            }
        })?;
        self.live_rows.remove(row_id);
        self.dead_row_count = self.dead_row_count.saturating_add(1);

        Ok(())
    }

    pub(crate) fn contains_relationship(&self, relationship: &Relationship) -> bool {
        self.lookup_relationship_row(relationship)
            .is_some_and(|row| {
                self.uniqueness_ref()
                    .is_some_and(|index| index.find(&self.rows, &row).is_some())
            })
    }

    fn lookup_relationship_row(&self, relationship: &Relationship) -> Option<RelationshipRow> {
        RelationshipRow::from_existing_relationship(relationship, &self.interner)
    }

    fn relationship_from_row(&self, row: &RelationshipRow) -> Result<Relationship, StoreError> {
        let resource = ObjectRef::new(
            ObjectType::try_from(self.interner.resolve(row.resource_type.0)?)?,
            ObjectId::try_from(self.interner.resolve(row.resource_id.0)?)?,
        );
        let relation = RelationName::try_from(self.interner.resolve(row.relation.0)?)?;
        let subject_object = ObjectRef::new(
            ObjectType::try_from(self.interner.resolve(row.subject_type.0)?)?,
            ObjectId::try_from(self.interner.resolve(row.subject_id.0)?)?,
        );
        let subject = match row.subject_relation {
            Some(relation) => SubjectRef::Userset {
                object: subject_object,
                relation: RelationName::try_from(self.interner.resolve(relation.0)?)?,
            },
            None => SubjectRef::Object(subject_object),
        };
        Ok(Relationship::new(resource, relation, subject))
    }

    /// Creates a short-lived compatibility reader that yields owned-domain relationship refs.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] when compact rows cannot be materialized into domain values.
    pub fn materialized_reader(&self) -> Result<MaterializedRelationshipReader<'_>, StoreError> {
        let mut rows = Vec::with_capacity(self.rows.len());
        for row in &self.rows {
            rows.push(self.relationship_from_row(row)?);
        }
        Ok(MaterializedRelationshipReader { store: self, rows })
    }

    fn resource_matcher(&self, filter: &RelationshipFilter) -> Option<ResourceMatcher> {
        let resource_type = self.interner.lookup(filter.resource_type.as_str())?;
        let optional_resource_id = match &filter.optional_resource_id {
            Some(resource_id) => match self.interner.lookup(resource_id.as_str()) {
                Some(id) => Some(ObjectIdId(id)),
                None => return None,
            },
            None => None,
        };
        let optional_relation = match &filter.optional_relation {
            Some(relation) => match self.interner.lookup(relation.as_str()) {
                Some(id) => Some(RelationId(id)),
                None => return None,
            },
            None => None,
        };
        let optional_subject = match &filter.optional_subject {
            Some(subject) => match self.subject_matcher(subject) {
                Some(subject) => Some(subject),
                None => return None,
            },
            None => None,
        };
        Some(ResourceMatcher {
            resource_type: ObjectTypeId(resource_type),
            optional_resource_id,
            optional_relation,
            optional_subject,
            remaining: filter.limit.get(),
        })
    }

    fn resource_relation_matcher(
        &self,
        resource: &ObjectRef,
        relation: &RelationName,
        limit: QueryLimit,
    ) -> Option<ResourceMatcher> {
        Some(ResourceMatcher {
            resource_type: ObjectTypeId(self.interner.lookup(resource.object_type().as_str())?),
            optional_resource_id: Some(ObjectIdId(
                self.interner.lookup(resource.object_id().as_str())?,
            )),
            optional_relation: Some(RelationId(self.interner.lookup(relation.as_str())?)),
            optional_subject: None,
            remaining: limit.get(),
        })
    }

    fn resource_relation_subject_matcher(
        &self,
        resource: &ObjectRef,
        relation: &RelationName,
        subject: &SubjectFilter,
    ) -> Option<ResourceMatcher> {
        let mut matcher =
            self.resource_relation_matcher(resource, relation, QueryLimit::new(NonZeroUsize::MIN))?;
        matcher.optional_subject = Some(self.subject_matcher(subject)?);
        Some(matcher)
    }

    fn subject_matcher(&self, filter: &SubjectFilter) -> Option<SubjectMatcher> {
        let subject_type = self.interner.lookup(filter.subject_type.as_str())?;
        let optional_subject_id = match &filter.optional_subject_id {
            Some(subject_id) => match self.interner.lookup(subject_id.as_str()) {
                Some(id) => Some(SubjectIdId(id)),
                None => return None,
            },
            None => None,
        };
        let optional_relation = match &filter.optional_relation {
            Some(relation) => match self.interner.lookup(relation.as_str()) {
                Some(id) => Some(RelationId(id)),
                None => return None,
            },
            None => None,
        };
        Some(SubjectMatcher {
            subject_type: SubjectTypeId(subject_type),
            optional_subject_id,
            optional_relation,
        })
    }

    fn resource_candidate_row_ids(&self, matcher: &ResourceMatcher) -> CandidateRowIds<'_> {
        match (matcher.optional_resource_id, matcher.optional_relation) {
            (Some(resource_id), Some(relation)) => {
                let key = ResourceIndexKey {
                    object_type: matcher.resource_type,
                    object_id: resource_id,
                    relation,
                };
                self.by_resource.candidates(&key)
            }
            (Some(resource_id), None) => {
                let key = ResourceObjectIndexKey {
                    object_type: matcher.resource_type,
                    object_id: resource_id,
                };
                self.by_resource_object.candidates(&key)
            }
            (None, Some(relation)) => {
                let key = ResourceTypeRelationIndexKey {
                    object_type: matcher.resource_type,
                    relation,
                };
                self.by_resource_type_relation.candidates(&key)
            }
            (None, None) => self.by_resource_type.candidates(&matcher.resource_type),
        }
    }

    fn subject_candidate_row_ids(&self, matcher: &SubjectMatcher) -> CandidateRowIds<'_> {
        match (matcher.optional_subject_id, matcher.optional_relation) {
            (Some(subject_id), relation) => {
                let key = SubjectIndexKey {
                    subject_type: matcher.subject_type,
                    subject_id,
                    relation,
                };
                self.by_subject.candidates(&key)
            }
            (None, Some(relation)) => {
                let key = SubjectTypeRelationIndexKey {
                    subject_type: matcher.subject_type,
                    relation,
                };
                self.by_subject_type_relation.candidates(&key)
            }
            (None, None) => self.by_subject_type.candidates(&matcher.subject_type),
        }
    }

    fn index_relationship(&mut self, row_id: RowId, row: &RelationshipRow) {
        self.by_resource.insert(ResourceIndexKey::from(row), row_id);
        self.by_resource_object
            .insert(ResourceObjectIndexKey::from(row), row_id);
        self.by_resource_type_relation
            .insert(ResourceTypeRelationIndexKey::from(row), row_id);
        self.by_resource_type.insert(row.resource_type, row_id);
        for key in SubjectIndexKey::from_row(row) {
            self.by_subject.insert(key, row_id);
        }
        if let Some(key) = SubjectTypeRelationIndexKey::from_row(row) {
            self.by_subject_type_relation.insert(key, row_id);
        }
        self.by_subject_type.insert(row.subject_type, row_id);
    }

    fn compact_if_needed(&mut self) -> Result<(), StoreError> {
        let dead_row_threshold = compaction_dead_row_threshold();
        if self.dead_row_count <= dead_row_threshold
            && self.dead_row_count.saturating_mul(4) <= self.rows.len()
        {
            return Ok(());
        }

        let mut compacted = Self::default();
        for row in self
            .rows
            .iter()
            .filter(|row| self.live_rows.contains(row.row_id))
        {
            compacted.insert_compact_row_from(self, row)?;
        }
        *self = compacted;
        Ok(())
    }

    fn insert_compact_row_from(
        &mut self,
        source: &Self,
        row: &RelationshipRow,
    ) -> Result<(), StoreError> {
        let row_id = RowId::from_len(self.rows.len())?;
        let compacted = RelationshipRow {
            row_id,
            resource_type: ObjectTypeId(self.interner.intern(source.resolve(row.resource_type.0))?),
            resource_id: ObjectIdId(self.interner.intern(source.resolve(row.resource_id.0))?),
            relation: RelationId(self.interner.intern(source.resolve(row.relation.0))?),
            subject_type: SubjectTypeId(self.interner.intern(source.resolve(row.subject_type.0))?),
            subject_id: SubjectIdId(self.interner.intern(source.resolve(row.subject_id.0))?),
            subject_relation: row
                .subject_relation
                .map(|relation| self.interner.intern(source.resolve(relation.0)))
                .transpose()?
                .map(RelationId),
        };
        let uniqueness = Self::ready_uniqueness_mut(&mut self.uniqueness)?;
        uniqueness.insert(&self.rows, row_id, &compacted);
        self.index_relationship(row_id, &compacted);
        self.rows.push(compacted);
        self.live_rows.insert(row_id);
        Ok(())
    }

    fn resolve(&self, id: SymbolId) -> &str {
        self.interner.resolve(id).unwrap_or("<invalid>")
    }

    fn uniqueness_ref(&self) -> Option<&RelationshipIdentityIndex> {
        match &self.uniqueness {
            UniquenessState::Ready(index) => Some(index),
            UniquenessState::KnownUniqueButNotIndexed | UniquenessState::UntrustedNotIndexed => {
                None
            }
        }
    }

    fn has_ready_uniqueness(&self) -> bool {
        matches!(self.uniqueness, UniquenessState::Ready(_))
    }

    fn ready_uniqueness_mut(
        uniqueness: &mut UniquenessState,
    ) -> Result<&mut RelationshipIdentityIndex, StoreError> {
        match uniqueness {
            UniquenessState::Ready(index) => Ok(index),
            UniquenessState::KnownUniqueButNotIndexed | UniquenessState::UntrustedNotIndexed => {
                Err(StoreError::InternalInvariant {
                    reason: "relationship uniqueness index was not initialized before mutation",
                })
            }
        }
    }
}

/// Short-lived compatibility reader over materialized domain relationships.
#[derive(Debug)]
pub struct MaterializedRelationshipReader<'a> {
    store: &'a IndexedRelationshipStore,
    rows: Vec<Relationship>,
}

impl RelationshipReader for MaterializedRelationshipReader<'_> {
    type Iter<'a>
        = RelationshipIter<'a>
    where
        Self: 'a;

    fn query_relationships(
        &self,
        filter: &RelationshipFilter,
    ) -> Result<Self::Iter<'_>, StoreError> {
        let matcher = self.store.resource_matcher(filter);
        let candidates = matcher.as_ref().map_or(CandidateRowIds::Empty, |matcher| {
            self.store.resource_candidate_row_ids(matcher)
        });
        Ok(RelationshipIter {
            rows: &self.rows,
            compact_rows: &self.store.rows,
            live_rows: &self.store.live_rows,
            all_rows_live: self.store.live_rows.is_all_live(),
            candidates,
            matcher: matcher.map(RelationshipMatcher::Resource),
        })
    }

    fn reverse_query_relationships(
        &self,
        filter: &SubjectFilter,
    ) -> Result<Self::Iter<'_>, StoreError> {
        let matcher = self.store.subject_matcher(filter);
        let candidates = matcher.as_ref().map_or(CandidateRowIds::Empty, |matcher| {
            self.store.subject_candidate_row_ids(matcher)
        });
        Ok(RelationshipIter {
            rows: &self.rows,
            compact_rows: &self.store.rows,
            live_rows: &self.store.live_rows,
            all_rows_live: self.store.live_rows.is_all_live(),
            candidates,
            matcher: matcher.map(RelationshipMatcher::Subject),
        })
    }
}

/// Iterator over indexed relationship query results.
#[derive(Debug)]
pub struct RelationshipIter<'a> {
    rows: &'a [Relationship],
    compact_rows: &'a [RelationshipRow],
    live_rows: &'a LiveRows,
    all_rows_live: bool,
    candidates: CandidateRowIds<'a>,
    matcher: Option<RelationshipMatcher>,
}

impl<'a> Iterator for RelationshipIter<'a> {
    type Item = &'a Relationship;

    fn next(&mut self) -> Option<Self::Item> {
        let matcher = self.matcher.as_mut()?;
        loop {
            let row_id = self.candidates.next()?;
            if !self.all_rows_live && !self.live_rows.contains(row_id) {
                continue;
            }
            let compact_row = self.compact_rows.get(row_id.index())?;
            let relationship = self.rows.get(row_id.index())?;
            match matcher {
                RelationshipMatcher::Resource(resource) => {
                    if resource.remaining == 0 {
                        return None;
                    }
                    if compact_row.matches_resource(resource) {
                        resource.remaining = resource.remaining.saturating_sub(1);
                        return Some(relationship);
                    }
                }
                RelationshipMatcher::Subject(subject) => {
                    if compact_row.matches_subject(subject) {
                        return Some(relationship);
                    }
                }
            }
        }
    }
}

#[derive(Debug)]
pub(crate) struct CompactRelationshipIter<'a> {
    store: &'a IndexedRelationshipStore,
    all_rows_live: bool,
    candidates: CandidateRowIds<'a>,
    matcher: Option<CompactRelationshipMatcher>,
}

impl<'a> Iterator for CompactRelationshipIter<'a> {
    type Item = RelationshipRef<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        let matcher = self.matcher.as_mut()?;
        loop {
            let row_id = self.candidates.next()?;
            if !self.all_rows_live && !self.store.live_rows.contains(row_id) {
                continue;
            }
            let row = self.store.rows.get(row_id.index())?;
            match matcher {
                CompactRelationshipMatcher::Resource(resource) => {
                    if resource.remaining == 0 {
                        return None;
                    }
                    if row.matches_resource(resource) {
                        resource.remaining = resource.remaining.saturating_sub(1);
                        return Some(RelationshipRef {
                            store: self.store,
                            row,
                        });
                    }
                }
                CompactRelationshipMatcher::Subject(subject) => {
                    if row.matches_subject(subject) {
                        return Some(RelationshipRef {
                            store: self.store,
                            row,
                        });
                    }
                }
            }
        }
    }
}

/// Borrowed compact relationship view used by evaluator hot paths.
#[derive(Debug, Clone, Copy)]
pub(crate) struct RelationshipRef<'a> {
    store: &'a IndexedRelationshipStore,
    row: &'a RelationshipRow,
}

impl RelationshipRef<'_> {
    fn is_deleted_by(&self, deleted: &HashSet<RelationshipRow>) -> bool {
        deleted.contains(self.row)
    }

    pub(crate) fn resource_object_legacy(&self) -> crate::model::Object {
        crate::model::Object {
            namespace: self.store.resolve(self.row.resource_type.0).to_string(),
            id: self.store.resolve(self.row.resource_id.0).to_string(),
        }
    }

    pub(crate) fn resource_type_eq(&self, expected: &ObjectType) -> bool {
        self.store.resolve(self.row.resource_type.0) == expected.as_str()
    }

    pub(crate) fn relation_name_eq(&self, expected: &RelationName) -> bool {
        self.store.resolve(self.row.relation.0) == expected.as_str()
    }

    pub(crate) fn relation_legacy(&self) -> crate::model::Relation {
        crate::model::Relation(self.store.resolve(self.row.relation.0).to_string())
    }

    pub(crate) fn direct_user_subject_id(&self) -> Option<&str> {
        if self.row.subject_relation.is_none()
            && self.store.resolve(self.row.subject_type.0) == "user"
        {
            Some(self.store.resolve(self.row.subject_id.0))
        } else {
            None
        }
    }

    pub(crate) fn subject_userset_relation_name(
        &self,
    ) -> Result<Option<(crate::model::Object, RelationName)>, ZanzibarError> {
        self.row
            .subject_relation
            .map(|relation| {
                Ok((
                    crate::model::Object {
                        namespace: self.store.resolve(self.row.subject_type.0).to_string(),
                        id: self.store.resolve(self.row.subject_id.0).to_string(),
                    },
                    RelationName::try_from(self.store.resolve(relation.0))?,
                ))
            })
            .transpose()
    }

    pub(crate) fn expanded_subject(&self) -> Result<crate::model::ExpandedUserset, ZanzibarError> {
        match self.row.subject_relation {
            Some(relation) => Ok(crate::model::ExpandedUserset::Userset(
                crate::model::Object {
                    namespace: self.store.resolve(self.row.subject_type.0).to_string(),
                    id: self.store.resolve(self.row.subject_id.0).to_string(),
                },
                crate::model::Relation(self.store.resolve(relation.0).to_string()),
            )),
            None if self.store.resolve(self.row.subject_type.0) == "user" => {
                Ok(crate::model::ExpandedUserset::User(
                    self.store.resolve(self.row.subject_id.0).to_string(),
                ))
            }
            None => Err(ZanzibarError::StorageError(format!(
                "legacy expand cannot represent direct subject type '{}'",
                self.store.resolve(self.row.subject_type.0),
            ))),
        }
    }
}

#[derive(Debug)]
enum CandidateRowIds<'a> {
    Empty,
    One(Option<RowId>),
    OneThenSlice {
        first: Option<RowId>,
        rest: std::slice::Iter<'a, RowId>,
    },
    Slice(std::slice::Iter<'a, RowId>),
}

impl Iterator for CandidateRowIds<'_> {
    type Item = RowId;

    fn next(&mut self) -> Option<Self::Item> {
        match self {
            Self::Empty => None,
            Self::One(row_id) => row_id.take(),
            Self::OneThenSlice { first, rest } => first.take().or_else(|| rest.next().copied()),
            Self::Slice(indexes) => indexes.next().copied(),
        }
    }
}

#[derive(Debug, Clone)]
enum PostingIndex<K> {
    Hash {
        primary: HashMap<K, RowId>,
        overflow: HashMap<K, Vec<RowId>>,
    },
    Sorted {
        keys: Vec<K>,
        ranges: Vec<RuntimePostingRange>,
        overflow: Vec<RowId>,
    },
}

impl<K> Default for PostingIndex<K> {
    fn default() -> Self {
        Self::Hash {
            primary: HashMap::new(),
            overflow: HashMap::new(),
        }
    }
}

impl<K> PostingIndex<K>
where
    K: Copy + Eq + Hash + Ord,
{
    fn from_sorted(keys: Vec<K>, ranges: Vec<RuntimePostingRange>, overflow: Vec<RowId>) -> Self {
        Self::Sorted {
            keys,
            ranges,
            overflow,
        }
    }

    fn insert(&mut self, key: K, row_id: RowId) {
        self.ensure_hash_profile();
        if let Self::Hash { primary, overflow } = self {
            match primary.entry(key) {
                Entry::Vacant(entry) => {
                    entry.insert(row_id);
                }
                Entry::Occupied(_) => {
                    overflow.entry(key).or_default().push(row_id);
                }
            }
        }
    }

    fn candidates(&self, key: &K) -> CandidateRowIds<'_> {
        match self {
            Self::Hash { primary, overflow } => {
                match (primary.get(key).copied(), overflow.get(key)) {
                    (Some(row_id), Some(row_ids)) => CandidateRowIds::OneThenSlice {
                        first: Some(row_id),
                        rest: row_ids.iter(),
                    },
                    (Some(row_id), None) => CandidateRowIds::One(Some(row_id)),
                    (None, Some(row_ids)) => CandidateRowIds::Slice(row_ids.iter()),
                    (None, None) => CandidateRowIds::Empty,
                }
            }
            Self::Sorted {
                keys,
                ranges,
                overflow,
            } => match keys.binary_search(key) {
                Ok(index) => ranges
                    .get(index)
                    .map_or(CandidateRowIds::Empty, |range| range.candidates(overflow)),
                Err(_) => CandidateRowIds::Empty,
            },
        }
    }

    fn ensure_hash_profile(&mut self) {
        let Self::Sorted {
            keys,
            ranges,
            overflow,
        } = self
        else {
            return;
        };
        let mut primary = HashMap::with_capacity(keys.len());
        let mut overflow_map = HashMap::new();
        for (key, range) in keys.iter().copied().zip(ranges.iter().copied()) {
            primary.insert(key, range.first_row_id);
            if let Some(row_ids) = range.overflow_slice(overflow) {
                overflow_map.insert(key, row_ids.to_vec());
            }
        }
        *self = Self::Hash {
            primary,
            overflow: overflow_map,
        };
    }
}

#[derive(Debug, Clone, Copy)]
struct RuntimePostingRange {
    first_row_id: RowId,
    overflow_start: u32,
    overflow_len: u32,
}

impl RuntimePostingRange {
    fn candidates<'a>(&self, overflow: &'a [RowId]) -> CandidateRowIds<'a> {
        match self.overflow_slice(overflow) {
            Some(row_ids) => CandidateRowIds::OneThenSlice {
                first: Some(self.first_row_id),
                rest: row_ids.iter(),
            },
            None => CandidateRowIds::One(Some(self.first_row_id)),
        }
    }

    fn overflow_slice<'a>(&self, overflow: &'a [RowId]) -> Option<&'a [RowId]> {
        if self.overflow_len == 0 {
            return None;
        }
        let start = usize::try_from(self.overflow_start).ok()?;
        let len = usize::try_from(self.overflow_len).ok()?;
        let end = start.checked_add(len)?;
        overflow.get(start..end)
    }
}

#[derive(Debug)]
enum RelationshipMatcher {
    Resource(ResourceMatcher),
    Subject(SubjectMatcher),
}

#[derive(Debug)]
enum CompactRelationshipMatcher {
    Resource(ResourceMatcher),
    Subject(SubjectMatcher),
}

#[derive(Debug, Clone, Copy)]
struct ResourceMatcher {
    resource_type: ObjectTypeId,
    optional_resource_id: Option<ObjectIdId>,
    optional_relation: Option<RelationId>,
    optional_subject: Option<SubjectMatcher>,
    remaining: usize,
}

#[derive(Debug, Clone, Copy)]
struct SubjectMatcher {
    subject_type: SubjectTypeId,
    optional_subject_id: Option<SubjectIdId>,
    optional_relation: Option<RelationId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
struct SymbolId(NonZeroU32);

impl SymbolId {
    fn from_index(index: usize) -> Result<Self, StoreError> {
        let value = index
            .checked_add(1)
            .and_then(|value| u32::try_from(value).ok())
            .and_then(NonZeroU32::new)
            .ok_or(StoreError::CapacityExceeded {
                component: "identifier interner",
            })?;
        Ok(Self(value))
    }

    fn from_snapshot_raw(value: u32, symbol_count: u32) -> Result<Self, SnapshotIoError> {
        let value = NonZeroU32::new(value).ok_or(SnapshotIoError::Format {
            reason: "symbol id must be non-zero",
        })?;
        if value.get() > symbol_count {
            return Err(SnapshotIoError::Format {
                reason: "symbol id is out of bounds",
            });
        }
        Ok(Self(value))
    }

    fn get(self) -> u32 {
        self.0.get()
    }

    fn index(self) -> usize {
        usize::try_from(self.0.get().saturating_sub(1)).unwrap_or(usize::MAX)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
struct ObjectTypeId(SymbolId);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
struct ObjectIdId(SymbolId);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
struct RelationId(SymbolId);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
struct SubjectTypeId(SymbolId);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
struct SubjectIdId(SymbolId);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
struct RowId(NonZeroU32);

impl RowId {
    fn from_len(len: usize) -> Result<Self, StoreError> {
        let value = len
            .checked_add(1)
            .and_then(|value| u32::try_from(value).ok())
            .and_then(NonZeroU32::new)
            .ok_or(StoreError::CapacityExceeded {
                component: "relationship rows",
            })?;
        Ok(Self(value))
    }

    fn from_snapshot_raw(value: u32, row_count: u32) -> Result<Self, SnapshotIoError> {
        let value = NonZeroU32::new(value).ok_or(SnapshotIoError::Format {
            reason: "row id must be non-zero",
        })?;
        if value.get() > row_count {
            return Err(SnapshotIoError::Format {
                reason: "row id is out of bounds",
            });
        }
        Ok(Self(value))
    }

    const fn raw(self) -> u32 {
        self.0.get()
    }

    fn index(self) -> usize {
        usize::try_from(self.0.get().saturating_sub(1)).unwrap_or(usize::MAX)
    }
}

#[derive(Debug, Clone, Default)]
struct IdentifierInterner {
    bytes: Vec<u8>,
    entries: Vec<InternedString>,
    ids_by_hash: HashMap<u64, SymbolId>,
    hash_collisions: HashMap<u64, Vec<SymbolId>>,
    hashes_by_id: Vec<u64>,
    sorted_lookup: Vec<SymbolId>,
}

impl IdentifierInterner {
    fn encode_symbol_acceleration(
        &self,
        layout: SnapshotEncodingLayout,
    ) -> Result<(Vec<u8>, Vec<u8>, u16), SnapshotIoError> {
        let mut hashes_by_id = Vec::with_capacity(self.entries.len());
        for index in 0..self.entries.len() {
            let id = SymbolId::from_index(index)?;
            hashes_by_id.push(hash_value(self.resolve(id)?));
        }
        let mut hashes =
            Vec::with_capacity(checked_mul_usize(hashes_by_id.len(), DISK_SYMBOL_HASH_LEN)?);
        for hash in &hashes_by_id {
            hashes.extend_from_slice(&hash.to_le_bytes());
        }

        let mut lookup = (0..self.entries.len())
            .map(SymbolId::from_index)
            .collect::<Result<Vec<_>, _>>()?;
        lookup.sort_by_key(|id| (hashes_by_id.get(id.index()).copied(), *id));
        let lookup_width =
            snapshot_symbol_width(checked_u32_from_usize(self.entries.len())?, layout);
        let mut lookup_bytes =
            Vec::with_capacity(checked_mul_usize(lookup.len(), lookup_width.byte_len())?);
        for id in lookup {
            lookup_width.encode_value(id.get(), &mut lookup_bytes);
        }
        Ok((hashes, lookup_bytes, lookup_width.flag_bits()))
    }

    fn decode_snapshot_sections(
        reader: &SnapshotReader<'_>,
        validation: SnapshotValidationMode,
    ) -> Result<Self, SnapshotIoError> {
        let header = reader.header();
        let bytes = reader.section(SectionKind::SymbolBytes)?;
        let table = reader.section(SectionKind::SymbolTable)?;
        let symbol_count = checked_usize_from_u32(header.symbol_count)?;
        if table.row_count() != u64::from(header.symbol_count) {
            return Err(SnapshotIoError::Format {
                reason: "symbol table row count does not match header",
            });
        }
        let (start_width, len_width) = if header.format_version == SnapshotFormatVersion::V3 {
            symbol_table_widths(table.flags())?
        } else {
            if table.flags() != 0 {
                return Err(SnapshotIoError::Format {
                    reason: "symbol table flags are unsupported",
                });
            }
            (SnapshotKeyWidth::U32, SnapshotKeyWidth::U32)
        };
        let expected_len = checked_mul_usize(
            symbol_count,
            checked_add_usize(start_width.byte_len(), len_width.byte_len())?,
        )?;
        if table.bytes().len() != expected_len {
            return Err(SnapshotIoError::Format {
                reason: "symbol table length does not match symbol count",
            });
        }

        let mut cursor = BinaryCursor::new(table.bytes());
        let mut entries = Vec::with_capacity(symbol_count);
        for _ in 0..symbol_count {
            let start = start_width.read_value(&mut cursor)?;
            let len = len_width.read_value(&mut cursor)?;
            let start_usize = checked_usize_from_u32(start)?;
            let len_usize = checked_usize_from_u32(len)?;
            let end = checked_add_usize(start_usize, len_usize)?;
            let symbol_bytes =
                bytes
                    .bytes()
                    .get(start_usize..end)
                    .ok_or(SnapshotIoError::Format {
                        reason: "symbol byte range is out of bounds",
                    })?;
            str::from_utf8(symbol_bytes).map_err(|_| SnapshotIoError::Format {
                reason: "symbol bytes are not valid utf-8",
            })?;
            entries.push(InternedString { start, len });
        }
        if !cursor.is_empty() {
            return Err(SnapshotIoError::Format {
                reason: "symbol table has trailing bytes",
            });
        }
        let hashes_by_id = decode_symbol_hashes(reader, header.symbol_count)?;
        let lookup = decode_symbol_lookup(reader, header.symbol_count, &hashes_by_id)?;
        Self::from_snapshot_parts(
            bytes.bytes().to_vec(),
            entries,
            hashes_by_id,
            lookup,
            validation,
        )
    }

    fn from_snapshot_parts(
        bytes: Vec<u8>,
        entries: Vec<InternedString>,
        hashes_by_id: Vec<u64>,
        lookup: Vec<SymbolId>,
        validation: SnapshotValidationMode,
    ) -> Result<Self, SnapshotIoError> {
        let mut interner = Self {
            bytes,
            entries,
            ids_by_hash: HashMap::new(),
            hash_collisions: HashMap::new(),
            hashes_by_id: Vec::new(),
            sorted_lookup: Vec::new(),
        };
        match validation {
            SnapshotValidationMode::Full => {
                let (ids_by_hash, hash_collisions) =
                    index_full_symbol_lookup(&interner.bytes, &interner.entries, &hashes_by_id)?;
                interner.ids_by_hash = ids_by_hash;
                interner.hash_collisions = hash_collisions;
            }
            SnapshotValidationMode::TrustedFastLoad => {
                interner.hashes_by_id = hashes_by_id;
                interner.sorted_lookup = lookup;
            }
        }
        Ok(interner)
    }

    fn intern(&mut self, value: &str) -> Result<SymbolId, StoreError> {
        if let Some(id) = self.lookup(value) {
            return Ok(id);
        }
        let id = SymbolId::from_index(self.entries.len())?;
        let hash = hash_value(value);
        if self.sorted_lookup.is_empty() {
            if let Some(existing) = self.ids_by_hash.get(&hash).copied() {
                if self
                    .resolve(existing)
                    .ok()
                    .is_some_and(|stored| stored == value)
                {
                    return Ok(existing);
                }
                self.hash_collisions.entry(hash).or_default().push(id);
            } else {
                self.ids_by_hash.insert(hash, id);
            }
        } else {
            self.hashes_by_id.push(hash);
            let insert_at = self.sorted_lookup.partition_point(|candidate| {
                match self.symbol_hash(*candidate) {
                    Some(candidate_hash) => (candidate_hash, *candidate) < (hash, id),
                    None => false,
                }
            });
            self.sorted_lookup.insert(insert_at, id);
        }
        let start = u32::try_from(self.bytes.len()).map_err(|_| StoreError::CapacityExceeded {
            component: "identifier interner bytes",
        })?;
        let len = u32::try_from(value.len()).map_err(|_| StoreError::CapacityExceeded {
            component: "identifier interner string",
        })?;
        let end =
            self.bytes
                .len()
                .checked_add(value.len())
                .ok_or(StoreError::CapacityExceeded {
                    component: "identifier interner bytes",
                })?;
        u32::try_from(end).map_err(|_| StoreError::CapacityExceeded {
            component: "identifier interner bytes",
        })?;
        self.bytes.extend_from_slice(value.as_bytes());
        self.entries.push(InternedString { start, len });
        Ok(id)
    }

    fn lookup(&self, value: &str) -> Option<SymbolId> {
        let hash = hash_value(value);
        if !self.sorted_lookup.is_empty() {
            let start = self
                .sorted_lookup
                .partition_point(|id| self.symbol_hash(*id).is_some_and(|stored| stored < hash));
            for id in self.sorted_lookup.get(start..)?.iter().copied() {
                if self.symbol_hash(id) != Some(hash) {
                    break;
                }
                if self.resolve(id).ok().is_some_and(|stored| stored == value) {
                    return Some(id);
                }
            }
            return None;
        }
        if let Some(id) = self.ids_by_hash.get(&hash).copied()
            && self.resolve(id).ok().is_some_and(|stored| stored == value)
        {
            return Some(id);
        }
        self.hash_collisions.get(&hash).and_then(|ids| {
            ids.iter()
                .copied()
                .find(|id| self.resolve(*id).ok().is_some_and(|stored| stored == value))
        })
    }

    fn resolve(&self, id: SymbolId) -> Result<&str, StoreError> {
        let entry = self
            .entries
            .get(id.index())
            .ok_or(StoreError::InternalInvariant {
                reason: "interned identifier id is out of bounds",
            })?;
        let start = usize::try_from(entry.start).map_err(|_| StoreError::InternalInvariant {
            reason: "interned identifier start offset is out of bounds",
        })?;
        let len = usize::try_from(entry.len).map_err(|_| StoreError::InternalInvariant {
            reason: "interned identifier length is out of bounds",
        })?;
        let end = start
            .checked_add(len)
            .ok_or(StoreError::InternalInvariant {
                reason: "interned identifier byte range overflowed",
            })?;
        let bytes = self
            .bytes
            .get(start..end)
            .ok_or(StoreError::InternalInvariant {
                reason: "interned identifier byte range is out of bounds",
            })?;
        str::from_utf8(bytes).map_err(|_| StoreError::InternalInvariant {
            reason: "interned identifier bytes are not valid utf-8",
        })
    }

    fn len(&self) -> usize {
        self.entries.len()
    }

    fn byte_len(&self) -> usize {
        self.bytes.len()
    }

    fn symbol_hash(&self, id: SymbolId) -> Option<u64> {
        self.hashes_by_id.get(id.index()).copied()
    }
}

#[derive(Debug, Clone, Copy)]
struct InternedString {
    start: u32,
    len: u32,
}

fn encode_v3_symbol_table(
    entries: &[InternedString],
    symbol_bytes_len: u32,
    layout: SnapshotEncodingLayout,
) -> Result<(Vec<u8>, u16), SnapshotIoError> {
    let start_width = snapshot_symbol_width(symbol_bytes_len, layout);
    let max_len = entries.iter().map(|entry| entry.len).max().unwrap_or(0);
    let len_width = snapshot_symbol_width(max_len, layout);
    let entry_width = checked_add_usize(start_width.byte_len(), len_width.byte_len())?;
    let mut table = Vec::with_capacity(checked_mul_usize(entries.len(), entry_width)?);
    for entry in entries {
        start_width.encode_value(entry.start, &mut table);
        len_width.encode_value(entry.len, &mut table);
    }
    Ok((table, symbol_table_flags(start_width, len_width)))
}

const fn snapshot_symbol_width(max_value: u32, layout: SnapshotEncodingLayout) -> SnapshotKeyWidth {
    match layout {
        SnapshotEncodingLayout::Compact => SnapshotKeyWidth::for_max(max_value),
        SnapshotEncodingLayout::CompressionFriendly => SnapshotKeyWidth::U32,
    }
}

fn decode_symbol_hashes(
    reader: &SnapshotReader<'_>,
    symbol_count: u32,
) -> Result<Vec<u64>, SnapshotIoError> {
    let section = reader.section(SectionKind::SymbolHashes)?;
    if section.row_count() != u64::from(symbol_count) {
        return Err(SnapshotIoError::Format {
            reason: "symbol hash row count does not match header",
        });
    }
    let count = checked_usize_from_u32(symbol_count)?;
    let expected_len = checked_mul_usize(count, DISK_SYMBOL_HASH_LEN)?;
    if section.bytes().len() != expected_len {
        return Err(SnapshotIoError::Format {
            reason: "symbol hash length does not match symbol count",
        });
    }

    let mut cursor = BinaryCursor::new(section.bytes());
    let mut hashes = Vec::with_capacity(count);
    for _ in 0..count {
        hashes.push(cursor.read_u64()?);
    }
    Ok(hashes)
}

fn decode_symbol_lookup(
    reader: &SnapshotReader<'_>,
    symbol_count: u32,
    hashes_by_id: &[u64],
) -> Result<Vec<SymbolId>, SnapshotIoError> {
    let section = reader.section(SectionKind::SymbolLookup)?;
    if section.row_count() != u64::from(symbol_count) {
        return Err(SnapshotIoError::Format {
            reason: "symbol lookup row count does not match header",
        });
    }
    let count = checked_usize_from_u32(symbol_count)?;
    let symbol_width = if reader.header().format_version == SnapshotFormatVersion::V3 {
        section_width_from_flags(section.flags())?
    } else {
        if section.flags() != 0 {
            return Err(SnapshotIoError::Format {
                reason: "symbol lookup flags are unsupported",
            });
        }
        SnapshotKeyWidth::U32
    };
    let expected_len = checked_mul_usize(count, symbol_width.byte_len())?;
    if section.bytes().len() != expected_len {
        return Err(SnapshotIoError::Format {
            reason: "symbol lookup length does not match symbol count",
        });
    }

    let mut cursor = BinaryCursor::new(section.bytes());
    let mut lookup = Vec::with_capacity(count);
    let mut previous = None;
    for _ in 0..count {
        let id = SymbolId::from_snapshot_raw(symbol_width.read_value(&mut cursor)?, symbol_count)?;
        let key = symbol_lookup_key(id, hashes_by_id)?;
        if previous.is_some_and(|candidate| candidate >= key) {
            return Err(SnapshotIoError::Format {
                reason: "symbol lookup is not strictly sorted",
            });
        }
        previous = Some(key);
        lookup.push(id);
    }
    if !cursor.is_empty() {
        return Err(SnapshotIoError::Format {
            reason: "symbol lookup has trailing bytes",
        });
    }
    Ok(lookup)
}

fn symbol_lookup_key(
    id: SymbolId,
    hashes_by_id: &[u64],
) -> Result<(u64, SymbolId), SnapshotIoError> {
    let hash = hashes_by_id
        .get(id.index())
        .copied()
        .ok_or(SnapshotIoError::Format {
            reason: "symbol lookup hash id is out of bounds",
        })?;
    Ok((hash, id))
}

type SnapshotSymbolIndex = (HashMap<u64, SymbolId>, HashMap<u64, Vec<SymbolId>>);

fn index_full_symbol_lookup(
    bytes: &[u8],
    entries: &[InternedString],
    hashes_by_id: &[u64],
) -> Result<SnapshotSymbolIndex, SnapshotIoError> {
    let mut ids_by_hash = HashMap::new();
    let mut hash_collisions = HashMap::new();
    for (index, stored_hash) in hashes_by_id.iter().copied().enumerate() {
        let id = SymbolId::from_index(index)?;
        let value = resolve_symbol_entry(bytes, entries, id)?;
        if hash_value(value) != stored_hash {
            return Err(SnapshotIoError::Format {
                reason: "symbol lookup hash does not match symbol bytes",
            });
        }
        if symbol_value_exists(
            bytes,
            entries,
            &ids_by_hash,
            &hash_collisions,
            stored_hash,
            value,
        )? {
            return Err(SnapshotIoError::Format {
                reason: "duplicate symbol in snapshot",
            });
        }
        match ids_by_hash.entry(stored_hash) {
            Entry::Vacant(vacant) => {
                vacant.insert(id);
            }
            Entry::Occupied(_) => {
                hash_collisions
                    .entry(stored_hash)
                    .or_insert_with(Vec::new)
                    .push(id);
            }
        }
    }
    Ok((ids_by_hash, hash_collisions))
}

fn symbol_value_exists(
    bytes: &[u8],
    entries: &[InternedString],
    ids_by_hash: &HashMap<u64, SymbolId>,
    hash_collisions: &HashMap<u64, Vec<SymbolId>>,
    hash: u64,
    value: &str,
) -> Result<bool, SnapshotIoError> {
    if let Some(id) = ids_by_hash.get(&hash).copied()
        && resolve_symbol_entry(bytes, entries, id)? == value
    {
        return Ok(true);
    }
    if let Some(ids) = hash_collisions.get(&hash) {
        for id in ids {
            if resolve_symbol_entry(bytes, entries, *id)? == value {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

fn resolve_symbol_entry<'a>(
    bytes: &'a [u8],
    entries: &[InternedString],
    id: SymbolId,
) -> Result<&'a str, SnapshotIoError> {
    let entry = entries.get(id.index()).ok_or(SnapshotIoError::Format {
        reason: "symbol lookup id is out of bounds",
    })?;
    let start = checked_usize_from_u32(entry.start)?;
    let len = checked_usize_from_u32(entry.len)?;
    let end = checked_add_usize(start, len)?;
    let value = bytes.get(start..end).ok_or(SnapshotIoError::Format {
        reason: "symbol byte range is out of bounds",
    })?;
    str::from_utf8(value).map_err(|_| SnapshotIoError::Format {
        reason: "symbol bytes are not valid utf-8",
    })
}

#[derive(Debug, Clone, Default)]
struct RelationshipIdentityIndex {
    primary: HashMap<u64, RowId>,
    hash_collisions: HashMap<u64, Vec<RowId>>,
}

impl RelationshipIdentityIndex {
    fn find(&self, rows: &[RelationshipRow], row: &RelationshipRow) -> Option<RowId> {
        let hash = row.identity_hash();
        if let Some(row_id) = self.primary.get(&hash).copied()
            && rows
                .get(row_id.index())
                .is_some_and(|candidate| candidate == row)
        {
            return Some(row_id);
        }
        self.hash_collisions.get(&hash).and_then(|row_ids| {
            row_ids.iter().copied().find(|row_id| {
                rows.get(row_id.index())
                    .is_some_and(|candidate| candidate == row)
            })
        })
    }

    fn insert(&mut self, rows: &[RelationshipRow], row_id: RowId, row: &RelationshipRow) {
        let hash = row.identity_hash();
        match self.primary.get(&hash).copied() {
            Some(existing)
                if rows
                    .get(existing.index())
                    .is_some_and(|candidate| candidate != row) =>
            {
                self.hash_collisions.entry(hash).or_default().push(row_id);
            }
            Some(_) => {}
            None => {
                self.primary.insert(hash, row_id);
            }
        }
    }

    fn remove(&mut self, rows: &[RelationshipRow], row: &RelationshipRow) -> Option<RowId> {
        let hash = row.identity_hash();
        if let Some(row_id) = self.primary.get(&hash).copied()
            && rows
                .get(row_id.index())
                .is_some_and(|candidate| candidate == row)
        {
            let removed = self.primary.remove(&hash)?;
            if let Some(collisions) = self.hash_collisions.get_mut(&hash)
                && let Some(promoted) = collisions.pop()
            {
                self.primary.insert(hash, promoted);
                if collisions.is_empty() {
                    self.hash_collisions.remove(&hash);
                }
            }
            return Some(removed);
        }

        let collisions = self.hash_collisions.get_mut(&hash)?;
        let index = collisions.iter().position(|row_id| {
            rows.get(row_id.index())
                .is_some_and(|candidate| candidate == row)
        })?;
        let removed = collisions.swap_remove(index);
        if collisions.is_empty() {
            self.hash_collisions.remove(&hash);
        }
        Some(removed)
    }
}

#[derive(Debug, Clone)]
enum UniquenessState {
    Ready(RelationshipIdentityIndex),
    KnownUniqueButNotIndexed,
    UntrustedNotIndexed,
}

#[derive(Debug, Clone)]
struct RelationshipRow {
    row_id: RowId,
    resource_type: ObjectTypeId,
    resource_id: ObjectIdId,
    relation: RelationId,
    subject_type: SubjectTypeId,
    subject_id: SubjectIdId,
    subject_relation: Option<RelationId>,
}

impl PartialEq for RelationshipRow {
    fn eq(&self, other: &Self) -> bool {
        self.resource_type == other.resource_type
            && self.resource_id == other.resource_id
            && self.relation == other.relation
            && self.subject_type == other.subject_type
            && self.subject_id == other.subject_id
            && self.subject_relation == other.subject_relation
    }
}

impl Eq for RelationshipRow {}

impl Hash for RelationshipRow {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.resource_type.hash(state);
        self.resource_id.hash(state);
        self.relation.hash(state);
        self.subject_type.hash(state);
        self.subject_id.hash(state);
        self.subject_relation.hash(state);
    }
}

impl RelationshipRow {
    fn identity_hash(&self) -> u64 {
        let mut state = DefaultHasher::new();
        self.hash(&mut state);
        state.finish()
    }

    fn from_relationship(
        row_id: RowId,
        relationship: &Relationship,
        interner: &mut IdentifierInterner,
    ) -> Result<Self, StoreError> {
        let resource_type =
            ObjectTypeId(interner.intern(relationship.resource().object_type().as_str())?);
        let resource_id =
            ObjectIdId(interner.intern(relationship.resource().object_id().as_str())?);
        let relation = RelationId(interner.intern(relationship.relation().as_str())?);
        let (subject_type, subject_id, subject_relation) = match relationship.subject() {
            SubjectRef::Object(object) => (
                SubjectTypeId(interner.intern(object.object_type().as_str())?),
                SubjectIdId(interner.intern(object.object_id().as_str())?),
                None,
            ),
            SubjectRef::Userset { object, relation } => (
                SubjectTypeId(interner.intern(object.object_type().as_str())?),
                SubjectIdId(interner.intern(object.object_id().as_str())?),
                Some(RelationId(interner.intern(relation.as_str())?)),
            ),
        };
        Ok(Self {
            row_id,
            resource_type,
            resource_id,
            relation,
            subject_type,
            subject_id,
            subject_relation,
        })
    }

    fn from_existing_relationship(
        relationship: &Relationship,
        interner: &IdentifierInterner,
    ) -> Option<Self> {
        let resource_type =
            ObjectTypeId(interner.lookup(relationship.resource().object_type().as_str())?);
        let resource_id =
            ObjectIdId(interner.lookup(relationship.resource().object_id().as_str())?);
        let relation = RelationId(interner.lookup(relationship.relation().as_str())?);
        let (subject_type, subject_id, subject_relation) = match relationship.subject() {
            SubjectRef::Object(object) => (
                SubjectTypeId(interner.lookup(object.object_type().as_str())?),
                SubjectIdId(interner.lookup(object.object_id().as_str())?),
                None,
            ),
            SubjectRef::Userset { object, relation } => (
                SubjectTypeId(interner.lookup(object.object_type().as_str())?),
                SubjectIdId(interner.lookup(object.object_id().as_str())?),
                Some(RelationId(interner.lookup(relation.as_str())?)),
            ),
        };
        Some(Self {
            row_id: RowId(NonZeroU32::MIN),
            resource_type,
            resource_id,
            relation,
            subject_type,
            subject_id,
            subject_relation,
        })
    }

    fn matches_resource(&self, matcher: &ResourceMatcher) -> bool {
        self.resource_type == matcher.resource_type
            && matcher
                .optional_resource_id
                .is_none_or(|resource_id| self.resource_id == resource_id)
            && matcher
                .optional_relation
                .is_none_or(|relation| self.relation == relation)
            && matcher
                .optional_subject
                .is_none_or(|subject| self.matches_subject(&subject))
    }

    fn matches_subject(&self, matcher: &SubjectMatcher) -> bool {
        self.subject_type == matcher.subject_type
            && matcher
                .optional_subject_id
                .is_none_or(|subject_id| self.subject_id == subject_id)
            && matcher
                .optional_relation
                .is_none_or(|relation| self.subject_relation == Some(relation))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
struct ResourceIndexKey {
    object_type: ObjectTypeId,
    object_id: ObjectIdId,
    relation: RelationId,
}

impl From<&RelationshipRow> for ResourceIndexKey {
    fn from(value: &RelationshipRow) -> Self {
        Self {
            object_type: value.resource_type,
            object_id: value.resource_id,
            relation: value.relation,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
struct ResourceObjectIndexKey {
    object_type: ObjectTypeId,
    object_id: ObjectIdId,
}

impl From<&RelationshipRow> for ResourceObjectIndexKey {
    fn from(value: &RelationshipRow) -> Self {
        Self {
            object_type: value.resource_type,
            object_id: value.resource_id,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
struct ResourceTypeRelationIndexKey {
    object_type: ObjectTypeId,
    relation: RelationId,
}

impl From<&RelationshipRow> for ResourceTypeRelationIndexKey {
    fn from(value: &RelationshipRow) -> Self {
        Self {
            object_type: value.resource_type,
            relation: value.relation,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
struct SubjectIndexKey {
    subject_type: SubjectTypeId,
    subject_id: SubjectIdId,
    relation: Option<RelationId>,
}

impl SubjectIndexKey {
    fn from_row(row: &RelationshipRow) -> Vec<Self> {
        match row.subject_relation {
            Some(relation) => vec![
                Self {
                    subject_type: row.subject_type,
                    subject_id: row.subject_id,
                    relation: Some(relation),
                },
                Self {
                    subject_type: row.subject_type,
                    subject_id: row.subject_id,
                    relation: None,
                },
            ],
            None => vec![Self {
                subject_type: row.subject_type,
                subject_id: row.subject_id,
                relation: None,
            }],
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
struct SubjectTypeRelationIndexKey {
    subject_type: SubjectTypeId,
    relation: RelationId,
}

impl SubjectTypeRelationIndexKey {
    fn from_row(row: &RelationshipRow) -> Option<Self> {
        row.subject_relation.map(|relation| Self {
            subject_type: row.subject_type,
            relation,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
struct DiskRelationshipRow {
    resource_type: u32,
    resource_id: u32,
    relation: u32,
    subject_type: u32,
    subject_id: u32,
    subject_relation: u32,
}

impl DiskRelationshipRow {
    fn encode_width(&self, width: SnapshotKeyWidth, target: &mut Vec<u8>) {
        width.encode_value(self.resource_type, target);
        width.encode_value(self.resource_id, target);
        width.encode_value(self.relation, target);
        width.encode_value(self.subject_type, target);
        width.encode_value(self.subject_id, target);
        width.encode_value(self.subject_relation, target);
    }
}

impl From<&RelationshipRow> for DiskRelationshipRow {
    fn from(value: &RelationshipRow) -> Self {
        Self {
            resource_type: value.resource_type.0.get(),
            resource_id: value.resource_id.0.get(),
            relation: value.relation.0.get(),
            subject_type: value.subject_type.0.get(),
            subject_id: value.subject_id.0.get(),
            subject_relation: value
                .subject_relation
                .map_or(0, |relation| relation.0.get()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
struct DiskIndexKey {
    first: u32,
    second: u32,
    third: u32,
}

impl DiskIndexKey {
    fn max_field(self, field_count: usize) -> Result<u32, SnapshotIoError> {
        match field_count {
            1 => Ok(self.first),
            2 => Ok(self.first.max(self.second)),
            3 => Ok(self.first.max(self.second).max(self.third)),
            _ => Err(SnapshotIoError::Format {
                reason: "index key field count is unsupported",
            }),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SnapshotKeyWidth {
    U8,
    U16,
    U24,
    U32,
}

impl SnapshotKeyWidth {
    const fn byte_len(self) -> usize {
        match self {
            Self::U8 => 1,
            Self::U16 => 2,
            Self::U24 => 3,
            Self::U32 => 4,
        }
    }

    const fn flag_bits(self) -> u16 {
        match self {
            Self::U8 => 0,
            Self::U16 => 1,
            Self::U24 => 2,
            Self::U32 => 3,
        }
    }

    fn from_flag_bits(value: u16) -> Result<Self, SnapshotIoError> {
        match value {
            0 => Ok(Self::U8),
            1 => Ok(Self::U16),
            2 => Ok(Self::U24),
            3 => Ok(Self::U32),
            _ => Err(SnapshotIoError::Format {
                reason: "snapshot index key width is unsupported",
            }),
        }
    }

    const fn for_max(value: u32) -> Self {
        if value <= u8::MAX as u32 {
            Self::U8
        } else if value <= u16::MAX as u32 {
            Self::U16
        } else if value <= 0x00FF_FFFF {
            Self::U24
        } else {
            Self::U32
        }
    }

    fn encode_value(self, value: u32, target: &mut Vec<u8>) {
        match self {
            Self::U8 => target.extend(value.to_le_bytes().into_iter().take(1)),
            Self::U16 => target.extend(value.to_le_bytes().into_iter().take(2)),
            Self::U24 => target.extend(value.to_le_bytes().into_iter().take(3)),
            Self::U32 => target.extend_from_slice(&value.to_le_bytes()),
        }
    }

    fn read_value(self, cursor: &mut BinaryCursor<'_>) -> Result<u32, SnapshotIoError> {
        match self {
            Self::U8 => {
                let [value] = cursor.read_array::<1>()?;
                Ok(u32::from(value))
            }
            Self::U16 => Ok(u32::from(u16::from_le_bytes(cursor.read_array::<2>()?))),
            Self::U24 => {
                let [first, second, third] = cursor.read_array::<3>()?;
                Ok(u32::from_le_bytes([first, second, third, 0]))
            }
            Self::U32 => Ok(u32::from_le_bytes(cursor.read_array::<4>()?)),
        }
    }
}

fn section_width_from_flags(flags: u16) -> Result<SnapshotKeyWidth, SnapshotIoError> {
    if flags & !SECTION_WIDTH_MASK != 0 {
        return Err(SnapshotIoError::Format {
            reason: "section width flags are unsupported",
        });
    }
    SnapshotKeyWidth::from_flag_bits(flags & SECTION_WIDTH_MASK)
}

fn symbol_table_flags(start_width: SnapshotKeyWidth, len_width: SnapshotKeyWidth) -> u16 {
    start_width.flag_bits() | (len_width.flag_bits() << SYMBOL_TABLE_LEN_WIDTH_SHIFT)
}

fn symbol_table_widths(
    flags: u16,
) -> Result<(SnapshotKeyWidth, SnapshotKeyWidth), SnapshotIoError> {
    if flags & !(SECTION_WIDTH_MASK | SYMBOL_TABLE_LEN_WIDTH_MASK) != 0 {
        return Err(SnapshotIoError::Format {
            reason: "symbol table flags are unsupported",
        });
    }
    let start_width = SnapshotKeyWidth::from_flag_bits(flags & SECTION_WIDTH_MASK)?;
    let len_width = SnapshotKeyWidth::from_flag_bits(
        (flags & SYMBOL_TABLE_LEN_WIDTH_MASK) >> SYMBOL_TABLE_LEN_WIDTH_SHIFT,
    )?;
    Ok((start_width, len_width))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SnapshotIndexEncoding {
    FixedV2,
    CompactV3 { key_width: SnapshotKeyWidth },
}

impl SnapshotIndexEncoding {
    const fn flags(self) -> u16 {
        match self {
            Self::FixedV2 => 0,
            Self::CompactV3 { key_width } => {
                INDEX_DIRECTORY_FLAG_V3_COMPACT
                    | (key_width.flag_bits() << INDEX_DIRECTORY_KEY_WIDTH_SHIFT)
            }
        }
    }

    fn from_flags(flags: u16) -> Result<Self, SnapshotIoError> {
        if flags == 0 {
            return Ok(Self::FixedV2);
        }
        if flags & INDEX_DIRECTORY_FLAG_V3_COMPACT == 0 {
            return Err(SnapshotIoError::Format {
                reason: "index directory flags are unsupported",
            });
        }
        let known = INDEX_DIRECTORY_FLAG_V3_COMPACT | INDEX_DIRECTORY_KEY_WIDTH_MASK;
        if flags & !known != 0 {
            return Err(SnapshotIoError::Format {
                reason: "index directory flags are unsupported",
            });
        }
        let width_bits =
            (flags & INDEX_DIRECTORY_KEY_WIDTH_MASK) >> INDEX_DIRECTORY_KEY_WIDTH_SHIFT;
        Ok(Self::CompactV3 {
            key_width: SnapshotKeyWidth::from_flag_bits(width_bits)?,
        })
    }
}

#[derive(Debug, Clone, Copy)]
struct DiskPostingRange {
    first_row_id: u32,
    overflow_start: u32,
    overflow_len: u32,
}

impl DiskPostingRange {
    fn encode(&self, target: &mut Vec<u8>) {
        target.extend_from_slice(&self.first_row_id.to_le_bytes());
        target.extend_from_slice(&self.overflow_start.to_le_bytes());
        target.extend_from_slice(&self.overflow_len.to_le_bytes());
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
enum SnapshotIndexKind {
    Resource = 1,
    ResourceObject = 2,
    ResourceTypeRelation = 3,
    ResourceType = 4,
    Subject = 5,
    SubjectTypeRelation = 6,
    SubjectType = 7,
}

impl SnapshotIndexKind {
    const ALL: [Self; SNAPSHOT_INDEX_KIND_COUNT] = [
        Self::Resource,
        Self::ResourceObject,
        Self::ResourceTypeRelation,
        Self::ResourceType,
        Self::Subject,
        Self::SubjectTypeRelation,
        Self::SubjectType,
    ];

    fn from_raw(value: u16) -> Result<Self, SnapshotIoError> {
        match value {
            1 => Ok(Self::Resource),
            2 => Ok(Self::ResourceObject),
            3 => Ok(Self::ResourceTypeRelation),
            4 => Ok(Self::ResourceType),
            5 => Ok(Self::Subject),
            6 => Ok(Self::SubjectTypeRelation),
            7 => Ok(Self::SubjectType),
            _ => Err(SnapshotIoError::Format {
                reason: "unknown snapshot index kind",
            }),
        }
    }

    const fn raw(self) -> u16 {
        self as u16
    }

    const fn key_field_count(self) -> usize {
        match self {
            Self::Resource | Self::Subject => 3,
            Self::ResourceObject | Self::ResourceTypeRelation | Self::SubjectTypeRelation => 2,
            Self::ResourceType | Self::SubjectType => 1,
        }
    }
}

#[derive(Debug)]
struct EncodedSnapshotIndexes {
    directory: Vec<u8>,
    keys: Vec<u8>,
    ranges: Vec<u8>,
    posting_row_ids: Vec<u8>,
    key_count: u64,
    range_count: u64,
    posting_row_id_count: u64,
}

impl EncodedSnapshotIndexes {
    fn from_rows(
        rows: &[DiskRelationshipRow],
        index_profile: IndexProfile,
    ) -> Result<Self, SnapshotIoError> {
        let mut groups = SnapshotIndexGroups::default();
        for (index, row) in rows.iter().copied().enumerate() {
            let row_id =
                checked_u32_from_usize(index.checked_add(1).ok_or(SnapshotIoError::Format {
                    reason: "row id overflowed",
                })?)?;
            groups.insert_row(row, row_id, index_profile);
        }

        let mut directory = Vec::with_capacity(
            SNAPSHOT_INDEX_KIND_COUNT
                .checked_mul(DISK_INDEX_DIRECTORY_LEN)
                .ok_or(SnapshotIoError::Format {
                    reason: "index directory length overflowed",
                })?,
        );
        let mut keys = Vec::new();
        let mut ranges = Vec::new();
        let mut posting_row_ids = Vec::new();
        let mut key_count = 0_u32;
        let mut range_count = 0_u32;
        let mut posting_row_id_count = 0_u32;

        for kind in SnapshotIndexKind::ALL {
            let group = groups.group(kind);
            let key_start = checked_u32_from_usize(keys.len())?;
            let range_start = range_count;
            let encoded_group =
                encode_v3_index_group(kind, group, &mut keys, &mut ranges, &mut posting_row_ids)?;
            key_count =
                key_count
                    .checked_add(encoded_group.key_count)
                    .ok_or(SnapshotIoError::Format {
                        reason: "index key count overflowed",
                    })?;
            range_count = range_count.checked_add(encoded_group.multi_count).ok_or(
                SnapshotIoError::Format {
                    reason: "posting range count overflowed",
                },
            )?;
            posting_row_id_count = posting_row_id_count
                .checked_add(encoded_group.overflow_row_id_count)
                .ok_or(SnapshotIoError::Format {
                    reason: "posting row id count overflowed",
                })?;
            directory.extend_from_slice(&kind.raw().to_le_bytes());
            directory.extend_from_slice(&encoded_group.encoding.flags().to_le_bytes());
            directory.extend_from_slice(&key_start.to_le_bytes());
            directory.extend_from_slice(&encoded_group.key_count.to_le_bytes());
            directory.extend_from_slice(&range_start.to_le_bytes());
            directory.extend_from_slice(&encoded_group.multi_count.to_le_bytes());
        }

        Ok(Self {
            directory,
            keys,
            ranges,
            posting_row_ids,
            key_count: u64::from(key_count),
            range_count: u64::from(range_count),
            posting_row_id_count: u64::from(posting_row_id_count),
        })
    }
}

#[derive(Debug, Default)]
struct SnapshotIndexGroups {
    resource: BTreeMap<DiskIndexKey, Vec<u32>>,
    resource_object: BTreeMap<DiskIndexKey, Vec<u32>>,
    resource_type_relation: BTreeMap<DiskIndexKey, Vec<u32>>,
    resource_type: BTreeMap<DiskIndexKey, Vec<u32>>,
    subject: BTreeMap<DiskIndexKey, Vec<u32>>,
    subject_type_relation: BTreeMap<DiskIndexKey, Vec<u32>>,
    subject_type: BTreeMap<DiskIndexKey, Vec<u32>>,
}

impl SnapshotIndexGroups {
    fn insert_row(&mut self, row: DiskRelationshipRow, row_id: u32, index_profile: IndexProfile) {
        self.resource
            .entry(DiskIndexKey {
                first: row.resource_type,
                second: row.resource_id,
                third: row.relation,
            })
            .or_default()
            .push(row_id);
        if !index_profile.supports_broad_resource_indexes() {
            return;
        }
        self.resource_object
            .entry(DiskIndexKey {
                first: row.resource_type,
                second: row.resource_id,
                third: 0,
            })
            .or_default()
            .push(row_id);
        self.resource_type_relation
            .entry(DiskIndexKey {
                first: row.resource_type,
                second: row.relation,
                third: 0,
            })
            .or_default()
            .push(row_id);
        self.resource_type
            .entry(DiskIndexKey {
                first: row.resource_type,
                second: 0,
                third: 0,
            })
            .or_default()
            .push(row_id);
        if !index_profile.supports_subject_reverse_lookup() {
            return;
        }
        self.subject
            .entry(DiskIndexKey {
                first: row.subject_type,
                second: row.subject_id,
                third: row.subject_relation,
            })
            .or_default()
            .push(row_id);
        if row.subject_relation != 0 {
            self.subject
                .entry(DiskIndexKey {
                    first: row.subject_type,
                    second: row.subject_id,
                    third: 0,
                })
                .or_default()
                .push(row_id);
            self.subject_type_relation
                .entry(DiskIndexKey {
                    first: row.subject_type,
                    second: row.subject_relation,
                    third: 0,
                })
                .or_default()
                .push(row_id);
        }
        self.subject_type
            .entry(DiskIndexKey {
                first: row.subject_type,
                second: 0,
                third: 0,
            })
            .or_default()
            .push(row_id);
    }

    fn group(&self, kind: SnapshotIndexKind) -> &BTreeMap<DiskIndexKey, Vec<u32>> {
        match kind {
            SnapshotIndexKind::Resource => &self.resource,
            SnapshotIndexKind::ResourceObject => &self.resource_object,
            SnapshotIndexKind::ResourceTypeRelation => &self.resource_type_relation,
            SnapshotIndexKind::ResourceType => &self.resource_type,
            SnapshotIndexKind::Subject => &self.subject,
            SnapshotIndexKind::SubjectTypeRelation => &self.subject_type_relation,
            SnapshotIndexKind::SubjectType => &self.subject_type,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct EncodedIndexGroup {
    encoding: SnapshotIndexEncoding,
    key_count: u32,
    multi_count: u32,
    overflow_row_id_count: u32,
}

fn encode_v3_index_group(
    kind: SnapshotIndexKind,
    group: &BTreeMap<DiskIndexKey, Vec<u32>>,
    keys: &mut Vec<u8>,
    ranges: &mut Vec<u8>,
    posting_row_ids: &mut Vec<u8>,
) -> Result<EncodedIndexGroup, SnapshotIoError> {
    let key_count = checked_u32_from_usize(group.len())?;
    let multi_count =
        checked_u32_from_usize(group.values().filter(|row_ids| row_ids.len() > 1).count())?;
    let key_width = snapshot_key_width(kind, group)?;
    let encoding = SnapshotIndexEncoding::CompactV3 { key_width };
    let mut overflow_row_id_count = 0_u32;

    for (key, row_ids) in group.iter().filter(|(_, row_ids)| row_ids.len() == 1) {
        encode_v3_key(kind, *key, key_width, keys);
        let row_id = *row_ids.first().ok_or(SnapshotIoError::Format {
            reason: "empty posting list",
        })?;
        keys.extend_from_slice(&row_id.to_le_bytes());
    }

    for (key, row_ids) in group.iter().filter(|(_, row_ids)| row_ids.len() > 1) {
        encode_v3_key(kind, *key, key_width, keys);
        let range = encode_v3_posting_range(row_ids, posting_row_ids)?;
        overflow_row_id_count = overflow_row_id_count
            .checked_add(range.overflow_row_id_count)
            .ok_or(SnapshotIoError::Format {
                reason: "posting row id count overflowed",
            })?;
        range.range.encode(ranges);
    }

    Ok(EncodedIndexGroup {
        encoding,
        key_count,
        multi_count,
        overflow_row_id_count,
    })
}

fn snapshot_key_width(
    kind: SnapshotIndexKind,
    group: &BTreeMap<DiskIndexKey, Vec<u32>>,
) -> Result<SnapshotKeyWidth, SnapshotIoError> {
    let field_count = kind.key_field_count();
    let mut max_field = 0_u32;
    for key in group.keys().copied() {
        max_field = max_field.max(key.max_field(field_count)?);
    }
    Ok(SnapshotKeyWidth::for_max(max_field))
}

fn encode_v3_key(
    kind: SnapshotIndexKind,
    key: DiskIndexKey,
    width: SnapshotKeyWidth,
    target: &mut Vec<u8>,
) {
    width.encode_value(key.first, target);
    if kind.key_field_count() >= 2 {
        width.encode_value(key.second, target);
    }
    if kind.key_field_count() >= 3 {
        width.encode_value(key.third, target);
    }
}

#[derive(Debug, Clone, Copy)]
struct EncodedPostingRange {
    range: DiskPostingRange,
    overflow_row_id_count: u32,
}

fn encode_v3_posting_range(
    row_ids: &[u32],
    posting_row_ids: &mut Vec<u8>,
) -> Result<EncodedPostingRange, SnapshotIoError> {
    let (first, rest) = row_ids.split_first().ok_or(SnapshotIoError::Format {
        reason: "empty posting list",
    })?;
    let overflow_start = checked_u32_from_usize(posting_row_ids.len())?;
    let overflow_row_id_count = checked_u32_from_usize(rest.len())?;
    let mut previous = *first;
    for row_id in rest {
        if *row_id <= previous {
            return Err(SnapshotIoError::Format {
                reason: "posting row ids are not strictly increasing",
            });
        }
        let delta = row_id
            .checked_sub(previous)
            .ok_or(SnapshotIoError::Format {
                reason: "posting row id delta underflowed",
            })?;
        encode_posting_delta_varint(delta, posting_row_ids)?;
        previous = *row_id;
    }
    let overflow_len = checked_u32_from_usize(
        posting_row_ids
            .len()
            .checked_sub(checked_usize_from_u32(overflow_start)?)
            .ok_or(SnapshotIoError::Format {
                reason: "posting row id length underflowed",
            })?,
    )?;
    Ok(EncodedPostingRange {
        range: DiskPostingRange {
            first_row_id: *first,
            overflow_start,
            overflow_len,
        },
        overflow_row_id_count,
    })
}

fn encode_posting_delta_varint(value: u32, target: &mut Vec<u8>) -> Result<(), SnapshotIoError> {
    if value == 0 {
        return Err(SnapshotIoError::Format {
            reason: "posting row id delta must be non-zero",
        });
    }
    let mut remaining = value;
    while remaining >= 0x80 {
        let low_bits = u8::try_from(remaining & 0x7F).map_err(|_| SnapshotIoError::Format {
            reason: "posting row id delta overflowed",
        })?;
        target.push(low_bits | 0x80);
        remaining >>= 7;
    }
    let final_byte = u8::try_from(remaining).map_err(|_| SnapshotIoError::Format {
        reason: "posting row id delta overflowed",
    })?;
    target.push(final_byte);
    Ok(())
}

#[derive(Debug)]
struct DecodedSnapshotIndexes {
    resource: PostingIndex<ResourceIndexKey>,
    resource_object: PostingIndex<ResourceObjectIndexKey>,
    resource_type_relation: PostingIndex<ResourceTypeRelationIndexKey>,
    resource_type: PostingIndex<ObjectTypeId>,
    subject: PostingIndex<SubjectIndexKey>,
    subject_type_relation: PostingIndex<SubjectTypeRelationIndexKey>,
    subject_type: PostingIndex<SubjectTypeId>,
}

impl DecodedSnapshotIndexes {
    fn decode(
        reader: &SnapshotReader<'_>,
        rows: &[RelationshipRow],
        profile: SnapshotLoadProfile,
        validation: SnapshotValidationMode,
    ) -> Result<Self, SnapshotIoError> {
        let directory = decode_index_directory(reader)?;
        let keys = decode_index_keys(reader)?;
        let ranges = decode_posting_ranges(reader)?;
        let posting_row_ids = decode_posting_row_ids(reader)?;
        let row_count = reader.header().relationship_count;
        let symbol_count = reader.header().symbol_count;
        validate_index_section_layout(&directory, &keys, &ranges, &posting_row_ids, row_count)?;
        let input = SnapshotIndexDecodeInput {
            directory: &directory,
            keys: &keys,
            ranges: &ranges,
            posting_row_ids: &posting_row_ids,
            rows,
            row_count,
            symbol_count,
            index_profile: reader.header().index_profile,
            profile,
            validation,
        };

        let mut decoded_posting_row_id_count = 0_u64;
        let resource = decode_resource_index(&input, &mut decoded_posting_row_id_count)?;
        let resource_object =
            decode_resource_object_index(&input, &mut decoded_posting_row_id_count)?;
        let resource_type_relation =
            decode_resource_type_relation_index(&input, &mut decoded_posting_row_id_count)?;
        let resource_type = decode_resource_type_index(&input, &mut decoded_posting_row_id_count)?;
        let subject = decode_subject_index(&input, &mut decoded_posting_row_id_count)?;
        let subject_type_relation =
            decode_subject_type_relation_index(&input, &mut decoded_posting_row_id_count)?;
        let subject_type = decode_subject_type_index(&input, &mut decoded_posting_row_id_count)?;
        validate_decoded_posting_row_id_count(&posting_row_ids, decoded_posting_row_id_count)?;

        Ok(Self {
            resource,
            resource_object,
            resource_type_relation,
            resource_type,
            subject,
            subject_type_relation,
            subject_type,
        })
    }
}

fn decode_resource_index(
    input: &SnapshotIndexDecodeInput<'_>,
    decoded_posting_row_id_count: &mut u64,
) -> Result<PostingIndex<ResourceIndexKey>, SnapshotIoError> {
    decode_index(
        input,
        &SnapshotIndexDecoder {
            kind: SnapshotIndexKind::Resource,
            key_from_disk: resource_key_from_disk,
            row_matches_key: row_matches_resource_key,
            coverage_bit: simple_index_coverage_bit,
            expected_mask: |_| 1,
            required_by_profile: |_| true,
        },
        decoded_posting_row_id_count,
    )
}

fn decode_resource_object_index(
    input: &SnapshotIndexDecodeInput<'_>,
    decoded_posting_row_id_count: &mut u64,
) -> Result<PostingIndex<ResourceObjectIndexKey>, SnapshotIoError> {
    decode_index(
        input,
        &SnapshotIndexDecoder {
            kind: SnapshotIndexKind::ResourceObject,
            key_from_disk: resource_object_key_from_disk,
            row_matches_key: row_matches_resource_object_key,
            coverage_bit: simple_index_coverage_bit,
            expected_mask: |_| 1,
            required_by_profile: IndexProfile::supports_broad_resource_indexes,
        },
        decoded_posting_row_id_count,
    )
}

fn decode_resource_type_relation_index(
    input: &SnapshotIndexDecodeInput<'_>,
    decoded_posting_row_id_count: &mut u64,
) -> Result<PostingIndex<ResourceTypeRelationIndexKey>, SnapshotIoError> {
    decode_index(
        input,
        &SnapshotIndexDecoder {
            kind: SnapshotIndexKind::ResourceTypeRelation,
            key_from_disk: resource_type_relation_key_from_disk,
            row_matches_key: row_matches_resource_type_relation_key,
            coverage_bit: simple_index_coverage_bit,
            expected_mask: |_| 1,
            required_by_profile: IndexProfile::supports_broad_resource_indexes,
        },
        decoded_posting_row_id_count,
    )
}

fn decode_resource_type_index(
    input: &SnapshotIndexDecodeInput<'_>,
    decoded_posting_row_id_count: &mut u64,
) -> Result<PostingIndex<ObjectTypeId>, SnapshotIoError> {
    decode_index(
        input,
        &SnapshotIndexDecoder {
            kind: SnapshotIndexKind::ResourceType,
            key_from_disk: resource_type_key_from_disk,
            row_matches_key: row_matches_resource_type_key,
            coverage_bit: simple_index_coverage_bit,
            expected_mask: |_| 1,
            required_by_profile: IndexProfile::supports_broad_resource_indexes,
        },
        decoded_posting_row_id_count,
    )
}

fn decode_subject_index(
    input: &SnapshotIndexDecodeInput<'_>,
    decoded_posting_row_id_count: &mut u64,
) -> Result<PostingIndex<SubjectIndexKey>, SnapshotIoError> {
    decode_index(
        input,
        &SnapshotIndexDecoder {
            kind: SnapshotIndexKind::Subject,
            key_from_disk: subject_key_from_disk,
            row_matches_key: row_matches_subject_key,
            coverage_bit: subject_index_coverage_bit,
            expected_mask: |row| if row.subject_relation.is_some() { 3 } else { 1 },
            required_by_profile: IndexProfile::supports_subject_reverse_lookup,
        },
        decoded_posting_row_id_count,
    )
}

fn decode_subject_type_relation_index(
    input: &SnapshotIndexDecodeInput<'_>,
    decoded_posting_row_id_count: &mut u64,
) -> Result<PostingIndex<SubjectTypeRelationIndexKey>, SnapshotIoError> {
    decode_index(
        input,
        &SnapshotIndexDecoder {
            kind: SnapshotIndexKind::SubjectTypeRelation,
            key_from_disk: subject_type_relation_key_from_disk,
            row_matches_key: row_matches_subject_type_relation_key,
            coverage_bit: simple_index_coverage_bit,
            expected_mask: |row| u8::from(row.subject_relation.is_some()),
            required_by_profile: IndexProfile::supports_subject_reverse_lookup,
        },
        decoded_posting_row_id_count,
    )
}

fn decode_subject_type_index(
    input: &SnapshotIndexDecodeInput<'_>,
    decoded_posting_row_id_count: &mut u64,
) -> Result<PostingIndex<SubjectTypeId>, SnapshotIoError> {
    decode_index(
        input,
        &SnapshotIndexDecoder {
            kind: SnapshotIndexKind::SubjectType,
            key_from_disk: subject_type_key_from_disk,
            row_matches_key: row_matches_subject_type_key,
            coverage_bit: simple_index_coverage_bit,
            expected_mask: |_| 1,
            required_by_profile: IndexProfile::supports_subject_reverse_lookup,
        },
        decoded_posting_row_id_count,
    )
}

#[derive(Debug, Clone, Copy)]
struct DiskIndexDirectoryEntry {
    kind: SnapshotIndexKind,
    encoding: SnapshotIndexEncoding,
    key_start: u32,
    key_count: u32,
    posting_range_start: u32,
    posting_range_count: u32,
}

#[derive(Debug, Clone, Copy)]
struct SnapshotIndexDecodeInput<'a> {
    directory: &'a [DiskIndexDirectoryEntry],
    keys: &'a DecodedIndexKeys,
    ranges: &'a [DiskPostingRange],
    posting_row_ids: &'a DecodedPostingRowIds,
    rows: &'a [RelationshipRow],
    row_count: u32,
    symbol_count: u32,
    index_profile: IndexProfile,
    profile: SnapshotLoadProfile,
    validation: SnapshotValidationMode,
}

#[derive(Debug)]
enum SnapshotIndexEntries<'a> {
    Borrowed {
        keys: &'a [DiskIndexKey],
        ranges: &'a [DiskPostingRange],
    },
    Owned(Vec<SnapshotIndexEntry>),
}

impl SnapshotIndexEntries<'_> {
    fn len(&self) -> usize {
        match self {
            Self::Borrowed { keys, .. } => keys.len(),
            Self::Owned(entries) => entries.len(),
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct SnapshotIndexEntry {
    key: DiskIndexKey,
    range: DiskPostingRange,
}

#[derive(Debug)]
enum DecodedIndexKeys {
    Fixed(Vec<DiskIndexKey>),
    Compact { bytes: Vec<u8>, row_count: u64 },
}

#[derive(Debug)]
enum DecodedPostingRowIds {
    Fixed(Vec<RowId>),
    DeltaVarint { bytes: Vec<u8>, row_count: u64 },
}

struct SnapshotIndexDecoder<K> {
    kind: SnapshotIndexKind,
    key_from_disk: fn(DiskIndexKey, u32) -> Result<K, SnapshotIoError>,
    row_matches_key: fn(&RelationshipRow, DiskIndexKey) -> bool,
    coverage_bit: fn(&RelationshipRow, DiskIndexKey) -> u8,
    expected_mask: fn(&RelationshipRow) -> u8,
    required_by_profile: fn(IndexProfile) -> bool,
}

fn decode_index_directory(
    reader: &SnapshotReader<'_>,
) -> Result<Vec<DiskIndexDirectoryEntry>, SnapshotIoError> {
    let section = reader.section(SectionKind::IndexDirectory)?;
    if section.row_count() != SNAPSHOT_INDEX_KIND_COUNT_U64 {
        return Err(SnapshotIoError::Format {
            reason: "index directory row count is invalid",
        });
    }
    let expected_len = checked_mul_usize(SNAPSHOT_INDEX_KIND_COUNT, DISK_INDEX_DIRECTORY_LEN)?;
    if section.bytes().len() != expected_len {
        return Err(SnapshotIoError::Format {
            reason: "index directory length is invalid",
        });
    }
    let mut cursor = BinaryCursor::new(section.bytes());
    let mut entries = Vec::with_capacity(SNAPSHOT_INDEX_KIND_COUNT);
    let mut seen = HashSet::with_capacity(SNAPSHOT_INDEX_KIND_COUNT);
    for _ in 0..SNAPSHOT_INDEX_KIND_COUNT {
        let kind = SnapshotIndexKind::from_raw(cursor.read_u16()?)?;
        let flags = cursor.read_u16()?;
        let encoding = SnapshotIndexEncoding::from_flags(flags)?;
        insert_unique(&mut seen, kind, "duplicate snapshot index kind")?;
        entries.push(DiskIndexDirectoryEntry {
            kind,
            encoding,
            key_start: cursor.read_u32()?,
            key_count: cursor.read_u32()?,
            posting_range_start: cursor.read_u32()?,
            posting_range_count: cursor.read_u32()?,
        });
    }
    Ok(entries)
}

fn decode_index_keys(reader: &SnapshotReader<'_>) -> Result<DecodedIndexKeys, SnapshotIoError> {
    let section = reader.section(SectionKind::IndexKeys)?;
    if reader.header().format_version == SnapshotFormatVersion::V3 {
        if section.bytes().is_empty() && section.row_count() != 0 {
            return Err(SnapshotIoError::Format {
                reason: "index key length does not match row count",
            });
        }
        return Ok(DecodedIndexKeys::Compact {
            bytes: section.bytes().to_vec(),
            row_count: section.row_count(),
        });
    }
    let row_count = checked_usize_from_u64(section.row_count())?;
    let expected_len = checked_mul_usize(row_count, DISK_INDEX_KEY_LEN)?;
    if section.bytes().len() != expected_len {
        return Err(SnapshotIoError::Format {
            reason: "index key length does not match row count",
        });
    }
    let mut cursor = BinaryCursor::new(section.bytes());
    let mut keys = Vec::with_capacity(row_count);
    for _ in 0..row_count {
        keys.push(DiskIndexKey {
            first: cursor.read_u32()?,
            second: cursor.read_u32()?,
            third: cursor.read_u32()?,
        });
    }
    Ok(DecodedIndexKeys::Fixed(keys))
}

fn decode_posting_ranges(
    reader: &SnapshotReader<'_>,
) -> Result<Vec<DiskPostingRange>, SnapshotIoError> {
    let section = reader.section(SectionKind::PostingRanges)?;
    let row_count = checked_usize_from_u64(section.row_count())?;
    let expected_len = checked_mul_usize(row_count, DISK_POSTING_RANGE_LEN)?;
    if section.bytes().len() != expected_len {
        return Err(SnapshotIoError::Format {
            reason: "posting range length does not match row count",
        });
    }
    let mut cursor = BinaryCursor::new(section.bytes());
    let mut ranges = Vec::with_capacity(row_count);
    for _ in 0..row_count {
        ranges.push(DiskPostingRange {
            first_row_id: cursor.read_u32()?,
            overflow_start: cursor.read_u32()?,
            overflow_len: cursor.read_u32()?,
        });
    }
    Ok(ranges)
}

fn decode_posting_row_ids(
    reader: &SnapshotReader<'_>,
) -> Result<DecodedPostingRowIds, SnapshotIoError> {
    let section = reader.section(SectionKind::PostingRowIds)?;
    if reader.header().format_version == SnapshotFormatVersion::V3 {
        return Ok(DecodedPostingRowIds::DeltaVarint {
            bytes: section.bytes().to_vec(),
            row_count: section.row_count(),
        });
    }
    let row_count = checked_usize_from_u64(section.row_count())?;
    let expected_len = checked_mul_usize(row_count, DISK_ROW_ID_LEN)?;
    if section.bytes().len() != expected_len {
        return Err(SnapshotIoError::Format {
            reason: "posting row id length does not match row count",
        });
    }
    let total_rows = reader.header().relationship_count;
    let mut cursor = BinaryCursor::new(section.bytes());
    let mut row_ids = Vec::with_capacity(row_count);
    for _ in 0..row_count {
        row_ids.push(RowId::from_snapshot_raw(cursor.read_u32()?, total_rows)?);
    }
    Ok(DecodedPostingRowIds::Fixed(row_ids))
}

fn validate_index_section_layout(
    directory: &[DiskIndexDirectoryEntry],
    keys: &DecodedIndexKeys,
    ranges: &[DiskPostingRange],
    posting_row_ids: &DecodedPostingRowIds,
    row_count: u32,
) -> Result<(), SnapshotIoError> {
    match (keys, posting_row_ids) {
        (DecodedIndexKeys::Fixed(_), DecodedPostingRowIds::Fixed(_)) => Ok(()),
        (
            DecodedIndexKeys::Compact {
                bytes: key_bytes,
                row_count: key_row_count,
            },
            DecodedPostingRowIds::DeltaVarint {
                bytes: posting_bytes,
                row_count: _,
            },
        ) => validate_v3_compact_index_sections(
            directory,
            key_bytes,
            *key_row_count,
            ranges,
            posting_bytes,
            row_count,
        ),
        _ => Err(SnapshotIoError::Format {
            reason: "index section encodings do not match snapshot format",
        }),
    }
}

fn validate_v3_compact_index_sections(
    directory: &[DiskIndexDirectoryEntry],
    key_bytes: &[u8],
    key_row_count: u64,
    ranges: &[DiskPostingRange],
    posting_bytes: &[u8],
    row_count: u32,
) -> Result<(), SnapshotIoError> {
    let mut total_keys = 0_u64;
    let mut total_ranges = 0_u64;
    let mut key_spans = Vec::with_capacity(directory.len());
    let mut range_spans = Vec::with_capacity(directory.len());

    for entry in directory {
        let SnapshotIndexEncoding::CompactV3 { key_width } = entry.encoding else {
            return Err(SnapshotIoError::Format {
                reason: "v3 index directory contains non-compact encoding",
            });
        };
        total_keys =
            total_keys
                .checked_add(u64::from(entry.key_count))
                .ok_or(SnapshotIoError::Format {
                    reason: "index key count overflowed",
                })?;
        total_ranges = total_ranges
            .checked_add(u64::from(entry.posting_range_count))
            .ok_or(SnapshotIoError::Format {
                reason: "posting range count overflowed",
            })?;

        let key_span = compact_index_key_span(*entry, key_width)?;
        if key_span.end > key_bytes.len() {
            return Err(SnapshotIoError::Format {
                reason: "compact index key range is out of bounds",
            });
        }
        key_spans.push(key_span);

        let range_span = index_range_span(*entry)?;
        if range_span.end > ranges.len() {
            return Err(SnapshotIoError::Format {
                reason: "posting range span is out of bounds",
            });
        }
        range_spans.push(range_span);
    }

    if total_keys != key_row_count {
        return Err(SnapshotIoError::Format {
            reason: "index key row count does not match directory",
        });
    }
    if total_ranges
        != u64::try_from(ranges.len()).map_err(|_| SnapshotIoError::LimitExceeded {
            component: "posting ranges",
        })?
    {
        return Err(SnapshotIoError::Format {
            reason: "posting range row count does not match directory",
        });
    }

    validate_spans_cover(
        key_spans,
        key_bytes.len(),
        "compact index key spans do not cover section",
    )?;
    validate_spans_cover(
        range_spans,
        ranges.len(),
        "posting range spans do not cover section",
    )?;

    validate_v3_posting_row_id_spans(ranges, posting_bytes, row_count)
}

fn compact_index_key_span(
    entry: DiskIndexDirectoryEntry,
    key_width: SnapshotKeyWidth,
) -> Result<Range<usize>, SnapshotIoError> {
    if entry.posting_range_count > entry.key_count {
        return Err(SnapshotIoError::Format {
            reason: "posting range count exceeds compact key count",
        });
    }
    let key_start = checked_usize_from_u32(entry.key_start)?;
    let key_count = checked_usize_from_u32(entry.key_count)?;
    let multi_count = checked_usize_from_u32(entry.posting_range_count)?;
    let singleton_count = key_count
        .checked_sub(multi_count)
        .ok_or(SnapshotIoError::Format {
            reason: "compact singleton count underflowed",
        })?;
    let key_len = checked_mul_usize(entry.kind.key_field_count(), key_width.byte_len())?;
    let singleton_entry_len = checked_add_usize(key_len, DISK_ROW_ID_LEN)?;
    let singleton_bytes_len = checked_mul_usize(singleton_count, singleton_entry_len)?;
    let multi_bytes_len = checked_mul_usize(multi_count, key_len)?;
    let group_bytes_len = checked_add_usize(singleton_bytes_len, multi_bytes_len)?;
    let key_end = checked_add_usize(key_start, group_bytes_len)?;
    Ok(key_start..key_end)
}

fn index_range_span(entry: DiskIndexDirectoryEntry) -> Result<Range<usize>, SnapshotIoError> {
    let range_start = checked_usize_from_u32(entry.posting_range_start)?;
    let range_count = checked_usize_from_u32(entry.posting_range_count)?;
    let range_end = checked_add_usize(range_start, range_count)?;
    Ok(range_start..range_end)
}

fn validate_spans_cover(
    mut spans: Vec<Range<usize>>,
    total_len: usize,
    reason: &'static str,
) -> Result<(), SnapshotIoError> {
    spans.sort_by(|left, right| left.start.cmp(&right.start).then(left.end.cmp(&right.end)));
    let mut cursor = 0_usize;
    for span in spans {
        if span.start > span.end || span.start != cursor {
            return Err(SnapshotIoError::Format { reason });
        }
        cursor = span.end;
    }
    if cursor != total_len {
        return Err(SnapshotIoError::Format { reason });
    }
    Ok(())
}

fn validate_v3_posting_row_id_spans(
    ranges: &[DiskPostingRange],
    bytes: &[u8],
    row_count: u32,
) -> Result<(), SnapshotIoError> {
    let mut spans = Vec::new();
    for range in ranges {
        RowId::from_snapshot_raw(range.first_row_id, row_count)?;
        let start = checked_usize_from_u32(range.overflow_start)?;
        let len = checked_usize_from_u32(range.overflow_len)?;
        let end = checked_add_usize(start, len)?;
        if end > bytes.len() {
            return Err(SnapshotIoError::Format {
                reason: "posting range points outside posting row ids",
            });
        }
        if len == 0 {
            continue;
        }
        spans.push(start..end);
    }
    validate_spans_cover(
        spans,
        bytes.len(),
        "posting row id spans do not cover section",
    )?;
    Ok(())
}

fn validate_decoded_posting_row_id_count(
    posting_row_ids: &DecodedPostingRowIds,
    decoded_count: u64,
) -> Result<(), SnapshotIoError> {
    let expected = match posting_row_ids {
        DecodedPostingRowIds::Fixed(row_ids) => {
            u64::try_from(row_ids.len()).map_err(|_| SnapshotIoError::LimitExceeded {
                component: "posting row ids",
            })?
        }
        DecodedPostingRowIds::DeltaVarint { row_count, .. } => *row_count,
    };
    if decoded_count != expected {
        return Err(SnapshotIoError::Format {
            reason: "posting row id row count does not match decoded varints",
        });
    }
    Ok(())
}

fn increment_decoded_posting_row_id_count(counter: &mut u64) -> Result<(), SnapshotIoError> {
    *counter = counter.checked_add(1).ok_or(SnapshotIoError::Format {
        reason: "posting row id count overflowed",
    })?;
    Ok(())
}

fn decode_index<K>(
    input: &SnapshotIndexDecodeInput<'_>,
    decoder: &SnapshotIndexDecoder<K>,
    decoded_posting_row_id_count: &mut u64,
) -> Result<PostingIndex<K>, SnapshotIoError>
where
    K: Copy + Eq + Hash + Ord,
{
    let entries = snapshot_index_entries(input, decoder.kind)?;
    if input.validation == SnapshotValidationMode::TrustedFastLoad
        && input.profile == SnapshotLoadProfile::FastLoad
    {
        return decode_trusted_fast_index(input, decoder, &entries, decoded_posting_row_id_count);
    }

    let mut coverage = vec![0_u8; input.rows.len()];
    let mut sorted_keys = Vec::with_capacity(entries.len());
    let mut sorted_ranges = Vec::with_capacity(entries.len());
    let mut sorted_overflow = Vec::new();
    let mut latency_index = PostingIndex::default();

    for (disk_key, range) in snapshot_index_entries_iter(&entries) {
        let typed_key = (decoder.key_from_disk)(disk_key, input.symbol_count)?;
        let row_ids = posting_row_id_iter(range, input.posting_row_ids, input.row_count)?;
        let overflow_start = checked_u32_from_usize(sorted_overflow.len())?;
        let mut overflow_len = 0_u32;
        let mut first = None;
        for row_id in row_ids {
            let row_id = row_id?;
            let row = input
                .rows
                .get(row_id.index())
                .ok_or(SnapshotIoError::Format {
                    reason: "posting row id points outside relationship rows",
                })?;
            if !(decoder.row_matches_key)(row, disk_key) {
                return Err(SnapshotIoError::Format {
                    reason: "posting row does not match index key",
                });
            }
            let bit = (decoder.coverage_bit)(row, disk_key);
            if bit == 0 {
                return Err(SnapshotIoError::Format {
                    reason: "index coverage bit is invalid",
                });
            }
            let mask = coverage
                .get_mut(row_id.index())
                .ok_or(SnapshotIoError::Format {
                    reason: "index coverage row id is out of bounds",
                })?;
            if *mask & bit != 0 {
                return Err(SnapshotIoError::Format {
                    reason: "duplicate posting row id in index",
                });
            }
            *mask |= bit;
            if first.is_none() {
                first = Some(row_id);
            } else {
                sorted_overflow.push(row_id);
                overflow_len = overflow_len.checked_add(1).ok_or(SnapshotIoError::Format {
                    reason: "posting overflow length overflowed",
                })?;
                increment_decoded_posting_row_id_count(decoded_posting_row_id_count)?;
            }
            if matches!(input.profile, SnapshotLoadProfile::Latency) {
                latency_index.insert(typed_key, row_id);
            }
        }
        let first_row_id = first.ok_or(SnapshotIoError::Format {
            reason: "empty posting range",
        })?;
        sorted_keys.push(typed_key);
        sorted_ranges.push(RuntimePostingRange {
            first_row_id,
            overflow_start,
            overflow_len,
        });
    }

    let required_by_profile = (decoder.required_by_profile)(input.index_profile);
    for (row, actual) in input.rows.iter().zip(coverage.iter().copied()) {
        let expected = if required_by_profile {
            (decoder.expected_mask)(row)
        } else {
            0
        };
        if actual != expected {
            return Err(SnapshotIoError::Format {
                reason: "index does not cover every required row",
            });
        }
    }

    match input.profile {
        SnapshotLoadProfile::FastLoad => Ok(PostingIndex::from_sorted(
            sorted_keys,
            sorted_ranges,
            sorted_overflow,
        )),
        SnapshotLoadProfile::Latency => Ok(latency_index),
    }
}

fn decode_trusted_fast_index<K>(
    input: &SnapshotIndexDecodeInput<'_>,
    decoder: &SnapshotIndexDecoder<K>,
    entries: &SnapshotIndexEntries<'_>,
    decoded_posting_row_id_count: &mut u64,
) -> Result<PostingIndex<K>, SnapshotIoError>
where
    K: Copy + Eq + Hash + Ord,
{
    let mut sorted_keys = Vec::with_capacity(entries.len());
    let mut sorted_ranges = Vec::with_capacity(entries.len());
    let mut sorted_overflow = Vec::new();
    for (disk_key, range) in snapshot_index_entries_iter(entries) {
        let typed_key = (decoder.key_from_disk)(disk_key, input.symbol_count)?;
        let first_row_id = RowId::from_snapshot_raw(range.first_row_id, input.row_count)?;
        let overflow_start = checked_u32_from_usize(sorted_overflow.len())?;
        let mut overflow_len = 0_u32;
        let mut row_ids = posting_row_id_iter(range, input.posting_row_ids, input.row_count)?;
        let first = row_ids.next().ok_or(SnapshotIoError::Format {
            reason: "empty posting range",
        })??;
        if first != first_row_id {
            return Err(SnapshotIoError::Format {
                reason: "posting range first row id mismatch",
            });
        }
        for row_id in row_ids {
            sorted_overflow.push(row_id?);
            overflow_len = overflow_len.checked_add(1).ok_or(SnapshotIoError::Format {
                reason: "posting overflow length overflowed",
            })?;
            increment_decoded_posting_row_id_count(decoded_posting_row_id_count)?;
        }
        sorted_keys.push(typed_key);
        sorted_ranges.push(RuntimePostingRange {
            first_row_id,
            overflow_start,
            overflow_len,
        });
    }
    Ok(PostingIndex::from_sorted(
        sorted_keys,
        sorted_ranges,
        sorted_overflow,
    ))
}

fn snapshot_index_entries<'a>(
    input: &'a SnapshotIndexDecodeInput<'_>,
    kind: SnapshotIndexKind,
) -> Result<SnapshotIndexEntries<'a>, SnapshotIoError> {
    let entry = input
        .directory
        .iter()
        .find(|entry| entry.kind == kind)
        .copied()
        .ok_or(SnapshotIoError::Format {
            reason: "missing snapshot index kind",
        })?;
    match (input.keys, entry.encoding) {
        (DecodedIndexKeys::Fixed(keys), SnapshotIndexEncoding::FixedV2) => {
            fixed_snapshot_index_entries(keys, input.ranges, entry)
        }
        (
            DecodedIndexKeys::Compact { bytes: keys, .. },
            SnapshotIndexEncoding::CompactV3 { key_width },
        ) => compact_snapshot_index_entries(keys, input.ranges, entry, key_width),
        _ => Err(SnapshotIoError::Format {
            reason: "index encoding does not match snapshot format",
        }),
    }
}

fn fixed_snapshot_index_entries<'a>(
    keys: &'a [DiskIndexKey],
    ranges: &'a [DiskPostingRange],
    entry: DiskIndexDirectoryEntry,
) -> Result<SnapshotIndexEntries<'a>, SnapshotIoError> {
    if entry.key_count != entry.posting_range_count {
        return Err(SnapshotIoError::Format {
            reason: "index key count does not match posting range count",
        });
    }
    let key_start = checked_usize_from_u32(entry.key_start)?;
    let key_count = checked_usize_from_u32(entry.key_count)?;
    let key_end = checked_add_usize(key_start, key_count)?;
    let range_start = checked_usize_from_u32(entry.posting_range_start)?;
    let range_count = checked_usize_from_u32(entry.posting_range_count)?;
    let range_end = checked_add_usize(range_start, range_count)?;
    let keys = keys
        .get(key_start..key_end)
        .ok_or(SnapshotIoError::Format {
            reason: "index key range is out of bounds",
        })?;
    let ranges = ranges
        .get(range_start..range_end)
        .ok_or(SnapshotIoError::Format {
            reason: "posting range span is out of bounds",
        })?;
    validate_sorted_keys(keys)?;
    Ok(SnapshotIndexEntries::Borrowed { keys, ranges })
}

fn compact_snapshot_index_entries<'a>(
    keys: &'a [u8],
    ranges: &'a [DiskPostingRange],
    entry: DiskIndexDirectoryEntry,
    key_width: SnapshotKeyWidth,
) -> Result<SnapshotIndexEntries<'a>, SnapshotIoError> {
    if entry.posting_range_count > entry.key_count {
        return Err(SnapshotIoError::Format {
            reason: "posting range count exceeds compact key count",
        });
    }
    let key_start = checked_usize_from_u32(entry.key_start)?;
    let key_count = checked_usize_from_u32(entry.key_count)?;
    let multi_count = checked_usize_from_u32(entry.posting_range_count)?;
    let singleton_count = key_count
        .checked_sub(multi_count)
        .ok_or(SnapshotIoError::Format {
            reason: "compact singleton count underflowed",
        })?;
    let key_len = checked_mul_usize(entry.kind.key_field_count(), key_width.byte_len())?;
    let singleton_entry_len = checked_add_usize(key_len, DISK_ROW_ID_LEN)?;
    let singleton_bytes_len = checked_mul_usize(singleton_count, singleton_entry_len)?;
    let multi_bytes_len = checked_mul_usize(multi_count, key_len)?;
    let group_bytes_len = checked_add_usize(singleton_bytes_len, multi_bytes_len)?;
    let key_end = checked_add_usize(key_start, group_bytes_len)?;
    let group_bytes = keys
        .get(key_start..key_end)
        .ok_or(SnapshotIoError::Format {
            reason: "compact index key range is out of bounds",
        })?;
    let range_start = checked_usize_from_u32(entry.posting_range_start)?;
    let range_end = checked_add_usize(range_start, multi_count)?;
    let multi_ranges = ranges
        .get(range_start..range_end)
        .ok_or(SnapshotIoError::Format {
            reason: "posting range span is out of bounds",
        })?;
    let singleton_bytes =
        group_bytes
            .get(..singleton_bytes_len)
            .ok_or(SnapshotIoError::Format {
                reason: "compact singleton keys are out of bounds",
            })?;
    let multi_key_bytes =
        group_bytes
            .get(singleton_bytes_len..)
            .ok_or(SnapshotIoError::Format {
                reason: "compact multi keys are out of bounds",
            })?;
    let singletons =
        decode_compact_singletons(entry.kind, key_width, singleton_bytes, singleton_count)?;
    let multis =
        decode_compact_multi_entries(entry.kind, key_width, multi_key_bytes, multi_ranges)?;
    Ok(SnapshotIndexEntries::Owned(merge_compact_entries(
        singletons, multis,
    )?))
}

fn snapshot_index_entries_iter<'a>(
    entries: &'a SnapshotIndexEntries<'a>,
) -> Box<dyn Iterator<Item = (DiskIndexKey, DiskPostingRange)> + 'a> {
    match entries {
        SnapshotIndexEntries::Borrowed { keys, ranges } => {
            Box::new(keys.iter().copied().zip(ranges.iter().copied()))
        }
        SnapshotIndexEntries::Owned(entries) => {
            Box::new(entries.iter().map(|entry| (entry.key, entry.range)))
        }
    }
}

fn decode_compact_singletons(
    kind: SnapshotIndexKind,
    key_width: SnapshotKeyWidth,
    bytes: &[u8],
    count: usize,
) -> Result<Vec<SnapshotIndexEntry>, SnapshotIoError> {
    let mut cursor = BinaryCursor::new(bytes);
    let mut entries = Vec::with_capacity(count);
    for _ in 0..count {
        let key = decode_compact_key(kind, key_width, &mut cursor)?;
        let row_id = cursor.read_u32()?;
        entries.push(SnapshotIndexEntry {
            key,
            range: DiskPostingRange {
                first_row_id: row_id,
                overflow_start: 0,
                overflow_len: 0,
            },
        });
    }
    validate_sorted_entries(&entries)?;
    Ok(entries)
}

fn decode_compact_multi_entries(
    kind: SnapshotIndexKind,
    key_width: SnapshotKeyWidth,
    bytes: &[u8],
    ranges: &[DiskPostingRange],
) -> Result<Vec<SnapshotIndexEntry>, SnapshotIoError> {
    let mut cursor = BinaryCursor::new(bytes);
    let mut entries = Vec::with_capacity(ranges.len());
    for range in ranges.iter().copied() {
        entries.push(SnapshotIndexEntry {
            key: decode_compact_key(kind, key_width, &mut cursor)?,
            range,
        });
    }
    validate_sorted_entries(&entries)?;
    Ok(entries)
}

fn decode_compact_key(
    kind: SnapshotIndexKind,
    key_width: SnapshotKeyWidth,
    cursor: &mut BinaryCursor<'_>,
) -> Result<DiskIndexKey, SnapshotIoError> {
    let first = key_width.read_value(cursor)?;
    let second = if kind.key_field_count() >= 2 {
        key_width.read_value(cursor)?
    } else {
        0
    };
    let third = if kind.key_field_count() >= 3 {
        key_width.read_value(cursor)?
    } else {
        0
    };
    Ok(DiskIndexKey {
        first,
        second,
        third,
    })
}

fn merge_compact_entries(
    singletons: Vec<SnapshotIndexEntry>,
    multis: Vec<SnapshotIndexEntry>,
) -> Result<Vec<SnapshotIndexEntry>, SnapshotIoError> {
    let mut merged = Vec::with_capacity(singletons.len().checked_add(multis.len()).ok_or(
        SnapshotIoError::Format {
            reason: "compact index entry count overflowed",
        },
    )?);
    let mut singleton_iter = singletons.into_iter().peekable();
    let mut multi_iter = multis.into_iter().peekable();
    while singleton_iter.peek().is_some() || multi_iter.peek().is_some() {
        match (singleton_iter.peek().copied(), multi_iter.peek().copied()) {
            (Some(singleton), Some(multi)) if singleton.key < multi.key => {
                merged.push(singleton_iter.next().ok_or(SnapshotIoError::Format {
                    reason: "compact singleton iterator ended unexpectedly",
                })?);
            }
            (Some(singleton), Some(multi)) if singleton.key > multi.key => {
                merged.push(multi_iter.next().ok_or(SnapshotIoError::Format {
                    reason: "compact multi iterator ended unexpectedly",
                })?);
            }
            (Some(_), Some(_)) => {
                return Err(SnapshotIoError::Format {
                    reason: "duplicate compact index key",
                });
            }
            (Some(_), None) => {
                merged.push(singleton_iter.next().ok_or(SnapshotIoError::Format {
                    reason: "compact singleton iterator ended unexpectedly",
                })?);
            }
            (None, Some(_)) => {
                merged.push(multi_iter.next().ok_or(SnapshotIoError::Format {
                    reason: "compact multi iterator ended unexpectedly",
                })?);
            }
            (None, None) => {}
        }
    }
    validate_sorted_entries(&merged)?;
    Ok(merged)
}

fn validate_sorted_entries(entries: &[SnapshotIndexEntry]) -> Result<(), SnapshotIoError> {
    if entries.windows(2).any(|window| {
        window
            .first()
            .zip(window.get(1))
            .is_some_and(|(left, right)| left.key >= right.key)
    }) {
        return Err(SnapshotIoError::Format {
            reason: "index keys are not strictly sorted",
        });
    }
    Ok(())
}

fn validate_sorted_keys(keys: &[DiskIndexKey]) -> Result<(), SnapshotIoError> {
    if keys.windows(2).any(|window| {
        window
            .first()
            .zip(window.get(1))
            .is_some_and(|(left, right)| left >= right)
    }) {
        return Err(SnapshotIoError::Format {
            reason: "index keys are not strictly sorted",
        });
    }
    Ok(())
}

#[derive(Debug)]
enum SnapshotPostingRowIds<'a> {
    One {
        first: Option<RowId>,
    },
    FixedMany {
        first: Option<RowId>,
        rest: std::slice::Iter<'a, RowId>,
        previous: RowId,
    },
    DeltaVarintMany {
        first: Option<RowId>,
        rest: DeltaVarintPostingIter<'a>,
    },
}

impl Iterator for SnapshotPostingRowIds<'_> {
    type Item = Result<RowId, SnapshotIoError>;

    fn next(&mut self) -> Option<Self::Item> {
        match self {
            Self::One { first } => first.take().map(Ok),
            Self::FixedMany {
                first,
                rest,
                previous,
            } => {
                if let Some(row_id) = first.take() {
                    return Some(Ok(row_id));
                }
                rest.next().copied().map(|row_id| {
                    if row_id <= *previous {
                        return Err(SnapshotIoError::Format {
                            reason: "posting row ids are not strictly increasing",
                        });
                    }
                    *previous = row_id;
                    Ok(row_id)
                })
            }
            Self::DeltaVarintMany { first, rest } => first.take().map(Ok).or_else(|| rest.next()),
        }
    }
}

#[derive(Debug)]
struct DeltaVarintPostingIter<'a> {
    bytes: &'a [u8],
    offset: usize,
    previous: RowId,
    row_count: u32,
}

impl Iterator for DeltaVarintPostingIter<'_> {
    type Item = Result<RowId, SnapshotIoError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.offset == self.bytes.len() {
            return None;
        }
        match decode_posting_delta_varint(self.bytes, &mut self.offset)
            .and_then(|delta| next_delta_row_id(self.previous, delta, self.row_count))
        {
            Ok(row_id) => {
                self.previous = row_id;
                Some(Ok(row_id))
            }
            Err(error) => {
                self.offset = self.bytes.len();
                Some(Err(error))
            }
        }
    }
}

fn posting_row_id_iter(
    range: DiskPostingRange,
    posting_row_ids: &DecodedPostingRowIds,
    row_count: u32,
) -> Result<SnapshotPostingRowIds<'_>, SnapshotIoError> {
    let first = RowId::from_snapshot_raw(range.first_row_id, row_count)?;
    if range.overflow_len == 0 {
        return Ok(SnapshotPostingRowIds::One { first: Some(first) });
    }
    match posting_row_ids {
        DecodedPostingRowIds::Fixed(row_ids) => {
            let start = checked_usize_from_u32(range.overflow_start)?;
            let len = checked_usize_from_u32(range.overflow_len)?;
            let end = checked_add_usize(start, len)?;
            let overflow = row_ids.get(start..end).ok_or(SnapshotIoError::Format {
                reason: "posting range points outside posting row ids",
            })?;
            Ok(SnapshotPostingRowIds::FixedMany {
                first: Some(first),
                rest: overflow.iter(),
                previous: first,
            })
        }
        DecodedPostingRowIds::DeltaVarint { bytes, .. } => {
            let start = checked_usize_from_u32(range.overflow_start)?;
            let len = checked_usize_from_u32(range.overflow_len)?;
            let end = checked_add_usize(start, len)?;
            let overflow = bytes.get(start..end).ok_or(SnapshotIoError::Format {
                reason: "posting range points outside posting row ids",
            })?;
            Ok(SnapshotPostingRowIds::DeltaVarintMany {
                first: Some(first),
                rest: DeltaVarintPostingIter {
                    bytes: overflow,
                    offset: 0,
                    previous: first,
                    row_count,
                },
            })
        }
    }
}

fn decode_posting_delta_varint(bytes: &[u8], offset: &mut usize) -> Result<u32, SnapshotIoError> {
    let mut value = 0_u32;
    let mut shift = 0_u32;
    loop {
        let byte = bytes.get(*offset).copied().ok_or(SnapshotIoError::Format {
            reason: "posting row id delta varint is truncated",
        })?;
        *offset = (*offset).checked_add(1).ok_or(SnapshotIoError::Format {
            reason: "posting row id delta offset overflowed",
        })?;
        let low_bits = u32::from(byte & 0x7F);
        if shift == 28 && low_bits > 0x0F {
            return Err(SnapshotIoError::Format {
                reason: "posting row id delta varint overflows u32",
            });
        }
        value |= low_bits.checked_shl(shift).ok_or(SnapshotIoError::Format {
            reason: "posting row id delta varint overflows u32",
        })?;
        if byte & 0x80 == 0 {
            if value == 0 {
                return Err(SnapshotIoError::Format {
                    reason: "posting row id delta must be non-zero",
                });
            }
            return Ok(value);
        }
        shift = shift.checked_add(7).ok_or(SnapshotIoError::Format {
            reason: "posting row id delta varint overflows u32",
        })?;
        if shift > 28 {
            return Err(SnapshotIoError::Format {
                reason: "posting row id delta varint is too long",
            });
        }
    }
}

fn next_delta_row_id(
    previous: RowId,
    delta: u32,
    row_count: u32,
) -> Result<RowId, SnapshotIoError> {
    let row_id = previous
        .raw()
        .checked_add(delta)
        .ok_or(SnapshotIoError::Format {
            reason: "posting row id delta overflowed",
        })?;
    RowId::from_snapshot_raw(row_id, row_count)
}

fn resource_key_from_disk(
    key: DiskIndexKey,
    symbol_count: u32,
) -> Result<ResourceIndexKey, SnapshotIoError> {
    Ok(ResourceIndexKey {
        object_type: ObjectTypeId(SymbolId::from_snapshot_raw(key.first, symbol_count)?),
        object_id: ObjectIdId(SymbolId::from_snapshot_raw(key.second, symbol_count)?),
        relation: RelationId(SymbolId::from_snapshot_raw(key.third, symbol_count)?),
    })
}

fn resource_object_key_from_disk(
    key: DiskIndexKey,
    symbol_count: u32,
) -> Result<ResourceObjectIndexKey, SnapshotIoError> {
    ensure_zero(
        key.third,
        "resource object index third key field must be zero",
    )?;
    Ok(ResourceObjectIndexKey {
        object_type: ObjectTypeId(SymbolId::from_snapshot_raw(key.first, symbol_count)?),
        object_id: ObjectIdId(SymbolId::from_snapshot_raw(key.second, symbol_count)?),
    })
}

fn resource_type_relation_key_from_disk(
    key: DiskIndexKey,
    symbol_count: u32,
) -> Result<ResourceTypeRelationIndexKey, SnapshotIoError> {
    ensure_zero(
        key.third,
        "resource type relation index third key field must be zero",
    )?;
    Ok(ResourceTypeRelationIndexKey {
        object_type: ObjectTypeId(SymbolId::from_snapshot_raw(key.first, symbol_count)?),
        relation: RelationId(SymbolId::from_snapshot_raw(key.second, symbol_count)?),
    })
}

fn resource_type_key_from_disk(
    key: DiskIndexKey,
    symbol_count: u32,
) -> Result<ObjectTypeId, SnapshotIoError> {
    ensure_zero(
        key.second,
        "resource type index second key field must be zero",
    )?;
    ensure_zero(
        key.third,
        "resource type index third key field must be zero",
    )?;
    Ok(ObjectTypeId(SymbolId::from_snapshot_raw(
        key.first,
        symbol_count,
    )?))
}

fn subject_key_from_disk(
    key: DiskIndexKey,
    symbol_count: u32,
) -> Result<SubjectIndexKey, SnapshotIoError> {
    let relation = if key.third == 0 {
        None
    } else {
        Some(RelationId(SymbolId::from_snapshot_raw(
            key.third,
            symbol_count,
        )?))
    };
    Ok(SubjectIndexKey {
        subject_type: SubjectTypeId(SymbolId::from_snapshot_raw(key.first, symbol_count)?),
        subject_id: SubjectIdId(SymbolId::from_snapshot_raw(key.second, symbol_count)?),
        relation,
    })
}

fn subject_type_relation_key_from_disk(
    key: DiskIndexKey,
    symbol_count: u32,
) -> Result<SubjectTypeRelationIndexKey, SnapshotIoError> {
    ensure_zero(
        key.third,
        "subject type relation index third key field must be zero",
    )?;
    Ok(SubjectTypeRelationIndexKey {
        subject_type: SubjectTypeId(SymbolId::from_snapshot_raw(key.first, symbol_count)?),
        relation: RelationId(SymbolId::from_snapshot_raw(key.second, symbol_count)?),
    })
}

fn subject_type_key_from_disk(
    key: DiskIndexKey,
    symbol_count: u32,
) -> Result<SubjectTypeId, SnapshotIoError> {
    ensure_zero(
        key.second,
        "subject type index second key field must be zero",
    )?;
    ensure_zero(key.third, "subject type index third key field must be zero")?;
    Ok(SubjectTypeId(SymbolId::from_snapshot_raw(
        key.first,
        symbol_count,
    )?))
}

fn ensure_zero(value: u32, reason: &'static str) -> Result<(), SnapshotIoError> {
    if value == 0 {
        Ok(())
    } else {
        Err(SnapshotIoError::Format { reason })
    }
}

fn row_matches_resource_key(row: &RelationshipRow, key: DiskIndexKey) -> bool {
    row.resource_type.0.get() == key.first
        && row.resource_id.0.get() == key.second
        && row.relation.0.get() == key.third
}

fn row_matches_resource_object_key(row: &RelationshipRow, key: DiskIndexKey) -> bool {
    row.resource_type.0.get() == key.first
        && row.resource_id.0.get() == key.second
        && key.third == 0
}

fn row_matches_resource_type_relation_key(row: &RelationshipRow, key: DiskIndexKey) -> bool {
    row.resource_type.0.get() == key.first && row.relation.0.get() == key.second && key.third == 0
}

fn row_matches_resource_type_key(row: &RelationshipRow, key: DiskIndexKey) -> bool {
    row.resource_type.0.get() == key.first && key.second == 0 && key.third == 0
}

fn row_matches_subject_key(row: &RelationshipRow, key: DiskIndexKey) -> bool {
    row.subject_type.0.get() == key.first
        && row.subject_id.0.get() == key.second
        && row.subject_relation.map_or(key.third == 0, |relation| {
            key.third == 0 || relation.0.get() == key.third
        })
}

fn row_matches_subject_type_relation_key(row: &RelationshipRow, key: DiskIndexKey) -> bool {
    row.subject_type.0.get() == key.first
        && row
            .subject_relation
            .is_some_and(|relation| relation.0.get() == key.second)
        && key.third == 0
}

fn row_matches_subject_type_key(row: &RelationshipRow, key: DiskIndexKey) -> bool {
    row.subject_type.0.get() == key.first && key.second == 0 && key.third == 0
}

fn simple_index_coverage_bit(_row: &RelationshipRow, _key: DiskIndexKey) -> u8 {
    1
}

fn subject_index_coverage_bit(row: &RelationshipRow, key: DiskIndexKey) -> u8 {
    match (row.subject_relation, key.third) {
        (_, 0) => 1,
        (Some(relation), value) if relation.0.get() == value => 2,
        _ => 0,
    }
}

#[derive(Debug)]
struct DecodedSnapshotRows {
    rows: Vec<RelationshipRow>,
    live_rows: LiveRows,
    uniqueness: UniquenessState,
}

fn record_relationship_decode_phase(
    timings: &mut Option<&mut SnapshotLoadPhaseTimings>,
    record: impl FnOnce(&mut SnapshotLoadPhaseTimings, std::time::Duration),
    phase_start: Instant,
) {
    if let Some(timings) = timings.as_deref_mut() {
        record(timings, phase_start.elapsed());
    }
}

fn decode_snapshot_rows(
    reader: &SnapshotReader<'_>,
    interner: &IdentifierInterner,
    validation: SnapshotValidationMode,
) -> Result<DecodedSnapshotRows, SnapshotIoError> {
    let header = reader.header();
    let section = reader.section(SectionKind::RelationshipRows)?;
    let row_count = checked_usize_from_u32(header.relationship_count)?;
    if section.row_count() != u64::from(header.relationship_count) {
        return Err(SnapshotIoError::Format {
            reason: "relationship row count does not match header",
        });
    }
    let symbol_width = if header.format_version == SnapshotFormatVersion::V3 {
        section_width_from_flags(section.flags())?
    } else {
        if section.flags() != 0 {
            return Err(SnapshotIoError::Format {
                reason: "relationship row flags are unsupported",
            });
        }
        SnapshotKeyWidth::U32
    };
    let expected_len =
        checked_mul_usize(row_count, checked_mul_usize(symbol_width.byte_len(), 6)?)?;
    if section.bytes().len() != expected_len {
        return Err(SnapshotIoError::Format {
            reason: "relationship row length does not match row count",
        });
    }
    let row_byte_len = checked_mul_usize(symbol_width.byte_len(), 6)?;
    let mut rows = Vec::with_capacity(row_count);
    let mut duplicate_detector = RelationshipIdentityIndex::default();
    let validate_semantics = validation == SnapshotValidationMode::Full;
    for (index, row_bytes) in section.bytes().chunks_exact(row_byte_len).enumerate() {
        let row_id = RowId::from_len(index)?;
        let fields = decode_snapshot_row_fields(row_bytes, symbol_width)?;
        let [
            resource_type,
            resource_id,
            relation,
            subject_type,
            subject_id,
            subject_relation,
        ] = fields;
        let row = RelationshipRow {
            row_id,
            resource_type: ObjectTypeId(SymbolId::from_snapshot_raw(
                resource_type,
                header.symbol_count,
            )?),
            resource_id: ObjectIdId(SymbolId::from_snapshot_raw(
                resource_id,
                header.symbol_count,
            )?),
            relation: RelationId(SymbolId::from_snapshot_raw(relation, header.symbol_count)?),
            subject_type: SubjectTypeId(SymbolId::from_snapshot_raw(
                subject_type,
                header.symbol_count,
            )?),
            subject_id: SubjectIdId(SymbolId::from_snapshot_raw(
                subject_id,
                header.symbol_count,
            )?),
            subject_relation: match subject_relation {
                0 => None,
                value => Some(RelationId(SymbolId::from_snapshot_raw(
                    value,
                    header.symbol_count,
                )?)),
            },
        };
        if validate_semantics {
            validate_row_domains(interner, &row)?;
            if duplicate_detector.find(&rows, &row).is_some() {
                return Err(SnapshotIoError::Format {
                    reason: "duplicate relationship row in snapshot",
                });
            }
            duplicate_detector.insert(&rows, row_id, &row);
        }
        rows.push(row);
    }
    let uniqueness = if validate_semantics {
        UniquenessState::KnownUniqueButNotIndexed
    } else {
        UniquenessState::UntrustedNotIndexed
    };
    Ok(DecodedSnapshotRows {
        rows,
        live_rows: LiveRows::full(row_count),
        uniqueness,
    })
}

fn decode_snapshot_row_fields(
    row_bytes: &[u8],
    symbol_width: SnapshotKeyWidth,
) -> Result<[u32; 6], SnapshotIoError> {
    match symbol_width {
        SnapshotKeyWidth::U8 => decode_snapshot_row_fields_u8(row_bytes),
        SnapshotKeyWidth::U16 => decode_snapshot_row_fields_u16(row_bytes),
        SnapshotKeyWidth::U24 => decode_snapshot_row_fields_u24(row_bytes),
        SnapshotKeyWidth::U32 => decode_snapshot_row_fields_u32(row_bytes),
    }
}

fn decode_snapshot_row_fields_u8(row_bytes: &[u8]) -> Result<[u32; 6], SnapshotIoError> {
    let [first, second, third, fourth, fifth, sixth] = row_bytes_to_array(row_bytes)?;
    Ok([
        u32::from(first),
        u32::from(second),
        u32::from(third),
        u32::from(fourth),
        u32::from(fifth),
        u32::from(sixth),
    ])
}

fn decode_snapshot_row_fields_u16(row_bytes: &[u8]) -> Result<[u32; 6], SnapshotIoError> {
    let [
        first_a,
        first_b,
        second_a,
        second_b,
        third_a,
        third_b,
        fourth_a,
        fourth_b,
        fifth_a,
        fifth_b,
        sixth_a,
        sixth_b,
    ] = row_bytes_to_array(row_bytes)?;
    Ok([
        u32::from(u16::from_le_bytes([first_a, first_b])),
        u32::from(u16::from_le_bytes([second_a, second_b])),
        u32::from(u16::from_le_bytes([third_a, third_b])),
        u32::from(u16::from_le_bytes([fourth_a, fourth_b])),
        u32::from(u16::from_le_bytes([fifth_a, fifth_b])),
        u32::from(u16::from_le_bytes([sixth_a, sixth_b])),
    ])
}

fn decode_snapshot_row_fields_u24(row_bytes: &[u8]) -> Result<[u32; 6], SnapshotIoError> {
    let [
        first_a,
        first_b,
        first_c,
        second_a,
        second_b,
        second_c,
        third_a,
        third_b,
        third_c,
        fourth_a,
        fourth_b,
        fourth_c,
        fifth_a,
        fifth_b,
        fifth_c,
        sixth_a,
        sixth_b,
        sixth_c,
    ] = row_bytes_to_array(row_bytes)?;
    Ok([
        u32::from_le_bytes([first_a, first_b, first_c, 0]),
        u32::from_le_bytes([second_a, second_b, second_c, 0]),
        u32::from_le_bytes([third_a, third_b, third_c, 0]),
        u32::from_le_bytes([fourth_a, fourth_b, fourth_c, 0]),
        u32::from_le_bytes([fifth_a, fifth_b, fifth_c, 0]),
        u32::from_le_bytes([sixth_a, sixth_b, sixth_c, 0]),
    ])
}

fn decode_snapshot_row_fields_u32(row_bytes: &[u8]) -> Result<[u32; 6], SnapshotIoError> {
    let [
        first_a,
        first_b,
        first_c,
        first_d,
        second_a,
        second_b,
        second_c,
        second_d,
        third_a,
        third_b,
        third_c,
        third_d,
        fourth_a,
        fourth_b,
        fourth_c,
        fourth_d,
        fifth_a,
        fifth_b,
        fifth_c,
        fifth_d,
        sixth_a,
        sixth_b,
        sixth_c,
        sixth_d,
    ] = row_bytes_to_array(row_bytes)?;
    Ok([
        u32::from_le_bytes([first_a, first_b, first_c, first_d]),
        u32::from_le_bytes([second_a, second_b, second_c, second_d]),
        u32::from_le_bytes([third_a, third_b, third_c, third_d]),
        u32::from_le_bytes([fourth_a, fourth_b, fourth_c, fourth_d]),
        u32::from_le_bytes([fifth_a, fifth_b, fifth_c, fifth_d]),
        u32::from_le_bytes([sixth_a, sixth_b, sixth_c, sixth_d]),
    ])
}

fn row_bytes_to_array<const LEN: usize>(row_bytes: &[u8]) -> Result<[u8; LEN], SnapshotIoError> {
    row_bytes.try_into().map_err(|_| SnapshotIoError::Format {
        reason: "relationship row width is invalid",
    })
}

fn validate_row_domains(
    interner: &IdentifierInterner,
    row: &RelationshipRow,
) -> Result<(), SnapshotIoError> {
    ObjectType::try_from(interner.resolve(row.resource_type.0)?)?;
    ObjectId::try_from(interner.resolve(row.resource_id.0)?)?;
    RelationName::try_from(interner.resolve(row.relation.0)?)?;
    SubjectType::try_from(interner.resolve(row.subject_type.0)?)?;
    SubjectId::try_from(interner.resolve(row.subject_id.0)?)?;
    if let Some(relation) = row.subject_relation {
        RelationName::try_from(interner.resolve(relation.0)?)?;
    }
    Ok(())
}

#[derive(Debug, Clone)]
struct LiveRows {
    all_live_len: Option<usize>,
    words: Vec<u64>,
}

impl Default for LiveRows {
    fn default() -> Self {
        Self {
            all_live_len: Some(0),
            words: Vec::new(),
        }
    }
}

impl LiveRows {
    fn full(len: usize) -> Self {
        Self {
            all_live_len: Some(len),
            words: Vec::new(),
        }
    }

    fn sparse_full(len: usize) -> Self {
        let word_count = len.div_ceil(u64::BITS as usize);
        let mut words = vec![u64::MAX; word_count];
        let remainder = len % u64::BITS as usize;
        if remainder != 0
            && let Some(last) = words.last_mut()
        {
            *last = (1_u64 << remainder) - 1;
        }
        Self {
            all_live_len: None,
            words,
        }
    }

    fn insert(&mut self, row_id: RowId) {
        let index = row_id.index();
        if let Some(len) = self.all_live_len {
            if index == len {
                self.all_live_len = len.checked_add(1);
                return;
            }
            if index < len {
                return;
            }
            *self = Self::sparse_full(len);
        }
        let word = index / u64::BITS as usize;
        if word >= self.words.len() {
            self.words.resize(word.saturating_add(1), 0);
        }
        if let Some(value) = self.words.get_mut(word) {
            *value |= 1_u64 << (index % u64::BITS as usize);
        }
    }

    fn remove(&mut self, row_id: RowId) {
        let index = row_id.index();
        if let Some(len) = self.all_live_len {
            *self = Self::sparse_full(len);
        }
        let word = index / u64::BITS as usize;
        if let Some(value) = self.words.get_mut(word) {
            *value &= !(1_u64 << (index % u64::BITS as usize));
        }
    }

    fn contains(&self, row_id: RowId) -> bool {
        let index = row_id.index();
        if let Some(len) = self.all_live_len {
            return index < len;
        }
        let word = index / u64::BITS as usize;
        self.words
            .get(word)
            .is_some_and(|value| value & (1_u64 << (index % u64::BITS as usize)) != 0)
    }

    fn is_all_live(&self) -> bool {
        self.all_live_len.is_some()
    }
}

fn compaction_dead_row_threshold() -> usize {
    if cfg!(test) { 16 } else { COMPACT_DEAD_ROWS }
}

fn relationship_identifier_values(relationship: &Relationship, mut visit: impl FnMut(&str)) {
    visit(relationship.resource().object_type().as_str());
    visit(relationship.resource().object_id().as_str());
    visit(relationship.relation().as_str());
    match relationship.subject() {
        SubjectRef::Object(object) => {
            visit(object.object_type().as_str());
            visit(object.object_id().as_str());
        }
        SubjectRef::Userset { object, relation } => {
            visit(object.object_type().as_str());
            visit(object.object_id().as_str());
            visit(relation.as_str());
        }
    }
}

fn hash_value<T: Hash + ?Sized>(value: &T) -> u64 {
    let mut state = DefaultHasher::new();
    value.hash(&mut state);
    state.finish()
}
