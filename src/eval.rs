//! Core evaluation logic for `check` and `expand` requests.

use std::collections::{HashMap, HashSet};
use std::hash::BuildHasher;

use crate::domain::{
    ObjectRef as DomainObjectRef, ObjectType, RelationName, Relationship, SubjectRef,
};
use crate::error::ZanzibarError;
use crate::model::{ExpandedUserset, NamespaceConfig, Object, Relation, User, UsersetExpression};
use crate::relationship::{
    IndexedRelationshipStore, QueryLimit, RelationshipFilter, RelationshipReader, SubjectFilter,
};
use crate::store::TupleStore;

fn check_internal<S, C>(
    object: &Object,
    relation: &Relation,
    user: &User,
    configs: &HashMap<String, NamespaceConfig, C>,
    store: &(impl TupleStore + ?Sized),
    visited: &mut HashSet<(Object, Relation, User), S>,
) -> Result<bool, ZanzibarError>
where
    S: BuildHasher,
    C: BuildHasher,
{
    if !visited.insert((object.clone(), relation.clone(), user.clone())) {
        // We've already seen this exact check in this path, so we're in a cycle.
        return Ok(false);
    }

    let config = configs
        .get(&object.namespace)
        .ok_or_else(|| ZanzibarError::NamespaceNotFound(object.namespace.clone()))?;

    let relation_config = config
        .relations
        .get(relation)
        .ok_or_else(|| ZanzibarError::RelationNotFound(relation.0.clone(), config.name.clone()))?;

    // If there's a rewrite rule, evaluate it. Otherwise, default to `This`.
    let rewrite = relation_config
        .userset_rewrite
        .as_ref()
        .unwrap_or(&UsersetExpression::This);

    let result = eval_expression(object, relation, user, configs, store, visited, rewrite);

    // Important: remove the current check from `visited` on the way back up the recursion.
    visited.remove(&(object.clone(), relation.clone(), user.clone()));
    result
}

fn eval_expression<S, C>(
    object: &Object,
    relation: &Relation,
    user: &User,
    configs: &HashMap<String, NamespaceConfig, C>,
    store: &(impl TupleStore + ?Sized),
    visited: &mut HashSet<(Object, Relation, User), S>,
    expression: &UsersetExpression,
) -> Result<bool, ZanzibarError>
where
    S: BuildHasher,
    C: BuildHasher,
{
    match expression {
        UsersetExpression::This => {
            // Direct check: see if the exact tuple exists.
            let direct_tuples = store.read_tuples(object, Some(relation), Some(user));
            if !direct_tuples.is_empty() {
                return Ok(true);
            }

            // Indirect check: see if the user is in a userset that has the relation.
            let indirect_tuples = store.read_tuples(object, Some(relation), None);
            for t in indirect_tuples {
                if let User::Userset(obj, rel) = t.user {
                    // Note: a subtle but important detail. When checking a userset,
                    // we must start a fresh `check` call, not `eval_expression`,
                    // as the new object (`obj`) may have its own rewrite rules.
                    if check_internal(&obj, &rel, user, configs, store, visited)? {
                        return Ok(true);
                    }
                }
            }
            Ok(false)
        }
        UsersetExpression::ComputedUserset {
            relation: computed_relation,
        } => {
            // Similarly, start a fresh `check` to respect the rewrite rules of the new relation.
            check_internal(object, computed_relation, user, configs, store, visited)
        }
        UsersetExpression::TupleToUserset {
            tupleset_relation,
            computed_userset_relation,
        } => {
            let tupleset = store.read_tuples(object, Some(tupleset_relation), None);
            for t in tupleset {
                if let User::Userset(intermediate_obj, _) = t.user {
                    if check_internal(
                        &intermediate_obj,
                        computed_userset_relation,
                        user,
                        configs,
                        store,
                        visited,
                    )? {
                        return Ok(true);
                    }
                }
            }
            Ok(false)
        }
        UsersetExpression::Union(expressions) => {
            for expr in expressions {
                if eval_expression(object, relation, user, configs, store, visited, expr)? {
                    return Ok(true);
                }
            }
            Ok(false)
        }
        UsersetExpression::Intersection(expressions) => {
            for expr in expressions {
                // If any sub-expression is false, the whole intersection is false.
                if !eval_expression(object, relation, user, configs, store, visited, expr)? {
                    return Ok(false);
                }
            }
            // If we get here, all sub-expressions were true.
            Ok(true)
        }
        UsersetExpression::Exclusion { base, exclude } => {
            // If the user is in the `exclude` set, the result is false.
            let is_excluded =
                eval_expression(object, relation, user, configs, store, visited, exclude)?;
            if is_excluded {
                return Ok(false);
            }
            // Otherwise, the result is determined by the `base` set.
            eval_expression(object, relation, user, configs, store, visited, base)
        }
    }
}

