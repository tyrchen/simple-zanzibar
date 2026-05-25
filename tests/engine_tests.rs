use std::{
    io,
    sync::{Arc, Barrier},
    thread,
};

use simple_zanzibar::{
    EngineError, TenantId, ZanzibarEngine, ZanzibarTenantShards,
    domain::Relationship,
    model::{
        CheckRequest, ExpandRequest, ExpandedUserset, LookupResourcesRequest,
        LookupSubjectsRequest, Object, Relation, User,
    },
    relationship::{
        Precondition, RelationshipFilter, RelationshipMutation, StoreError, SubjectFilter,
    },
    revision::Consistency,
    schema::SchemaSource,
};

const DOC_SCHEMA: &str = r"
    namespace doc {
        relation viewer {}
    }
";

#[test]
fn test_should_make_engine_send_and_sync() {
    fn assert_send_sync<T: Send + Sync>() {}

    assert_send_sync::<ZanzibarEngine>();
}

#[test]
fn test_should_isolate_multiple_engine_instances() -> Result<(), Box<dyn std::error::Error>> {
    let first = ZanzibarEngine::builder().build();
    let second = ZanzibarEngine::builder().build();
    first.apply_schema(SchemaSource {
        name: Some("doc-schema"),
        text: DOC_SCHEMA,
    })?;
    second.apply_schema(SchemaSource {
        name: Some("doc-schema"),
        text: DOC_SCHEMA,
    })?;

    first.touch_relationship("doc:readme#viewer@user:alice")?;

    assert!(first.check_relation(&doc_object(), &viewer(), &User::user_id("alice"))?);
    assert!(!second.check_relation(&doc_object(), &viewer(), &User::user_id("alice"))?);
    Ok(())
}

#[test]
fn test_should_shard_engines_by_tenant() -> Result<(), Box<dyn std::error::Error>> {
    let shards = ZanzibarTenantShards::default();
    let tenant_a = TenantId::new("tenant-a")?;
    let tenant_b = TenantId::new("tenant-b")?;

    let first_a = shards.get_or_create(tenant_a.clone());
    let second_a = shards.get_or_create(tenant_a.clone());
    let engine_b = shards.get_or_create(tenant_b.clone());
    assert!(Arc::ptr_eq(&first_a, &second_a));

    for engine in [&first_a, &engine_b] {
        engine.apply_schema(SchemaSource {
            name: Some("doc-schema"),
            text: DOC_SCHEMA,
        })?;
    }
    first_a.touch_relationship("doc:readme#viewer@user:alice")?;

    assert!(first_a.check_relation(&doc_object(), &viewer(), &User::user_id("alice"))?);
    assert!(!engine_b.check_relation(&doc_object(), &viewer(), &User::user_id("alice"))?);
    assert_eq!(shards.tenants(), vec![tenant_a, tenant_b]);
    Ok(())
}

#[test]
fn test_should_use_public_engine_request_response_api() -> Result<(), Box<dyn std::error::Error>> {
    let engine = ZanzibarEngine::builder().build();
    engine.apply_schema(SchemaSource {
        name: Some("doc-schema"),
        text: DOC_SCHEMA,
    })?;

    let relationship: Relationship = "doc:readme#viewer@user:alice".parse()?;
    let subject = SubjectFilter::exact("user".try_into()?, "alice".try_into()?, None);
    let precondition = Precondition::MustNotMatch(RelationshipFilter::for_exact_subject(
        relationship.resource(),
        relationship.relation().clone(),
        subject,
    ));
    let token = engine.write_relationships_with_preconditions(
        [RelationshipMutation::Touch(relationship)],
        [precondition],
    )?;

    let object = doc_object();
    let relation = viewer();
    let alice = User::UserId("alice".to_string());

    assert!(
        engine
            .check(CheckRequest::new(
                object.clone(),
                relation.clone(),
                alice.clone(),
                Consistency::Latest,
            ))?
            .allowed
    );
    assert!(
        engine
            .check(CheckRequest::new(
                object.clone(),
                relation.clone(),
                alice.clone(),
                Consistency::Exact(token),
            ))?
            .allowed
    );
    assert!(matches!(
        engine
            .expand(ExpandRequest::new(
                object.clone(),
                relation.clone(),
                Consistency::Latest,
            ))?
            .expanded,
        ExpandedUserset::Union(_)
    ));
    assert_eq!(
        engine
            .lookup_resources(LookupResourcesRequest {
                subject: alice.clone(),
                permission: relation.clone(),
                resource_type: "doc".to_string(),
            })?
            .resources,
        vec![object.clone()],
    );
    assert_eq!(
        engine
            .lookup_subjects(LookupSubjectsRequest {
                resource: object,
                permission: relation,
                subject_type: "user".to_string(),
            })?
            .subjects,
        vec![alice],
    );
    Ok(())
}

#[test]
fn test_should_validate_public_requests_before_snapshot_access() {
    let engine = ZanzibarEngine::builder().build();

    let result = engine.check(CheckRequest::new(
        Object::new("doc", "bad id"),
        viewer(),
        User::user_id("alice"),
        Consistency::Latest,
    ));

    assert!(matches!(result, Err(EngineError::Domain(_))));
}

