#[cfg(feature = "bench-internals")]
use std::sync::Mutex;
use std::{collections::HashSet, fs, num::NonZeroU64, ops::Range, path::PathBuf, process};

use proptest::prelude::*;
#[cfg(feature = "bench-internals")]
use simple_zanzibar::eval::{evaluation_read_counters, reset_evaluation_read_counters};
#[cfg(feature = "bench-internals")]
use simple_zanzibar::relationship::{reset_store_view_read_counters, store_view_read_counters};
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
#[cfg(feature = "bench-internals")]
static BENCH_COUNTER_TEST_LOCK: Mutex<()> = Mutex::new(());

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

#[test]
fn test_should_preserve_delta_tombstone_masking_for_lookup()
-> Result<(), Box<dyn std::error::Error>> {
    let engine = seeded_engine()?;
    engine.write_relationships([
        RelationshipMutation::delete("doc:one#viewer@user:alice")?,
        RelationshipMutation::touch("doc:three#viewer@user:alice")?,
    ])?;

    let resources = engine.lookup_resources(LookupResourcesRequest::new(
        User::user_id("alice"),
        relation("viewer"),
        "doc",
    ))?;

    let actual = resources.resources.into_iter().collect::<HashSet<_>>();
    let expected = HashSet::from([object("doc", "two"), object("doc", "three")]);
    assert_eq!(actual, expected);
    Ok(())
}

#[test]
fn test_should_keep_plain_computed_shortcut_conservative_for_deny_and_intersection()
-> Result<(), Box<dyn std::error::Error>> {
    let engine = ZanzibarEngine::builder().build();
    engine.add_dsl(
        r#"
        namespace doc {
            relation owner {}
            relation reviewer {}
            relation banned {}
            relation visible {
                rewrite exclusion(
                    computed_userset(relation: "owner"),
                    computed_userset(relation: "banned")
                )
            }
            relation approved {
                rewrite intersection(
                    computed_userset(relation: "owner"),
                    computed_userset(relation: "reviewer")
                )
            }
        }
        "#,
    )?;
    engine.write_relationships([
        RelationshipMutation::touch("doc:one#owner@user:alice")?,
        RelationshipMutation::touch("doc:one#banned@user:alice")?,
        RelationshipMutation::touch("doc:one#reviewer@user:bob")?,
    ])?;

    assert!(!engine.check_relation(
        &object("doc", "one"),
        &relation("visible"),
        &User::user_id("alice"),
    )?);
    assert!(!engine.check_relation(
        &object("doc", "one"),
        &relation("approved"),
        &User::user_id("alice"),
    )?);
    Ok(())
}

#[test]
fn test_should_not_leak_reusable_lookup_context_between_candidates()
-> Result<(), Box<dyn std::error::Error>> {
    let engine = ZanzibarEngine::builder().build();
    engine.add_dsl(
        r#"
        namespace doc {
            relation viewer {}
            relation can_view {
                rewrite computed_userset(relation: "viewer")
            }
        }
        "#,
    )?;
    engine.write_relationships([
        RelationshipMutation::touch("doc:one#viewer@user:alice")?,
        RelationshipMutation::touch("doc:two#viewer@user:alice")?,
    ])?;

    let resources = engine.lookup_resources(LookupResourcesRequest::new(
        User::user_id("alice"),
        relation("can_view"),
        "doc",
    ))?;

    assert_eq!(
        resources.resources,
        vec![object("doc", "one"), object("doc", "two")]
    );
    Ok(())
}

#[test]
fn test_should_preserve_cycle_denial_with_compiled_relation_ids()
-> Result<(), Box<dyn std::error::Error>> {
    let engine = ZanzibarEngine::builder().build();
    engine.add_dsl(
        r#"
        namespace doc {
            relation first {
                rewrite computed_userset(relation: "second")
            }
            relation second {
                rewrite computed_userset(relation: "first")
            }
        }
        "#,
    )?;

    assert!(!engine.check_relation(
        &object("doc", "one"),
        &relation("first"),
        &User::user_id("alice"),
    )?);
    Ok(())
}

