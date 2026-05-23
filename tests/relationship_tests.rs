use std::collections::HashSet;
use std::str::FromStr;

use proptest::prelude::*;
use simple_zanzibar::domain::{DomainError, Relationship};
use simple_zanzibar::relationship::{
    IndexedRelationshipStore, Precondition, QueryLimit, RelationshipFilter, RelationshipMutation,
    RelationshipReader, StoreError, SubjectFilter,
};

fn relationship(value: &str) -> Result<Relationship, DomainError> {
    Relationship::from_str(value)
}

fn exact_filter(value: &str) -> Result<RelationshipFilter, DomainError> {
    let relationship = relationship(value)?;
    let subject = match relationship.subject() {
        simple_zanzibar::domain::SubjectRef::Object(object) => SubjectFilter::exact(
            object.object_type().as_str().try_into()?,
            object.object_id().as_str().try_into()?,
            None,
        ),
        simple_zanzibar::domain::SubjectRef::Userset { object, relation } => SubjectFilter::exact(
            object.object_type().as_str().try_into()?,
            object.object_id().as_str().try_into()?,
            Some(relation.clone()),
        ),
    };

    Ok(RelationshipFilter::for_exact_subject(
        relationship.resource(),
        relationship.relation().clone(),
        subject,
    ))
}

#[test]
fn test_should_create_touch_and_delete_relationships() -> Result<(), Box<dyn std::error::Error>> {
    let mut store = IndexedRelationshipStore::default();
    let tuple = relationship("doc:readme#viewer@user:alice")?;

    store.apply_mutations([RelationshipMutation::Create(tuple.clone())], [])?;
    assert!(matches!(
        store.apply_mutations([RelationshipMutation::Create(tuple.clone())], []),
        Err(StoreError::RelationshipAlreadyExists { .. })
    ));

    store.apply_mutations([RelationshipMutation::Touch(tuple.clone())], [])?;
    assert_eq!(store.rows().len(), 1);

    store.apply_mutations([RelationshipMutation::Delete(tuple.clone())], [])?;
    assert!(store.rows().is_empty());
    assert!(matches!(
        store.apply_mutations([RelationshipMutation::Delete(tuple)], []),
        Err(StoreError::RelationshipNotFound { .. })
    ));

    Ok(())
}

#[test]
fn test_should_enforce_preconditions() -> Result<(), Box<dyn std::error::Error>> {
    let mut store = IndexedRelationshipStore::default();
    let tuple = relationship("doc:readme#viewer@user:alice")?;
    let filter = exact_filter("doc:readme#viewer@user:alice")?;

    assert!(matches!(
        store.apply_mutations(
            [RelationshipMutation::Touch(tuple.clone())],
            [Precondition::MustMatch(filter.clone())],
        ),
        Err(StoreError::PreconditionFailed { .. })
    ));

    store.apply_mutations(
        [RelationshipMutation::Touch(tuple.clone())],
        [Precondition::MustNotMatch(filter.clone())],
    )?;
    assert!(matches!(
        store.apply_mutations(
            [RelationshipMutation::Touch(tuple)],
            [Precondition::MustNotMatch(filter)],
        ),
        Err(StoreError::PreconditionFailed { .. })
    ));

    Ok(())
}

#[test]
fn test_should_apply_batch_atomically() -> Result<(), Box<dyn std::error::Error>> {
    let mut store = IndexedRelationshipStore::default();
    let existing = relationship("doc:readme#viewer@user:alice")?;
    let created = relationship("doc:readme#viewer@user:bob")?;
    let missing = relationship("doc:missing#viewer@user:alice")?;
    store.apply_mutations([RelationshipMutation::Touch(existing.clone())], [])?;

    assert!(matches!(
        store.apply_mutations(
            [
                RelationshipMutation::Create(created.clone()),
                RelationshipMutation::Delete(missing),
            ],
            [],
        ),
        Err(StoreError::RelationshipNotFound { .. })
    ));

    assert_eq!(store.rows(), &[existing]);
    assert!(store
        .query_relationships(&exact_filter(&created.to_string())?)?
        .next()
        .is_none());
    Ok(())
}

