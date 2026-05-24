//! Benchmarks for a realistic multi-tenant collaboration authorization workload.

use std::{
    env, fs,
    hint::black_box,
    num::NonZeroU32,
    path::{Path, PathBuf},
    process,
    time::Duration,
};

use criterion::{BenchmarkGroup, Criterion, measurement::WallTime};
use simple_zanzibar::{
    SnapshotIntegrityMode, SnapshotLoadOptions, SnapshotSaveOptions, SnapshotValidationMode,
    ZanzibarEngine,
    eval::EvaluationLimits,
    model::{
        CheckRequest, ExpandRequest, LookupObjectPermissionsRequest, LookupPermissionsRequest,
        LookupResourcesRequest, LookupSubjectsRequest, Object, Relation, User,
    },
    relationship::RelationshipMutation,
    revision::Consistency,
};

const SCALES: [WorkloadScale; 2] = [
    WorkloadScale {
        label: "100k",
        relationships: 100_000,
    },
    WorkloadScale {
        label: "1m",
        relationships: 1_000_000,
    },
];
const MUTATION_BATCH_LIMIT: usize = 10_000;
const TARGET_USER: &str = "target_user";
const DIRECT_USER: &str = "direct_user";
const EDITOR_USER: &str = "editor_user";
const OWNER_USER: &str = "owner_user";
const AUDITOR_USER: &str = "auditor_user";
const BLOCKED_USER: &str = "blocked_user";

#[derive(Debug, Clone, Copy)]
struct WorkloadScale {
    label: &'static str,
    relationships: usize,
}

#[derive(Debug)]
struct RealworldScenario {
    engine: ZanzibarEngine,
    inherited_doc: Object,
    direct_doc: Object,
    denied_doc: Object,
    shared_doc: Object,
    launch_project: Object,
    can_view: Relation,
    can_edit: Relation,
    can_share: Relation,
    target_user: User,
    direct_user: User,
    editor_user: User,
    blocked_user: User,
    lookup_resources: LookupResourcesRequest,
    lookup_subjects: LookupSubjectsRequest,
    lookup_permissions: LookupPermissionsRequest,
    lookup_object_permissions: LookupObjectPermissionsRequest,
    expand_shared_doc: ExpandRequest,
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

    for scale in SCALES {
        if should_build_scale(scale, &filters) {
            bench_scale(&mut criterion, scale, &filters);
        }
    }

    criterion.final_summary();
}

fn bench_scale(criterion: &mut Criterion, scale: WorkloadScale, filters: &[String]) {
    let group_name = format!("realworld_authorization/{}_rules", scale.label);
    let read_benchmarks = [
        "check_doc_inherited_workspace_member",
        "check_doc_direct_user",
        "check_doc_denied_by_ban",
        "check_doc_project_editor",
        "lookup_resources_target_user",
        "lookup_subjects_shared_doc",
        "lookup_permissions_shared_doc",
        "lookup_object_permissions_shared_doc",
        "expand_shared_doc",
        "mixed_read_workload",
    ];
    let snapshot_benchmarks = [
        "snapshot_load_compact",
        "snapshot_load_trusted_fast",
        "snapshot_file_size",
    ];

    let read_requested = read_benchmarks
        .iter()
        .any(|benchmark| should_benchmark(&format!("{group_name}/{benchmark}"), filters));
    let snapshot_requested = snapshot_benchmarks
        .iter()
        .any(|benchmark| should_benchmark(&format!("{group_name}/{benchmark}"), filters));

    let scenario = if read_requested || snapshot_requested {
        Some(build_scenario(scale.relationships))
    } else {
        None
    };

    if let Some(scenario) = scenario.as_ref()
        && read_requested
    {
        bench_reads(criterion, &group_name, filters, scenario);
    }

    if let Some(scenario) = scenario.as_ref()
        && snapshot_requested
    {
        bench_snapshot_artifacts(criterion, &group_name, filters, scenario, scale);
    }
}

fn bench_reads(
    criterion: &mut Criterion,
    group_name: &str,
    filters: &[String],
    scenario: &RealworldScenario,
) {
    let mut group = criterion.benchmark_group(group_name);
    bench_check_reads(&mut group, group_name, filters, scenario);
    bench_lookup_reads(&mut group, group_name, filters, scenario);
    bench_expand_and_mixed_reads(&mut group, group_name, filters, scenario);
    group.finish();
}