#[test]
fn test_should_persist_direct_non_user_subject_relationships()
-> Result<(), Box<dyn std::error::Error>> {
    let engine = ZanzibarEngine::builder().build();
    engine.apply_schema(SchemaSource {
        name: Some("doc-schema"),
        text: DOC_SCHEMA,
    })?;
    let relationship: Relationship = "doc:readme#viewer@group:eng".parse()?;

    engine.write_relationships([RelationshipMutation::Create(relationship.clone())])?;
    let duplicate =
        engine.write_relationships([RelationshipMutation::Create(relationship.clone())]);

    assert!(matches!(
        duplicate,
        Err(EngineError::Store(
            StoreError::RelationshipAlreadyExists { .. }
        ))
    ));

    engine.apply_schema(SchemaSource {
        name: Some("doc-schema"),
        text: DOC_SCHEMA,
    })?;
    engine.write_relationships([RelationshipMutation::Delete(relationship)])?;
    Ok(())
}

#[test]
fn test_should_publish_atomic_snapshots_while_latest_readers_run()
-> Result<(), Box<dyn std::error::Error>> {
    let engine = Arc::new(ZanzibarEngine::builder().build());
    let schema_token = engine.apply_schema(SchemaSource {
        name: Some("doc-schema"),
        text: DOC_SCHEMA,
    })?;
    let object = doc_object();
    let relation = viewer();
    let alice = User::UserId("alice".to_string());
    let start = Arc::new(Barrier::new(5));

    let writer_engine = Arc::clone(&engine);
    let writer_start = Arc::clone(&start);
    let writer = thread::spawn(move || {
        writer_start.wait();
        let relationship: Relationship = "doc:readme#viewer@user:alice".parse()?;
        writer_engine.write_relationships([RelationshipMutation::Touch(relationship)])
    });

    let readers = (0..4)
        .map(|_| {
            let reader_engine = Arc::clone(&engine);
            let reader_start = Arc::clone(&start);
            let reader_object = object.clone();
            let reader_relation = relation.clone();
            let reader_alice = alice.clone();
            thread::spawn(move || -> Result<usize, simple_zanzibar::EngineError> {
                reader_start.wait();
                let mut allowed_reads = 0;
                for _ in 0..500 {
                    let response = reader_engine.check(CheckRequest::new(
                        reader_object.clone(),
                        reader_relation.clone(),
                        reader_alice.clone(),
                        Consistency::Latest,
                    ))?;
                    if response.allowed {
                        allowed_reads += 1;
                    }
                }
                Ok(allowed_reads)
            })
        })
        .collect::<Vec<_>>();

    let write_token = writer.join().map_err(|_| thread_panic_error("writer"))??;
    for reader in readers {
        let allowed_reads = reader.join().map_err(|_| thread_panic_error("reader"))??;
        assert!(allowed_reads <= 500);
    }

    assert!(
        engine
            .check(CheckRequest::new(
                object.clone(),
                relation.clone(),
                alice.clone(),
                Consistency::Exact(write_token),
            ))?
            .allowed
    );
    assert!(
        !engine
            .check(CheckRequest::new(
                object,
                relation,
                alice,
                Consistency::Exact(schema_token),
            ))?
            .allowed
    );
    Ok(())
}

fn doc_object() -> Object {
    Object {
        namespace: "doc".to_string(),
        id: "readme".to_string(),
    }
}

fn viewer() -> Relation {
    Relation("viewer".to_string())
}

fn thread_panic_error(name: &str) -> io::Error {
    io::Error::other(format!("{name} thread panicked"))
}

#[cfg(feature = "serde")]
#[test]
fn test_should_serialize_public_request_dtos_with_camel_case()
-> Result<(), Box<dyn std::error::Error>> {
    let request = CheckRequest::new(
        doc_object(),
        viewer(),
        User::UserId("alice".to_string()),
        Consistency::Latest,
    );

    let serialized = serde_json::to_value(&request)?;

    assert_eq!(
        serialized,
        serde_json::json!({
            "object": {
                "namespace": "doc",
                "id": "readme",
            },
            "relation": "viewer",
            "user": {
                "type": "userId",
                "value": "alice",
            },
            "consistency": {
                "kind": "latest",
            },
        }),
    );

    let decoded: CheckRequest = serde_json::from_value(serialized)?;
    assert_eq!(decoded, request);
    Ok(())
}

#[cfg(feature = "serde")]
#[test]
fn test_should_validate_domain_values_during_deserialization() {
    let result: Result<Relationship, _> = serde_json::from_str(r#""doc:bad id#viewer@user:alice""#);
    let invalid_check: Result<CheckRequest, _> = serde_json::from_value(serde_json::json!({
        "object": {
            "namespace": "doc",
            "id": "bad id",
        },
        "relation": "viewer",
        "user": {
            "type": "userId",
            "value": "alice",
        },
        "consistency": {
            "kind": "latest",
        },
    }));
    let invalid_lookup_resources: Result<LookupResourcesRequest, _> =
        serde_json::from_value(serde_json::json!({
            "subject": {
                "type": "userId",
                "value": "alice",
            },
            "permission": "viewer",
            "resourceType": "bad-type",
        }));
    let invalid_lookup_subjects: Result<LookupSubjectsRequest, _> =
        serde_json::from_value(serde_json::json!({
            "resource": {
                "namespace": "doc",
                "id": "readme",
            },
            "permission": "viewer",
            "subjectType": "bad type",
        }));

    assert!(result.is_err());
    assert!(invalid_check.is_err());
    assert!(invalid_lookup_resources.is_err());
    assert!(invalid_lookup_subjects.is_err());
}
