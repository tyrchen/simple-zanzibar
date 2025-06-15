//! DSL parsing logic using `pest`.

use crate::error::ZanzibarError;
use crate::model::{NamespaceConfig, Relation, RelationConfig, UsersetExpression};
use pest::Parser;
use pest::iterators::Pair;
use pest_derive::Parser;
use std::collections::HashMap;

#[derive(Parser)]
#[grammar = "grammar.pest"]
struct ZanzibarParser;

/// Parses a DSL string into a vector of `NamespaceConfig`s.
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

fn parse_namespace(pair: Pair<Rule>) -> Result<NamespaceConfig, ZanzibarError> {
    let mut inner = pair.into_inner();

    // Skip the NAMESPACE keyword and get the identifier
    let _namespace_keyword = inner.next().unwrap(); // This should be NAMESPACE
    let name_pair = inner.next().unwrap(); // This should be IDENTIFIER
    let name = name_pair.as_str().to_string();
    let mut relations = HashMap::new();

    for relation_pair in inner {
        let (rel, rel_config) = parse_relation(relation_pair)?;
        relations.insert(rel, rel_config);
    }

    Ok(NamespaceConfig { name, relations })
}

fn parse_relation(pair: Pair<Rule>) -> Result<(Relation, RelationConfig), ZanzibarError> {
    let mut inner = pair.into_inner();

    // Skip the RELATION keyword
    let _relation_keyword = inner.next().unwrap(); // This should be RELATION
    let name_pair = inner.next();
    if name_pair.is_none() {
        return Err(ZanzibarError::ParseError(
            "No relation name found".to_string(),
        ));
    }
    let name_str = name_pair.unwrap().as_str();
    let name = Relation(name_str.to_string());

    let rewrite = match inner.next() {
        Some(rewrite_pair) => {
            if rewrite_pair.as_rule() == Rule::rewrite_rule {
                Some(parse_rewrite(rewrite_pair)?)
            } else {
                None
            }
        }
        None => None,
    };

    Ok((
        name.clone(),
        RelationConfig {
            name,
            userset_rewrite: rewrite,
        },
    ))
}

fn parse_rewrite(pair: Pair<Rule>) -> Result<UsersetExpression, ZanzibarError> {
    let mut inner = pair.into_inner();

    // Skip the REWRITE keyword
    let _rewrite_keyword = inner.next().unwrap(); // This should be REWRITE

    // Get the actual expression
    let expression_pair = inner.next().unwrap();

    parse_expression(expression_pair)
}

fn parse_expression(pair: Pair<Rule>) -> Result<UsersetExpression, ZanzibarError> {
    // If this is an expression rule, we need to get its inner content
    if pair.as_rule() == Rule::expression {
        let inner_pair = pair.into_inner().next().unwrap();
        return parse_expression(inner_pair);
    }

    match pair.as_rule() {
        Rule::term => {
            // Term is an intermediate rule, get its inner content
            let inner_pair = pair.into_inner().next().unwrap();
            parse_expression(inner_pair)
        }
        Rule::this_expr => Ok(UsersetExpression::This),
        Rule::computed_userset_expr => {
            let mut inner = pair.into_inner();
            let relation_str = inner.next().unwrap().as_str().trim_matches('\"');
            Ok(UsersetExpression::ComputedUserset {
                relation: Relation(relation_str.to_string()),
            })
        }
        Rule::tuple_to_userset_expr => {
            let mut inner = pair.into_inner();
            let tupleset_str = inner.next().unwrap().as_str().trim_matches('\"');
            let computed_str = inner.next().unwrap().as_str().trim_matches('\"');
            Ok(UsersetExpression::TupleToUserset {
                tupleset_relation: Relation(tupleset_str.to_string()),
                computed_userset_relation: Relation(computed_str.to_string()),
            })
        }
        Rule::union_expr | Rule::intersection_expr => {
            let is_union = pair.as_rule() == Rule::union_expr;

            let mut inner = pair.into_inner();
            // Skip the keyword (UNION or INTERSECTION)
            let _keyword = inner.next().unwrap();

            // Parse the remaining expressions
            let expressions = inner.map(parse_expression).collect::<Result<Vec<_>, _>>()?;

            if is_union {
                Ok(UsersetExpression::Union(expressions))
            } else {
                Ok(UsersetExpression::Intersection(expressions))
            }
        }
        Rule::exclusion_expr => {
            let mut inner = pair.into_inner();

            // Skip the EXCLUSION keyword
            let _keyword = inner.next().unwrap();

            let base = parse_expression(inner.next().unwrap())?;
            let exclude = parse_expression(inner.next().unwrap())?;
            Ok(UsersetExpression::Exclusion {
                base: Box::new(base),
                exclude: Box::new(exclude),
            })
        }
        _ => Err(ZanzibarError::ParseError(format!(
            "Unexpected rule: {:?}",
            pair.as_rule()
        ))),
    }
}
