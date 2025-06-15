//! Core evaluation logic for `check` and `expand` requests.

use crate::error::ZanzibarError;
use crate::model::{ExpandedUserset, NamespaceConfig, Object, Relation, User, UsersetExpression};
use crate::store::TupleStore;
use std::collections::HashSet;

fn check_internal(
    object: &Object,
    relation: &Relation,
    user: &User,
    config: &NamespaceConfig,
    store: &(impl TupleStore + ?Sized),
    visited: &mut HashSet<(Object, Relation, User)>,
) -> Result<bool, ZanzibarError> {
    if !visited.insert((object.clone(), relation.clone(), user.clone())) {
        // We've already seen this exact check in this path, so we're in a cycle.
        return Ok(false);
    }

    let relation_config = config
        .relations
        .get(relation)
        .ok_or_else(|| ZanzibarError::RelationNotFound(relation.0.clone(), config.name.clone()))?;

    // If there's a rewrite rule, evaluate it. Otherwise, default to `This`.
    let rewrite = relation_config
        .userset_rewrite
        .as_ref()
        .unwrap_or(&UsersetExpression::This);

    let result = eval_expression(object, relation, user, config, store, visited, rewrite);

    // Important: remove the current check from `visited` on the way back up the recursion.
    visited.remove(&(object.clone(), relation.clone(), user.clone()));
    result
}

fn eval_expression(
    object: &Object,
    relation: &Relation,
    user: &User,
    config: &NamespaceConfig,
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
                if let User::Userset(obj, rel) = t.user {
                    // Note: a subtle but important detail. When checking a userset,
                    // we must start a fresh `check` call, not `eval_expression`,
                    // as the new object (`obj`) may have its own rewrite rules.
                    if check_internal(&obj, &rel, user, config, store, visited)? {
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
            check_internal(object, computed_relation, user, config, store, visited)
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
                        config,
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
                if eval_expression(object, relation, user, config, store, visited, expr)? {
                    return Ok(true);
                }
            }
            Ok(false)
        }
        UsersetExpression::Intersection(expressions) => {
            for expr in expressions {
                // If any sub-expression is false, the whole intersection is false.
                if !eval_expression(object, relation, user, config, store, visited, expr)? {
                    return Ok(false);
                }
            }
            // If we get here, all sub-expressions were true.
            Ok(true)
        }
        UsersetExpression::Exclusion { base, exclude } => {
            // If the user is in the `exclude` set, the result is false.
            let is_excluded =
                eval_expression(object, relation, user, config, store, visited, exclude)?;
            if is_excluded {
                return Ok(false);
            }
            // Otherwise, the result is determined by the `base` set.
            eval_expression(object, relation, user, config, store, visited, base)
        }
    }
}

/// Evaluates a `check` request.
pub fn check(
    object: &Object,
    relation: &Relation,
    user: &User,
    config: &NamespaceConfig,
    store: &(impl TupleStore + ?Sized),
    visited: &mut HashSet<(Object, Relation, User)>,
) -> Result<bool, ZanzibarError> {
    check_internal(object, relation, user, config, store, visited)
}

/// Evaluates an `expand` request.
pub fn expand(
    object: &Object,
    relation: &Relation,
    config: &NamespaceConfig,
    store: &(impl TupleStore + ?Sized),
) -> Result<ExpandedUserset, ZanzibarError> {
    let relation_config = config
        .relations
        .get(relation)
        .ok_or_else(|| ZanzibarError::RelationNotFound(relation.0.clone(), config.name.clone()))?;

    let rewrite = relation_config
        .userset_rewrite
        .as_ref()
        .unwrap_or(&UsersetExpression::This);

    expand_expression(object, relation, config, store, rewrite)
}

fn expand_expression(
    object: &Object,
    relation: &Relation,
    config: &NamespaceConfig,
    store: &(impl TupleStore + ?Sized),
    expression: &UsersetExpression,
) -> Result<ExpandedUserset, ZanzibarError> {
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
        } => expand(object, computed_relation, config, store),
        UsersetExpression::TupleToUserset {
            tupleset_relation,
            computed_userset_relation,
        } => {
            let tupleset = store.read_tuples(object, Some(tupleset_relation), None);
            let mut users = Vec::new();
            for t in tupleset {
                if let User::Userset(intermediate_obj, _) = t.user {
                    users.push(expand(
                        &intermediate_obj,
                        computed_userset_relation,
                        config,
                        store,
                    )?);
                }
            }
            Ok(ExpandedUserset::Union(users))
        }
        UsersetExpression::Union(expressions) => {
            let mut sub_expressions = Vec::new();
            for expr in expressions {
                sub_expressions.push(expand_expression(object, relation, config, store, expr)?);
            }
            Ok(ExpandedUserset::Union(sub_expressions))
        }
        UsersetExpression::Intersection(expressions) => {
            let mut sub_expressions = Vec::new();
            for expr in expressions {
                sub_expressions.push(expand_expression(object, relation, config, store, expr)?);
            }
            Ok(ExpandedUserset::Intersection(sub_expressions))
        }
        UsersetExpression::Exclusion { base, exclude } => {
            let base_expr = expand_expression(object, relation, config, store, base)?;
            let exclude_expr = expand_expression(object, relation, config, store, exclude)?;
            Ok(ExpandedUserset::Exclusion {
                base: Box::new(base_expr),
                exclude: Box::new(exclude_expr),
            })
        }
    }
}
