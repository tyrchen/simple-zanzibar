//! A simplified Rust implementation of Google's Zanzibar authorization system.
//!
//! ```
//! use simple_zanzibar::model::{
//!     ExpandedUserset, LookupResourcesRequest, LookupSubjectsRequest, Object, Relation,
//!     RelationTuple, User,
//! };
//! use simple_zanzibar::revision::Consistency;
//! use simple_zanzibar::ZanzibarService;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let mut service = ZanzibarService::new();
//! service.add_dsl(
//!     r"
//!     namespace doc {
//!         relation viewer {}
//!     }
//!     ",
//! )?;
//!
//! let doc = Object {
//!     namespace: "doc".to_string(),
//!     id: "readme".to_string(),
//! };
//! let viewer = Relation("viewer".to_string());
//! let alice = User::UserId("alice".to_string());
//! let token = service.write_tuple_with_token(&RelationTuple {
//!     object: doc.clone(),
//!     relation: viewer.clone(),
//!     user: alice.clone(),
//! })?;
//!
//! assert!(service.check(&doc, &viewer, &alice)?);
//! assert!(service.check_with_consistency(&doc, &viewer, &alice, Consistency::Exact(token))?);
//! assert!(matches!(
//!     service.expand(&doc, &viewer)?,
//!     ExpandedUserset::Union(_)
//! ));
//! assert_eq!(
//!     service
//!         .lookup_resources(&LookupResourcesRequest {
//!             subject: alice.clone(),
//!             permission: viewer.clone(),
//!             resource_type: "doc".to_string(),
//!         })?
//!         .resources,
//!     vec![doc.clone()],
//! );
//! assert_eq!(
//!     service
//!         .lookup_subjects(&LookupSubjectsRequest {
//!             resource: doc,
//!             permission: viewer,
//!             subject_type: "user".to_string(),
//!         })?
//!         .subjects,
//!     vec![alice],
//! );
//! # Ok(())
//! # }
//! ```

#![forbid(unsafe_code)]
#![warn(rust_2024_compatibility, missing_docs, missing_debug_implementations)]

pub mod api;
pub mod domain;
pub mod error;
pub mod eval;
pub mod model;
pub mod parser;
pub mod relationship;
pub mod revision;
pub mod schema;
pub mod store;

use std::{
    collections::{HashMap, HashSet, VecDeque},
    fmt,
    num::NonZeroUsize,
    sync::Arc,
};

use arc_swap::ArcSwapOption;

pub use crate::api::{EngineError, ZanzibarEngine, ZanzibarEngineBuilder};
use crate::{
    error::ZanzibarError,
    eval::EvaluationLimits,
    model::{
        LookupResources, LookupResourcesRequest, LookupSubjects, LookupSubjectsRequest,
        NamespaceConfig, Object, Relation, RelationTuple, User,
    },
    relationship::{
        IndexedRelationshipStore, Precondition, RelationshipFilter, RelationshipMutation,
        SubjectFilter,
    },
    revision::{
        Consistency, ConsistencyError, ConsistencyToken, DatastoreId, PublishedSnapshot, Revision,
        SchemaHash, default_retained_snapshots,
    },
    schema::CompiledSchema,
    store::{InMemoryTupleStore, TupleStore},
};

/// Compatibility facade for the local Zanzibar engine.
///
/// This type preserves the original `ZanzibarService` API while delegating schema-backed reads and
/// writes to the typed schema, indexed relationship store, and revision snapshot engine. New code
/// should prefer the request/response methods on this facade; Phase 6 keeps the legacy tuple/config
/// helpers only as a migration boundary.
pub struct ZanzibarService {
    configs: HashMap<String, NamespaceConfig>,
    schema: Option<CompiledSchema>,
    relationships: Arc<IndexedRelationshipStore>,
    current_snapshot: ArcSwapOption<PublishedSnapshot>,
    snapshot_history: VecDeque<Arc<PublishedSnapshot>>,
    datastore_id: DatastoreId,
    retained_snapshots: NonZeroUsize,
    last_revision: Option<Revision>,
    evaluation_limits: EvaluationLimits,
    store: Box<dyn TupleStore>,
}