fn bench_check_reads(
    group: &mut BenchmarkGroup<'_, WallTime>,
    group_name: &str,
    filters: &[String],
    scenario: &RealworldScenario,
) {
    let inherited_name = "check_doc_inherited_workspace_member";
    if should_benchmark(&format!("{group_name}/{inherited_name}"), filters) {
        bench_check_read(
            group,
            inherited_name,
            scenario,
            CheckRead {
                object: &scenario.inherited_doc,
                relation: &scenario.can_view,
                user: &scenario.target_user,
                context: "inherited check failed",
            },
        );
    }

    let direct_name = "check_doc_direct_user";
    if should_benchmark(&format!("{group_name}/{direct_name}"), filters) {
        bench_check_read(
            group,
            direct_name,
            scenario,
            CheckRead {
                object: &scenario.direct_doc,
                relation: &scenario.can_view,
                user: &scenario.direct_user,
                context: "direct check failed",
            },
        );
    }

    let denied_name = "check_doc_denied_by_ban";
    if should_benchmark(&format!("{group_name}/{denied_name}"), filters) {
        bench_check_read(
            group,
            denied_name,
            scenario,
            CheckRead {
                object: &scenario.denied_doc,
                relation: &scenario.can_view,
                user: &scenario.blocked_user,
                context: "denied check failed",
            },
        );
    }

    let editor_name = "check_doc_project_editor";
    if should_benchmark(&format!("{group_name}/{editor_name}"), filters) {
        bench_check_read(
            group,
            editor_name,
            scenario,
            CheckRead {
                object: &scenario.inherited_doc,
                relation: &scenario.can_edit,
                user: &scenario.editor_user,
                context: "editor check failed",
            },
        );
    }
}

#[derive(Debug, Clone, Copy)]
struct CheckRead<'a> {
    object: &'a Object,
    relation: &'a Relation,
    user: &'a User,
    context: &'static str,
}

fn bench_check_read(
    group: &mut BenchmarkGroup<'_, WallTime>,
    name: &str,
    scenario: &RealworldScenario,
    read: CheckRead<'_>,
) {
    group.bench_function(name, |bencher| {
        bencher.iter(|| {
            black_box(must(
                scenario.engine.check(black_box(CheckRequest::new(
                    read.object.clone(),
                    read.relation.clone(),
                    read.user.clone(),
                    Consistency::Latest,
                ))),
                read.context,
            ))
        });
    });
}

fn bench_lookup_reads(
    group: &mut BenchmarkGroup<'_, WallTime>,
    group_name: &str,
    filters: &[String],
    scenario: &RealworldScenario,
) {
    let lookup_resources_name = "lookup_resources_target_user";
    if should_benchmark(&format!("{group_name}/{lookup_resources_name}"), filters) {
        group.bench_function(lookup_resources_name, |bencher| {
            bencher.iter(|| {
                black_box(must(
                    scenario
                        .engine
                        .lookup_resources(black_box(scenario.lookup_resources.clone())),
                    "lookup resources failed",
                ))
            });
        });
    }

    let lookup_subjects_name = "lookup_subjects_shared_doc";
    if should_benchmark(&format!("{group_name}/{lookup_subjects_name}"), filters) {
        group.bench_function(lookup_subjects_name, |bencher| {
            bencher.iter(|| {
                black_box(must(
                    scenario
                        .engine
                        .lookup_subjects(black_box(scenario.lookup_subjects.clone())),
                    "lookup subjects failed",
                ))
            });
        });
    }

    let lookup_permissions_name = "lookup_permissions_shared_doc";
    if should_benchmark(&format!("{group_name}/{lookup_permissions_name}"), filters) {
        group.bench_function(lookup_permissions_name, |bencher| {
            bencher.iter(|| {
                black_box(must(
                    scenario
                        .engine
                        .lookup_permissions(black_box(scenario.lookup_permissions.clone())),
                    "lookup permissions failed",
                ))
            });
        });
    }

    let lookup_object_permissions_name = "lookup_object_permissions_shared_doc";
    if should_benchmark(
        &format!("{group_name}/{lookup_object_permissions_name}"),
        filters,
    ) {
        group.bench_function(lookup_object_permissions_name, |bencher| {
            bencher.iter(|| {
                black_box(must(
                    scenario.engine.lookup_object_permissions(black_box(
                        scenario.lookup_object_permissions.clone(),
                    )),
                    "lookup object permissions failed",
                ))
            });
        });
    }
}