#[test]
fn test_should_cap_mutation_and_precondition_batches() -> Result<(), Box<dyn std::error::Error>> {
    let mut store = IndexedRelationshipStore::default();
    let tuple = relationship("doc:readme#viewer@user:alice")?;
    let filter = exact_filter("doc:readme#viewer@user:alice")?;

    let mutations = std::iter::repeat_n(RelationshipMutation::Touch(tuple), 10_001);
    assert!(matches!(
        store.apply_mutations(mutations, []),
        Err(StoreError::MutationBatchTooLarge { .. })
    ));

    let preconditions = std::iter::repeat_n(Precondition::MustNotMatch(filter), 101);
    assert!(matches!(
        store.apply_mutations([], preconditions),
        Err(StoreError::PreconditionBatchTooLarge { .. })
    ));

    Ok(())
}

#[test]
fn test_should_query_resource_and_subject_indexes() -> Result<(), Box<dyn std::error::Error>> {
    let mut store = IndexedRelationshipStore::default();
    let alice = relationship("doc:readme#viewer@user:alice")?;
    let group = relationship("doc:readme#viewer@group:eng#member")?;
    store.apply_mutations(
        [
            RelationshipMutation::Create(alice.clone()),
            RelationshipMutation::Create(group.clone()),
        ],
        [],
    )?;

    let resource_filter = RelationshipFilter::new(
        "doc".try_into()?,
        Some("readme".try_into()?),
        Some("viewer".try_into()?),
        None,
        QueryLimit::default(),
    );
    assert_eq!(
        store
            .query_relationships(&resource_filter)?
            .collect::<Vec<_>>()
            .len(),
        2
    );

    let resource_type_filter =
        RelationshipFilter::new("doc".try_into()?, None, None, None, QueryLimit::default());
    assert_eq!(
        store
            .query_relationships(&resource_type_filter)?
            .collect::<Vec<_>>()
            .len(),
        2
    );

    let resource_object_filter = RelationshipFilter::new(
        "doc".try_into()?,
        Some("readme".try_into()?),
        None,
        None,
        QueryLimit::default(),
    );
    assert_eq!(
        store
            .query_relationships(&resource_object_filter)?
            .collect::<Vec<_>>()
            .len(),
        2
    );

    let resource_type_relation_filter = RelationshipFilter::new(
        "doc".try_into()?,
        None,
        Some("viewer".try_into()?),
        None,
        QueryLimit::default(),
    );
    assert_eq!(
        store
            .query_relationships(&resource_type_relation_filter)?
            .collect::<Vec<_>>()
            .len(),
        2
    );

    let subject_filter = SubjectFilter::exact(
        "group".try_into()?,
        "eng".try_into()?,
        Some("member".try_into()?),
    );
    assert_eq!(
        store
            .reverse_query_relationships(&subject_filter)?
            .collect::<Vec<_>>(),
        vec![&group]
    );

    let subject_type_filter = SubjectFilter::new("group".try_into()?, None, None);
    assert_eq!(
        store
            .reverse_query_relationships(&subject_type_filter)?
            .collect::<Vec<_>>(),
        vec![&group]
    );

    let subject_type_relation_filter =
        SubjectFilter::new("group".try_into()?, None, Some("member".try_into()?));
    assert_eq!(
        store
            .reverse_query_relationships(&subject_type_relation_filter)?
            .collect::<Vec<_>>(),
        vec![&group]
    );

    Ok(())
}

