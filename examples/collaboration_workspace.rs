//! Example: collaborative workspace authorization with import/export.
//!
//! The scenario models a small workspace where users inherit access from a workspace into a project
//! and then into documents. It also demonstrates guarded updates, permission lookup APIs,
//! reviewable policy export/import, and zstd-compressed snapshot save/load.

use std::{
    error::Error,
    fs, io,
    path::{Path, PathBuf},
    process,
};

use simple_zanzibar::{
    SnapshotLoadOptions, SnapshotSaveOptions, ZanzibarEngine,
    model::{
        CheckRequest, LookupObjectPermissionsRequest, LookupPermissionsRequest,
        LookupResourcesRequest, LookupSubjectsRequest, Object, Relation, User,
    },
    relationship::RelationshipMutation,
    revision::{Consistency, ConsistencyToken},
    schema::SchemaSource,
};

const SCHEMA: &str = r#"
namespace workspace {
    relation owner {}
    relation member {}
    relation auditor {}

    relation admin {
        rewrite computed_userset(relation: "owner")
    }

    relation viewer {
        rewrite union(
            computed_userset(relation: "owner"),
            computed_userset(relation: "member"),
            computed_userset(relation: "auditor")
        )
    }
}

namespace project {
    relation parent {}
    relation owner {}
    relation editor {}
    relation viewer {}
    relation banned {}

    relation can_view {
        rewrite exclusion(
            union(
                computed_userset(relation: "owner"),
                computed_userset(relation: "editor"),
                computed_userset(relation: "viewer"),
                tuple_to_userset(tupleset: "parent", computed_userset: "viewer")
            ),
            computed_userset(relation: "banned")
        )
    }

    relation can_edit {
        rewrite exclusion(
            union(
                computed_userset(relation: "owner"),
                computed_userset(relation: "editor")
            ),
            computed_userset(relation: "banned")
        )
    }

    relation can_admin {
        rewrite computed_userset(relation: "owner")
    }
}

namespace doc {
    relation parent {}
    relation owner {}
    relation editor {}
    relation viewer {}
    relation banned {}

    relation can_view {
        rewrite exclusion(
            union(
                computed_userset(relation: "owner"),
                computed_userset(relation: "editor"),
                computed_userset(relation: "viewer"),
                tuple_to_userset(tupleset: "parent", computed_userset: "can_view")
            ),
            computed_userset(relation: "banned")
        )
    }

    relation can_edit {
        rewrite exclusion(
            union(
                computed_userset(relation: "owner"),
                computed_userset(relation: "editor"),
                tuple_to_userset(tupleset: "parent", computed_userset: "can_edit")
            ),
            computed_userset(relation: "banned")
        )
    }

    relation can_share {
        rewrite union(
            computed_userset(relation: "owner"),
            computed_userset(relation: "editor")
        )
    }
}
"#;

fn main() -> Result<(), Box<dyn Error>> {
    let paths = ExamplePaths::new();
    remove_directory_if_exists(&paths.root)?;
    fs::create_dir_all(&paths.root)?;

    let engine = ZanzibarEngine::builder().build();
    engine.apply_schema(SchemaSource {
        name: Some("collaboration"),
        text: SCHEMA,
    })?;

    let initial_token = grant_initial_relationships(&engine)?;
    show_exact_consistency_check(&engine, initial_token)?;
    show_initial_permissions(&engine)?;
    assert_initial_access(&engine)?;

    change_project_membership(&engine)?;
    assert_updated_access(&engine)?;
    show_updated_permissions(&engine)?;

    demonstrate_schema_guardrail(&engine)?;
    export_and_reload_policy(&engine, &paths.policy_dir)?;
    save_and_load_zstd_snapshot(&engine, &paths.snapshot)?;

    remove_directory_if_exists(&paths.root)?;
    println!("\ncollaboration workspace example completed");
    Ok(())
}

