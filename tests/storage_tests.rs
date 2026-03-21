//! Tests for the tuple store.

use simple_zanzibar::{
    model::{Object, Relation, RelationTuple, User},
    store::{InMemoryTupleStore, TupleStore},
};

#[test]
fn test_should_write_and_read_tuple() {
    let mut store = InMemoryTupleStore::default();

    let tuple = RelationTuple::new(
        Object::new("doc", "readme"),
        Relation::new("owner"),
        User::user_id("alice"),
    );

    assert!(store.write_tuple(tuple.clone()).is_ok());

    let results = store.read_tuples(&tuple.object, Some(&tuple.relation), Some(&tuple.user));
    assert_eq!(results.len(), 1);
    assert_eq!(results[0], tuple);
}

#[test]
fn test_should_reject_duplicate_tuple() {
    let mut store = InMemoryTupleStore::default();
    let tuple = RelationTuple::new(
        Object::new("doc", "readme"),
        Relation::new("owner"),
        User::user_id("alice"),
    );
    assert!(store.write_tuple(tuple.clone()).is_ok());
    assert!(store.write_tuple(tuple).is_err());
}

#[test]
fn test_should_delete_tuple() {
    let mut store = InMemoryTupleStore::default();
    let tuple = RelationTuple::new(
        Object::new("doc", "readme"),
        Relation::new("owner"),
        User::user_id("alice"),
    );

    assert!(store.write_tuple(tuple.clone()).is_ok());
    assert!(store.delete_tuple(&tuple).is_ok());

    let results = store.read_tuples(&tuple.object, Some(&tuple.relation), Some(&tuple.user));
    assert!(results.is_empty());
}

#[test]
fn test_should_reject_delete_nonexistent_tuple() {
    let mut store = InMemoryTupleStore::default();
    let tuple = RelationTuple::new(
        Object::new("doc", "readme"),
        Relation::new("owner"),
        User::user_id("alice"),
    );
    assert!(store.delete_tuple(&tuple).is_err());
}
