//! Typed schema IR, resolver, compiler, and validator.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use thiserror::Error;

use crate::domain::{ObjectType, RelationName, Relationship, SubjectRef};
use crate::error::ZanzibarError;
use crate::model::{NamespaceConfig, UsersetExpression as LegacyUsersetExpression};
use crate::parser::{self, LegacyNamespaceAst, LegacyRelationAst};

/// A source schema document.
#[derive(Debug, Clone, Copy)]
pub struct SchemaSource<'a> {
    /// Optional human-readable source name for diagnostics.
    pub name: Option<&'a str>,
    /// Schema source text.
    pub text: &'a str,
}

/// Errors produced while compiling or validating schemas.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum SchemaError {
    /// The schema has two definitions with the same object type.
    #[error("duplicate namespace definition '{namespace}'")]
    DuplicateNamespace {
        /// Duplicate namespace name.
        namespace: String,
    },

    /// A namespace has two relations or permissions with the same name.
    #[error("duplicate relation '{relation}' in namespace '{namespace}'")]
    DuplicateRelation {
        /// Namespace containing the duplicate.
        namespace: String,
        /// Duplicate relation name.
        relation: String,
    },

    /// A namespace was requested but is not present in the schema.
    #[error("namespace '{namespace}' not found")]
    NamespaceNotFound {
        /// Missing namespace name.
        namespace: String,
    },

    /// A relation was requested but is not present in the namespace.
    #[error("relation '{relation}' not found in namespace '{namespace}'")]
    RelationNotFound {
        /// Namespace searched.
        namespace: String,
        /// Missing relation name.
        relation: String,
    },

    /// A schema expression references a relation that does not exist.
    #[error(
        "relation '{relation}' in '{namespace}.{owner}' references missing relation '{missing}'"
    )]
    MissingRelationReference {
        /// Namespace containing the owner relation.
        namespace: String,
        /// Relation that owns the invalid expression.
        owner: String,
        /// Invalid expression edge.
        relation: &'static str,
        /// Missing relation name.
        missing: String,
    },

    /// A tuple-to-userset target relation cannot be resolved from the known schema.
    #[error("tuple-to-userset in '{namespace}.{owner}' references unavailable target relation '{missing}'")]
    MissingTupleToUsersetTarget {
        /// Namespace containing the owner relation.
        namespace: String,
        /// Relation that owns the invalid expression.
        owner: String,
        /// Missing target relation name.
        missing: String,
    },

    /// A set operation does not contain enough operands.
    #[error("{operator} in '{namespace}.{owner}' must contain at least {min_operands} operands")]
    EmptyExpression {
        /// Namespace containing the owner relation.
        namespace: String,
        /// Relation that owns the invalid expression.
        owner: String,
        /// Operator name.
        operator: &'static str,
        /// Minimum operand count.
        min_operands: usize,
    },
}

/// Immutable compiled schema.
#[derive(Debug, Clone)]
pub struct CompiledSchema {
    definitions: Arc<[NamespaceDefinition]>,
    resolver: SchemaResolver,
}

impl CompiledSchema {
    fn new(definitions: Arc<[NamespaceDefinition]>) -> Result<Self, SchemaError> {
        let resolver = SchemaResolver::new(Arc::clone(&definitions))?;
        Ok(Self {
            definitions,
            resolver,
        })
    }

    /// Creates and validates a compiled schema from typed namespace definitions.
    ///
    /// # Errors
    ///
    /// Returns [`SchemaError`] when definitions are duplicated or expression references cannot be
    /// resolved.
    pub fn from_definitions(
        definitions: impl IntoIterator<Item = NamespaceDefinition>,
    ) -> Result<Self, SchemaError> {
        let definitions = definitions.into_iter().collect::<Vec<_>>();
        let compiled = Self::new(Arc::from(definitions.into_boxed_slice()))?;
        validate_references(&compiled)?;
        Ok(compiled)
    }

