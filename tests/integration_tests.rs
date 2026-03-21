//! Integration tests for the Zanzibar service.
//!
//! These tests model real-world authorization systems end-to-end:
//! DSL parsing → tuple writing → authorization checks.

use simple_zanzibar::{
    ZanzibarService,
    model::{Object, Relation, RelationTuple, User},
};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn tuple(ns: &str, id: &str, rel: &str, user_id: &str) -> RelationTuple {
    RelationTuple::new(
        Object::new(ns, id),
        Relation::new(rel),
        User::user_id(user_id),
    )
}

fn parent_tuple(ns: &str, id: &str, parent_ns: &str, parent_id: &str, rel: &str) -> RelationTuple {
    RelationTuple::new(
        Object::new(ns, id),
        Relation::new("parent"),
        User::userset(Object::new(parent_ns, parent_id), Relation::new(rel)),
    )
}

// ---------------------------------------------------------------------------
// 1. Google-Drive-like document system
// ---------------------------------------------------------------------------

const GOOGLE_DRIVE_DSL: &str = r#"
    namespace org {
        relation member {}
    }

    namespace folder {
        relation owner {}
        relation parent {}

        relation viewer {
            rewrite union(
                this,
                computed_userset(relation: "owner"),
                tuple_to_userset(tupleset: "parent", computed_userset: "viewer")
            )
        }

        relation editor {
            rewrite union(
                this,
                computed_userset(relation: "owner"),
                tuple_to_userset(tupleset: "parent", computed_userset: "editor")
            )
        }
    }

    namespace doc {
        relation owner {}
        relation parent {}

        relation viewer {
            rewrite union(
                this,
                computed_userset(relation: "owner"),
                computed_userset(relation: "editor"),
                tuple_to_userset(tupleset: "parent", computed_userset: "viewer")
            )
        }

        relation editor {
            rewrite union(
                this,
                computed_userset(relation: "owner"),
                tuple_to_userset(tupleset: "parent", computed_userset: "editor")
            )
        }
    }
"#;

#[test]
fn test_should_model_google_drive_permissions() -> Result<(), Box<dyn std::error::Error>> {
    let mut svc = ZanzibarService::new();
    svc.add_dsl(GOOGLE_DRIVE_DSL)?;

    //  org:acme
    //    └─ folder:root  (owner: alice)
    //         └─ folder:eng  (viewer: bob)
    //              └─ doc:design  (editor: charlie)

    svc.write_tuple(tuple("folder", "root", "owner", "alice"))?;
    svc.write_tuple(parent_tuple("folder", "eng", "folder", "root", "viewer"))?;
    svc.write_tuple(tuple("folder", "eng", "viewer", "bob"))?;
    svc.write_tuple(parent_tuple("doc", "design", "folder", "eng", "viewer"))?;
    svc.write_tuple(tuple("doc", "design", "editor", "charlie"))?;

    let doc = Object::new("doc", "design");
    let viewer = Relation::new("viewer");
    let editor = Relation::new("editor");

    // Alice: owner of root → viewer of eng (inherited) → viewer of doc (inherited)
    assert!(svc.check(&doc, &viewer, &User::user_id("alice"))?);
    // Alice: owner of root → editor of eng → editor of doc (inherited)
    assert!(svc.check(&doc, &editor, &User::user_id("alice"))?);

    // Bob: direct viewer of eng → viewer of doc (inherited)
    assert!(svc.check(&doc, &viewer, &User::user_id("bob"))?);
    // Bob: NOT an editor of eng → NOT editor of doc
    assert!(!svc.check(&doc, &editor, &User::user_id("bob"))?);

    // Charlie: direct editor of doc → also viewer of doc
    assert!(svc.check(&doc, &viewer, &User::user_id("charlie"))?);
    assert!(svc.check(&doc, &editor, &User::user_id("charlie"))?);

    // Dave: no permissions anywhere
    assert!(!svc.check(&doc, &viewer, &User::user_id("dave"))?);
    assert!(!svc.check(&doc, &editor, &User::user_id("dave"))?);

    Ok(())
}

