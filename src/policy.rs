//! Policy text import/export helpers.

use std::{
    collections::{BTreeMap, HashMap},
    fs, io,
    path::Path,
};

use thiserror::Error;

use crate::{
    domain::Relationship,
    error::ZanzibarError,
    model::{NamespaceConfig, RelationConfig, UsersetExpression},
    snapshot::{SnapshotIoError, SnapshotSaveOptions},
};

const SCHEMA_FILE_NAME: &str = "schema.zed";
const RELATIONSHIP_DIRECTORY_NAME: &str = "relationships";
const RELATIONSHIP_FILE_EXTENSION: &str = "zedtuples";

/// Complete reviewable policy text.
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize, serde::Deserialize),
    serde(rename_all = "camelCase", deny_unknown_fields)
)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicyText {
    /// Canonical schema DSL.
    pub schema: String,
    /// Grouped relationship text files.
    pub relationship_files: Vec<PolicyTextFile>,
}

impl PolicyText {
    /// Creates policy text from schema and grouped relationship files.
    #[must_use]
    pub fn new(schema: String, relationship_files: Vec<PolicyTextFile>) -> Self {
        Self {
            schema,
            relationship_files,
        }
    }

    /// Creates policy text with all relationships in one logical file.
    #[must_use]
    pub fn from_single_relationship_file(schema: String, relationships: String) -> Self {
        Self {
            schema,
            relationship_files: vec![PolicyTextFile {
                path: format!("{RELATIONSHIP_DIRECTORY_NAME}/all.{RELATIONSHIP_FILE_EXTENSION}"),
                contents: relationships,
            }],
        }
    }
}

/// One reviewable policy text file.
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize, serde::Deserialize),
    serde(rename_all = "camelCase", deny_unknown_fields)
)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicyTextFile {
    /// Relative file path used when exporting to a directory.
    pub path: String,
    /// File contents.
    pub contents: String,
}

/// Errors produced while importing or exporting policy files.
#[derive(Debug, Error)]
pub enum PolicyIoError {
    /// Filesystem I/O failed.
    #[error("policy io failed")]
    Io {
        /// Source I/O error.
        #[source]
        source: io::Error,
    },

    /// Policy parsing, validation, or mutation failed.
    #[error("policy operation failed")]
    Zanzibar {
        /// Source Zanzibar error.
        #[source]
        source: ZanzibarError,
    },

    /// Snapshot save/load failed while processing policy text.
    #[error("policy snapshot operation failed")]
    Snapshot {
        /// Source snapshot error.
        #[source]
        source: SnapshotIoError,
    },

    /// The export path was invalid for policy file output.
    #[error("policy export path is invalid: {reason}")]
    InvalidExportPath {
        /// Static invalid path reason.
        reason: &'static str,
    },

    /// An engine lock was poisoned before policy IO could run.
    #[error("engine lock poisoned during {operation}")]
    LockPoisoned {
        /// Operation that attempted to acquire the poisoned lock.
        operation: &'static str,
    },
}

impl From<io::Error> for PolicyIoError {
    fn from(source: io::Error) -> Self {
        Self::Io { source }
    }
}

impl From<ZanzibarError> for PolicyIoError {
    fn from(source: ZanzibarError) -> Self {
        Self::Zanzibar { source }
    }
}

impl From<SnapshotIoError> for PolicyIoError {
    fn from(source: SnapshotIoError) -> Self {
        Self::Snapshot { source }
    }
}

pub(crate) fn canonical_schema_source(configs: &HashMap<String, NamespaceConfig>) -> String {
    let mut output = String::new();
    let mut namespaces = configs.values().collect::<Vec<_>>();
    namespaces.sort_by(|left, right| left.name.cmp(&right.name));
    for namespace in namespaces {
        output.push_str("namespace ");
        output.push_str(&namespace.name);
        output.push_str(" {\n");
        let mut relations = namespace.relations.values().collect::<Vec<_>>();
        relations.sort_by(|left, right| left.name.0.cmp(&right.name.0));
        for relation in relations {
            push_relation(&mut output, relation);
        }
        output.push_str("}\n\n");
    }
    output
}

pub(crate) fn export_policy_text(
    configs: &HashMap<String, NamespaceConfig>,
    relationships: Vec<Relationship>,
) -> PolicyText {
    PolicyText {
        schema: canonical_schema_source(configs),
        relationship_files: relationship_files(relationships),
    }
}

pub(crate) fn write_policy_files(
    directory: &Path,
    policy: &PolicyText,
) -> Result<(), PolicyIoError> {
    if directory.as_os_str().is_empty() {
        return Err(PolicyIoError::InvalidExportPath {
            reason: "directory path must not be empty",
        });
    }

    fs::create_dir_all(directory)?;
    fs::write(directory.join(SCHEMA_FILE_NAME), policy.schema.as_bytes())?;
    let relationship_directory = directory.join(RELATIONSHIP_DIRECTORY_NAME);
    fs::create_dir_all(&relationship_directory)?;
    for file in &policy.relationship_files {
        let relative = Path::new(&file.path);
        if relative.is_absolute() || relative.components().any(is_parent_component) {
            return Err(PolicyIoError::InvalidExportPath {
                reason: "policy file path must be relative and stay inside the export directory",
            });
        }
        fs::write(directory.join(relative), file.contents.as_bytes())?;
    }
    Ok(())
}

