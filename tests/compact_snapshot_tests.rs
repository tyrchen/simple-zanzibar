use std::{
    collections::BTreeSet,
    fs,
    num::{NonZeroU32, NonZeroU64},
    path::{Path, PathBuf},
    process,
    sync::atomic::{AtomicU64, Ordering},
};

use proptest::{prelude::*, test_runner::TestCaseError};
use simple_zanzibar::{
    SnapshotIoError, SnapshotLoadOptions, SnapshotLoadProfile, SnapshotSaveOptions, ZanzibarEngine,
    ZanzibarService,
    eval::EvaluationLimits,
    model::{LookupResourcesRequest, LookupSubjectsRequest, Object, Relation, RelationTuple, User},
    relationship::RelationshipMutation,
    revision::Consistency,
};

static NEXT_TEST_FILE: AtomicU64 = AtomicU64::new(1);

const HEADER_LEN: usize = 76;
const DIRECTORY_ENTRY_LEN: usize = 28;
const FOOTER_LEN: usize = 32;
const SECTION_COUNT_OFFSET: usize = 16;
const RELATIONSHIP_COUNT_OFFSET: usize = 60;
const SYMBOL_COUNT_OFFSET: usize = 64;
const SECTION_KIND_SYMBOL_BYTES: u16 = 2;
const SECTION_KIND_SYMBOL_TABLE: u16 = 3;
const SECTION_KIND_RELATIONSHIP_ROWS: u16 = 4;
const SECTION_KIND_INDEX_DIRECTORY: u16 = 5;
const SECTION_KIND_INDEX_KEYS: u16 = 6;
const SECTION_KIND_POSTING_RANGES: u16 = 7;
const SECTION_KIND_POSTING_ROW_IDS: u16 = 8;

#[test]
fn test_should_save_and_load_snapshot_with_equivalent_behavior()
-> Result<(), Box<dyn std::error::Error>> {
    let (service, writer_token) = populated_service()?;
    let path = temp_snapshot_path("equivalence");
    service.save_snapshot(&path, SnapshotSaveOptions::default())?;

    for profile in [SnapshotLoadProfile::FastLoad, SnapshotLoadProfile::Latency] {
        let mut loaded = ZanzibarService::load_snapshot(
            &path,
            SnapshotLoadOptions {
                profile,
                max_file_bytes: non_zero_u64(16 * 1024 * 1024),
            },
        )?;
        assert_equivalent_behavior(&service, &loaded)?;

        let writer_token_result = loaded.check_with_consistency(
            &doc("direct_doc"),
            &Relation("can_view".to_string()),
            &User::UserId("alice".to_string()),
            Consistency::Exact(writer_token.clone()),
        );
        assert!(matches!(
            writer_token_result,
            Err(simple_zanzibar::error::ZanzibarError::Consistency(_))
        ));

        let bob_tuple = simple_zanzibar::model::RelationTuple {
            object: doc("direct_doc"),
            relation: Relation("viewer".to_string()),
            user: User::UserId("bob".to_string()),
        };
        let bob_token = loaded.write_tuple_with_token(&bob_tuple)?;
        assert!(loaded.check_with_consistency(
            &doc("direct_doc"),
            &Relation("viewer".to_string()),
            &User::UserId("bob".to_string()),
            Consistency::Exact(bob_token),
        )?);
    }

    remove_file(&path);
    Ok(())
}

#[test]
fn test_should_save_and_load_snapshot_through_public_engine_api()
-> Result<(), Box<dyn std::error::Error>> {
    let service = populated_service()?.0;
    let path = temp_snapshot_path("engine");
    service.save_snapshot(&path, SnapshotSaveOptions::default())?;

    let engine = ZanzibarEngine::load_snapshot(&path, SnapshotLoadOptions::default())?;
    let allowed = engine
        .check(simple_zanzibar::model::CheckRequest::new(
            doc("direct_doc"),
            Relation("can_view".to_string()),
            User::UserId("alice".to_string()),
            Consistency::Latest,
        ))?
        .allowed;

    assert!(allowed);
    remove_file(&path);
    Ok(())
}

