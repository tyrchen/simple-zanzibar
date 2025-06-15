//! Tests for the core evaluation logic.

use simple_zanzibar::error::ZanzibarError;
use simple_zanzibar::model::{
    NamespaceConfig, Object, Relation, RelationConfig, RelationTuple, User, UsersetExpression,
};
use simple_zanzibar::ZanzibarService;
use std::collections::HashMap;

/// Creates a ZanzibarService pre-populated with a common test configuration.
fn create_test_service() -> ZanzibarService {
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
    service.add_config(doc_namespace);

    let folder_namespace = NamespaceConfig {
        name: "folder".to_string(),
        relations: HashMap::from([(
            Relation("viewer".to_string()),
            RelationConfig {
                name: Relation("viewer".to_string()),
                userset_rewrite: None,
            },
        )]),
    };
    service.add_config(folder_namespace);

    service
}

#[test]
fn test_check_direct_access() -> Result<(), ZanzibarError> {
    let mut service = create_test_service();
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
    let mut service = create_test_service();
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
    let mut service = create_test_service();

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

    // Bob can view Folder A
    service.write_tuple(RelationTuple {
        object: folder_a.clone(),
        relation: viewer_rel.clone(),
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
