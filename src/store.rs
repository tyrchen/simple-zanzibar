//! Defines the storage abstraction for relation tuples.
//!
//! The [`TupleStore`] trait provides the interface for storing and retrieving
//! [`RelationTuple`]s, while [`InMemoryTupleStore`] offers a simple indexed
//! in-memory implementation.

use std::collections::HashMap;

use crate::{
    error::StoreError,
    model::{Object, Relation, RelationTuple, User},
};

/// A trait for abstracting the storage and retrieval of [`RelationTuple`]s.
///
/// This allows the core logic to be decoupled from the specific storage backend.
/// Implementations must also implement [`Debug`] for diagnostic purposes.
pub trait TupleStore: std::fmt::Debug {
    /// Reads tuples from the store, with optional filtering.
    ///
    /// # Arguments
    ///
    /// * `object` - The object to filter by.
    /// * `relation` - An optional relation to filter by.
    /// * `user` - An optional user to filter by.
    fn read_tuples(
        &self,
        object: &Object,
        relation: Option<&Relation>,
        user: Option<&User>,
    ) -> Vec<RelationTuple>;

    /// Writes a single tuple to the store.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::DuplicateTuple`] if the tuple already exists.
    fn write_tuple(&mut self, tuple: RelationTuple) -> Result<(), StoreError>;

    /// Deletes a single tuple from the store.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::TupleNotFound`] if the tuple does not exist.
    fn delete_tuple(&mut self, tuple: &RelationTuple) -> Result<(), StoreError>;
}

/// A simple, in-memory implementation of the [`TupleStore`] trait.
///
/// Tuples are indexed by [`Object`] for efficient lookup, avoiding full scans
/// on every read operation.
///
/// # Examples
///
/// ```
/// use simple_zanzibar::store::{InMemoryTupleStore, TupleStore};
/// use simple_zanzibar::model::{Object, Relation, RelationTuple, User};
///
/// let mut store = InMemoryTupleStore::default();
/// let tuple = RelationTuple::new(
///     Object::new("doc", "readme"),
///     Relation::new("owner"),
///     User::user_id("alice"),
/// );
/// store.write_tuple(tuple).unwrap();
/// ```
#[derive(Debug, Default)]
pub struct InMemoryTupleStore {
    /// Tuples indexed by object for O(1) object lookup.
    index: HashMap<Object, Vec<RelationTuple>>,
}

impl TupleStore for InMemoryTupleStore {
    fn read_tuples(
        &self,
        object: &Object,
        relation: Option<&Relation>,
        user: Option<&User>,
    ) -> Vec<RelationTuple> {
        let Some(tuples) = self.index.get(object) else {
            return Vec::new();
        };
        tuples
            .iter()
            .filter(|t| {
                relation.is_none_or(|r| t.relation == *r) && user.is_none_or(|u| t.user == *u)
            })
            .cloned()
            .collect()
    }

    fn write_tuple(&mut self, tuple: RelationTuple) -> Result<(), StoreError> {
        let tuples = self.index.entry(tuple.object.clone()).or_default();
        if tuples.contains(&tuple) {
            return Err(StoreError::DuplicateTuple);
        }
        tuples.push(tuple);
        Ok(())
    }