/// Evaluates a `check` request.
///
/// # Errors
///
/// Returns [`ZanzibarError::RelationNotFound`] when the requested relation is not defined in the
/// supplied namespace config.
pub fn check<S>(
    object: &Object,
    relation: &Relation,
    user: &User,
    config: &NamespaceConfig,
    store: &(impl TupleStore + ?Sized),
    visited: &mut HashSet<(Object, Relation, User), S>,
) -> Result<bool, ZanzibarError>
where
    S: BuildHasher,
{
    let configs = HashMap::from([(config.name.clone(), config.clone())]);
    check_internal(object, relation, user, &configs, store, visited)
}

/// Evaluates a `check` request against a whole schema config map.
///
/// # Errors
///
/// Returns [`ZanzibarError::NamespaceNotFound`] or [`ZanzibarError::RelationNotFound`] when a
/// recursive userset edge references an unknown namespace or relation.
pub fn check_with_configs<S, C>(
    object: &Object,
    relation: &Relation,
    user: &User,
    configs: &HashMap<String, NamespaceConfig, C>,
    store: &(impl TupleStore + ?Sized),
    visited: &mut HashSet<(Object, Relation, User), S>,
) -> Result<bool, ZanzibarError>
where
    S: BuildHasher,
    C: BuildHasher,
{
    check_internal(object, relation, user, configs, store, visited)
}

/// Evaluates a `check` request against indexed relationships.
///
/// # Errors
///
/// Returns [`ZanzibarError`] when domain conversion, schema lookup, or relationship query setup
/// fails.
pub fn check_with_indexed_store<S, C>(
    object: &Object,
    relation: &Relation,
    user: &User,
    configs: &HashMap<String, NamespaceConfig, C>,
    relationships: &IndexedRelationshipStore,
    visited: &mut HashSet<(Object, Relation, User), S>,
) -> Result<bool, ZanzibarError>
where
    S: BuildHasher,
    C: BuildHasher,
{
    if !visited.insert((object.clone(), relation.clone(), user.clone())) {
        return Ok(false);
    }

    let config = configs
        .get(&object.namespace)
        .ok_or_else(|| ZanzibarError::NamespaceNotFound(object.namespace.clone()))?;
    let relation_config = config
        .relations
        .get(relation)
        .ok_or_else(|| ZanzibarError::RelationNotFound(relation.0.clone(), config.name.clone()))?;
    let rewrite = relation_config
        .userset_rewrite
        .as_ref()
        .unwrap_or(&UsersetExpression::This);

    let result = eval_indexed_expression(
        object,
        relation,
        user,
        configs,
        relationships,
        visited,
        rewrite,
    );
    visited.remove(&(object.clone(), relation.clone(), user.clone()));
    result
}