fn grant_initial_relationships(
    engine: &ZanzibarEngine,
) -> Result<ConsistencyToken, Box<dyn Error>> {
    let mutations = [
        "workspace:platform#owner@user:alice",
        "workspace:platform#member@user:bob",
        "workspace:platform#auditor@user:eve",
        "project:launch#parent@workspace:platform#viewer",
        "project:launch#owner@user:alice",
        "project:launch#editor@user:bob",
        "project:launch#viewer@user:carol",
        "doc:launch_plan#parent@project:launch#can_view",
        "doc:launch_plan#viewer@user:dave",
        "doc:budget#parent@project:launch#can_view",
        "doc:budget#banned@user:eve",
    ]
    .into_iter()
    .map(RelationshipMutation::touch)
    .collect::<Result<Vec<_>, _>>()?;

    Ok(engine.write_relationships(mutations)?)
}

fn show_exact_consistency_check(
    engine: &ZanzibarEngine,
    token: ConsistencyToken,
) -> Result<(), Box<dyn Error>> {
    let allowed = engine
        .check(CheckRequest::new(
            doc("launch_plan"),
            relation("can_view"),
            user("alice"),
            Consistency::Exact(token),
        ))?
        .allowed;
    println!("Alice can view the launch plan at the initial revision: {allowed}");
    Ok(())
}

fn show_initial_permissions(engine: &ZanzibarEngine) -> Result<(), Box<dyn Error>> {
    println!("\ninitial access checks");
    print_check(engine, "Alice", "can_edit", "launch_plan")?;
    print_check(engine, "Bob", "can_edit", "launch_plan")?;
    print_check(engine, "Carol", "can_edit", "launch_plan")?;
    print_check(engine, "Eve", "can_view", "budget")?;

    let carol_permissions = engine.lookup_permissions(LookupPermissionsRequest::new(
        user("carol"),
        doc("launch_plan"),
        Consistency::Latest,
    ))?;
    println!(
        "Carol relations/permissions on launch_plan: {}",
        relation_names(&carol_permissions.permissions).join(", ")
    );

    let editable_projects = engine.lookup_resources(LookupResourcesRequest::new(
        user("bob"),
        relation("can_edit"),
        "project",
    ))?;
    println!(
        "Projects Bob can edit: {}",
        object_names(&editable_projects.resources).join(", ")
    );

    let project_editors = engine.lookup_subjects(LookupSubjectsRequest::new(
        project("launch"),
        relation("can_edit"),
        "user",
    ))?;
    println!(
        "Users who can edit the launch project: {}",
        user_names(&project_editors.subjects).join(", ")
    );

    Ok(())
}

fn assert_initial_access(engine: &ZanzibarEngine) -> Result<(), Box<dyn Error>> {
    assert!(can(engine, doc("launch_plan"), "can_edit", user("alice"))?);
    assert!(can(engine, doc("launch_plan"), "can_edit", user("bob"))?);
    assert!(!can(engine, doc("launch_plan"), "can_edit", user("carol"))?);
    assert!(can(engine, doc("launch_plan"), "can_view", user("eve"))?);
    assert!(!can(engine, doc("budget"), "can_view", user("eve"))?);
    Ok(())
}

fn change_project_membership(engine: &ZanzibarEngine) -> Result<(), Box<dyn Error>> {
    engine.touch_relationship("project:launch#editor@user:carol")?;
    engine.delete_relationship("project:launch#editor@user:bob")?;
    println!("\nupdated project membership: Carol is now editor, Bob is no longer editor");
    Ok(())
}

fn assert_updated_access(engine: &ZanzibarEngine) -> Result<(), Box<dyn Error>> {
    assert!(!can(engine, doc("launch_plan"), "can_edit", user("bob"))?);
    assert!(can(engine, doc("launch_plan"), "can_view", user("bob"))?);
    assert!(can(engine, doc("launch_plan"), "can_edit", user("carol"))?);
    assert!(!can(engine, doc("budget"), "can_view", user("eve"))?);
    Ok(())
}

fn show_updated_permissions(engine: &ZanzibarEngine) -> Result<(), Box<dyn Error>> {
    let carol_permissions = engine.lookup_permissions(LookupPermissionsRequest::new(
        user("carol"),
        doc("launch_plan"),
        Consistency::Latest,
    ))?;
    println!(
        "Carol relations/permissions after promotion: {}",
        relation_names(&carol_permissions.permissions).join(", ")
    );

    let budget_permissions = engine.lookup_object_permissions(
        LookupObjectPermissionsRequest::new(doc("budget"), "user", Consistency::Latest),
    )?;
    println!("Budget document relation/permission groups:");
    for permission in budget_permissions.permissions {
        println!(
            "  {}: {}",
            permission.permission.0,
            user_names(&permission.subjects).join(", ")
        );
    }
    Ok(())
}

