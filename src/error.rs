//! Defines custom error types for the Zanzibar authorization system.

use std::fmt;

use thiserror::Error;

/// Errors that can occur in storage operations.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum StoreError {
    /// The tuple already exists in the store.
    DuplicateTuple,
    /// The tuple was not found in the store.
    TupleNotFound,
}

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DuplicateTuple => write!(f, "tuple already exists"),
            Self::TupleNotFound => write!(f, "tuple not found"),
        }
    }
}

impl std::error::Error for StoreError {}

/// Errors that can occur in the Zanzibar authorization system.
#[derive(Error, Debug)]
#[non_exhaustive]
pub enum ZanzibarError {
    /// The specified namespace was not found in the configuration.
    #[error("namespace '{0}' not found")]
    NamespaceNotFound(String),

    /// The specified relation was not found in the namespace.
    #[error("relation '{0}' not found in namespace '{1}'")]
    RelationNotFound(String, String),

    /// An error occurred while parsing the DSL input.
    #[error("parsing error: {0}")]
    ParseError(String),

    /// An error occurred in the storage backend.
    #[error("storage error: {0}")]
    StorageError(#[from] StoreError),
}