fn bench_expand_and_mixed_reads(
    group: &mut BenchmarkGroup<'_, WallTime>,
    group_name: &str,
    filters: &[String],
    scenario: &RealworldScenario,
) {
    let expand_name = "expand_shared_doc";
    if should_benchmark(&format!("{group_name}/{expand_name}"), filters) {
        group.bench_function(expand_name, |bencher| {
            bencher.iter(|| {
                black_box(must(
                    scenario
                        .engine
                        .expand(black_box(scenario.expand_shared_doc.clone())),
                    "expand failed",
                ))
            });
        });
    }

    let mixed_name = "mixed_read_workload";
    if should_benchmark(&format!("{group_name}/{mixed_name}"), filters) {
        group.bench_function(mixed_name, |bencher| {
            bencher.iter(|| black_box(run_mixed_read_workload(scenario)));
        });
    }
}

fn bench_snapshot_artifacts(
    criterion: &mut Criterion,
    group_name: &str,
    filters: &[String],
    scenario: &RealworldScenario,
    scale: WorkloadScale,
) {
    let load_name = "snapshot_load_compact";
    let trusted_name = "snapshot_load_trusted_fast";
    let size_name = "snapshot_file_size";
    let needs_snapshot = [load_name, trusted_name, size_name]
        .iter()
        .any(|name| should_benchmark(&format!("{group_name}/{name}"), filters));
    if !needs_snapshot {
        return;
    }

    let path = prepared_snapshot(scenario, scale);
    let mut group = criterion.benchmark_group(group_name);

    if should_benchmark(&format!("{group_name}/{load_name}"), filters) {
        group.bench_function(load_name, |bencher| {
            bencher.iter(|| {
                black_box(must(
                    ZanzibarEngine::load_snapshot(&path, SnapshotLoadOptions::default()),
                    "snapshot load failed",
                ))
            });
        });
    }

    if should_benchmark(&format!("{group_name}/{trusted_name}"), filters) {
        group.bench_function(trusted_name, |bencher| {
            bencher.iter(|| {
                black_box(must(
                    ZanzibarEngine::load_snapshot(&path, trusted_fast_load_options()),
                    "trusted snapshot load failed",
                ))
            });
        });
    }

    if should_benchmark(&format!("{group_name}/{size_name}"), filters) {
        let len = must(fs::metadata(&path), "snapshot metadata failed").len();
        eprintln!("{group_name}/{size_name}: {len} bytes");
        group.bench_function(size_name, |bencher| {
            bencher.iter(|| black_box(len));
        });
    }

    group.finish();
    remove_file(&path);
}

fn run_mixed_read_workload(scenario: &RealworldScenario) -> usize {
    let mut allowed = 0_usize;
    let checks = [
        (
            scenario.inherited_doc.clone(),
            scenario.can_view.clone(),
            scenario.target_user.clone(),
        ),
        (
            scenario.direct_doc.clone(),
            scenario.can_view.clone(),
            scenario.direct_user.clone(),
        ),
        (
            scenario.denied_doc.clone(),
            scenario.can_view.clone(),
            scenario.blocked_user.clone(),
        ),
        (
            scenario.inherited_doc.clone(),
            scenario.can_edit.clone(),
            scenario.editor_user.clone(),
        ),
        (
            scenario.shared_doc.clone(),
            scenario.can_share.clone(),
            scenario.editor_user.clone(),
        ),
    ];

    for (object, relation, user) in checks {
        let response = must(
            scenario.engine.check(CheckRequest::new(
                object,
                relation,
                user,
                Consistency::Latest,
            )),
            "mixed check failed",
        );
        if response.allowed {
            allowed = allowed.saturating_add(1);
        }
    }

    allowed = allowed.saturating_add(
        must(
            scenario
                .engine
                .lookup_resources(scenario.lookup_resources.clone()),
            "mixed lookup resources failed",
        )
        .resources
        .len(),
    );
    allowed = allowed.saturating_add(
        must(
            scenario
                .engine
                .lookup_subjects(scenario.lookup_subjects.clone()),
            "mixed lookup subjects failed",
        )
        .subjects
        .len(),
    );
    allowed = allowed.saturating_add(
        must(
            scenario
                .engine
                .lookup_permissions(scenario.lookup_permissions.clone()),
            "mixed lookup permissions failed",
        )
        .permissions
        .len(),
    );
    allowed
}

