//! Tests for the core evaluation logic.

use simple_zanzibar::ZanzibarService;
use simple_zanzibar::error::ZanzibarError;
use simple_zanzibar::eval::{EvaluationError, EvaluationLimits, Membership};
use simple_zanzibar::model::{
    ExpandedUserset, NamespaceConfig, Object, Relation, RelationConfig, RelationTuple, User,
    UsersetExpression,
};
use std::collections::HashMap;
use std::num::NonZeroU32;

/// Creates a `ZanzibarService` pre-populated with a common test configuration.
fn create_test_service() -> Result<ZanzibarService, ZanzibarError> {
    let mut service = ZanzibarService::new();

    let doc_namespace = NamespaceConfig {
        name: "doc".to_string(),
        relations: HashMap::from([
            (
                Relation("parent".to_string()),
                RelationConfig {
                    name: Relation("parent".to_string()),
                    userset_rewrite: None,
                },
            ),
            (
                Relation("owner".to_string()),
                RelationConfig {
                    name: Relation("owner".to_string()),
                    userset_rewrite: None,
                },
            ),
            (
                Relation("viewer".to_string()),
                RelationConfig {
                    name: Relation("viewer".to_string()),
                    userset_rewrite: Some(UsersetExpression::Union(vec![
                        // Users directly granted viewer access
                        UsersetExpression::This,
                        // Owners are also viewers
                        UsersetExpression::ComputedUserset {
                            relation: Relation("owner".to_string()),
                        },
                        // Users who can view the parent folder are also viewers
                        UsersetExpression::TupleToUserset {
                            tupleset_relation: Relation("parent".to_string()),
                            computed_userset_relation: Relation("viewer".to_string()),
                        },
                    ])),
                },
            ),
        ]),
    };
    service.add_config(doc_namespace)?;

    let folder_namespace = NamespaceConfig {
        name: "folder".to_string(),
        relations: HashMap::from([
            (
                Relation("owner".to_string()),
                RelationConfig {
                    name: Relation("owner".to_string()),
                    userset_rewrite: None,
                },
            ),
            (
                Relation("viewer".to_string()),
                RelationConfig {
                    name: Relation("viewer".to_string()),
                    userset_rewrite: Some(UsersetExpression::ComputedUserset {
                        relation: Relation("owner".to_string()),
                    }),
                },
            ),
        ]),
    };
    service.add_config(folder_namespace)?;

    Ok(service)
}

#[test]
fn test_check_direct_access() -> Result<(), ZanzibarError> {
    let mut service = create_test_service()?;
    let doc1 = Object {
        namespace: "doc".to_string(),
        id: "1".to_string(),
    };
    let alice = User::UserId("alice".to_string());
    let owner_rel = Relation("owner".to_string());

    service.write_tuple(RelationTuple {
        object: doc1.clone(),
        relation: owner_rel.clone(),
        user: alice.clone(),
    })?;

    assert!(service.check(&doc1, &owner_rel, &alice)?);
    Ok(())
}

#[test]
fn test_check_computed_userset() -> Result<(), ZanzibarError> {
    let mut service = create_test_service()?;
    let doc1 = Object {
        namespace: "doc".to_string(),
        id: "1".to_string(),
    };
    let alice = User::UserId("alice".to_string());
    let owner_rel = Relation("owner".to_string());
    let viewer_rel = Relation("viewer".to_string());

    service.write_tuple(RelationTuple {
        object: doc1.clone(),
        relation: owner_rel,
        user: alice.clone(),
    })?;

    // Alice is an owner, so she should also be a viewer via ComputedUserset.
    assert!(service.check(&doc1, &viewer_rel, &alice)?);
    Ok(())
}

