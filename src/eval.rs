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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        model::{NamespaceConfig, RelationConfig, RelationTuple},
        store::InMemoryTupleStore,
    };

    /// Recursively collect user IDs from an expanded userset (test helper).
    fn collect_user_ids(expanded: &ExpandedUserset, out: &mut Vec<String>) {
        match expanded {
            ExpandedUserset::User(id) => out.push(id.clone()),
            ExpandedUserset::Userset(_, _) => {}
            ExpandedUserset::Union(children) | ExpandedUserset::Intersection(children) => {
                for child in children {
                    collect_user_ids(child, out);
                }
            }
            ExpandedUserset::Exclusion { base, exclude: _ } => {
                collect_user_ids(base, out);
            }
        }
    }

    /// Helper: build a configs map from a list of namespace configs.
    fn configs_from(namespaces: Vec<NamespaceConfig>) -> HashMap<String, NamespaceConfig> {
        namespaces
            .into_iter()
            .map(|c| (c.name.clone(), c))
            .collect()
    }

    /// Helper: build a minimal store with given tuples.
    fn store_with(tuples: Vec<RelationTuple>) -> InMemoryTupleStore {
        let mut store = InMemoryTupleStore::default();
        for t in tuples {
            store.write_tuple(t).unwrap();
        }
        store
    }

    // -- Intersection tests --

    #[test]
    fn test_should_grant_intersection_when_all_match() {
        // viewer = intersection(this, computed_userset("member"))
        // Alice is both a direct viewer AND a member → should be true.
        let ns = NamespaceConfig::new("team")
            .with_relation(RelationConfig::new(Relation::new("member")))
            .with_relation(RelationConfig::new(Relation::new("viewer")).with_rewrite(
                UsersetExpression::Intersection(vec![
                    UsersetExpression::This,
                    UsersetExpression::ComputedUserset {
                        relation: Relation::new("member"),
                    },
                ]),
            ));
        let configs = configs_from(vec![ns]);
        let store = store_with(vec![
            RelationTuple::new(
                Object::new("team", "eng"),
                Relation::new("viewer"),
                User::user_id("alice"),
            ),
            RelationTuple::new(
                Object::new("team", "eng"),
                Relation::new("member"),
                User::user_id("alice"),
            ),
        ]);

        let result = check(
            &Object::new("team", "eng"),
            &Relation::new("viewer"),
            &User::user_id("alice"),
            &configs,
            &store,
            &mut HashSet::new(),
        )
        .unwrap();
        assert!(result);
    }

    #[test]
    fn test_should_deny_intersection_when_one_fails() {
        // viewer = intersection(this, computed_userset("member"))
        // Alice is a direct viewer but NOT a member → should be false.
        let ns = NamespaceConfig::new("team")
            .with_relation(RelationConfig::new(Relation::new("member")))
            .with_relation(RelationConfig::new(Relation::new("viewer")).with_rewrite(
                UsersetExpression::Intersection(vec![
                    UsersetExpression::This,
                    UsersetExpression::ComputedUserset {
                        relation: Relation::new("member"),
                    },
                ]),
            ));
        let configs = configs_from(vec![ns]);
        let store = store_with(vec![RelationTuple::new(
            Object::new("team", "eng"),
            Relation::new("viewer"),
            User::user_id("alice"),
        )]);

        let result = check(
            &Object::new("team", "eng"),
            &Relation::new("viewer"),
            &User::user_id("alice"),
            &configs,
            &store,
            &mut HashSet::new(),
        )
        .unwrap();
        assert!(!result);
    }

    #[test]
    fn test_should_deny_empty_intersection() {
        let ns = NamespaceConfig::new("test").with_relation(
            RelationConfig::new(Relation::new("viewer"))
                .with_rewrite(UsersetExpression::Intersection(vec![])),
        );
        let configs = configs_from(vec![ns]);
        let store = InMemoryTupleStore::default();

        let result = check(
            &Object::new("test", "1"),
            &Relation::new("viewer"),
            &User::user_id("alice"),
            &configs,
            &store,
            &mut HashSet::new(),
        )
        .unwrap();
        assert!(!result, "empty intersection must deny access");
    }

    // -- Exclusion tests --

    #[test]
    fn test_should_grant_exclusion_when_not_excluded() {
        // viewer = exclusion(base: this, exclude: computed_userset("banned"))
        // Alice is a direct viewer and NOT banned → true.
        let ns = NamespaceConfig::new("doc")
            .with_relation(RelationConfig::new(Relation::new("banned")))
            .with_relation(RelationConfig::new(Relation::new("viewer")).with_rewrite(
                UsersetExpression::Exclusion {
                    base: Box::new(UsersetExpression::This),
                    exclude: Box::new(UsersetExpression::ComputedUserset {
                        relation: Relation::new("banned"),
                    }),
                },
            ));
        let configs = configs_from(vec![ns]);
        let store = store_with(vec![RelationTuple::new(
            Object::new("doc", "1"),
            Relation::new("viewer"),
            User::user_id("alice"),
        )]);

        let result = check(
            &Object::new("doc", "1"),
            &Relation::new("viewer"),
            &User::user_id("alice"),
            &configs,
            &store,
            &mut HashSet::new(),
        )
        .unwrap();
        assert!(result);
    }

    #[test]
    fn test_should_deny_exclusion_when_excluded() {
        // Alice is a direct viewer but ALSO banned → false.
        let ns = NamespaceConfig::new("doc")
            .with_relation(RelationConfig::new(Relation::new("banned")))
            .with_relation(RelationConfig::new(Relation::new("viewer")).with_rewrite(
                UsersetExpression::Exclusion {
                    base: Box::new(UsersetExpression::This),
                    exclude: Box::new(UsersetExpression::ComputedUserset {
                        relation: Relation::new("banned"),
                    }),
                },
            ));
        let configs = configs_from(vec![ns]);
        let store = store_with(vec![
            RelationTuple::new(
                Object::new("doc", "1"),
                Relation::new("viewer"),
                User::user_id("alice"),
            ),
            RelationTuple::new(
                Object::new("doc", "1"),
                Relation::new("banned"),
                User::user_id("alice"),
            ),
        ]);

        let result = check(
            &Object::new("doc", "1"),
            &Relation::new("viewer"),
            &User::user_id("alice"),
            &configs,
            &store,
            &mut HashSet::new(),
        )
        .unwrap();
        assert!(!result);
    }

    #[test]
    fn test_should_deny_exclusion_when_not_in_base() {
        // Bob is NOT a direct viewer (not in base) → false regardless of exclusion.
        let ns = NamespaceConfig::new("doc")
            .with_relation(RelationConfig::new(Relation::new("banned")))
            .with_relation(RelationConfig::new(Relation::new("viewer")).with_rewrite(
                UsersetExpression::Exclusion {
                    base: Box::new(UsersetExpression::This),
                    exclude: Box::new(UsersetExpression::ComputedUserset {
                        relation: Relation::new("banned"),
                    }),
                },
            ));
        let configs = configs_from(vec![ns]);
        let store = InMemoryTupleStore::default();

        let result = check(
            &Object::new("doc", "1"),
            &Relation::new("viewer"),
            &User::user_id("bob"),
            &configs,
            &store,
            &mut HashSet::new(),
        )
        .unwrap();
        assert!(!result);
    }

    // -- Cycle detection --

    #[test]
    fn test_should_detect_cycle_and_deny() {
        // rel_a = computed_userset("rel_b"), rel_b = computed_userset("rel_a")
        // This creates an infinite loop. Cycle detection should break it and return false.
        let ns = NamespaceConfig::new("test")
            .with_relation(RelationConfig::new(Relation::new("rel_a")).with_rewrite(
                UsersetExpression::ComputedUserset {
                    relation: Relation::new("rel_b"),
                },
            ))
            .with_relation(RelationConfig::new(Relation::new("rel_b")).with_rewrite(
                UsersetExpression::ComputedUserset {
                    relation: Relation::new("rel_a"),
                },
            ));
        let configs = configs_from(vec![ns]);
        let store = InMemoryTupleStore::default();

        let result = check(
            &Object::new("test", "1"),
            &Relation::new("rel_a"),
            &User::user_id("alice"),
            &configs,
            &store,
            &mut HashSet::new(),
        )
        .unwrap();
        assert!(!result, "cyclic reference must not cause infinite loop");
    }

    // -- Error cases --

    #[test]
    fn test_should_error_on_unknown_namespace() {
        let configs = HashMap::new();
        let store = InMemoryTupleStore::default();

        let result = check(
            &Object::new("nonexistent", "1"),
            &Relation::new("viewer"),
            &User::user_id("alice"),
            &configs,
            &store,
            &mut HashSet::new(),
        );
        assert!(matches!(result, Err(ZanzibarError::NamespaceNotFound(_))));
    }

    #[test]
    fn test_should_error_on_unknown_relation() {
        let ns =
            NamespaceConfig::new("doc").with_relation(RelationConfig::new(Relation::new("owner")));
        let configs = configs_from(vec![ns]);
        let store = InMemoryTupleStore::default();

        let result = check(
            &Object::new("doc", "1"),
            &Relation::new("nonexistent"),
            &User::user_id("alice"),
            &configs,
            &store,
            &mut HashSet::new(),
        );
        assert!(matches!(result, Err(ZanzibarError::RelationNotFound(_, _))));
    }

    // -- Cross-namespace --

    #[test]
    fn test_should_resolve_cross_namespace_with_distinct_schemas() {
        // doc namespace: viewer = tuple_to_userset(parent, viewer)
        // folder namespace: viewer = this (direct only, no computed userset)
        // These have DIFFERENT relation schemas. The old code (single config) would fail here.
        let doc_ns = NamespaceConfig::new("doc")
            .with_relation(RelationConfig::new(Relation::new("parent")))
            .with_relation(RelationConfig::new(Relation::new("viewer")).with_rewrite(
                UsersetExpression::TupleToUserset {
                    tupleset_relation: Relation::new("parent"),
                    computed_userset_relation: Relation::new("viewer"),
                },
            ));
        let folder_ns = NamespaceConfig::new("folder")
            .with_relation(RelationConfig::new(Relation::new("viewer")));
        let configs = configs_from(vec![doc_ns, folder_ns]);

        let store = store_with(vec![
            // folder:A has viewer bob
            RelationTuple::new(
                Object::new("folder", "A"),
                Relation::new("viewer"),
                User::user_id("bob"),
            ),
            // doc:1 has parent folder:A#viewer
            RelationTuple::new(
                Object::new("doc", "1"),
                Relation::new("parent"),
                User::userset(Object::new("folder", "A"), Relation::new("viewer")),
            ),
        ]);

        // Bob can view doc:1 via folder:A inheritance.
        let result = check(
            &Object::new("doc", "1"),
            &Relation::new("viewer"),
            &User::user_id("bob"),
            &configs,
            &store,
            &mut HashSet::new(),
        )
        .unwrap();
        assert!(result);

        // Alice cannot view doc:1 — she's not a folder:A viewer.
        let result = check(
            &Object::new("doc", "1"),
            &Relation::new("viewer"),
            &User::user_id("alice"),
            &configs,
            &store,
            &mut HashSet::new(),
        )
        .unwrap();
        assert!(!result);
    }

    // -- Expand tests --

    #[test]
    fn test_should_expand_intersection() {
        let ns = NamespaceConfig::new("doc")
            .with_relation(RelationConfig::new(Relation::new("editor")))
            .with_relation(RelationConfig::new(Relation::new("viewer")).with_rewrite(
                UsersetExpression::Intersection(vec![
                    UsersetExpression::This,
                    UsersetExpression::ComputedUserset {
                        relation: Relation::new("editor"),
                    },
                ]),
            ));
        let configs = configs_from(vec![ns]);
        let store = store_with(vec![
            RelationTuple::new(
                Object::new("doc", "1"),
                Relation::new("viewer"),
                User::user_id("alice"),
            ),
            RelationTuple::new(
                Object::new("doc", "1"),
                Relation::new("editor"),
                User::user_id("bob"),
            ),
        ]);

        let expanded = expand(
            &Object::new("doc", "1"),
            &Relation::new("viewer"),
            &configs,
            &store,
        )
        .unwrap();
        assert!(matches!(expanded, ExpandedUserset::Intersection(_)));
    }

    #[test]
    fn test_should_expand_exclusion() {
        let ns = NamespaceConfig::new("doc")
            .with_relation(RelationConfig::new(Relation::new("banned")))
            .with_relation(RelationConfig::new(Relation::new("viewer")).with_rewrite(
                UsersetExpression::Exclusion {
                    base: Box::new(UsersetExpression::This),
                    exclude: Box::new(UsersetExpression::ComputedUserset {
                        relation: Relation::new("banned"),
                    }),
                },
            ));
        let configs = configs_from(vec![ns]);
        let store = store_with(vec![
            RelationTuple::new(
                Object::new("doc", "1"),
                Relation::new("viewer"),
                User::user_id("alice"),
            ),
            RelationTuple::new(
                Object::new("doc", "1"),
                Relation::new("banned"),
                User::user_id("bob"),
            ),
        ]);

        let expanded = expand(
            &Object::new("doc", "1"),
            &Relation::new("viewer"),
            &configs,
            &store,
        )
        .unwrap();
        assert!(matches!(expanded, ExpandedUserset::Exclusion { .. }));

        // Verify the base contains alice and the exclude contains bob.
        let mut base_users = Vec::new();
        let mut exclude_users = Vec::new();
        if let ExpandedUserset::Exclusion { base, exclude } = &expanded {
            collect_user_ids(base, &mut base_users);
            collect_user_ids(exclude, &mut exclude_users);
        }
        assert!(base_users.contains(&"alice".to_string()));
        assert!(exclude_users.contains(&"bob".to_string()));
    }

    #[test]
    fn test_should_expand_error_on_unknown_namespace() {
        let configs = HashMap::new();
        let store = InMemoryTupleStore::default();

        let result = expand(
            &Object::new("nonexistent", "1"),
            &Relation::new("viewer"),
            &configs,
            &store,
        );
        assert!(matches!(result, Err(ZanzibarError::NamespaceNotFound(_))));
    }
}