#[test]
fn test_should_write_deterministic_snapshot_bytes() -> Result<(), Box<dyn std::error::Error>> {
    let service = populated_service()?.0;
    let first = temp_snapshot_path("deterministic_first");
    let second = temp_snapshot_path("deterministic_second");

    service.save_snapshot(&first, SnapshotSaveOptions::default())?;
    service.save_snapshot(&second, SnapshotSaveOptions::default())?;

    let first_bytes = fs::read(&first)?;
    let second_bytes = fs::read(&second)?;
    assert_eq!(first_bytes, second_bytes);

    remove_file(&first);
    remove_file(&second);
    Ok(())
}

#[test]
fn test_should_match_tiny_golden_snapshot_fixture() -> Result<(), Box<dyn std::error::Error>> {
    let service = tiny_service()?;
    let path = temp_snapshot_path("golden");
    service.save_snapshot(&path, SnapshotSaveOptions::default())?;

    let actual = fs::read(&path)?;
    let expected = decode_hex(include_str!("fixtures/snapshots/tiny.szsnap.hex"))?;
    assert_eq!(actual, expected);

    let loaded = ZanzibarService::load_snapshot(&path, SnapshotLoadOptions::default())?;
    assert!(loaded.check(
        &doc("readme"),
        &Relation("viewer".to_string()),
        &User::UserId("alice".to_string()),
    )?);
    remove_file(&path);
    Ok(())
}

#[test]
fn test_should_reject_bad_magic_version_and_header_length() -> Result<(), Box<dyn std::error::Error>>
{
    let bytes = snapshot_bytes()?;

    let mut bad_magic = bytes.clone();
    set_byte(&mut bad_magic, 0, b'X')?;
    assert_corrupt_rejected("bad_magic", &bad_magic)?;

    let mut bad_version = bytes.clone();
    set_u16(&mut bad_version, 8, 2)?;
    assert_corrupt_rejected("bad_version", &bad_version)?;

    let mut bad_header = bytes;
    set_u32(&mut bad_header, 12, 77)?;
    assert_corrupt_rejected("bad_header", &bad_header)?;
    Ok(())
}

#[test]
fn test_should_reject_duplicate_overlapping_and_out_of_bounds_sections()
-> Result<(), Box<dyn std::error::Error>> {
    let bytes = snapshot_bytes()?;

    let mut duplicate = bytes.clone();
    set_u16(&mut duplicate, HEADER_LEN, SECTION_KIND_SYMBOL_BYTES)?;
    assert_corrupt_rejected("duplicate_section", &duplicate)?;

    let schema = section_range(&bytes, 1)?;
    let symbol_entry = directory_entry_offset(SECTION_KIND_SYMBOL_BYTES)?;
    let mut overlap = bytes.clone();
    set_u64(&mut overlap, symbol_entry + 4, u64::try_from(schema.start)?)?;
    assert_corrupt_rejected("overlap_section", &overlap)?;

    let rows_entry = directory_entry_offset(SECTION_KIND_RELATIONSHIP_ROWS)?;
    let mut out_of_bounds = bytes;
    set_u64(&mut out_of_bounds, rows_entry + 12, u64::MAX)?;
    assert_corrupt_rejected("out_of_bounds_section", &out_of_bounds)?;
    Ok(())
}

#[test]
fn test_should_reject_checksum_mismatch() -> Result<(), Box<dyn std::error::Error>> {
    let mut bytes = snapshot_bytes()?;
    let schema = section_range(&bytes, 1)?;
    set_byte(&mut bytes, schema.start, b'X')?;

    assert_corrupt_rejected("checksum_mismatch", &bytes)?;
    Ok(())
}

#[test]
fn test_should_reject_missing_required_section_count() -> Result<(), Box<dyn std::error::Error>> {
    let mut bytes = snapshot_bytes()?;
    set_u32(&mut bytes, SECTION_COUNT_OFFSET, 8)?;

    assert_corrupt_rejected("missing_required_section_count", &bytes)?;
    Ok(())
}