// ---------------------------------------------------------------------------
// 2. RBAC with group membership and banned users (exclusion)
// ---------------------------------------------------------------------------

const RBAC_BANNED_DSL: &str = r#"
    namespace group {
        relation member {}
    }

    namespace resource {
        relation group_access {}
        relation banned {}

        relation viewer {
            rewrite exclusion(
                union(
                    this,
                    tuple_to_userset(tupleset: "group_access", computed_userset: "member")
                ),
                computed_userset(relation: "banned")
            )
        }
    }
"#;

#[test]
fn test_should_model_rbac_with_banned_users() -> Result<(), Box<dyn std::error::Error>> {
    let mut svc = ZanzibarService::new();
    svc.add_dsl(RBAC_BANNED_DSL)?;

    // group:eng has members alice, bob, eve
    svc.write_tuple(tuple("group", "eng", "member", "alice"))?;
    svc.write_tuple(tuple("group", "eng", "member", "bob"))?;
    svc.write_tuple(tuple("group", "eng", "member", "eve"))?;

    // resource:secret grants access to group:eng
    svc.write_tuple(RelationTuple::new(
        Object::new("resource", "secret"),
        Relation::new("group_access"),
        User::userset(Object::new("group", "eng"), Relation::new("member")),
    ))?;

    // eve is banned from resource:secret
    svc.write_tuple(tuple("resource", "secret", "banned", "eve"))?;

    // charlie has direct viewer access
    svc.write_tuple(tuple("resource", "secret", "viewer", "charlie"))?;

    let resource = Object::new("resource", "secret");
    let viewer = Relation::new("viewer");

    // alice: eng member, not banned → allowed
    assert!(svc.check(&resource, &viewer, &User::user_id("alice"))?);
    // bob: eng member, not banned → allowed
    assert!(svc.check(&resource, &viewer, &User::user_id("bob"))?);
    // eve: eng member, but banned → DENIED
    assert!(!svc.check(&resource, &viewer, &User::user_id("eve"))?);
    // charlie: direct viewer, not banned → allowed
    assert!(svc.check(&resource, &viewer, &User::user_id("charlie"))?);
    // dave: no access at all
    assert!(!svc.check(&resource, &viewer, &User::user_id("dave"))?);

    Ok(())
}

// ---------------------------------------------------------------------------
// 3. Deep folder nesting (4 levels)
// ---------------------------------------------------------------------------

const DEEP_NESTING_DSL: &str = r#"
    namespace folder {
        relation owner {}
        relation parent {}
        relation viewer {
            rewrite union(
                this,
                computed_userset(relation: "owner"),
                tuple_to_userset(tupleset: "parent", computed_userset: "viewer")
            )
        }
    }
"#;

#[test]
fn test_should_propagate_through_deep_nesting() -> Result<(), Box<dyn std::error::Error>> {
    let mut svc = ZanzibarService::new();
    svc.add_dsl(DEEP_NESTING_DSL)?;

    // folder:L1 → folder:L2 → folder:L3 → folder:L4
    svc.write_tuple(tuple("folder", "L1", "viewer", "alice"))?;
    svc.write_tuple(parent_tuple("folder", "L2", "folder", "L1", "viewer"))?;
    svc.write_tuple(parent_tuple("folder", "L3", "folder", "L2", "viewer"))?;
    svc.write_tuple(parent_tuple("folder", "L4", "folder", "L3", "viewer"))?;

    let viewer = Relation::new("viewer");

    // Alice's access should propagate through all 4 levels.
    assert!(svc.check(
        &Object::new("folder", "L1"),
        &viewer,
        &User::user_id("alice")
    )?);
    assert!(svc.check(
        &Object::new("folder", "L2"),
        &viewer,
        &User::user_id("alice")
    )?);
    assert!(svc.check(
        &Object::new("folder", "L3"),
        &viewer,
        &User::user_id("alice")
    )?);
    assert!(svc.check(
        &Object::new("folder", "L4"),
        &viewer,
        &User::user_id("alice")
    )?);

    // Bob has no access at any level.
    assert!(!svc.check(&Object::new("folder", "L4"), &viewer, &User::user_id("bob"))?);

    // Owner at L3 should propagate to L4 but NOT upward to L2/L1.
    svc.write_tuple(tuple("folder", "L3", "owner", "charlie"))?;
    assert!(svc.check(
        &Object::new("folder", "L4"),
        &viewer,
        &User::user_id("charlie")
    )?);
    assert!(svc.check(
        &Object::new("folder", "L3"),
        &viewer,
        &User::user_id("charlie")
    )?);
    assert!(!svc.check(
        &Object::new("folder", "L2"),
        &viewer,
        &User::user_id("charlie")
    )?);

    Ok(())
}

