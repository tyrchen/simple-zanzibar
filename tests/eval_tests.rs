//! Tests for the core evaluation logic.

use simple_zanzibar::{
    ZanzibarService,
    error::ZanzibarError,
    model::{
        NamespaceConfig, Object, Relation, RelationConfig, RelationTuple, User, UsersetExpression,
    },
};

/// Creates a `ZanzibarService` pre-populated with a common test configuration.
fn create_test_service() -> ZanzibarService {
    let mut service = ZanzibarService::new();

    let doc_namespace = NamespaceConfig::new("doc")
        .with_relation(RelationConfig::new(Relation::new("parent")))
        .with_relation(RelationConfig::new(Relation::new("owner")))
        .with_relation(RelationConfig::new(Relation::new("viewer")).with_rewrite(
            UsersetExpression::Union(vec![
                UsersetExpression::This,
                UsersetExpression::ComputedUserset {
                    relation: Relation::new("owner"),
                },
                UsersetExpression::TupleToUserset {
                    tupleset_relation: Relation::new("parent"),
                    computed_userset_relation: Relation::new("viewer"),
                },
            ]),
        ));
    service.add_config(doc_namespace);

    let folder_namespace =
        NamespaceConfig::new("folder").with_relation(RelationConfig::new(Relation::new("viewer")));
    service.add_config(folder_namespace);

    service
}

#[test]
fn test_should_check_direct_access() -> Result<(), ZanzibarError> {
    let mut service = create_test_service();
    let doc1 = Object::new("doc", "1");
    let alice = User::user_id("alice");
    let owner_rel = Relation::new("owner");

    service.write_tuple(RelationTuple::new(
        doc1.clone(),
        owner_rel.clone(),
        alice.clone(),
    ))?;

    assert!(service.check(&doc1, &owner_rel, &alice)?);
    Ok(())
}

#[test]
fn test_should_check_computed_userset() -> Result<(), ZanzibarError> {
    let mut service = create_test_service();
    let doc1 = Object::new("doc", "1");
    let alice = User::user_id("alice");
    let owner_rel = Relation::new("owner");
    let viewer_rel = Relation::new("viewer");

    service.write_tuple(RelationTuple::new(doc1.clone(), owner_rel, alice.clone()))?;

    // Alice is an owner, so she should also be a viewer via ComputedUserset.
    assert!(service.check(&doc1, &viewer_rel, &alice)?);
    Ok(())
}

#[test]
fn test_should_check_hierarchical_access() -> Result<(), ZanzibarError> {
    let mut service = create_test_service();

    let folder_a = Object::new("folder", "A");
    let doc1 = Object::new("doc", "1");
    let bob = User::user_id("bob");
    let viewer_rel = Relation::new("viewer");

    // Bob can view Folder A
    service.write_tuple(RelationTuple::new(
        folder_a.clone(),
        viewer_rel.clone(),
        bob.clone(),
    ))?;

    // Doc 1 is in Folder A
    service.write_tuple(RelationTuple::new(
        doc1.clone(),
        Relation::new("parent"),
        User::userset(folder_a, Relation::new("viewer")),
    ))?;

    // Assert that Bob can view Doc 1 due to inheritance.
    assert!(service.check(&doc1, &viewer_rel, &bob)?);
    Ok(())
}
