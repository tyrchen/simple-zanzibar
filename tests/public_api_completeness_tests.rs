use std::{
    fs,
    num::NonZeroU32,
    path::{Path, PathBuf},
    process,
    sync::atomic::{AtomicU64, Ordering},
};

use simple_zanzibar::{
    EngineError, PolicyText, SnapshotLoadOptions, SnapshotSaveOptions, ZanzibarEngine,
    ZanzibarService,
    domain::Relationship,
    eval::EvaluationLimits,
    model::{
        CheckRequest, LookupObjectPermissionsRequest, LookupPermissionsRequest,
        LookupResourcesRequest, Object, PermissionSubjects, Relation, User,
    },
    relationship::RelationshipMutation,
    revision::Consistency,
    schema::SchemaSource,
};

static NEXT_TEST_PATH: AtomicU64 = AtomicU64::new(1);

#[test]
fn test_should_lookup_subject_permissions_and_object_permission_subjects()
-> Result<(), Box<dyn std::error::Error>> {
    let service = populated_service()?;
    let alice = User::UserId("alice".to_string());
    let direct_doc = doc("direct_doc");

    let permissions = service.lookup_permissions(&LookupPermissionsRequest {
        subject: alice.clone(),
        resource: direct_doc.clone(),
        consistency: Consistency::Latest,
    })?;

    assert_eq!(
        permissions.permissions,
        vec![relation("can_view"), relation("viewer")],
    );

    let object_permissions =
        service.lookup_object_permissions(&LookupObjectPermissionsRequest {
            resource: direct_doc,
            subject_type: "user".to_string(),
            consistency: Consistency::Latest,
        })?;

    assert_eq!(
        object_permissions.permissions,
        vec![
            PermissionSubjects {
                permission: relation("can_view"),
                subjects: vec![alice.clone()],
            },
            PermissionSubjects {
                permission: relation("viewer"),
                subjects: vec![alice],
            },
        ],
    );
    Ok(())
}

#[test]
fn test_should_expose_permission_lookup_through_engine_api()
-> Result<(), Box<dyn std::error::Error>> {
    let policy = populated_service()?.export_policy_text()?;
    let engine = ZanzibarEngine::from_policy_text(&policy)?;

    let permissions = engine.lookup_permissions(LookupPermissionsRequest::new(
        User::UserId("alice".to_string()),
        doc("inherited_doc"),
        Consistency::Latest,
    ))?;

    assert_eq!(
        permissions.permissions,
        vec![relation("can_view"), relation("parent")],
    );
    Ok(())
}

#[test]
fn test_should_offer_checked_ergonomic_engine_helpers() -> Result<(), Box<dyn std::error::Error>> {
    let engine = ZanzibarEngine::builder().build();
    engine.apply_schema(SchemaSource {
        name: Some("docs"),
        text: r"
        namespace doc {
            relation viewer {}
        }
        ",
    })?;

    engine.touch_relationship("doc:readme#viewer@user:alice")?;

    assert!(
        engine
            .check(CheckRequest::new(
                doc("readme"),
                relation("viewer"),
                User::UserId("alice".to_string()),
                Consistency::Latest,
            ))?
            .allowed
    );
    assert_eq!(
        engine
            .lookup_permissions(LookupPermissionsRequest::new(
                User::UserId("alice".to_string()),
                doc("readme"),
                Consistency::Latest,
            ))?
            .permissions,
        vec![relation("viewer")],
    );
    assert_eq!(
        engine
            .lookup_object_permissions(LookupObjectPermissionsRequest::new(
                doc("readme"),
                "user",
                Consistency::Latest,
            ))?
            .permissions,
        vec![PermissionSubjects {
            permission: relation("viewer"),
            subjects: vec![User::UserId("alice".to_string())],
        }],
    );

    engine.delete_relationship("doc:readme#viewer@user:alice")?;
    assert!(
        !engine
            .check(CheckRequest::new(
                doc("readme"),
                relation("viewer"),
                User::UserId("alice".to_string()),
                Consistency::Latest,
            ))?
            .allowed
    );

    Ok(())
}

