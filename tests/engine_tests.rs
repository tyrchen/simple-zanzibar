use std::io;
use std::sync::{Arc, Barrier};
use std::thread;

use simple_zanzibar::ZanzibarEngine;
use simple_zanzibar::domain::Relationship;
use simple_zanzibar::model::{
    CheckRequest, ExpandRequest, ExpandedUserset, LookupResourcesRequest, LookupSubjectsRequest,
    Object, Relation, User,
};
use simple_zanzibar::relationship::{
    Precondition, RelationshipFilter, RelationshipMutation, SubjectFilter,
};
use simple_zanzibar::revision::Consistency;
use simple_zanzibar::schema::SchemaSource;

const DOC_SCHEMA: &str = r"
    namespace doc {
        relation viewer {}
    }
";

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

    assert!(result.is_err());
}
