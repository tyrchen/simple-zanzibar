//! A simplified Rust implementation of Google's Zanzibar authorization system.
//!
//! ```
//! use simple_zanzibar::model::{
//!     ExpandedUserset, LookupResourcesRequest, LookupSubjectsRequest, Object, Relation,
//!     RelationTuple, User,
//! };
//! use simple_zanzibar::revision::Consistency;
//! use simple_zanzibar::ZanzibarEngine;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let service = ZanzibarEngine::builder().build();
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
//! assert!(service.check_relation(&doc, &viewer, &alice)?);
//! assert!(service.check_with_consistency(&doc, &viewer, &alice, Consistency::Exact(token))?);
//! assert!(matches!(
//!     service.expand_relation(&doc, &viewer)?,
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
pub mod policy;
pub mod relationship;
pub mod revision;
mod runtime;
pub mod schema;
pub mod snapshot;
pub mod store;

use std::{
    collections::{HashMap, VecDeque},
    fmt,
    num::NonZeroUsize,
    path::Path,
    sync::Arc,
};

use arc_swap::ArcSwapOption;

pub use crate::{
    api::{EngineError, TenantId, ZanzibarEngine, ZanzibarEngineBuilder, ZanzibarTenantShards},
    policy::{PolicyIoError, PolicyText, PolicyTextFile},
    snapshot::{
        IndexProfile, SnapshotCompression, SnapshotIntegrityMode, SnapshotIoError,
        SnapshotLoadOptions, SnapshotLoadProfile, SnapshotSaveOptions, SnapshotValidationMode,
    },
};
use crate::{
    error::ZanzibarError,
    eval::EvaluationLimits,
    model::{NamespaceConfig, Relation},
    relationship::{
        IndexedRelationshipStore, Precondition, RelationshipFilter, RelationshipMutation,
        RelationshipStoreView, SubjectFilter,
    },
    revision::{
        ConsistencyError, ConsistencyToken, DatastoreId, PublishedSnapshot, Revision, SchemaHash,
        default_retained_snapshots,
    },
    runtime::{EngineState, SharedEngineState},
    schema::CompiledSchema,
};

pub(crate) struct WriterState {
    configs: HashMap<String, NamespaceConfig>,
    schema: Option<CompiledSchema>,
    relationships: Arc<RelationshipStoreView>,
    current_snapshot: ArcSwapOption<PublishedSnapshot>,
    snapshot_history: VecDeque<Arc<PublishedSnapshot>>,
    datastore_id: DatastoreId,
    retained_snapshots: NonZeroUsize,
    last_revision: Option<Revision>,
    evaluation_limits: EvaluationLimits,
    published_state: SharedEngineState,
}

impl fmt::Debug for WriterState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WriterState")
            .field("configs", &self.configs)
            .field("schema", &self.schema)
            .field("relationships", &self.relationships)
            .field("current_snapshot", &self.current_snapshot)
            .field("snapshot_history", &self.snapshot_history)
            .field("datastore_id", &self.datastore_id)
            .field("retained_snapshots", &self.retained_snapshots)
            .field("last_revision", &self.last_revision)
            .field("evaluation_limits", &self.evaluation_limits)
            .field("published_state", &self.published_state)
            .finish()
    }
}

impl Default for WriterState {
    fn default() -> Self {
        Self::new()
    }
}

impl WriterState {
    #[must_use]
    pub(crate) fn new() -> Self {
        Self::new_with_publisher(Arc::new(ArcSwapOption::empty()))
    }

    #[must_use]
    pub(crate) fn new_with_publisher(published_state: SharedEngineState) -> Self {
        WriterState {
            configs: HashMap::new(),
            schema: None,
            relationships: Arc::new(RelationshipStoreView::default()),
            current_snapshot: ArcSwapOption::empty(),
            snapshot_history: VecDeque::new(),
            datastore_id: DatastoreId::new_unique(),
            retained_snapshots: default_retained_snapshots(),
            last_revision: None,
            evaluation_limits: EvaluationLimits::default(),
            published_state,
        }
    }

    #[must_use]
    pub(crate) fn with_snapshot_retention(retained_snapshots: NonZeroUsize) -> Self {
        let mut service = Self::new();
        service.retained_snapshots = retained_snapshots;
        service
    }

    #[must_use]
    pub(crate) fn with_snapshot_retention_and_publisher(
        retained_snapshots: NonZeroUsize,
        published_state: SharedEngineState,
    ) -> Self {
        let mut service = Self::new_with_publisher(published_state);
        service.retained_snapshots = retained_snapshots;
        service
    }

    #[must_use]
    pub(crate) fn with_evaluation_limits(mut self, limits: EvaluationLimits) -> Self {
        self.evaluation_limits = limits;
        self
    }