#[cfg(feature = "bench-internals")]
#[test]
fn test_should_record_lookup_and_memo_opportunity_counters()
-> Result<(), Box<dyn std::error::Error>> {
    let _guard = BENCH_COUNTER_TEST_LOCK
        .lock()
        .map_err(|_| "bench counter test lock poisoned")?;
    let engine = ZanzibarEngine::builder().build();
    engine.add_dsl(
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
            relation can_view {
                rewrite tuple_to_userset(tupleset: "parent", computed_userset: "inherited_viewer")
            }
        }
        "#,
    )?;
    engine.write_relationships([
        RelationshipMutation::touch("group:shared#member@user:alice")?,
        RelationshipMutation::touch("folder:shared#viewer@group:shared#member")?,
        RelationshipMutation::touch("doc:one#parent@folder:shared#viewer")?,
        RelationshipMutation::touch("doc:two#parent@folder:shared#viewer")?,
    ])?;

    reset_evaluation_read_counters();
    let resources = engine.lookup_resources(LookupResourcesRequest::new(
        User::user_id("alice"),
        relation("can_view"),
        "doc",
    ))?;
    let counters = evaluation_read_counters();

    assert_eq!(resources.resources.len(), 2);
    assert!(counters.lookup_resources_candidate_resources >= 2);
    assert!(counters.lookup_resources_full_root_checks >= 2);
    assert!(counters.check_memo_hit_opportunities > 0);
    assert!(counters.check_memo_hits > 0);
    assert!(counters.check_memo_inserts > 0);
    Ok(())
}

#[test]
fn test_should_lookup_resources_through_tuple_to_userset_computed_subject_relation()
-> Result<(), Box<dyn std::error::Error>> {
    let engine = ZanzibarEngine::builder().build();
    engine.add_dsl(
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
            relation can_view {
                rewrite tuple_to_userset(tupleset: "parent", computed_userset: "inherited_viewer")
            }
        }
        "#,
    )?;
    engine.write_relationships([
        RelationshipMutation::touch("group:shared#member@user:alice")?,
        RelationshipMutation::touch("folder:shared#viewer@group:shared#member")?,
        RelationshipMutation::touch("doc:inherited#parent@folder:shared#inherited_viewer")?,
    ])?;

    assert!(engine.check_relation(
        &object("doc", "inherited"),
        &relation("can_view"),
        &User::user_id("alice"),
    )?);

    let resources = engine.lookup_resources(LookupResourcesRequest::new(
        User::user_id("alice"),
        relation("can_view"),
        "doc",
    ))?;

    assert_eq!(resources.resources, vec![object("doc", "inherited")]);
    Ok(())
}

#[test]
fn test_should_lookup_resources_when_tuple_to_userset_ignores_tuple_subject_relation()
-> Result<(), Box<dyn std::error::Error>> {
    let engine = ZanzibarEngine::builder().build();
    engine.add_dsl(
        r#"
        namespace group {
            relation member {}
        }

        namespace folder {
            relation owner {}
            relation viewer {}
        }

        namespace doc {
            relation parent {}
            relation can_view {
                rewrite tuple_to_userset(tupleset: "parent", computed_userset: "viewer")
            }
        }
        "#,
    )?;
    engine.write_relationships([
        RelationshipMutation::touch("group:shared#member@user:alice")?,
        RelationshipMutation::touch("folder:shared#viewer@group:shared#member")?,
        RelationshipMutation::touch("doc:inherited#parent@folder:shared#owner")?,
    ])?;

    assert!(engine.check_relation(
        &object("doc", "inherited"),
        &relation("can_view"),
        &User::user_id("alice"),
    )?);

    let resources = engine.lookup_resources(LookupResourcesRequest::new(
        User::user_id("alice"),
        relation("can_view"),
        "doc",
    ))?;

    assert_eq!(resources.resources, vec![object("doc", "inherited")]);
    Ok(())
}

