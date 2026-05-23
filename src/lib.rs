//! A simplified Rust implementation of Google's Zanzibar authorization system.

pub mod domain;
pub mod error;
pub mod eval;
pub mod model;
pub mod parser;
pub mod schema;
pub mod store;

use std::collections::{HashMap, HashSet};

use crate::error::ZanzibarError;
use crate::model::{NamespaceConfig, Object, Relation, RelationTuple, User};
use crate::schema::CompiledSchema;
use crate::store::{InMemoryTupleStore, TupleStore};

/// The main service for handling Zanzibar authorization checks.
pub struct ZanzibarService {
    configs: HashMap<String, NamespaceConfig>,
    schema: Option<CompiledSchema>,
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
        ZanzibarService {
            configs: HashMap::new(),
            schema: None,
            store: Box::new(InMemoryTupleStore::default()),
        }
    }

    /// Parses a DSL string and adds the resulting configurations to the service.
    ///
    /// # Errors
    ///
    /// Returns [`ZanzibarError::ParseError`] when the DSL cannot be parsed.
    pub fn add_dsl(&mut self, dsl: &str) -> Result<(), ZanzibarError> {
        schema::compile_legacy_dsl(dsl)?;
        let configs = parser::parse_dsl(dsl)?;
        let mut next_configs = self.configs.clone();
        for config in configs {
            next_configs.insert(config.name.clone(), config);
        }
        let compiled_schema = schema::compile_legacy_configs(next_configs.values().cloned())?;
        self.configs = next_configs;
        self.schema = Some(compiled_schema);
        Ok(())
    }

    /// Adds or updates a namespace configuration.
    ///
    /// # Errors
    ///
    /// Returns [`ZanzibarError::Schema`] when the updated schema does not validate.
    pub fn add_config(&mut self, config: NamespaceConfig) -> Result<(), ZanzibarError> {
        let mut next_configs = self.configs.clone();
        next_configs.insert(config.name.clone(), config);
        let compiled_schema = schema::compile_legacy_configs(next_configs.values().cloned())?;
        self.configs = next_configs;
        self.schema = Some(compiled_schema);
        Ok(())
    }

    /// Writes a relation tuple to the store.
    ///
    /// # Errors
    ///
    /// Returns [`ZanzibarError::StorageError`] when the underlying store rejects the write.
    pub fn write_tuple(&mut self, tuple: RelationTuple) -> Result<(), ZanzibarError> {
        if let Some(schema) = &self.schema {
            let relationship = domain::Relationship::try_from(&tuple)?;
            schema.validate_relationship(&relationship)?;
        }

        self.store
            .write_tuple(tuple)
            .map_err(ZanzibarError::StorageError)
    }

    /// Deletes a relation tuple from the store.
    ///
    /// # Errors
    ///
    /// Returns [`ZanzibarError::StorageError`] when the underlying store rejects the delete.
    pub fn delete_tuple(&mut self, tuple: &RelationTuple) -> Result<(), ZanzibarError> {
        self.store
            .delete_tuple(tuple)
            .map_err(ZanzibarError::StorageError)
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
        if !self.configs.contains_key(&object.namespace) {
            return Err(ZanzibarError::NamespaceNotFound(object.namespace.clone()));
        }

        if let Some(schema) = &self.schema {
            let object_type = domain::ObjectType::try_from(object.namespace.as_str())?;
            let relation_name = domain::RelationName::try_from(relation.0.as_str())?;
            schema.resolver().relation(&object_type, &relation_name)?;
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
        if !self.configs.contains_key(&object.namespace) {
            return Err(ZanzibarError::NamespaceNotFound(object.namespace.clone()));
        }

        if let Some(schema) = &self.schema {
            let object_type = domain::ObjectType::try_from(object.namespace.as_str())?;
            let relation_name = domain::RelationName::try_from(relation.0.as_str())?;
            schema.resolver().relation(&object_type, &relation_name)?;
        }

        eval::expand_with_configs(object, relation, &self.configs, self.store.as_ref())
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn it_works() {
        assert_eq!(2 + 2, 4);
    }
}
