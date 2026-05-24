//! Public request/response engine API.

use std::{
    borrow::Borrow,
    collections::HashMap,
    fmt,
    num::NonZeroUsize,
    path::Path,
    str::FromStr,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, SyncSender},
    },
    thread::{self, JoinHandle},
};

use arc_swap::{ArcSwap, ArcSwapOption};
use thiserror::Error;

use crate::{
    WriterState,
    domain::{DomainError, ObjectType, RelationName},
    error::ZanzibarError,
    eval::{self, EvaluationError, EvaluationLimits},
    model::{
        CheckRequest, CheckResponse, ExpandRequest, ExpandResponse, ExpandedUserset,
        LookupObjectPermissions, LookupObjectPermissionsRequest, LookupPermissions,
        LookupPermissionsRequest, LookupResources, LookupResourcesRequest, LookupSubjects,
        LookupSubjectsRequest, NamespaceConfig, Object, PermissionSubjects, Relation,
        RelationTuple, User,
    },
    policy::{self, PolicyIoError, PolicyText},
    relationship::{Precondition, RelationshipMutation, StoreError},
    revision::{Consistency, ConsistencyError, ConsistencyToken, default_retained_snapshots},
    runtime::{EngineState, SharedEngineState},
    schema::{SchemaError, SchemaSource},
    snapshot::{IndexProfile, SnapshotIoError, SnapshotLoadOptions, SnapshotSaveOptions},
};

const DEFAULT_WRITER_QUEUE_CAPACITY: usize = 1024;
const MAX_TENANT_ID_BYTES: usize = 128;

macro_rules! enter_api_span {
    ($operation:literal) => {
        #[cfg(feature = "tracing")]
        let _span_guard = tracing::debug_span!("zanzibar.engine", operation = $operation).entered();
    };
}

/// Public local Zanzibar engine.
///
/// Reads clone immutable published snapshots through an atomic pointer and do not acquire a
/// service-level lock. Writes are submitted to one bounded writer actor that owns the mutable
/// schema, relationship store, and revision history for this engine instance.
#[derive(Debug)]
pub struct ZanzibarEngine {
    state: SharedEngineState,
    writer: WriterActor,
}

impl ZanzibarEngine {
    /// Creates a builder for a local engine.
    #[must_use]
    pub fn builder() -> ZanzibarEngineBuilder {
        ZanzibarEngineBuilder::new()
    }

    /// Checks whether a subject has a relation or permission on an object.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError`] when request validation, consistency, store access, or evaluation
    /// fails.
    pub fn check(&self, request: CheckRequest) -> Result<CheckResponse, EngineError> {
        enter_api_span!("check");
        let (snapshot, limits) = self.snapshot_for_consistency(request.consistency)?;
        let object_type = ObjectType::try_from(request.object.namespace.as_str())?;
        let relation_name = RelationName::try_from(request.relation.0.as_str())?;
        let relation_definition = snapshot
            .schema()
            .resolver()
            .relation(&object_type, &relation_name)?;
        let allowed = eval::check_prepared_with_snapshot(
            &snapshot,
            &request.object,
            &request.relation,
            &request.user,
            relation_definition,
            limits,
        )?
        .is_allowed();
        Ok(CheckResponse { allowed })
    }

    /// Checks a relation or permission using latest consistency.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError`] when request validation, consistency, store access, or evaluation
    /// fails.
    pub fn check_relation(
        &self,
        object: &Object,
        relation: &Relation,
        user: &User,
    ) -> Result<bool, EngineError> {
        self.check_relation_with_consistency(object, relation, user, Consistency::Latest)
    }

    /// Checks a relation or permission at the requested consistency.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError`] when request validation, consistency, store access, or evaluation
    /// fails.
    pub fn check_relation_with_consistency(
        &self,
        object: &Object,
        relation: &Relation,
        user: &User,
        consistency: Consistency,
    ) -> Result<bool, EngineError> {
        Ok(self
            .check(CheckRequest::new(
                object.clone(),
                relation.clone(),
                user.clone(),
                consistency,
            ))?
            .allowed)
    }

    /// Checks a relation or permission at the requested consistency.
    ///
    /// This convenience method is equivalent to [`Self::check_relation_with_consistency`].
    ///
    /// # Errors
    ///
    /// Returns [`EngineError`] when request validation, consistency, store access, or evaluation
    /// fails.
    pub fn check_with_consistency(
        &self,
        object: &Object,
        relation: &Relation,
        user: &User,
        consistency: Consistency,
    ) -> Result<bool, EngineError> {
        self.check_relation_with_consistency(object, relation, user, consistency)
    }