fn build_scenario(relationships: usize) -> RealworldScenario {
    let engine = ZanzibarEngine::builder()
        .evaluation_limits(evaluation_limits())
        .build();
    must(
        engine.add_dsl(REALWORLD_SCHEMA),
        "failed to apply realworld schema",
    );
    apply_relationships(&engine, relationships);

    let scenario = RealworldScenario {
        engine,
        inherited_doc: object("doc", "tenant_000_doc_inherited"),
        direct_doc: object("doc", "tenant_000_doc_direct"),
        denied_doc: object("doc", "tenant_000_doc_denied"),
        shared_doc: object("doc", "tenant_000_doc_shared"),
        launch_project: object("project", "tenant_000_project_000"),
        can_view: relation("can_view"),
        can_edit: relation("can_edit"),
        can_share: relation("can_share"),
        target_user: user(TARGET_USER),
        direct_user: user(DIRECT_USER),
        editor_user: user(EDITOR_USER),
        blocked_user: user(BLOCKED_USER),
        lookup_resources: LookupResourcesRequest::new(
            user(TARGET_USER),
            relation("can_view"),
            "doc",
        ),
        lookup_subjects: LookupSubjectsRequest::new(
            object("doc", "tenant_000_doc_shared"),
            relation("can_view"),
            "user",
        ),
        lookup_permissions: LookupPermissionsRequest::new(
            user(EDITOR_USER),
            object("doc", "tenant_000_doc_shared"),
            Consistency::Latest,
        ),
        lookup_object_permissions: LookupObjectPermissionsRequest::new(
            object("doc", "tenant_000_doc_shared"),
            "user",
            Consistency::Latest,
        ),
        expand_shared_doc: ExpandRequest::new(
            object("doc", "tenant_000_doc_shared"),
            relation("can_view"),
            Consistency::Latest,
        ),
    };

    validate_scenario(&scenario);
    scenario
}

fn validate_scenario(scenario: &RealworldScenario) {
    ensure_check(
        scenario,
        &scenario.inherited_doc,
        &scenario.can_view,
        &scenario.target_user,
        true,
        "target user inherits doc view through workspace membership",
    );
    ensure_check(
        scenario,
        &scenario.direct_doc,
        &scenario.can_view,
        &scenario.direct_user,
        true,
        "direct user can view direct doc",
    );
    ensure_check(
        scenario,
        &scenario.denied_doc,
        &scenario.can_view,
        &scenario.blocked_user,
        false,
        "blocked user denied by doc ban",
    );
    ensure_check(
        scenario,
        &scenario.inherited_doc,
        &scenario.can_edit,
        &scenario.editor_user,
        true,
        "project editor inherits doc edit",
    );
    ensure_check(
        scenario,
        &scenario.shared_doc,
        &scenario.can_share,
        &scenario.editor_user,
        true,
        "doc editor can share",
    );
    ensure_check(
        scenario,
        &scenario.launch_project,
        &relation("can_view"),
        &user(AUDITOR_USER),
        true,
        "workspace auditor can view project",
    );
}

fn ensure_check(
    scenario: &RealworldScenario,
    object: &Object,
    relation: &Relation,
    user: &User,
    expected: bool,
    context: &str,
) {
    let actual = must(
        scenario.engine.check(CheckRequest::new(
            object.clone(),
            relation.clone(),
            user.clone(),
            Consistency::Latest,
        )),
        context,
    )
    .allowed;
    if actual != expected {
        abort(&format!("{context}: expected {expected}, got {actual}"));
    }
}

fn apply_relationships(engine: &ZanzibarEngine, relationships: usize) {
    let fixed = fixed_relationships();
    let generated = relationships.saturating_sub(fixed.len());
    let mut batch = Vec::with_capacity(MUTATION_BATCH_LIMIT);

    for relationship in fixed {
        push_mutation(engine, &mut batch, relationship);
    }

    for index in 0..generated {
        push_mutation(engine, &mut batch, generated_relationship(index));
    }

    flush_batch(engine, &mut batch);
}

fn push_mutation(
    engine: &ZanzibarEngine,
    batch: &mut Vec<RelationshipMutation>,
    relationship: String,
) {
    batch.push(must(
        RelationshipMutation::touch(relationship),
        "failed to parse benchmark relationship",
    ));
    if batch.len() == MUTATION_BATCH_LIMIT {
        flush_batch(engine, batch);
    }
}