impl fmt::Debug for ZanzibarService {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ZanzibarService")
            .field("configs", &self.configs)
            .field("schema", &self.schema)
            .field("relationships", &self.relationships)
            .field("current_snapshot", &self.current_snapshot)
            .field("snapshot_history", &self.snapshot_history)
            .field("datastore_id", &self.datastore_id)
            .field("retained_snapshots", &self.retained_snapshots)
            .field("last_revision", &self.last_revision)
            .field("evaluation_limits", &self.evaluation_limits)
            .field("store", &"<dyn TupleStore>")
            .finish()
    }
}

impl Default for ZanzibarService {
    fn default() -> Self {
        Self::new()
    }
}

impl ZanzibarService {
    /// Creates a new service with an in-memory store.
    #[must_use]
    pub fn new() -> Self {
        ZanzibarService {
            configs: HashMap::new(),
            schema: None,
            relationships: Arc::new(IndexedRelationshipStore::default()),
            current_snapshot: ArcSwapOption::empty(),
            snapshot_history: VecDeque::new(),
            datastore_id: DatastoreId::new_unique(),
            retained_snapshots: default_retained_snapshots(),
            last_revision: None,
            evaluation_limits: EvaluationLimits::default(),
            store: Box::new(InMemoryTupleStore::default()),
        }
    }

    /// Creates a new service with a custom exact-snapshot retention window.
    #[must_use]
    pub fn with_snapshot_retention(retained_snapshots: NonZeroUsize) -> Self {
        let mut service = Self::new();
        service.retained_snapshots = retained_snapshots;
        service
    }

    /// Sets evaluation recursion and fanout limits.
    #[must_use]
    pub fn with_evaluation_limits(mut self, limits: EvaluationLimits) -> Self {
        self.evaluation_limits = limits;
        self
    }

    /// Parses a DSL string and adds the resulting configurations to the service.
    ///
    /// # Errors
    ///
    /// Returns [`ZanzibarError::ParseError`] when the DSL cannot be parsed.
    pub fn add_dsl(&mut self, dsl: &str) -> Result<(), ZanzibarError> {
        self.add_dsl_with_token(dsl).map(|_| ())
    }

    /// Parses a DSL string, adds the resulting configurations, and returns a consistency token.
    ///
    /// # Errors
    ///
    /// Returns [`ZanzibarError::ParseError`] when the DSL cannot be parsed.
    pub fn add_dsl_with_token(&mut self, dsl: &str) -> Result<ConsistencyToken, ZanzibarError> {
        schema::compile_legacy_dsl(dsl)?;
        let configs = parser::parse_dsl(dsl)?;
        let mut next_configs = self.configs.clone();
        for config in configs {
            next_configs.insert(config.name.clone(), config);
        }
        let compiled_schema = schema::compile_legacy_configs(next_configs.values().cloned())?;
        let next_relationships = self.relationship_store_for_schema(&compiled_schema)?;
        self.publish_snapshot(next_configs, compiled_schema, next_relationships)
    }

    /// Adds or updates a namespace configuration.
    ///
    /// # Errors
    ///
    /// Returns [`ZanzibarError::Schema`] when the updated schema does not validate.
    pub fn add_config(&mut self, config: NamespaceConfig) -> Result<(), ZanzibarError> {
        self.add_config_with_token(config).map(|_| ())
    }