    /// Expands the effective userset for an object relation or permission.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError`] when request validation, consistency, store access, or evaluation
    /// fails.
    pub fn expand(&self, request: ExpandRequest) -> Result<ExpandResponse, EngineError> {
        enter_api_span!("expand");
        let (snapshot, limits) = self.snapshot_for_consistency(request.consistency)?;
        let object_type = ObjectType::try_from(request.object.namespace.as_str())?;
        let relation_name = RelationName::try_from(request.relation.0.as_str())?;
        snapshot
            .schema()
            .resolver()
            .relation(&object_type, &relation_name)?;
        let expanded =
            eval::expand_with_snapshot(&snapshot, &request.object, &request.relation, limits)?;
        Ok(ExpandResponse { expanded })
    }

    /// Expands a relation or permission using latest consistency.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError`] when request validation, consistency, store access, or evaluation
    /// fails.
    pub fn expand_relation(
        &self,
        object: &Object,
        relation: &Relation,
    ) -> Result<ExpandedUserset, EngineError> {
        Ok(self
            .expand(ExpandRequest::new(
                object.clone(),
                relation.clone(),
                Consistency::Latest,
            ))?
            .expanded)
    }

    /// Looks up resources of a type that a subject can access at latest consistency.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError`] when request validation, store access, or evaluation fails.
    pub fn lookup_resources(
        &self,
        request: impl Borrow<LookupResourcesRequest>,
    ) -> Result<LookupResources, EngineError> {
        self.lookup_resources_with_consistency(request, Consistency::Latest)
    }

    /// Looks up resources of a type at the requested consistency.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError`] when request validation, consistency, store access, or evaluation
    /// fails.
    pub fn lookup_resources_with_consistency(
        &self,
        request: impl Borrow<LookupResourcesRequest>,
        consistency: Consistency,
    ) -> Result<LookupResources, EngineError> {
        enter_api_span!("lookup_resources");
        let (snapshot, limits) = self.snapshot_for_consistency(consistency)?;
        Self::ensure_subject_reverse_lookup_supported(&snapshot, "lookup_resources")?;
        let request = request.borrow();
        Ok(eval::lookup_resources_with_snapshot(
            &snapshot, request, limits,
        )?)
    }

    /// Looks up subjects of a type that can access a resource at latest consistency.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError`] when request validation, store access, or evaluation fails.
    pub fn lookup_subjects(
        &self,
        request: impl Borrow<LookupSubjectsRequest>,
    ) -> Result<LookupSubjects, EngineError> {
        self.lookup_subjects_with_consistency(request, Consistency::Latest)
    }

    /// Looks up subjects of a type at the requested consistency.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError`] when request validation, consistency, store access, or evaluation
    /// fails.
    pub fn lookup_subjects_with_consistency(
        &self,
        request: impl Borrow<LookupSubjectsRequest>,
        consistency: Consistency,
    ) -> Result<LookupSubjects, EngineError> {
        enter_api_span!("lookup_subjects");
        let (snapshot, limits) = self.snapshot_for_consistency(consistency)?;
        let request = request.borrow();
        Ok(eval::lookup_subjects_with_snapshot(
            &snapshot, request, limits,
        )?)
    }

    /// Applies relationship mutations and publishes a new revision.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError`] when no schema is loaded, validation fails, or mutation semantics
    /// are invalid.
    pub fn write_relationships(
        &self,
        mutations: impl IntoIterator<Item = RelationshipMutation>,
    ) -> Result<ConsistencyToken, EngineError> {
        enter_api_span!("write_relationships");
        self.write_relationships_with_preconditions(mutations, [])
    }

    /// Applies relationship mutations with optional preconditions.
    ///
    /// This is an alias for [`Self::write_relationships_with_preconditions`] kept as the explicit
    /// batch mutation verb in the public API.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError`] when no schema is loaded, validation fails, preconditions fail, or
    /// mutation semantics are invalid.
    pub fn apply_relationship_mutations(
        &self,
        mutations: impl IntoIterator<Item = RelationshipMutation>,
        preconditions: impl IntoIterator<Item = Precondition>,
    ) -> Result<ConsistencyToken, EngineError> {
        self.write_relationships_with_preconditions(mutations, preconditions)
    }

    /// Writes one legacy tuple as an idempotent relationship touch.
    ///
    /// Prefer [`Self::write_relationships`] for high-throughput callers because it batches many
    /// mutations into one writer-actor turn.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError`] when the tuple is invalid, no schema is loaded, or validation fails.
    pub fn write_tuple(&self, tuple: impl Borrow<RelationTuple>) -> Result<(), EngineError> {
        self.write_tuple_with_token(tuple.borrow()).map(drop)
    }

    /// Writes one legacy tuple as an idempotent relationship touch and returns the published token.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError`] when the tuple is invalid, no schema is loaded, or validation fails.
    pub fn write_tuple_with_token(
        &self,
        tuple: &RelationTuple,
    ) -> Result<ConsistencyToken, EngineError> {
        let relationship = crate::domain::Relationship::try_from(tuple)?;
        self.write_relationships([RelationshipMutation::Touch(relationship)])
    }

