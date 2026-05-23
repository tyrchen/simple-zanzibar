//! Indexed in-memory relationship store and mutation semantics.

use std::{
    collections::{
        BTreeMap, HashMap, HashSet,
        hash_map::{DefaultHasher, Entry},
    },
    hash::{Hash, Hasher},
    num::{NonZeroU32, NonZeroUsize},
    str,
};

use thiserror::Error;

use crate::{
    domain::{
        DomainError, ObjectId, ObjectRef, ObjectType, RelationName, Relationship, SubjectId,
        SubjectRef, SubjectType,
    },
    error::ZanzibarError,
    model::User,
    snapshot::{
        BinaryCursor, SectionKind, SnapshotIoError, SnapshotLoadProfile, SnapshotReader,
        SnapshotSectionWriter, checked_add_usize, checked_mul_usize, checked_u32_from_usize,
        checked_usize_from_u32, checked_usize_from_u64, insert_unique,
    },
};

const DEFAULT_QUERY_LIMIT: usize = 1_000;
const MAX_MUTATIONS_PER_BATCH: usize = 10_000;
const MAX_PRECONDITIONS_PER_BATCH: usize = 100;
const COMPACT_DEAD_ROWS: usize = 100_000;
const DISK_SYMBOL_LEN: usize = 8;
const DISK_RELATIONSHIP_ROW_LEN: usize = 24;
const DISK_INDEX_DIRECTORY_LEN: usize = 20;
const DISK_INDEX_KEY_LEN: usize = 12;
const DISK_POSTING_RANGE_LEN: usize = 12;
const DISK_ROW_ID_LEN: usize = 4;
const SNAPSHOT_INDEX_KIND_COUNT: usize = 7;
const SNAPSHOT_INDEX_KIND_COUNT_U64: u64 = SNAPSHOT_INDEX_KIND_COUNT as u64;

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
#[derive(Debug, Default)]
pub struct IndexedRelationshipStore {
    interner: IdentifierInterner,
    rows: Vec<RelationshipRow>,
    live_rows: LiveRows,
    dead_row_count: usize,
    uniqueness: RelationshipIdentityIndex,
    by_resource: PostingIndex<ResourceIndexKey>,
    by_resource_object: PostingIndex<ResourceObjectIndexKey>,
    by_resource_type_relation: PostingIndex<ResourceTypeRelationIndexKey>,
    by_resource_type: PostingIndex<ObjectTypeId>,
    by_subject: PostingIndex<SubjectIndexKey>,
    by_subject_type_relation: PostingIndex<SubjectTypeRelationIndexKey>,
    by_subject_type: PostingIndex<SubjectTypeId>,
}