    /// Returns all namespace definitions in source order.
    #[must_use]
    pub fn definitions(&self) -> &[NamespaceDefinition] {
        &self.definitions
    }

    /// Returns the schema resolver.
    #[must_use]
    pub fn resolver(&self) -> &SchemaResolver {
        &self.resolver
    }

    /// Validates that a relationship references a known resource namespace and relation.
    ///
    /// # Errors
    ///
    /// Returns [`SchemaError::RelationNotFound`] or [`SchemaError::NamespaceNotFound`] when the
    /// relationship resource does not match this schema.
    pub fn validate_relationship(&self, relationship: &Relationship) -> Result<(), SchemaError> {
        self.resolver.relation(
            relationship.resource().object_type(),
            relationship.relation(),
        )?;
        if let SubjectRef::Userset { object, relation } = relationship.subject() {
            self.resolver.relation(object.object_type(), relation)?;
        }
        Ok(())
    }
}

/// A typed namespace definition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NamespaceDefinition {
    name: ObjectType,
    relations: Arc<[RelationDefinition]>,
}

impl NamespaceDefinition {
    /// Creates a namespace definition from validated fields.
    #[must_use]
    pub fn new(name: ObjectType, relations: Arc<[RelationDefinition]>) -> Self {
        Self { name, relations }
    }

    /// Returns the namespace name.
    #[must_use]
    pub fn name(&self) -> &ObjectType {
        &self.name
    }

    /// Returns relation definitions.
    #[must_use]
    pub fn relations(&self) -> &[RelationDefinition] {
        &self.relations
    }
}

/// A typed relation or permission definition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelationDefinition {
    name: RelationName,
    allowed_subject_types: AllowedSubjectTypes,
    userset_rewrite: Option<UsersetExpression>,
}

impl RelationDefinition {
    /// Creates a relation definition from validated fields.
    #[must_use]
    pub fn new(name: RelationName, userset_rewrite: Option<UsersetExpression>) -> Self {
        Self {
            name,
            allowed_subject_types: AllowedSubjectTypes::Unspecified,
            userset_rewrite,
        }
    }

    /// Creates a relation definition with explicit allowed subject object types.
    #[must_use]
    pub fn with_allowed_subject_types(
        name: RelationName,
        allowed_subject_types: Arc<[ObjectType]>,
        userset_rewrite: Option<UsersetExpression>,
    ) -> Self {
        Self {
            name,
            allowed_subject_types: AllowedSubjectTypes::Explicit(allowed_subject_types),
            userset_rewrite,
        }
    }

    /// Returns the relation name.
    #[must_use]
    pub fn name(&self) -> &RelationName {
        &self.name
    }

    /// Returns allowed subject object type metadata.
    #[must_use]
    pub fn allowed_subject_types(&self) -> &AllowedSubjectTypes {
        &self.allowed_subject_types
    }

    /// Returns the optional userset rewrite.
    #[must_use]
    pub fn userset_rewrite(&self) -> Option<&UsersetExpression> {
        self.userset_rewrite.as_ref()
    }
}

/// Allowed subject object type metadata for a relation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AllowedSubjectTypes {
    /// Legacy schema source did not declare allowed subjects.
    Unspecified,
    /// The relation accepts userset subjects from these object types.
    Explicit(Arc<[ObjectType]>),
}

/// Typed userset expression.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UsersetExpression {
    /// Direct relationships stored on this object and relation.
    This,
    /// A relation on the same object.
    ComputedUserset {
        /// Referenced relation.
        relation: RelationName,
    },
    /// A relation reached through userset subjects on another relation.
    TupleToUserset {
        /// Relation containing intermediate userset subjects.
        tupleset_relation: RelationName,
        /// Relation to evaluate on each intermediate object.
        computed_userset_relation: RelationName,
    },
    /// Union of child expressions.
    Union(Vec<UsersetExpression>),
    /// Intersection of child expressions.
    Intersection(Vec<UsersetExpression>),
    /// Exclusion of one expression from another.
    Exclusion {
        /// Base expression.
        base: Box<UsersetExpression>,
        /// Expression to subtract.
        exclude: Box<UsersetExpression>,
    },
}

