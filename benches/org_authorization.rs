//! Benchmarks for large organization authorization scenarios.

use std::{
    collections::HashMap,
    fmt::Display,
    hint::black_box,
    num::{NonZeroU32, NonZeroUsize},
    sync::Arc,
    time::Duration,
};

use criterion::{BatchSize, Criterion};
use simple_zanzibar::{
    ZanzibarEngine,
    domain::{RelationName, Relationship, SubjectId, SubjectRef, SubjectType},
    eval::{
        EvaluationLimits, check_with_snapshot, expand_with_snapshot,
        lookup_resources_with_snapshot, lookup_subjects_with_snapshot,
    },
    model::{LookupResourcesRequest, LookupSubjectsRequest, Object, Relation, User},
    parser,
    relationship::{
        IndexedRelationshipStore, QueryLimit, RelationshipFilter, RelationshipMutation,
        RelationshipReader, SubjectFilter,
    },
    revision::{PublishedSnapshot, Revision, SchemaHash},
    schema,
};

const ORG_RULE_SIZES: [usize; 3] = [1_000, 100_000, 1_000_000];
const WRITE_BATCH_RULES: usize = 10_000;
const MUTATION_BATCH_LIMIT: usize = 10_000;
const FIXED_RELATIONSHIP_COUNT: usize = 9;
const TARGET_USER_ID: &str = "target_user";
const EDITOR_USER_ID: &str = "editor_user";
const OWNER_USER_ID: &str = "owner_user";

#[derive(Debug)]
struct OrgScenario {
    snapshot: Arc<PublishedSnapshot>,
    limits: EvaluationLimits,
    direct_doc: Object,
    inherited_doc: Object,
    denied_doc: Object,
    edit_doc: Object,
    target_user: User,
    editor_user: User,
    can_view: Relation,
    can_edit: Relation,
    lookup_resources_request: LookupResourcesRequest,
    lookup_subjects_request: LookupSubjectsRequest,
}

fn main() {
    if cfg!(debug_assertions) {
        return;
    }

    let filters = benchmark_filters();
    let mut criterion = Criterion::default()
        .sample_size(10)
        .warm_up_time(Duration::from_millis(100))
        .measurement_time(Duration::from_millis(500))
        .configure_from_args();

    bench_building_blocks(&mut criterion, &filters);
    bench_org_authorization(&mut criterion, &filters);
    criterion.final_summary();
}

fn bench_building_blocks(criterion: &mut Criterion, filters: &[String]) {
    let parse_name = "building_blocks/relationship_parse";
    if should_benchmark(parse_name, filters) {
        criterion.bench_function(parse_name, |bencher| {
            bencher.iter(|| {
                black_box(parse_relationship(black_box(
                    "doc:direct_doc#viewer@group:target_team#member",
                )))
            });
        });
    }

    let schema_name = "building_blocks/schema_apply";
    if should_benchmark(schema_name, filters) {
        criterion.bench_function(schema_name, |bencher| {
            bencher.iter_batched(
                || ZanzibarEngine::builder().build(),
                |service| {
                    must(
                        service.add_dsl(black_box(org_schema())),
                        "failed to apply schema",
                    );
                    black_box(service)
                },
                BatchSize::SmallInput,
            );
        });
    }

    let write_name = "building_blocks/write_batch_10k";
    if should_benchmark(write_name, filters) {
        criterion.bench_function(write_name, |bencher| {
            bencher.iter_batched(
                configured_service,
                |service| {
                    apply_generated_relationships(&service, WRITE_BATCH_RULES);
                    black_box(service)
                },
                BatchSize::LargeInput,
            );
        });
    }

    let exact_query_name = "building_blocks/indexed_store_exact_query_100k";
    let reverse_query_name = "building_blocks/indexed_store_reverse_query_100k";
    if !should_benchmark(exact_query_name, filters)
        && !should_benchmark(reverse_query_name, filters)
    {
        return;
    }

    let store = indexed_store_with_relationships(100_000);

    if should_benchmark(exact_query_name, filters) {
        let relationship =
            parse_relationship("doc:bulk_doc_099990#viewer@group:target_team#member");
        let filter = exact_relationship_filter(&relationship);
        criterion.bench_function(exact_query_name, |bencher| {
            bencher.iter(|| black_box(store.any_resource_match(black_box(&filter))));
        });
    }

    if should_benchmark(reverse_query_name, filters) {
        let reader = must(
            store.materialized_reader(),
            "failed to materialize indexed relationship store reader",
        );
        let target_team_filter = userset_subject_filter("group", "target_team", "member");
        criterion.bench_function(reverse_query_name, |bencher| {
            bencher.iter(|| {
                let count = must(
                    reader.reverse_query_relationships(black_box(&target_team_filter)),
                    "failed to reverse query indexed relationship store",
                )
                .count();
                black_box(count)
            });
        });
    }
}