    /// Deletes one legacy tuple.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError`] when the tuple is invalid, no schema is loaded, validation fails, or
    /// the tuple is absent.
    pub fn delete_tuple(&self, tuple: &RelationTuple) -> Result<(), EngineError> {
        let relationship = crate::domain::Relationship::try_from(tuple)?;
        self.write_relationships([RelationshipMutation::Delete(relationship)])
            .map(drop)
    }

    /// Deletes one legacy tuple and returns the published token.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError`] when the tuple is invalid, no schema is loaded, validation fails, or
    /// the tuple is absent.
    pub fn delete_tuple_with_token(
        &self,
        tuple: &RelationTuple,
    ) -> Result<ConsistencyToken, EngineError> {
        let relationship = crate::domain::Relationship::try_from(tuple)?;
        self.write_relationships([RelationshipMutation::Delete(relationship)])
    }

    /// Creates one relationship, failing if it already exists.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError`] when the relationship text is invalid, no schema is loaded,
    /// validation fails, or the relationship already exists.
    pub fn create_relationship(&self, relationship: &str) -> Result<ConsistencyToken, EngineError> {
        enter_api_span!("create_relationship");
        self.write_relationships([RelationshipMutation::create(relationship)?])
    }

    /// Grants or refreshes one relationship idempotently.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError`] when the relationship text is invalid, no schema is loaded, or
    /// validation fails.
    pub fn touch_relationship(&self, relationship: &str) -> Result<ConsistencyToken, EngineError> {
        enter_api_span!("touch_relationship");
        self.write_relationships([RelationshipMutation::touch(relationship)?])
    }

    /// Deletes one relationship, failing if it does not exist.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError`] when the relationship text is invalid, no schema is loaded,
    /// validation fails, or the relationship does not exist.
    pub fn delete_relationship(&self, relationship: &str) -> Result<ConsistencyToken, EngineError> {
        enter_api_span!("delete_relationship");
        self.write_relationships([RelationshipMutation::delete(relationship)?])
    }

    /// Applies relationship mutations with preconditions and publishes a new revision.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError`] when no schema is loaded, validation fails, preconditions fail, or
    /// mutation semantics are invalid.
    pub fn write_relationships_with_preconditions(
        &self,
        mutations: impl IntoIterator<Item = RelationshipMutation>,
        preconditions: impl IntoIterator<Item = Precondition>,
    ) -> Result<ConsistencyToken, EngineError> {
        enter_api_span!("write_relationships_with_preconditions");
        self.submit_write("write_relationships", |response| {
            WriterCommand::WriteRelationships {
                mutations: mutations.into_iter().collect(),
                preconditions: preconditions.into_iter().collect(),
                response,
            }
        })
    }

    /// Applies a schema document and publishes a new revision.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError`] when the schema cannot be parsed or validated.
    pub fn apply_schema(&self, source: SchemaSource<'_>) -> Result<ConsistencyToken, EngineError> {
        enter_api_span!("apply_schema");
        self.submit_write("apply_schema", |response| WriterCommand::ApplySchema {
            text: source.text.to_string(),
            response,
        })
    }

    /// Applies a legacy DSL schema document and publishes a new revision.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError`] when the DSL cannot be parsed or validated.
    pub fn add_dsl(&self, dsl: &str) -> Result<(), EngineError> {
        self.add_dsl_with_token(dsl).map(drop)
    }

    /// Applies a legacy DSL schema document and returns the published token.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError`] when the DSL cannot be parsed or validated.
    pub fn add_dsl_with_token(&self, dsl: &str) -> Result<ConsistencyToken, EngineError> {
        self.apply_schema(SchemaSource {
            name: Some("dsl"),
            text: dsl,
        })
    }

    /// Applies one structured namespace config and publishes a new revision.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError`] when the namespace config cannot be validated.
    pub fn apply_namespace_config(
        &self,
        config: NamespaceConfig,
    ) -> Result<ConsistencyToken, EngineError> {
        self.apply_namespace_configs([config])
    }

    /// Applies one structured namespace config and returns the published token.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError`] when the namespace config cannot be validated.
    pub fn add_config_with_token(
        &self,
        config: NamespaceConfig,
    ) -> Result<ConsistencyToken, EngineError> {
        self.apply_namespace_config(config)
    }

    /// Applies structured namespace configs and publishes a single new revision.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError`] when any namespace config cannot be validated.
    pub fn apply_namespace_configs(
        &self,
        configs: impl IntoIterator<Item = NamespaceConfig>,
    ) -> Result<ConsistencyToken, EngineError> {
        let configs = configs.into_iter().collect();
        self.submit_write("apply_namespace_configs", |response| {
            WriterCommand::ApplyNamespaceConfigs { configs, response }
        })
    }