    /// Adds or updates a namespace configuration and returns a consistency token.
    ///
    /// # Errors
    ///
    /// Returns [`ZanzibarError::Schema`] when the updated schema does not validate.
    pub fn add_config_with_token(
        &mut self,
        config: NamespaceConfig,
    ) -> Result<ConsistencyToken, ZanzibarError> {
        let mut next_configs = self.configs.clone();
        next_configs.insert(config.name.clone(), config);
        let compiled_schema = schema::compile_legacy_configs(next_configs.values().cloned())?;
        let next_relationships = self.relationship_store_for_schema(&compiled_schema)?;
        self.publish_snapshot(next_configs, compiled_schema, next_relationships)
    }

    /// Writes a relation tuple to the store.
    ///
    /// # Errors
    ///
    /// Returns [`ZanzibarError::StorageError`] when the underlying store rejects the write.
    pub fn write_tuple(&mut self, tuple: RelationTuple) -> Result<(), ZanzibarError> {
        if self.schema.is_some() {
            return self.write_tuple_with_token(&tuple).map(|_| ());
        }

        self.store
            .write_tuple(tuple)
            .map_err(ZanzibarError::StorageError)
    }

    /// Writes a relation tuple and returns a consistency token.
    ///
    /// # Errors
    ///
    /// Returns [`ZanzibarError::SchemaRequired`] when no schema has been loaded.
    pub fn write_tuple_with_token(
        &mut self,
        tuple: &RelationTuple,
    ) -> Result<ConsistencyToken, ZanzibarError> {
        let relationship = domain::Relationship::try_from(tuple)?;
        self.apply_relationship_mutations([RelationshipMutation::Create(relationship)], [])
    }

    /// Applies a validated batch of relationship mutations.
    ///
    /// ```
    /// use simple_zanzibar::domain::Relationship;
    /// use simple_zanzibar::relationship::{
    ///     Precondition, RelationshipFilter, RelationshipMutation, SubjectFilter,
    /// };
    /// use simple_zanzibar::ZanzibarService;
    ///
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// let mut service = ZanzibarService::new();
    /// service.add_dsl(
    ///     r"
    ///     namespace doc {
    ///         relation viewer {}
    ///     }
    ///     ",
    /// )?;
    ///
    /// let relationship: Relationship = "doc:readme#viewer@user:alice".parse()?;
    /// let subject = SubjectFilter::exact("user".try_into()?, "alice".try_into()?, None);
    /// let precondition = Precondition::MustNotMatch(RelationshipFilter::for_exact_subject(
    ///     relationship.resource(),
    ///     relationship.relation().clone(),
    ///     subject,
    /// ));
    /// service.apply_relationship_mutations(
    ///     [RelationshipMutation::Touch(relationship)],
    ///     [precondition],
    /// )?;
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// # Errors
    ///
    /// Returns [`ZanzibarError::SchemaRequired`] when no schema has been loaded, or a typed
    /// validation/store error when any relationship, precondition, or mutation semantic is invalid.
    pub fn apply_relationship_mutations(
        &mut self,
        mutations: impl IntoIterator<Item = RelationshipMutation>,
        preconditions: impl IntoIterator<Item = Precondition>,
    ) -> Result<ConsistencyToken, ZanzibarError> {
        let schema = self.schema.clone().ok_or(ZanzibarError::SchemaRequired)?;
        let mutations = mutations.into_iter().collect::<Vec<_>>();
        let preconditions = preconditions.into_iter().collect::<Vec<_>>();

        for mutation in &mutations {
            schema.validate_relationship(mutation.relationship())?;
        }
        for precondition in &preconditions {
            validate_precondition_filter(&schema, precondition)?;
        }

        let mut candidate = (*self.relationships).clone();
        candidate.apply_mutations(mutations, preconditions)?;
        self.publish_snapshot(self.configs.clone(), schema, Arc::new(candidate))
    }

    /// Deletes a relation tuple from the store.
    ///
    /// # Errors
    ///
    /// Returns [`ZanzibarError::StorageError`] when the underlying store rejects the delete.
    pub fn delete_tuple(&mut self, tuple: &RelationTuple) -> Result<(), ZanzibarError> {
        if self.schema.is_some() {
            return self.delete_tuple_with_token(tuple).map(|_| ());
        }

        self.store
            .delete_tuple(tuple)
            .map_err(ZanzibarError::StorageError)
    }