#[test]
fn test_should_lookup_resources_with_object_specific_tuple_subject_relation_fallback()
-> Result<(), Box<dyn std::error::Error>> {
    let engine = ZanzibarEngine::builder().build();
    engine.add_dsl(
        r#"
        namespace group {
            relation member {}
        }

        namespace folder {
            relation owner {}
            relation viewer {}
        }

        namespace doc {
            relation parent {}
            relation can_view {
                rewrite tuple_to_userset(tupleset: "parent", computed_userset: "viewer")
            }
        }
        "#,
    )?;
    engine.write_relationships([
        RelationshipMutation::touch("group:shared#member@user:alice")?,
        RelationshipMutation::touch("folder:exact#viewer@group:shared#member")?,
        RelationshipMutation::touch("folder:fallback#viewer@group:shared#member")?,
        RelationshipMutation::touch("doc:exact#parent@folder:exact#viewer")?,
        RelationshipMutation::touch("doc:fallback#parent@folder:fallback#owner")?,
    ])?;

    let resources = engine.lookup_resources(LookupResourcesRequest::new(
        User::user_id("alice"),
        relation("can_view"),
        "doc",
    ))?;

    assert_eq!(
        resources.resources,
        vec![object("doc", "exact"), object("doc", "fallback")]
    );
    Ok(())
}

#[cfg(feature = "bench-internals")]
#[test]
fn test_should_not_cache_active_cycle_denials_in_request_memo()
-> Result<(), Box<dyn std::error::Error>> {
    let _guard = BENCH_COUNTER_TEST_LOCK
        .lock()
        .map_err(|_| "bench counter test lock poisoned")?;
    let engine = ZanzibarEngine::builder().build();
    engine.add_dsl(
        r#"
        namespace doc {
            relation seed {}
            relation first {
                rewrite computed_userset(relation: "second")
            }
            relation second {
                rewrite computed_userset(relation: "first")
            }
        }
        "#,
    )?;
    engine.write_relationships([RelationshipMutation::touch("doc:loop#seed@user:alice")?])?;

    reset_evaluation_read_counters();
    let resources = engine.lookup_resources(LookupResourcesRequest::new(
        User::user_id("alice"),
        relation("first"),
        "doc",
    ))?;
    let counters = evaluation_read_counters();

    assert!(resources.resources.is_empty());
    assert!(counters.check_active_cycle_denials > 0);
    assert!(counters.check_memo_active_cycle_skips > 0);
    Ok(())
}

#[test]
fn test_should_stream_lookup_subjects_through_base_side_of_exclusion()
-> Result<(), Box<dyn std::error::Error>> {
    let engine = ZanzibarEngine::builder().build();
    engine.add_dsl(
        r#"
        namespace doc {
            relation viewer {}
            relation banned {}
            relation can_view {
                rewrite exclusion(
                    computed_userset(relation: "viewer"),
                    computed_userset(relation: "banned")
                )
            }
        }
        "#,
    )?;
    engine.write_relationships([
        RelationshipMutation::touch("doc:one#viewer@user:alice")?,
        RelationshipMutation::touch("doc:one#viewer@user:bob")?,
        RelationshipMutation::touch("doc:one#banned@user:bob")?,
        RelationshipMutation::touch("doc:one#banned@user:charlie")?,
    ])?;

    let subjects = engine.lookup_subjects(LookupSubjectsRequest::new(
        object("doc", "one"),
        relation("can_view"),
        "user",
    ))?;

    assert_eq!(subjects.subjects, vec![User::user_id("alice")]);
    Ok(())
}

#[test]
fn test_should_verify_lookup_subjects_when_nested_userset_relation_has_exclusion()
-> Result<(), Box<dyn std::error::Error>> {
    let engine = ZanzibarEngine::builder().build();
    engine.add_dsl(
        r#"
        namespace group {
            relation member_base {}
            relation banned {}
            relation member {
                rewrite exclusion(
                    computed_userset(relation: "member_base"),
                    computed_userset(relation: "banned")
                )
            }
        }

        namespace doc {
            relation viewer {}
        }
        "#,
    )?;
    engine.write_relationships([
        RelationshipMutation::touch("doc:one#viewer@group:eng#member")?,
        RelationshipMutation::touch("group:eng#member_base@user:alice")?,
        RelationshipMutation::touch("group:eng#member_base@user:bob")?,
        RelationshipMutation::touch("group:eng#banned@user:bob")?,
    ])?;

    let subjects = engine.lookup_subjects(LookupSubjectsRequest::new(
        object("doc", "one"),
        relation("viewer"),
        "user",
    ))?;

    assert_eq!(subjects.subjects, vec![User::user_id("alice")]);
    Ok(())
}