fn demonstrate_schema_guardrail(engine: &ZanzibarEngine) -> Result<(), Box<dyn Error>> {
    match engine.delete_relation("doc", "viewer") {
        Ok(_) => Err(Box::new(io::Error::other(
            "schema guardrail unexpectedly allowed deleting a live relation",
        ))),
        Err(error) => {
            println!("\nschema guardrail rejected deleting doc.viewer: {error}");
            assert!(can(engine, doc("launch_plan"), "can_view", user("dave"))?);
            Ok(())
        }
    }
}

fn export_and_reload_policy(
    engine: &ZanzibarEngine,
    directory: &Path,
) -> Result<(), Box<dyn Error>> {
    engine.export_policy_files(directory)?;
    let policy = engine.export_policy_text()?;
    println!(
        "\nexported reviewable policy: {} schema bytes, {} relationship files",
        policy.schema.len(),
        policy.relationship_files.len()
    );
    for file in &policy.relationship_files {
        println!("  {} ({} bytes)", file.path, file.contents.len());
    }

    let imported = ZanzibarEngine::from_policy_text(&policy)?;
    assert_updated_access(&imported)?;
    Ok(())
}

fn save_and_load_zstd_snapshot(
    engine: &ZanzibarEngine,
    snapshot: &Path,
) -> Result<(), Box<dyn Error>> {
    engine.save_snapshot(snapshot, SnapshotSaveOptions::zstd())?;
    let size = fs::metadata(snapshot)?.len();
    let loaded = ZanzibarEngine::load_snapshot(snapshot, SnapshotLoadOptions::zstd())?;
    assert_updated_access(&loaded)?;
    println!("saved and loaded zstd snapshot: {size} bytes");
    Ok(())
}

fn print_check(
    engine: &ZanzibarEngine,
    user_id: &str,
    permission: &str,
    doc_id: &str,
) -> Result<(), Box<dyn Error>> {
    let allowed = can(
        engine,
        doc(doc_id),
        permission,
        user(&user_id.to_ascii_lowercase()),
    )?;
    println!("  {user_id} {permission} {doc_id}: {allowed}");
    Ok(())
}

fn can(
    engine: &ZanzibarEngine,
    object: Object,
    permission: &str,
    subject: User,
) -> Result<bool, Box<dyn Error>> {
    Ok(engine
        .check(CheckRequest::new(
            object,
            relation(permission),
            subject,
            Consistency::Latest,
        ))?
        .allowed)
}

fn project(id: &str) -> Object {
    Object::new("project", id)
}

fn doc(id: &str) -> Object {
    Object::new("doc", id)
}

fn relation(name: &str) -> Relation {
    Relation::new(name)
}

fn user(id: &str) -> User {
    User::user_id(id)
}

fn relation_names(relations: &[Relation]) -> Vec<String> {
    relations
        .iter()
        .map(|relation| relation.0.clone())
        .collect()
}

fn object_names(objects: &[Object]) -> Vec<String> {
    objects
        .iter()
        .map(|object| format!("{}:{}", object.namespace, object.id))
        .collect()
}

fn user_names(users: &[User]) -> Vec<String> {
    users.iter().map(user_name).collect()
}

fn user_name(user: &User) -> String {
    match user {
        User::UserId(id) => format!("user:{id}"),
        User::Userset(object, relation) => {
            format!("{}:{}#{}", object.namespace, object.id, relation.0)
        }
    }
}

fn remove_directory_if_exists(path: &Path) -> io::Result<()> {
    match fs::remove_dir_all(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

#[derive(Debug)]
struct ExamplePaths {
    root: PathBuf,
    policy_dir: PathBuf,
    snapshot: PathBuf,
}

impl ExamplePaths {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "simple-zanzibar-collaboration-example-{}",
            process::id()
        ));
        Self {
            policy_dir: root.join("policy"),
            snapshot: root.join("workspace.szsnap.zst"),
            root,
        }
    }
}
