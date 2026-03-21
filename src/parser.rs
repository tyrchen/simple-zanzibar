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