    /// Deletes a relation tuple and returns a consistency token.
    ///
    /// # Errors
    ///
    /// Returns [`ZanzibarError::SchemaRequired`] when no schema has been loaded.
    pub fn delete_tuple_with_token(
        &mut self,
        tuple: &RelationTuple,
    ) -> Result<ConsistencyToken, ZanzibarError> {
        let relationship = domain::Relationship::try_from(tuple)?;
        self.apply_relationship_mutations([RelationshipMutation::Delete(relationship)], [])
    }

    /// Checks if a user has a specific relation to an object.
    ///
    /// # Errors
    ///
    /// Returns [`ZanzibarError::NamespaceNotFound`] when the object's namespace has not been
    /// configured, or [`ZanzibarError::RelationNotFound`] when the relation is missing from that
    /// namespace.
    pub fn check(
        &self,
        object: &Object,
        relation: &Relation,
        user: &User,
    ) -> Result<bool, ZanzibarError> {
        if self.schema.is_some() {
            return self.check_with_consistency(object, relation, user, Consistency::Latest);
        }

        if !self.configs.contains_key(&object.namespace) {
            return Err(ZanzibarError::NamespaceNotFound(object.namespace.clone()));
        }

        eval::check_with_configs(
            object,
            relation,
            user,
            &self.configs,
            self.store.as_ref(),
            &mut HashSet::new(),
        )
    }

    /// Checks if a user has a relation to an object at the requested consistency.
    ///
    /// # Errors
    ///
    /// Returns typed consistency, domain, schema, or evaluation errors when the check cannot run.
    pub fn check_with_consistency(
        &self,
        object: &Object,
        relation: &Relation,
        user: &User,
        consistency: Consistency,
    ) -> Result<bool, ZanzibarError> {
        let snapshot = self.snapshot_for_consistency(consistency)?;
        let resource = domain::ObjectRef::try_from(object)?;
        let relation_name = domain::RelationName::try_from(relation.0.as_str())?;
        snapshot
            .schema()
            .resolver()
            .relation(resource.object_type(), &relation_name)?;
        Ok(
            eval::check_with_snapshot(&snapshot, object, relation, user, self.evaluation_limits)?
                .is_allowed(),
        )
    }

    /// Expands the userset for a given object and relation.
    ///
    /// # Errors
    ///
    /// Returns [`ZanzibarError::NamespaceNotFound`] when the object's namespace has not been
    /// configured, or [`ZanzibarError::RelationNotFound`] when the relation is missing from that
    /// namespace.
    pub fn expand(
        &self,
        object: &Object,
        relation: &Relation,
    ) -> Result<model::ExpandedUserset, ZanzibarError> {
        if self.schema.is_some() {
            return self.expand_with_consistency(object, relation, Consistency::Latest);
        }

        if !self.configs.contains_key(&object.namespace) {
            return Err(ZanzibarError::NamespaceNotFound(object.namespace.clone()));
        }
        eval::expand_with_configs(object, relation, &self.configs, self.store.as_ref())
    }

    /// Expands the userset for a given object and relation at the requested consistency.
    ///
    /// # Errors
    ///
    /// Returns typed consistency, domain, schema, or evaluation errors when the expand cannot run.
    pub fn expand_with_consistency(
        &self,
        object: &Object,
        relation: &Relation,
        consistency: Consistency,
    ) -> Result<model::ExpandedUserset, ZanzibarError> {
        let snapshot = self.snapshot_for_consistency(consistency)?;
        let object_type = domain::ObjectType::try_from(object.namespace.as_str())?;
        let relation_name = domain::RelationName::try_from(relation.0.as_str())?;
        snapshot
            .schema()
            .resolver()
            .relation(&object_type, &relation_name)?;
        eval::expand_with_snapshot(&snapshot, object, relation, self.evaluation_limits)
    }