#[cfg(feature = "bench-internals")]
#[test]
fn test_should_not_expand_exclusion_only_subjects_as_lookup_subject_candidates()
-> Result<(), Box<dyn std::error::Error>> {
    let _guard = BENCH_COUNTER_TEST_LOCK
        .lock()
        .map_err(|_| "bench counter test lock poisoned")?;
    let engine = ZanzibarEngine::builder().build();
    engine.add_dsl(
        r#"
        namespace doc {
            relation viewer {}
            relation banned {}
            relation can_view {
                rewrite exclusion(
                    computed_userset(relation: "viewer"),
                    computed_userset(relation: "banned")
                )
            }
        }
        "#,
    )?;
    engine.write_relationships([
        RelationshipMutation::touch("doc:one#viewer@user:alice")?,
        RelationshipMutation::touch("doc:one#banned@user:bob")?,
    ])?;

    reset_evaluation_read_counters();
    let subjects = engine.lookup_subjects(LookupSubjectsRequest::new(
        object("doc", "one"),
        relation("can_view"),
        "user",
    ))?;
    let counters = evaluation_read_counters();

    assert_eq!(subjects.subjects, vec![User::user_id("alice")]);
    assert!(counters.lookup_subjects_candidate_subjects >= 1);
    assert!(counters.lookup_subjects_full_root_checks >= 1);
    Ok(())
}

#[cfg(feature = "bench-internals")]
#[test]
fn test_should_return_direct_lookup_resource_without_full_root_check()
-> Result<(), Box<dyn std::error::Error>> {
    let _guard = BENCH_COUNTER_TEST_LOCK
        .lock()
        .map_err(|_| "bench counter test lock poisoned")?;
    let engine = ZanzibarEngine::builder().build();
    engine.add_dsl(
        r#"
        namespace doc {
            relation viewer {}
            relation can_view {
                rewrite computed_userset(relation: "viewer")
            }
        }
        "#,
    )?;
    engine.write_relationships([RelationshipMutation::touch("doc:direct#viewer@user:alice")?])?;

    reset_evaluation_read_counters();
    let resources = engine.lookup_resources(LookupResourcesRequest::new(
        User::user_id("alice"),
        relation("can_view"),
        "doc",
    ))?;
    let counters = evaluation_read_counters();

    assert_eq!(resources.resources, vec![object("doc", "direct")]);
    assert_eq!(counters.lookup_resources_full_root_checks, 0);
    assert_eq!(counters.lookup_resources_proven_without_check, 1);
    Ok(())
}

#[cfg(feature = "bench-internals")]
#[test]
fn test_should_verify_lookup_resources_exclusion_with_residual_only()
-> Result<(), Box<dyn std::error::Error>> {
    let _guard = BENCH_COUNTER_TEST_LOCK
        .lock()
        .map_err(|_| "bench counter test lock poisoned")?;
    let engine = ZanzibarEngine::builder().build();
    engine.add_dsl(
        r#"
        namespace doc {
            relation viewer {}
            relation banned {}
            relation can_view {
                rewrite exclusion(
                    computed_userset(relation: "viewer"),
                    computed_userset(relation: "banned")
                )
            }
        }
        "#,
    )?;
    engine.write_relationships([
        RelationshipMutation::touch("doc:allowed#viewer@user:alice")?,
        RelationshipMutation::touch("doc:blocked#viewer@user:alice")?,
        RelationshipMutation::touch("doc:blocked#banned@user:alice")?,
    ])?;

    reset_evaluation_read_counters();
    let resources = engine.lookup_resources(LookupResourcesRequest::new(
        User::user_id("alice"),
        relation("can_view"),
        "doc",
    ))?;
    let counters = evaluation_read_counters();

    assert_eq!(resources.resources, vec![object("doc", "allowed")]);
    assert_eq!(counters.lookup_resources_full_root_checks, 0);
    assert!(counters.lookup_resources_residual_checks >= 2);
    Ok(())
}

