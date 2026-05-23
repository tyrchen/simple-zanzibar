//! DSL parsing logic using `pest`.

use std::collections::HashMap;

use generated::{Rule, ZanzibarParser};
use pest::{
    Parser,
    iterators::{Pair, Pairs},
};

use crate::{
    error::ZanzibarError,
    model::{NamespaceConfig, Relation, RelationConfig, UsersetExpression},
};

mod generated {
    //! Generated `pest` parser bindings.

    #![allow(missing_docs)]

    use pest_derive::Parser;

    #[derive(Parser)]
    #[grammar = "grammar.pest"]
    pub(super) struct ZanzibarParser;
}

#[derive(Debug, Clone)]
pub(crate) struct LegacyNamespaceAst {
    pub(crate) name: String,
    pub(crate) relations: Vec<LegacyRelationAst>,
}

#[derive(Debug, Clone)]
pub(crate) struct LegacyRelationAst {
    pub(crate) name: String,
    pub(crate) rewrite: Option<UsersetExpression>,
}

pub(crate) fn parse_dsl_ast(dsl: &str) -> Result<Vec<LegacyNamespaceAst>, ZanzibarError> {
    let pairs = ZanzibarParser::parse(Rule::file, dsl)
        .map_err(|e| ZanzibarError::ParseError(e.to_string()))?;

    let mut namespaces = Vec::new();
    for pair in pairs {
        for inner_pair in pair.into_inner() {
            if inner_pair.as_rule() == Rule::namespace_def {
                namespaces.push(parse_namespace_ast(inner_pair)?);
            }
        }
    }

    Ok(namespaces)
}

/// Parses a DSL string into a vector of `NamespaceConfig`s.
///
/// # Errors
///
/// Returns [`ZanzibarError::ParseError`] when the input does not match the DSL grammar.
pub fn parse_dsl(dsl: &str) -> Result<Vec<NamespaceConfig>, ZanzibarError> {
    parse_dsl_ast(dsl)?
        .into_iter()
        .map(TryFrom::try_from)
        .collect()
}

fn parse_namespace_ast(pair: Pair<Rule>) -> Result<LegacyNamespaceAst, ZanzibarError> {
    let mut inner = pair.into_inner();

    // Skip the NAMESPACE keyword and get the identifier.
    let _namespace_keyword = next_pair(&mut inner, "namespace keyword")?;
    let name_pair = next_pair(&mut inner, "namespace identifier")?;
    let name = name_pair.as_str().to_string();
    let mut relations = Vec::new();

    for relation_pair in inner {
        relations.push(parse_relation_ast(relation_pair)?);
    }

    Ok(LegacyNamespaceAst { name, relations })
}

fn parse_relation_ast(pair: Pair<Rule>) -> Result<LegacyRelationAst, ZanzibarError> {
    let mut inner = pair.into_inner();

    // Skip the RELATION keyword.
    let _relation_keyword = next_pair(&mut inner, "relation keyword")?;
    let name_pair = next_pair(&mut inner, "relation name")?;
    let name = name_pair.as_str().to_string();

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

    Ok(LegacyRelationAst { name, rewrite })
}

impl TryFrom<LegacyNamespaceAst> for NamespaceConfig {
    type Error = ZanzibarError;

    fn try_from(namespace: LegacyNamespaceAst) -> Result<Self, Self::Error> {
        let mut relations = HashMap::new();
        for relation in namespace.relations {
            let name = Relation(relation.name);
            relations.insert(
                name.clone(),
                RelationConfig {
                    name,
                    userset_rewrite: relation.rewrite,
                },
            );
        }

        Ok(NamespaceConfig {
            name: namespace.name,
            relations,
        })
    }
}

fn parse_rewrite(pair: Pair<Rule>) -> Result<UsersetExpression, ZanzibarError> {
    let mut inner = pair.into_inner();

    // Skip the REWRITE keyword.
    let _rewrite_keyword = next_pair(&mut inner, "rewrite keyword")?;

    // Get the actual expression.
    let expression_pair = next_pair(&mut inner, "rewrite expression")?;

    parse_expression(expression_pair)
}

fn parse_expression(pair: Pair<Rule>) -> Result<UsersetExpression, ZanzibarError> {
    // If this is an expression rule, we need to get its inner content
    if pair.as_rule() == Rule::expression {
        let mut inner = pair.into_inner();
        let inner_pair = next_pair(&mut inner, "expression term")?;
        return parse_expression(inner_pair);
    }

    match pair.as_rule() {
        Rule::term => {
            // Term is an intermediate rule, get its inner content
            let mut inner = pair.into_inner();
            let inner_pair = next_pair(&mut inner, "term expression")?;
            parse_expression(inner_pair)
        }
        Rule::this_expr => Ok(UsersetExpression::This),
        Rule::computed_userset_expr => {
            let mut inner = pair.into_inner();
            // Skip COMPUTED_USERSET keyword, get STRING_LITERAL
            let _keyword = next_pair(&mut inner, "computed_userset keyword")?;
            let relation_pair = next_pair(&mut inner, "computed_userset relation")?;
            let relation_str = relation_pair.as_str().trim_matches('\"');
            Ok(UsersetExpression::ComputedUserset {
                relation: Relation(relation_str.to_string()),
            })
        }
        Rule::tuple_to_userset_expr => {
            let mut inner = pair.into_inner();
            // Based on the grammar, we should have: TUPLE_TO_USERSET, STRING_LITERAL,
            // STRING_LITERAL
            let _keyword = next_pair(&mut inner, "tuple_to_userset keyword")?;
            let tupleset_pair = next_pair(&mut inner, "tuple_to_userset tupleset relation")?;
            let computed_pair = next_pair(&mut inner, "tuple_to_userset computed relation")?;
            let tupleset_str = tupleset_pair.as_str().trim_matches('\"');
            let computed_str = computed_pair.as_str().trim_matches('\"');
            Ok(UsersetExpression::TupleToUserset {
                tupleset_relation: Relation(tupleset_str.to_string()),
                computed_userset_relation: Relation(computed_str.to_string()),
            })
        }
        Rule::union_expr | Rule::intersection_expr => {
            let is_union = pair.as_rule() == Rule::union_expr;

            let mut inner = pair.into_inner();
            // Skip the keyword (UNION or INTERSECTION)
            let _keyword = next_pair(&mut inner, "set operation keyword")?;

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
            let _keyword = next_pair(&mut inner, "exclusion keyword")?;

            let base = parse_expression(next_pair(&mut inner, "exclusion base expression")?)?;
            let exclude = parse_expression(next_pair(&mut inner, "exclusion exclude expression")?)?;
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

fn next_pair<'input>(
    pairs: &mut Pairs<'input, Rule>,
    expected: &str,
) -> Result<Pair<'input, Rule>, ZanzibarError> {
    pairs
        .next()
        .ok_or_else(|| ZanzibarError::ParseError(format!("Expected {expected}")))
}
