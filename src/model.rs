//! Core data structures for the Zanzibar authorization system.
//!
//! This module defines the fundamental types used to represent objects,
//! relations, users, relation tuples, and namespace configurations.

use std::{collections::HashMap, hash::Hash};

/// Represents a namespaced digital object (e.g., `doc:readme`, `folder:A`).
///
/// # Examples
///
/// ```
/// use simple_zanzibar::model::Object;
///
/// let doc = Object::new("doc", "readme");
/// assert_eq!(doc.namespace, "doc");
/// assert_eq!(doc.id, "readme");
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub struct Object {
    /// The namespace this object belongs to (e.g., "doc", "folder").
    pub namespace: String,
    /// The unique identifier within the namespace (e.g., "readme", "A").
    pub id: String,
}

impl Object {
    /// Creates a new object with the given namespace and id.
    pub fn new(namespace: impl Into<String>, id: impl Into<String>) -> Self {
        Self {
            namespace: namespace.into(),
            id: id.into(),
        }
    }
}

/// Represents a relation or permission type on an object (e.g., `owner`, `editor`, `viewer`).
///
/// # Examples
///
/// ```
/// use simple_zanzibar::model::Relation;
///
/// let rel = Relation::new("viewer");
/// assert_eq!(rel.0, "viewer");
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub struct Relation(pub String);

impl Relation {
    /// Creates a new relation with the given name.
    pub fn new(name: impl Into<String>) -> Self {
        Self(name.into())
    }
}

/// Represents either a specific user ID or a reference to a userset (e.g., a group).
///
/// # Examples
///
/// ```
/// use simple_zanzibar::model::{Object, Relation, User};
///
/// let alice = User::user_id("alice");
/// let group_members = User::userset(Object::new("group", "eng"), Relation::new("member"));
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum User {
    /// A specific user, identified by a unique string (e.g., `"alice@example.com"`).
    UserId(String),
    /// A set of users, identified by an object-relation pair (e.g., `group:eng#member`).
    Userset(Object, Relation),
}

impl User {
    /// Creates a [`User::UserId`] variant.
    pub fn user_id(id: impl Into<String>) -> Self {
        Self::UserId(id.into())
    }

    /// Creates a [`User::Userset`] variant.
    pub fn userset(object: Object, relation: Relation) -> Self {
        Self::Userset(object, relation)
    }
}

/// The core relation tuple, representing a single permission assertion.
///
/// This is the atomic unit of authorization data
/// (e.g., `doc:readme#owner@user:alice`).
///
/// # Examples
///
/// ```
/// use simple_zanzibar::model::{Object, Relation, RelationTuple, User};
///
/// let tuple = RelationTuple::new(
///     Object::new("doc", "readme"),
///     Relation::new("owner"),
///     User::user_id("alice"),
/// );
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub struct RelationTuple {
    /// The object this tuple applies to.
    pub object: Object,
    /// The relation being asserted.
    pub relation: Relation,
    /// The user (or userset) being granted the relation.
    pub user: User,
}

impl RelationTuple {
    /// Creates a new relation tuple.
    pub fn new(object: Object, relation: Relation, user: User) -> Self {
        Self {
            object,
            relation,
            user,
        }
    }
}

/// Defines the schema and policy rules for a particular namespace.
///
/// # Examples
///
/// ```
/// use simple_zanzibar::model::{NamespaceConfig, Relation, RelationConfig, UsersetExpression};
///
/// let config = NamespaceConfig::new("doc")
///     .with_relation(RelationConfig::new(Relation::new("owner")))
///     .with_relation(
///         RelationConfig::new(Relation::new("viewer"))
///             .with_rewrite(UsersetExpression::Union(vec![
///                 UsersetExpression::This,
///                 UsersetExpression::ComputedUserset {
///                     relation: Relation::new("owner"),
///                 },
///             ])),
///     );
/// ```
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct NamespaceConfig {
    /// The name of the namespace.
    pub name: String,
    /// The relations defined in this namespace, keyed by relation.
    pub relations: HashMap<Relation, RelationConfig>,
}

impl NamespaceConfig {
    /// Creates a new namespace configuration with no relations.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            relations: HashMap::new(),
        }
    }

    /// Adds a relation configuration, returning self for chaining.
    pub fn with_relation(mut self, config: RelationConfig) -> Self {
        self.relations.insert(config.name.clone(), config);
        self
    }
}

/// Defines a specific relation within a namespace, including its rewrite rules.
///
/// # Examples
///
/// ```
/// use simple_zanzibar::model::{Relation, RelationConfig, UsersetExpression};
///
/// let config = RelationConfig::new(Relation::new("viewer"))
///     .with_rewrite(UsersetExpression::This);
/// ```
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct RelationConfig {
    /// The name of this relation.
    pub name: Relation,
    /// The optional userset rewrite expression for computing effective permissions.
    pub userset_rewrite: Option<UsersetExpression>,
}

impl RelationConfig {
    /// Creates a new relation configuration with no rewrite rule.
    pub fn new(name: Relation) -> Self {
        Self {
            name,
            userset_rewrite: None,
        }
    }

    /// Sets the userset rewrite expression, returning self for chaining.
    pub fn with_rewrite(mut self, rewrite: UsersetExpression) -> Self {
        self.userset_rewrite = Some(rewrite);
        self
    }
}

/// Represents a tree of userset computations, forming the core of the policy language.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum UsersetExpression {
    /// `this` - The set of users directly granted this relation.
    This,
    /// A set computed from another relation on the *same* object
    /// (e.g., an `editor` is also a `viewer`).
    ComputedUserset {
        /// The relation to compute from.
        relation: Relation,
    },
    /// A set computed by first finding a related object via a `tupleset` relation,
    /// and then computing a userset from that related object
    /// (e.g., for `doc:readme`, find its `parent` folder, then take `viewers` of that folder).
    TupleToUserset {
        /// The relation used to find the intermediate object.
        tupleset_relation: Relation,
        /// The relation to compute on the intermediate object.
        computed_userset_relation: Relation,
    },
    /// The union of multiple sub-expressions.
    Union(Vec<UsersetExpression>),
    /// The intersection of multiple sub-expressions.
    Intersection(Vec<UsersetExpression>),
    /// The exclusion (or difference) of one set from another.
    Exclusion {
        /// The base set.
        base: Box<UsersetExpression>,
        /// The set to exclude from the base.
        exclude: Box<UsersetExpression>,
    },
}

/// Represents the result of an `expand` operation, detailing the effective userset.
#[derive(Debug, PartialEq)]
#[non_exhaustive]
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
        /// The base set.
        base: Box<ExpandedUserset>,
        /// The set excluded from the base.
        exclude: Box<ExpandedUserset>,
    },
}
