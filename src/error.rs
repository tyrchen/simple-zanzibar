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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_should_display_store_errors() {
        assert_eq!(
            StoreError::DuplicateTuple.to_string(),
            "tuple already exists"
        );
        assert_eq!(StoreError::TupleNotFound.to_string(), "tuple not found");
    }

    #[test]
    fn test_should_display_zanzibar_errors() {
        assert_eq!(
            ZanzibarError::NamespaceNotFound("doc".into()).to_string(),
            "namespace 'doc' not found"
        );
        assert_eq!(
            ZanzibarError::RelationNotFound("viewer".into(), "doc".into()).to_string(),
            "relation 'viewer' not found in namespace 'doc'"
        );
        assert_eq!(
            ZanzibarError::ParseError("bad input".into()).to_string(),
            "parsing error: bad input"
        );
        assert_eq!(
            ZanzibarError::StorageError(StoreError::DuplicateTuple).to_string(),
            "storage error: tuple already exists"
        );
    }

    #[test]
    fn test_should_convert_store_error_to_zanzibar_error() {
        let store_err = StoreError::TupleNotFound;
        let zanzibar_err: ZanzibarError = store_err.into();
        assert!(matches!(
            zanzibar_err,
            ZanzibarError::StorageError(StoreError::TupleNotFound)
        ));
    }
}
