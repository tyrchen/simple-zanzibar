//! Defines custom error types for the application.
use thiserror::Error;

use crate::domain::DomainError;
use crate::eval::EvaluationError;
use crate::relationship::StoreError;
use crate::revision::ConsistencyError;
use crate::schema::SchemaError;

/// Top-level error returned by the compatibility service and public helper APIs.
#[derive(Error, Debug, PartialEq)]
pub enum ZanzibarError {
    /// Requested namespace is not configured.
    #[error("Namespace '{0}' not found")]
    NamespaceNotFound(String),

    /// Requested relation is not configured in the namespace.
    #[error("Relation '{0}' not found in namespace '{1}'")]
    RelationNotFound(String, String),

    /// DSL parsing failed.
    #[error("Parsing error: {0}")]
    ParseError(String),

    /// Legacy tuple store operation failed.
    #[error("Storage error: {0}")]
    StorageError(String),

    /// Operation requires a loaded schema snapshot.
    #[error("Schema must be loaded before applying relationship batches")]
    SchemaRequired,

    /// Domain validation failed.
    #[error(transparent)]
    Domain(#[from] DomainError),

    /// Schema compilation or validation failed.
    #[error(transparent)]
    Schema(#[from] SchemaError),

    /// Indexed relationship store operation failed.
    #[error(transparent)]
    Store(#[from] StoreError),

    /// Consistency token validation failed.
    #[error(transparent)]
    Consistency(#[from] ConsistencyError),

    /// Graph evaluation failed.
    #[error(transparent)]
    Evaluation(#[from] EvaluationError),
}
