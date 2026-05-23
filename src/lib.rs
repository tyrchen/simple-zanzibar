//! A simplified Rust implementation of Google's Zanzibar authorization system.

pub mod domain;
pub mod error;
pub mod eval;
pub mod model;
pub mod parser;
pub mod relationship;
pub mod schema;
pub mod store;

use std::collections::{HashMap, HashSet};

use crate::error::ZanzibarError;
use crate::model::{NamespaceConfig, Object, Relation, RelationTuple, User};
use crate::relationship::{
    IndexedRelationshipStore, Precondition, RelationshipFilter, RelationshipMutation, SubjectFilter,
};
use crate::schema::CompiledSchema;
use crate::store::{InMemoryTupleStore, TupleStore};

/// The main service for handling Zanzibar authorization checks.
pub struct ZanzibarService {
    configs: HashMap<String, NamespaceConfig>,
    schema: Option<CompiledSchema>,
    relationships: IndexedRelationshipStore,
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
            relationships: IndexedRelationshipStore::default(),
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
        let next_relationships = self.rebuild_relationship_store(&compiled_schema)?;
        self.configs = next_configs;
        self.schema = Some(compiled_schema);
        self.relationships = next_relationships;
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
        let next_relationships = self.rebuild_relationship_store(&compiled_schema)?;
        self.configs = next_configs;
        self.schema = Some(compiled_schema);
        self.relationships = next_relationships;
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
            self.relationships
                .apply_mutations([RelationshipMutation::Create(relationship)], [])?;
        }

        self.store
            .write_tuple(tuple)
            .map_err(ZanzibarError::StorageError)
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
    ) -> Result<(), ZanzibarError> {
        let schema = self.schema.as_ref().ok_or(ZanzibarError::SchemaRequired)?;
        let mutations = mutations.into_iter().collect::<Vec<_>>();
        let preconditions = preconditions.into_iter().collect::<Vec<_>>();

        for mutation in &mutations {
            schema.validate_relationship(mutation.relationship())?;
        }
        for precondition in &preconditions {
            validate_precondition_filter(schema, precondition)?;
        }

        let mut candidate = self.relationships.clone();
        candidate.apply_mutations(mutations, preconditions)?;
        let tuples = candidate
            .rows()
            .iter()
            .map(relation_tuple_from_relationship)
            .collect::<Result<Vec<_>, _>>()?;

        self.store.replace_all(tuples);
        self.relationships = candidate;
        Ok(())
    }

    /// Deletes a relation tuple from the store.
    ///
    /// # Errors
    ///
    /// Returns [`ZanzibarError::StorageError`] when the underlying store rejects the delete.
    pub fn delete_tuple(&mut self, tuple: &RelationTuple) -> Result<(), ZanzibarError> {
        if self.schema.is_some() {
            let relationship = domain::Relationship::try_from(tuple)?;
            self.relationships
                .apply_mutations([RelationshipMutation::Delete(relationship)], [])?;
        }

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
            let resource = domain::ObjectRef::try_from(object)?;
            let object_type = resource.object_type().clone();
            let relation_name = domain::RelationName::try_from(relation.0.as_str())?;
            schema.resolver().relation(&object_type, &relation_name)?;
            return eval::check_with_indexed_store(
                object,
                relation,
                user,
                &self.configs,
                &self.relationships,
                &mut HashSet::new(),
            );
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

    fn rebuild_relationship_store(
        &self,
        schema: &CompiledSchema,
    ) -> Result<IndexedRelationshipStore, ZanzibarError> {
        let mut relationships = IndexedRelationshipStore::default();
        for tuple in self.store.all_tuples() {
            let relationship = domain::Relationship::try_from(&tuple)?;
            schema.validate_relationship(&relationship)?;
            relationships.apply_mutations([RelationshipMutation::Touch(relationship)], [])?;
        }
        Ok(relationships)
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

fn relation_tuple_from_relationship(
    relationship: &domain::Relationship,
) -> Result<RelationTuple, ZanzibarError> {
    let object = legacy_object_from_domain(relationship.resource());
    let relation = Relation(relationship.relation().as_str().to_string());
    let user = match relationship.subject() {
        domain::SubjectRef::Object(subject) if subject.object_type().as_str() == "user" => {
            User::UserId(subject.object_id().as_str().to_string())
        }
        domain::SubjectRef::Object(subject) => {
            return Err(ZanzibarError::StorageError(format!(
                "legacy tuple store cannot represent direct subject type '{}'",
                subject.object_type()
            )));
        }
        domain::SubjectRef::Userset { object, relation } => User::Userset(
            legacy_object_from_domain(object),
            Relation(relation.as_str().to_string()),
        ),
    };

    Ok(RelationTuple {
        object,
        relation,
        user,
    })
}

fn legacy_object_from_domain(object: &domain::ObjectRef) -> Object {
    Object {
        namespace: object.object_type().as_str().to_string(),
        id: object.object_id().as_str().to_string(),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn it_works() {
        assert_eq!(2 + 2, 4);
    }
}
