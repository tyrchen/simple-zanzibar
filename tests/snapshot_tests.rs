//! Snapshot tests for parser output, expand results, and error messages.
//!
//! These tests capture the exact output format of key operations so that
//! any unintentional changes to output structure are caught immediately.

use simple_zanzibar::{
    ZanzibarService,
    model::{ExpandedUserset, Object, Relation, RelationTuple, User},
};

// -- Parser snapshots --

#[test]
fn test_snapshot_parse_simple_policy() {
    let dsl = r#"
        namespace doc {
            relation owner {}
            relation viewer { rewrite this }
        }
    "#;
    let configs = simple_zanzibar::parser::parse_dsl(dsl).unwrap();
    let doc = configs.iter().find(|c| c.name == "doc").unwrap();

    let owner = doc.relations.get(&Relation::new("owner")).unwrap();
    insta::assert_debug_snapshot!("parse_owner_no_rewrite", &owner.userset_rewrite);

    let viewer = doc.relations.get(&Relation::new("viewer")).unwrap();
    insta::assert_debug_snapshot!("parse_viewer_this", &viewer.userset_rewrite);
}

#[test]
fn test_snapshot_parse_complex_policy() {
    let dsl = r#"
        namespace doc {
            relation owner {}
            relation editor {}
            relation banned {}
            relation viewer {
                rewrite exclusion(
                    union(
                        this,
                        computed_userset(relation: "owner"),
                        computed_userset(relation: "editor")
                    ),
                    computed_userset(relation: "banned")
                )
            }
        }
    "#;
    let configs = simple_zanzibar::parser::parse_dsl(dsl).unwrap();
    let doc = configs.iter().find(|c| c.name == "doc").unwrap();

    let viewer = doc.relations.get(&Relation::new("viewer")).unwrap();
    insta::assert_debug_snapshot!("parse_exclusion_with_nested_union", &viewer.userset_rewrite);
}

#[test]
fn test_snapshot_parse_tuple_to_userset() {
    let dsl = r#"
        namespace doc {
            relation parent {}
            relation viewer {
                rewrite tuple_to_userset(tupleset: "parent", computed_userset: "viewer")
            }
        }
    "#;
    let configs = simple_zanzibar::parser::parse_dsl(dsl).unwrap();
    let doc = configs.iter().find(|c| c.name == "doc").unwrap();

    let viewer = doc.relations.get(&Relation::new("viewer")).unwrap();
    insta::assert_debug_snapshot!("parse_tuple_to_userset", &viewer.userset_rewrite);
}

// -- Expand snapshots --

fn setup_expand_service() -> ZanzibarService {
    let mut service = ZanzibarService::new();
    service
        .add_dsl(
            r#"
        namespace folder {
            relation viewer {}
        }
        namespace doc {
            relation owner {}
            relation editor {}
            relation parent {}
            relation viewer {
                rewrite union(
                    this,
                    computed_userset(relation: "owner"),
                    computed_userset(relation: "editor"),
                    tuple_to_userset(tupleset: "parent", computed_userset: "viewer")
                )
            }
        }
    "#,
        )
        .unwrap();

    // doc:readme has owner=alice, editor=bob, viewer=charlie
    service
        .write_tuple(RelationTuple::new(
            Object::new("doc", "readme"),
            Relation::new("owner"),
            User::user_id("alice"),
        ))
        .unwrap();
    service
        .write_tuple(RelationTuple::new(
            Object::new("doc", "readme"),
            Relation::new("editor"),
            User::user_id("bob"),
        ))
        .unwrap();
    service
        .write_tuple(RelationTuple::new(
            Object::new("doc", "readme"),
            Relation::new("viewer"),
            User::user_id("charlie"),
        ))
        .unwrap();
    // doc:readme's parent is folder:docs
    service
        .write_tuple(RelationTuple::new(
            Object::new("doc", "readme"),
            Relation::new("parent"),
            User::userset(Object::new("folder", "docs"), Relation::new("viewer")),
        ))
        .unwrap();
    // folder:docs has viewer=dave
    service
        .write_tuple(RelationTuple::new(
            Object::new("folder", "docs"),
            Relation::new("viewer"),
            User::user_id("dave"),
        ))
        .unwrap();

    service
}

#[test]
fn test_snapshot_expand_union_with_hierarchy() {
    let service = setup_expand_service();
    let expanded = service
        .expand(&Object::new("doc", "readme"), &Relation::new("viewer"))
        .unwrap();
    insta::assert_debug_snapshot!("expand_viewer_union_hierarchy", expanded);
}

#[test]
fn test_snapshot_expand_direct_relation() {
    let service = setup_expand_service();
    let expanded = service
        .expand(&Object::new("doc", "readme"), &Relation::new("owner"))
        .unwrap();
    insta::assert_debug_snapshot!("expand_owner_direct", expanded);
}

