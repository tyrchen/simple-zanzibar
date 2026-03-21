//! Integration tests for the Zanzibar service.
//! These tests demonstrate the full workflow from DSL parsing to authorization checks.

use simple_zanzibar::{
    ZanzibarService,
    model::{Object, Relation, RelationTuple, User},
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
fn test_should_verify_document_system() -> Result<(), Box<dyn std::error::Error>> {
    let mut service = ZanzibarService::new();
    service.add_dsl(DOCUMENT_SYSTEM_DSL)?;

    let doc1 = Object::new("doc", "document1");
    let doc2 = Object::new("doc", "document2");

    let owner_rel = Relation::new("owner");
    let viewer_rel = Relation::new("viewer");
    let editor_rel = Relation::new("editor");
    let parent_rel = Relation::new("parent");

    let alice = User::user_id("alice");
    let bob = User::user_id("bob");
    let charlie = User::user_id("charlie");

    // Alice owns document1
    service.write_tuple(RelationTuple::new(
        doc1.clone(),
        owner_rel.clone(),
        alice.clone(),
    ))?;

    // Bob is a direct viewer of document1
    service.write_tuple(RelationTuple::new(
        doc1.clone(),
        viewer_rel.clone(),
        bob.clone(),
    ))?;

    // Document2 has document1 as parent
    service.write_tuple(RelationTuple::new(
        doc2.clone(),
        parent_rel,
        User::userset(doc1.clone(), viewer_rel.clone()),
    ))?;

    // Charlie is a direct editor of document1
    service.write_tuple(RelationTuple::new(
        doc1.clone(),
        editor_rel.clone(),
        charlie.clone(),
    ))?;

    // Test direct ownership
    assert!(service.check(&doc1, &owner_rel, &alice)?);
    assert!(!service.check(&doc1, &owner_rel, &bob)?);

    // Test viewer permissions (union of direct, owner, and editor)
    assert!(service.check(&doc1, &viewer_rel, &alice)?); // owner -> viewer
    assert!(service.check(&doc1, &viewer_rel, &bob)?); // direct viewer
    assert!(service.check(&doc1, &viewer_rel, &charlie)?); // editor -> viewer

    // Test inherited viewer permissions through parent relationship
    assert!(service.check(&doc2, &viewer_rel, &alice)?); // inherited from doc1 owner
    assert!(service.check(&doc2, &viewer_rel, &bob)?); // inherited from doc1 viewer

    // Test editor permissions (direct only, no rewrite)
    assert!(!service.check(&doc1, &editor_rel, &alice)?); // owner but not editor
    assert!(!service.check(&doc1, &editor_rel, &bob)?); // viewer but not editor
    assert!(service.check(&doc1, &editor_rel, &charlie)?); // direct editor

    Ok(())
}

#[test]
fn test_should_verify_folder_system() -> Result<(), Box<dyn std::error::Error>> {
    let mut service = ZanzibarService::new();
    service.add_dsl(DOCUMENT_SYSTEM_DSL)?;

    let root_folder = Object::new("folder", "root");
    let sub_folder = Object::new("folder", "sub");

    let viewer_rel = Relation::new("viewer");
    let inherited_viewer_rel = Relation::new("inherited_viewer");
    let parent_rel = Relation::new("parent");

    let alice = User::user_id("alice");
    let bob = User::user_id("bob");

    // Alice can view root folder
    service.write_tuple(RelationTuple::new(
        root_folder.clone(),
        viewer_rel.clone(),
        alice.clone(),
    ))?;

    // Sub folder has root folder as parent
    service.write_tuple(RelationTuple::new(
        sub_folder.clone(),
        parent_rel,
        User::userset(root_folder.clone(), viewer_rel.clone()),
    ))?;

    // Bob has direct inherited_viewer on sub folder
    service.write_tuple(RelationTuple::new(
        sub_folder.clone(),
        inherited_viewer_rel.clone(),
        bob.clone(),
    ))?;

    // Test direct permissions
    assert!(service.check(&root_folder, &viewer_rel, &alice)?);
    assert!(!service.check(&root_folder, &viewer_rel, &bob)?);

    // Test inherited permissions
    assert!(service.check(&sub_folder, &inherited_viewer_rel, &alice)?); // inherited from parent
    assert!(service.check(&sub_folder, &inherited_viewer_rel, &bob)?); // direct permission

    Ok(())
}

#[test]
fn test_should_expand_userset() -> Result<(), Box<dyn std::error::Error>> {
    let mut service = ZanzibarService::new();

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

    let obj = Object::new("test", "item1");
    let owner_rel = Relation::new("owner");
    let viewer_rel = Relation::new("viewer");

    let alice = User::user_id("alice");
    let bob = User::user_id("bob");

    service.write_tuple(RelationTuple::new(obj.clone(), owner_rel, alice.clone()))?;

    service.write_tuple(RelationTuple::new(obj.clone(), viewer_rel.clone(), bob))?;

    // The expanded result should show the union structure
    let expanded = service.expand(&obj, &viewer_rel)?;
    assert!(format!("{expanded:?}").contains("alice"));
    assert!(format!("{expanded:?}").contains("bob"));

    Ok(())
}

#[test]
fn test_should_handle_errors() {
    let mut service = ZanzibarService::new();

    // Test with invalid DSL
    let invalid_dsl = r#"
        namespace invalid {
            relation bad_syntax {
                rewrite unknown_operator(this)
            }
        }
    "#;
    assert!(service.add_dsl(invalid_dsl).is_err());

    // Test with unknown namespace
    let unknown_obj = Object::new("unknown", "item");
    let rel = Relation::new("viewer");
    let user = User::user_id("alice");
    assert!(service.check(&unknown_obj, &rel, &user).is_err());
}

#[test]
fn test_should_manage_tuples() -> Result<(), Box<dyn std::error::Error>> {
    let mut service = ZanzibarService::new();

    let simple_dsl = r#"
        namespace test {
            relation viewer {}
        }
    "#;

    service.add_dsl(simple_dsl)?;

    let obj = Object::new("test", "item");
    let rel = Relation::new("viewer");
    let user = User::user_id("alice");

    let tuple = RelationTuple::new(obj.clone(), rel.clone(), user.clone());

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