fn eval_indexed_expression<S, C>(
    object: &Object,
    relation: &Relation,
    user: &User,
    configs: &HashMap<String, NamespaceConfig, C>,
    relationships: &IndexedRelationshipStore,
    visited: &mut HashSet<(Object, Relation, User), S>,
    expression: &UsersetExpression,
) -> Result<bool, ZanzibarError>
where
    S: BuildHasher,
    C: BuildHasher,
{
    match expression {
        UsersetExpression::This => {
            eval_indexed_this(object, relation, user, configs, relationships, visited)
        }
        UsersetExpression::ComputedUserset {
            relation: computed_relation,
        } => check_with_indexed_store(
            object,
            computed_relation,
            user,
            configs,
            relationships,
            visited,
        ),
        UsersetExpression::TupleToUserset {
            tupleset_relation,
            computed_userset_relation,
        } => {
            for relationship in indexed_resource_relation(relationships, object, tupleset_relation)?
            {
                if let SubjectRef::Userset {
                    object: intermediate,
                    ..
                } = relationship.subject()
                {
                    let intermediate_object = legacy_object(intermediate);
                    if check_with_indexed_store(
                        &intermediate_object,
                        computed_userset_relation,
                        user,
                        configs,
                        relationships,
                        visited,
                    )? {
                        return Ok(true);
                    }
                }
            }
            Ok(false)
        }
        UsersetExpression::Union(expressions) => {
            for expression in expressions {
                if eval_indexed_expression(
                    object,
                    relation,
                    user,
                    configs,
                    relationships,
                    visited,
                    expression,
                )? {
                    return Ok(true);
                }
            }
            Ok(false)
        }
        UsersetExpression::Intersection(expressions) => {
            for expression in expressions {
                if !eval_indexed_expression(
                    object,
                    relation,
                    user,
                    configs,
                    relationships,
                    visited,
                    expression,
                )? {
                    return Ok(false);
                }
            }
            Ok(true)
        }
        UsersetExpression::Exclusion { base, exclude } => {
            if !eval_indexed_expression(
                object,
                relation,
                user,
                configs,
                relationships,
                visited,
                base,
            )? {
                return Ok(false);
            }
            let excluded = eval_indexed_expression(
                object,
                relation,
                user,
                configs,
                relationships,
                visited,
                exclude,
            )?;
            Ok(!excluded)
        }
    }
}

fn eval_indexed_this<S, C>(
    object: &Object,
    relation: &Relation,
    user: &User,
    configs: &HashMap<String, NamespaceConfig, C>,
    relationships: &IndexedRelationshipStore,
    visited: &mut HashSet<(Object, Relation, User), S>,
) -> Result<bool, ZanzibarError>
where
    S: BuildHasher,
    C: BuildHasher,
{
    let resource = DomainObjectRef::try_from(object)?;
    let relation_name = RelationName::try_from(relation)?;
    let subject = SubjectFilter::try_from(user)?;
    let exact_filter =
        RelationshipFilter::for_exact_subject(&resource, relation_name.clone(), subject);
    if relationships.any_resource_match(&exact_filter)? {
        return Ok(true);
    }

    for relationship in indexed_resource_relation(relationships, object, relation)? {
        if let SubjectRef::Userset {
            object: userset_object,
            relation: userset_relation,
        } = relationship.subject()
        {
            let nested_object = legacy_object(userset_object);
            let nested_relation = legacy_relation(userset_relation);
            if check_with_indexed_store(
                &nested_object,
                &nested_relation,
                user,
                configs,
                relationships,
                visited,
            )? {
                return Ok(true);
            }
        }
    }

    Ok(false)
}

fn indexed_resource_relation<'a>(
    relationships: &'a IndexedRelationshipStore,
    object: &Object,
    relation: &Relation,
) -> Result<impl Iterator<Item = &'a Relationship>, ZanzibarError> {
    let resource = DomainObjectRef::try_from(object)?;
    let relation_name = RelationName::try_from(relation)?;
    let filter = RelationshipFilter::new(
        ObjectType::try_from(object.namespace.as_str())?,
        Some(resource.object_id().clone()),
        Some(relation_name),
        None,
        QueryLimit::default(),
    );
    Ok(relationships.query_relationships(&filter)?)
}

fn legacy_object(object: &DomainObjectRef) -> Object {
    Object {
        namespace: object.object_type().as_str().to_string(),
        id: object.object_id().as_str().to_string(),
    }
}

fn legacy_relation(relation: &RelationName) -> Relation {
    Relation(relation.as_str().to_string())
}

