//! Tests for the DSL parser.

use simple_zanzibar::{error::ZanzibarError, model::Relation};

const TEST_DSL: &str = r#"
    // Defines a document namespace with hierarchical permissions.
    namespace doc {
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
            rewrite intersection(
                computed_userset(relation: "owner"),
                exclusion(
                    this,
                    computed_userset(relation: "viewer")
                )
            )
        }
    }

    // A simple folder namespace.
    namespace folder {
        relation viewer {}
    }
"#;

#[test]
fn test_should_parse_full_dsl() -> Result<(), ZanzibarError> {
    let configs = simple_zanzibar::parser::parse_dsl(TEST_DSL)?;
    assert_eq!(configs.len(), 2);

    let doc_config = configs
        .iter()
        .find(|c| c.name == "doc")
        .expect("doc namespace not found");
    assert_eq!(doc_config.relations.len(), 4);
    assert!(doc_config.relations.contains_key(&Relation::new("owner")));
    assert!(doc_config.relations.contains_key(&Relation::new("parent")));
    assert!(doc_config.relations.contains_key(&Relation::new("viewer")));
    assert!(doc_config.relations.contains_key(&Relation::new("editor")));

    let viewer_rewrite = doc_config
        .relations
        .get(&Relation::new("viewer"))
        .expect("viewer relation not found")
        .userset_rewrite
        .as_ref();
    assert!(viewer_rewrite.is_some());

    let folder_config = configs
        .iter()
        .find(|c| c.name == "folder")
        .expect("folder namespace not found");
    assert_eq!(folder_config.relations.len(), 1);
    assert!(
        folder_config
            .relations
            .contains_key(&Relation::new("viewer"))
    );

    Ok(())
}