fn flush_batch(engine: &ZanzibarEngine, batch: &mut Vec<RelationshipMutation>) {
    if batch.is_empty() {
        return;
    }
    let mutations = std::mem::take(batch);
    must(
        engine.write_relationships(mutations),
        "failed to apply benchmark relationships",
    );
}

fn fixed_relationships() -> Vec<String> {
    [
        format!("group:tenant_000_members#member@user:{TARGET_USER}"),
        format!("workspace:tenant_000#owner@user:{OWNER_USER}"),
        "workspace:tenant_000#member@group:tenant_000_members#member".to_string(),
        format!("workspace:tenant_000#auditor@user:{AUDITOR_USER}"),
        "project:tenant_000_project_000#parent@workspace:tenant_000#viewer".to_string(),
        format!("project:tenant_000_project_000#editor@user:{EDITOR_USER}"),
        "folder:tenant_000_folder_000#parent@project:tenant_000_project_000#can_view".to_string(),
        format!("folder:tenant_000_folder_000#editor@user:{EDITOR_USER}"),
        "doc:tenant_000_doc_inherited#parent@folder:tenant_000_folder_000#can_view".to_string(),
        format!("doc:tenant_000_doc_direct#viewer@user:{DIRECT_USER}"),
        "doc:tenant_000_doc_denied#parent@folder:tenant_000_folder_000#can_view".to_string(),
        format!("doc:tenant_000_doc_denied#banned@user:{BLOCKED_USER}"),
        format!("doc:tenant_000_doc_shared#editor@user:{EDITOR_USER}"),
        "doc:tenant_000_doc_shared#viewer@group:tenant_000_members#member".to_string(),
    ]
    .into_iter()
    .collect()
}

fn generated_relationship(index: usize) -> String {
    let cohort = index / 10;
    let tenant = cohort % 128;
    let team = cohort % 20_000;
    let project = cohort % 50_000;
    let folder = cohort % 100_000;
    let target_team = if cohort.is_multiple_of(100) { 0 } else { team };

    match index % 10 {
        0 => format!("group:tenant_{tenant:03}_team_{team:05}#member@user:user_{cohort:07}"),
        1 => format!(
            "workspace:tenant_{tenant:03}#member@group:tenant_{tenant:03}_team_{team:05}#member",
        ),
        2 => format!("workspace:tenant_{tenant:03}#auditor@user:auditor_{cohort:07}"),
        3 => format!(
            "project:tenant_{tenant:03}_project_{project:05}#parent@workspace:tenant_{tenant:03}#\
             viewer",
        ),
        4 => format!(
            "project:tenant_{tenant:03}_project_{project:05}#editor@group:tenant_{tenant:\
             03}_team_{team:05}#member",
        ),
        5 => format!(
            "folder:tenant_{tenant:03}_folder_{folder:06}#parent@project:tenant_{tenant:\
             03}_project_{project:05}#can_view",
        ),
        6 => format!("folder:tenant_{tenant:03}_folder_{folder:06}#editor@user:editor_{cohort:07}"),
        7 => format!(
            "doc:tenant_{tenant:03}_doc_{cohort:07}#parent@folder:tenant_{tenant:\
             03}_folder_{folder:06}#can_view",
        ),
        8 => format!(
            "doc:tenant_{tenant:03}_doc_{cohort:07}#viewer@group:tenant_{tenant:\
             03}_team_{target_team:05}#member",
        ),
        _ => format!("doc:tenant_{tenant:03}_doc_{cohort:07}#banned@user:blocked_{cohort:07}"),
    }
}

fn prepared_snapshot(scenario: &RealworldScenario, scale: WorkloadScale) -> PathBuf {
    let path = env::temp_dir().join(format!(
        "simple_zanzibar_realworld_{}_{}_{}.szsnap",
        scale.label,
        process::id(),
        unique_suffix(),
    ));
    must(
        scenario
            .engine
            .save_snapshot(&path, SnapshotSaveOptions::default()),
        "failed to save realworld snapshot",
    );
    path
}

fn trusted_fast_load_options() -> SnapshotLoadOptions {
    SnapshotLoadOptions {
        validation: SnapshotValidationMode::TrustedFastLoad,
        integrity: SnapshotIntegrityMode::External,
        ..SnapshotLoadOptions::default()
    }
}