/// Evaluates an `expand` request.
///
/// # Errors
///
/// Returns [`ZanzibarError::RelationNotFound`] when the requested relation is not defined in the
/// supplied namespace config.
pub fn expand(
    object: &Object,
    relation: &Relation,
    config: &NamespaceConfig,
    store: &(impl TupleStore + ?Sized),
) -> Result<ExpandedUserset, ZanzibarError> {
    let configs = HashMap::from([(config.name.clone(), config.clone())]);
    expand_with_configs(object, relation, &configs, store)
}

/// Evaluates an `expand` request against a whole schema config map.
///
/// # Errors
///
/// Returns [`ZanzibarError::NamespaceNotFound`] or [`ZanzibarError::RelationNotFound`] when the
/// requested relation or a recursive userset edge cannot be resolved.
pub fn expand_with_configs<C>(
    object: &Object,
    relation: &Relation,
    configs: &HashMap<String, NamespaceConfig, C>,
    store: &(impl TupleStore + ?Sized),
) -> Result<ExpandedUserset, ZanzibarError>
where
    C: BuildHasher,
{
    let config = configs
        .get(&object.namespace)
        .ok_or_else(|| ZanzibarError::NamespaceNotFound(object.namespace.clone()))?;

    let relation_config = config
        .relations
        .get(relation)
        .ok_or_else(|| ZanzibarError::RelationNotFound(relation.0.clone(), config.name.clone()))?;

    let rewrite = relation_config
        .userset_rewrite
        .as_ref()
        .unwrap_or(&UsersetExpression::This);

    expand_expression(object, relation, configs, store, rewrite)
}

fn expand_expression<C>(
    object: &Object,
    relation: &Relation,
    configs: &HashMap<String, NamespaceConfig, C>,
    store: &(impl TupleStore + ?Sized),
    expression: &UsersetExpression,
) -> Result<ExpandedUserset, ZanzibarError>
where
    C: BuildHasher,
{
    match expression {
        UsersetExpression::This => {
            let direct_tuples = store.read_tuples(object, Some(relation), None);
            let mut users = Vec::new();
            for t in direct_tuples {
                match t.user {
                    User::UserId(id) => users.push(ExpandedUserset::User(id)),
                    User::Userset(obj, rel) => users.push(ExpandedUserset::Userset(obj, rel)),
                }
            }
            Ok(ExpandedUserset::Union(users))
        }
        UsersetExpression::ComputedUserset {
            relation: computed_relation,
        } => expand_with_configs(object, computed_relation, configs, store),
        UsersetExpression::TupleToUserset {
            tupleset_relation,
            computed_userset_relation,
        } => {
            let tupleset = store.read_tuples(object, Some(tupleset_relation), None);
            let mut users = Vec::new();
            for t in tupleset {
                if let User::Userset(intermediate_obj, _) = t.user {
                    users.push(expand_with_configs(
                        &intermediate_obj,
                        computed_userset_relation,
                        configs,
                        store,
                    )?);
                }
            }
            Ok(ExpandedUserset::Union(users))
        }
        UsersetExpression::Union(expressions) => {
            let mut sub_expressions = Vec::new();
            for expr in expressions {
                sub_expressions.push(expand_expression(object, relation, configs, store, expr)?);
            }
            Ok(ExpandedUserset::Union(sub_expressions))
        }
        UsersetExpression::Intersection(expressions) => {
            let mut sub_expressions = Vec::new();
            for expr in expressions {
                sub_expressions.push(expand_expression(object, relation, configs, store, expr)?);
            }
            Ok(ExpandedUserset::Intersection(sub_expressions))
        }
        UsersetExpression::Exclusion { base, exclude } => {
            let base_expr = expand_expression(object, relation, configs, store, base)?;
            let exclude_expr = expand_expression(object, relation, configs, store, exclude)?;
            Ok(ExpandedUserset::Exclusion {
                base: Box::new(base_expr),
                exclude: Box::new(exclude_expr),
            })
        }
    }
}
