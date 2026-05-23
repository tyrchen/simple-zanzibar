use std::sync::Arc;

use simple_zanzibar::ZanzibarService;
use simple_zanzibar::domain::ObjectType;
use simple_zanzibar::error::ZanzibarError;
use simple_zanzibar::schema::{
    self, CompiledSchema, NamespaceDefinition, RelationDefinition, SchemaError, UsersetExpression,
};

const VALID_SCHEMA: &str = r#"
    namespace doc {
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

    namespace folder {
        relation viewer {}
    }
"#;

#[test]
fn test_should_compile_legacy_dsl_to_schema_ir() -> Result<(), ZanzibarError> {
    let compiled = schema::compile_legacy_dsl(VALID_SCHEMA)?;
    let doc_type = "doc".try_into()?;
    let viewer = "viewer".try_into()?;

    let relation = compiled.resolver().relation(&doc_type, &viewer)?;
    assert_eq!(relation.name().as_str(), "viewer");
    assert!(relation.userset_rewrite().is_some());

    Ok(())
}

#[test]
fn test_should_reject_duplicate_namespace_definitions() {
    let dsl = r"
        namespace doc {
            relation viewer {}
        }

        namespace doc {
            relation owner {}
        }
    ";

    let error = schema::compile_legacy_dsl(dsl).err();
    assert!(matches!(
        error,
        Some(ZanzibarError::Schema(SchemaError::DuplicateNamespace { namespace }))
            if namespace == "doc"
    ));
}

#[test]
fn test_should_reject_duplicate_relation_definitions() {
    let dsl = r"
        namespace doc {
            relation viewer {}
            relation viewer {}
        }
    ";

    let error = schema::compile_legacy_dsl(dsl).err();
    assert!(matches!(
        error,
        Some(ZanzibarError::Schema(SchemaError::DuplicateRelation { namespace, relation }))
            if namespace == "doc" && relation == "viewer"
    ));
}

#[test]
fn test_should_reject_missing_computed_userset_relation() {
    let dsl = r#"
        namespace doc {
            relation viewer {
                rewrite computed_userset(relation: "owner")
            }
        }
    "#;

    let error = schema::compile_legacy_dsl(dsl).err();
    assert!(matches!(
        error,
        Some(ZanzibarError::Schema(
            SchemaError::MissingRelationReference { missing, .. }
        )) if missing == "owner"
    ));
}

#[test]
fn test_should_reject_missing_tuple_to_userset_left_relation() {
    let dsl = r#"
        namespace doc {
            relation viewer {
                rewrite tuple_to_userset(tupleset: "parent", computed_userset: "viewer")
            }
        }
    "#;

    let error = schema::compile_legacy_dsl(dsl).err();
    assert!(matches!(
        error,
        Some(ZanzibarError::Schema(
            SchemaError::MissingRelationReference { missing, .. }
        )) if missing == "parent"
    ));
}

#[test]
fn test_should_reject_missing_tuple_to_userset_target_relation() {
    let dsl = r#"
        namespace doc {
            relation parent {}
            relation viewer {
                rewrite tuple_to_userset(tupleset: "parent", computed_userset: "missing")
            }
        }
    "#;

    let error = schema::compile_legacy_dsl(dsl).err();
    assert!(matches!(
        error,
        Some(ZanzibarError::Schema(
            SchemaError::MissingTupleToUsersetTarget { missing, .. }
        )) if missing == "missing"
    ));
}

#[test]
fn test_should_validate_tuple_to_userset_target_against_explicit_allowed_subjects()
-> Result<(), ZanzibarError> {
    let doc_type = ObjectType::try_from("doc")?;
    let folder_type = ObjectType::try_from("folder")?;
    let group_type = ObjectType::try_from("group")?;

    let doc_namespace = NamespaceDefinition::new(
        doc_type,
        Arc::from(
            vec![
                RelationDefinition::with_allowed_subject_types(
                    "parent".try_into()?,
                    Arc::from(vec![folder_type.clone()].into_boxed_slice()),
                    None,
                ),
                RelationDefinition::new(
                    "viewer".try_into()?,
                    Some(UsersetExpression::TupleToUserset {
                        tupleset_relation: "parent".try_into()?,
                        computed_userset_relation: "viewer".try_into()?,
                    }),
                ),
            ]
            .into_boxed_slice(),
        ),
    );
    let folder_namespace = NamespaceDefinition::new(
        folder_type,
        Arc::from(vec![RelationDefinition::new("viewer".try_into()?, None)].into_boxed_slice()),
    );
    let unrelated_namespace = NamespaceDefinition::new(
        group_type,
        Arc::from(vec![RelationDefinition::new("member".try_into()?, None)].into_boxed_slice()),
    );

    let compiled =
        CompiledSchema::from_definitions([doc_namespace, folder_namespace, unrelated_namespace])?;
    let doc = "doc".try_into()?;
    let viewer = "viewer".try_into()?;

    assert!(compiled.resolver().relation(&doc, &viewer).is_ok());

    Ok(())
}

#[test]
fn test_should_reject_tuple_to_userset_target_missing_on_explicit_allowed_subject()
-> Result<(), ZanzibarError> {
    let doc_type = ObjectType::try_from("doc")?;
    let folder_type = ObjectType::try_from("folder")?;
    let group_type = ObjectType::try_from("group")?;

    let doc_namespace = NamespaceDefinition::new(
        doc_type,
        Arc::from(
            vec![
                RelationDefinition::with_allowed_subject_types(
                    "parent".try_into()?,
                    Arc::from(vec![folder_type.clone()].into_boxed_slice()),
                    None,
                ),
                RelationDefinition::new(
                    "viewer".try_into()?,
                    Some(UsersetExpression::TupleToUserset {
                        tupleset_relation: "parent".try_into()?,
                        computed_userset_relation: "member".try_into()?,
                    }),
                ),
            ]
            .into_boxed_slice(),
        ),
    );
    let folder_namespace = NamespaceDefinition::new(
        folder_type,
        Arc::from(vec![RelationDefinition::new("viewer".try_into()?, None)].into_boxed_slice()),
    );
    let unrelated_namespace = NamespaceDefinition::new(
        group_type,
        Arc::from(vec![RelationDefinition::new("member".try_into()?, None)].into_boxed_slice()),
    );

    let error =
        CompiledSchema::from_definitions([doc_namespace, folder_namespace, unrelated_namespace])
            .err();
    assert!(matches!(
        error,
        Some(SchemaError::MissingTupleToUsersetTarget { missing, .. }) if missing == "member"
    ));

    Ok(())
}

#[test]
fn test_should_reject_relationship_with_unknown_userset_subject_relation()
-> Result<(), ZanzibarError> {
    let mut service = ZanzibarService::new();
    service.add_dsl(VALID_SCHEMA)?;

    let tuple = simple_zanzibar::model::RelationTuple {
        object: simple_zanzibar::model::Object {
            namespace: "doc".to_string(),
            id: "readme".to_string(),
        },
        relation: simple_zanzibar::model::Relation("parent".to_string()),
        user: simple_zanzibar::model::User::Userset(
            simple_zanzibar::model::Object {
                namespace: "folder".to_string(),
                id: "root".to_string(),
            },
            simple_zanzibar::model::Relation("missing".to_string()),
        ),
    };

    let error = service.write_tuple(tuple).err();
    assert!(matches!(
        error,
        Some(ZanzibarError::Schema(SchemaError::RelationNotFound { relation, .. }))
            if relation == "missing"
    ));

    Ok(())
}

#[test]
fn test_should_reject_empty_union() {
    let dsl = r"
        namespace doc {
            relation viewer {
                rewrite union()
            }
        }
    ";

    let error = schema::compile_legacy_dsl(dsl).err();
    assert!(matches!(
        error,
        Some(ZanzibarError::Schema(SchemaError::EmptyExpression {
            operator: "union",
            ..
        }))
    ));
}

#[test]
fn test_service_should_reject_invalid_schema_without_mutating_previous_schema()
-> Result<(), ZanzibarError> {
    let mut service = ZanzibarService::new();
    service.add_dsl(VALID_SCHEMA)?;

    let invalid = r#"
        namespace invalid {
            relation viewer {
                rewrite computed_userset(relation: "missing")
            }
        }
    "#;

    assert!(service.add_dsl(invalid).is_err());

    let doc = simple_zanzibar::model::Object {
        namespace: "doc".to_string(),
        id: "readme".to_string(),
    };
    let viewer = simple_zanzibar::model::Relation("viewer".to_string());
    let alice = simple_zanzibar::model::User::UserId("alice".to_string());

    assert!(!service.check(&doc, &viewer, &alice)?);

    Ok(())
}
