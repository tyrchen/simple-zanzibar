//! Defines the storage abstraction for relation tuples.

use std::collections::HashSet;

use crate::model::{Object, Relation, RelationTuple, User};

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
    ///
    /// # Errors
    ///
    /// Returns an error string when the tuple cannot be written, such as attempting to create a
    /// duplicate tuple in stores that enforce uniqueness.
    fn write_tuple(&mut self, tuple: RelationTuple) -> Result<(), String>;

    /// Deletes a single tuple from the store.
    ///
    /// # Returns
    ///
    /// `Ok(())` if the delete was successful, or an error string if it failed.
    ///
    /// # Errors
    ///
    /// Returns an error string when the tuple cannot be deleted, such as when it does not exist.
    fn delete_tuple(&mut self, tuple: &RelationTuple) -> Result<(), String>;

    /// Returns all tuples currently stored.
    ///
    /// This compatibility method lets the v2 indexed store rebuild from legacy state during the
    /// migration period.
    fn all_tuples(&self) -> Vec<RelationTuple>;

    /// Replaces all tuples currently stored.
    ///
    /// This compatibility method lets the service apply validated batch mutations atomically
    /// across the legacy tuple store and the indexed relationship store during the migration
    /// period.
    fn replace_all(&mut self, tuples: Vec<RelationTuple>);
}

/// A simple, in-memory implementation of the `TupleStore` trait using a `HashSet`.
#[derive(Debug, Default)]
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

    fn all_tuples(&self) -> Vec<RelationTuple> {
        self.store.iter().cloned().collect()
    }

    fn replace_all(&mut self, tuples: Vec<RelationTuple>) {
        self.store = tuples.into_iter().collect();
    }
}