#[cfg(feature = "bench-internals")]
#[test]
fn test_should_keep_userset_lookup_resource_on_full_root_verification()
-> Result<(), Box<dyn std::error::Error>> {
    let _guard = BENCH_COUNTER_TEST_LOCK
        .lock()
        .map_err(|_| "bench counter test lock poisoned")?;
    let engine = ZanzibarEngine::builder().build();
    engine.add_dsl(
        r#"
        namespace group {
            relation member {}
        }

        namespace doc {
            relation viewer {}
            relation can_view {
                rewrite computed_userset(relation: "viewer")
            }
        }
        "#,
    )?;
    engine.write_relationships([
        RelationshipMutation::touch("group:eng#member@user:alice")?,
        RelationshipMutation::touch("doc:nested#viewer@group:eng#member")?,
    ])?;

    reset_evaluation_read_counters();
    let resources = engine.lookup_resources(LookupResourcesRequest::new(
        User::user_id("alice"),
        relation("can_view"),
        "doc",
    ))?;
    let counters = evaluation_read_counters();

    assert_eq!(resources.resources, vec![object("doc", "nested")]);
    assert_eq!(counters.lookup_resources_proven_without_check, 0);
    assert!(counters.lookup_resources_full_root_checks >= 1);
    Ok(())
}

#[cfg(feature = "bench-internals")]
#[test]
fn test_should_prune_lookup_resources_candidates_from_exclusion_only_relations()
-> Result<(), Box<dyn std::error::Error>> {
    let _guard = BENCH_COUNTER_TEST_LOCK
        .lock()
        .map_err(|_| "bench counter test lock poisoned")?;
    let engine = ZanzibarEngine::builder().build();
    engine.add_dsl(
        r#"
        namespace doc {
            relation viewer {}
            relation banned {}
            relation can_view {
                rewrite exclusion(
                    computed_userset(relation: "viewer"),
                    computed_userset(relation: "banned")
                )
            }
        }
        "#,
    )?;
    engine.write_relationships([
        RelationshipMutation::touch("doc:blocked_one#banned@user:alice")?,
        RelationshipMutation::touch("doc:blocked_two#banned@user:alice")?,
    ])?;

    reset_evaluation_read_counters();
    let resources = engine.lookup_resources(LookupResourcesRequest::new(
        User::user_id("alice"),
        relation("can_view"),
        "doc",
    ))?;
    let counters = evaluation_read_counters();

    assert!(resources.resources.is_empty());
    assert!(counters.lookup_resources_schema_pruned >= 2);
    Ok(())
}

#[cfg(feature = "bench-internals")]
#[test]
fn test_should_plan_tuple_to_userset_root_relation_without_fallback()
-> Result<(), Box<dyn std::error::Error>> {
    let _guard = BENCH_COUNTER_TEST_LOCK
        .lock()
        .map_err(|_| "bench counter test lock poisoned")?;
    let engine = ZanzibarEngine::builder().build();
    engine.add_dsl(
        r#"
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
        "#,
    )?;
    engine.write_relationships([RelationshipMutation::touch(
        "doc:blocked#banned@user:alice",
    )?])?;

    reset_evaluation_read_counters();
    let resources = engine.lookup_resources(LookupResourcesRequest::new(
        User::user_id("alice"),
        relation("can_view"),
        "doc",
    ))?;
    let counters = evaluation_read_counters();

    assert!(resources.resources.is_empty());
    assert_eq!(counters.lookup_resources_planner_fallbacks, 0);
    assert!(counters.lookup_resources_schema_pruned >= 1);
    Ok(())
}

#[test]
fn test_should_continue_lookup_resources_frontier_from_pruned_target_type_userset()
-> Result<(), Box<dyn std::error::Error>> {
    let engine = ZanzibarEngine::builder().build();
    engine.add_dsl(
        r#"
        namespace group {
            relation member {}
        }

        namespace doc {
            relation seed {}
            relation viewer {}
            relation can_view {
                rewrite computed_userset(relation: "viewer")
            }
        }
        "#,
    )?;
    engine.write_relationships([
        RelationshipMutation::touch("doc:seed#seed@user:alice")?,
        RelationshipMutation::touch("group:eng#member@doc:seed#seed")?,
        RelationshipMutation::touch("doc:target#viewer@group:eng#member")?,
    ])?;

    assert!(engine.check_relation(
        &object("doc", "target"),
        &relation("can_view"),
        &User::user_id("alice"),
    )?);

    let resources = engine.lookup_resources(LookupResourcesRequest::new(
        User::user_id("alice"),
        relation("can_view"),
        "doc",
    ))?;

    assert_eq!(resources.resources, vec![object("doc", "target")]);
    Ok(())
}

