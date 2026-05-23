use std::{num::NonZeroUsize, str::FromStr};

use simple_zanzibar::{
    ZanzibarService,
    error::ZanzibarError,
    model::{NamespaceConfig, Object, Relation, RelationConfig, RelationTuple, User},
    revision::{Consistency, ConsistencyError, ConsistencyToken},
};

const DOC_SCHEMA: &str = r"
    namespace doc {
        relation viewer {}
    }
";

#[test]
fn test_should_round_trip_consistency_token() -> Result<(), Box<dyn std::error::Error>> {
    let mut service = ZanzibarService::new();
    let token = service.add_dsl_with_token(DOC_SCHEMA)?;

    let parsed = ConsistencyToken::from_str(&token.to_string())?;

    assert_eq!(parsed, token);
    Ok(())
}

#[test]
fn test_should_hash_schema_independently_of_namespace_order()
-> Result<(), Box<dyn std::error::Error>> {
    let doc = namespace("doc", "viewer");
    let folder = namespace("folder", "viewer");

    let mut first = ZanzibarService::new();
    first.add_config(doc.clone())?;
    let first_token = first.add_config_with_token(folder.clone())?;

    let mut second = ZanzibarService::new();
    second.add_config(folder)?;
    let second_token = second.add_config_with_token(doc)?;

    assert_eq!(first_token.schema_hash(), second_token.schema_hash());
    Ok(())
}

#[test]
fn test_should_read_exact_snapshot_without_later_writes() -> Result<(), Box<dyn std::error::Error>>
{
    let mut service = ZanzibarService::new();
    service.add_dsl(DOC_SCHEMA)?;
    let object = doc_object();
    let relation = Relation("viewer".to_string());
    let alice = User::UserId("alice".to_string());
    let bob = User::UserId("bob".to_string());

    let alice_token = service.write_tuple_with_token(&tuple("readme", "alice"))?;
    service.write_tuple_with_token(&tuple("readme", "bob"))?;

    assert!(service.check_with_consistency(
        &object,
        &relation,
        &alice,
        Consistency::Exact(alice_token.clone()),
    )?);
    assert!(!service.check_with_consistency(
        &object,
        &relation,
        &bob,
        Consistency::Exact(alice_token),
    )?);
    assert!(service.check_with_consistency(&object, &relation, &bob, Consistency::Latest)?);
    Ok(())
}

#[test]
fn test_should_read_exact_snapshot_without_later_delete() -> Result<(), Box<dyn std::error::Error>>
{
    let mut service = ZanzibarService::new();
    service.add_dsl(DOC_SCHEMA)?;
    let object = doc_object();
    let relation = Relation("viewer".to_string());
    let alice = User::UserId("alice".to_string());
    let tuple = tuple("readme", "alice");

    let write_token = service.write_tuple_with_token(&tuple)?;
    service.delete_tuple_with_token(&tuple)?;

    assert!(service.check_with_consistency(
        &object,
        &relation,
        &alice,
        Consistency::Exact(write_token),
    )?);
    assert!(!service.check_with_consistency(&object, &relation, &alice, Consistency::Latest)?);
    Ok(())
}

#[test]
fn test_should_read_exact_schema_snapshot_without_later_schema()
-> Result<(), Box<dyn std::error::Error>> {
    let mut service = ZanzibarService::new();
    let doc_only_token = service.add_dsl_with_token(DOC_SCHEMA)?;
    service.add_dsl(
        r"
        namespace folder {
            relation viewer {}
        }
    ",
    )?;

    let folder = Object {
        namespace: "folder".to_string(),
        id: "root".to_string(),
    };
    let result = service.check_with_consistency(
        &folder,
        &Relation("viewer".to_string()),
        &User::UserId("alice".to_string()),
        Consistency::Exact(doc_only_token),
    );

    assert!(matches!(result, Err(ZanzibarError::Schema(_))));
    assert!(!service.check_with_consistency(
        &folder,
        &Relation("viewer".to_string()),
        &User::UserId("alice".to_string()),
        Consistency::Latest,
    )?);
    Ok(())
}

