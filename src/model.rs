//! Core data structures for the Zanzibar authorization system.

use std::hash::Hash;

/// Represents a namespaced digital object.
/// e.g., `doc:readme`, `folder:A`
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Object {
    pub namespace: String,
    pub id: String,
}

/// Represents a relation or permission type on an object.
/// e.g., `owner`, `editor`, `viewer`
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Relation(pub String);

/// Represents either a specific user ID or a reference to a userset (e.g., a group).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum User {
    /// A specific user, identified by a unique string.
    /// e.g., `"10"`, `"alice@example.com"`
    UserId(String),
    /// A set of users, identified by an object-relation pair.
    /// e.g., `group:eng#member`
    Userset(Object, Relation),
}

/// The core relation tuple, representing a single permission assertion.
/// This is the atomic unit of authorization data.
/// e.g., `(doc:readme#owner@user:alice)`
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RelationTuple {
    pub object: Object,
    pub relation: Relation,
    pub user: User,
}

/// Defines the schema and policy rules for a particular namespace.
#[derive(Debug, Clone, Default)]
pub struct NamespaceConfig {
    pub name: String,
    pub relations: std::collections::HashMap<Relation, RelationConfig>,
}

/// Defines a specific relation within a namespace, including its rewrite rules.
#[derive(Debug, Clone)]
pub struct RelationConfig {
    pub name: Relation,
    pub userset_rewrite: Option<UsersetExpression>,
}

/// Represents a tree of userset computations, forming the core of the policy language.
#[derive(Debug, Clone)]
pub enum UsersetExpression {
    /// `this` - The set of users directly granted this relation.
    This,
    /// A set computed from another relation on the *same* object.
    /// e.g., an `editor` is also a `viewer`.
    ComputedUserset { relation: Relation },
    /// A set computed by first finding a related object via a `tupleset` relation,
    /// and then computing a userset from that related object.
    /// e.g., for `doc:readme`, find its `parent` folder, then take `viewers` of that folder.
    TupleToUserset {
        tupleset_relation: Relation,
        computed_userset_relation: Relation,
    },
    /// The union of multiple sub-expressions.
    Union(Vec<UsersetExpression>),
    /// The intersection of multiple sub-expressions.
    Intersection(Vec<UsersetExpression>),
    /// The exclusion (or difference) of one set from another.
    Exclusion {
        base: Box<UsersetExpression>,
        exclude: Box<UsersetExpression>,
    },
}

/// Represents the result of an `expand` operation, detailing the effective userset.
#[derive(Debug, PartialEq)]
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
        base: Box<ExpandedUserset>,
        exclude: Box<ExpandedUserset>,
    },
}
