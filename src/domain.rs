//! Validated domain primitives for the local Zanzibar engine.

use std::fmt;
use std::str::FromStr;

use crate::model::{Object, Relation, RelationTuple, User};

const MAX_TYPE_BYTES: usize = 64;
const MAX_RELATION_BYTES: usize = 64;
const MAX_ID_BYTES: usize = 256;
const MAX_RELATIONSHIP_BYTES: usize = 768;
const LEGACY_USER_SUBJECT_TYPE: &str = "user";

/// Identifies a kind of validated domain identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdentifierKind {
    /// Object type, also called namespace in the legacy model.
    ObjectType,
    /// Object identifier inside an object type.
    ObjectId,
    /// Relation or permission name.
    RelationName,
    /// Subject type for direct users.
    SubjectType,
    /// Subject identifier inside a subject type.
    SubjectId,
}

impl fmt::Display for IdentifierKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ObjectType => formatter.write_str("object type"),
            Self::ObjectId => formatter.write_str("object id"),
            Self::RelationName => formatter.write_str("relation name"),
            Self::SubjectType => formatter.write_str("subject type"),
            Self::SubjectId => formatter.write_str("subject id"),
        }
    }
}

/// Errors produced while validating domain primitives.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DomainError {
    /// An identifier exceeds the configured byte cap.
    #[error("identifier {kind} exceeds {max_bytes} bytes")]
    IdentifierTooLong {
        /// Identifier category.
        kind: IdentifierKind,
        /// Maximum accepted byte length.
        max_bytes: usize,
    },

    /// An identifier is empty.
    #[error("identifier {kind} must not be empty")]
    EmptyIdentifier {
        /// Identifier category.
        kind: IdentifierKind,
    },

    /// An identifier contains a byte outside its allowlist.
    #[error("identifier {kind} contains invalid byte at offset {offset}")]
    InvalidIdentifierByte {
        /// Identifier category.
        kind: IdentifierKind,
        /// Byte offset of the rejected input byte.
        offset: usize,
    },

    /// A relationship string does not match the accepted grammar.
    #[error("relationship is malformed: {reason}")]
    MalformedRelationship {
        /// Static parse failure reason.
        reason: &'static str,
    },
}

macro_rules! validated_identifier {
    ($name:ident, $kind:expr, $max:expr, $validator:ident) => {
        #[doc = concat!("Validated ", stringify!($name), " domain primitive.")]
        #[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub struct $name(String);

        impl $name {
            #[doc = concat!("Creates a validated ", stringify!($name), ".")]
            ///
            /// # Errors
            ///
            /// Returns [`DomainError`] when the value is empty, too long, or contains bytes outside
            /// the identifier allowlist.
            pub fn new(value: impl Into<String>) -> Result<Self, DomainError> {
                let value = value.into();
                $validator($kind, &value, $max)?;
                Ok(Self(value))
            }

            /// Returns the validated identifier as a string slice.
            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(&self.0)
            }
        }

        impl FromStr for $name {
            type Err = DomainError;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                Self::new(value)
            }
        }

        impl TryFrom<&str> for $name {
            type Error = DomainError;

            fn try_from(value: &str) -> Result<Self, Self::Error> {
                Self::new(value)
            }
        }

        impl TryFrom<String> for $name {
            type Error = DomainError;

            fn try_from(value: String) -> Result<Self, Self::Error> {
                Self::new(value)
            }
        }
    };
}

validated_identifier!(
    ObjectType,
    IdentifierKind::ObjectType,
    MAX_TYPE_BYTES,
    validate_type_identifier
);
validated_identifier!(
    ObjectId,
    IdentifierKind::ObjectId,
    MAX_ID_BYTES,
    validate_id_identifier
);
validated_identifier!(
    RelationName,
    IdentifierKind::RelationName,
    MAX_RELATION_BYTES,
    validate_type_identifier
);
validated_identifier!(
    SubjectType,
    IdentifierKind::SubjectType,
    MAX_TYPE_BYTES,
    validate_type_identifier
);
validated_identifier!(
    SubjectId,
    IdentifierKind::SubjectId,
    MAX_ID_BYTES,
    validate_id_identifier
);