#[test]
fn test_check_hierarchical_access() -> Result<(), ZanzibarError> {
    let mut service = create_test_service()?;

    let folder_a = Object {
        namespace: "folder".to_string(),
        id: "A".to_string(),
    };
    let doc1 = Object {
        namespace: "doc".to_string(),
        id: "1".to_string(),
    };
    let bob = User::UserId("bob".to_string());
    let viewer_rel = Relation("viewer".to_string());

    // Bob owns Folder A, and folder#viewer rewrites to folder#owner.
    service.write_tuple(RelationTuple {
        object: folder_a.clone(),
        relation: Relation("owner".to_string()),
        user: bob.clone(),
    })?;

    // Doc 1 is in Folder A
    service.write_tuple(RelationTuple {
        object: doc1.clone(),
        relation: Relation("parent".to_string()),
        // Note: The user here is a *userset* pointing to the folder object.
        user: User::Userset(folder_a, Relation("viewer".to_string())),
    })?;

    // Assert that Bob can view Doc 1 due to inheritance.
    assert!(service.check(&doc1, &viewer_rel, &bob)?);
    Ok(())
}

#[test]
fn test_check_cross_namespace_userset_uses_target_namespace_rewrite() -> Result<(), ZanzibarError> {
    let mut service = create_test_service()?;

    let folder_a = Object {
        namespace: "folder".to_string(),
        id: "A".to_string(),
    };
    let doc1 = Object {
        namespace: "doc".to_string(),
        id: "1".to_string(),
    };
    let bob = User::UserId("bob".to_string());
    let viewer_rel = Relation("viewer".to_string());

    service.write_tuple(RelationTuple {
        object: folder_a.clone(),
        relation: Relation("owner".to_string()),
        user: bob.clone(),
    })?;

    service.write_tuple(RelationTuple {
        object: doc1.clone(),
        relation: Relation("parent".to_string()),
        user: User::Userset(folder_a, viewer_rel.clone()),
    })?;

    assert!(service.check(&doc1, &viewer_rel, &bob)?);
    Ok(())
}

#[test]
fn test_should_evaluate_intersection_and_exclusion() -> Result<(), ZanzibarError> {
    let mut service = ZanzibarService::new();
    let owner = Relation("owner".to_string());
    let editor = Relation("editor".to_string());
    let banned = Relation("banned".to_string());
    let owner_editor = Relation("owner_editor".to_string());
    let allowed_owner = Relation("allowed_owner".to_string());
    service.add_config(NamespaceConfig {
        name: "doc".to_string(),
        relations: HashMap::from([
            plain_relation(owner.clone()),
            plain_relation(editor.clone()),
            plain_relation(banned.clone()),
            (
                owner_editor.clone(),
                RelationConfig {
                    name: owner_editor.clone(),
                    userset_rewrite: Some(UsersetExpression::Intersection(vec![
                        UsersetExpression::ComputedUserset {
                            relation: owner.clone(),
                        },
                        UsersetExpression::ComputedUserset {
                            relation: editor.clone(),
                        },
                    ])),
                },
            ),
            (
                allowed_owner.clone(),
                RelationConfig {
                    name: allowed_owner.clone(),
                    userset_rewrite: Some(UsersetExpression::Exclusion {
                        base: Box::new(UsersetExpression::ComputedUserset {
                            relation: owner.clone(),
                        }),
                        exclude: Box::new(UsersetExpression::ComputedUserset {
                            relation: banned.clone(),
                        }),
                    }),
                },
            ),
        ]),
    })?;

    let doc = Object {
        namespace: "doc".to_string(),
        id: "1".to_string(),
    };
    let alice = User::UserId("alice".to_string());
    let bob = User::UserId("bob".to_string());
    service.write_tuple(tuple(doc.clone(), owner.clone(), alice.clone()))?;
    service.write_tuple(tuple(doc.clone(), editor, alice.clone()))?;
    service.write_tuple(tuple(doc.clone(), owner, bob.clone()))?;
    service.write_tuple(tuple(doc.clone(), banned, bob.clone()))?;

    assert!(service.check(&doc, &owner_editor, &alice)?);
    assert!(!service.check(&doc, &owner_editor, &bob)?);
    assert!(service.check(&doc, &allowed_owner, &alice)?);
    assert!(!service.check(&doc, &allowed_owner, &bob)?);
    Ok(())
}