    /// Replaces the complete schema document and publishes a new revision.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError`] when the schema cannot be parsed or existing relationships no longer
    /// validate against it.
    pub fn replace_schema(
        &self,
        source: SchemaSource<'_>,
    ) -> Result<ConsistencyToken, EngineError> {
        enter_api_span!("replace_schema");
        self.submit_write("replace_schema", |response| WriterCommand::ReplaceSchema {
            text: source.text.to_string(),
            response,
        })
    }

    /// Deletes one namespace definition.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError`] when the namespace is missing or existing relationships still
    /// reference it.
    pub fn delete_namespace(&self, namespace: &str) -> Result<ConsistencyToken, EngineError> {
        enter_api_span!("delete_namespace");
        self.submit_write("delete_namespace", |response| {
            WriterCommand::DeleteNamespace {
                namespace: namespace.to_string(),
                response,
            }
        })
    }

    /// Deletes one relation definition.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError`] when the relation is missing or existing relationships still
    /// reference it.
    pub fn delete_relation(
        &self,
        namespace: &str,
        relation: &str,
    ) -> Result<ConsistencyToken, EngineError> {
        enter_api_span!("delete_relation");
        self.submit_write("delete_relation", |response| {
            WriterCommand::DeleteRelation {
                namespace: namespace.to_string(),
                relation: relation.to_string(),
                response,
            }
        })
    }

    /// Builds a new engine from policy text.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError`] when policy text cannot be parsed or validated.
    pub fn from_policy_text(policy: &PolicyText) -> Result<Self, EngineError> {
        enter_api_span!("from_policy_text");
        let engine = Self::builder().build();
        engine.apply_policy_text(policy)?;
        Ok(engine)
    }

    /// Replaces this engine's state with policy text.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError`] when policy text cannot be parsed or validated.
    pub fn apply_policy_text(&self, policy: &PolicyText) -> Result<ConsistencyToken, EngineError> {
        enter_api_span!("apply_policy_text");
        let policy = policy.clone();
        self.submit_write("apply_policy_text", |response| {
            WriterCommand::ApplyPolicyText { policy, response }
        })
    }

    /// Saves a snapshot built from policy text without keeping an engine.
    ///
    /// # Errors
    ///
    /// Returns [`PolicyIoError`] when policy parsing or snapshot writing fails.
    pub fn save_snapshot_from_policy_text(
        path: impl AsRef<Path>,
        policy: &PolicyText,
        options: SnapshotSaveOptions,
    ) -> Result<(), PolicyIoError> {
        policy::save_snapshot_from_policy_text(path.as_ref(), policy, options)
    }

    /// Exports the latest state as deterministic policy text.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError`] when no schema has been loaded.
    pub fn export_policy_text(&self) -> Result<PolicyText, EngineError> {
        enter_api_span!("export_policy_text");
        let snapshot = self.latest_snapshot()?;
        Ok(policy::export_policy_text(
            snapshot.configs(),
            snapshot.relationships().rows(),
        ))
    }

    /// Exports deterministic policy files under `directory`.
    ///
    /// # Errors
    ///
    /// Returns [`PolicyIoError`] when no schema has been loaded or file output fails.
    pub fn export_policy_files(&self, directory: impl AsRef<Path>) -> Result<(), PolicyIoError> {
        enter_api_span!("export_policy_files");
        let snapshot = self
            .latest_snapshot()
            .map_err(|_| PolicyIoError::Zanzibar {
                source: ZanzibarError::SchemaRequired,
            })?;
        let policy =
            policy::export_policy_text(snapshot.configs(), snapshot.relationships().rows());
        policy::write_policy_files(directory.as_ref(), &policy)
    }

    /// Looks up every relation or permission a subject has on one resource.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError`] when request validation or evaluation fails.
    pub fn lookup_permissions(
        &self,
        request: impl Borrow<LookupPermissionsRequest>,
    ) -> Result<LookupPermissions, EngineError> {
        enter_api_span!("lookup_permissions");
        let request = request.borrow();
        let (snapshot, limits) = self.snapshot_for_consistency(request.consistency.clone())?;
        let object_type = ObjectType::try_from(request.resource.namespace.as_str())?;
        snapshot.schema().resolver().namespace(&object_type)?;
        let mut permissions = Vec::new();
        for relation_definition in snapshot
            .schema()
            .resolver()
            .sorted_relations(&object_type)?
        {
            let relation = Relation(relation_definition.name().as_str().to_string());
            if eval::check_prepared_with_snapshot(
                &snapshot,
                &request.resource,
                &relation,
                &request.subject,
                relation_definition,
                limits,
            )?
            .is_allowed()
            {
                permissions.push(relation);
            }
        }
        Ok(LookupPermissions { permissions })
    }

