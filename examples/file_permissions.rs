//! Example: File Permissions System using Simplified Zanzibar
//!
//! This example demonstrates how to use the simplified Zanzibar authorization system
//! to implement a file permissions system with hierarchical access control.

use simple_zanzibar::{
    model::{Object, Relation, RelationTuple, User},
    ZanzibarService,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("🔐 Simplified Zanzibar File Permissions Example");
    println!("================================================\n");

    // Initialize the Zanzibar service
    let mut service = ZanzibarService::new();

    // Define the policy using DSL
    let policy_dsl = r#"
        // File system with hierarchical permissions
        namespace file {
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
                rewrite union(
                    this,
                    computed_userset(relation: "owner")
                )
            }
        }

        // Folder system with inheritance
        namespace folder {
            relation owner {}

            relation parent {}

            relation viewer {
                rewrite union(
                    this,
                    computed_userset(relation: "owner"),
                    tuple_to_userset(tupleset: "parent", computed_userset: "viewer")
                )
            }
        }
    "#;

    // Load the policy
    println!("📋 Loading authorization policy...");
    service.add_dsl(policy_dsl)?;
    println!("✅ Policy loaded successfully\n");

    // Create objects
    let root_folder = Object {
        namespace: "folder".to_string(),
        id: "root".to_string(),
    };
    let docs_folder = Object {
        namespace: "folder".to_string(),
        id: "docs".to_string(),
    };
    let readme_file = Object {
        namespace: "file".to_string(),
        id: "readme.md".to_string(),
    };
    let secret_file = Object {
        namespace: "file".to_string(),
        id: "secret.txt".to_string(),
    };

    // Create relations
    let owner_rel = Relation("owner".to_string());
    let viewer_rel = Relation("viewer".to_string());
    let editor_rel = Relation("editor".to_string());
    let parent_rel = Relation("parent".to_string());

    // Create users
    let alice = User::UserId("alice".to_string());
    let bob = User::UserId("bob".to_string());
    let charlie = User::UserId("charlie".to_string());

    println!("👥 Setting up users and permissions...");

    // Alice owns the root folder
    service.write_tuple(RelationTuple {
        object: root_folder.clone(),
        relation: owner_rel.clone(),
        user: alice.clone(),
    })?;
    println!("   Alice owns root folder");

    // Docs folder is a child of root folder
    service.write_tuple(RelationTuple {
        object: docs_folder.clone(),
        relation: parent_rel.clone(),
        user: User::Userset(root_folder.clone(), viewer_rel.clone()),
    })?;
    println!("   Docs folder inherits from root folder");

    // README file is in docs folder
    service.write_tuple(RelationTuple {
        object: readme_file.clone(),
        relation: parent_rel.clone(),
        user: User::Userset(docs_folder.clone(), viewer_rel.clone()),
    })?;
    println!("   README file is in docs folder");

    // Bob can edit the README file
    service.write_tuple(RelationTuple {
        object: readme_file.clone(),
        relation: editor_rel.clone(),
        user: bob.clone(),
    })?;
    println!("   Bob can edit README file");

    // Charlie can view the secret file directly
    service.write_tuple(RelationTuple {
        object: secret_file.clone(),
        relation: viewer_rel.clone(),
        user: charlie.clone(),
    })?;
    println!("   Charlie can view secret file");

    println!("\n🔍 Testing authorization checks...\n");

    // Test Alice's permissions (owner of root, should inherit everywhere)
    println!("Alice's permissions:");
    println!(
        "  Can view root folder: {}",
        service.check(&root_folder, &viewer_rel, &alice)?
    );
    println!(
        "  Can view docs folder: {}",
        service.check(&docs_folder, &viewer_rel, &alice)?
    );
    println!(
        "  Can view README file: {}",
        service.check(&readme_file, &viewer_rel, &alice)?
    );
    println!(
        "  Can edit README file: {}",
        service.check(&readme_file, &editor_rel, &alice)?
    );
    println!(
        "  Can view secret file: {}",
        service.check(&secret_file, &viewer_rel, &alice)?
    );

    println!("\nBob's permissions:");
    println!(
        "  Can view root folder: {}",
        service.check(&root_folder, &viewer_rel, &bob)?
    );
    println!(
        "  Can view README file: {}",
        service.check(&readme_file, &viewer_rel, &bob)?
    );
    println!(
        "  Can edit README file: {}",
        service.check(&readme_file, &editor_rel, &bob)?
    );
    println!(
        "  Can view secret file: {}",
        service.check(&secret_file, &viewer_rel, &bob)?
    );

    println!("\nCharlie's permissions:");
    println!(
        "  Can view root folder: {}",
        service.check(&root_folder, &viewer_rel, &charlie)?
    );
    println!(
        "  Can view README file: {}",
        service.check(&readme_file, &viewer_rel, &charlie)?
    );
    println!(
        "  Can view secret file: {}",
        service.check(&secret_file, &viewer_rel, &charlie)?
    );

    println!("\n🔄 Testing dynamic permission changes...\n");

    // Grant Bob viewer access to root folder
    service.write_tuple(RelationTuple {
        object: root_folder.clone(),
        relation: viewer_rel.clone(),
        user: bob.clone(),
    })?;
    println!("✅ Granted Bob viewer access to root folder");

    println!("Bob's updated permissions:");
    println!(
        "  Can view root folder: {}",
        service.check(&root_folder, &viewer_rel, &bob)?
    );
    println!(
        "  Can view docs folder: {}",
        service.check(&docs_folder, &viewer_rel, &bob)?
    );

    // Revoke Bob's editor access to README
    service.delete_tuple(&RelationTuple {
        object: readme_file.clone(),
        relation: editor_rel.clone(),
        user: bob.clone(),
    })?;
    println!("\n❌ Revoked Bob's editor access to README file");

    println!("Bob's permissions after revocation:");
    println!(
        "  Can view README file: {}",
        service.check(&readme_file, &viewer_rel, &bob)?
    );
    println!(
        "  Can edit README file: {}",
        service.check(&readme_file, &editor_rel, &bob)?
    );

    println!("\n🌳 Testing expand functionality...\n");

    // Expand the viewer userset for README file
    let expanded = service.expand(&readme_file, &viewer_rel)?;
    println!("Users who can view README file: {:?}", expanded);

    println!("\n✨ Example completed successfully!");
    Ok(())
}