#[test]
fn test_should_return_depth_exceeded_distinct_from_denied() -> Result<(), ZanzibarError> {
    let mut service = ZanzibarService::new().with_evaluation_limits(EvaluationLimits {
        max_depth: NonZeroU32::MIN,
        max_fanout_per_step: non_zero(100),
        max_lookup_results: non_zero(100),
    });
    let parent = Relation("parent".to_string());
    let viewer = Relation("viewer".to_string());
    service.add_config(NamespaceConfig {
        name: "doc".to_string(),
        relations: HashMap::from([
            plain_relation(parent.clone()),
            (
                viewer.clone(),
                RelationConfig {
                    name: viewer.clone(),
                    userset_rewrite: Some(UsersetExpression::ComputedUserset {
                        relation: parent.clone(),
                    }),
                },
            ),
        ]),
    })?;
    let doc = Object {
        namespace: "doc".to_string(),
        id: "1".to_string(),
    };

    let result = service.check(&doc, &viewer, &User::UserId("alice".to_string()));

    assert!(matches!(
        result,
        Err(ZanzibarError::Evaluation(
            EvaluationError::DepthExceeded { .. }
        ))
    ));
    Ok(())
}

#[test]
fn test_should_return_fanout_exceeded() -> Result<(), ZanzibarError> {
    let mut service = ZanzibarService::new().with_evaluation_limits(EvaluationLimits {
        max_depth: non_zero(50),
        max_fanout_per_step: NonZeroU32::MIN,
        max_lookup_results: non_zero(100),
    });
    let viewer = Relation("viewer".to_string());
    let member = Relation("member".to_string());
    service.add_config(NamespaceConfig {
        name: "doc".to_string(),
        relations: HashMap::from([plain_relation(viewer.clone())]),
    })?;
    service.add_config(NamespaceConfig {
        name: "group".to_string(),
        relations: HashMap::from([plain_relation(member.clone())]),
    })?;
    let doc = Object {
        namespace: "doc".to_string(),
        id: "1".to_string(),
    };
    for group_id in ["eng", "ops"] {
        service.write_tuple(tuple(
            doc.clone(),
            viewer.clone(),
            User::Userset(
                Object {
                    namespace: "group".to_string(),
                    id: group_id.to_string(),
                },
                member.clone(),
            ),
        ))?;
    }

    let result = service.check(&doc, &viewer, &User::UserId("alice".to_string()));

    assert!(matches!(
        result,
        Err(ZanzibarError::Evaluation(
            EvaluationError::FanoutExceeded { .. }
        ))
    ));
    Ok(())
}

#[test]
fn test_should_return_fanout_exceeded_after_limit_plus_one_relationships()
-> Result<(), ZanzibarError> {
    let mut service = ZanzibarService::new().with_evaluation_limits(EvaluationLimits {
        max_depth: non_zero(50),
        max_fanout_per_step: non_zero(1_000),
        max_lookup_results: non_zero(100),
    });
    let viewer = Relation("viewer".to_string());
    let member = Relation("member".to_string());
    let doc = Object {
        namespace: "doc".to_string(),
        id: "1".to_string(),
    };
    for index in 0..1_001 {
        service.write_tuple(tuple(
            doc.clone(),
            viewer.clone(),
            User::Userset(
                Object {
                    namespace: "group".to_string(),
                    id: format!("group-{index}"),
                },
                member.clone(),
            ),
        ))?;
    }
    service.add_dsl(
        r"
        namespace doc {
            relation viewer {}
        }

        namespace group {
            relation member {}
        }
        ",
    )?;

    let result = service.check(&doc, &viewer, &User::UserId("alice".to_string()));

    assert!(matches!(
        result,
        Err(ZanzibarError::Evaluation(
            EvaluationError::FanoutExceeded { .. }
        ))
    ));
    Ok(())
}