// ---------------------------------------------------------------------------
// 4. Intersection: team membership + direct grant required
// ---------------------------------------------------------------------------

const INTERSECTION_DSL: &str = r#"
    namespace team {
        relation member {}
    }

    namespace doc {
        relation team_ref {}
        relation viewer_candidate {}
        relation viewer {
            rewrite intersection(
                computed_userset(relation: "viewer_candidate"),
                tuple_to_userset(tupleset: "team_ref", computed_userset: "member")
            )
        }
    }
"#;

#[test]
fn test_should_require_both_conditions_for_intersection() -> Result<(), Box<dyn std::error::Error>>
{
    let mut svc = ZanzibarService::new();
    svc.add_dsl(INTERSECTION_DSL)?;

    // team:eng members: alice, bob
    svc.write_tuple(tuple("team", "eng", "member", "alice"))?;
    svc.write_tuple(tuple("team", "eng", "member", "bob"))?;

    // doc:spec is linked to team:eng
    svc.write_tuple(RelationTuple::new(
        Object::new("doc", "spec"),
        Relation::new("team_ref"),
        User::userset(Object::new("team", "eng"), Relation::new("member")),
    ))?;

    // Only alice is also a viewer_candidate
    svc.write_tuple(tuple("doc", "spec", "viewer_candidate", "alice"))?;
    // charlie is a viewer_candidate but NOT a team member
    svc.write_tuple(tuple("doc", "spec", "viewer_candidate", "charlie"))?;

    let doc = Object::new("doc", "spec");
    let viewer = Relation::new("viewer");

    // alice: team member AND viewer_candidate → allowed
    assert!(svc.check(&doc, &viewer, &User::user_id("alice"))?);
    // bob: team member but NOT viewer_candidate → denied
    assert!(!svc.check(&doc, &viewer, &User::user_id("bob"))?);
    // charlie: viewer_candidate but NOT team member → denied
    assert!(!svc.check(&doc, &viewer, &User::user_id("charlie"))?);

    Ok(())
}

// ---------------------------------------------------------------------------
// 5. Permission lifecycle: grant → verify → revoke → verify → re-grant
// ---------------------------------------------------------------------------

#[test]
fn test_should_handle_permission_lifecycle() -> Result<(), Box<dyn std::error::Error>> {
    let mut svc = ZanzibarService::new();
    svc.add_dsl("namespace doc { relation viewer {} }")?;

    let doc = Object::new("doc", "1");
    let viewer = Relation::new("viewer");
    let alice = User::user_id("alice");
    let t = RelationTuple::new(doc.clone(), viewer.clone(), alice.clone());

    // Phase 1: no access
    assert!(!svc.check(&doc, &viewer, &alice)?);

    // Phase 2: grant → access
    svc.write_tuple(t.clone())?;
    assert!(svc.check(&doc, &viewer, &alice)?);

    // Phase 3: revoke → no access
    svc.delete_tuple(&t)?;
    assert!(!svc.check(&doc, &viewer, &alice)?);

    // Phase 4: re-grant → access restored
    svc.write_tuple(t)?;
    assert!(svc.check(&doc, &viewer, &alice)?);

    Ok(())
}

// ---------------------------------------------------------------------------
// 6. Group membership (userset indirection)
// ---------------------------------------------------------------------------