#[test]
fn test_should_reject_malformed_utf8_and_invalid_symbol_ids()
-> Result<(), Box<dyn std::error::Error>> {
    let bytes = snapshot_bytes()?;

    let mut bad_utf8 = bytes.clone();
    let symbol_bytes = section_range(&bad_utf8, SECTION_KIND_SYMBOL_BYTES)?;
    set_byte(&mut bad_utf8, symbol_bytes.start, 0xFF)?;
    rewrite_checksum(&mut bad_utf8)?;
    assert_corrupt_rejected("bad_utf8", &bad_utf8)?;

    let mut invalid_symbol = bytes;
    let rows = section_range(&invalid_symbol, SECTION_KIND_RELATIONSHIP_ROWS)?;
    let symbol_count = read_u32(&invalid_symbol, SYMBOL_COUNT_OFFSET)?;
    set_u32(
        &mut invalid_symbol,
        rows.start,
        symbol_count
            .checked_add(1)
            .ok_or("symbol count overflowed")?,
    )?;
    rewrite_checksum(&mut invalid_symbol)?;
    assert_corrupt_rejected("invalid_symbol_id", &invalid_symbol)?;
    Ok(())
}

#[test]
fn test_should_reject_duplicate_symbols_and_invalid_posting_row_ids()
-> Result<(), Box<dyn std::error::Error>> {
    let bytes = snapshot_bytes()?;

    let mut duplicate_symbol = bytes.clone();
    let symbol_table = section_range(&duplicate_symbol, SECTION_KIND_SYMBOL_TABLE)?;
    assert!(symbol_table.len() >= 16);
    let first_start = read_u32(&duplicate_symbol, symbol_table.start)?;
    let first_len = read_u32(&duplicate_symbol, symbol_table.start + 4)?;
    set_u32(&mut duplicate_symbol, symbol_table.start + 8, first_start)?;
    set_u32(&mut duplicate_symbol, symbol_table.start + 12, first_len)?;
    rewrite_checksum(&mut duplicate_symbol)?;
    assert_corrupt_rejected("duplicate_symbol", &duplicate_symbol)?;

    let mut invalid_row_id = bytes;
    let posting_ranges = section_range(&invalid_row_id, SECTION_KIND_POSTING_RANGES)?;
    let relationship_count = read_u32(&invalid_row_id, RELATIONSHIP_COUNT_OFFSET)?;
    set_u32(
        &mut invalid_row_id,
        posting_ranges.start,
        relationship_count
            .checked_add(1)
            .ok_or("relationship count overflowed")?,
    )?;
    rewrite_checksum(&mut invalid_row_id)?;
    assert_corrupt_rejected("invalid_posting_row_id", &invalid_row_id)?;
    Ok(())
}

#[test]
fn test_should_reject_unsorted_index_keys_and_bad_posting_ranges()
-> Result<(), Box<dyn std::error::Error>> {
    let bytes = snapshot_bytes()?;

    let mut unsorted = bytes.clone();
    let key_count = first_index_key_count(&unsorted)?;
    assert!(key_count > 1);
    let keys = section_range(&unsorted, SECTION_KIND_INDEX_KEYS)?;
    let first_key = copy_range(&unsorted, keys.start, 12)?;
    set_range(&mut unsorted, keys.start + 12, &first_key)?;
    rewrite_checksum(&mut unsorted)?;
    assert_corrupt_rejected("unsorted_keys", &unsorted)?;

    let mut bad_posting = bytes;
    let ranges = section_range(&bad_posting, SECTION_KIND_POSTING_RANGES)?;
    let posting_ids = section_range(&bad_posting, SECTION_KIND_POSTING_ROW_IDS)?;
    let posting_id_count = posting_ids.len() / 4;
    set_u32(
        &mut bad_posting,
        ranges.start + 4,
        u32::try_from(posting_id_count)?,
    )?;
    set_u32(&mut bad_posting, ranges.start + 8, 1)?;
    rewrite_checksum(&mut bad_posting)?;
    assert_corrupt_rejected("bad_posting_range", &bad_posting)?;
    Ok(())
}

proptest! {
    #[test]
    fn test_should_preserve_random_direct_relationship_snapshots(
        pairs in prop::collection::vec((0_u8..16, 0_u8..16), 0..32),
    ) {
        round_trip_random_direct_relationships(&pairs)
            .map_err(|error| TestCaseError::fail(error.to_string()))?;
    }
}