pub(crate) fn apply_policy_text_to_service(
    service: &mut crate::WriterState,
    policy: &PolicyText,
) -> Result<crate::revision::ConsistencyToken, ZanzibarError> {
    let published_state = service.published_state.clone();
    let mut candidate = crate::WriterState::with_snapshot_retention(service.retained_snapshots)
        .with_evaluation_limits(service.evaluation_limits);
    candidate.datastore_id = service.datastore_id;
    candidate.last_revision = service.last_revision;
    let mut token = candidate.replace_dsl_with_token(&policy.schema)?;
    let mut mutations = Vec::with_capacity(policy_import_batch_size());
    for relationship in parse_relationships(policy)? {
        mutations.push(crate::relationship::RelationshipMutation::Create(
            relationship,
        ));
        if mutations.len() == policy_import_batch_size() {
            token = candidate.apply_relationship_mutations(std::mem::take(&mut mutations), [])?;
        }
    }
    if !mutations.is_empty() {
        token = candidate.apply_relationship_mutations(mutations, [])?;
    }
    candidate.replace_publisher(published_state);
    *service = candidate;
    Ok(token)
}

pub(crate) fn save_snapshot_from_policy_text(
    path: &Path,
    policy: &PolicyText,
    options: SnapshotSaveOptions,
) -> Result<(), PolicyIoError> {
    let service = crate::WriterState::from_policy_text(policy)?;
    service.save_snapshot(path, options)?;
    Ok(())
}

fn relationship_files(relationships: Vec<Relationship>) -> Vec<PolicyTextFile> {
    let mut groups = BTreeMap::<String, Vec<String>>::new();
    for relationship in relationships {
        groups
            .entry(relationship.resource().object_type().as_str().to_string())
            .or_default()
            .push(relationship.to_string());
    }

    groups
        .into_iter()
        .map(|(resource_type, mut lines)| {
            lines.sort();
            let mut contents = lines.join("\n");
            if !contents.is_empty() {
                contents.push('\n');
            }
            PolicyTextFile {
                path: format!(
                    "{RELATIONSHIP_DIRECTORY_NAME}/{resource_type}.{RELATIONSHIP_FILE_EXTENSION}",
                ),
                contents,
            }
        })
        .collect()
}

fn parse_relationships(policy: &PolicyText) -> Result<Vec<Relationship>, ZanzibarError> {
    let mut relationships = Vec::new();
    for file in &policy.relationship_files {
        for line in file.contents.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with("//") {
                continue;
            }
            relationships.push(trimmed.parse()?);
        }
    }
    Ok(relationships)
}

fn push_relation(output: &mut String, relation: &RelationConfig) {
    if let Some(expression) = &relation.userset_rewrite {
        output.push_str("    relation ");
        output.push_str(&relation.name.0);
        output.push_str(" {\n        rewrite ");
        push_expression(output, expression);
        output.push_str("\n    }\n");
    } else {
        output.push_str("    relation ");
        output.push_str(&relation.name.0);
        output.push_str(" {}\n");
    }
}

fn push_expression(output: &mut String, expression: &UsersetExpression) {
    match expression {
        UsersetExpression::This => output.push_str("this"),
        UsersetExpression::ComputedUserset { relation } => {
            output.push_str("computed_userset(relation: \"");
            output.push_str(&relation.0);
            output.push_str("\")");
        }
        UsersetExpression::TupleToUserset {
            tupleset_relation,
            computed_userset_relation,
        } => {
            output.push_str("tuple_to_userset(tupleset: \"");
            output.push_str(&tupleset_relation.0);
            output.push_str("\", computed_userset: \"");
            output.push_str(&computed_userset_relation.0);
            output.push_str("\")");
        }
        UsersetExpression::Union(expressions) => {
            push_expression_list(output, "union", expressions);
        }
        UsersetExpression::Intersection(expressions) => {
            push_expression_list(output, "intersection", expressions);
        }
        UsersetExpression::Exclusion { base, exclude } => {
            output.push_str("exclusion(");
            push_expression(output, base);
            output.push_str(", ");
            push_expression(output, exclude);
            output.push(')');
        }
    }
}

fn push_expression_list(output: &mut String, name: &str, expressions: &[UsersetExpression]) {
    output.push_str(name);
    output.push('(');
    let mut separator = "";
    for expression in expressions {
        output.push_str(separator);
        push_expression(output, expression);
        separator = ", ";
    }
    output.push(')');
}

fn is_parent_component(component: std::path::Component<'_>) -> bool {
    matches!(component, std::path::Component::ParentDir)
}

const fn policy_import_batch_size() -> usize {
    10_000
}