/// A validated object reference.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ObjectRef {
    object_type: ObjectType,
    object_id: ObjectId,
}

impl ObjectRef {
    /// Creates a validated object reference.
    #[must_use]
    pub fn new(object_type: ObjectType, object_id: ObjectId) -> Self {
        Self {
            object_type,
            object_id,
        }
    }

    /// Returns the object type.
    #[must_use]
    pub fn object_type(&self) -> &ObjectType {
        &self.object_type
    }

    /// Returns the object identifier.
    #[must_use]
    pub fn object_id(&self) -> &ObjectId {
        &self.object_id
    }
}

impl fmt::Display for ObjectRef {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}:{}", self.object_type, self.object_id)
    }
}

impl FromStr for ObjectRef {
    type Err = DomainError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let (object_type, object_id) = split_once(value, ':', "object reference must contain ':'")?;
        Ok(Self::new(
            ObjectType::try_from(object_type)?,
            ObjectId::try_from(object_id)?,
        ))
    }
}

impl TryFrom<&Object> for ObjectRef {
    type Error = DomainError;

    fn try_from(value: &Object) -> Result<Self, Self::Error> {
        Ok(Self::new(
            ObjectType::try_from(value.namespace.as_str())?,
            ObjectId::try_from(value.id.as_str())?,
        ))
    }
}

/// A validated subject reference.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum SubjectRef {
    /// A direct subject object, usually `user:<id>`.
    Object(ObjectRef),
    /// A userset subject such as `group:eng#member`.
    Userset {
        /// Userset object.
        object: ObjectRef,
        /// Userset relation.
        relation: RelationName,
    },
}

impl fmt::Display for SubjectRef {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Object(object) => write!(formatter, "{object}"),
            Self::Userset { object, relation } => write!(formatter, "{object}#{relation}"),
        }
    }
}

impl FromStr for SubjectRef {
    type Err = DomainError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.split_once('#') {
            Some((object, relation)) => Ok(Self::Userset {
                object: object.parse()?,
                relation: RelationName::try_from(relation)?,
            }),
            None => Ok(Self::Object(parse_subject_object(value)?)),
        }
    }
}

impl TryFrom<&User> for SubjectRef {
    type Error = DomainError;

    fn try_from(value: &User) -> Result<Self, Self::Error> {
        match value {
            User::UserId(id) => Ok(Self::Object(ObjectRef::new(
                ObjectType::try_from(LEGACY_USER_SUBJECT_TYPE)?,
                ObjectId::try_from(id.as_str())?,
            ))),
            User::Userset(object, relation) => Ok(Self::Userset {
                object: ObjectRef::try_from(object)?,
                relation: RelationName::try_from(relation.0.as_str())?,
            }),
        }
    }
}

/// A validated relationship tuple.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Relationship {
    resource: ObjectRef,
    relation: RelationName,
    subject: SubjectRef,
}

impl Relationship {
    /// Creates a validated relationship from already validated parts.
    #[must_use]
    pub fn new(resource: ObjectRef, relation: RelationName, subject: SubjectRef) -> Self {
        Self {
            resource,
            relation,
            subject,
        }
    }

    /// Returns the relationship resource object.
    #[must_use]
    pub fn resource(&self) -> &ObjectRef {
        &self.resource
    }

    /// Returns the relationship relation.
    #[must_use]
    pub fn relation(&self) -> &RelationName {
        &self.relation
    }

    /// Returns the relationship subject.
    #[must_use]
    pub fn subject(&self) -> &SubjectRef {
        &self.subject
    }
}

impl fmt::Display for Relationship {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{}#{}@{}",
            self.resource, self.relation, self.subject
        )
    }
}