fn assert_equivalent_behavior(
    original: &ZanzibarService,
    loaded: &ZanzibarService,
) -> Result<(), Box<dyn std::error::Error>> {
    let alice = User::UserId("alice".to_string());
    let can_view = Relation("can_view".to_string());
    assert!(original.check(&doc("direct_doc"), &can_view, &alice)?);
    assert!(loaded.check(&doc("direct_doc"), &can_view, &alice)?);
    assert!(original.check(&doc("inherited_doc"), &can_view, &alice)?);
    assert!(loaded.check(&doc("inherited_doc"), &can_view, &alice)?);
    assert!(!original.check(&doc("denied_doc"), &can_view, &alice)?);
    assert!(!loaded.check(&doc("denied_doc"), &can_view, &alice)?);

    for (resource, relation) in [
        ("direct_doc", "viewer"),
        ("direct_doc", "can_view"),
        ("inherited_doc", "can_view"),
        ("denied_doc", "can_view"),
    ] {
        let object = doc(resource);
        let relation = Relation(relation.to_string());
        assert_eq!(
            original.expand(&object, &relation)?,
            loaded.expand(&object, &relation)?,
        );
    }

    assert_eq!(
        original.lookup_resources(&LookupResourcesRequest {
            subject: alice.clone(),
            permission: can_view.clone(),
            resource_type: "doc".to_string(),
        })?,
        loaded.lookup_resources(&LookupResourcesRequest {
            subject: alice.clone(),
            permission: can_view.clone(),
            resource_type: "doc".to_string(),
        })?,
    );
    assert_eq!(
        original.lookup_subjects(&LookupSubjectsRequest {
            resource: doc("direct_doc"),
            permission: can_view.clone(),
            subject_type: "user".to_string(),
        })?,
        loaded.lookup_subjects(&LookupSubjectsRequest {
            resource: doc("direct_doc"),
            permission: can_view,
            subject_type: "user".to_string(),
        })?,
    );
    Ok(())
}

fn round_trip_random_direct_relationships(
    pairs: &[(u8, u8)],
) -> Result<(), Box<dyn std::error::Error>> {
    let mut service = ZanzibarService::new();
    service.add_dsl(
        r"
    namespace doc {
        relation viewer {}
    }
    ",
    )?;

    let unique_pairs = pairs.iter().copied().collect::<BTreeSet<_>>();
    for (doc_id, user_id) in &unique_pairs {
        service.write_tuple_with_token(&RelationTuple {
            object: doc(&format!("random_doc_{doc_id}")),
            relation: Relation("viewer".to_string()),
            user: User::UserId(format!("user_{user_id}")),
        })?;
    }

    let path = temp_snapshot_path("proptest");
    service.save_snapshot(&path, SnapshotSaveOptions::default())?;
    for profile in [SnapshotLoadProfile::FastLoad, SnapshotLoadProfile::Latency] {
        let loaded = ZanzibarService::load_snapshot(
            &path,
            SnapshotLoadOptions {
                profile,
                max_file_bytes: non_zero_u64(16 * 1024 * 1024),
            },
        )?;
        assert_random_direct_equivalence(&service, &loaded, &unique_pairs)?;
    }
    remove_file(&path);
    Ok(())
}

fn assert_random_direct_equivalence(
    original: &ZanzibarService,
    loaded: &ZanzibarService,
    unique_pairs: &BTreeSet<(u8, u8)>,
) -> Result<(), Box<dyn std::error::Error>> {
    let viewer = Relation("viewer".to_string());
    for (doc_id, user_id) in unique_pairs {
        let object = doc(&format!("random_doc_{doc_id}"));
        let user = User::UserId(format!("user_{user_id}"));
        assert_eq!(
            original.check(&object, &viewer, &user)?,
            loaded.check(&object, &viewer, &user)?,
        );
    }

    for doc_id in 0_u8..16 {
        let object = doc(&format!("random_doc_{doc_id}"));
        assert_eq!(
            original.expand(&object, &viewer)?,
            loaded.expand(&object, &viewer)?,
        );
    }

    for user_id in 0_u8..16 {
        let request = LookupResourcesRequest {
            subject: User::UserId(format!("user_{user_id}")),
            permission: viewer.clone(),
            resource_type: "doc".to_string(),
        };
        assert_eq!(
            original.lookup_resources(&request)?,
            loaded.lookup_resources(&request)?,
        );
    }

    Ok(())
}

fn populated_service() -> Result<
    (ZanzibarService, simple_zanzibar::revision::ConsistencyToken),
    Box<dyn std::error::Error>,
