use std::{
    fs,
    path::{Path, PathBuf},
    process,
    sync::atomic::{AtomicU64, Ordering},
};

use simple_zanzibar::{
    PolicyText, SnapshotLoadOptions, SnapshotSaveOptions, ZanzibarEngine,
    model::{
        CheckRequest, LookupObjectPermissionsRequest, LookupPermissionsRequest,
        LookupResourcesRequest, LookupSubjectsRequest, Object, PermissionSubjects, Relation, User,
    },
    revision::Consistency,
};

static NEXT_TEST_PATH: AtomicU64 = AtomicU64::new(1);

#[test]
fn test_should_run_reviewable_policy_to_snapshot_e2e() -> Result<(), Box<dyn std::error::Error>> {
    let policy = PolicyText::from_single_relationship_file(
        schema().to_string(),
        [
            "folder:root#viewer@group:eng#member",
            "group:eng#member@user:alice",
            "doc:readme#parent@folder:root#viewer",
            "doc:secret#parent@folder:root#viewer",
            "doc:secret#banned@user:alice",
        ]
        .join("\n"),
    );
    let engine = ZanzibarEngine::from_policy_text(&policy)?;
    let alice = User::user_id("alice");
    let readme = object("doc", "readme");
    let secret = object("doc", "secret");

    let token = engine.touch_relationship("doc:readme#owner@user:bob")?;
    assert!(
        engine
            .check(CheckRequest::new(
                readme.clone(),
                relation("viewer"),
                alice.clone(),
                Consistency::Exact(token),
            ))?
            .allowed
    );
    assert!(
        !engine
            .check(CheckRequest::new(
                secret.clone(),
                relation("viewer"),
                alice.clone(),
                Consistency::Latest,
            ))?
            .allowed
    );
    assert_eq!(
        engine
            .lookup_resources(LookupResourcesRequest::new(
                alice.clone(),
                relation("viewer"),
                "doc",
            ))?
            .resources,
        vec![readme.clone()],
    );
    assert_eq!(
        engine
            .lookup_subjects(LookupSubjectsRequest::new(
                readme.clone(),
                relation("viewer"),
                "user",
            ))?
            .subjects,
        vec![User::user_id("bob"), User::user_id("alice")],
    );
    assert_eq!(
        engine
            .lookup_permissions(LookupPermissionsRequest::new(
                User::user_id("bob"),
                readme.clone(),
                Consistency::Latest,
            ))?
            .permissions,
        vec![relation("owner"), relation("viewer")],
    );
    assert_eq!(
        engine
            .lookup_object_permissions(LookupObjectPermissionsRequest::new(
                readme.clone(),
                "user",
                Consistency::Latest,
            ))?
            .permissions,
        vec![
            PermissionSubjects {
                permission: relation("owner"),
                subjects: vec![User::user_id("bob")],
            },
            PermissionSubjects {
                permission: relation("parent"),
                subjects: vec![User::user_id("alice")],
            },
            PermissionSubjects {
                permission: relation("viewer"),
                subjects: vec![User::user_id("bob"), User::user_id("alice")],
            },
        ],
    );

    let snapshot_path = temp_snapshot_path("prod_e2e");
    engine.save_snapshot(&snapshot_path, SnapshotSaveOptions::zstd())?;
    let loaded = ZanzibarEngine::load_snapshot(&snapshot_path, SnapshotLoadOptions::zstd())?;
    remove_file(&snapshot_path)?;

    assert_equivalent_queries(&engine, &loaded, &readme, &secret, &alice)?;
    Ok(())
}

fn assert_equivalent_queries(
    left: &ZanzibarEngine,
    right: &ZanzibarEngine,
    readme: &Object,
    secret: &Object,
    alice: &User,
) -> Result<(), Box<dyn std::error::Error>> {
    for engine in [left, right] {
        assert!(engine.check_relation(readme, &relation("viewer"), alice)?);
        assert!(!engine.check_relation(secret, &relation("viewer"), alice)?);
        assert_eq!(
            engine
                .lookup_resources(LookupResourcesRequest::new(
                    alice.clone(),
                    relation("viewer"),
                    "doc",
                ))?
                .resources,
            vec![readme.clone()],
        );
    }
    Ok(())
}

fn schema() -> &'static str {
    r#"
    namespace user {
        relation member {}
    }

    namespace group {
        relation member {}
    }

    namespace folder {
        relation viewer {}
    }

    namespace doc {
        relation owner {}
        relation parent {}
        relation banned {}
        relation viewer {
            rewrite exclusion(
                union(
                    computed_userset(relation: "owner"),
                    tuple_to_userset(tupleset: "parent", computed_userset: "viewer")
                ),
                computed_userset(relation: "banned")
            )
        }
    }
    "#
}

fn object(namespace: &str, id: &str) -> Object {
    Object::new(namespace, id)
}

fn relation(name: &str) -> Relation {
    Relation::new(name)
}

fn temp_snapshot_path(prefix: &str) -> PathBuf {
    let sequence = NEXT_TEST_PATH.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "simple-zanzibar-{prefix}-{}-{sequence}.szsnap.zst",
        process::id(),
    ))
}

fn remove_file(path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(Box::new(error)),
    }
}
