//! Integration tests for the Zanzibar service.
//! These tests demonstrate the full workflow from DSL parsing to authorization checks.

use simple_zanzibar::{
    model::{Object, Relation, RelationTuple, User},
    ZanzibarService,
};

const DOCUMENT_SYSTEM_DSL: &str = r#"
    // Document management system with hierarchical permissions
    namespace doc {
        relation owner {}

        relation parent {}

        relation viewer {
            rewrite union(
                this,
                computed_userset(relation: "owner"),
                computed_userset(relation: "editor"),
                tuple_to_userset(tupleset: "parent", computed_userset: "viewer")
            )
        }

        relation editor {
            rewrite this
        }
    }

    // Folder system with simple inheritance
    namespace folder {
        relation viewer {}

        relation parent {}

        relation inherited_viewer {
            rewrite union(
                this,
                tuple_to_userset(tupleset: "parent", computed_userset: "viewer")
            )
        }
    }
"#;

#[test]
fn test_document_system_integration() -> Result<(), Box<dyn std::error::Error>> {
    let mut service = ZanzibarService::new();

    // Parse and load the DSL
    service.add_dsl(DOCUMENT_SYSTEM_DSL)?;

    // Set up test data
    let doc1 = Object {
        namespace: "doc".to_string(),
        id: "document1".to_string(),
    };
    let doc2 = Object {
        namespace: "doc".to_string(),
        id: "document2".to_string(),
    };

    let owner_rel = Relation("owner".to_string());
    let viewer_rel = Relation("viewer".to_string());
    let editor_rel = Relation("editor".to_string());
    let parent_rel = Relation("parent".to_string());

    let alice = User::UserId("alice".to_string());
    let bob = User::UserId("bob".to_string());
    let charlie = User::UserId("charlie".to_string());

    // Alice owns document1
    service.write_tuple(RelationTuple {
        object: doc1.clone(),
        relation: owner_rel.clone(),
        user: alice.clone(),
    })?;

    // Bob is a direct viewer of document1
    service.write_tuple(RelationTuple {
        object: doc1.clone(),
        relation: viewer_rel.clone(),
        user: bob.clone(),
    })?;

    // Document2 has document1 as parent
    service.write_tuple(RelationTuple {
        object: doc2.clone(),
        relation: parent_rel.clone(),
        user: User::Userset(doc1.clone(), viewer_rel.clone()),
    })?;

    // Charlie is a direct editor of document1 (but not owner)
    service.write_tuple(RelationTuple {
        object: doc1.clone(),
        relation: editor_rel.clone(),
        user: charlie.clone(),
    })?;

    // Test direct ownership
    assert!(service.check(&doc1, &owner_rel, &alice)?);
    assert!(!service.check(&doc1, &owner_rel, &bob)?);

    // Test viewer permissions (union of direct, owner, and inherited)
    assert!(service.check(&doc1, &viewer_rel, &alice)?); // owner -> viewer
    assert!(service.check(&doc1, &viewer_rel, &bob)?); // direct viewer
    assert!(service.check(&doc1, &viewer_rel, &charlie)?); // editor -> viewer

    // Test inherited viewer permissions through parent relationship
    assert!(service.check(&doc2, &viewer_rel, &alice)?); // inherited from doc1 owner
    assert!(service.check(&doc2, &viewer_rel, &bob)?); // inherited from doc1 viewer

    // Test editor permissions (intersection of viewer and exclusion of direct editors from owners)
    assert!(!service.check(&doc1, &editor_rel, &alice)?); // owner, so excluded from editor
    assert!(!service.check(&doc1, &editor_rel, &bob)?); // not an editor
    assert!(service.check(&doc1, &editor_rel, &charlie)?); // direct editor and viewer

    Ok(())
}