> {
    let mut service = ZanzibarService::new().with_evaluation_limits(EvaluationLimits {
        max_depth: non_zero_u32(32),
        max_fanout_per_step: non_zero_u32(10_000),
        max_lookup_results: non_zero_u32(1_000),
    });
    let mut token = service.add_dsl_with_token(schema())?;
    for relationship in [
        "group:eng#member@user:alice",
        "folder:root#viewer@group:eng#member",
        "doc:inherited_doc#parent@folder:root#inherited_viewer",
        "doc:direct_doc#viewer@group:eng#member",
        "doc:denied_doc#viewer@group:eng#member",
        "doc:denied_doc#banned@user:alice",
        "doc:other_doc#viewer@user:carol",
    ] {
        let parsed = relationship.parse()?;
        token = service.apply_relationship_mutations([RelationshipMutation::Touch(parsed)], [])?;
    }
    Ok((service, token))
}

fn snapshot_bytes() -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let service = populated_service()?.0;
    let path = temp_snapshot_path("bytes");
    service.save_snapshot(&path, SnapshotSaveOptions::default())?;
    let bytes = fs::read(&path)?;
    remove_file(&path);
    Ok(bytes)
}

fn tiny_service() -> Result<ZanzibarService, Box<dyn std::error::Error>> {
    let mut service = ZanzibarService::new();
    service.add_dsl(
        r"
    namespace doc {
        relation viewer {}
    }
    ",
    )?;
    service.write_tuple_with_token(&simple_zanzibar::model::RelationTuple {
        object: doc("readme"),
        relation: Relation("viewer".to_string()),
        user: User::UserId("alice".to_string()),
    })?;
    Ok(service)
}

fn assert_corrupt_rejected(name: &str, bytes: &[u8]) -> Result<(), Box<dyn std::error::Error>> {
    let path = temp_snapshot_path(name);
    fs::write(&path, bytes)?;
    let result = ZanzibarService::load_snapshot(&path, SnapshotLoadOptions::default());
    remove_file(&path);
    assert!(matches!(
        result,
        Err(SnapshotIoError::Format { .. } | SnapshotIoError::Domain { .. })
    ));
    Ok(())
}

fn schema() -> &'static str {
    r#"
    namespace group {
        relation member {}
    }

    namespace folder {
        relation viewer {}
        relation inherited_viewer {
            rewrite computed_userset(relation: "viewer")
        }
    }

    namespace doc {
        relation parent {}
        relation viewer {}
        relation banned {}
        relation can_view {
            rewrite exclusion(
                union(
                    computed_userset(relation: "viewer"),
                    tuple_to_userset(tupleset: "parent", computed_userset: "inherited_viewer")
                ),
                computed_userset(relation: "banned")
            )
        }
    }
    "#
}

fn doc(id: &str) -> Object {
    Object {
        namespace: "doc".to_string(),
        id: id.to_string(),
    }
}

fn temp_snapshot_path(name: &str) -> PathBuf {
    let counter = NEXT_TEST_FILE.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "simple_zanzibar_{name}_{}_{}.szsnap",
        process::id(),
        counter,
    ))
}

fn remove_file(path: &Path) {
    let _ = fs::remove_file(path);
}

fn section_range(
    bytes: &[u8],
    section_kind: u16,
) -> Result<std::ops::Range<usize>, Box<dyn std::error::Error>> {
    let section_count = usize::try_from(read_u32(bytes, SECTION_COUNT_OFFSET)?)?;
    for index in 0..section_count {
        let entry = HEADER_LEN
            .checked_add(
                index
                    .checked_mul(DIRECTORY_ENTRY_LEN)
                    .ok_or("entry overflow")?,
            )
            .ok_or("entry overflow")?;
        if read_u16(bytes, entry)? == section_kind {
            let start = usize::try_from(read_u64(bytes, entry + 4)?)?;
            let len = usize::try_from(read_u64(bytes, entry + 12)?)?;
            let end = start.checked_add(len).ok_or("section end overflowed")?;
            return Ok(start..end);
        }
    }
    Err(format!("section {section_kind} not found").into())
}

fn directory_entry_offset(section_kind: u16) -> Result<usize, Box<dyn std::error::Error>> {
    let bytes = snapshot_bytes()?;
    let section_count = usize::try_from(read_u32(&bytes, SECTION_COUNT_OFFSET)?)?;
    for index in 0..section_count {
        let entry = HEADER_LEN
            .checked_add(
                index
                    .checked_mul(DIRECTORY_ENTRY_LEN)
                    .ok_or("entry overflow")?,
            )
            .ok_or("entry overflow")?;
        if read_u16(&bytes, entry)? == section_kind {
            return Ok(entry);
        }
    }
    Err(format!("section {section_kind} not found").into())
}

