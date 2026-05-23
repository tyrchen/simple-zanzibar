//! Indexed in-memory relationship store and mutation semantics.

use std::{
    collections::{
        HashMap, HashSet,
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
};

const DEFAULT_QUERY_LIMIT: usize = 1_000;
const MAX_MUTATIONS_PER_BATCH: usize = 10_000;
const MAX_PRECONDITIONS_PER_BATCH: usize = 100;
const COMPACT_DEAD_ROWS: usize = 100_000;

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
struct PostingIndex<K> {
    primary: HashMap<K, RowId>,
    overflow: HashMap<K, Vec<RowId>>,
}

impl<K> Default for PostingIndex<K> {
    fn default() -> Self {
        Self {
            primary: HashMap::new(),
            overflow: HashMap::new(),
        }
    }
}

impl<K> PostingIndex<K>
where
    K: Copy + Eq + Hash,
{
    fn insert(&mut self, key: K, row_id: RowId) {
        match self.primary.entry(key) {
            Entry::Vacant(entry) => {
                entry.insert(row_id);
            }
            Entry::Occupied(_) => {
                self.overflow.entry(key).or_default().push(row_id);
            }
        }
    }

    fn candidates(&self, key: &K) -> CandidateRowIds<'_> {
        match (self.primary.get(key).copied(), self.overflow.get(key)) {
            (Some(row_id), Some(row_ids)) => CandidateRowIds::OneThenSlice {
                first: Some(row_id),
                rest: row_ids.iter(),
            },
            (Some(row_id), None) => CandidateRowIds::One(Some(row_id)),
            (None, Some(row_ids)) => CandidateRowIds::Slice(row_ids.iter()),
            (None, None) => CandidateRowIds::Empty,
        }
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
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
