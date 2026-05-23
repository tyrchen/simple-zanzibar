//! Public request/response engine API.

use std::num::NonZeroUsize;
use std::sync::{RwLock, RwLockReadGuard, RwLockWriteGuard};

use thiserror::Error;

use crate::ZanzibarService;
use crate::domain::DomainError;
use crate::error::ZanzibarError;
use crate::eval::{EvaluationError, EvaluationLimits};
use crate::model::{
    CheckRequest, CheckResponse, ExpandRequest, ExpandResponse, LookupResources,
    LookupResourcesRequest, LookupSubjects, LookupSubjectsRequest,
};
use crate::relationship::{Precondition, RelationshipMutation, StoreError};
use crate::revision::ConsistencyError;
use crate::revision::{ConsistencyToken, default_retained_snapshots};
use crate::schema::{SchemaError, SchemaSource};

macro_rules! enter_api_span {
    ($operation:literal) => {
        #[cfg(feature = "tracing")]
        let _span_guard = tracing::debug_span!("zanzibar.engine", operation = $operation).entered();
    };
}

/// Public local Zanzibar engine.
///
/// The engine owns the in-memory schema, relationship indexes, retained revision snapshots, and a
/// writer gate. All public reads acquire one coherent published snapshot, while writes publish a new
/// snapshot atomically.
///
/// ```
/// use simple_zanzibar::domain::Relationship;
/// use simple_zanzibar::model::{
///     CheckRequest, ExpandRequest, LookupResourcesRequest, LookupSubjectsRequest, Object,
///     Relation, User,
/// };
/// use simple_zanzibar::relationship::{
///     Precondition, RelationshipFilter, RelationshipMutation, SubjectFilter,
/// };
/// use simple_zanzibar::revision::Consistency;
/// use simple_zanzibar::schema::SchemaSource;
/// use simple_zanzibar::ZanzibarEngine;
///
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// let engine = ZanzibarEngine::builder().build();
/// engine.apply_schema(SchemaSource {
///     name: Some("docs"),
///     text: r"
///     namespace doc {
///         relation viewer {}
///     }
///     ",
/// })?;
///
/// let relationship: Relationship = "doc:readme#viewer@user:alice".parse()?;
/// let subject = SubjectFilter::exact("user".try_into()?, "alice".try_into()?, None);
/// let precondition = Precondition::MustNotMatch(RelationshipFilter::for_exact_subject(
///     relationship.resource(),
///     relationship.relation().clone(),
///     subject,
/// ));
/// let token = engine.write_relationships_with_preconditions(
///     [RelationshipMutation::Touch(relationship)],
///     [precondition],
/// )?;
///
/// let doc = Object {
///     namespace: "doc".to_string(),
///     id: "readme".to_string(),
/// };
/// let viewer = Relation("viewer".to_string());
/// let alice = User::UserId("alice".to_string());
///
/// assert!(
///     engine
///         .check(CheckRequest::new(
///             doc.clone(),
///             viewer.clone(),
///             alice.clone(),
///             Consistency::Latest,
///         ))?
///         .allowed
/// );
/// assert!(
///     engine
///         .check(CheckRequest::new(
///             doc.clone(),
///             viewer.clone(),
///             alice.clone(),
///             Consistency::Exact(token),
///         ))?
///         .allowed
/// );
/// engine.expand(ExpandRequest::new(
///     doc.clone(),
///     viewer.clone(),
///     Consistency::Latest,
/// ))?;
/// assert_eq!(
///     engine
///         .lookup_resources(LookupResourcesRequest {
///             subject: alice.clone(),
///             permission: viewer.clone(),
///             resource_type: "doc".to_string(),
///         })?
///         .resources,
///     vec![doc.clone()],
/// );
/// assert_eq!(
///     engine
///         .lookup_subjects(LookupSubjectsRequest {
///             resource: doc,
///             permission: viewer,
///             subject_type: "user".to_string(),
///         })?
///         .subjects,
///     vec![alice],
/// );
/// # Ok(())
/// # }
/// ```
#[derive(Debug)]
pub struct ZanzibarEngine {
    service: RwLock<ZanzibarService>,
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
        let service = self.read_service("check")?;
        let allowed = service.check_with_consistency(
            &request.object,
            &request.relation,
            &request.user,
            request.consistency,
        )?;
        Ok(CheckResponse { allowed })
    }

    /// Expands the effective userset for an object relation or permission.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError`] when request validation, consistency, store access, or evaluation
    /// fails.
    pub fn expand(&self, request: ExpandRequest) -> Result<ExpandResponse, EngineError> {
        enter_api_span!("expand");
        let service = self.read_service("expand")?;
        let expanded = service.expand_with_consistency(
            &request.object,
            &request.relation,
            request.consistency,
        )?;
        Ok(ExpandResponse { expanded })
    }

    /// Looks up resources of a type that a subject can access at latest consistency.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError`] when request validation, store access, or evaluation fails.
    #[allow(
        clippy::needless_pass_by_value,
        reason = "public API follows the request/response ownership contract in specs/15-public-api-design.md"
    )]
    pub fn lookup_resources(
        &self,
        request: LookupResourcesRequest,
    ) -> Result<LookupResources, EngineError> {
        enter_api_span!("lookup_resources");
        let service = self.read_service("lookup_resources")?;
        Ok(service.lookup_resources(&request)?)
    }

    /// Looks up subjects of a type that can access a resource at latest consistency.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError`] when request validation, store access, or evaluation fails.
    #[allow(
        clippy::needless_pass_by_value,
        reason = "public API follows the request/response ownership contract in specs/15-public-api-design.md"
    )]
    pub fn lookup_subjects(
        &self,
        request: LookupSubjectsRequest,
    ) -> Result<LookupSubjects, EngineError> {
        enter_api_span!("lookup_subjects");
        let service = self.read_service("lookup_subjects")?;
        Ok(service.lookup_subjects(&request)?)
    }

    /// Applies relationship mutations and publishes a new revision.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError`] when no schema is loaded, validation fails, or mutation semantics are
    /// invalid.
    pub fn write_relationships(
        &self,
        mutations: impl IntoIterator<Item = RelationshipMutation>,
    ) -> Result<ConsistencyToken, EngineError> {
        enter_api_span!("write_relationships");
        self.write_relationships_with_preconditions(mutations, [])
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
        let mut service = self.write_service("write_relationships")?;
        Ok(service.apply_relationship_mutations(mutations, preconditions)?)
    }

    /// Applies a schema document and publishes a new revision.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError`] when the schema cannot be parsed or validated.
    pub fn apply_schema(&self, source: SchemaSource<'_>) -> Result<ConsistencyToken, EngineError> {
        enter_api_span!("apply_schema");
        let mut service = self.write_service("apply_schema")?;
        Ok(service.add_dsl_with_token(source.text)?)
    }

    fn read_service(
        &self,
        operation: &'static str,
    ) -> Result<RwLockReadGuard<'_, ZanzibarService>, EngineError> {
        self.service
            .read()
            .map_err(|_| EngineError::LockPoisoned { operation })
    }

    fn write_service(
        &self,
        operation: &'static str,
    ) -> Result<RwLockWriteGuard<'_, ZanzibarService>, EngineError> {
        self.service
            .write()
            .map_err(|_| EngineError::LockPoisoned { operation })
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
}

impl ZanzibarEngineBuilder {
    /// Creates a builder with production defaults.
    #[must_use]
    pub fn new() -> Self {
        Self {
            retained_snapshots: default_retained_snapshots(),
            evaluation_limits: EvaluationLimits::default(),
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

    /// Builds the engine.
    #[must_use]
    pub fn build(self) -> ZanzibarEngine {
        let service = ZanzibarService::with_snapshot_retention(self.retained_snapshots)
            .with_evaluation_limits(self.evaluation_limits);
        ZanzibarEngine {
            service: RwLock::new(service),
        }
    }
}

impl Default for ZanzibarEngineBuilder {
    fn default() -> Self {
        Self::new()
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

    /// The engine lock was poisoned by a previous panic.
    #[error("engine lock poisoned during {operation}")]
    LockPoisoned {
        /// Operation that attempted to acquire the poisoned lock.
        operation: &'static str,
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
