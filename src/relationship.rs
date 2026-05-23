//! Indexed in-memory relationship store and mutation semantics.

use std::collections::{BTreeSet, HashMap, HashSet, btree_set};
use std::num::NonZeroUsize;

use thiserror::Error;

use crate::domain::{
    ObjectId, ObjectRef, ObjectType, RelationName, Relationship, SubjectId, SubjectRef, SubjectType,
};
use crate::error::ZanzibarError;
use crate::model::User;

const DEFAULT_QUERY_LIMIT: usize = 1_000;
const MAX_MUTATIONS_PER_BATCH: usize = 10_000;
const MAX_PRECONDITIONS_PER_BATCH: usize = 100;

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
#[derive(Debug, Clone, Default)]
pub struct IndexedRelationshipStore {
    uniqueness: HashSet<Relationship>,
    rows: Vec<Relationship>,
    by_resource: HashMap<ResourceIndexKey, BTreeSet<usize>>,
    by_resource_object: HashMap<ObjectRef, BTreeSet<usize>>,
    by_resource_type_relation: HashMap<ResourceTypeRelationIndexKey, BTreeSet<usize>>,
    by_resource_type: HashMap<ObjectType, BTreeSet<usize>>,
    by_subject: HashMap<SubjectIndexKey, BTreeSet<usize>>,
    by_subject_type_relation: HashMap<SubjectTypeRelationIndexKey, BTreeSet<usize>>,
    by_subject_type: HashMap<SubjectType, BTreeSet<usize>>,
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
            return self.apply_single_mutation(mutation);
        }

        let mut candidate = self.clone();
        for mutation in mutations {
            match mutation {
                RelationshipMutation::Create(relationship) => candidate.create(relationship)?,
                RelationshipMutation::Touch(relationship) => {
                    if !candidate.uniqueness.contains(&relationship) {
                        candidate.insert(relationship);
                    }
                }
                RelationshipMutation::Delete(relationship) => candidate.delete(&relationship)?,
            }
        }

        *self = candidate;
        Ok(())
    }

    /// Returns true when at least one resource-side relationship matches.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] when the underlying reader cannot evaluate the filter.
    pub fn any_resource_match(&self, filter: &RelationshipFilter) -> Result<bool, StoreError> {
        Ok(self.query_relationships(filter)?.next().is_some())
    }

    /// Returns all rows. Intended for tests and migration checks.
    #[must_use]
    pub fn rows(&self) -> &[Relationship] {
        &self.rows
    }

    fn check_precondition(&self, precondition: &Precondition) -> Result<(), StoreError> {
        match precondition {
            Precondition::MustMatch(filter) if !self.any_resource_match(filter)? => {
                Err(StoreError::PreconditionFailed {
                    precondition: Box::new(precondition.clone()),
                })
            }
            Precondition::MustNotMatch(filter) if self.any_resource_match(filter)? => {
                Err(StoreError::PreconditionFailed {
                    precondition: Box::new(precondition.clone()),
                })
            }
            _ => Ok(()),
        }
    }

    fn create(&mut self, relationship: Relationship) -> Result<(), StoreError> {
        if self.uniqueness.contains(&relationship) {
            return Err(StoreError::RelationshipAlreadyExists {
                relationship: Box::new(relationship),
            });
        }
        self.insert(relationship);
        Ok(())
    }

    fn apply_single_mutation(&mut self, mutation: RelationshipMutation) -> Result<(), StoreError> {
        match mutation {
            RelationshipMutation::Create(relationship) => self.create(relationship),
            RelationshipMutation::Touch(relationship) => {
                if !self.uniqueness.contains(&relationship) {
                    self.insert(relationship);
                }
                Ok(())
            }
            RelationshipMutation::Delete(relationship) => self.delete(&relationship),
        }
    }

    fn insert(&mut self, relationship: Relationship) {
        let index = self.rows.len();
        self.uniqueness.insert(relationship.clone());
        self.index_relationship(index, &relationship);
        self.rows.push(relationship);
    }

    fn delete(&mut self, relationship: &Relationship) -> Result<(), StoreError> {
        if !self.uniqueness.remove(relationship) {
            return Err(StoreError::RelationshipNotFound {
                relationship: Box::new(relationship.clone()),
            });
        }

        if let Some(index) = self.rows.iter().position(|row| row == relationship) {
            let removed = self.rows.swap_remove(index);
            self.deindex_relationship(index, &removed);
            if index < self.rows.len() {
                let moved = self.rows.get(index).cloned().ok_or_else(|| {
                    StoreError::RelationshipNotFound {
                        relationship: Box::new(relationship.clone()),
                    }
                })?;
                self.deindex_relationship(self.rows.len(), &moved);
                self.index_relationship(index, &moved);
            }
        }

        Ok(())
    }

    fn resource_candidate_indexes(&self, filter: &RelationshipFilter) -> CandidateIndexes<'_> {
        match (&filter.optional_resource_id, &filter.optional_relation) {
            (Some(resource_id), Some(relation)) => {
                let key = ResourceIndexKey {
                    object: ObjectRef::new(filter.resource_type.clone(), resource_id.clone()),
                    relation: relation.clone(),
                };
                set_candidates(self.by_resource.get(&key))
            }
            (Some(resource_id), None) => {
                let object = ObjectRef::new(filter.resource_type.clone(), resource_id.clone());
                set_candidates(self.by_resource_object.get(&object))
            }
            (None, Some(relation)) => {
                let key = ResourceTypeRelationIndexKey {
                    object_type: filter.resource_type.clone(),
                    relation: relation.clone(),
                };
                set_candidates(self.by_resource_type_relation.get(&key))
            }
            (None, None) => set_candidates(self.by_resource_type.get(&filter.resource_type)),
        }
    }

    fn subject_candidate_indexes(&self, filter: &SubjectFilter) -> CandidateIndexes<'_> {
        match (&filter.optional_subject_id, &filter.optional_relation) {
            (Some(subject_id), relation) => {
                let key = SubjectIndexKey {
                    subject_type: filter.subject_type.clone(),
                    subject_id: subject_id.clone(),
                    relation: relation.clone(),
                };
                set_candidates(self.by_subject.get(&key))
            }
            (None, Some(relation)) => {
                let key = SubjectTypeRelationIndexKey {
                    subject_type: filter.subject_type.clone(),
                    relation: relation.clone(),
                };
                set_candidates(self.by_subject_type_relation.get(&key))
            }
            (None, None) => set_candidates(self.by_subject_type.get(&filter.subject_type)),
        }
    }

    fn index_relationship(&mut self, index: usize, relationship: &Relationship) {
        self.by_resource
            .entry(ResourceIndexKey::from(relationship))
            .or_default()
            .insert(index);
        self.by_resource_object
            .entry(relationship.resource().clone())
            .or_default()
            .insert(index);
        self.by_resource_type_relation
            .entry(ResourceTypeRelationIndexKey::from(relationship))
            .or_default()
            .insert(index);
        self.by_resource_type
            .entry(relationship.resource().object_type().clone())
            .or_default()
            .insert(index);
        for key in SubjectIndexKey::from_relationship(relationship) {
            self.by_subject.entry(key).or_default().insert(index);
        }
        for key in SubjectTypeRelationIndexKey::from_relationship(relationship) {
            self.by_subject_type_relation
                .entry(key)
                .or_default()
                .insert(index);
        }
        self.by_subject_type
            .entry(subject_type(relationship.subject()))
            .or_default()
            .insert(index);
    }

    fn deindex_relationship(&mut self, index: usize, relationship: &Relationship) {
        remove_index(
            &mut self.by_resource,
            &ResourceIndexKey::from(relationship),
            index,
        );
        remove_index(&mut self.by_resource_object, relationship.resource(), index);
        remove_index(
            &mut self.by_resource_type_relation,
            &ResourceTypeRelationIndexKey::from(relationship),
            index,
        );
        remove_index(
            &mut self.by_resource_type,
            relationship.resource().object_type(),
            index,
        );
        for key in SubjectIndexKey::from_relationship(relationship) {
            remove_index(&mut self.by_subject, &key, index);
        }
        for key in SubjectTypeRelationIndexKey::from_relationship(relationship) {
            remove_index(&mut self.by_subject_type_relation, &key, index);
        }
        remove_index(
            &mut self.by_subject_type,
            &subject_type(relationship.subject()),
            index,
        );
    }
}