fn first_index_key_count(bytes: &[u8]) -> Result<u32, Box<dyn std::error::Error>> {
    let directory = section_range(bytes, SECTION_KIND_INDEX_DIRECTORY)?;
    read_u32(bytes, directory.start + 8)
}

fn copy_range(
    bytes: &[u8],
    offset: usize,
    len: usize,
) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let end = offset.checked_add(len).ok_or("range overflowed")?;
    Ok(bytes
        .get(offset..end)
        .ok_or("range out of bounds")?
        .to_vec())
}

fn set_range(
    bytes: &mut [u8],
    offset: usize,
    value: &[u8],
) -> Result<(), Box<dyn std::error::Error>> {
    let end = offset.checked_add(value.len()).ok_or("range overflowed")?;
    bytes
        .get_mut(offset..end)
        .ok_or("range out of bounds")?
        .copy_from_slice(value);
    Ok(())
}

fn set_byte(bytes: &mut [u8], offset: usize, value: u8) -> Result<(), Box<dyn std::error::Error>> {
    let target = bytes.get_mut(offset).ok_or("byte offset out of bounds")?;
    *target = value;
    Ok(())
}

fn set_u16(bytes: &mut [u8], offset: usize, value: u16) -> Result<(), Box<dyn std::error::Error>> {
    set_range(bytes, offset, &value.to_le_bytes())
}

fn set_u32(bytes: &mut [u8], offset: usize, value: u32) -> Result<(), Box<dyn std::error::Error>> {
    set_range(bytes, offset, &value.to_le_bytes())
}

fn set_u64(bytes: &mut [u8], offset: usize, value: u64) -> Result<(), Box<dyn std::error::Error>> {
    set_range(bytes, offset, &value.to_le_bytes())
}

fn read_u16(bytes: &[u8], offset: usize) -> Result<u16, Box<dyn std::error::Error>> {
    let mut array = [0_u8; 2];
    array.copy_from_slice(copy_range(bytes, offset, 2)?.as_slice());
    Ok(u16::from_le_bytes(array))
}

fn read_u32(bytes: &[u8], offset: usize) -> Result<u32, Box<dyn std::error::Error>> {
    let mut array = [0_u8; 4];
    array.copy_from_slice(copy_range(bytes, offset, 4)?.as_slice());
    Ok(u32::from_le_bytes(array))
}

fn read_u64(bytes: &[u8], offset: usize) -> Result<u64, Box<dyn std::error::Error>> {
    let mut array = [0_u8; 8];
    array.copy_from_slice(copy_range(bytes, offset, 8)?.as_slice());
    Ok(u64::from_le_bytes(array))
}

fn decode_hex(value: &str) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let trimmed = value.trim();
    if !trimmed.len().is_multiple_of(2) {
        return Err("hex fixture has odd length".into());
    }
    let mut bytes = Vec::with_capacity(trimmed.len() / 2);
    let mut index = 0;
    while index < trimmed.len() {
        let end = index.checked_add(2).ok_or("hex index overflowed")?;
        let pair = trimmed.get(index..end).ok_or("hex range out of bounds")?;
        bytes.push(u8::from_str_radix(pair, 16)?);
        index = end;
    }
    Ok(bytes)
}

fn rewrite_checksum(bytes: &mut [u8]) -> Result<(), Box<dyn std::error::Error>> {
    let footer_start = bytes
        .len()
        .checked_sub(FOOTER_LEN)
        .ok_or("missing footer")?;
    let digest = blake3::hash(bytes.get(..footer_start).ok_or("footer offset invalid")?);
    bytes
        .get_mut(footer_start..)
        .ok_or("footer out of bounds")?
        .copy_from_slice(digest.as_bytes());
    Ok(())
}

fn non_zero_u32(value: u32) -> NonZeroU32 {
    match NonZeroU32::new(value) {
        Some(value) => value,
        None => NonZeroU32::MIN,
    }
}

fn non_zero_u64(value: u64) -> NonZeroU64 {
    match NonZeroU64::new(value) {
        Some(value) => value,
        None => NonZeroU64::MIN,
    }
}