    /// Looks up resources of a type that the request subject can access.
    ///
    /// # Errors
    ///
    /// Returns [`ZanzibarError::SchemaRequired`] when no schema has been loaded, or typed
    /// consistency, domain, schema, store, or evaluation errors when lookup cannot run.
    pub fn lookup_resources(
        &self,
        request: &LookupResourcesRequest,
    ) -> Result<LookupResources, ZanzibarError> {
        self.lookup_resources_with_consistency(request, Consistency::Latest)
    }

    /// Looks up resources of a type at the requested consistency.
    ///
    /// # Errors
    ///
    /// Returns [`ZanzibarError::SchemaRequired`] when no schema has been loaded, or typed
    /// consistency, domain, schema, store, or evaluation errors when lookup cannot run.
    pub fn lookup_resources_with_consistency(
        &self,
        request: &LookupResourcesRequest,
        consistency: Consistency,
    ) -> Result<LookupResources, ZanzibarError> {
        let snapshot = self.snapshot_for_consistency(consistency)?;
        eval::lookup_resources_with_snapshot(&snapshot, request, self.evaluation_limits)
    }

    /// Looks up subjects of a type that can access the request resource.
    ///
    /// # Errors
    ///
    /// Returns [`ZanzibarError::SchemaRequired`] when no schema has been loaded, or typed
    /// consistency, domain, schema, store, or evaluation errors when lookup cannot run.
    pub fn lookup_subjects(
        &self,
        request: &LookupSubjectsRequest,
    ) -> Result<LookupSubjects, ZanzibarError> {
        self.lookup_subjects_with_consistency(request, Consistency::Latest)
    }

    /// Looks up subjects of a type at the requested consistency.
    ///
    /// # Errors
    ///
    /// Returns [`ZanzibarError::SchemaRequired`] when no schema has been loaded, or typed
    /// consistency, domain, schema, store, or evaluation errors when lookup cannot run.
    pub fn lookup_subjects_with_consistency(
        &self,
        request: &LookupSubjectsRequest,
        consistency: Consistency,
    ) -> Result<LookupSubjects, ZanzibarError> {
        let snapshot = self.snapshot_for_consistency(consistency)?;
        eval::lookup_subjects_with_snapshot(&snapshot, request, self.evaluation_limits)
    }

    fn relationship_store_for_schema(
        &self,
        schema: &CompiledSchema,
    ) -> Result<Arc<IndexedRelationshipStore>, ZanzibarError> {
        if self.schema.is_some() {
            return self.revalidate_relationship_store(schema);
        }
        self.rebuild_relationship_store_from_legacy_tuples(schema)
    }

    fn revalidate_relationship_store(
        &self,
        schema: &CompiledSchema,
    ) -> Result<Arc<IndexedRelationshipStore>, ZanzibarError> {
        let mut relationships = IndexedRelationshipStore::default();
        for relationship in self.relationships.rows() {
            schema.validate_relationship(&relationship)?;
            relationships.apply_mutations([RelationshipMutation::Touch(relationship)], [])?;
        }
        Ok(Arc::new(relationships))
    }

    fn rebuild_relationship_store_from_legacy_tuples(
        &self,
        schema: &CompiledSchema,
    ) -> Result<Arc<IndexedRelationshipStore>, ZanzibarError> {
        let mut relationships = IndexedRelationshipStore::default();
        for tuple in self.store.all_tuples() {
            let relationship = domain::Relationship::try_from(&tuple)?;
            schema.validate_relationship(&relationship)?;
            relationships.apply_mutations([RelationshipMutation::Touch(relationship)], [])?;
        }
        Ok(Arc::new(relationships))
    }