fn bench_org_authorization(criterion: &mut Criterion, filters: &[String]) {
    for rules in ORG_RULE_SIZES {
        let group_name = format!("org_authorization/{}_rules", rule_label(rules));
        if !should_benchmark_group(&group_name, ORG_BENCHMARKS, filters) {
            continue;
        }

        let scenario = build_org_scenario(rules);
        let mut group = criterion.benchmark_group(group_name);

        group.bench_function("check_direct_group_viewer", |bencher| {
            bencher.iter(|| {
                black_box(check_with_snapshot(
                    &scenario.snapshot,
                    black_box(&scenario.direct_doc),
                    black_box(&scenario.can_view),
                    black_box(&scenario.target_user),
                    black_box(scenario.limits),
                ))
            });
        });

        group.bench_function("check_inherited_folder_viewer", |bencher| {
            bencher.iter(|| {
                black_box(check_with_snapshot(
                    &scenario.snapshot,
                    black_box(&scenario.inherited_doc),
                    black_box(&scenario.can_view),
                    black_box(&scenario.target_user),
                    black_box(scenario.limits),
                ))
            });
        });

        group.bench_function("check_denied_exclusion", |bencher| {
            bencher.iter(|| {
                black_box(check_with_snapshot(
                    &scenario.snapshot,
                    black_box(&scenario.denied_doc),
                    black_box(&scenario.can_view),
                    black_box(&scenario.target_user),
                    black_box(scenario.limits),
                ))
            });
        });

        group.bench_function("check_editor_can_edit", |bencher| {
            bencher.iter(|| {
                black_box(check_with_snapshot(
                    &scenario.snapshot,
                    black_box(&scenario.edit_doc),
                    black_box(&scenario.can_edit),
                    black_box(&scenario.editor_user),
                    black_box(scenario.limits),
                ))
            });
        });

        group.bench_function("expand_direct_doc_viewers", |bencher| {
            bencher.iter(|| {
                black_box(expand_with_snapshot(
                    &scenario.snapshot,
                    black_box(&scenario.direct_doc),
                    black_box(&scenario.can_view),
                    black_box(scenario.limits),
                ))
            });
        });

        group.bench_function("lookup_resources_target_user", |bencher| {
            bencher.iter(|| {
                black_box(lookup_resources_with_snapshot(
                    &scenario.snapshot,
                    black_box(&scenario.lookup_resources_request),
                    black_box(scenario.limits),
                ))
            });
        });

        group.bench_function("lookup_subjects_direct_doc", |bencher| {
            bencher.iter(|| {
                black_box(lookup_subjects_with_snapshot(
                    &scenario.snapshot,
                    black_box(&scenario.lookup_subjects_request),
                    black_box(scenario.limits),
                ))
            });
        });

        group.finish();
    }
}

const ORG_BENCHMARKS: &[&str] = &[
    "check_direct_group_viewer",
    "check_inherited_folder_viewer",
    "check_denied_exclusion",
    "check_editor_can_edit",
    "expand_direct_doc_viewers",
    "lookup_resources_target_user",
    "lookup_subjects_direct_doc",
];

fn benchmark_filters() -> Vec<String> {
    let mut filters = Vec::new();
    let mut skip_next = false;

    for argument in std::env::args().skip(1) {
        if skip_next {
            skip_next = false;
            continue;
        }

        if argument == "--bench" {
            continue;
        }

        if option_takes_value(argument.as_str()) {
            skip_next = !argument.contains('=');
            continue;
        }

        if argument.starts_with('-') {
            continue;
        }

        filters.push(argument);
    }

    filters
}

