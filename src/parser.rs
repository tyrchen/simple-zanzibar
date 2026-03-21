//! DSL parsing logic using `pest`.
//!
//! This module provides the parser for the Zanzibar policy DSL, converting
//! textual policy definitions into [`NamespaceConfig`] structures.

use std::collections::HashMap;

use pest::{
    Parser,
    iterators::{Pair, Pairs},
};
use pest_derive::Parser;

use crate::{
    error::ZanzibarError,
    model::{NamespaceConfig, Relation, RelationConfig, UsersetExpression},
};

#[derive(Parser)]
#[grammar = "grammar.pest"]
struct ZanzibarParser;

/// Parses a DSL string into a vector of [`NamespaceConfig`]s.
///
/// # Errors
///
/// Returns [`ZanzibarError::ParseError`] if the input is not valid DSL.
///
/// # Examples
///
/// ```
/// use simple_zanzibar::parser::parse_dsl;
///
/// let dsl = r#"
///     namespace doc {
///         relation owner {}
///         relation viewer {
///             rewrite this
///         }
///     }
/// "#;
/// let configs = parse_dsl(dsl).unwrap();
/// assert_eq!(configs.len(), 1);
/// assert_eq!(configs[0].name, "doc");
/// ```
pub fn parse_dsl(dsl: &str) -> Result<Vec<NamespaceConfig>, ZanzibarError> {
    let pairs = ZanzibarParser::parse(Rule::file, dsl)
        .map_err(|e| ZanzibarError::ParseError(e.to_string()))?;

    let mut configs = Vec::new();
    for pair in pairs {
        for inner_pair in pair.into_inner() {
            if inner_pair.as_rule() == Rule::namespace_def {
                configs.push(parse_namespace(inner_pair)?);
            }
        }
    }
    Ok(configs)
}

/// Extracts the next pair from an iterator, returning a parse error if exhausted.
fn next_pair<'i>(pairs: &mut Pairs<'i, Rule>) -> Result<Pair<'i, Rule>, ZanzibarError> {
    pairs
        .next()
        .ok_or_else(|| ZanzibarError::ParseError("unexpected end of input".to_string()))
}

fn parse_namespace(pair: Pair<'_, Rule>) -> Result<NamespaceConfig, ZanzibarError> {
    let mut inner = pair.into_inner();

    // Skip the NAMESPACE keyword and get the identifier.
    let _namespace_keyword = next_pair(&mut inner)?;
    let name_pair = next_pair(&mut inner)?;
    let name = name_pair.as_str().to_string();
    let mut relations = HashMap::new();

    for relation_pair in inner {
        let (rel, rel_config) = parse_relation(relation_pair)?;
        relations.insert(rel, rel_config);
    }

    Ok(NamespaceConfig { name, relations })
}

fn parse_relation(pair: Pair<'_, Rule>) -> Result<(Relation, RelationConfig), ZanzibarError> {
    let mut inner = pair.into_inner();

    // Skip the RELATION keyword.
    let _relation_keyword = next_pair(&mut inner)?;
    let name_pair = next_pair(&mut inner)?;
    let name = Relation(name_pair.as_str().to_string());

    let rewrite = match inner.next() {
        Some(rewrite_pair) if rewrite_pair.as_rule() == Rule::rewrite_rule => {
            Some(parse_rewrite(rewrite_pair)?)
        }
        _ => None,
    };

    Ok((
        name.clone(),
        RelationConfig {
            name,
            userset_rewrite: rewrite,
        },
    ))
}

fn parse_rewrite(pair: Pair<'_, Rule>) -> Result<UsersetExpression, ZanzibarError> {
    let mut inner = pair.into_inner();

    // Skip the REWRITE keyword.
    let _rewrite_keyword = next_pair(&mut inner)?;
    let expression_pair = next_pair(&mut inner)?;

    parse_expression(expression_pair)
}