    /// Looks up subjects grouped by every relation or permission they have on one resource.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError`] when request validation, store access, or evaluation fails.
    pub fn lookup_object_permissions(
        &self,
        request: impl Borrow<LookupObjectPermissionsRequest>,
    ) -> Result<LookupObjectPermissions, EngineError> {
        enter_api_span!("lookup_object_permissions");
        let request = request.borrow();
        let (snapshot, limits) = self.snapshot_for_consistency(request.consistency.clone())?;
        let object_type = ObjectType::try_from(request.resource.namespace.as_str())?;
        snapshot.schema().resolver().namespace(&object_type)?;
        let subject_type = crate::domain::SubjectType::try_from(request.subject_type.as_str())?;
        if subject_type.as_str() != "user" {
            let subject_object_type = ObjectType::try_from(subject_type.as_str())?;
            snapshot
                .schema()
                .resolver()
                .namespace(&subject_object_type)?;
        }

        let mut permissions = Vec::new();
        for relation_definition in snapshot
            .schema()
            .resolver()
            .sorted_relations(&object_type)?
        {
            let permission = Relation(relation_definition.name().as_str().to_string());
            let subjects = eval::lookup_subjects_with_snapshot(
                &snapshot,
                &LookupSubjectsRequest {
                    resource: request.resource.clone(),
                    permission: permission.clone(),
                    subject_type: request.subject_type.clone(),
                },
                limits,
            )?
            .subjects;
            if !subjects.is_empty() {
                permissions.push(PermissionSubjects {
                    permission,
                    subjects,
                });
            }
        }
        Ok(LookupObjectPermissions { permissions })
    }

    /// Saves the latest published snapshot to a versioned `.szsnap` artifact.
    ///
    /// # Errors
    ///
    /// Returns [`SnapshotIoError`] when no schema is loaded, the save options are unsupported, or
    /// the artifact cannot be written.
    pub fn save_snapshot(
        &self,
        path: impl AsRef<Path>,
        options: SnapshotSaveOptions,
    ) -> Result<(), SnapshotIoError> {
        enter_api_span!("save_snapshot");
        let snapshot = self.latest_snapshot().map_err(|error| match error {
            EngineError::SchemaRequired => SnapshotIoError::Format {
                reason: "schema snapshot is required before saving",
            },
            _ => SnapshotIoError::Format {
                reason: "engine state unavailable during snapshot save",
            },
        })?;
        crate::snapshot::save_snapshot_file(path.as_ref(), &snapshot, options)
    }

    /// Loads a versioned `.szsnap` artifact into a new engine.
    ///
    /// # Errors
    ///
    /// Returns [`SnapshotIoError`] when the artifact cannot be read or fails validation.
    pub fn load_snapshot(
        path: impl AsRef<Path>,
        options: SnapshotLoadOptions,
    ) -> Result<Self, SnapshotIoError> {
        enter_api_span!("load_snapshot");
        let state = Arc::new(ArcSwapOption::empty());
        let writer_state =
            WriterState::load_snapshot_with_publisher(path, options, Arc::clone(&state))?;
        Ok(Self {
            state,
            writer: WriterActor::start(writer_state, default_writer_queue_capacity()),
        })
    }

    fn submit_write(
        &self,
        operation: &'static str,
        build_command: impl FnOnce(WriteResponseSender) -> WriterCommand,
    ) -> Result<ConsistencyToken, EngineError> {
        let (sender, receiver) = mpsc::sync_channel(1);
        self.writer.send(build_command(sender), operation)?;
        receiver
            .recv()
            .map_err(|_| EngineError::WriterUnavailable { operation })?
            .map_err(EngineError::from)
    }

    fn current_state(&self) -> Result<Arc<EngineState>, EngineError> {
        self.state.load_full().ok_or(EngineError::SchemaRequired)
    }

    fn latest_snapshot(&self) -> Result<Arc<crate::revision::PublishedSnapshot>, EngineError> {
        Ok(self.current_state()?.latest_snapshot())
    }

    fn snapshot_for_consistency(
        &self,
        consistency: Consistency,
    ) -> Result<(Arc<crate::revision::PublishedSnapshot>, EvaluationLimits), EngineError> {
        let state = self.current_state()?;
        let limits = state.evaluation_limits();
        let snapshot = state.snapshot_for_consistency(consistency)?;
        Ok((snapshot, limits))
    }

    fn ensure_subject_reverse_lookup_supported(
        snapshot: &crate::revision::PublishedSnapshot,
        operation: &'static str,
    ) -> Result<(), EngineError> {
        let profile = snapshot.relationships().index_profile();
        if profile.supports_subject_reverse_lookup() {
            return Ok(());
        }
        Err(EngineError::UnsupportedIndexProfile { operation, profile })
    }
}