    fn delete_tuple(&mut self, tuple: &RelationTuple) -> Result<(), StoreError> {
        let tuples = self
            .index
            .get_mut(&tuple.object)
            .ok_or(StoreError::TupleNotFound)?;
        let pos = tuples
            .iter()
            .position(|t| t == tuple)
            .ok_or(StoreError::TupleNotFound)?;
        tuples.swap_remove(pos);
        if tuples.is_empty() {
            self.index.remove(&tuple.object);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_tuple(ns: &str, id: &str, rel: &str, user: &str) -> RelationTuple {
        RelationTuple::new(Object::new(ns, id), Relation::new(rel), User::user_id(user))
    }

    #[test]
    fn test_should_return_empty_for_empty_store() {
        let store = InMemoryTupleStore::default();
        let results = store.read_tuples(&Object::new("doc", "1"), None, None);
        assert!(results.is_empty());
    }

    #[test]
    fn test_should_return_empty_for_nonexistent_object() {
        let mut store = InMemoryTupleStore::default();
        store
            .write_tuple(make_tuple("doc", "1", "owner", "alice"))
            .unwrap();

        let results = store.read_tuples(&Object::new("doc", "nonexistent"), None, None);
        assert!(results.is_empty());
    }

    #[test]
    fn test_should_read_with_no_relation_filter() {
        let mut store = InMemoryTupleStore::default();
        let obj = Object::new("doc", "1");
        store
            .write_tuple(make_tuple("doc", "1", "owner", "alice"))
            .unwrap();
        store
            .write_tuple(make_tuple("doc", "1", "viewer", "bob"))
            .unwrap();

        let alice = User::user_id("alice");
        // Filter by user only — should return alice's tuple regardless of relation.
        let results = store.read_tuples(&obj, None, Some(&alice));
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].relation, Relation::new("owner"));
    }

    #[test]
    fn test_should_read_with_no_user_filter() {
        let mut store = InMemoryTupleStore::default();
        let obj = Object::new("doc", "1");
        store
            .write_tuple(make_tuple("doc", "1", "viewer", "alice"))
            .unwrap();
        store
            .write_tuple(make_tuple("doc", "1", "viewer", "bob"))
            .unwrap();
        store
            .write_tuple(make_tuple("doc", "1", "owner", "charlie"))
            .unwrap();

        // Filter by relation only — should return all viewers.
        let viewer = Relation::new("viewer");
        let results = store.read_tuples(&obj, Some(&viewer), None);
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn test_should_read_all_tuples_for_object() {
        let mut store = InMemoryTupleStore::default();
        let obj = Object::new("doc", "1");
        store
            .write_tuple(make_tuple("doc", "1", "owner", "alice"))
            .unwrap();
        store
            .write_tuple(make_tuple("doc", "1", "viewer", "bob"))
            .unwrap();
        store
            .write_tuple(make_tuple("doc", "1", "editor", "charlie"))
            .unwrap();

        // No filters — return all tuples for this object.
        let results = store.read_tuples(&obj, None, None);
        assert_eq!(results.len(), 3);
    }

    #[test]
    fn test_should_isolate_tuples_by_object() {
        let mut store = InMemoryTupleStore::default();
        store
            .write_tuple(make_tuple("doc", "1", "owner", "alice"))
            .unwrap();
        store
            .write_tuple(make_tuple("doc", "2", "owner", "bob"))
            .unwrap();

        let results = store.read_tuples(&Object::new("doc", "1"), None, None);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].user, User::user_id("alice"));
    }

    #[test]
    fn test_should_store_and_read_userset_tuples() {
        let mut store = InMemoryTupleStore::default();
        let tuple = RelationTuple::new(
            Object::new("doc", "1"),
            Relation::new("parent"),
            User::userset(Object::new("folder", "A"), Relation::new("viewer")),
        );
        store.write_tuple(tuple.clone()).unwrap();

        let results = store.read_tuples(
            &Object::new("doc", "1"),
            Some(&Relation::new("parent")),
            None,
        );
        assert_eq!(results.len(), 1);
        assert_eq!(results[0], tuple);
    }

    #[test]
    fn test_should_allow_rewrite_after_delete() {
        let mut store = InMemoryTupleStore::default();
        let tuple = make_tuple("doc", "1", "owner", "alice");

        store.write_tuple(tuple.clone()).unwrap();
        store.delete_tuple(&tuple).unwrap();
        // Should be able to re-insert after deletion.
        assert!(store.write_tuple(tuple).is_ok());
    }

    #[test]
    fn test_should_clean_index_on_last_delete() {
        let mut store = InMemoryTupleStore::default();
        let tuple = make_tuple("doc", "1", "owner", "alice");

        store.write_tuple(tuple.clone()).unwrap();
        store.delete_tuple(&tuple).unwrap();
        // Internal index should be cleaned up — object key removed.
        assert!(!store.index.contains_key(&Object::new("doc", "1")));
    }
}
