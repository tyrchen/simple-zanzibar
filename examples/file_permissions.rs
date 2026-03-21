//! Example: File Permissions System using Simplified Zanzibar
//!
//! This example demonstrates how to use the simplified Zanzibar authorization system
//! to implement a file permissions system with hierarchical access control.

use simple_zanzibar::{
    ZanzibarService,
    model::{Object, Relation, RelationTuple, User},
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("Simplified Zanzibar File Permissions Example");
    println!("=============================================\n");

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

    println!("Loading authorization policy...");
    service.add_dsl(policy_dsl)?;
    println!("Policy loaded successfully\n");

    // Create objects
    let root_folder = Object::new("folder", "root");
    let docs_folder = Object::new("folder", "docs");
    let readme_file = Object::new("file", "readme.md");
    let secret_file = Object::new("file", "secret.txt");

    // Create relations
    let owner_rel = Relation::new("owner");
    let viewer_rel = Relation::new("viewer");
    let editor_rel = Relation::new("editor");
    let parent_rel = Relation::new("parent");

    // Create users
    let alice = User::user_id("alice");
    let bob = User::user_id("bob");
    let charlie = User::user_id("charlie");

    println!("Setting up users and permissions...");

    // Alice owns the root folder
    service.write_tuple(RelationTuple::new(
        root_folder.clone(),
        owner_rel.clone(),
        alice.clone(),
    ))?;
    println!("  Alice owns root folder");

    // Docs folder is a child of root folder
    service.write_tuple(RelationTuple::new(
        docs_folder.clone(),
        parent_rel.clone(),
        User::userset(root_folder.clone(), viewer_rel.clone()),
    ))?;
    println!("  Docs folder inherits from root folder");

    // README file is in docs folder
    service.write_tuple(RelationTuple::new(
        readme_file.clone(),
        parent_rel.clone(),
        User::userset(docs_folder.clone(), viewer_rel.clone()),
    ))?;
    println!("  README file is in docs folder");

    // Bob can edit the README file
    service.write_tuple(RelationTuple::new(
        readme_file.clone(),
        editor_rel.clone(),
        bob.clone(),
    ))?;
    println!("  Bob can edit README file");

    // Charlie can view the secret file directly
    service.write_tuple(RelationTuple::new(
        secret_file.clone(),
        viewer_rel.clone(),
        charlie.clone(),
    ))?;
    println!("  Charlie can view secret file");

    println!("\nTesting authorization checks...\n");

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

    println!("\nTesting dynamic permission changes...\n");

    // Grant Bob viewer access to root folder
    service.write_tuple(RelationTuple::new(
        root_folder.clone(),
        viewer_rel.clone(),
        bob.clone(),
    ))?;
    println!("Granted Bob viewer access to root folder");

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
    service.delete_tuple(&RelationTuple::new(
        readme_file.clone(),
        editor_rel.clone(),
        bob.clone(),
    ))?;
    println!("\nRevoked Bob's editor access to README file");

    println!("Bob's permissions after revocation:");
    println!(
        "  Can view README file: {}",
        service.check(&readme_file, &viewer_rel, &bob)?
    );
    println!(
        "  Can edit README file: {}",
        service.check(&readme_file, &editor_rel, &bob)?
    );

    println!("\nTesting expand functionality...\n");

    let expanded = service.expand(&readme_file, &viewer_rel)?;
    println!("Users who can view README file: {expanded:?}");

    println!("\nExample completed successfully!");
    Ok(())
}