fn option_takes_value(argument: &str) -> bool {
    matches!(
        argument,
        "--sample-size"
            | "--warm-up-time"
            | "--measurement-time"
            | "--nresamples"
            | "--confidence-level"
            | "--significance-level"
            | "--noise-threshold"
            | "--profile-time"
            | "--plotting-backend"
            | "--save-baseline"
            | "--baseline"
            | "--load-baseline"
            | "--output-format"
    ) || argument.starts_with("--sample-size=")
        || argument.starts_with("--warm-up-time=")
        || argument.starts_with("--measurement-time=")
        || argument.starts_with("--nresamples=")
        || argument.starts_with("--confidence-level=")
        || argument.starts_with("--significance-level=")
        || argument.starts_with("--noise-threshold=")
        || argument.starts_with("--profile-time=")
        || argument.starts_with("--plotting-backend=")
        || argument.starts_with("--save-baseline=")
        || argument.starts_with("--baseline=")
        || argument.starts_with("--load-baseline=")
        || argument.starts_with("--output-format=")
}

fn should_benchmark(name: &str, filters: &[String]) -> bool {
    filters.is_empty() || filters.iter().any(|filter| name.contains(filter.as_str()))
}

fn should_benchmark_group(prefix: &str, benchmarks: &[&str], filters: &[String]) -> bool {
    filters.is_empty()
        || benchmarks.iter().any(|benchmark| {
            let name = format!("{prefix}/{benchmark}");
            should_benchmark(&name, filters)
        })
}

fn rule_label(rules: usize) -> &'static str {
    match rules {
        1_000 => "1k",
        100_000 => "100k",
        1_000_000 => "1m",
        _ => "custom",
    }
}

fn build_org_scenario(rules: usize) -> OrgScenario {
    let limits = evaluation_limits();
    let snapshot = build_org_snapshot(rules);

    let scenario = OrgScenario {
        snapshot,
        limits,
        direct_doc: object("doc", "direct_doc"),
        inherited_doc: object("doc", "inherited_doc"),
        denied_doc: object("doc", "denied_doc"),
        edit_doc: object("doc", "edit_doc"),
        target_user: User::UserId(TARGET_USER_ID.to_string()),
        editor_user: User::UserId(EDITOR_USER_ID.to_string()),
        can_view: relation("can_view"),
        can_edit: relation("can_edit"),
        lookup_resources_request: LookupResourcesRequest {
            subject: User::UserId(TARGET_USER_ID.to_string()),
            permission: relation("can_view"),
            resource_type: "doc".to_string(),
        },
        lookup_subjects_request: LookupSubjectsRequest {
            resource: object("doc", "direct_doc"),
            permission: relation("can_view"),
            subject_type: "user".to_string(),
        },
    };

    validate_scenario(&scenario);
    scenario
}

fn configured_service() -> ZanzibarEngine {
    let service = ZanzibarEngine::builder()
        .retained_snapshots(non_zero_usize(1))
        .evaluation_limits(evaluation_limits())
        .build();
    must(
        service.add_dsl(org_schema()),
        "failed to apply organization schema",
    );
    service
}

fn evaluation_limits() -> EvaluationLimits {
    EvaluationLimits {
        max_depth: non_zero_u32(50),
        max_fanout_per_step: non_zero_u32(100_000),
        max_lookup_results: non_zero_u32(1_000),
    }
}

fn build_org_snapshot(rules: usize) -> Arc<PublishedSnapshot> {
    let configs = must(
        parser::parse_dsl(org_schema()),
        "failed to parse org schema",
    );
    let schema = must(
        schema::compile_legacy_configs(configs.clone()),
        "failed to compile org schema",
    );
    let mut relationships = IndexedRelationshipStore::default();
    apply_generated_store_relationships(&mut relationships, rules);
    let configs = configs
        .into_iter()
        .map(|config| (config.name.clone(), config))
        .collect::<HashMap<_, _>>();
    Arc::new(PublishedSnapshot::new(
        Revision::first(),
        SchemaHash::for_schema(&schema),
        Arc::new(configs),
        Arc::new(schema),
        Arc::new(relationships),
    ))
}

fn org_schema() -> &'static str {
    r#"
    namespace group {
        relation member {}
    }

    namespace folder {
        relation viewer {}
        relation editor {}
        relation owner {}

        relation inherited_viewer {
            rewrite union(
                this,
                computed_userset(relation: "viewer"),
                computed_userset(relation: "editor"),
                computed_userset(relation: "owner")
            )
        }
    }

    namespace doc {
        relation parent {}
        relation viewer {}
        relation editor {}
        relation owner {}
        relation banned {}

        relation can_view {
            rewrite exclusion(
                union(
                    computed_userset(relation: "viewer"),
                    computed_userset(relation: "editor"),
                    computed_userset(relation: "owner"),
                    tuple_to_userset(tupleset: "parent", computed_userset: "inherited_viewer")
                ),
                computed_userset(relation: "banned")
            )
        }

        relation can_edit {
            rewrite union(
                computed_userset(relation: "editor"),
                computed_userset(relation: "owner")
            )
        }
    }
    "#
}