#[test]
fn test_should_export_and_import_reviewable_policy_text() -> Result<(), Box<dyn std::error::Error>>
{
    let service = populated_service()?;
    let policy = service.export_policy_text()?;

    assert!(policy.schema.starts_with("namespace doc {\n"));
    assert_eq!(
        policy
            .relationship_files
            .iter()
            .map(|file| file.path.as_str())
            .collect::<Vec<_>>(),
        vec![
            "relationships/doc.zedtuples",
            "relationships/folder.zedtuples",
            "relationships/group.zedtuples",
        ],
    );
    let doc_relationships = policy
        .relationship_files
        .iter()
        .find(|file| file.path == "relationships/doc.zedtuples")
        .ok_or("missing doc relationship file")?;
    assert!(
        doc_relationships
            .contents
            .contains("doc:direct_doc#viewer@group:eng#member\n")
    );

    let loaded = ZanzibarService::from_policy_text(&policy)?;
    assert_equivalent_public_queries(&service, &loaded)?;
    Ok(())
}

#[test]
fn test_should_export_policy_files_grouped_for_review() -> Result<(), Box<dyn std::error::Error>> {
    let service = populated_service()?;
    let directory = temp_directory("policy_export");
    service.export_policy_files(&directory)?;

    let schema = fs::read_to_string(directory.join("schema.zed"))?;
    let doc_relationships =
        fs::read_to_string(directory.join("relationships").join("doc.zedtuples"))?;
    let group_relationships =
        fs::read_to_string(directory.join("relationships").join("group.zedtuples"))?;

    assert!(schema.starts_with("namespace doc {\n"));
    assert_eq!(
        doc_relationships.lines().collect::<Vec<_>>(),
        vec![
            "doc:denied_doc#banned@user:alice",
            "doc:denied_doc#viewer@group:eng#member",
            "doc:direct_doc#viewer@group:eng#member",
            "doc:inherited_doc#parent@folder:root#inherited_viewer",
        ],
    );
    assert_eq!(group_relationships, "group:eng#member@user:alice\n");

    remove_directory(&directory);
    Ok(())
}

#[test]
fn test_should_save_snapshot_from_policy_text_with_zstd() -> Result<(), Box<dyn std::error::Error>>
{
    let policy = populated_service()?.export_policy_text()?;
    let path = temp_snapshot_path("policy_zstd");
    ZanzibarService::save_snapshot_from_policy_text(&path, &policy, SnapshotSaveOptions::zstd())?;

    let loaded = ZanzibarService::load_snapshot(&path, SnapshotLoadOptions::zstd())?;

    assert!(loaded.check(
        &doc("direct_doc"),
        &relation("can_view"),
        &User::UserId("alice".to_string()),
    )?);
    remove_file(&path);
    Ok(())
}

#[test]
fn test_should_reject_duplicate_policy_text_relationships_atomically()
-> Result<(), Box<dyn std::error::Error>> {
    let mut service = populated_service()?;
    let before = service.export_policy_text()?;
    let duplicate_policy = PolicyText::from_single_relationship_file(
        schema().to_string(),
        "group:eng#member@user:alice\ngroup:eng#member@user:alice\n".to_string(),
    );

    let result = service.apply_policy_text(&duplicate_policy);

    assert!(result.is_err());
    assert_eq!(service.export_policy_text()?, before);
    Ok(())
}

#[test]
fn test_should_replace_and_delete_schema_policy_with_atomic_revalidation()
-> Result<(), Box<dyn std::error::Error>> {
    let mut service = populated_service()?;
    let before = service.export_policy_text()?;

    let delete_with_live_relationship = service.delete_relation("doc", "viewer");
    assert!(delete_with_live_relationship.is_err());
    assert_eq!(service.export_policy_text()?, before);

    let relationship: Relationship = "doc:direct_doc#viewer@group:eng#member".parse()?;
    service.apply_relationship_mutations([RelationshipMutation::Delete(relationship)], [])?;
    let still_invalid = service.delete_relation("doc", "viewer");
    assert!(still_invalid.is_err());

    for relationship in [
        "doc:denied_doc#viewer@group:eng#member",
        "doc:denied_doc#banned@user:alice",
        "doc:inherited_doc#parent@folder:root#inherited_viewer",
    ] {
        service.apply_relationship_mutations(
            [RelationshipMutation::Delete(relationship.parse()?)],
            [],
        )?;
    }
    service.delete_namespace("doc")?;

    let check = service.check(
        &doc("direct_doc"),
        &relation("can_view"),
        &User::UserId("alice".to_string()),
    );
    assert!(check.is_err());
    Ok(())
}