    /// Builds a new service from canonical or hand-authored policy text.
    ///
    /// Relationship files accept one relationship per line. Blank lines and full-line `#` or `//`
    /// comments are ignored.
    ///
    /// # Errors
    ///
    /// Returns [`ZanzibarError`] when the schema, relationship text, or relationship semantics are
    /// invalid.
    pub fn from_policy_text(policy: &PolicyText) -> Result<Self, ZanzibarError> {
        let mut service = Self::new();
        service.apply_policy_text(policy)?;
        Ok(service)
    }

    /// Replaces this service with the state described by policy text and returns the final token.
    ///
    /// The replacement is atomic with respect to validation: if schema parsing or relationship
    /// application fails, the previous service state remains unchanged.
    ///
    /// # Errors
    ///
    /// Returns [`ZanzibarError`] when policy text cannot be parsed or validated.
    pub fn apply_policy_text(
        &mut self,
        policy: &PolicyText,
    ) -> Result<ConsistencyToken, ZanzibarError> {
        policy::apply_policy_text_to_service(self, policy)
    }

    /// Parses a DSL string, adds the resulting configurations, and returns a consistency token.
    ///
    /// # Errors
    ///
    /// Returns [`ZanzibarError::ParseError`] when the DSL cannot be parsed.
    pub fn add_dsl_with_token(&mut self, dsl: &str) -> Result<ConsistencyToken, ZanzibarError> {
        schema::compile_legacy_dsl(dsl)?;
        let configs = parser::parse_dsl(dsl)?;
        self.apply_namespace_configs(configs)
    }

    pub(crate) fn apply_namespace_configs(
        &mut self,
        configs: impl IntoIterator<Item = NamespaceConfig>,
    ) -> Result<ConsistencyToken, ZanzibarError> {
        let mut next_configs = self.configs.clone();
        for config in configs {
            next_configs.insert(config.name.clone(), config);
        }
        let compiled_schema = schema::compile_legacy_configs(next_configs.values().cloned())?;
        let next_relationships = self.relationship_store_for_schema(&compiled_schema)?;
        self.publish_snapshot(next_configs, compiled_schema, next_relationships)
    }

    /// Replaces the complete schema DSL and returns a consistency token.
    ///
    /// # Errors
    ///
    /// Returns [`ZanzibarError`] when the DSL cannot be parsed or existing relationships do not
    /// validate against the replacement schema.
    pub fn replace_dsl_with_token(&mut self, dsl: &str) -> Result<ConsistencyToken, ZanzibarError> {
        schema::compile_legacy_dsl(dsl)?;
        let configs = parser::parse_dsl(dsl)?;
        let next_configs = configs
            .into_iter()
            .map(|config| (config.name.clone(), config))
            .collect::<HashMap<_, _>>();
        let compiled_schema = schema::compile_legacy_configs(next_configs.values().cloned())?;
        let next_relationships = self.relationship_store_for_schema(&compiled_schema)?;
        self.publish_snapshot(next_configs, compiled_schema, next_relationships)
    }

    /// Deletes one namespace definition and publishes a new revision.
    ///
    /// Existing relationships are revalidated against the candidate schema. If any relationship
    /// still references the deleted namespace, the operation fails and the current state is
    /// unchanged.
    ///
    /// # Errors
    ///
    /// Returns [`ZanzibarError::NamespaceNotFound`] when the namespace is missing, or another typed
    /// error when the resulting schema cannot validate existing relationships.
    pub fn delete_namespace(&mut self, namespace: &str) -> Result<ConsistencyToken, ZanzibarError> {
        domain::ObjectType::try_from(namespace)?;
        let mut next_configs = self.configs.clone();
        if next_configs.remove(namespace).is_none() {
            return Err(ZanzibarError::NamespaceNotFound(namespace.to_string()));
        }
        let compiled_schema = schema::compile_legacy_configs(next_configs.values().cloned())?;
        let next_relationships = self.relationship_store_for_schema(&compiled_schema)?;
        self.publish_snapshot(next_configs, compiled_schema, next_relationships)
    }

    /// Deletes one relation definition and publishes a new revision.
    ///
    /// Existing relationships are revalidated against the candidate schema. If any relationship
    /// still references the deleted relation, the operation fails and the current state is
    /// unchanged.
    ///
    /// # Errors
    ///
    /// Returns [`ZanzibarError::NamespaceNotFound`] or [`ZanzibarError::RelationNotFound`] when the
    /// target does not exist, or another typed error when existing relationships fail validation.
    pub fn delete_relation(
        &mut self,
        namespace: &str,
        relation: &str,
    ) -> Result<ConsistencyToken, ZanzibarError> {
        domain::ObjectType::try_from(namespace)?;
        domain::RelationName::try_from(relation)?;
        let mut next_configs = self.configs.clone();
        let config = next_configs
            .get_mut(namespace)
            .ok_or_else(|| ZanzibarError::NamespaceNotFound(namespace.to_string()))?;
        let relation_key = Relation(relation.to_string());
        if config.relations.remove(&relation_key).is_none() {
            return Err(ZanzibarError::RelationNotFound(
                relation.to_string(),
                namespace.to_string(),
            ));
        }
        let compiled_schema = schema::compile_legacy_configs(next_configs.values().cloned())?;
        let next_relationships = self.relationship_store_for_schema(&compiled_schema)?;
        self.publish_snapshot(next_configs, compiled_schema, next_relationships)
    }

