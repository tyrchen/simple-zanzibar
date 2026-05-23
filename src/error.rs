//! Defines custom error types for the application.
use thiserror::Error;

use crate::domain::DomainError;
use crate::relationship::StoreError;
use crate::schema::SchemaError;

#[derive(Error, Debug, PartialEq)]
pub enum ZanzibarError {
    #[error("Namespace '{0}' not found")]
    NamespaceNotFound(String),

    #[error("Relation '{0}' not found in namespace '{1}'")]
    RelationNotFound(String, String),

    #[error("Parsing error: {0}")]
    ParseError(String),

    #[error("Storage error: {0}")]
    StorageError(String),

    #[error("Schema must be loaded before applying relationship batches")]
    SchemaRequired,

    #[error(transparent)]
    Domain(#[from] DomainError),

    #[error(transparent)]
    Schema(#[from] SchemaError),

    #[error(transparent)]
    Store(#[from] StoreError),
}