#[test]
fn test_should_expose_schema_replacement_and_policy_export_through_engine()
-> Result<(), Box<dyn std::error::Error>> {
    let engine = ZanzibarEngine::builder().build();
    engine.apply_schema(SchemaSource {
        name: Some("docs"),
        text: r"
        namespace doc {
            relation viewer {}
        }
        ",
    })?;
    let relationship: Relationship = "doc:readme#viewer@user:alice".parse()?;
    engine.write_relationships([RelationshipMutation::Touch(relationship)])?;

    let replacement = engine.replace_schema(SchemaSource {
        name: Some("broken"),
        text: r"
        namespace doc {
            relation editor {}
        }
        ",
    });
    assert!(matches!(replacement, Err(EngineError::Schema(_))));

    let exported = engine.export_policy_text()?;
    assert!(exported.schema.contains("relation viewer {}"));

    let directory = temp_directory("engine_policy_export");
    engine.export_policy_files(&directory)?;
    assert!(directory.join("schema.zed").exists());
    remove_directory(&directory);
    Ok(())
}

fn assert_equivalent_public_queries(
    original: &ZanzibarService,
    loaded: &ZanzibarService,
) -> Result<(), Box<dyn std::error::Error>> {
    let alice = User::UserId("alice".to_string());
    let request = LookupResourcesRequest {
        subject: alice.clone(),
        permission: relation("can_view"),
        resource_type: "doc".to_string(),
    };
    assert_eq!(
        original.lookup_resources(&request)?,
        loaded.lookup_resources(&request)?
    );
    assert_eq!(
        original.lookup_permissions(&LookupPermissionsRequest {
            subject: alice,
            resource: doc("direct_doc"),
            consistency: Consistency::Latest,
        })?,
        loaded.lookup_permissions(&LookupPermissionsRequest {
            subject: User::UserId("alice".to_string()),
            resource: doc("direct_doc"),
            consistency: Consistency::Latest,
        })?,
    );
    assert_eq!(
        original.check(
            &doc("inherited_doc"),
            &relation("can_view"),
            &User::UserId("alice".to_string()),
        )?,
        loaded.check(
            &doc("inherited_doc"),
            &relation("can_view"),
            &User::UserId("alice".to_string()),
        )?,
    );
    Ok(())
}

fn populated_service() -> Result<ZanzibarService, Box<dyn std::error::Error>> {
    let mut service = ZanzibarService::new().with_evaluation_limits(EvaluationLimits {
        max_depth: non_zero_u32(32),
        max_fanout_per_step: non_zero_u32(10_000),
        max_lookup_results: non_zero_u32(1_000),
    });
    service.add_dsl(schema())?;
    for relationship in [
        "group:eng#member@user:alice",
        "folder:root#viewer@group:eng#member",
        "doc:inherited_doc#parent@folder:root#inherited_viewer",
        "doc:direct_doc#viewer@group:eng#member",
        "doc:denied_doc#viewer@group:eng#member",
        "doc:denied_doc#banned@user:alice",
    ] {
        service.apply_relationship_mutations(
            [RelationshipMutation::Touch(relationship.parse()?)],
            [],
        )?;
    }
    Ok(service)
}

fn schema() -> &'static str {
    r#"
    namespace group {
        relation member {}
    }

    namespace folder {
        relation viewer {}
        relation inherited_viewer {
            rewrite computed_userset(relation: "viewer")
        }
    }

    namespace doc {
        relation parent {}
        relation viewer {}
        relation banned {}
        relation can_view {
            rewrite exclusion(
                union(
                    computed_userset(relation: "viewer"),
                    tuple_to_userset(tupleset: "parent", computed_userset: "inherited_viewer")
                ),
                computed_userset(relation: "banned")
            )
        }
    }
    "#
}

fn doc(id: &str) -> Object {
    Object::new("doc", id)
}

fn relation(name: &str) -> Relation {
    Relation::new(name)
}

fn temp_snapshot_path(name: &str) -> PathBuf {
    temp_directory_name(name).with_extension("szsnap")
}

fn temp_directory(name: &str) -> PathBuf {
    temp_directory_name(name)
}

fn temp_directory_name(name: &str) -> PathBuf {
    let counter = NEXT_TEST_PATH.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "simple_zanzibar_public_api_{name}_{}_{}",
        process::id(),
        counter,
    ))
}

fn remove_file(path: &Path) {
    let _ = fs::remove_file(path);
}

fn remove_directory(path: &Path) {
    let _ = fs::remove_dir_all(path);
}

fn non_zero_u32(value: u32) -> NonZeroU32 {
    match NonZeroU32::new(value) {
        Some(value) => value,
        None => NonZeroU32::MIN,
    }
}
