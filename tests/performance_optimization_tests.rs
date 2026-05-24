use std::{collections::HashSet, fs, num::NonZeroU64, ops::Range, path::PathBuf, process};

use proptest::prelude::*;
use simple_zanzibar::{
    EngineError, IndexProfile, SnapshotIntegrityMode, SnapshotLoadOptions, SnapshotLoadProfile,
    SnapshotSaveOptions, SnapshotValidationMode, ZanzibarEngine,
    model::{
        LookupObjectPermissionsRequest, LookupResourcesRequest, LookupSubjectsRequest, Object,
        Relation, User,
    },
    relationship::{RelationshipMutation, StoreError},
    revision::Consistency,
};

const HEADER_LEN: usize = 76;
const DIRECTORY_ENTRY_LEN: usize = 28;
const RELATIONSHIP_COUNT_OFFSET: usize = 60;
const RELATIONSHIP_ROWS_SECTION: u16 = 4;

#[test]
fn test_should_build_lazy_uniqueness_after_full_snapshot_load()
-> Result<(), Box<dyn std::error::Error>> {
    let engine = seeded_engine()?;
    let path = unique_snapshot_path("lazy-full");
    engine.save_snapshot(&path, SnapshotSaveOptions::default())?;

    let loaded = ZanzibarEngine::load_snapshot(&path, SnapshotLoadOptions::default())?;
    loaded.write_relationships([RelationshipMutation::touch("doc:two#viewer@user:bob")?])?;
    loaded.write_relationships([RelationshipMutation::delete("doc:two#viewer@user:bob")?])?;
    loaded.write_relationships([RelationshipMutation::create("doc:three#viewer@user:bob")?])?;

    fs::remove_file(path)?;
    Ok(())
}

#[test]
fn test_should_reject_trusted_duplicate_rows_on_first_write_without_publishing()
-> Result<(), Box<dyn std::error::Error>> {
    let engine = seeded_engine()?;
    let path = unique_snapshot_path("trusted-duplicate");
    engine.save_snapshot(&path, SnapshotSaveOptions::default())?;
    duplicate_second_relationship_row(&path)?;

    let loaded = ZanzibarEngine::load_snapshot(&path, trusted_external_options())?;
    let before = loaded.check_relation(
        &object("doc", "one"),
        &relation("viewer"),
        &User::user_id("alice"),
    )?;
    let result =
        loaded.write_relationships([RelationshipMutation::touch("doc:four#viewer@user:bob")?]);
    assert!(matches!(
        result,
        Err(EngineError::Store(StoreError::InternalInvariant { .. }))
    ));
    let after = loaded.check_relation(
        &object("doc", "one"),
        &relation("viewer"),
        &User::user_id("alice"),
    )?;
    assert_eq!(before, after);

    fs::remove_file(path)?;
    Ok(())
}

#[test]
fn test_should_support_check_only_profile_and_reject_reverse_lookup()
-> Result<(), Box<dyn std::error::Error>> {
    let engine = seeded_engine()?;
    let path = unique_snapshot_path("check-only");
    engine.save_snapshot(
        &path,
        SnapshotSaveOptions {
            index_profile: IndexProfile::CheckOnly,
            ..SnapshotSaveOptions::default()
        },
    )?;

    let default_load = ZanzibarEngine::load_snapshot(&path, SnapshotLoadOptions::default());
    assert!(default_load.is_err());

    let loaded = ZanzibarEngine::load_snapshot(
        &path,
        SnapshotLoadOptions {
            required_index_profile: IndexProfile::CheckOnly,
            ..SnapshotLoadOptions::default()
        },
    )?;
    assert!(loaded.check_relation(
        &object("doc", "one"),
        &relation("viewer"),
        &User::user_id("alice"),
    )?);
    let subjects = loaded.lookup_subjects(LookupSubjectsRequest::new(
        object("doc", "one"),
        relation("viewer"),
        "user",
    ))?;
    assert_eq!(subjects.subjects, vec![User::user_id("alice")]);
    let reverse = loaded.lookup_resources(LookupResourcesRequest::new(
        User::user_id("alice"),
        relation("viewer"),
        "doc",
    ));
    assert!(matches!(
        reverse,
        Err(EngineError::UnsupportedIndexProfile {
            profile: IndexProfile::CheckOnly,
            ..
        })
    ));

    fs::remove_file(path)?;
    Ok(())
}