const GROUP_MEMBERSHIP_DSL: &str = r#"
    namespace group {
        relation member {}
    }

    namespace doc {
        relation viewer {}
    }
"#;

#[test]
fn test_should_resolve_group_membership() -> Result<(), Box<dyn std::error::Error>> {
    let mut svc = ZanzibarService::new();
    svc.add_dsl(GROUP_MEMBERSHIP_DSL)?;

    // group:eng has alice and bob as members
    svc.write_tuple(tuple("group", "eng", "member", "alice"))?;
    svc.write_tuple(tuple("group", "eng", "member", "bob"))?;

    // doc:design viewer is group:eng#member (userset)
    svc.write_tuple(RelationTuple::new(
        Object::new("doc", "design"),
        Relation::new("viewer"),
        User::userset(Object::new("group", "eng"), Relation::new("member")),
    ))?;

    let doc = Object::new("doc", "design");
    let viewer = Relation::new("viewer");

    // alice and bob are group members → viewers
    assert!(svc.check(&doc, &viewer, &User::user_id("alice"))?);
    assert!(svc.check(&doc, &viewer, &User::user_id("bob"))?);
    // charlie is not a group member → no access
    assert!(!svc.check(&doc, &viewer, &User::user_id("charlie"))?);

    Ok(())
}

// ---------------------------------------------------------------------------
// 7. Cross-namespace with completely different schemas
// ---------------------------------------------------------------------------

#[test]
fn test_should_handle_cross_namespace_distinct_schemas() -> Result<(), Box<dyn std::error::Error>> {
    let mut svc = ZanzibarService::new();

    // 'project' namespace has admin/contributor — no viewer relation at all
    // 'ticket' namespace has assignee + parent → uses contributor from project
    svc.add_dsl(
        r#"
        namespace project {
            relation admin {}
            relation contributor {}
        }

        namespace ticket {
            relation assignee {}
            relation project_ref {}
            relation viewer {
                rewrite union(
                    computed_userset(relation: "assignee"),
                    tuple_to_userset(tupleset: "project_ref", computed_userset: "contributor")
                )
            }
        }
    "#,
    )?;

    svc.write_tuple(tuple("project", "alpha", "contributor", "alice"))?;
    svc.write_tuple(tuple("project", "alpha", "admin", "boss"))?;
    svc.write_tuple(tuple("ticket", "123", "assignee", "bob"))?;
    svc.write_tuple(RelationTuple::new(
        Object::new("ticket", "123"),
        Relation::new("project_ref"),
        User::userset(
            Object::new("project", "alpha"),
            Relation::new("contributor"),
        ),
    ))?;

    let ticket = Object::new("ticket", "123");
    let viewer = Relation::new("viewer");

    // bob: direct assignee → viewer
    assert!(svc.check(&ticket, &viewer, &User::user_id("bob"))?);
    // alice: project contributor → ticket viewer via tuple_to_userset
    assert!(svc.check(&ticket, &viewer, &User::user_id("alice"))?);
    // boss: project admin, but admin ≠ contributor → no ticket access
    assert!(!svc.check(&ticket, &viewer, &User::user_id("boss"))?);

    Ok(())
}

// ---------------------------------------------------------------------------
// 8. Error handling
// ---------------------------------------------------------------------------

#[test]
fn test_should_error_on_invalid_dsl() {
    let mut svc = ZanzibarService::new();
    assert!(svc.add_dsl("this is not valid DSL").is_err());
}

#[test]
fn test_should_error_on_unknown_namespace_check() {
    let svc = ZanzibarService::new();
    let result = svc.check(
        &Object::new("unknown", "1"),
        &Relation::new("viewer"),
        &User::user_id("alice"),
    );
    assert!(result.is_err());
}

#[test]
fn test_should_error_on_unknown_relation_check() {
    let mut svc = ZanzibarService::new();
    svc.add_dsl("namespace doc { relation owner {} }").unwrap();
    let result = svc.check(
        &Object::new("doc", "1"),
        &Relation::new("nonexistent"),
        &User::user_id("alice"),
    );
    assert!(result.is_err());
}