proptest! {
    #[test]
    fn test_should_keep_indexes_equivalent_to_reference_set(ops in proptest::collection::vec((0usize..24, any::<bool>()), 1..128)) {
        let universe = relationship_universe()
            .map_err(|error| TestCaseError::fail(error.to_string()))?;
        let mut store = IndexedRelationshipStore::default();
        let mut reference = HashSet::new();

        for (index, should_touch) in ops {
            let tuple = universe
                .get(index % universe.len())
                .ok_or_else(|| TestCaseError::fail("relationship universe must not be empty"))?
                .clone();
            if should_touch {
                let _ = store.apply_mutations([RelationshipMutation::Touch(tuple.clone())], []);
                reference.insert(tuple);
            } else {
                let _ = store.apply_mutations([RelationshipMutation::Delete(tuple.clone())], []);
                reference.remove(&tuple);
            }

            let rows = store.rows().iter().cloned().collect::<HashSet<_>>();
            prop_assert_eq!(rows, reference.clone());

            for tuple in &universe {
                let indexed_matches = store
                    .query_relationships(&exact_filter(&tuple.to_string())
                        .map_err(|error| TestCaseError::fail(error.to_string()))?)
                    .map_err(|error| TestCaseError::fail(error.to_string()))?
                    .cloned()
                    .collect::<HashSet<_>>();
                let reference_matches = reference
                    .iter()
                    .filter(|candidate| *candidate == tuple)
                    .cloned()
                    .collect::<HashSet<_>>();
                prop_assert_eq!(indexed_matches, reference_matches);
            }

            for subject in subject_filters()? {
                let indexed_matches = store
                    .reverse_query_relationships(&subject)
                    .map_err(|error| TestCaseError::fail(error.to_string()))?
                    .cloned()
                    .collect::<HashSet<_>>();
                let reference_matches = reference
                    .iter()
                    .filter(|candidate| subject_filter_matches(candidate, &subject))
                    .cloned()
                    .collect::<HashSet<_>>();
                prop_assert_eq!(indexed_matches, reference_matches);
            }
        }
    }

    #[test]
    fn test_should_keep_indexes_equivalent_after_random_batches(batches in proptest::collection::vec(proptest::collection::vec((0usize..24, 0u8..3), 1..8), 1..64)) {
        let universe = relationship_universe()
            .map_err(|error| TestCaseError::fail(error.to_string()))?;
        let mut store = IndexedRelationshipStore::default();
        let mut reference = HashSet::new();

        for batch in batches {
            let mutations = batch
                .into_iter()
                .map(|(index, operation)| {
                    let tuple = universe
                        .get(index % universe.len())
                        .ok_or_else(|| TestCaseError::fail("relationship universe must not be empty"))?
                        .clone();
                    Ok(match operation {
                        0 => RelationshipMutation::Create(tuple),
                        1 => RelationshipMutation::Touch(tuple),
                        _ => RelationshipMutation::Delete(tuple),
                    })
                })
                .collect::<Result<Vec<_>, TestCaseError>>()?;

            let expected = apply_reference_batch(&reference, &mutations);
            let actual = store.apply_mutations(mutations, []);
            match expected {
                Some(next_reference) => {
                    prop_assert!(actual.is_ok());
                    reference = next_reference;
                }
                None => {
                    prop_assert!(actual.is_err());
                }
            }

            assert_index_equivalence(&store, &reference, &universe)?;
        }
    }
}

fn apply_reference_batch(
    reference: &HashSet<Relationship>,
    mutations: &[RelationshipMutation],
) -> Option<HashSet<Relationship>> {
    let mut seen = HashSet::with_capacity(mutations.len());
    for mutation in mutations {
        if !seen.insert(mutation.relationship().clone()) {
            return None;
        }
    }

    let mut candidate = reference.clone();
    for mutation in mutations {
        match mutation {
            RelationshipMutation::Create(relationship) => {
                if !candidate.insert(relationship.clone()) {
                    return None;
                }
            }
            RelationshipMutation::Touch(relationship) => {
                candidate.insert(relationship.clone());
            }
            RelationshipMutation::Delete(relationship) => {
                if !candidate.remove(relationship) {
                    return None;
                }
            }
        }
    }
    Some(candidate)
}

fn assert_index_equivalence(
    store: &IndexedRelationshipStore,
    reference: &HashSet<Relationship>,
    universe: &[Relationship],
) -> Result<(), TestCaseError> {
    let rows = store.rows().iter().cloned().collect::<HashSet<_>>();
    if rows != *reference {
        return Err(TestCaseError::fail("row set drifted from reference set"));
    }

    for tuple in universe {
        let indexed_matches = store
            .query_relationships(
                &exact_filter(&tuple.to_string())
                    .map_err(|error| TestCaseError::fail(error.to_string()))?,
            )
            .map_err(|error| TestCaseError::fail(error.to_string()))?
            .cloned()
            .collect::<HashSet<_>>();
        let reference_matches = reference
            .iter()
            .filter(|candidate| *candidate == tuple)
            .cloned()
            .collect::<HashSet<_>>();
        if indexed_matches != reference_matches {
            return Err(TestCaseError::fail(
                "resource index drifted from reference set",
            ));
        }
    }

    for filter in resource_filters().map_err(|error| TestCaseError::fail(error.to_string()))? {
        let indexed_matches = store
            .query_relationships(&filter)
            .map_err(|error| TestCaseError::fail(error.to_string()))?
            .cloned()
            .collect::<HashSet<_>>();
        let reference_matches = reference
            .iter()
            .filter(|candidate| resource_filter_matches(candidate, &filter))
            .cloned()
            .collect::<HashSet<_>>();
        if indexed_matches != reference_matches {
            return Err(TestCaseError::fail(
                "broad resource index drifted from reference set",
            ));
        }
    }

    for subject in subject_filters().map_err(|error| TestCaseError::fail(error.to_string()))? {
        let indexed_matches = store
            .reverse_query_relationships(&subject)
            .map_err(|error| TestCaseError::fail(error.to_string()))?
            .cloned()
            .collect::<HashSet<_>>();
        let reference_matches = reference
            .iter()
            .filter(|candidate| subject_filter_matches(candidate, &subject))
            .cloned()
            .collect::<HashSet<_>>();
        if indexed_matches != reference_matches {
            return Err(TestCaseError::fail(
                "subject index drifted from reference set",
            ));
        }
    }

    Ok(())
}

