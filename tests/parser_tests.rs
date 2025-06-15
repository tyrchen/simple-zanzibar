//! Tests for the DSL parser.

use simple_zanzibar::error::ZanzibarError;
use simple_zanzibar::model::Relation;
use simple_zanzibar::ZanzibarService;

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
fn test_parse_full_dsl() -> Result<(), ZanzibarError> {
    let mut service = ZanzibarService::new();
    service.add_dsl(TEST_DSL)?;

    // Retrieve the configs to check them. This requires making the field public
    // or adding a getter. For a test, we can just re-parse and check the result.
    let configs = simple_zanzibar::parser::parse_dsl(TEST_DSL)?;
    assert_eq!(configs.len(), 2);

    let doc_config = configs
        .iter()
        .find(|c| c.name == "doc")
        .expect("doc namespace not found");
    assert_eq!(doc_config.relations.len(), 4);
    assert!(doc_config
        .relations
        .contains_key(&Relation("owner".to_string())));
    assert!(doc_config
        .relations
        .contains_key(&Relation("parent".to_string())));
    assert!(doc_config
        .relations
        .contains_key(&Relation("viewer".to_string())));
    assert!(doc_config
        .relations
        .contains_key(&Relation("editor".to_string())));

    let viewer_rewrite = doc_config
        .relations
        .get(&Relation("viewer".to_string()))
        .unwrap()
        .userset_rewrite
        .as_ref();
    assert!(viewer_rewrite.is_some());
    // A more detailed assertion could inspect the structure of the rewrite enum.

    let folder_config = configs
        .iter()
        .find(|c| c.name == "folder")
        .expect("folder namespace not found");
    assert_eq!(folder_config.relations.len(), 1);
    assert!(folder_config
        .relations
        .contains_key(&Relation("viewer".to_string())));

    Ok(())
}