#[test]
fn test_should_support_check_and_object_audit_profile() -> Result<(), Box<dyn std::error::Error>> {
    let engine = seeded_engine()?;
    let path = unique_snapshot_path("object-audit");
    engine.save_snapshot(
        &path,
        SnapshotSaveOptions {
            index_profile: IndexProfile::CheckAndObjectAudit,
            ..SnapshotSaveOptions::default()
        },
    )?;

    let loaded = ZanzibarEngine::load_snapshot(
        &path,
        SnapshotLoadOptions {
            required_index_profile: IndexProfile::CheckAndObjectAudit,
            ..SnapshotLoadOptions::default()
        },
    )?;
    let permissions = loaded.lookup_object_permissions(LookupObjectPermissionsRequest::new(
        object("doc", "one"),
        "user",
        Consistency::Latest,
    ))?;
    let [permission] = permissions.permissions.as_slice() else {
        return Err("expected one permission group".into());
    };
    assert_eq!(permission.permission, relation("viewer"));
    assert_eq!(permission.subjects, vec![User::user_id("alice")]);

    fs::remove_file(path)?;
    Ok(())
}

#[test]
fn test_should_preserve_exact_revision_across_segmented_delta_delete()
-> Result<(), Box<dyn std::error::Error>> {
    let engine = seeded_engine()?;
    let stable_doc = object("doc", "stable");
    let viewer = relation("viewer");
    let alice = User::user_id("alice");
    let relationship = "doc:stable#viewer@user:alice";
    let token = engine.write_relationships([RelationshipMutation::touch(relationship)?])?;

    engine.write_relationships([RelationshipMutation::delete(relationship)?])?;

    assert!(!engine.check_relation(&stable_doc, &viewer, &alice)?);
    assert!(engine.check_with_consistency(
        &stable_doc,
        &viewer,
        &alice,
        Consistency::Exact(token),
    )?);
    Ok(())
}

#[test]
fn test_should_save_canonical_snapshot_from_segmented_delta_view()
-> Result<(), Box<dyn std::error::Error>> {
    let engine = seeded_engine()?;
    engine.write_relationships([
        RelationshipMutation::delete("doc:one#viewer@user:alice")?,
        RelationshipMutation::touch("doc:delta#viewer@user:bob")?,
    ])?;
    let path = unique_snapshot_path("delta-canonical");
    engine.save_snapshot(&path, SnapshotSaveOptions::default())?;

    let loaded = ZanzibarEngine::load_snapshot(&path, SnapshotLoadOptions::default())?;
    assert!(!loaded.check_relation(
        &object("doc", "one"),
        &relation("viewer"),
        &User::user_id("alice"),
    )?);
    assert!(loaded.check_relation(
        &object("doc", "delta"),
        &relation("viewer"),
        &User::user_id("bob"),
    )?);

    fs::remove_file(path)?;
    Ok(())
}

proptest! {
    #[test]
    fn test_should_match_reference_set_for_segmented_delta_publication(
        operations in proptest::collection::vec((0_u8..3, 0_u8..8, 0_u8..8), 1..128),
    ) {
        let engine = ZanzibarEngine::builder().build();
        let schema_result = engine.add_dsl(
            r"
            namespace doc {
                relation viewer {}
            }
            ",
        );
        prop_assert!(schema_result.is_ok());

        let viewer = relation("viewer");
        let mut reference = HashSet::new();
        for (operation, doc_index, user_index) in operations {
            let relationship = format!("doc:{doc_index}#viewer@user:{user_index}");
            let doc = object("doc", &doc_index.to_string());
            let user = User::user_id(user_index.to_string());
            match operation {
                0 => {
                    let result = engine.write_relationships([
                        RelationshipMutation::create(relationship.as_str())
                            .map_err(|error| TestCaseError::fail(error.to_string()))?,
                    ]);
                    if reference.insert(relationship.clone()) {
                        prop_assert!(result.is_ok());
                    } else {
                        let is_duplicate = matches!(
                            result,
                            Err(EngineError::Store(StoreError::RelationshipAlreadyExists { .. }))
                        );
                        prop_assert!(is_duplicate);
                    }
                }
                1 => {
                    let mutation = RelationshipMutation::touch(relationship.as_str())
                        .map_err(|error| TestCaseError::fail(error.to_string()))?;
                    prop_assert!(engine.write_relationships([mutation]).is_ok());
                    reference.insert(relationship.clone());
                }
                _ => {
                    let result = engine.write_relationships([
                        RelationshipMutation::delete(relationship.as_str())
                            .map_err(|error| TestCaseError::fail(error.to_string()))?,
                    ]);
                    if reference.remove(&relationship) {
                        prop_assert!(result.is_ok());
                    } else {
                        let is_missing = matches!(
                            result,
                            Err(EngineError::Store(StoreError::RelationshipNotFound { .. }))
                        );
                        prop_assert!(is_missing);
                    }
                }
            }
            let actual = engine
                .check_relation(&doc, &viewer, &user)
                .map_err(|error| TestCaseError::fail(error.to_string()))?;
            prop_assert_eq!(actual, reference.contains(&relationship));
        }
    }
}