fn validate_scenario(scenario: &OrgScenario) {
    ensure_check(
        &scenario.snapshot,
        scenario.limits,
        &scenario.direct_doc,
        &scenario.can_view,
        &scenario.target_user,
        true,
        "direct group viewer check",
    );
    ensure_check(
        &scenario.snapshot,
        scenario.limits,
        &scenario.inherited_doc,
        &scenario.can_view,
        &scenario.target_user,
        true,
        "inherited folder viewer check",
    );
    ensure_check(
        &scenario.snapshot,
        scenario.limits,
        &scenario.denied_doc,
        &scenario.can_view,
        &scenario.target_user,
        false,
        "denied exclusion check",
    );
    ensure_check(
        &scenario.snapshot,
        scenario.limits,
        &scenario.edit_doc,
        &scenario.can_edit,
        &scenario.editor_user,
        true,
        "editor check",
    );
}

fn ensure_check(
    snapshot: &PublishedSnapshot,
    limits: EvaluationLimits,
    object: &Object,
    relation: &Relation,
    user: &User,
    expected: bool,
    context: &str,
) {
    let actual = must(
        check_with_snapshot(snapshot, object, relation, user, limits),
        context,
    )
    .is_allowed();
    if actual != expected {
        eprintln!("{context}: expected {expected}, got {actual}");
        std::process::abort();
    }
}

fn apply_generated_relationships(service: &ZanzibarEngine, rules: usize) {
    let fixed_relationships = fixed_relationships();
    let mut batch = Vec::with_capacity(MUTATION_BATCH_LIMIT);

    for relationship in fixed_relationships {
        push_relationship(service, &mut batch, relationship);
    }

    let generated = rules.saturating_sub(fixed_relationship_count());
    for index in 0..generated {
        push_relationship(service, &mut batch, generated_relationship(index));
    }

    flush_relationships(service, &mut batch);
}

fn apply_generated_store_relationships(store: &mut IndexedRelationshipStore, rules: usize) {
    let fixed_relationships = fixed_relationships();
    let mut batch = Vec::with_capacity(MUTATION_BATCH_LIMIT);

    for relationship in fixed_relationships {
        push_store_relationship(store, &mut batch, relationship);
    }

    let generated = rules.saturating_sub(fixed_relationship_count());
    for index in 0..generated {
        push_store_relationship(store, &mut batch, generated_relationship(index));
    }

    flush_store_relationships(store, &mut batch);
}

fn push_relationship(
    service: &ZanzibarEngine,
    batch: &mut Vec<RelationshipMutation>,
    relationship: Relationship,
) {
    batch.push(RelationshipMutation::Touch(relationship));
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
        service.write_relationships_with_preconditions(mutations, []),
        "failed to apply generated relationship batch",
    );
}

fn indexed_store_with_relationships(count: usize) -> IndexedRelationshipStore {
    let mut store = IndexedRelationshipStore::default();
    let mut batch = Vec::with_capacity(MUTATION_BATCH_LIMIT);

    for relationship in fixed_relationships() {
        push_store_relationship(&mut store, &mut batch, relationship);
    }

    let generated = count.saturating_sub(fixed_relationship_count());
    for index in 0..generated {
        push_store_relationship(&mut store, &mut batch, generated_relationship(index));
    }

    flush_store_relationships(&mut store, &mut batch);
    store
}

fn push_store_relationship(
    store: &mut IndexedRelationshipStore,
    batch: &mut Vec<RelationshipMutation>,
    relationship: Relationship,
) {
    batch.push(RelationshipMutation::Touch(relationship));
    if batch.len() == MUTATION_BATCH_LIMIT {
        flush_store_relationships(store, batch);
    }
}

fn flush_store_relationships(
    store: &mut IndexedRelationshipStore,
    batch: &mut Vec<RelationshipMutation>,
) {
    if batch.is_empty() {
        return;
    }

    let mutations = std::mem::take(batch);
    must(
        store.apply_mutations(mutations, []),
        "failed to apply indexed store batch",
    );
}