#[test]
fn test_should_not_spend_indirect_fanout_on_direct_grants() -> Result<(), ZanzibarError> {
    let mut service = ZanzibarService::new().with_evaluation_limits(EvaluationLimits {
        max_depth: non_zero(50),
        max_fanout_per_step: NonZeroU32::MIN,
        max_lookup_results: non_zero(100),
    });
    let viewer = Relation("viewer".to_string());
    let member = Relation("member".to_string());
    let doc = Object {
        namespace: "doc".to_string(),
        id: "1".to_string(),
    };
    service.add_dsl(
        r"
        namespace doc {
            relation viewer {}
        }

        namespace group {
            relation member {}
        }
        ",
    )?;
    for index in 0..100 {
        service.write_tuple(tuple(
            doc.clone(),
            viewer.clone(),
            User::UserId(format!("direct-{index}")),
        ))?;
    }
    let group = Object {
        namespace: "group".to_string(),
        id: "eng".to_string(),
    };
    service.write_tuple(tuple(
        doc.clone(),
        viewer.clone(),
        User::Userset(group.clone(), member.clone()),
    ))?;
    service.write_tuple(tuple(group, member, User::UserId("alice".to_string())))?;

    assert!(service.check(&doc, &viewer, &User::UserId("alice".to_string()))?);
    Ok(())
}

#[test]
fn test_should_deny_recursion_cycle_without_depth_error() -> Result<(), ZanzibarError> {
    let mut service = ZanzibarService::new();
    let viewer = Relation("viewer".to_string());
    service.add_config(NamespaceConfig {
        name: "doc".to_string(),
        relations: HashMap::from([(
            viewer.clone(),
            RelationConfig {
                name: viewer.clone(),
                userset_rewrite: Some(UsersetExpression::ComputedUserset {
                    relation: viewer.clone(),
                }),
            },
        )]),
    })?;
    let doc = Object {
        namespace: "doc".to_string(),
        id: "1".to_string(),
    };

    assert!(!service.check(&doc, &viewer, &User::UserId("alice".to_string()))?);
    Ok(())
}

#[test]
fn test_should_bound_expand_recursion_depth() -> Result<(), ZanzibarError> {
    let mut service = ZanzibarService::new().with_evaluation_limits(EvaluationLimits {
        max_depth: NonZeroU32::MIN,
        max_fanout_per_step: non_zero(100),
        max_lookup_results: non_zero(100),
    });
    let parent = Relation("parent".to_string());
    let viewer = Relation("viewer".to_string());
    service.add_config(NamespaceConfig {
        name: "doc".to_string(),
        relations: HashMap::from([
            plain_relation(parent.clone()),
            (
                viewer.clone(),
                RelationConfig {
                    name: viewer.clone(),
                    userset_rewrite: Some(UsersetExpression::ComputedUserset { relation: parent }),
                },
            ),
        ]),
    })?;

    let result = service.expand(
        &Object {
            namespace: "doc".to_string(),
            id: "1".to_string(),
        },
        &viewer,
    );

    assert!(matches!(
        result,
        Err(ZanzibarError::Evaluation(
            EvaluationError::DepthExceeded { .. }
        ))
    ));
    Ok(())
}

