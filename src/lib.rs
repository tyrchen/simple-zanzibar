//! A simplified Rust implementation of Google's Zanzibar authorization system.
//!
//! This library provides a policy DSL, an in-memory tuple store, and an evaluation
//! engine that supports cross-namespace authorization checks including computed
//! usersets, tuple-to-userset references, and set operations (union, intersection,
//! exclusion).
//!
//! # Examples
//!
//! ```
//! use simple_zanzibar::ZanzibarService;
//! use simple_zanzibar::model::{Object, Relation, RelationTuple, User};
//!
//! let mut service = ZanzibarService::new();
//! service.add_dsl(r#"
//!     namespace doc {
//!         relation owner {}
//!         relation viewer {
//!             rewrite union(this, computed_userset(relation: "owner"))
//!         }
//!     }
//! "#).unwrap();
//!
//! let doc = Object::new("doc", "readme");
//! let owner = Relation::new("owner");
//! let viewer = Relation::new("viewer");
//! let alice = User::user_id("alice");
//!
//! service.write_tuple(RelationTuple::new(doc.clone(), owner, alice.clone())).unwrap();
//! assert!(service.check(&doc, &viewer, &alice).unwrap());
//! ```

pub mod error;
pub mod eval;
pub mod model;
pub mod parser;
pub mod store;

use std::collections::{HashMap, HashSet};

use crate::{
    error::ZanzibarError,
    model::{NamespaceConfig, Object, Relation, RelationTuple, User},
    store::{InMemoryTupleStore, TupleStore},
};

/// The main service for handling Zanzibar authorization checks.
///
/// Holds namespace configurations and a tuple store, providing a high-level API
/// for writing tuples and performing authorization checks across namespaces.
#[derive(Debug)]
pub struct ZanzibarService {
    configs: HashMap<String, NamespaceConfig>,
    store: Box<dyn TupleStore>,
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
        Self {
            configs: HashMap::new(),
            store: Box::new(InMemoryTupleStore::default()),
        }
    }

    /// Creates a new service with a custom store implementation.
    #[must_use]
    pub fn with_store(store: Box<dyn TupleStore>) -> Self {
        Self {
            configs: HashMap::new(),
            store,
        }
    }

    /// Parses a DSL string and adds the resulting configurations to the service.
    ///
    /// # Errors
    ///
    /// Returns [`ZanzibarError::ParseError`] if the DSL input is invalid.
    pub fn add_dsl(&mut self, dsl: &str) -> Result<(), ZanzibarError> {
        let configs = parser::parse_dsl(dsl)?;
        for config in configs {
            self.add_config(config);
        }
        Ok(())
    }

    /// Adds or updates a namespace configuration.
    pub fn add_config(&mut self, config: NamespaceConfig) {
        self.configs.insert(config.name.clone(), config);
    }

    /// Writes a relation tuple to the store.
    ///
    /// # Errors
    ///
    /// Returns [`ZanzibarError::StorageError`] if the tuple already exists.
    pub fn write_tuple(&mut self, tuple: RelationTuple) -> Result<(), ZanzibarError> {
        self.store.write_tuple(tuple)?;
        Ok(())
    }

    /// Deletes a relation tuple from the store.
    ///
    /// # Errors
    ///
    /// Returns [`ZanzibarError::StorageError`] if the tuple does not exist.
    pub fn delete_tuple(&mut self, tuple: &RelationTuple) -> Result<(), ZanzibarError> {
        self.store.delete_tuple(tuple)?;
        Ok(())
    }

    /// Checks if a user has a specific relation to an object.
    ///
    /// This resolves all userset rewrites, computed usersets, and
    /// tuple-to-userset references across namespace boundaries.
    ///
    /// # Errors
    ///
    /// Returns [`ZanzibarError::NamespaceNotFound`] if the object's namespace is not configured.
    /// Returns [`ZanzibarError::RelationNotFound`] if the relation is not defined in the namespace.
    pub fn check(
        &self,
        object: &Object,
        relation: &Relation,
        user: &User,
    ) -> Result<bool, ZanzibarError> {
        eval::check(
            object,
            relation,
            user,
            &self.configs,
            self.store.as_ref(),
            &mut HashSet::new(),
        )
    }

    /// Expands the userset for a given object and relation.
    ///
    /// Returns the full tree of users and usersets that have the given relation.
    ///
    /// # Errors
    ///
    /// Returns [`ZanzibarError::NamespaceNotFound`] if the object's namespace is not configured.
    /// Returns [`ZanzibarError::RelationNotFound`] if the relation is not defined in the namespace.
    pub fn expand(
        &self,
        object: &Object,
        relation: &Relation,
    ) -> Result<model::ExpandedUserset, ZanzibarError> {
        eval::expand(object, relation, &self.configs, self.store.as_ref())
    }
}