fn resource_filters() -> Result<Vec<RelationshipFilter>, DomainError> {
    Ok(vec![
        RelationshipFilter::new("doc".try_into()?, None, None, None, QueryLimit::default()),
        RelationshipFilter::new(
            "folder".try_into()?,
            None,
            None,
            None,
            QueryLimit::default(),
        ),
        RelationshipFilter::new(
            "doc".try_into()?,
            Some("a".try_into()?),
            None,
            None,
            QueryLimit::default(),
        ),
        RelationshipFilter::new(
            "folder".try_into()?,
            Some("c".try_into()?),
            None,
            None,
            QueryLimit::default(),
        ),
        RelationshipFilter::new(
            "doc".try_into()?,
            None,
            Some("viewer".try_into()?),
            None,
            QueryLimit::default(),
        ),
        RelationshipFilter::new(
            "folder".try_into()?,
            None,
            Some("owner".try_into()?),
            None,
            QueryLimit::default(),
        ),
    ])
}

fn relationship_universe() -> Result<Vec<Relationship>, DomainError> {
    let direct = ["doc", "folder"]
        .into_iter()
        .flat_map(|object_type| {
            ["a", "b", "c"].into_iter().flat_map(move |object_id| {
                ["viewer", "owner"].into_iter().flat_map(move |relation| {
                    ["alice", "bob"].into_iter().map(move |subject| {
                        relationship(&format!(
                            "{object_type}:{object_id}#{relation}@user:{subject}"
                        ))
                    })
                })
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let usersets = ["doc", "folder"]
        .into_iter()
        .flat_map(|object_type| {
            ["a", "b", "c"].into_iter().flat_map(move |object_id| {
                ["viewer", "owner"].into_iter().map(move |relation| {
                    relationship(&format!(
                        "{object_type}:{object_id}#{relation}@group:eng#member"
                    ))
                })
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(direct.into_iter().chain(usersets).collect())
}

fn subject_filters() -> Result<Vec<SubjectFilter>, DomainError> {
    Ok(vec![
        SubjectFilter::exact("user".try_into()?, "alice".try_into()?, None),
        SubjectFilter::exact("user".try_into()?, "bob".try_into()?, None),
        SubjectFilter::new("user".try_into()?, None, None),
        SubjectFilter::exact(
            "group".try_into()?,
            "eng".try_into()?,
            Some("member".try_into()?),
        ),
        SubjectFilter::exact("group".try_into()?, "eng".try_into()?, None),
        SubjectFilter::new("group".try_into()?, None, None),
        SubjectFilter::new("group".try_into()?, None, Some("member".try_into()?)),
    ])
}

fn resource_filter_matches(relationship: &Relationship, filter: &RelationshipFilter) -> bool {
    relationship.resource().object_type() == filter.resource_type()
        && filter
            .optional_resource_id()
            .is_none_or(|object_id| relationship.resource().object_id() == object_id)
        && filter
            .optional_relation()
            .is_none_or(|relation| relationship.relation() == relation)
        && filter
            .optional_subject()
            .is_none_or(|subject| subject_filter_matches(relationship, subject))
}

fn subject_filter_matches(relationship: &Relationship, filter: &SubjectFilter) -> bool {
    match relationship.subject() {
        simple_zanzibar::domain::SubjectRef::Object(object) => {
            filter.optional_relation().is_none()
                && object.object_type().as_str() == filter.subject_type().as_str()
                && filter
                    .optional_subject_id()
                    .is_none_or(|subject_id| object.object_id().as_str() == subject_id.as_str())
        }
        simple_zanzibar::domain::SubjectRef::Userset { object, relation } => {
            object.object_type().as_str() == filter.subject_type().as_str()
                && filter
                    .optional_subject_id()
                    .is_none_or(|subject_id| object.object_id().as_str() == subject_id.as_str())
                && filter
                    .optional_relation()
                    .is_none_or(|expected_relation| relation == expected_relation)
        }
    }
}