#[cfg(feature = "bench-internals")]
#[test]
fn test_should_verify_lookup_resources_intersection_with_residual_guard()
-> Result<(), Box<dyn std::error::Error>> {
    let _guard = BENCH_COUNTER_TEST_LOCK
        .lock()
        .map_err(|_| "bench counter test lock poisoned")?;
    let engine = ZanzibarEngine::builder().build();
    engine.add_dsl(
        r#"
        namespace doc {
            relation viewer {}
            relation reviewer {}
            relation can_view {
                rewrite intersection(
                    computed_userset(relation: "viewer"),
                    computed_userset(relation: "reviewer")
                )
            }
        }
        "#,
    )?;
    engine.write_relationships([
        RelationshipMutation::touch("doc:allowed#viewer@user:alice")?,
        RelationshipMutation::touch("doc:allowed#reviewer@user:alice")?,
        RelationshipMutation::touch("doc:needs_review#viewer@user:alice")?,
    ])?;

    reset_evaluation_read_counters();
    let resources = engine.lookup_resources(LookupResourcesRequest::new(
        User::user_id("alice"),
        relation("can_view"),
        "doc",
    ))?;
    let counters = evaluation_read_counters();

    assert_eq!(resources.resources, vec![object("doc", "allowed")]);
    assert!(counters.lookup_resources_candidate_resources >= 2);
    assert_eq!(counters.lookup_resources_full_root_checks, 0);
    assert!(counters.lookup_resources_residual_checks >= 2);
    assert_eq!(counters.lookup_resources_planner_fallbacks, 0);
    Ok(())
}

#[cfg(feature = "bench-internals")]
#[test]
fn test_should_report_delta_stats_and_read_counters() -> Result<(), Box<dyn std::error::Error>> {
    let _guard = BENCH_COUNTER_TEST_LOCK
        .lock()
        .map_err(|_| "bench counter test lock poisoned")?;
    let engine = seeded_engine()?;
    let path = unique_snapshot_path("bench-delta-stats");
    engine.save_snapshot(&path, SnapshotSaveOptions::default())?;
    let loaded = ZanzibarEngine::load_snapshot(&path, SnapshotLoadOptions::default())?;
    fs::remove_file(path)?;
    loaded.write_relationships([RelationshipMutation::delete("doc:one#viewer@user:alice")?])?;

    reset_store_view_read_counters();
    assert!(loaded.check_relation(
        &object("doc", "two"),
        &relation("viewer"),
        &User::user_id("alice"),
    )?);
    let counters = store_view_read_counters();
    let stats = loaded.bench_relationship_delta_stats(Consistency::Latest)?;

    assert!(counters.query_calls > 0);
    assert!(counters.delta_segments_inspected > 0);
    assert!(counters.tombstone_checks > 0);
    assert_eq!(stats.delta_segments, 1);
    assert_eq!(stats.delta_deleted_rows, 1);
    assert_eq!(stats.delta_mutations, 1);
    Ok(())
}

#[cfg(feature = "bench-internals")]
#[test]
fn test_should_report_posting_histograms() -> Result<(), Box<dyn std::error::Error>> {
    let _guard = BENCH_COUNTER_TEST_LOCK
        .lock()
        .map_err(|_| "bench counter test lock poisoned")?;
    let engine = seeded_engine()?;
    let path = unique_snapshot_path("bench-posting-histograms");
    engine.save_snapshot(&path, SnapshotSaveOptions::default())?;
    let loaded = ZanzibarEngine::load_snapshot(&path, SnapshotLoadOptions::default())?;
    fs::remove_file(path)?;
    loaded.write_relationships([RelationshipMutation::touch("doc:one#viewer@user:bob")?])?;

    let histograms = loaded.bench_relationship_posting_histograms(Consistency::Latest)?;

    assert!(histograms.resource.keys > 0);
    assert!(histograms.resource.total_postings > 0);
    assert!(histograms.subject.total_postings >= histograms.resource.total_postings);
    assert!(histograms.subject_type.max_posting_len > 0);
    assert_eq!(histograms.resource.keys, 4);
    assert_eq!(histograms.resource.total_postings, 5);
    assert_eq!(histograms.resource.max_posting_len, 2);
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