    fn publish_snapshot(
        &mut self,
        configs: HashMap<String, NamespaceConfig>,
        schema: CompiledSchema,
        relationships: Arc<IndexedRelationshipStore>,
    ) -> Result<ConsistencyToken, ZanzibarError> {
        let revision = self.next_revision()?;
        let schema_hash = SchemaHash::for_schema(&schema);
        let snapshot = Arc::new(PublishedSnapshot::new(
            revision,
            schema_hash,
            Arc::new(configs.clone()),
            Arc::new(schema.clone()),
            Arc::clone(&relationships),
        ));
        let token = ConsistencyToken::new(revision, schema_hash, self.datastore_id);

        self.configs = configs;
        self.schema = Some(schema);
        self.relationships = relationships;
        self.store.replace_all(Vec::new());
        self.current_snapshot.store(Some(Arc::clone(&snapshot)));
        self.snapshot_history.push_back(snapshot);
        while self.snapshot_history.len() > self.retained_snapshots.get() {
            self.snapshot_history.pop_front();
        }
        self.last_revision = Some(revision);
        Ok(token)
    }

    fn next_revision(&self) -> Result<Revision, ConsistencyError> {
        match self.last_revision {
            Some(revision) => revision.next(),
            None => Ok(Revision::first()),
        }
    }

    fn snapshot_for_consistency(
        &self,
        consistency: Consistency,
    ) -> Result<Arc<PublishedSnapshot>, ZanzibarError> {
        match consistency {
            Consistency::Latest => self
                .current_snapshot
                .load_full()
                .ok_or(ZanzibarError::SchemaRequired),
            Consistency::Exact(token) => {
                if token.datastore_id() != self.datastore_id {
                    return Err(ConsistencyError::WrongDatastore.into());
                }
                if self
                    .last_revision
                    .is_none_or(|latest| token.revision() > latest)
                {
                    return Err(ConsistencyError::RevisionUnavailable {
                        revision: token.revision(),
                    }
                    .into());
                }
                if let Some(oldest) = self.snapshot_history.front()
                    && token.revision() < oldest.revision()
                {
                    return Err(ConsistencyError::RevisionExpired {
                        revision: token.revision(),
                    }
                    .into());
                }
                let snapshot = self
                    .snapshot_history
                    .iter()
                    .find(|snapshot| snapshot.revision() == token.revision())
                    .cloned()
                    .ok_or(ConsistencyError::RevisionUnavailable {
                        revision: token.revision(),
                    })?;
                if snapshot.schema_hash() != token.schema_hash() {
                    return Err(ConsistencyError::SchemaHashMismatch {
                        revision: token.revision(),
                    }
                    .into());
                }
                Ok(snapshot)
            }
        }
    }
}

fn validate_precondition_filter(
    schema: &CompiledSchema,
    precondition: &Precondition,
) -> Result<(), ZanzibarError> {
    match precondition {
        Precondition::MustMatch(filter) | Precondition::MustNotMatch(filter) => {
            validate_relationship_filter(schema, filter)
        }
    }
}

fn validate_relationship_filter(
    schema: &CompiledSchema,
    filter: &RelationshipFilter,
) -> Result<(), ZanzibarError> {
    schema.resolver().namespace(filter.resource_type())?;
    if let Some(relation) = filter.optional_relation() {
        schema
            .resolver()
            .relation(filter.resource_type(), relation)?;
    }
    if let Some(subject) = filter.optional_subject() {
        validate_subject_filter(schema, subject)?;
    }
    Ok(())
}

fn validate_subject_filter(
    schema: &CompiledSchema,
    filter: &SubjectFilter,
) -> Result<(), ZanzibarError> {
    if let Some(relation) = filter.optional_relation() {
        let object_type = domain::ObjectType::try_from(filter.subject_type().as_str())?;
        schema.resolver().relation(&object_type, relation)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn it_works() {
        assert_eq!(2 + 2, 4);
    }
}
