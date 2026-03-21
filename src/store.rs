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
