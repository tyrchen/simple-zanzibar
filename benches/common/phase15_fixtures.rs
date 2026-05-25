//! Shared Phase 15 read-optimization benchmark fixtures.

use std::{num::NonZeroU32, process};

use simple_zanzibar::{ZanzibarEngine, eval::EvaluationLimits, relationship::RelationshipMutation};

pub const PHASE15_TARGET_USER_ID: &str = "target_user";
pub const PHASE15_TARGETED_RULES: usize = 100_000;
pub const PHASE15_LOOKUP_SUBJECTS: usize = 2_048;
pub const PHASE15_HIGH_FANOUT: usize = 16_384;

pub fn build_phase15_shared_parent_engine(rules: usize) -> ZanzibarEngine {
    let service = phase15_engine(PHASE15_SHARED_PARENT_SCHEMA);
    let mut batch = Vec::with_capacity(MUTATION_BATCH_LIMIT);
    push_relationship_line(
        &service,
        &mut batch,
        format!("group:shared#member@user:{PHASE15_TARGET_USER_ID}"),
    );
    push_relationship_line(
        &service,
        &mut batch,
        "folder:shared#viewer@group:shared#member".to_string(),
    );
    for index in 0..rules.saturating_sub(2) {
        push_relationship_line(
            &service,
            &mut batch,
            format!("doc:shared_{index:06}#parent@folder:shared#viewer"),
        );
    }
    flush_relationships(&service, &mut batch);
    service
}

pub fn build_phase15_lookup_subjects_engine(subjects: usize) -> ZanzibarEngine {
    let service = phase15_engine(PHASE15_LOOKUP_SUBJECTS_SCHEMA);
    let mut batch = Vec::with_capacity(MUTATION_BATCH_LIMIT);
    for index in 0..subjects {
        push_relationship_line(
            &service,
            &mut batch,
            format!("group:alloc_{index:05}#member@user:alloc_user_{index:05}"),
        );
        push_relationship_line(
            &service,
            &mut batch,
            format!("doc:allocation#viewer@group:alloc_{index:05}#member"),
        );
    }
    flush_relationships(&service, &mut batch);
    service
}

pub fn build_phase15_high_fanout_engine(subjects: usize) -> ZanzibarEngine {
    let service = phase15_engine(PHASE15_DIRECT_VIEWER_SCHEMA);
    let mut batch = Vec::with_capacity(MUTATION_BATCH_LIMIT);
    for index in 0..subjects {
        push_relationship_line(
            &service,
            &mut batch,
            format!("doc:high_fanout#viewer@user:fanout_{index:05}"),
        );
    }
    flush_relationships(&service, &mut batch);
    service
}

fn phase15_engine(schema: &str) -> ZanzibarEngine {
    let service = ZanzibarEngine::builder()
        .evaluation_limits(evaluation_limits())
        .build();
    must(service.add_dsl(schema), "failed to apply phase15 schema");
    service
}

fn push_relationship_line(
    service: &ZanzibarEngine,
    batch: &mut Vec<RelationshipMutation>,
    relationship: String,
) {
    batch.push(must(
        RelationshipMutation::touch(relationship),
        "failed to parse phase15 relationship",
    ));
    if batch.len() == MUTATION_BATCH_LIMIT {
        flush_relationships(service, batch);
    }
}

fn flush_relationships(service: &ZanzibarEngine, batch: &mut Vec<RelationshipMutation>) {
    if batch.is_empty() {
        return;
    }
    let mutations = std::mem::take(batch);
    must(
        service.write_relationships(mutations),
        "failed to write phase15 relationships",
    );
}

fn evaluation_limits() -> EvaluationLimits {
    EvaluationLimits {
        max_depth: non_zero_u32(50),
        max_fanout_per_step: non_zero_u32(100_000),
        max_lookup_results: non_zero_u32(1_000),
    }
}

fn non_zero_u32(value: u32) -> NonZeroU32 {
    match NonZeroU32::new(value) {
        Some(value) => value,
        None => NonZeroU32::MIN,
    }
}

fn must<T, E>(result: Result<T, E>, context: &str) -> T
where
    E: std::fmt::Display,
{
    match result {
        Ok(value) => value,
        Err(error) => {
            eprintln!("{context}: {error}");
            process::abort();
        }
    }
}

const MUTATION_BATCH_LIMIT: usize = 10_000;

const PHASE15_SHARED_PARENT_SCHEMA: &str = r#"
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
        relation viewer {}

        relation can_view {
            rewrite union(
                computed_userset(relation: "viewer"),
                tuple_to_userset(tupleset: "parent", computed_userset: "inherited_viewer")
            )
        }
    }
    "#;

const PHASE15_LOOKUP_SUBJECTS_SCHEMA: &str = r#"
    namespace group {
        relation member {}
    }

    namespace doc {
        relation viewer {}

        relation can_view {
            rewrite computed_userset(relation: "viewer")
        }
    }
    "#;

const PHASE15_DIRECT_VIEWER_SCHEMA: &str = r"
    namespace doc {
        relation viewer {}
    }
    ";