#[test]
fn test_should_bound_expand_cycle_without_depth_error() -> Result<(), ZanzibarError> {
    let mut service = ZanzibarService::new().with_evaluation_limits(EvaluationLimits {
        max_depth: NonZeroU32::MIN,
        max_fanout_per_step: non_zero(100),
        max_lookup_results: non_zero(100),
    });
    let viewer = Relation("viewer".to_string());
    service.add_config(NamespaceConfig {
        name: "doc".to_string(),
        relations: HashMap::from([(
            viewer.clone(),
            RelationConfig {
                name: viewer.clone(),
                userset_rewrite: Some(UsersetExpression::ComputedUserset {
                    relation: viewer.clone(),
                }),
            },
        )]),
    })?;

    let expanded = service.expand(
        &Object {
            namespace: "doc".to_string(),
            id: "1".to_string(),
        },
        &viewer,
    )?;

    assert_eq!(expanded, ExpandedUserset::Union(Vec::new()));
    Ok(())
}

#[test]
fn test_should_expand_from_snapshot_path() -> Result<(), ZanzibarError> {
    let mut service = create_test_service()?;
    let doc = Object {
        namespace: "doc".to_string(),
        id: "1".to_string(),
    };
    let owner = Relation("owner".to_string());
    service.write_tuple(tuple(
        doc.clone(),
        owner.clone(),
        User::UserId("alice".to_string()),
    ))?;

    let expanded = service.expand(&doc, &owner)?;

    assert_eq!(
        expanded,
        ExpandedUserset::Union(vec![ExpandedUserset::User("alice".to_string())])
    );
    Ok(())
}

#[test]
fn test_should_not_spend_expand_tuple_to_userset_fanout_on_direct_grants()
-> Result<(), ZanzibarError> {
    let mut service = ZanzibarService::new().with_evaluation_limits(EvaluationLimits {
        max_depth: non_zero(50),
        max_fanout_per_step: NonZeroU32::MIN,
        max_lookup_results: non_zero(100),
    });
    let parent = Relation("parent".to_string());
    let viewer = Relation("viewer".to_string());
    let inherited_viewer = Relation("inherited_viewer".to_string());
    service.add_dsl(
        r#"
        namespace doc {
            relation parent {}
            relation inherited_viewer {
                rewrite tuple_to_userset(tupleset: "parent", computed_userset: "viewer")
            }
        }

        namespace folder {
            relation viewer {}
        }
        "#,
    )?;

    let doc = Object {
        namespace: "doc".to_string(),
        id: "1".to_string(),
    };
    for index in 0..100 {
        service.write_tuple(tuple(
            doc.clone(),
            parent.clone(),
            User::UserId(format!("direct-{index}")),
        ))?;
    }
    let folder = Object {
        namespace: "folder".to_string(),
        id: "root".to_string(),
    };
    service.write_tuple(tuple(
        doc.clone(),
        parent,
        User::Userset(folder.clone(), viewer.clone()),
    ))?;
    service.write_tuple(tuple(folder, viewer, User::UserId("alice".to_string())))?;

    let expanded = service.expand(&doc, &inherited_viewer)?;

    assert_eq!(
        expanded,
        ExpandedUserset::Union(vec![ExpandedUserset::Union(vec![ExpandedUserset::User(
            "alice".to_string()
        )])])
    );
    Ok(())
}

#[test]
fn test_should_apply_membership_algebra_to_conditional_shape() {
    assert_eq!(
        Membership::Denied.union(Membership::Conditional),
        Membership::Conditional
    );
    assert_eq!(
        Membership::Allowed.intersection(Membership::Conditional),
        Membership::Conditional
    );
    assert_eq!(
        Membership::Conditional.exclusion(Membership::Denied),
        Membership::Conditional
    );
    assert_eq!(
        Membership::Allowed.exclusion(Membership::Allowed),
        Membership::Denied
    );
}

fn plain_relation(relation: Relation) -> (Relation, RelationConfig) {
    (
        relation.clone(),
        RelationConfig {
            name: relation,
            userset_rewrite: None,
        },
    )
}

fn tuple(object: Object, relation: Relation, user: User) -> RelationTuple {
    RelationTuple {
        object,
        relation,
        user,
    }
}

fn non_zero(value: u32) -> NonZeroU32 {
    NonZeroU32::new(value).unwrap_or(NonZeroU32::MIN)
}