impl RelationshipReader for IndexedRelationshipStore {
    type Iter<'a> = RelationshipIter<'a>;

    fn query_relationships(
        &self,
        filter: &RelationshipFilter,
    ) -> Result<Self::Iter<'_>, StoreError> {
        Ok(RelationshipIter {
            rows: &self.rows,
            candidates: self.resource_candidate_indexes(filter),
            matcher: RelationshipMatcher::Resource {
                filter: filter.clone(),
                remaining: filter.limit.get(),
            },
        })
    }

    fn reverse_query_relationships(
        &self,
        filter: &SubjectFilter,
    ) -> Result<Self::Iter<'_>, StoreError> {
        Ok(RelationshipIter {
            rows: &self.rows,
            candidates: self.subject_candidate_indexes(filter),
            matcher: RelationshipMatcher::Subject {
                filter: filter.clone(),
            },
        })
    }
}

/// Iterator over indexed relationship query results.
#[derive(Debug)]
pub struct RelationshipIter<'a> {
    rows: &'a [Relationship],
    candidates: CandidateIndexes<'a>,
    matcher: RelationshipMatcher,
}

impl<'a> Iterator for RelationshipIter<'a> {
    type Item = &'a Relationship;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let index = self.candidates.next()?;
            let relationship = self.rows.get(index)?;
            match &mut self.matcher {
                RelationshipMatcher::Resource { filter, remaining } => {
                    if *remaining == 0 {
                        return None;
                    }
                    if relationship_matches_filter(relationship, filter) {
                        *remaining = remaining.saturating_sub(1);
                        return Some(relationship);
                    }
                }
                RelationshipMatcher::Subject { filter } => {
                    if subject_matches_filter(relationship.subject(), filter) {
                        return Some(relationship);
                    }
                }
            }
        }
    }
}

