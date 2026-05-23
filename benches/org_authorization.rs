//! Benchmarks for large organization authorization scenarios.

use std::{
    fmt::Display,
    hint::black_box,
    num::{NonZeroU32, NonZeroUsize},
    time::Duration,
};

use criterion::{BatchSize, Criterion};
use simple_zanzibar::{
    ZanzibarService,
    domain::{RelationName, Relationship, SubjectId, SubjectRef, SubjectType},
    eval::EvaluationLimits,
    model::{LookupResourcesRequest, LookupSubjectsRequest, Object, Relation, RelationTuple, User},
    relationship::{
        IndexedRelationshipStore, QueryLimit, RelationshipFilter, RelationshipMutation,
        RelationshipReader, SubjectFilter,
    },
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
    service: ZanzibarService,
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
                ZanzibarService::new,
                |mut service| {
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
                |mut service| {
                    apply_generated_relationships(&mut service, WRITE_BATCH_RULES);
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
            bencher.iter(|| {
                black_box(must(
                    store.any_resource_match(black_box(&filter)),
                    "failed to query indexed relationship store",
                ))
            });
        });
    }

    if should_benchmark(reverse_query_name, filters) {
        let target_team_filter = userset_subject_filter("group", "target_team", "member");
        criterion.bench_function(reverse_query_name, |bencher| {
            bencher.iter(|| {
                let count = must(
                    store.reverse_query_relationships(black_box(&target_team_filter)),
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
                black_box(scenario.service.check(
                    black_box(&scenario.direct_doc),
                    black_box(&scenario.can_view),
                    black_box(&scenario.target_user),
                ))
            });
        });

        group.bench_function("check_inherited_folder_viewer", |bencher| {
            bencher.iter(|| {
                black_box(scenario.service.check(
                    black_box(&scenario.inherited_doc),
                    black_box(&scenario.can_view),
                    black_box(&scenario.target_user),
                ))
            });
        });

        group.bench_function("check_denied_exclusion", |bencher| {
            bencher.iter(|| {
                black_box(scenario.service.check(
                    black_box(&scenario.denied_doc),
                    black_box(&scenario.can_view),
                    black_box(&scenario.target_user),
                ))
            });
        });

        group.bench_function("check_editor_can_edit", |bencher| {
            bencher.iter(|| {
                black_box(scenario.service.check(
                    black_box(&scenario.edit_doc),
                    black_box(&scenario.can_edit),
                    black_box(&scenario.editor_user),
                ))
            });
        });

        group.bench_function("expand_direct_doc_viewers", |bencher| {
            bencher.iter(|| {
                black_box(scenario.service.expand(
                    black_box(&scenario.direct_doc),
                    black_box(&scenario.can_view),
                ))
            });
        });

        group.bench_function("lookup_resources_target_user", |bencher| {
            bencher.iter(|| {
                black_box(
                    scenario
                        .service
                        .lookup_resources(black_box(&scenario.lookup_resources_request)),
                )
            });
        });

        group.bench_function("lookup_subjects_direct_doc", |bencher| {
            bencher.iter(|| {
                black_box(
                    scenario
                        .service
                        .lookup_subjects(black_box(&scenario.lookup_subjects_request)),
                )
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
    let mut service = service_with_legacy_relationships(rules);
    must(
        service.add_dsl(org_schema()),
        "failed to apply organization schema",
    );

    let scenario = OrgScenario {
        service,
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

fn configured_service() -> ZanzibarService {
    let mut service = empty_service();
    must(
        service.add_dsl(org_schema()),
        "failed to apply organization schema",
    );
    service
}

fn empty_service() -> ZanzibarService {
    ZanzibarService::new().with_evaluation_limits(EvaluationLimits {
        max_depth: non_zero_u32(50),
        max_fanout_per_step: non_zero_u32(100_000),
        max_lookup_results: non_zero_u32(1_000),
    })
}

fn service_with_legacy_relationships(rules: usize) -> ZanzibarService {
    let mut service = empty_service();

    for relationship in fixed_relationships() {
        write_legacy_tuple(&mut service, &relationship);
    }

    let generated = rules.saturating_sub(fixed_relationship_count());
    for index in 0..generated {
        let relationship = generated_relationship(index);
        write_legacy_tuple(&mut service, &relationship);
    }

    service
}

fn write_legacy_tuple(service: &mut ZanzibarService, relationship: &Relationship) {
    let tuple = relationship_tuple(relationship);
    must(service.write_tuple(tuple), "failed to write legacy tuple");
}

fn relationship_tuple(relationship: &Relationship) -> RelationTuple {
    RelationTuple {
        object: Object {
            namespace: relationship.resource().object_type().to_string(),
            id: relationship.resource().object_id().to_string(),
        },
        relation: Relation(relationship.relation().to_string()),
        user: legacy_user(relationship.subject()),
    }
}

fn legacy_user(subject: &SubjectRef) -> User {
    match subject {
        SubjectRef::Object(object) if object.object_type().as_str() == "user" => {
            User::UserId(object.object_id().to_string())
        }
        SubjectRef::Object(object) => {
            eprintln!("legacy tuple setup cannot represent direct non-user subject: {object}");
            std::process::abort();
        }
        SubjectRef::Userset { object, relation } => User::Userset(
            Object {
                namespace: object.object_type().to_string(),
                id: object.object_id().to_string(),
            },
            Relation(relation.to_string()),
        ),
    }
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
        &scenario.service,
        &scenario.direct_doc,
        &scenario.can_view,
        &scenario.target_user,
        true,
        "direct group viewer check",
    );
    ensure_check(
        &scenario.service,
        &scenario.inherited_doc,
        &scenario.can_view,
        &scenario.target_user,
        true,
        "inherited folder viewer check",
    );
    ensure_check(
        &scenario.service,
        &scenario.denied_doc,
        &scenario.can_view,
        &scenario.target_user,
        false,
        "denied exclusion check",
    );
    ensure_check(
        &scenario.service,
        &scenario.edit_doc,
        &scenario.can_edit,
        &scenario.editor_user,
        true,
        "editor check",
    );
}

fn ensure_check(
    service: &ZanzibarService,
    object: &Object,
    relation: &Relation,
    user: &User,
    expected: bool,
    context: &str,
) {
    let actual = must(service.check(object, relation, user), context);
    if actual != expected {
        eprintln!("{context}: expected {expected}, got {actual}");
        std::process::abort();
    }
}

fn apply_generated_relationships(service: &mut ZanzibarService, rules: usize) {
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

fn push_relationship(
    service: &mut ZanzibarService,
    batch: &mut Vec<RelationshipMutation>,
    relationship: Relationship,
) {
    batch.push(RelationshipMutation::Touch(relationship));
    if batch.len() == MUTATION_BATCH_LIMIT {
        flush_relationships(service, batch);
    }
}

fn flush_relationships(service: &mut ZanzibarService, batch: &mut Vec<RelationshipMutation>) {
    if batch.is_empty() {
        return;
    }

    let mutations = std::mem::take(batch);
    must(
        service.apply_relationship_mutations(mutations, []),
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
    QueryLimit::new(NonZeroUsize::new(value).unwrap_or(NonZeroUsize::MIN))
}

fn non_zero_u32(value: u32) -> NonZeroU32 {
    NonZeroU32::new(value).unwrap_or(NonZeroU32::MIN)
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