const REALWORLD_SCHEMA: &str = r#"
    namespace group {
        relation member {}
    }

    namespace workspace {
        relation owner {}
        relation member {}
        relation auditor {}

        relation admin {
            rewrite computed_userset(relation: "owner")
        }

        relation viewer {
            rewrite union(
                computed_userset(relation: "owner"),
                computed_userset(relation: "member"),
                computed_userset(relation: "auditor")
            )
        }

        relation editor {
            rewrite union(
                computed_userset(relation: "owner"),
                computed_userset(relation: "member")
            )
        }
    }

    namespace project {
        relation parent {}
        relation owner {}
        relation editor {}
        relation viewer {}
        relation banned {}

        relation can_view {
            rewrite exclusion(
                union(
                    computed_userset(relation: "owner"),
                    computed_userset(relation: "editor"),
                    computed_userset(relation: "viewer"),
                    tuple_to_userset(tupleset: "parent", computed_userset: "viewer")
                ),
                computed_userset(relation: "banned")
            )
        }

        relation can_edit {
            rewrite exclusion(
                union(
                    computed_userset(relation: "owner"),
                    computed_userset(relation: "editor"),
                    tuple_to_userset(tupleset: "parent", computed_userset: "editor")
                ),
                computed_userset(relation: "banned")
            )
        }

        relation can_admin {
            rewrite computed_userset(relation: "owner")
        }
    }

    namespace folder {
        relation parent {}
        relation owner {}
        relation editor {}
        relation viewer {}
        relation banned {}

        relation can_view {
            rewrite exclusion(
                union(
                    computed_userset(relation: "owner"),
                    computed_userset(relation: "editor"),
                    computed_userset(relation: "viewer"),
                    tuple_to_userset(tupleset: "parent", computed_userset: "can_view")
                ),
                computed_userset(relation: "banned")
            )
        }

        relation can_edit {
            rewrite exclusion(
                union(
                    computed_userset(relation: "owner"),
                    computed_userset(relation: "editor"),
                    tuple_to_userset(tupleset: "parent", computed_userset: "can_edit")
                ),
                computed_userset(relation: "banned")
            )
        }
    }

    namespace doc {
        relation parent {}
        relation owner {}
        relation editor {}
        relation viewer {}
        relation banned {}

        relation can_view {
            rewrite exclusion(
                union(
                    computed_userset(relation: "owner"),
                    computed_userset(relation: "editor"),
                    computed_userset(relation: "viewer"),
                    tuple_to_userset(tupleset: "parent", computed_userset: "can_view")
                ),
                computed_userset(relation: "banned")
            )
        }

        relation can_edit {
            rewrite exclusion(
                union(
                    computed_userset(relation: "owner"),
                    computed_userset(relation: "editor"),
                    tuple_to_userset(tupleset: "parent", computed_userset: "can_edit")
                ),
                computed_userset(relation: "banned")
            )
        }

        relation can_share {
            rewrite union(
                computed_userset(relation: "owner"),
                computed_userset(relation: "editor")
            )
        }
    }
    "#;

fn evaluation_limits() -> EvaluationLimits {
    EvaluationLimits {
        max_depth: non_zero_u32(50),
        max_fanout_per_step: non_zero_u32(100_000),
        max_lookup_results: non_zero_u32(1_000),
    }
}

fn object(namespace: &str, id: &str) -> Object {
    Object::new(namespace, id)
}

fn relation(name: &str) -> Relation {
    Relation::new(name)
}

fn user(id: &str) -> User {
    User::user_id(id)
}

fn should_build_scale(scale: WorkloadScale, filters: &[String]) -> bool {
    let prefix = format!("realworld_authorization/{}_rules", scale.label);
    filters.is_empty()
        || filters
            .iter()
            .any(|filter| prefix.contains(filter) || filter.contains(scale.label))
}

fn benchmark_filters() -> Vec<String> {
    let mut filters = Vec::new();
    let mut skip_next = false;

    for argument in env::args().skip(1) {
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

fn unique_suffix() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT_SUFFIX: AtomicU64 = AtomicU64::new(1);
    NEXT_SUFFIX.fetch_add(1, Ordering::Relaxed)
}

fn remove_file(path: &Path) {
    let _ = fs::remove_file(path);
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
        Err(error) => abort(&format!("{context}: {error}")),
    }
}

fn abort(message: &str) -> ! {
    eprintln!("{message}");
    process::abort();
}