#[test]
fn test_snapshot_expand_exclusion() {
    let mut service = ZanzibarService::new();
    service
        .add_dsl(
            r#"
        namespace doc {
            relation banned {}
            relation viewer {
                rewrite exclusion(this, computed_userset(relation: "banned"))
            }
        }
    "#,
        )
        .unwrap();

    service
        .write_tuple(RelationTuple::new(
            Object::new("doc", "1"),
            Relation::new("viewer"),
            User::user_id("alice"),
        ))
        .unwrap();
    service
        .write_tuple(RelationTuple::new(
            Object::new("doc", "1"),
            Relation::new("viewer"),
            User::user_id("bob"),
        ))
        .unwrap();
    service
        .write_tuple(RelationTuple::new(
            Object::new("doc", "1"),
            Relation::new("banned"),
            User::user_id("bob"),
        ))
        .unwrap();

    let expanded = service
        .expand(&Object::new("doc", "1"), &Relation::new("viewer"))
        .unwrap();
    insta::assert_debug_snapshot!("expand_exclusion_banned", expanded);
}

#[test]
fn test_snapshot_expand_intersection() {
    let mut service = ZanzibarService::new();
    service
        .add_dsl(
            r#"
        namespace team {
            relation member {}
            relation viewer {
                rewrite intersection(this, computed_userset(relation: "member"))
            }
        }
    "#,
        )
        .unwrap();

    service
        .write_tuple(RelationTuple::new(
            Object::new("team", "eng"),
            Relation::new("viewer"),
            User::user_id("alice"),
        ))
        .unwrap();
    service
        .write_tuple(RelationTuple::new(
            Object::new("team", "eng"),
            Relation::new("member"),
            User::user_id("alice"),
        ))
        .unwrap();
    service
        .write_tuple(RelationTuple::new(
            Object::new("team", "eng"),
            Relation::new("member"),
            User::user_id("bob"),
        ))
        .unwrap();

    let expanded = service
        .expand(&Object::new("team", "eng"), &Relation::new("viewer"))
        .unwrap();
    insta::assert_debug_snapshot!("expand_intersection", expanded);
}

// -- Error snapshots --

#[test]
fn test_snapshot_error_namespace_not_found() {
    let service = ZanzibarService::new();
    let err = service
        .check(
            &Object::new("nonexistent", "1"),
            &Relation::new("viewer"),
            &User::user_id("alice"),
        )
        .unwrap_err();
    insta::assert_snapshot!("error_namespace_not_found", err.to_string());
}

#[test]
fn test_snapshot_error_relation_not_found() {
    let mut service = ZanzibarService::new();
    service
        .add_dsl("namespace doc { relation owner {} }")
        .unwrap();

    let err = service
        .check(
            &Object::new("doc", "1"),
            &Relation::new("nonexistent"),
            &User::user_id("alice"),
        )
        .unwrap_err();
    insta::assert_snapshot!("error_relation_not_found", err.to_string());
}

#[test]
fn test_snapshot_error_parse_error() {
    let err = simple_zanzibar::parser::parse_dsl("this is not valid DSL!!!").unwrap_err();
    insta::assert_snapshot!("error_parse_error", err.to_string());
}

#[test]
fn test_snapshot_error_duplicate_tuple() {
    let mut service = ZanzibarService::new();
    service
        .add_dsl("namespace doc { relation owner {} }")
        .unwrap();

    let tuple = RelationTuple::new(
        Object::new("doc", "1"),
        Relation::new("owner"),
        User::user_id("alice"),
    );
    service.write_tuple(tuple.clone()).unwrap();
    let err = service.write_tuple(tuple).unwrap_err();
    insta::assert_snapshot!("error_duplicate_tuple", err.to_string());
}

// -- Expand structure verification --

/// Recursively collect all user IDs from an expanded userset.
fn collect_users(expanded: &ExpandedUserset) -> Vec<String> {
    let mut users = Vec::new();
    collect_users_inner(expanded, &mut users);
    users.sort();
    users
}

fn collect_users_inner(expanded: &ExpandedUserset, out: &mut Vec<String>) {
    match expanded {
        ExpandedUserset::User(id) => out.push(id.clone()),
        ExpandedUserset::Userset(_, _) => {}
        ExpandedUserset::Union(children) | ExpandedUserset::Intersection(children) => {
            for child in children {
                collect_users_inner(child, out);
            }
        }
        ExpandedUserset::Exclusion { base, exclude: _ } => {
            // Only collect from base — exclude is the subtracted set.
            collect_users_inner(base, out);
        }
        _ => {}
    }
}

#[test]
fn test_snapshot_expand_contains_all_users() {
    let service = setup_expand_service();
    let expanded = service
        .expand(&Object::new("doc", "readme"), &Relation::new("viewer"))
        .unwrap();

    let users = collect_users(&expanded);
    // viewer = union(direct[charlie], owners[alice], editors[bob], parent->folder:docs[dave])
    assert_eq!(users, vec!["alice", "bob", "charlie", "dave"]);
}