/// Resolver for namespace and relation definitions.
#[derive(Debug, Clone)]
pub struct SchemaResolver {
    definitions: Arc<[NamespaceDefinition]>,
    namespace_indexes: HashMap<ObjectType, usize>,
    relation_indexes: HashMap<ObjectType, HashMap<RelationName, usize>>,
}

impl SchemaResolver {
    fn new(definitions: Arc<[NamespaceDefinition]>) -> Result<Self, SchemaError> {
        let mut namespace_indexes = HashMap::with_capacity(definitions.len());
        let mut relation_indexes = HashMap::with_capacity(definitions.len());

        for (namespace_index, namespace) in definitions.iter().enumerate() {
            let previous = namespace_indexes.insert(namespace.name.clone(), namespace_index);
            if previous.is_some() {
                return Err(SchemaError::DuplicateNamespace {
                    namespace: namespace.name.to_string(),
                });
            }

            let mut relations = HashMap::with_capacity(namespace.relations.len());
            for (relation_index, relation) in namespace.relations.iter().enumerate() {
                let previous = relations.insert(relation.name.clone(), relation_index);
                if previous.is_some() {
                    return Err(SchemaError::DuplicateRelation {
                        namespace: namespace.name.to_string(),
                        relation: relation.name.to_string(),
                    });
                }
            }
            relation_indexes.insert(namespace.name.clone(), relations);
        }

        Ok(Self {
            definitions,
            namespace_indexes,
            relation_indexes,
        })
    }

    /// Resolves a namespace definition.
    ///
    /// # Errors
    ///
    /// Returns [`SchemaError::NamespaceNotFound`] when the object type is unknown.
    pub fn namespace(&self, object_type: &ObjectType) -> Result<&NamespaceDefinition, SchemaError> {
        let index = self.namespace_indexes.get(object_type).ok_or_else(|| {
            SchemaError::NamespaceNotFound {
                namespace: object_type.to_string(),
            }
        })?;
        self.definitions
            .get(*index)
            .ok_or_else(|| SchemaError::NamespaceNotFound {
                namespace: object_type.to_string(),
            })
    }

    /// Resolves a relation definition.
    ///
    /// # Errors
    ///
    /// Returns [`SchemaError::NamespaceNotFound`] when the object type is unknown or
    /// [`SchemaError::RelationNotFound`] when the relation is unknown.
    pub fn relation(
        &self,
        object_type: &ObjectType,
        relation: &RelationName,
    ) -> Result<&RelationDefinition, SchemaError> {
        let namespace = self.namespace(object_type)?;
        let relations = self.relation_indexes.get(object_type).ok_or_else(|| {
            SchemaError::NamespaceNotFound {
                namespace: object_type.to_string(),
            }
        })?;
        let relation_index =
            relations
                .get(relation)
                .ok_or_else(|| SchemaError::RelationNotFound {
                    namespace: object_type.to_string(),
                    relation: relation.to_string(),
                })?;
        namespace
            .relations
            .get(*relation_index)
            .ok_or_else(|| SchemaError::RelationNotFound {
                namespace: object_type.to_string(),
                relation: relation.to_string(),
            })
    }

    fn relation_exists_anywhere(&self, relation: &RelationName) -> bool {
        self.relation_indexes
            .values()
            .any(|relations| relations.contains_key(relation))
    }
}

/// Compiles legacy DSL source into a typed schema.
///
/// # Errors
///
/// Returns [`ZanzibarError::ParseError`] for syntax errors, [`crate::domain::DomainError`] for
/// invalid identifiers, or [`SchemaError`] for invalid schema references.
pub fn compile_legacy_dsl(source: &str) -> Result<CompiledSchema, ZanzibarError> {
    compile_legacy_ast(parser::parse_dsl_ast(source)?)
}

