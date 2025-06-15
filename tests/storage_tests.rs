use simple_zanzibar::model::{Object, Relation, RelationTuple, User};
use simple_zanzibar::store::{InMemoryTupleStore, TupleStore};

#[test]
fn test_write_and_read_tuple() {
    let mut store = InMemoryTupleStore::default();

    let tuple = RelationTuple {
        object: Object {
            namespace: "doc".to_string(),
            id: "readme".to_string(),
        },
        relation: Relation("owner".to_string()),
        user: User::UserId("alice".to_string()),
    };

    assert!(store.write_tuple(tuple.clone()).is_ok());

    let results = store.read_tuples(&tuple.object, Some(&tuple.relation), Some(&tuple.user));
    assert_eq!(results.len(), 1);
    assert_eq!(results[0], tuple);
}

#[test]
fn test_write_duplicate_tuple() {
    let mut store = InMemoryTupleStore::default();
    let tuple = RelationTuple {
        object: Object {
            namespace: "doc".to_string(),
            id: "readme".to_string(),
        },
        relation: Relation("owner".to_string()),
        user: User::UserId("alice".to_string()),
    };
    assert!(store.write_tuple(tuple.clone()).is_ok());
    assert!(store.write_tuple(tuple).is_err());
}

#[test]
fn test_delete_tuple() {
    let mut store = InMemoryTupleStore::default();
    let tuple = RelationTuple {
        object: Object {
            namespace: "doc".to_string(),
            id: "readme".to_string(),
        },
        relation: Relation("owner".to_string()),
        user: User::UserId("alice".to_string()),
    };

    assert!(store.write_tuple(tuple.clone()).is_ok());
    assert!(store.delete_tuple(&tuple).is_ok());

    let results = store.read_tuples(&tuple.object, Some(&tuple.relation), Some(&tuple.user));
    assert!(results.is_empty());
}

#[test]
fn test_delete_nonexistent_tuple() {
    let mut store = InMemoryTupleStore::default();
    let tuple = RelationTuple {
        object: Object {
            namespace: "doc".to_string(),
            id: "readme".to_string(),
        },
        relation: Relation("owner".to_string()),
        user: User::UserId("alice".to_string()),
    };
    assert!(store.delete_tuple(&tuple).is_err());
}