impl Default for ZanzibarEngine {
    fn default() -> Self {
        Self::builder().build()
    }
}

/// Builder for [`ZanzibarEngine`].
#[derive(Debug, Clone, Copy)]
pub struct ZanzibarEngineBuilder {
    retained_snapshots: NonZeroUsize,
    evaluation_limits: EvaluationLimits,
    writer_queue_capacity: NonZeroUsize,
}

impl ZanzibarEngineBuilder {
    /// Creates a builder with production defaults.
    #[must_use]
    pub fn new() -> Self {
        Self {
            retained_snapshots: default_retained_snapshots(),
            evaluation_limits: EvaluationLimits::default(),
            writer_queue_capacity: default_writer_queue_capacity(),
        }
    }

    /// Sets how many exact revision snapshots the engine retains.
    #[must_use]
    pub fn retained_snapshots(mut self, retained_snapshots: NonZeroUsize) -> Self {
        self.retained_snapshots = retained_snapshots;
        self
    }

    /// Sets evaluator recursion, fanout, and lookup-result limits.
    #[must_use]
    pub fn evaluation_limits(mut self, evaluation_limits: EvaluationLimits) -> Self {
        self.evaluation_limits = evaluation_limits;
        self
    }

    /// Sets the bounded writer actor queue capacity.
    #[must_use]
    pub fn writer_queue_capacity(mut self, writer_queue_capacity: NonZeroUsize) -> Self {
        self.writer_queue_capacity = writer_queue_capacity;
        self
    }

    /// Builds the engine.
    #[must_use]
    pub fn build(self) -> ZanzibarEngine {
        let state = Arc::new(ArcSwapOption::empty());
        let writer_state = WriterState::with_snapshot_retention_and_publisher(
            self.retained_snapshots,
            Arc::clone(&state),
        )
        .with_evaluation_limits(self.evaluation_limits);
        ZanzibarEngine {
            state,
            writer: WriterActor::start(writer_state, self.writer_queue_capacity),
        }
    }
}

impl Default for ZanzibarEngineBuilder {
    fn default() -> Self {
        Self::new()
    }
}

/// Validated tenant identifier used by [`ZanzibarTenantShards`].
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TenantId(String);

impl TenantId {
    /// Creates a tenant id after validating length and charset.
    ///
    /// # Errors
    ///
    /// Returns [`TenantIdError`] when `value` is empty, too long, or contains unsupported bytes.
    pub fn new(value: impl Into<String>) -> Result<Self, TenantIdError> {
        let value = value.into();
        validate_tenant_id(&value)?;
        Ok(Self(value))
    }

    /// Returns the tenant id as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for TenantId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl FromStr for TenantId {
    type Err = TenantIdError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::new(value)
    }
}

impl TryFrom<&str> for TenantId {
    type Error = TenantIdError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

/// Error returned by [`TenantId`] validation.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum TenantIdError {
    /// Tenant id was empty.
    #[error("tenant id must not be empty")]
    Empty,
    /// Tenant id exceeded the maximum byte length.
    #[error("tenant id exceeds maximum byte length {max_bytes}")]
    TooLong {
        /// Maximum accepted tenant id length in bytes.
        max_bytes: usize,
    },
    /// Tenant id contained an unsupported byte.
    #[error("tenant id contains invalid byte at offset {offset}")]
    InvalidByte {
        /// Byte offset of the invalid value.
        offset: usize,
    },
}

/// Tenant-sharded owner of independent [`ZanzibarEngine`] instances.
#[derive(Debug)]
pub struct ZanzibarTenantShards {
    shards: Arc<ArcSwap<ShardMap>>,
    create_gate: Mutex<()>,
    builder: ZanzibarEngineBuilder,
}

impl ZanzibarTenantShards {
    /// Creates an empty tenant shard set using `builder` for new tenants.
    #[must_use]
    pub fn new(builder: ZanzibarEngineBuilder) -> Self {
        Self {
            shards: Arc::new(ArcSwap::from_pointee(ShardMap::default())),
            create_gate: Mutex::new(()),
            builder,
        }
    }

    /// Returns an existing tenant engine without creating it.
    #[must_use]
    pub fn get(&self, tenant: &TenantId) -> Option<Arc<ZanzibarEngine>> {
        self.shards.load_full().engines.get(tenant).cloned()
    }

    /// Returns an existing tenant engine or creates one under a short creation gate.
    #[must_use]
    pub fn get_or_create(&self, tenant: TenantId) -> Arc<ZanzibarEngine> {
        if let Some(engine) = self.get(&tenant) {
            return engine;
        }
        let _guard = self
            .create_gate
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(engine) = self.get(&tenant) {
            return engine;
        }
        let mut next = (*self.shards.load_full()).clone();
        let engine = Arc::new(self.builder.build());
        next.engines.insert(tenant, Arc::clone(&engine));
        self.shards.store(Arc::new(next));
        engine
    }