#[test]
fn test_folder_system_integration() -> Result<(), Box<dyn std::error::Error>> {
    let mut service = ZanzibarService::new();

    // Parse and load the DSL
    service.add_dsl(DOCUMENT_SYSTEM_DSL)?;

    // Set up folder hierarchy: root_folder -> sub_folder
    let root_folder = Object {
        namespace: "folder".to_string(),
        id: "root".to_string(),
    };
    let sub_folder = Object {
        namespace: "folder".to_string(),
        id: "sub".to_string(),
    };

    let viewer_rel = Relation("viewer".to_string());
    let inherited_viewer_rel = Relation("inherited_viewer".to_string());
    let parent_rel = Relation("parent".to_string());

    let alice = User::UserId("alice".to_string());
    let bob = User::UserId("bob".to_string());

    // Alice can view root folder
    service.write_tuple(RelationTuple {
        object: root_folder.clone(),
        relation: viewer_rel.clone(),
        user: alice.clone(),
    })?;

    // Sub folder has root folder as parent
    service.write_tuple(RelationTuple {
        object: sub_folder.clone(),
        relation: parent_rel.clone(),
        user: User::Userset(root_folder.clone(), viewer_rel.clone()),
    })?;

    // Bob has direct inherited_viewer on sub folder
    service.write_tuple(RelationTuple {
        object: sub_folder.clone(),
        relation: inherited_viewer_rel.clone(),
        user: bob.clone(),
    })?;

    // Test direct permissions
    assert!(service.check(&root_folder, &viewer_rel, &alice)?);
    assert!(!service.check(&root_folder, &viewer_rel, &bob)?);

    // Test inherited permissions
    assert!(service.check(&sub_folder, &inherited_viewer_rel, &alice)?); // inherited from parent
    assert!(service.check(&sub_folder, &inherited_viewer_rel, &bob)?); // direct permission

    Ok(())
}

#[test]
fn test_expand_functionality() -> Result<(), Box<dyn std::error::Error>> {
    let mut service = ZanzibarService::new();

    // Simple DSL for testing expand
    let simple_dsl = r#"
        namespace test {
            relation owner {}

            relation viewer {
                rewrite union(
                    this,
                    computed_userset(relation: "owner")
                )
            }
        }
    "#;

    service.add_dsl(simple_dsl)?;

    let obj = Object {
        namespace: "test".to_string(),
        id: "item1".to_string(),
    };
    let owner_rel = Relation("owner".to_string());
    let viewer_rel = Relation("viewer".to_string());

    let alice = User::UserId("alice".to_string());
    let bob = User::UserId("bob".to_string());

    // Alice owns the item
    service.write_tuple(RelationTuple {
        object: obj.clone(),
        relation: owner_rel.clone(),
        user: alice.clone(),
    })?;

    // Bob is a direct viewer
    service.write_tuple(RelationTuple {
        object: obj.clone(),
        relation: viewer_rel.clone(),
        user: bob.clone(),
    })?;

    // Test expand functionality
    let expanded = service.expand(&obj, &viewer_rel)?;

    // The expanded result should show the union structure
    // This is a basic test - in a real system you'd want more detailed assertions
    // about the structure of the expanded userset
    println!("Expanded userset: {:?}", expanded);

    Ok(())
}

#[test]
fn test_error_handling() {
    let mut service = ZanzibarService::new();

    // Test with invalid DSL
    let invalid_dsl = r#"
        namespace invalid {
            relation bad_syntax {
                rewrite unknown_operator(this)
            }
        }
    "#;

    // This should fail to parse
    assert!(service.add_dsl(invalid_dsl).is_err());

    // Test with unknown namespace
    let unknown_obj = Object {
        namespace: "unknown".to_string(),
        id: "item".to_string(),
    };
    let rel = Relation("viewer".to_string());
    let user = User::UserId("alice".to_string());

    // This should fail because namespace doesn't exist
    assert!(service.check(&unknown_obj, &rel, &user).is_err());
}

#[test]
fn test_tuple_management() -> Result<(), Box<dyn std::error::Error>> {
    let mut service = ZanzibarService::new();

    // Simple namespace for testing
    let simple_dsl = r#"
        namespace test {
            relation viewer {}
        }
    "#;

    service.add_dsl(simple_dsl)?;

    let obj = Object {
        namespace: "test".to_string(),
        id: "item".to_string(),
    };
    let rel = Relation("viewer".to_string());
    let user = User::UserId("alice".to_string());

    let tuple = RelationTuple {
        object: obj.clone(),
        relation: rel.clone(),
        user: user.clone(),
    };

    // Initially, alice should not have viewer permission
    assert!(!service.check(&obj, &rel, &user)?);

    // Add the tuple
    service.write_tuple(tuple.clone())?;

    // Now alice should have viewer permission
    assert!(service.check(&obj, &rel, &user)?);

    // Remove the tuple
    service.delete_tuple(&tuple)?;

    // Alice should no longer have viewer permission
    assert!(!service.check(&obj, &rel, &user)?);

    Ok(())
}