#[derive(Debug)]
enum CandidateIndexes<'a> {
    Empty,
    Set(btree_set::Iter<'a, usize>),
}

impl Iterator for CandidateIndexes<'_> {
    type Item = usize;

    fn next(&mut self) -> Option<Self::Item> {
        match self {
            Self::Empty => None,
            Self::Set(indexes) => indexes.next().copied(),
        }
    }
}

#[derive(Debug)]
enum RelationshipMatcher {
    Resource {
        filter: RelationshipFilter,
        remaining: usize,
    },
    Subject {
        filter: SubjectFilter,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
struct ResourceIndexKey {
    object: ObjectRef,
    relation: RelationName,
}

impl From<&Relationship> for ResourceIndexKey {
    fn from(value: &Relationship) -> Self {
        Self {
            object: value.resource().clone(),
            relation: value.relation().clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
struct ResourceTypeRelationIndexKey {
    object_type: ObjectType,
    relation: RelationName,
}

impl From<&Relationship> for ResourceTypeRelationIndexKey {
    fn from(value: &Relationship) -> Self {
        Self {
            object_type: value.resource().object_type().clone(),
            relation: value.relation().clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
struct SubjectIndexKey {
    subject_type: SubjectType,
    subject_id: SubjectId,
    relation: Option<RelationName>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
struct SubjectTypeRelationIndexKey {
    subject_type: SubjectType,
    relation: RelationName,
}

impl SubjectTypeRelationIndexKey {
    fn from_relationship(relationship: &Relationship) -> Vec<Self> {
        match relationship.subject() {
            SubjectRef::Object(_) => Vec::new(),
            SubjectRef::Userset { object, relation } => vec![Self {
                subject_type: SubjectType::from(object.object_type()),
                relation: relation.clone(),
            }],
        }
    }
}

impl SubjectIndexKey {
    fn from_relationship(relationship: &Relationship) -> Vec<Self> {
        match relationship.subject() {
            SubjectRef::Object(object) => {
                let exact = Self {
                    subject_type: SubjectType::from(object.object_type()),
                    subject_id: SubjectId::from(object.object_id()),
                    relation: None,
                };
                vec![exact]
            }
            SubjectRef::Userset { object, relation } => {
                let subject_type = SubjectType::from(object.object_type());
                let subject_id = SubjectId::from(object.object_id());
                vec![
                    Self {
                        subject_type: subject_type.clone(),
                        subject_id: subject_id.clone(),
                        relation: Some(relation.clone()),
                    },
                    Self {
                        subject_type,
                        subject_id,
                        relation: None,
                    },
                ]
            }
        }
    }
}

fn relationship_matches_filter(relationship: &Relationship, filter: &RelationshipFilter) -> bool {
    relationship.resource().object_type() == &filter.resource_type
        && filter
            .optional_resource_id
            .as_ref()
            .is_none_or(|resource_id| relationship.resource().object_id() == resource_id)
        && filter
            .optional_relation
            .as_ref()
            .is_none_or(|relation| relationship.relation() == relation)
        && filter
            .optional_subject
            .as_ref()
            .is_none_or(|subject| subject_matches_filter(relationship.subject(), subject))
}

fn subject_matches_filter(subject: &SubjectRef, filter: &SubjectFilter) -> bool {
    match subject {
        SubjectRef::Object(object) => {
            filter.optional_relation.is_none()
                && object.object_type().as_str() == filter.subject_type.as_str()
                && filter
                    .optional_subject_id
                    .as_ref()
                    .is_none_or(|subject_id| object.object_id().as_str() == subject_id.as_str())
        }
        SubjectRef::Userset { object, relation } => {
            object.object_type().as_str() == filter.subject_type.as_str()
                && filter
                    .optional_subject_id
                    .as_ref()
                    .is_none_or(|subject_id| object.object_id().as_str() == subject_id.as_str())
                && filter
                    .optional_relation
                    .as_ref()
                    .is_none_or(|expected_relation| relation == expected_relation)
        }
    }
}

fn remove_index<K>(map: &mut HashMap<K, BTreeSet<usize>>, key: &K, index: usize)
where
    K: Eq + std::hash::Hash,
{
    let should_remove = if let Some(indexes) = map.get_mut(key) {
        indexes.remove(&index);
        indexes.is_empty()
    } else {
        false
    };
    if should_remove {
        map.remove(key);
    }
}

fn set_candidates(indexes: Option<&BTreeSet<usize>>) -> CandidateIndexes<'_> {
    indexes.map_or(CandidateIndexes::Empty, |indexes| {
        CandidateIndexes::Set(indexes.iter())
    })
}

fn subject_type(subject: &SubjectRef) -> SubjectType {
    match subject {
        SubjectRef::Object(object) | SubjectRef::Userset { object, .. } => {
            SubjectType::from(object.object_type())
        }
    }
}