fn seeded_engine() -> Result<ZanzibarEngine, Box<dyn std::error::Error>> {
    let engine = ZanzibarEngine::builder().build();
    engine.add_dsl(
        r#"
        namespace group {
            relation member {}
        }

        namespace doc {
            relation viewer {}
            relation parent {}
            relation inherited {
                rewrite tuple_to_userset(tupleset: "parent", computed_userset: "member")
            }
        }
        "#,
    )?;
    engine.write_relationships([
        RelationshipMutation::touch("doc:one#viewer@user:alice")?,
        RelationshipMutation::touch("doc:two#viewer@user:alice")?,
        RelationshipMutation::touch("group:eng#member@user:alice")?,
        RelationshipMutation::touch("doc:inherited#parent@group:eng#member")?,
    ])?;
    Ok(engine)
}

fn duplicate_second_relationship_row(path: &PathBuf) -> Result<(), Box<dyn std::error::Error>> {
    let mut bytes = fs::read(path)?;
    let row_count = usize::try_from(read_u32(&bytes, RELATIONSHIP_COUNT_OFFSET)?)?;
    if row_count < 2 {
        return Err("snapshot fixture needs at least two relationship rows".into());
    }
    let rows = section_range(&bytes, RELATIONSHIP_ROWS_SECTION)?;
    if rows.len() % row_count != 0 {
        return Err("relationship row section length does not match row count".into());
    }
    let row_len = rows.len() / row_count;
    let second_start = rows
        .start
        .checked_add(row_len)
        .ok_or("row offset overflowed")?;
    let second_end = second_start
        .checked_add(row_len)
        .ok_or("row offset overflowed")?;
    let first = bytes
        .get(rows.start..second_start)
        .ok_or("first row range is invalid")?
        .to_vec();
    let second = bytes
        .get_mut(second_start..second_end)
        .ok_or("second row range is invalid")?;
    second.copy_from_slice(&first);
    fs::write(path, bytes)?;
    Ok(())
}

fn section_range(bytes: &[u8], kind: u16) -> Result<Range<usize>, Box<dyn std::error::Error>> {
    let directory = bytes
        .get(HEADER_LEN..)
        .ok_or("snapshot directory is missing")?;
    for entry in directory.chunks_exact(DIRECTORY_ENTRY_LEN).take(11) {
        let entry_kind = read_u16(entry, 0)?;
        if entry_kind != kind {
            continue;
        }
        let offset = usize::try_from(read_u64(entry, 4)?)?;
        let len = usize::try_from(read_u64(entry, 12)?)?;
        let end = offset.checked_add(len).ok_or("section range overflowed")?;
        return Ok(offset..end);
    }
    Err("relationship rows section not found".into())
}

fn read_u16(bytes: &[u8], offset: usize) -> Result<u16, Box<dyn std::error::Error>> {
    let end = offset.checked_add(2).ok_or("u16 offset overflowed")?;
    let slice = bytes.get(offset..end).ok_or("u16 range is invalid")?;
    let mut value = [0_u8; 2];
    value.copy_from_slice(slice);
    Ok(u16::from_le_bytes(value))
}

fn read_u32(bytes: &[u8], offset: usize) -> Result<u32, Box<dyn std::error::Error>> {
    let end = offset.checked_add(4).ok_or("u32 offset overflowed")?;
    let slice = bytes.get(offset..end).ok_or("u32 range is invalid")?;
    let mut value = [0_u8; 4];
    value.copy_from_slice(slice);
    Ok(u32::from_le_bytes(value))
}

fn read_u64(bytes: &[u8], offset: usize) -> Result<u64, Box<dyn std::error::Error>> {
    let end = offset.checked_add(8).ok_or("u64 offset overflowed")?;
    let slice = bytes.get(offset..end).ok_or("u64 range is invalid")?;
    let mut value = [0_u8; 8];
    value.copy_from_slice(slice);
    Ok(u64::from_le_bytes(value))
}

fn trusted_external_options() -> SnapshotLoadOptions {
    SnapshotLoadOptions {
        validation: SnapshotValidationMode::TrustedFastLoad,
        integrity: SnapshotIntegrityMode::External,
        profile: SnapshotLoadProfile::FastLoad,
        max_file_bytes: non_zero_u64(16 * 1024 * 1024),
        required_index_profile: IndexProfile::Full,
        compression: simple_zanzibar::SnapshotCompression::None,
    }
}

fn unique_snapshot_path(label: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "simple_zanzibar_perf_test_{}_{}.szsnap",
        label,
        process::id(),
    ))
}

fn object(namespace: &str, id: &str) -> Object {
    Object {
        namespace: namespace.to_string(),
        id: id.to_string(),
    }
}

fn relation(name: &str) -> Relation {
    Relation(name.to_string())
}

fn non_zero_u64(value: u64) -> NonZeroU64 {
    match NonZeroU64::new(value) {
        Some(value) => value,
        None => NonZeroU64::MIN,
    }
}