impl FromStr for Relationship {
    type Err = DomainError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value.len() > MAX_RELATIONSHIP_BYTES {
            return Err(DomainError::IdentifierTooLong {
                kind: IdentifierKind::ObjectId,
                max_bytes: MAX_RELATIONSHIP_BYTES,
            });
        }

        let (resource, subject) =
            split_once(value, '@', "relationship must contain one '@' separator")?;
        if subject.contains('@') {
            return Err(DomainError::MalformedRelationship {
                reason: "relationship must contain one '@' separator",
            });
        }

        let (object, relation) =
            split_once(resource, '#', "relationship resource must contain '#'")?;
        if relation.contains('#') {
            return Err(DomainError::MalformedRelationship {
                reason: "relationship resource must contain one '#'",
            });
        }

        Ok(Self::new(
            object.parse()?,
            RelationName::try_from(relation)?,
            subject.parse()?,
        ))
    }
}

impl TryFrom<&RelationTuple> for Relationship {
    type Error = DomainError;

    fn try_from(value: &RelationTuple) -> Result<Self, Self::Error> {
        Ok(Self::new(
            ObjectRef::try_from(&value.object)?,
            RelationName::try_from(value.relation.0.as_str())?,
            SubjectRef::try_from(&value.user)?,
        ))
    }
}

fn parse_subject_object(value: &str) -> Result<ObjectRef, DomainError> {
    let (subject_type, subject_id) = split_once(value, ':', "subject reference must contain ':'")?;
    Ok(ObjectRef::new(
        ObjectType::new(SubjectType::try_from(subject_type)?.as_str())?,
        ObjectId::new(SubjectId::try_from(subject_id)?.as_str())?,
    ))
}

fn split_once<'a>(
    value: &'a str,
    delimiter: char,
    missing_reason: &'static str,
) -> Result<(&'a str, &'a str), DomainError> {
    let (left, right) = value
        .split_once(delimiter)
        .ok_or(DomainError::MalformedRelationship {
            reason: missing_reason,
        })?;
    if left.is_empty() || right.is_empty() {
        return Err(DomainError::MalformedRelationship {
            reason: missing_reason,
        });
    }
    Ok((left, right))
}

fn validate_type_identifier(
    kind: IdentifierKind,
    value: &str,
    max_bytes: usize,
) -> Result<(), DomainError> {
    if value.is_empty() {
        return Err(DomainError::EmptyIdentifier { kind });
    }
    if value.len() > max_bytes {
        return Err(DomainError::IdentifierTooLong { kind, max_bytes });
    }

    for (offset, byte) in value.bytes().enumerate() {
        let valid = if offset == 0 {
            byte.is_ascii_alphabetic()
        } else {
            byte.is_ascii_alphanumeric() || byte == b'_'
        };
        if !valid {
            return Err(DomainError::InvalidIdentifierByte { kind, offset });
        }
    }

    Ok(())
}

fn validate_id_identifier(
    kind: IdentifierKind,
    value: &str,
    max_bytes: usize,
) -> Result<(), DomainError> {
    if value.is_empty() {
        return Err(DomainError::EmptyIdentifier { kind });
    }
    if value.len() > max_bytes {
        return Err(DomainError::IdentifierTooLong { kind, max_bytes });
    }

    for (offset, byte) in value.bytes().enumerate() {
        let valid = byte.is_ascii_graphic()
            && !matches!(byte, b'#' | b'@' | b':' | b'/' | b'\\')
            && !byte.is_ascii_whitespace();
        if !valid {
            return Err(DomainError::InvalidIdentifierByte { kind, offset });
        }
    }

    Ok(())
}

impl TryFrom<&Relation> for RelationName {
    type Error = DomainError;

    fn try_from(value: &Relation) -> Result<Self, Self::Error> {
        Self::try_from(value.0.as_str())
    }
}