    /// Returns sorted tenant ids currently present in this shard set.
    #[must_use]
    pub fn tenants(&self) -> Vec<TenantId> {
        let mut tenants = self
            .shards
            .load_full()
            .engines
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        tenants.sort();
        tenants
    }
}

impl Default for ZanzibarTenantShards {
    fn default() -> Self {
        Self::new(ZanzibarEngine::builder())
    }
}

/// Error returned by public [`ZanzibarEngine`] methods.
#[derive(Debug, Error, PartialEq)]
pub enum EngineError {
    /// Domain primitive validation failed.
    #[error(transparent)]
    Domain(DomainError),

    /// Schema compilation or validation failed.
    #[error(transparent)]
    Schema(SchemaError),

    /// Relationship store access or mutation semantics failed.
    #[error(transparent)]
    Store(StoreError),

    /// Consistency token validation failed.
    #[error(transparent)]
    Consistency(ConsistencyError),

    /// Graph evaluation failed.
    #[error(transparent)]
    Evaluation(EvaluationError),

    /// Requested namespace is not loaded.
    #[error("namespace '{namespace}' not found")]
    NamespaceNotFound {
        /// Missing namespace name.
        namespace: String,
    },

    /// Requested relation is not loaded in the namespace.
    #[error("relation '{relation}' not found in namespace '{namespace}'")]
    RelationNotFound {
        /// Namespace searched.
        namespace: String,
        /// Missing relation name.
        relation: String,
    },

    /// Schema source parsing failed.
    #[error("schema parse error: {message}")]
    ParseError {
        /// Parser diagnostic.
        message: String,
    },

    /// Legacy compatibility storage failed.
    #[error("storage error: {message}")]
    StorageError {
        /// Storage diagnostic.
        message: String,
    },

    /// Operation requires a loaded schema.
    #[error("schema must be loaded before this operation")]
    SchemaRequired,

    /// The writer actor is unavailable.
    #[error("engine writer actor unavailable during {operation}")]
    WriterUnavailable {
        /// Operation that attempted to use the writer actor.
        operation: &'static str,
    },

    /// The loaded index profile cannot support a requested operation.
    #[error("index profile {profile:?} does not support {operation}")]
    UnsupportedIndexProfile {
        /// Operation that requires an omitted index family.
        operation: &'static str,
        /// Loaded profile.
        profile: IndexProfile,
    },
}

impl From<ZanzibarError> for EngineError {
    fn from(error: ZanzibarError) -> Self {
        match error {
            ZanzibarError::NamespaceNotFound(namespace) => Self::NamespaceNotFound { namespace },
            ZanzibarError::RelationNotFound(relation, namespace) => Self::RelationNotFound {
                namespace,
                relation,
            },
            ZanzibarError::ParseError(message) => Self::ParseError { message },
            ZanzibarError::StorageError(message) => Self::StorageError { message },
            ZanzibarError::SchemaRequired => Self::SchemaRequired,
            ZanzibarError::Domain(error) => Self::Domain(error),
            ZanzibarError::Schema(error) => Self::Schema(error),
            ZanzibarError::Store(error) => Self::Store(error),
            ZanzibarError::Consistency(error) => Self::Consistency(error),
            ZanzibarError::Evaluation(error) => Self::Evaluation(error),
        }
    }
}

impl From<EngineError> for ZanzibarError {
    fn from(error: EngineError) -> Self {
        match error {
            EngineError::NamespaceNotFound { namespace } => Self::NamespaceNotFound(namespace),
            EngineError::RelationNotFound {
                namespace,
                relation,
            } => Self::RelationNotFound(relation, namespace),
            EngineError::ParseError { message } => Self::ParseError(message),
            EngineError::StorageError { message } => Self::StorageError(message),
            EngineError::SchemaRequired => Self::SchemaRequired,
            EngineError::Domain(error) => Self::Domain(error),
            EngineError::Schema(error) => Self::Schema(error),
            EngineError::Store(error) => Self::Store(error),
            EngineError::Consistency(error) => Self::Consistency(error),
            EngineError::Evaluation(error) => Self::Evaluation(error),
            EngineError::WriterUnavailable { operation } => Self::StorageError(format!(
                "engine writer actor unavailable during {operation}",
            )),
            EngineError::UnsupportedIndexProfile { operation, profile } => Self::StorageError(
                format!("index profile {profile:?} does not support {operation}"),
            ),
        }
    }
}

impl From<DomainError> for EngineError {
    fn from(error: DomainError) -> Self {
        Self::Domain(error)
    }
}

impl From<SchemaError> for EngineError {
    fn from(error: SchemaError) -> Self {
        Self::Schema(error)
    }
}

impl From<StoreError> for EngineError {
    fn from(error: StoreError) -> Self {
        Self::Store(error)
    }
}