#[test]
fn test_should_error_on_duplicate_tuple() {
    let mut svc = ZanzibarService::new();
    svc.add_dsl("namespace doc { relation owner {} }").unwrap();
    let t = tuple("doc", "1", "owner", "alice");
    svc.write_tuple(t.clone()).unwrap();
    assert!(svc.write_tuple(t).is_err());
}

#[test]
fn test_should_error_on_delete_nonexistent_tuple() {
    let mut svc = ZanzibarService::new();
    svc.add_dsl("namespace doc { relation owner {} }").unwrap();
    let t = tuple("doc", "1", "owner", "alice");
    assert!(svc.delete_tuple(&t).is_err());
}

// ---------------------------------------------------------------------------
// 9. with_store / Default / config override
// ---------------------------------------------------------------------------

#[test]
fn test_should_create_service_with_default() {
    let svc = ZanzibarService::default();
    // Should work identically to ::new()
    let result = svc.check(
        &Object::new("doc", "1"),
        &Relation::new("viewer"),
        &User::user_id("alice"),
    );
    assert!(result.is_err()); // no namespace configured
}

#[test]
fn test_should_create_service_with_custom_store() {
    use simple_zanzibar::store::InMemoryTupleStore;

    let store = Box::new(InMemoryTupleStore::default());
    let mut svc = ZanzibarService::with_store(store);
    svc.add_dsl("namespace doc { relation viewer {} }").unwrap();
    svc.write_tuple(tuple("doc", "1", "viewer", "alice"))
        .unwrap();
    assert!(
        svc.check(
            &Object::new("doc", "1"),
            &Relation::new("viewer"),
            &User::user_id("alice"),
        )
        .unwrap()
    );
}

#[test]
fn test_should_override_config_on_readd() {
    let mut svc = ZanzibarService::new();

    // First: viewer = this (direct only)
    svc.add_dsl("namespace doc { relation owner {} relation viewer { rewrite this } }")
        .unwrap();
    svc.write_tuple(tuple("doc", "1", "owner", "alice"))
        .unwrap();
    // alice is owner but viewer=this means only direct → false
    assert!(
        !svc.check(
            &Object::new("doc", "1"),
            &Relation::new("viewer"),
            &User::user_id("alice"),
        )
        .unwrap()
    );

    // Override: viewer = union(this, computed_userset("owner"))
    svc.add_dsl(
        r#"namespace doc {
            relation owner {}
            relation viewer { rewrite union(this, computed_userset(relation: "owner")) }
        }"#,
    )
    .unwrap();
    // Now alice should be viewer via owner
    assert!(
        svc.check(
            &Object::new("doc", "1"),
            &Relation::new("viewer"),
            &User::user_id("alice"),
        )
        .unwrap()
    );
}

// ---------------------------------------------------------------------------
// 10. Expand end-to-end
// ---------------------------------------------------------------------------

#[test]
fn test_should_expand_with_full_hierarchy() -> Result<(), Box<dyn std::error::Error>> {
    let mut svc = ZanzibarService::new();
    svc.add_dsl(GOOGLE_DRIVE_DSL)?;

    svc.write_tuple(tuple("folder", "root", "owner", "alice"))?;
    svc.write_tuple(parent_tuple("folder", "eng", "folder", "root", "viewer"))?;
    svc.write_tuple(tuple("folder", "eng", "viewer", "bob"))?;
    svc.write_tuple(parent_tuple("doc", "design", "folder", "eng", "viewer"))?;
    svc.write_tuple(tuple("doc", "design", "editor", "charlie"))?;

    let expanded = svc.expand(&Object::new("doc", "design"), &Relation::new("viewer"))?;

    // Verify the Debug representation contains all expected users
    let debug = format!("{expanded:?}");
    assert!(
        debug.contains("alice"),
        "should contain alice (folder owner)"
    );
    assert!(debug.contains("bob"), "should contain bob (folder viewer)");
    assert!(debug.contains("charlie"), "should contain charlie (editor)");

    Ok(())
}
