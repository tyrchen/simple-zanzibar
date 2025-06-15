# Technical Context: Simplified Rust Zanzibar

This document outlines the technical design of the simplified Zanzibar implementation.

## Core Data Structures (Rust)
- **`Object`**: Represents a namespaced digital object.
  - `struct Object { namespace: String, id: String }`
- **`Relation`**: Represents a type of permission.
  - `struct Relation(pub String);`
- **`User`**: Represents a user or a userset.
  - `enum User { UserId(String), Userset(Object, Relation) }`
- **`RelationTuple`**: The atomic unit of permission.
  - `struct RelationTuple { object: Object, relation: Relation, user: User }`
- **`NamespaceConfig`**: Defines the schema and policy rules for a namespace.
  - `struct NamespaceConfig { name: String, relations: HashMap<Relation, RelationConfig> }`
- **`RelationConfig`**: Defines a specific relation within a namespace.
  - `struct RelationConfig { name: Relation, userset_rewrite: Option<UsersetExpression> }`
- **`UsersetExpression`**: Defines how a userset is computed through rewrite rules.
  - `enum UsersetExpression { This, ComputedUserset { ... }, TupleToUserset { ... }, Union(...), Intersection(...), Exclusion { ... } }`

## Core API
- **`TupleStore` Trait**: An abstraction for storing and retrieving relation tuples.
  - `fn read_tuples(...) -> Vec<RelationTuple>`
  - `fn write_tuple(...) -> Result<(), String>`
  - `fn delete_tuple(...) -> Result<(), String>`
  - An initial `InMemoryTupleStore` will be implemented using a `HashSet`.
- **`check` Function**: The primary authorization decision point.
  - `pub fn check(object: &Object, relation: &Relation, user: &User, config: &NamespaceConfig, store: &impl TupleStore) -> bool`
- **`expand` Function**: Returns the effective userset for a given permission.
  - `pub fn expand(object: &Object, relation: &Relation, config: &NamespaceConfig, store: &impl TupleStore) -> ExpandedUserset`

## DSL (Domain-Specific Language)
A text-based language will be used to define `NamespaceConfig` and `RelationConfig` structures. The DSL will support keywords like `namespace`, `relation`, `rewrite`, and operators like `union`, `intersection`, and `exclusion` to mirror the `UsersetExpression` structure. It will be parsed into the core Rust data structures.