fn fixed_relationships() -> [Relationship; FIXED_RELATIONSHIP_COUNT] {
    [
        parse_relationship(format!("group:target_team#member@user:{TARGET_USER_ID}")),
        parse_relationship("folder:target_folder#viewer@group:target_team#member"),
        parse_relationship("doc:inherited_doc#parent@folder:target_folder#inherited_viewer"),
        parse_relationship("doc:direct_doc#viewer@group:target_team#member"),
        parse_relationship("doc:denied_doc#viewer@group:target_team#member"),
        parse_relationship(format!("doc:denied_doc#banned@user:{TARGET_USER_ID}")),
        parse_relationship(format!("group:edit_team#member@user:{EDITOR_USER_ID}")),
        parse_relationship("doc:edit_doc#editor@group:edit_team#member"),
        parse_relationship(format!("doc:owner_doc#owner@user:{OWNER_USER_ID}")),
    ]
}

fn fixed_relationship_count() -> usize {
    FIXED_RELATIONSHIP_COUNT
}

fn generated_relationship(index: usize) -> Relationship {
    match index % 6 {
        0 => parse_relationship(format!(
            "doc:bulk_doc_{index:06}#viewer@group:target_team#member",
        )),
        1 => parse_relationship(format!(
            "doc:bulk_doc_{index:06}#parent@folder:bulk_folder_{:05}#inherited_viewer",
            index / 100,
        )),
        2 => parse_relationship(format!(
            "folder:bulk_folder_{:05}#viewer@group:bulk_team_{:05}#member",
            index / 100,
            index % 10_000,
        )),
        3 => parse_relationship(format!(
            "group:bulk_team_{:05}#member@user:bulk_user_{index:06}",
            index % 10_000,
        )),
        4 => parse_relationship(format!(
            "doc:bulk_doc_{index:06}#editor@group:bulk_team_{:05}#member",
            index % 10_000,
        )),
        _ => parse_relationship(format!(
            "doc:bulk_doc_{index:06}#banned@user:blocked_user_{index:06}",
        )),
    }
}

fn exact_relationship_filter(relationship: &Relationship) -> RelationshipFilter {
    RelationshipFilter::new(
        relationship.resource().object_type().clone(),
        Some(relationship.resource().object_id().clone()),
        Some(relationship.relation().clone()),
        Some(subject_filter(relationship.subject())),
        query_limit(1),
    )
}

fn subject_filter(subject: &SubjectRef) -> SubjectFilter {
    match subject {
        SubjectRef::Object(object) => SubjectFilter::exact(
            create_subject_type(object.object_type().as_str()),
            create_subject_id(object.object_id().as_str()),
            None,
        ),
        SubjectRef::Userset { object, relation } => SubjectFilter::exact(
            create_subject_type(object.object_type().as_str()),
            create_subject_id(object.object_id().as_str()),
            Some(relation.clone()),
        ),
    }
}

fn userset_subject_filter(subject_type: &str, subject_id: &str, relation: &str) -> SubjectFilter {
    SubjectFilter::exact(
        create_subject_type(subject_type),
        create_subject_id(subject_id),
        Some(relation_name(relation)),
    )
}

fn parse_relationship(value: impl AsRef<str>) -> Relationship {
    must(
        value.as_ref().parse(),
        "failed to parse benchmark relationship",
    )
}

fn object(namespace: &str, id: &str) -> Object {
    Object {
        namespace: namespace.to_string(),
        id: id.to_string(),
    }
}

fn relation(value: &str) -> Relation {
    Relation(value.to_string())
}

fn relation_name(value: &str) -> RelationName {
    must(
        RelationName::try_from(value),
        "failed to create relation name",
    )
}

fn create_subject_type(value: &str) -> SubjectType {
    must(
        SubjectType::try_from(value),
        "failed to create subject type",
    )
}

fn create_subject_id(value: &str) -> SubjectId {
    must(SubjectId::try_from(value), "failed to create subject id")
}

fn query_limit(value: usize) -> QueryLimit {
    QueryLimit::new(non_zero_usize(value))
}

fn non_zero_u32(value: u32) -> NonZeroU32 {
    NonZeroU32::new(value).unwrap_or(NonZeroU32::MIN)
}

fn non_zero_usize(value: usize) -> NonZeroUsize {
    NonZeroUsize::new(value).unwrap_or(NonZeroUsize::MIN)
}

fn must<T, E: Display>(result: Result<T, E>, context: &str) -> T {
    match result {
        Ok(value) => value,
        Err(error) => {
            eprintln!("{context}: {error}");
            std::process::abort();
        }
    }
}
