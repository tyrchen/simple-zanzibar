//! Core data structures for the Zanzibar authorization system.

use std::hash::Hash;

use crate::revision::Consistency;

/// Represents a namespaced digital object.
/// e.g., `doc:readme`, `folder:A`
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize, serde::Deserialize),
    serde(rename_all = "camelCase", deny_unknown_fields)
)]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Object {
    /// Object namespace/type.
    pub namespace: String,
    /// Object identifier within the namespace.
    pub id: String,
}

impl Object {
    /// Creates a namespaced object.
    #[must_use]
    pub fn new(namespace: impl Into<String>, id: impl Into<String>) -> Self {
        Self {
            namespace: namespace.into(),
            id: id.into(),
        }
    }
}

/// Represents a relation or permission type on an object.
/// e.g., `owner`, `editor`, `viewer`
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Relation(pub String);

impl Relation {
    /// Creates a relation or permission name.
    #[must_use]
    pub fn new(name: impl Into<String>) -> Self {
        Self(name.into())
    }
}

/// Represents either a specific user ID or a reference to a userset (e.g., a group).
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize, serde::Deserialize),
    serde(rename_all = "camelCase", tag = "type", content = "value")
)]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum User {
    /// A specific user, identified by a unique string.
    /// e.g., `"10"`, `"alice@example.com"`
    UserId(String),
    /// A set of users, identified by an object-relation pair.
    /// e.g., `group:eng#member`
    Userset(Object, Relation),
}

impl User {
    /// Creates a direct user subject.
    #[must_use]
    pub fn user_id(id: impl Into<String>) -> Self {
        Self::UserId(id.into())
    }

    /// Creates a userset subject.
    #[must_use]
    pub fn userset(object: Object, relation: Relation) -> Self {
        Self::Userset(object, relation)
    }
}

/// The core relation tuple, representing a single permission assertion.
/// This is the atomic unit of authorization data.
/// e.g., `(doc:readme#owner@user:alice)`
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize, serde::Deserialize),
    serde(rename_all = "camelCase", deny_unknown_fields)
)]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RelationTuple {
    /// Relationship resource object.
    pub object: Object,
    /// Relationship relation.
    pub relation: Relation,
    /// Relationship subject.
    pub user: User,
}

impl RelationTuple {
    /// Creates a relationship tuple.
    #[must_use]
    pub fn new(object: Object, relation: Relation, user: User) -> Self {
        Self {
            object,
            relation,
            user,
        }
    }
}

/// Request for a check at a specified consistency level.
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize, serde::Deserialize),
    serde(rename_all = "camelCase", deny_unknown_fields)
)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckRequest {
    /// Protected object to evaluate.
    pub object: Object,
    /// Relation or permission to evaluate.
    pub relation: Relation,
    /// Subject whose membership is checked.
    pub user: User,
    /// Consistency selector for the read.
    pub consistency: Consistency,
}

impl CheckRequest {
    /// Creates a check request.
    #[must_use]
    pub fn new(object: Object, relation: Relation, user: User, consistency: Consistency) -> Self {
        Self {
            object,
            relation,
            user,
            consistency,
        }
    }
}

/// Response for a check request.
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize, serde::Deserialize),
    serde(rename_all = "camelCase", deny_unknown_fields)
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CheckResponse {
    /// Whether the subject has the requested relation or permission.
    pub allowed: bool,
}

/// Request for expanding an object relation or permission at a specified consistency level.
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize, serde::Deserialize),
    serde(rename_all = "camelCase", deny_unknown_fields)
)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExpandRequest {
    /// Protected object to expand.
    pub object: Object,
    /// Relation or permission to expand.
    pub relation: Relation,
    /// Consistency selector for the read.
    pub consistency: Consistency,
}

impl ExpandRequest {
    /// Creates an expand request.
    #[must_use]
    pub fn new(object: Object, relation: Relation, consistency: Consistency) -> Self {
        Self {
            object,
            relation,
            consistency,
        }
    }
}

/// Response for an expand request.
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize, serde::Deserialize),
    serde(rename_all = "camelCase", deny_unknown_fields)
)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExpandResponse {
    /// Expanded userset tree.
    pub expanded: ExpandedUserset,
}

/// Request for resources of one type that a subject can access through a permission.
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize, serde::Deserialize),
    serde(rename_all = "camelCase", deny_unknown_fields)
)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LookupResourcesRequest {
    /// Subject whose accessible resources are requested.
    pub subject: User,
    /// Permission or relation to check on each candidate resource.
    pub permission: Relation,
    /// Resource namespace/type to return.
    pub resource_type: String,
}

/// Resources returned by a lookup request.
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize, serde::Deserialize),
    serde(rename_all = "camelCase", deny_unknown_fields)
)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LookupResources {
    /// De-duplicated resources that passed the shared check evaluator.
    pub resources: Vec<Object>,
}

