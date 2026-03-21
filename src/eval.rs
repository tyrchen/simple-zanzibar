//! Core evaluation logic for `check` and `expand` requests.
//!
//! This module implements the Zanzibar authorization evaluation engine, supporting
//! cross-namespace resolution, cycle detection, and all userset expression types
//! (union, intersection, exclusion, computed userset, tuple-to-userset).

use std::collections::{HashMap, HashSet};

use crate::{
    error::ZanzibarError,
    model::{ExpandedUserset, NamespaceConfig, Object, Relation, User, UsersetExpression},
    store::TupleStore,
};

/// Evaluates a `check` request to determine if a user has a specific relation to an object.
///
/// This resolves all userset rewrites, computed usersets, and tuple-to-userset references
/// across namespace boundaries.
///
/// # Errors
///
/// Returns [`ZanzibarError::NamespaceNotFound`] if the object's namespace is not configured.
/// Returns [`ZanzibarError::RelationNotFound`] if the relation is not defined in the namespace.
pub fn check(
    object: &Object,
    relation: &Relation,
    user: &User,
    configs: &HashMap<String, NamespaceConfig>,
    store: &(impl TupleStore + ?Sized),
    visited: &mut HashSet<(Object, Relation, User)>,
) -> Result<bool, ZanzibarError> {
    check_internal(object, relation, user, configs, store, visited)
}

/// Evaluates an `expand` request, returning the full userset tree for a given
/// object and relation.
///
/// # Errors
///
/// Returns [`ZanzibarError::NamespaceNotFound`] if the object's namespace is not configured.
/// Returns [`ZanzibarError::RelationNotFound`] if the relation is not defined in the namespace.
pub fn expand(
    object: &Object,
    relation: &Relation,
    configs: &HashMap<String, NamespaceConfig>,
    store: &(impl TupleStore + ?Sized),
) -> Result<ExpandedUserset, ZanzibarError> {
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

fn check_internal(
    object: &Object,
    relation: &Relation,
    user: &User,
    configs: &HashMap<String, NamespaceConfig>,
    store: &(impl TupleStore + ?Sized),
    visited: &mut HashSet<(Object, Relation, User)>,
) -> Result<bool, ZanzibarError> {
    if !visited.insert((object.clone(), relation.clone(), user.clone())) {
        // Cycle detected — this exact check is already on the call stack.
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

    // Remove the current check from `visited` on the way back up the recursion,
    // allowing the same triple to be checked via different paths.
    visited.remove(&(object.clone(), relation.clone(), user.clone()));
    result
}

fn eval_expression(
    object: &Object,
    relation: &Relation,
    user: &User,
    configs: &HashMap<String, NamespaceConfig>,
    store: &(impl TupleStore + ?Sized),
    visited: &mut HashSet<(Object, Relation, User)>,
    expression: &UsersetExpression,
) -> Result<bool, ZanzibarError> {
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
                if let User::Userset(ref obj, ref rel) = t.user {
                    // Start a fresh check for the referenced userset, which may be
                    // in a different namespace — configs lookup handles this correctly.
                    if check_internal(obj, rel, user, configs, store, visited)? {
                        return Ok(true);
                    }
                }
            }
            Ok(false)
        }
        UsersetExpression::ComputedUserset {
            relation: computed_relation,
        } => {
            // Check the computed relation on the same object.
            check_internal(object, computed_relation, user, configs, store, visited)
        }
        UsersetExpression::TupleToUserset {
            tupleset_relation,
            computed_userset_relation,
        } => {
            let tupleset = store.read_tuples(object, Some(tupleset_relation), None);
            for t in tupleset {
                if let User::Userset(ref intermediate_obj, _) = t.user {
                    // The intermediate object may be in a different namespace;
                    // check_internal resolves the correct config for it.
                    if check_internal(
                        intermediate_obj,
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
            // Empty intersection grants no access (safe default).
            if expressions.is_empty() {
                return Ok(false);
            }
            for expr in expressions {
                if !eval_expression(object, relation, user, configs, store, visited, expr)? {
                    return Ok(false);
                }
            }
            Ok(true)
        }
        UsersetExpression::Exclusion { base, exclude } => {
            let is_excluded =
                eval_expression(object, relation, user, configs, store, visited, exclude)?;
            if is_excluded {
                return Ok(false);
            }
            eval_expression(object, relation, user, configs, store, visited, base)
        }
    }
}

fn expand_expression(
    object: &Object,
    relation: &Relation,
    configs: &HashMap<String, NamespaceConfig>,
    store: &(impl TupleStore + ?Sized),
    expression: &UsersetExpression,
) -> Result<ExpandedUserset, ZanzibarError> {
    match expression {
        UsersetExpression::This => {
            let direct_tuples = store.read_tuples(object, Some(relation), None);
            let users = direct_tuples
                .into_iter()
                .map(|t| match t.user {
                    User::UserId(id) => ExpandedUserset::User(id),
                    User::Userset(obj, rel) => ExpandedUserset::Userset(obj, rel),
                })
                .collect();
            Ok(ExpandedUserset::Union(users))
        }
        UsersetExpression::ComputedUserset {
            relation: computed_relation,
        } => expand(object, computed_relation, configs, store),
        UsersetExpression::TupleToUserset {
            tupleset_relation,
            computed_userset_relation,
        } => {
            let tupleset = store.read_tuples(object, Some(tupleset_relation), None);
            let mut users = Vec::with_capacity(tupleset.len());
            for t in tupleset {
                if let User::Userset(intermediate_obj, _) = t.user {
                    users.push(expand(
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
            let mut sub_expressions = Vec::with_capacity(expressions.len());
            for expr in expressions {
                sub_expressions.push(expand_expression(object, relation, configs, store, expr)?);
            }
            Ok(ExpandedUserset::Union(sub_expressions))
        }
        UsersetExpression::Intersection(expressions) => {
            let mut sub_expressions = Vec::with_capacity(expressions.len());
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
