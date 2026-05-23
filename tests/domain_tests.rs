use std::str::FromStr;

use simple_zanzibar::domain::{DomainError, IdentifierKind, ObjectType, Relationship, SubjectRef};

#[test]
fn test_should_parse_direct_relationship() -> Result<(), DomainError> {
    let relationship = Relationship::from_str("doc:readme#viewer@user:alice")?;

    assert_eq!(relationship.resource().object_type().as_str(), "doc");
    assert_eq!(relationship.resource().object_id().as_str(), "readme");
    assert_eq!(relationship.relation().as_str(), "viewer");
    assert_eq!(relationship.to_string(), "doc:readme#viewer@user:alice");

    Ok(())
}

#[test]
fn test_should_parse_userset_relationship() -> Result<(), DomainError> {
    let relationship = Relationship::from_str("doc:readme#viewer@group:eng#member")?;

    assert!(matches!(
        relationship.subject(),
        SubjectRef::Userset { object, relation }
            if object.object_type().as_str() == "group"
                && object.object_id().as_str() == "eng"
                && relation.as_str() == "member"
    ));

    Ok(())
}

#[test]
fn test_should_reject_invalid_type_identifier() {
    let error = ObjectType::try_from("_doc").err();
    assert!(matches!(
        error,
        Some(DomainError::InvalidIdentifierByte {
            kind: IdentifierKind::ObjectType,
            offset: 0,
        })
    ));
}

#[test]
fn test_should_reject_invalid_object_id_bytes() {
    let error = Relationship::from_str("doc:read/me#viewer@user:alice").err();
    assert!(matches!(
        error,
        Some(DomainError::InvalidIdentifierByte {
            kind: IdentifierKind::ObjectId,
            ..
        })
    ));
}

#[test]
fn test_should_reject_malformed_relationship() {
    let error = Relationship::from_str("doc:readme#viewer").err();
    assert!(matches!(
        error,
        Some(DomainError::MalformedRelationship { .. })
    ));
}