impl From<ConsistencyError> for EngineError {
    fn from(error: ConsistencyError) -> Self {
        Self::Consistency(error)
    }
}

impl From<EvaluationError> for EngineError {
    fn from(error: EvaluationError) -> Self {
        Self::Evaluation(error)
    }
}

type WriteResponseSender = SyncSender<Result<ConsistencyToken, ZanzibarError>>;

enum WriterCommand {
    WriteRelationships {
        mutations: Vec<RelationshipMutation>,
        preconditions: Vec<Precondition>,
        response: WriteResponseSender,
    },
    ApplySchema {
        text: String,
        response: WriteResponseSender,
    },
    ApplyNamespaceConfigs {
        configs: Vec<NamespaceConfig>,
        response: WriteResponseSender,
    },
    ReplaceSchema {
        text: String,
        response: WriteResponseSender,
    },
    DeleteNamespace {
        namespace: String,
        response: WriteResponseSender,
    },
    DeleteRelation {
        namespace: String,
        relation: String,
        response: WriteResponseSender,
    },
    ApplyPolicyText {
        policy: PolicyText,
        response: WriteResponseSender,
    },
    Shutdown,
}

struct WriterActor {
    sender: SyncSender<WriterCommand>,
    handle: Mutex<Option<JoinHandle<()>>>,
    shutdown: AtomicBool,
}

impl WriterActor {
    fn start(mut state: WriterState, queue_capacity: NonZeroUsize) -> Self {
        let (sender, receiver) = mpsc::sync_channel(queue_capacity.get());
        let handle = thread::spawn(move || {
            while let Ok(command) = receiver.recv() {
                match command {
                    WriterCommand::WriteRelationships {
                        mutations,
                        preconditions,
                        response,
                    } => {
                        drop(
                            response
                                .send(state.apply_relationship_mutations(mutations, preconditions)),
                        );
                    }
                    WriterCommand::ApplySchema { text, response } => {
                        drop(response.send(state.add_dsl_with_token(&text)));
                    }
                    WriterCommand::ApplyNamespaceConfigs { configs, response } => {
                        drop(response.send(state.apply_namespace_configs(configs)));
                    }
                    WriterCommand::ReplaceSchema { text, response } => {
                        drop(response.send(state.replace_dsl_with_token(&text)));
                    }
                    WriterCommand::DeleteNamespace {
                        namespace,
                        response,
                    } => {
                        drop(response.send(state.delete_namespace(&namespace)));
                    }
                    WriterCommand::DeleteRelation {
                        namespace,
                        relation,
                        response,
                    } => {
                        drop(response.send(state.delete_relation(&namespace, &relation)));
                    }
                    WriterCommand::ApplyPolicyText { policy, response } => {
                        drop(response.send(state.apply_policy_text(&policy)));
                    }
                    WriterCommand::Shutdown => break,
                }
            }
        });
        Self {
            sender,
            handle: Mutex::new(Some(handle)),
            shutdown: AtomicBool::new(false),
        }
    }

    fn send(&self, command: WriterCommand, operation: &'static str) -> Result<(), EngineError> {
        if self.shutdown.load(Ordering::Acquire) {
            return Err(EngineError::WriterUnavailable { operation });
        }
        self.sender
            .send(command)
            .map_err(|_| EngineError::WriterUnavailable { operation })
    }
}

impl fmt::Debug for WriterActor {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WriterActor")
            .field("sender", &"<sync sender>")
            .field("handle", &"<join handle>")
            .finish()
    }
}

impl Drop for WriterActor {
    fn drop(&mut self) {
        if !self.shutdown.swap(true, Ordering::AcqRel) {
            drop(self.sender.try_send(WriterCommand::Shutdown));
        }
        if let Ok(mut handle) = self.handle.lock()
            && let Some(handle) = handle.take()
        {
            drop(handle.join());
        }
    }
}

#[derive(Debug, Clone, Default)]
struct ShardMap {
    engines: HashMap<TenantId, Arc<ZanzibarEngine>>,
}

fn default_writer_queue_capacity() -> NonZeroUsize {
    match NonZeroUsize::new(DEFAULT_WRITER_QUEUE_CAPACITY) {
        Some(value) => value,
        None => NonZeroUsize::MIN,
    }
}

fn validate_tenant_id(value: &str) -> Result<(), TenantIdError> {
    if value.is_empty() {
        return Err(TenantIdError::Empty);
    }
    if value.len() > MAX_TENANT_ID_BYTES {
        return Err(TenantIdError::TooLong {
            max_bytes: MAX_TENANT_ID_BYTES,
        });
    }
    for (offset, byte) in value.bytes().enumerate() {
        if !(byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.')) {
            return Err(TenantIdError::InvalidByte { offset });
        }
    }
    Ok(())
}
