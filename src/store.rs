//! Defines the storage abstraction for relation tuples.

use crate::model::{Object, Relation, RelationTuple, User};
use std::collections::HashSet;

/// A trait for abstracting the storage and retrieval of `RelationTuple`s.
/// This allows the core logic to be decoupled from the specific storage backend.
pub trait TupleStore {
    /// Reads tuples from the store, with optional filtering.
    ///
    /// # Arguments
    ///
    /// * `object` - The object to filter by.
    /// * `relation` - An optional relation to filter by.
    /// * `user` - An optional user to filter by.
    ///
    /// # Returns
    ///
    /// A vector of matching `RelationTuple`s.
    fn read_tuples(
        &self,
        object: &Object,
        relation: Option<&Relation>,
        user: Option<&User>,
    ) -> Vec<RelationTuple>;

    /// Writes a single tuple to the store.
    ///
    /// # Returns
    ///
    /// `Ok(())` if the write was successful, or an error string if it failed.
    fn write_tuple(&mut self, tuple: RelationTuple) -> Result<(), String>;

    /// Deletes a single tuple from the store.
    ///
    /// # Returns
    ///
    /// `Ok(())` if the delete was successful, or an error string if it failed.
    fn delete_tuple(&mut self, tuple: &RelationTuple) -> Result<(), String>;
}

/// A simple, in-memory implementation of the `TupleStore` trait using a `HashSet`.
#[derive(Default)]
pub struct InMemoryTupleStore {
    store: HashSet<RelationTuple>,
}

impl TupleStore for InMemoryTupleStore {
    fn read_tuples(
        &self,
        object: &Object,
        relation: Option<&Relation>,
        user: Option<&User>,
    ) -> Vec<RelationTuple> {
        self.store
            .iter()
            .filter(|t| {
                t.object == *object
                    && relation.is_none_or(|r| t.relation == *r)
                    && user.is_none_or(|u| t.user == *u)
            })
            .cloned()
            .collect()
    }

    fn write_tuple(&mut self, tuple: RelationTuple) -> Result<(), String> {
        if self.store.insert(tuple) {
            Ok(())
        } else {
            Err("Tuple already exists".to_string())
        }
    }

    fn delete_tuple(&mut self, tuple: &RelationTuple) -> Result<(), String> {
        if self.store.remove(tuple) {
            Ok(())
        } else {
            Err("Tuple not found".to_string())
        }
    }
}