    /// Applies a validated batch of relationship mutations.
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

        let next_relationships = self
            .relationships
            .apply_mutations(mutations, preconditions)?;
        self.publish_snapshot(self.configs.clone(), schema, next_relationships)
    }

    /// Saves the latest published snapshot to a versioned `.szsnap` artifact.
    ///
    /// Snapshot artifacts are deterministic for the same published snapshot and are intended for
    /// trusted build pipelines to ship prebuilt local authorization data. The loader still treats
    /// the file as untrusted input and validates every section before publishing it.
    ///
    /// # Errors
    ///
    /// Returns [`SnapshotIoError`] when no schema snapshot is loaded, the selected options are not
    /// supported by format version 1, or the file cannot be written.
    pub fn save_snapshot(
        &self,
        path: impl AsRef<Path>,
        options: SnapshotSaveOptions,
    ) -> Result<(), SnapshotIoError> {
        let snapshot = self
            .current_snapshot
            .load_full()
            .ok_or(SnapshotIoError::Format {
                reason: "schema snapshot is required before saving",
            })?;
        snapshot::save_snapshot_file(path.as_ref(), &snapshot, options)
    }

    pub(crate) fn load_snapshot_with_publisher(
        path: impl AsRef<Path>,
        options: SnapshotLoadOptions,
        published_state: SharedEngineState,
    ) -> Result<Self, SnapshotIoError> {
        let loaded = snapshot::load_snapshot_file(path.as_ref(), options)?;
        let snapshot = Arc::new(PublishedSnapshot::new(
            loaded.revision,
            loaded.schema_hash,
            Arc::new(loaded.configs.clone()),
            Arc::new(loaded.schema.clone()),
            Arc::clone(&loaded.relationships),
        ));
        let mut service = Self::with_snapshot_retention_and_publisher(
            snapshot::one_snapshot_retention(),
            published_state,
        );
        service.configs = loaded.configs;
        service.schema = Some(loaded.schema);
        service.relationships = loaded.relationships;
        service.current_snapshot.store(Some(Arc::clone(&snapshot)));
        service.snapshot_history.push_back(snapshot);
        service.last_revision = Some(loaded.revision);
        service.publish_current_engine_state();
        Ok(service)
    }

    fn relationship_store_for_schema(
        &self,
        schema: &CompiledSchema,
    ) -> Result<Arc<RelationshipStoreView>, ZanzibarError> {
        if self.schema.is_some() {
            return self.revalidate_relationship_store(schema);
        }
        Ok(Arc::new(RelationshipStoreView::default()))
    }

    fn revalidate_relationship_store(
        &self,
        schema: &CompiledSchema,
    ) -> Result<Arc<RelationshipStoreView>, ZanzibarError> {
        let mut relationships = IndexedRelationshipStore::default();
        for relationship in self.relationships.rows() {
            schema.validate_relationship(&relationship)?;
            relationships.apply_mutations([RelationshipMutation::Touch(relationship)], [])?;
        }
        Ok(Arc::new(RelationshipStoreView::from_checkpoint(Arc::new(
            relationships,
        ))))
    }

    fn publish_snapshot(
        &mut self,
        configs: HashMap<String, NamespaceConfig>,
        schema: CompiledSchema,
        relationships: Arc<RelationshipStoreView>,
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
        self.current_snapshot.store(Some(Arc::clone(&snapshot)));
        self.snapshot_history.push_back(snapshot);
        while self.snapshot_history.len() > self.retained_snapshots.get() {
            self.snapshot_history.pop_front();
        }
        self.last_revision = Some(revision);
        self.publish_current_engine_state();
        Ok(token)
    }

    pub(crate) fn replace_publisher(&mut self, published_state: SharedEngineState) {
        self.published_state = published_state;
        self.publish_current_engine_state();
    }

    fn publish_current_engine_state(&self) {
        match (self.current_snapshot.load_full(), self.last_revision) {
            (Some(snapshot), Some(revision)) => {
                self.published_state.store(Some(Arc::new(EngineState::new(
                    snapshot,
                    self.snapshot_history.clone(),
                    self.datastore_id,
                    revision,
                    self.evaluation_limits,
                ))));
            }
            _ => self.published_state.store(None),
        }
    }

    fn next_revision(&self) -> Result<Revision, ConsistencyError> {
        match self.last_revision {
            Some(revision) => revision.next(),
            None => Ok(Revision::first()),
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
