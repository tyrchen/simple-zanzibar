//! A simplified Rust implementation of Google's Zanzibar authorization system.

pub mod error;
pub mod eval;
pub mod model;
pub mod parser;
pub mod store;

use crate::error::ZanzibarError;
use crate::model::{NamespaceConfig, Object, Relation, RelationTuple, User};
use crate::store::{InMemoryTupleStore, TupleStore};
use std::collections::{HashMap, HashSet};

/// The main service for handling Zanzibar authorization checks.
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
    pub fn new() -> Self {
        ZanzibarService {
            configs: HashMap::new(),
            store: Box::new(InMemoryTupleStore::default()),
        }
    }

    /// Parses a DSL string and adds the resulting configurations to the service.
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
    pub fn write_tuple(&mut self, tuple: RelationTuple) -> Result<(), ZanzibarError> {
        self.store
            .write_tuple(tuple)
            .map_err(ZanzibarError::StorageError)
    }

    /// Deletes a relation tuple from the store.
    pub fn delete_tuple(&mut self, tuple: &RelationTuple) -> Result<(), ZanzibarError> {
        self.store
            .delete_tuple(tuple)
            .map_err(ZanzibarError::StorageError)
    }

    /// Checks if a user has a specific relation to an object.
    pub fn check(
        &self,
        object: &Object,
        relation: &Relation,
        user: &User,
    ) -> Result<bool, ZanzibarError> {
        let config = self
            .configs
            .get(&object.namespace)
            .ok_or_else(|| ZanzibarError::NamespaceNotFound(object.namespace.clone()))?;

        eval::check(
            object,
            relation,
            user,
            config,
            self.store.as_ref(),
            &mut HashSet::new(),
        )
    }

    /// Expands the userset for a given object and relation.
    pub fn expand(
        &self,
        object: &Object,
        relation: &Relation,
    ) -> Result<model::ExpandedUserset, ZanzibarError> {
        let config = self
            .configs
            .get(&object.namespace)
            .ok_or_else(|| ZanzibarError::NamespaceNotFound(object.namespace.clone()))?;

        eval::expand(object, relation, config, self.store.as_ref())
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn it_works() {
        assert_eq!(2 + 2, 4);
    }
}