/// Compiles legacy namespace configs into a typed schema.
///
/// # Errors
///
/// Returns [`crate::domain::DomainError`] for invalid identifiers or [`SchemaError`] for invalid
/// schema references.
pub fn compile_legacy_configs(
    configs: impl IntoIterator<Item = NamespaceConfig>,
) -> Result<CompiledSchema, ZanzibarError> {
    let mut namespaces = Vec::new();
    for config in configs {
        let mut relations = Vec::new();
        for relation in config.relations.into_values() {
            relations.push(LegacyRelationAst {
                name: relation.name.0,
                rewrite: relation.userset_rewrite,
            });
        }
        namespaces.push(LegacyNamespaceAst {
            name: config.name,
            relations,
        });
    }
    compile_legacy_ast(namespaces)
}

fn compile_legacy_ast(
    namespaces: Vec<LegacyNamespaceAst>,
) -> Result<CompiledSchema, ZanzibarError> {
    let mut definitions = Vec::with_capacity(namespaces.len());
    for namespace in namespaces {
        let mut relations = Vec::with_capacity(namespace.relations.len());
        for relation in namespace.relations {
            relations.push(compile_legacy_relation(relation)?);
        }
        definitions.push(NamespaceDefinition::new(
            ObjectType::try_from(namespace.name.as_str())?,
            Arc::from(relations.into_boxed_slice()),
        ));
    }

    CompiledSchema::from_definitions(definitions).map_err(Into::into)
}

fn compile_legacy_relation(
    relation: LegacyRelationAst,
) -> Result<RelationDefinition, ZanzibarError> {
    Ok(RelationDefinition::new(
        RelationName::try_from(relation.name.as_str())?,
        relation
            .rewrite
            .map(compile_legacy_expression)
            .transpose()?,
    ))
}

fn compile_legacy_expression(
    expression: LegacyUsersetExpression,
) -> Result<UsersetExpression, ZanzibarError> {
    match expression {
        LegacyUsersetExpression::This => Ok(UsersetExpression::This),
        LegacyUsersetExpression::ComputedUserset { relation } => {
            Ok(UsersetExpression::ComputedUserset {
                relation: RelationName::try_from(relation.0.as_str())?,
            })
        }
        LegacyUsersetExpression::TupleToUserset {
            tupleset_relation,
            computed_userset_relation,
        } => Ok(UsersetExpression::TupleToUserset {
            tupleset_relation: RelationName::try_from(tupleset_relation.0.as_str())?,
            computed_userset_relation: RelationName::try_from(
                computed_userset_relation.0.as_str(),
            )?,
        }),
        LegacyUsersetExpression::Union(expressions) => expressions
            .into_iter()
            .map(compile_legacy_expression)
            .collect::<Result<Vec<_>, _>>()
            .map(UsersetExpression::Union),
        LegacyUsersetExpression::Intersection(expressions) => expressions
            .into_iter()
            .map(compile_legacy_expression)
            .collect::<Result<Vec<_>, _>>()
            .map(UsersetExpression::Intersection),
        LegacyUsersetExpression::Exclusion { base, exclude } => Ok(UsersetExpression::Exclusion {
            base: Box::new(compile_legacy_expression(*base)?),
            exclude: Box::new(compile_legacy_expression(*exclude)?),
        }),
    }
}

fn validate_references(compiled: &CompiledSchema) -> Result<(), SchemaError> {
    for namespace in compiled.definitions() {
        let mut relations = HashSet::with_capacity(namespace.relations().len());
        for relation in namespace.relations() {
            let inserted = relations.insert(relation.name().clone());
            if !inserted {
                return Err(SchemaError::DuplicateRelation {
                    namespace: namespace.name().to_string(),
                    relation: relation.name().to_string(),
                });
            }
        }

        for relation in namespace.relations() {
            if let Some(expression) = relation.userset_rewrite() {
                validate_expression(compiled, namespace, relation.name(), expression)?;
            }
        }
    }
    Ok(())
}