#[test]
fn test_should_reject_wrong_datastore_token() -> Result<(), Box<dyn std::error::Error>> {
    let mut service = ZanzibarService::new();
    service.add_dsl(DOC_SCHEMA)?;
    let token = service.write_tuple_with_token(&tuple("readme", "alice"))?;
    let wrong_token = ConsistencyToken::from_str(&replace_token_field(
        &token.to_string(),
        3,
        "00000000000000000000000000000000",
    )?)?;

    let result = service.check_with_consistency(
        &doc_object(),
        &Relation("viewer".to_string()),
        &User::UserId("alice".to_string()),
        Consistency::Exact(wrong_token),
    );

    assert!(matches!(
        result,
        Err(ZanzibarError::Consistency(ConsistencyError::WrongDatastore))
    ));
    Ok(())
}

#[test]
fn test_should_reject_expired_revision_token() -> Result<(), Box<dyn std::error::Error>> {
    let mut service =
        ZanzibarService::with_snapshot_retention(NonZeroUsize::new(1).ok_or("invalid retention")?);
    let schema_token = service.add_dsl_with_token(DOC_SCHEMA)?;
    service.write_tuple_with_token(&tuple("readme", "alice"))?;

    let result = service.check_with_consistency(
        &doc_object(),
        &Relation("viewer".to_string()),
        &User::UserId("alice".to_string()),
        Consistency::Exact(schema_token),
    );

    assert!(matches!(
        result,
        Err(ZanzibarError::Consistency(
            ConsistencyError::RevisionExpired { .. }
        ))
    ));
    Ok(())
}

#[test]
fn test_should_reject_future_revision_token() -> Result<(), Box<dyn std::error::Error>> {
    let mut service = ZanzibarService::new();
    service.add_dsl(DOC_SCHEMA)?;
    let token = service.write_tuple_with_token(&tuple("readme", "alice"))?;
    let future = ConsistencyToken::new(
        token.revision().next()?,
        token.schema_hash(),
        token.datastore_id(),
    );

    let result = service.check_with_consistency(
        &doc_object(),
        &Relation("viewer".to_string()),
        &User::UserId("alice".to_string()),
        Consistency::Exact(future),
    );

    assert!(matches!(
        result,
        Err(ZanzibarError::Consistency(
            ConsistencyError::RevisionUnavailable { .. }
        ))
    ));
    Ok(())
}

#[test]
fn test_should_reject_schema_hash_mismatch() -> Result<(), Box<dyn std::error::Error>> {
    let mut service = ZanzibarService::new();
    service.add_dsl(DOC_SCHEMA)?;
    let token = service.write_tuple_with_token(&tuple("readme", "alice"))?;
    let mismatched = ConsistencyToken::from_str(&replace_token_field(
        &token.to_string(),
        2,
        "0000000000000000000000000000000000000000000000000000000000000000",
    )?)?;

    let result = service.check_with_consistency(
        &doc_object(),
        &Relation("viewer".to_string()),
        &User::UserId("alice".to_string()),
        Consistency::Exact(mismatched),
    );

    assert!(matches!(
        result,
        Err(ZanzibarError::Consistency(
            ConsistencyError::SchemaHashMismatch { .. }
        ))
    ));
    Ok(())
}

#[test]
fn test_should_reject_oversized_token_string() {
    let oversized = "x".repeat(123);

    let result = ConsistencyToken::from_str(&oversized);

    assert!(matches!(result, Err(ConsistencyError::InvalidToken { .. })));
}

fn tuple(document: &str, user: &str) -> RelationTuple {
    RelationTuple {
        object: Object {
            namespace: "doc".to_string(),
            id: document.to_string(),
        },
        relation: Relation("viewer".to_string()),
        user: User::UserId(user.to_string()),
    }
}

fn doc_object() -> Object {
    Object {
        namespace: "doc".to_string(),
        id: "readme".to_string(),
    }
}

fn namespace(name: &str, relation: &str) -> NamespaceConfig {
    let relation = Relation(relation.to_string());
    NamespaceConfig {
        name: name.to_string(),
        relations: [(
            relation.clone(),
            RelationConfig {
                name: relation,
                userset_rewrite: None,
            },
        )]
        .into_iter()
        .collect(),
    }
}

fn replace_token_field(
    token: &str,
    field_index: usize,
    replacement: &str,
) -> Result<String, String> {
    let mut parts = token.split(':').map(str::to_string).collect::<Vec<_>>();
    let mut replaced = false;
    for (index, part) in parts.iter_mut().enumerate() {
        if index == field_index {
            *part = replacement.to_string();
            replaced = true;
        }
    }
    if replaced {
        Ok(parts.join(":"))
    } else {
        Err("token field not found".to_string())
    }
}