fn parse_expression(pair: Pair<'_, Rule>) -> Result<UsersetExpression, ZanzibarError> {
    // If this is an expression rule, unwrap to get its inner content.
    if pair.as_rule() == Rule::expression {
        let inner_pair = next_pair(&mut pair.into_inner())?;
        return parse_expression(inner_pair);
    }

    match pair.as_rule() {
        Rule::term => {
            // Term is an intermediate rule, get its inner content.
            let inner_pair = next_pair(&mut pair.into_inner())?;
            parse_expression(inner_pair)
        }
        Rule::this_expr => Ok(UsersetExpression::This),
        Rule::computed_userset_expr => {
            let mut inner = pair.into_inner();
            let _keyword = next_pair(&mut inner)?;
            let relation_str = next_pair(&mut inner)?.as_str().trim_matches('\"');
            Ok(UsersetExpression::ComputedUserset {
                relation: Relation(relation_str.to_string()),
            })
        }
        Rule::tuple_to_userset_expr => {
            let mut inner = pair.into_inner();
            let _keyword = next_pair(&mut inner)?;
            let tupleset_str = next_pair(&mut inner)?.as_str().trim_matches('\"');
            let computed_str = next_pair(&mut inner)?.as_str().trim_matches('\"');
            Ok(UsersetExpression::TupleToUserset {
                tupleset_relation: Relation(tupleset_str.to_string()),
                computed_userset_relation: Relation(computed_str.to_string()),
            })
        }
        Rule::union_expr | Rule::intersection_expr => {
            let is_union = pair.as_rule() == Rule::union_expr;
            let mut inner = pair.into_inner();
            let _keyword = next_pair(&mut inner)?;

            let expressions = inner.map(parse_expression).collect::<Result<Vec<_>, _>>()?;

            if is_union {
                Ok(UsersetExpression::Union(expressions))
            } else {
                Ok(UsersetExpression::Intersection(expressions))
            }
        }
        Rule::exclusion_expr => {
            let mut inner = pair.into_inner();
            let _keyword = next_pair(&mut inner)?;
            let base = parse_expression(next_pair(&mut inner)?)?;
            let exclude = parse_expression(next_pair(&mut inner)?)?;
            Ok(UsersetExpression::Exclusion {
                base: Box::new(base),
                exclude: Box::new(exclude),
            })
        }
        _ => Err(ZanzibarError::ParseError(format!(
            "unexpected rule: {:?}",
            pair.as_rule()
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_should_parse_empty_file() {
        let configs = parse_dsl("").unwrap();
        assert!(configs.is_empty());
    }

    #[test]
    fn test_should_parse_empty_namespace() {
        let configs = parse_dsl("namespace empty {}").unwrap();
        assert_eq!(configs.len(), 1);
        assert_eq!(configs[0].name, "empty");
        assert!(configs[0].relations.is_empty());
    }

    #[test]
    fn test_should_parse_relation_without_rewrite() {
        let configs = parse_dsl("namespace doc { relation owner {} }").unwrap();
        let owner = configs[0].relations.get(&Relation::new("owner")).unwrap();
        assert!(owner.userset_rewrite.is_none());
    }

    #[test]
    fn test_should_parse_this_rewrite() {
        let configs = parse_dsl("namespace doc { relation viewer { rewrite this } }").unwrap();
        let viewer = configs[0].relations.get(&Relation::new("viewer")).unwrap();
        assert!(matches!(
            viewer.userset_rewrite,
            Some(UsersetExpression::This)
        ));
    }

    #[test]
    fn test_should_parse_computed_userset() {
        let dsl =
            r#"namespace doc { relation viewer { rewrite computed_userset(relation: "owner") } }"#;
        let configs = parse_dsl(dsl).unwrap();
        let viewer = configs[0].relations.get(&Relation::new("viewer")).unwrap();
        assert!(matches!(
            viewer.userset_rewrite,
            Some(UsersetExpression::ComputedUserset { .. })
        ));
        if let Some(UsersetExpression::ComputedUserset { relation }) = &viewer.userset_rewrite {
            assert_eq!(relation.0, "owner");
        }
    }

    #[test]
    fn test_should_parse_tuple_to_userset() {
        let dsl = r#"
            namespace doc {
                relation viewer {
                    rewrite tuple_to_userset(tupleset: "parent", computed_userset: "viewer")
                }
            }
        "#;
        let configs = parse_dsl(dsl).unwrap();
        let viewer = configs[0].relations.get(&Relation::new("viewer")).unwrap();
        if let Some(UsersetExpression::TupleToUserset {
            tupleset_relation,
            computed_userset_relation,
        }) = &viewer.userset_rewrite
        {
            assert_eq!(tupleset_relation.0, "parent");
            assert_eq!(computed_userset_relation.0, "viewer");
        } else {
            panic!("expected TupleToUserset");
        }
    }

    #[test]
    fn test_should_parse_union_with_multiple_children() {
        let dsl = r#"
            namespace doc {
                relation viewer {
                    rewrite union(this, computed_userset(relation: "owner"), computed_userset(relation: "editor"))
                }
            }
        "#;
        let configs = parse_dsl(dsl).unwrap();
        let viewer = configs[0].relations.get(&Relation::new("viewer")).unwrap();
        if let Some(UsersetExpression::Union(children)) = &viewer.userset_rewrite {
            assert_eq!(children.len(), 3);
        } else {
            panic!("expected Union");
        }
    }

    #[test]
    fn test_should_parse_intersection() {
        let dsl = r#"
            namespace doc {
                relation viewer {
                    rewrite intersection(this, computed_userset(relation: "member"))
                }
            }
        "#;
        let configs = parse_dsl(dsl).unwrap();
        let viewer = configs[0].relations.get(&Relation::new("viewer")).unwrap();
        if let Some(UsersetExpression::Intersection(children)) = &viewer.userset_rewrite {
            assert_eq!(children.len(), 2);
        } else {
            panic!("expected Intersection");
        }
    }

    #[test]
    fn test_should_parse_exclusion() {
        let dsl = r#"
            namespace doc {
                relation viewer {
                    rewrite exclusion(this, computed_userset(relation: "banned"))
                }
            }
        "#;
        let configs = parse_dsl(dsl).unwrap();
        let viewer = configs[0].relations.get(&Relation::new("viewer")).unwrap();
        assert!(matches!(
            viewer.userset_rewrite,
            Some(UsersetExpression::Exclusion { .. })
        ));
    }

    #[test]
    fn test_should_parse_nested_expressions() {
        let dsl = r#"
            namespace doc {
                relation viewer {
                    rewrite union(
                        this,
                        intersection(
                            computed_userset(relation: "member"),
                            exclusion(
                                computed_userset(relation: "editor"),
                                computed_userset(relation: "banned")
                            )
                        )
                    )
                }
            }
        "#;
        let configs = parse_dsl(dsl).unwrap();
        let viewer = configs[0].relations.get(&Relation::new("viewer")).unwrap();
        if let Some(UsersetExpression::Union(children)) = &viewer.userset_rewrite {
            assert_eq!(children.len(), 2);
            assert!(matches!(children[0], UsersetExpression::This));
            assert!(matches!(children[1], UsersetExpression::Intersection(_)));
        } else {
            panic!("expected nested Union");
        }
    }

    #[test]
    fn test_should_parse_comments_between_elements() {
        let dsl = r#"
            // Top-level comment
            namespace doc {
                // Comment before relation
                relation owner {} // Inline comment after
                // Another comment
                relation viewer { rewrite this }
            }
        "#;
        let configs = parse_dsl(dsl).unwrap();
        assert_eq!(configs[0].relations.len(), 2);
    }

    #[test]
    fn test_should_parse_multiple_namespaces() {
        let dsl = r#"
            namespace doc { relation owner {} }
            namespace folder { relation viewer {} }
            namespace org { relation member {} }
        "#;
        let configs = parse_dsl(dsl).unwrap();
        assert_eq!(configs.len(), 3);
    }

    #[test]
    fn test_should_reject_invalid_syntax() {
        assert!(parse_dsl("not valid dsl at all").is_err());
    }

    #[test]
    fn test_should_reject_unclosed_namespace() {
        assert!(parse_dsl("namespace doc {").is_err());
    }

    #[test]
    fn test_should_reject_unknown_rewrite_keyword() {
        let dsl = r#"namespace doc { relation v { rewrite unknown_thing(this) } }"#;
        assert!(parse_dsl(dsl).is_err());
    }
}