fn validate_expression(
    compiled: &CompiledSchema,
    namespace: &NamespaceDefinition,
    owner: &RelationName,
    expression: &UsersetExpression,
) -> Result<(), SchemaError> {
    match expression {
        UsersetExpression::This => Ok(()),
        UsersetExpression::ComputedUserset { relation } => ensure_relation_in_namespace(
            compiled,
            namespace.name(),
            owner,
            "computed userset",
            relation,
        )
        .map(|_| ()),
        UsersetExpression::TupleToUserset {
            tupleset_relation,
            computed_userset_relation,
        } => {
            let tupleset_relation_definition = ensure_relation_in_namespace(
                compiled,
                namespace.name(),
                owner,
                "tuple-to-userset tupleset",
                tupleset_relation,
            )?;
            validate_tuple_to_userset_target(
                compiled,
                namespace,
                owner,
                tupleset_relation_definition,
                computed_userset_relation,
            )
        }
        UsersetExpression::Union(expressions) => {
            validate_operands(namespace, owner, "union", 1, expressions)?;
            for child in expressions {
                validate_expression(compiled, namespace, owner, child)?;
            }
            Ok(())
        }
        UsersetExpression::Intersection(expressions) => {
            validate_operands(namespace, owner, "intersection", 1, expressions)?;
            for child in expressions {
                validate_expression(compiled, namespace, owner, child)?;
            }
            Ok(())
        }
        UsersetExpression::Exclusion { base, exclude } => {
            validate_expression(compiled, namespace, owner, base)?;
            validate_expression(compiled, namespace, owner, exclude)
        }
    }
}

fn validate_operands(
    namespace: &NamespaceDefinition,
    owner: &RelationName,
    operator: &'static str,
    min_operands: usize,
    expressions: &[UsersetExpression],
) -> Result<(), SchemaError> {
    if expressions.len() < min_operands {
        return Err(SchemaError::EmptyExpression {
            namespace: namespace.name().to_string(),
            owner: owner.to_string(),
            operator,
            min_operands,
        });
    }
    Ok(())
}

fn ensure_relation_in_namespace<'schema>(
    compiled: &'schema CompiledSchema,
    namespace: &ObjectType,
    owner: &RelationName,
    relation_kind: &'static str,
    relation: &RelationName,
) -> Result<&'schema RelationDefinition, SchemaError> {
    match compiled.resolver().relation(namespace, relation) {
        Ok(relation) => Ok(relation),
        Err(SchemaError::RelationNotFound { .. }) => Err(SchemaError::MissingRelationReference {
            namespace: namespace.to_string(),
            owner: owner.to_string(),
            relation: relation_kind,
            missing: relation.to_string(),
        }),
        Err(error) => Err(error),
    }
}

fn validate_tuple_to_userset_target(
    compiled: &CompiledSchema,
    namespace: &NamespaceDefinition,
    owner: &RelationName,
    tupleset_relation: &RelationDefinition,
    computed_userset_relation: &RelationName,
) -> Result<(), SchemaError> {
    match tupleset_relation.allowed_subject_types() {
        AllowedSubjectTypes::Explicit(object_types) => {
            for object_type in object_types.iter() {
                if compiled
                    .resolver()
                    .relation(object_type, computed_userset_relation)
                    .is_err()
                {
                    return Err(SchemaError::MissingTupleToUsersetTarget {
                        namespace: namespace.name().to_string(),
                        owner: owner.to_string(),
                        missing: computed_userset_relation.to_string(),
                    });
                }
            }
            Ok(())
        }
        AllowedSubjectTypes::Unspecified => {
            if compiled
                .resolver()
                .relation_exists_anywhere(computed_userset_relation)
            {
                Ok(())
            } else {
                Err(SchemaError::MissingTupleToUsersetTarget {
                    namespace: namespace.name().to_string(),
                    owner: owner.to_string(),
                    missing: computed_userset_relation.to_string(),
                })
            }
        }
    }
}