/// Request for subjects of one type that can access a resource through a permission.
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize, serde::Deserialize),
    serde(rename_all = "camelCase", deny_unknown_fields)
)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LookupSubjectsRequest {
    /// Protected resource to check.
    pub resource: Object,
    /// Permission or relation to evaluate on the resource.
    pub permission: Relation,
    /// Subject namespace/type to return.
    pub subject_type: String,
}

/// Subjects returned by a lookup request.
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize, serde::Deserialize),
    serde(rename_all = "camelCase", deny_unknown_fields)
)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LookupSubjects {
    /// De-duplicated subjects that passed the shared check evaluator.
    pub subjects: Vec<User>,
}

/// Request for all permissions a subject has on one resource.
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize, serde::Deserialize),
    serde(rename_all = "camelCase", deny_unknown_fields)
)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LookupPermissionsRequest {
    /// Subject whose permissions are requested.
    pub subject: User,
    /// Resource object to evaluate.
    pub resource: Object,
    /// Consistency selector for the read.
    pub consistency: Consistency,
}

/// Permissions returned by a subject/resource lookup request.
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize, serde::Deserialize),
    serde(rename_all = "camelCase", deny_unknown_fields)
)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LookupPermissions {
    /// Sorted relations or permissions that evaluated to allowed.
    pub permissions: Vec<Relation>,
}

/// Request for subjects grouped by every permission they have on one resource.
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize, serde::Deserialize),
    serde(rename_all = "camelCase", deny_unknown_fields)
)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LookupObjectPermissionsRequest {
    /// Resource object to evaluate.
    pub resource: Object,
    /// Subject namespace/type to return.
    pub subject_type: String,
    /// Consistency selector for the read.
    pub consistency: Consistency,
}

/// Subjects grouped by permission for one resource.
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize, serde::Deserialize),
    serde(rename_all = "camelCase", deny_unknown_fields)
)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LookupObjectPermissions {
    /// Permission groups with non-empty subjects.
    pub permissions: Vec<PermissionSubjects>,
}

/// Subjects that have one permission on a resource.
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize, serde::Deserialize),
    serde(rename_all = "camelCase", deny_unknown_fields)
)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PermissionSubjects {
    /// Relation or permission name.
    pub permission: Relation,
    /// Subjects that passed the shared check evaluator.
    pub subjects: Vec<User>,
}

/// Defines the schema and policy rules for a particular namespace.
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize, serde::Deserialize),
    serde(rename_all = "camelCase", deny_unknown_fields)
)]
#[derive(Debug, Clone, Default)]
pub struct NamespaceConfig {
    /// Namespace/type name.
    pub name: String,
    /// Relation definitions keyed by relation name.
    pub relations: std::collections::HashMap<Relation, RelationConfig>,
}

/// Defines a specific relation within a namespace, including its rewrite rules.
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize, serde::Deserialize),
    serde(rename_all = "camelCase", deny_unknown_fields)
)]
#[derive(Debug, Clone)]
pub struct RelationConfig {
    /// Relation name.
    pub name: Relation,
    /// Optional userset rewrite for computed permissions.
    pub userset_rewrite: Option<UsersetExpression>,
}

/// Represents a tree of userset computations, forming the core of the policy language.
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize, serde::Deserialize),
    serde(rename_all = "camelCase")
)]
#[derive(Debug, Clone)]
pub enum UsersetExpression {
    /// `this` - The set of users directly granted this relation.
    This,
    /// A set computed from another relation on the *same* object.
    /// e.g., an `editor` is also a `viewer`.
    ComputedUserset {
        /// Relation on the same object to compute.
        relation: Relation,
    },
    /// A set computed by first finding a related object via a `tupleset` relation,
    /// and then computing a userset from that related object.
    /// e.g., for `doc:readme`, find its `parent` folder, then take `viewers` of that folder.
    TupleToUserset {
        /// Relation that points from the source object to intermediate objects.
        tupleset_relation: Relation,
        /// Relation to evaluate on each intermediate object.
        computed_userset_relation: Relation,
    },
    /// The union of multiple sub-expressions.
    Union(Vec<UsersetExpression>),
    /// The intersection of multiple sub-expressions.
    Intersection(Vec<UsersetExpression>),
    /// The exclusion (or difference) of one set from another.
    Exclusion {
        /// Base userset expression.
        base: Box<UsersetExpression>,
        /// Userset expression to subtract from the base.
        exclude: Box<UsersetExpression>,
    },
}

/// Represents the result of an `expand` operation, detailing the effective userset.
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize, serde::Deserialize),
    serde(rename_all = "camelCase")
)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExpandedUserset {
    /// A specific user who has the permission.
    User(String),
    /// A reference to another userset that contributes to the permission.
    Userset(Object, Relation),
    /// The union of multiple expanded usersets.
    Union(Vec<ExpandedUserset>),
    /// The intersection of multiple expanded usersets.
    Intersection(Vec<ExpandedUserset>),
    /// The exclusion of one expanded userset from another.
    Exclusion {
        /// Base expanded userset.
        base: Box<ExpandedUserset>,
        /// Expanded userset to subtract from the base.
        exclude: Box<ExpandedUserset>,
    },
}