impl Clone for IndexedRelationshipStore {
    fn clone(&self) -> Self {
        Self {
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

    pub(crate) fn encode_snapshot_sections(
        &self,
        writer: &mut SnapshotSectionWriter,
    ) -> Result<(), SnapshotIoError> {
        let mut symbol_table = Vec::with_capacity(
            self.interner
                .entries
                .len()
                .checked_mul(DISK_SYMBOL_LEN)
                .ok_or(SnapshotIoError::Format {
                    reason: "symbol table length overflowed",
                })?,
        );
        for entry in &self.interner.entries {
            symbol_table.extend_from_slice(&entry.start.to_le_bytes());
            symbol_table.extend_from_slice(&entry.len.to_le_bytes());
        }
        writer.add_section(
            SectionKind::SymbolBytes,
            self.interner.bytes.clone(),
            u64::try_from(self.interner.bytes.len()).map_err(|_| {
                SnapshotIoError::LimitExceeded {
                    component: "symbol bytes",
                }
            })?,
        )?;
        writer.add_section(
            SectionKind::SymbolTable,
            symbol_table,
            u64::try_from(self.interner.entries.len()).map_err(|_| {
                SnapshotIoError::LimitExceeded {
                    component: "symbol table",
                }
            })?,
        )?;

        let disk_rows = self.live_disk_rows();
        let mut rows = Vec::with_capacity(
            disk_rows
                .len()
                .checked_mul(DISK_RELATIONSHIP_ROW_LEN)
                .ok_or(SnapshotIoError::Format {
                    reason: "relationship rows length overflowed",
                })?,
        );
        for row in &disk_rows {
            row.encode(&mut rows);
        }
        writer.add_section(
            SectionKind::RelationshipRows,
            rows,
            u64::try_from(disk_rows.len()).map_err(|_| SnapshotIoError::LimitExceeded {
                component: "relationship rows",
            })?,
        )?;

        let indexes = EncodedSnapshotIndexes::from_rows(&disk_rows)?;
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
    ) -> Result<Self, SnapshotIoError> {
        let interner = IdentifierInterner::decode_snapshot_sections(reader)?;
        let (rows, live_rows, uniqueness) = decode_snapshot_rows(reader, &interner)?;
        let decoded_indexes = DecodedSnapshotIndexes::decode(reader, &rows, profile)?;
        Ok(Self {
            interner,
            rows,
            live_rows,
            dead_row_count: 0,
            uniqueness,
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
            candidates,
            matcher: matcher.map(CompactRelationshipMatcher::Subject),
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
        self.uniqueness.insert(&self.rows, row_id, &row);
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
        let row_id = self.uniqueness.remove(&self.rows, &row).ok_or_else(|| {
            StoreError::RelationshipNotFound {
                relationship: Box::new(relationship.clone()),
            }
        })?;
        self.live_rows.remove(row_id);
        self.dead_row_count = self.dead_row_count.saturating_add(1);

        Ok(())
    }

    fn contains_relationship(&self, relationship: &Relationship) -> bool {
        self.lookup_relationship_row(relationship)
            .is_some_and(|row| self.uniqueness.find(&self.rows, &row).is_some())
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
        self.uniqueness.insert(&self.rows, row_id, &compacted);
        self.index_relationship(row_id, &compacted);
        self.rows.push(compacted);
        self.live_rows.insert(row_id);
        Ok(())
    }

    fn resolve(&self, id: SymbolId) -> &str {
        self.interner.resolve(id).unwrap_or("<invalid>")
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
    candidates: CandidateRowIds<'a>,
    matcher: Option<RelationshipMatcher>,
}

impl<'a> Iterator for RelationshipIter<'a> {
    type Item = &'a Relationship;

    fn next(&mut self) -> Option<Self::Item> {
        let matcher = self.matcher.as_mut()?;
        loop {
            let row_id = self.candidates.next()?;
            if !self.live_rows.contains(row_id) {
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
    candidates: CandidateRowIds<'a>,
    matcher: Option<CompactRelationshipMatcher>,
}

impl<'a> Iterator for CompactRelationshipIter<'a> {
    type Item = RelationshipRef<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        let matcher = self.matcher.as_mut()?;
        loop {
            let row_id = self.candidates.next()?;
            if !self.store.live_rows.contains(row_id) {
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
    pub(crate) fn resource_object_legacy(&self) -> crate::model::Object {
        crate::model::Object {
            namespace: self.store.resolve(self.row.resource_type.0).to_string(),
            id: self.store.resolve(self.row.resource_id.0).to_string(),
        }
    }

    pub(crate) fn resource_type_eq(&self, expected: &ObjectType) -> bool {
        self.store.resolve(self.row.resource_type.0) == expected.as_str()
    }

    pub(crate) fn relation_legacy(&self) -> crate::model::Relation {
        crate::model::Relation(self.store.resolve(self.row.relation.0).to_string())
    }

    pub(crate) fn subject_userset_legacy(
        &self,
    ) -> Option<(crate::model::Object, crate::model::Relation)> {
        self.row.subject_relation.map(|relation| {
            (
                crate::model::Object {
                    namespace: self.store.resolve(self.row.subject_type.0).to_string(),
                    id: self.store.resolve(self.row.subject_id.0).to_string(),
                },
                crate::model::Relation(self.store.resolve(relation.0).to_string()),
            )
        })
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
}

impl IdentifierInterner {
    fn decode_snapshot_sections(reader: &SnapshotReader<'_>) -> Result<Self, SnapshotIoError> {
        let header = reader.header();
        let bytes = reader.section(SectionKind::SymbolBytes)?;
        let table = reader.section(SectionKind::SymbolTable)?;
        let symbol_count = checked_usize_from_u32(header.symbol_count)?;
        if table.row_count() != u64::from(header.symbol_count) {
            return Err(SnapshotIoError::Format {
                reason: "symbol table row count does not match header",
            });
        }
        let expected_len = checked_mul_usize(symbol_count, DISK_SYMBOL_LEN)?;
        if table.bytes().len() != expected_len {
            return Err(SnapshotIoError::Format {
                reason: "symbol table length does not match symbol count",
            });
        }

        let mut cursor = BinaryCursor::new(table.bytes());
        let mut entries = Vec::with_capacity(symbol_count);
        for _ in 0..symbol_count {
            let start = cursor.read_u32()?;
            let len = cursor.read_u32()?;
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
        Self::from_snapshot_parts(bytes.bytes().to_vec(), entries)
    }

    fn from_snapshot_parts(
        bytes: Vec<u8>,
        entries: Vec<InternedString>,
    ) -> Result<Self, SnapshotIoError> {
        let mut interner = Self {
            bytes,
            entries,
            ids_by_hash: HashMap::new(),
            hash_collisions: HashMap::new(),
        };
        for index in 0..interner.entries.len() {
            let id = SymbolId::from_index(index)?;
            let (hash, hash_exists, duplicate) = {
                let value = interner.resolve(id)?;
                let hash = hash_value(value);
                (
                    hash,
                    interner.ids_by_hash.contains_key(&hash),
                    interner.has_symbol_value(hash, value)?,
                )
            };
            if duplicate {
                return Err(SnapshotIoError::Format {
                    reason: "duplicate symbol in snapshot",
                });
            }
            if hash_exists {
                interner.hash_collisions.entry(hash).or_default().push(id);
            } else {
                interner.ids_by_hash.insert(hash, id);
            }
        }
        Ok(interner)
    }

    fn has_symbol_value(&self, hash: u64, value: &str) -> Result<bool, StoreError> {
        if let Some(id) = self.ids_by_hash.get(&hash).copied()
            && self.resolve(id)? == value
        {
            return Ok(true);
        }
        if let Some(ids) = self.hash_collisions.get(&hash) {
            for id in ids {
                if self.resolve(*id)? == value {
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }

    fn intern(&mut self, value: &str) -> Result<SymbolId, StoreError> {
        if let Some(id) = self.lookup(value) {
            return Ok(id);
        }
        let id = SymbolId::from_index(self.entries.len())?;
        let hash = hash_value(value);
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
}

#[derive(Debug, Clone, Copy)]
struct InternedString {
    start: u32,
    len: u32,
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
    fn encode(&self, target: &mut Vec<u8>) {
        target.extend_from_slice(&self.resource_type.to_le_bytes());
        target.extend_from_slice(&self.resource_id.to_le_bytes());
        target.extend_from_slice(&self.relation.to_le_bytes());
        target.extend_from_slice(&self.subject_type.to_le_bytes());
        target.extend_from_slice(&self.subject_id.to_le_bytes());
        target.extend_from_slice(&self.subject_relation.to_le_bytes());
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
    fn encode(&self, target: &mut Vec<u8>) {
        target.extend_from_slice(&self.first.to_le_bytes());
        target.extend_from_slice(&self.second.to_le_bytes());
        target.extend_from_slice(&self.third.to_le_bytes());
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
    fn from_rows(rows: &[DiskRelationshipRow]) -> Result<Self, SnapshotIoError> {
        let mut groups = SnapshotIndexGroups::default();
        for (index, row) in rows.iter().copied().enumerate() {
            let row_id =
                checked_u32_from_usize(index.checked_add(1).ok_or(SnapshotIoError::Format {
                    reason: "row id overflowed",
                })?)?;
            groups.insert_row(row, row_id);
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
            let key_start = key_count;
            let range_start = range_count;
            for (key, row_ids) in group {
                key.encode(&mut keys);
                let range = encode_posting_range(row_ids, &mut posting_row_ids)?;
                range.encode(&mut ranges);
                key_count = key_count.checked_add(1).ok_or(SnapshotIoError::Format {
                    reason: "index key count overflowed",
                })?;
                range_count = range_count.checked_add(1).ok_or(SnapshotIoError::Format {
                    reason: "posting range count overflowed",
                })?;
                posting_row_id_count =
                    u32::try_from(posting_row_ids.len().checked_div(DISK_ROW_ID_LEN).ok_or(
                        SnapshotIoError::Format {
                            reason: "posting row id length overflowed",
                        },
                    )?)
                    .map_err(|_| SnapshotIoError::LimitExceeded {
                        component: "posting row ids",
                    })?;
            }
            let group_len = checked_u32_from_usize(group.len())?;
            directory.extend_from_slice(&kind.raw().to_le_bytes());
            directory.extend_from_slice(&0_u16.to_le_bytes());
            directory.extend_from_slice(&key_start.to_le_bytes());
            directory.extend_from_slice(&group_len.to_le_bytes());
            directory.extend_from_slice(&range_start.to_le_bytes());
            directory.extend_from_slice(&group_len.to_le_bytes());
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
    fn insert_row(&mut self, row: DiskRelationshipRow, row_id: u32) {
        self.resource
            .entry(DiskIndexKey {
                first: row.resource_type,
                second: row.resource_id,
                third: row.relation,
            })
            .or_default()
            .push(row_id);
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

fn encode_posting_range(
    row_ids: &[u32],
    posting_row_ids: &mut Vec<u8>,
) -> Result<DiskPostingRange, SnapshotIoError> {
    let (first, rest) = row_ids.split_first().ok_or(SnapshotIoError::Format {
        reason: "empty posting list",
    })?;
    let overflow_start =
        checked_u32_from_usize(posting_row_ids.len().checked_div(DISK_ROW_ID_LEN).ok_or(
            SnapshotIoError::Format {
                reason: "posting row id length overflowed",
            },
        )?)?;
    let overflow_len = checked_u32_from_usize(rest.len())?;
    for row_id in rest {
        posting_row_ids.extend_from_slice(&row_id.to_le_bytes());
    }
    Ok(DiskPostingRange {
        first_row_id: *first,
        overflow_start,
        overflow_len,
    })
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
    ) -> Result<Self, SnapshotIoError> {
        let directory = decode_index_directory(reader)?;
        let keys = decode_index_keys(reader)?;
        let ranges = decode_posting_ranges(reader)?;
        let posting_row_ids = decode_posting_row_ids(reader)?;
        let row_count = reader.header().relationship_count;
        let input = SnapshotIndexDecodeInput {
            directory: &directory,
            keys: &keys,
            ranges: &ranges,
            posting_row_ids: &posting_row_ids,
            rows,
            row_count,
            profile,
        };

        Ok(Self {
            resource: decode_index(
                &input,
                &SnapshotIndexDecoder {
                    kind: SnapshotIndexKind::Resource,
                    key_from_disk: resource_key_from_disk,
                    row_matches_key: row_matches_resource_key,
                    coverage_bit: simple_index_coverage_bit,
                    expected_mask: |_| 1,
                },
            )?,
            resource_object: decode_index(
                &input,
                &SnapshotIndexDecoder {
                    kind: SnapshotIndexKind::ResourceObject,
                    key_from_disk: resource_object_key_from_disk,
                    row_matches_key: row_matches_resource_object_key,
                    coverage_bit: simple_index_coverage_bit,
                    expected_mask: |_| 1,
                },
            )?,
            resource_type_relation: decode_index(
                &input,
                &SnapshotIndexDecoder {
                    kind: SnapshotIndexKind::ResourceTypeRelation,
                    key_from_disk: resource_type_relation_key_from_disk,
                    row_matches_key: row_matches_resource_type_relation_key,
                    coverage_bit: simple_index_coverage_bit,
                    expected_mask: |_| 1,
                },
            )?,
            resource_type: decode_index(
                &input,
                &SnapshotIndexDecoder {
                    kind: SnapshotIndexKind::ResourceType,
                    key_from_disk: resource_type_key_from_disk,
                    row_matches_key: row_matches_resource_type_key,
                    coverage_bit: simple_index_coverage_bit,
                    expected_mask: |_| 1,
                },
            )?,
            subject: decode_index(
                &input,
                &SnapshotIndexDecoder {
                    kind: SnapshotIndexKind::Subject,
                    key_from_disk: subject_key_from_disk,
                    row_matches_key: row_matches_subject_key,
                    coverage_bit: subject_index_coverage_bit,
                    expected_mask: |row| if row.subject_relation.is_some() { 3 } else { 1 },
                },
            )?,
            subject_type_relation: decode_index(
                &input,
                &SnapshotIndexDecoder {
                    kind: SnapshotIndexKind::SubjectTypeRelation,
                    key_from_disk: subject_type_relation_key_from_disk,
                    row_matches_key: row_matches_subject_type_relation_key,
                    coverage_bit: simple_index_coverage_bit,
                    expected_mask: |row| u8::from(row.subject_relation.is_some()),
                },
            )?,
            subject_type: decode_index(
                &input,
                &SnapshotIndexDecoder {
                    kind: SnapshotIndexKind::SubjectType,
                    key_from_disk: subject_type_key_from_disk,
                    row_matches_key: row_matches_subject_type_key,
                    coverage_bit: simple_index_coverage_bit,
                    expected_mask: |_| 1,
                },
            )?,
        })
    }
}

#[derive(Debug, Clone, Copy)]
struct DiskIndexDirectoryEntry {
    kind: SnapshotIndexKind,
    key_start: u32,
    key_count: u32,
    posting_range_start: u32,
    posting_range_count: u32,
}

#[derive(Debug, Clone, Copy)]
struct SnapshotIndexDecodeInput<'a> {
    directory: &'a [DiskIndexDirectoryEntry],
    keys: &'a [DiskIndexKey],
    ranges: &'a [DiskPostingRange],
    posting_row_ids: &'a [RowId],
    rows: &'a [RelationshipRow],
    row_count: u32,
    profile: SnapshotLoadProfile,
}

#[derive(Debug, Clone, Copy)]
struct SnapshotIndexSlices<'a> {
    keys: &'a [DiskIndexKey],
    ranges: &'a [DiskPostingRange],
}

struct SnapshotIndexDecoder<K> {
    kind: SnapshotIndexKind,
    key_from_disk: fn(DiskIndexKey) -> Result<K, SnapshotIoError>,
    row_matches_key: fn(&RelationshipRow, DiskIndexKey) -> bool,
    coverage_bit: fn(&RelationshipRow, DiskIndexKey) -> u8,
    expected_mask: fn(&RelationshipRow) -> u8,
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
        if flags != 0 {
            return Err(SnapshotIoError::Format {
                reason: "index directory flags are unsupported",
            });
        }
        insert_unique(&mut seen, kind, "duplicate snapshot index kind")?;
        entries.push(DiskIndexDirectoryEntry {
            kind,
            key_start: cursor.read_u32()?,
            key_count: cursor.read_u32()?,
            posting_range_start: cursor.read_u32()?,
            posting_range_count: cursor.read_u32()?,
        });
    }
    Ok(entries)
}

fn decode_index_keys(reader: &SnapshotReader<'_>) -> Result<Vec<DiskIndexKey>, SnapshotIoError> {
    let section = reader.section(SectionKind::IndexKeys)?;
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
    Ok(keys)
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

fn decode_posting_row_ids(reader: &SnapshotReader<'_>) -> Result<Vec<RowId>, SnapshotIoError> {
    let section = reader.section(SectionKind::PostingRowIds)?;
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
    Ok(row_ids)
}

fn decode_index<K>(
    input: &SnapshotIndexDecodeInput<'_>,
    decoder: &SnapshotIndexDecoder<K>,
) -> Result<PostingIndex<K>, SnapshotIoError>
where
    K: Copy + Eq + Hash + Ord,
{
    let slices = snapshot_index_slices(input, decoder.kind)?;

    let mut coverage = vec![0_u8; input.rows.len()];
    let mut sorted_keys = Vec::with_capacity(slices.keys.len());
    let mut sorted_ranges = Vec::with_capacity(slices.ranges.len());
    let mut sorted_overflow = Vec::new();
    let mut latency_index = PostingIndex::default();

    for (disk_key, range) in slices
        .keys
        .iter()
        .copied()
        .zip(slices.ranges.iter().copied())
    {
        let typed_key = (decoder.key_from_disk)(disk_key)?;
        let row_ids = posting_row_id_iter(range, input.posting_row_ids, input.row_count)?;
        let overflow_start = checked_u32_from_usize(sorted_overflow.len())?;
        let mut overflow_len = 0_u32;
        let mut first = None;
        for row_id in row_ids {
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

    for (row, actual) in input.rows.iter().zip(coverage.iter().copied()) {
        if actual != (decoder.expected_mask)(row) {
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

fn snapshot_index_slices<'a>(
    input: &'a SnapshotIndexDecodeInput<'_>,
    kind: SnapshotIndexKind,
) -> Result<SnapshotIndexSlices<'a>, SnapshotIoError> {
    let entry = input
        .directory
        .iter()
        .find(|entry| entry.kind == kind)
        .copied()
        .ok_or(SnapshotIoError::Format {
            reason: "missing snapshot index kind",
        })?;
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
    let keys = input
        .keys
        .get(key_start..key_end)
        .ok_or(SnapshotIoError::Format {
            reason: "index key range is out of bounds",
        })?;
    let ranges = input
        .ranges
        .get(range_start..range_end)
        .ok_or(SnapshotIoError::Format {
            reason: "posting range span is out of bounds",
        })?;
    validate_sorted_keys(keys)?;
    Ok(SnapshotIndexSlices { keys, ranges })
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
    Many {
        first: Option<RowId>,
        rest: std::slice::Iter<'a, RowId>,
    },
}

impl Iterator for SnapshotPostingRowIds<'_> {
    type Item = RowId;

    fn next(&mut self) -> Option<Self::Item> {
        match self {
            Self::One { first } => first.take(),
            Self::Many { first, rest } => first.take().or_else(|| rest.next().copied()),
        }
    }
}

fn posting_row_id_iter(
    range: DiskPostingRange,
    posting_row_ids: &[RowId],
    row_count: u32,
) -> Result<SnapshotPostingRowIds<'_>, SnapshotIoError> {
    let first = RowId::from_snapshot_raw(range.first_row_id, row_count)?;
    if range.overflow_len == 0 {
        return Ok(SnapshotPostingRowIds::One { first: Some(first) });
    }
    let start = checked_usize_from_u32(range.overflow_start)?;
    let len = checked_usize_from_u32(range.overflow_len)?;
    let end = checked_add_usize(start, len)?;
    let overflow = posting_row_ids
        .get(start..end)
        .ok_or(SnapshotIoError::Format {
            reason: "posting range points outside posting row ids",
        })?;
    Ok(SnapshotPostingRowIds::Many {
        first: Some(first),
        rest: overflow.iter(),
    })
}

fn resource_key_from_disk(key: DiskIndexKey) -> Result<ResourceIndexKey, SnapshotIoError> {
    Ok(ResourceIndexKey {
        object_type: ObjectTypeId(SymbolId::from_snapshot_raw(key.first, u32::MAX)?),
        object_id: ObjectIdId(SymbolId::from_snapshot_raw(key.second, u32::MAX)?),
        relation: RelationId(SymbolId::from_snapshot_raw(key.third, u32::MAX)?),
    })
}

fn resource_object_key_from_disk(
    key: DiskIndexKey,
) -> Result<ResourceObjectIndexKey, SnapshotIoError> {
    ensure_zero(
        key.third,
        "resource object index third key field must be zero",
    )?;
    Ok(ResourceObjectIndexKey {
        object_type: ObjectTypeId(SymbolId::from_snapshot_raw(key.first, u32::MAX)?),
        object_id: ObjectIdId(SymbolId::from_snapshot_raw(key.second, u32::MAX)?),
    })
}

fn resource_type_relation_key_from_disk(
    key: DiskIndexKey,
) -> Result<ResourceTypeRelationIndexKey, SnapshotIoError> {
    ensure_zero(
        key.third,
        "resource type relation index third key field must be zero",
    )?;
    Ok(ResourceTypeRelationIndexKey {
        object_type: ObjectTypeId(SymbolId::from_snapshot_raw(key.first, u32::MAX)?),
        relation: RelationId(SymbolId::from_snapshot_raw(key.second, u32::MAX)?),
    })
}

fn resource_type_key_from_disk(key: DiskIndexKey) -> Result<ObjectTypeId, SnapshotIoError> {
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
        u32::MAX,
    )?))
}

fn subject_key_from_disk(key: DiskIndexKey) -> Result<SubjectIndexKey, SnapshotIoError> {
    let relation = if key.third == 0 {
        None
    } else {
        Some(RelationId(SymbolId::from_snapshot_raw(
            key.third,
            u32::MAX,
        )?))
    };
    Ok(SubjectIndexKey {
        subject_type: SubjectTypeId(SymbolId::from_snapshot_raw(key.first, u32::MAX)?),
        subject_id: SubjectIdId(SymbolId::from_snapshot_raw(key.second, u32::MAX)?),
        relation,
    })
}

fn subject_type_relation_key_from_disk(
    key: DiskIndexKey,
) -> Result<SubjectTypeRelationIndexKey, SnapshotIoError> {
    ensure_zero(
        key.third,
        "subject type relation index third key field must be zero",
    )?;
    Ok(SubjectTypeRelationIndexKey {
        subject_type: SubjectTypeId(SymbolId::from_snapshot_raw(key.first, u32::MAX)?),
        relation: RelationId(SymbolId::from_snapshot_raw(key.second, u32::MAX)?),
    })
}

fn subject_type_key_from_disk(key: DiskIndexKey) -> Result<SubjectTypeId, SnapshotIoError> {
    ensure_zero(
        key.second,
        "subject type index second key field must be zero",
    )?;
    ensure_zero(key.third, "subject type index third key field must be zero")?;
    Ok(SubjectTypeId(SymbolId::from_snapshot_raw(
        key.first,
        u32::MAX,
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

fn decode_snapshot_rows(
    reader: &SnapshotReader<'_>,
    interner: &IdentifierInterner,
) -> Result<(Vec<RelationshipRow>, LiveRows, RelationshipIdentityIndex), SnapshotIoError> {
    let header = reader.header();
    let section = reader.section(SectionKind::RelationshipRows)?;
    let row_count = checked_usize_from_u32(header.relationship_count)?;
    if section.row_count() != u64::from(header.relationship_count) {
        return Err(SnapshotIoError::Format {
            reason: "relationship row count does not match header",
        });
    }
    let expected_len = checked_mul_usize(row_count, DISK_RELATIONSHIP_ROW_LEN)?;
    if section.bytes().len() != expected_len {
        return Err(SnapshotIoError::Format {
            reason: "relationship row length does not match row count",
        });
    }
    let mut cursor = BinaryCursor::new(section.bytes());
    let mut rows = Vec::with_capacity(row_count);
    let mut live_rows = LiveRows::default();
    let mut uniqueness = RelationshipIdentityIndex::default();
    for index in 0..row_count {
        let row_id = RowId::from_len(index)?;
        let row = RelationshipRow {
            row_id,
            resource_type: ObjectTypeId(SymbolId::from_snapshot_raw(
                cursor.read_u32()?,
                header.symbol_count,
            )?),
            resource_id: ObjectIdId(SymbolId::from_snapshot_raw(
                cursor.read_u32()?,
                header.symbol_count,
            )?),
            relation: RelationId(SymbolId::from_snapshot_raw(
                cursor.read_u32()?,
                header.symbol_count,
            )?),
            subject_type: SubjectTypeId(SymbolId::from_snapshot_raw(
                cursor.read_u32()?,
                header.symbol_count,
            )?),
            subject_id: SubjectIdId(SymbolId::from_snapshot_raw(
                cursor.read_u32()?,
                header.symbol_count,
            )?),
            subject_relation: match cursor.read_u32()? {
                0 => None,
                value => Some(RelationId(SymbolId::from_snapshot_raw(
                    value,
                    header.symbol_count,
                )?)),
            },
        };
        validate_row_domains(interner, &row)?;
        if uniqueness.find(&rows, &row).is_some() {
            return Err(SnapshotIoError::Format {
                reason: "duplicate relationship row in snapshot",
            });
        }
        uniqueness.insert(&rows, row_id, &row);
        rows.push(row);
        live_rows.insert(row_id);
    }
    Ok((rows, live_rows, uniqueness))
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

#[derive(Debug, Clone, Default)]
struct LiveRows {
    words: Vec<u64>,
}

impl LiveRows {
    fn insert(&mut self, row_id: RowId) {
        let index = row_id.index();
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
        let word = index / u64::BITS as usize;
        if let Some(value) = self.words.get_mut(word) {
            *value &= !(1_u64 << (index % u64::BITS as usize));
        }
    }

    fn contains(&self, row_id: RowId) -> bool {
        let index = row_id.index();
        let word = index / u64::BITS as usize;
        self.words
            .get(word)
            .is_some_and(|value| value & (1_u64 << (index % u64::BITS as usize)) != 0)
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
