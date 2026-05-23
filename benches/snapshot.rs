//! Benchmarks for compact snapshot build, save, load, file size, and load RSS harnesses.

use std::{
    env, fs,
    hint::black_box,
    num::NonZeroU32,
    path::{Path, PathBuf},
    process,
    time::Duration,
};

use criterion::{BatchSize, Criterion};
use simple_zanzibar::{
    SnapshotIntegrityMode, SnapshotLoadOptions, SnapshotLoadProfile, SnapshotSaveOptions,
    SnapshotValidationMode, ZanzibarEngine,
    domain::Relationship,
    eval::EvaluationLimits,
    model::{LookupResourcesRequest, Object, Relation, User},
    relationship::RelationshipMutation,
};

const ORG_RULE_SIZES: [usize; 3] = [1_000, 100_000, 1_000_000];
const MUTATION_BATCH_LIMIT: usize = 10_000;
const FIXED_RELATIONSHIP_COUNT: usize = 9;
const TARGET_USER_ID: &str = "target_user";
const EDITOR_USER_ID: &str = "editor_user";
const OWNER_USER_ID: &str = "owner_user";
const PREPARE_PATH_ENV: &str = "SZS_SNAPSHOT_PREPARE_PATH";
const LOAD_PATH_ENV: &str = "SZS_SNAPSHOT_LOAD_PATH";
const RSS_ONCE_ENV: &str = "SZS_SNAPSHOT_RSS_ONCE";

fn main() {
    if cfg!(debug_assertions) {
        return;
    }

    if let Ok(path) = env::var(PREPARE_PATH_ENV) {
        let service = build_service_with_relationships(1_000_000);
        must(
            service.save_snapshot(Path::new(&path), SnapshotSaveOptions::default()),
            "failed to prepare snapshot benchmark file",
        );
        return;
    }

    if env::var(RSS_ONCE_ENV).ok().as_deref() == Some("1")
        && let Ok(path) = env::var(LOAD_PATH_ENV)
    {
        let service = must(
            ZanzibarEngine::load_snapshot(path, SnapshotLoadOptions::default()),
            "failed to load snapshot once for RSS measurement",
        );
        black_box(service);
        eprintln!("snapshot_load_peak_rss/1m: single load completed");
        return;
    }

    let filters = benchmark_filters();
    let mut criterion = Criterion::default()
        .sample_size(10)
        .warm_up_time(Duration::from_millis(100))
        .measurement_time(Duration::from_millis(500))
        .configure_from_args();

    bench_snapshot_build(&mut criterion, &filters);
    bench_snapshot_save(&mut criterion, &filters);
    bench_snapshot_load(&mut criterion, &filters);
    bench_snapshot_loaded_queries(&mut criterion, &filters);
    bench_snapshot_file_size(&mut criterion, &filters);
    criterion.final_summary();
}

fn bench_snapshot_build(criterion: &mut Criterion, filters: &[String]) {
    for rules in ORG_RULE_SIZES {
        let name = format!("snapshot_build_from_relationships/{}", rule_label(rules));
        if !should_benchmark(&name, filters) {
            continue;
        }
        let relationships = generated_relationships(rules);
        criterion.bench_function(&name, |bencher| {
            bencher.iter_batched(
                configured_service,
                |service| {
                    apply_relationships(&service, &relationships);
                    black_box(service)
                },
                BatchSize::LargeInput,
            );
        });
    }
}

fn bench_snapshot_save(criterion: &mut Criterion, filters: &[String]) {
    let name = "snapshot_save_uncompressed/1m";
    if !should_benchmark(name, filters) {
        return;
    }
    let service = build_service_with_relationships(1_000_000);
    criterion.bench_function(name, |bencher| {
        bencher.iter_batched(
            || unique_snapshot_path("save_1m"),
            |path| {
                must(
                    service.save_snapshot(&path, SnapshotSaveOptions::default()),
                    "failed to save snapshot",
                );
                let metadata = must(fs::metadata(&path), "failed to stat saved snapshot");
                remove_file(&path);
                black_box(metadata.len())
            },
            BatchSize::LargeInput,
        );
    });
}

fn bench_snapshot_load(criterion: &mut Criterion, filters: &[String]) {
    for rules in ORG_RULE_SIZES {
        let name = format!("snapshot_load_compact/{}", rule_label(rules));
        if should_benchmark(&name, filters) {
            let path = prepared_snapshot_file(rules, &name);
            criterion.bench_function(&name, |bencher| {
                bencher.iter(|| {
                    black_box(must(
                        ZanzibarEngine::load_snapshot(&path, SnapshotLoadOptions::default()),
                        "failed to load compact snapshot",
                    ))
                });
            });
            remove_file_if_owned(&path);
        }
    }

    let trusted_name = "snapshot_load_trusted_fast/1m";
    if should_benchmark(trusted_name, filters) {
        let path = prepared_snapshot_file(1_000_000, trusted_name);
        criterion.bench_function(trusted_name, |bencher| {
            bencher.iter(|| {
                black_box(must(
                    ZanzibarEngine::load_snapshot(&path, trusted_fast_load_options()),
                    "failed to trusted-load compact snapshot",
                ))
            });
        });
        remove_file_if_owned(&path);
    }

    let reindex_name = "snapshot_load_and_reindex/1m";
    if should_benchmark(reindex_name, filters) {
        let path = prepared_snapshot_file(1_000_000, reindex_name);
        criterion.bench_function(reindex_name, |bencher| {
            bencher.iter(|| {
                black_box(must(
                    ZanzibarEngine::load_snapshot(
                        &path,
                        SnapshotLoadOptions {
                            profile: SnapshotLoadProfile::Latency,
                            ..SnapshotLoadOptions::default()
                        },
                    ),
                    "failed to load and reindex snapshot",
                ))
            });
        });
        remove_file_if_owned(&path);
    }

    let rss_name = "snapshot_load_peak_rss/1m";
    if should_benchmark(rss_name, filters) {
        let path = prepared_snapshot_file(1_000_000, rss_name);
        criterion.bench_function(rss_name, |bencher| {
            bencher.iter(|| {
                black_box(must(
                    ZanzibarEngine::load_snapshot(&path, SnapshotLoadOptions::default()),
                    "failed to load compact snapshot for RSS",
                ))
            });
        });
        remove_file_if_owned(&path);
    }
}

fn bench_snapshot_loaded_queries(criterion: &mut Criterion, filters: &[String]) {
    let full = LoadedQueryBenchNames {
        direct: "snapshot_loaded_check_direct/1m",
        inherited: "snapshot_loaded_check_inherited/1m",
        lookup: "snapshot_loaded_lookup_resources/1m",
    };
    let trusted = LoadedQueryBenchNames {
        direct: "snapshot_trusted_loaded_check_direct/1m",
        inherited: "snapshot_trusted_loaded_check_inherited/1m",
        lookup: "snapshot_trusted_loaded_lookup_resources/1m",
    };
    let full_requested = loaded_query_requested(full, filters);
    let trusted_requested = loaded_query_requested(trusted, filters);
    if !full_requested && !trusted_requested {
        return;
    }

    let path = prepared_snapshot_file(1_000_000, "snapshot_loaded_queries/1m");
    if full_requested {
        let service = must(
            ZanzibarEngine::load_snapshot(&path, SnapshotLoadOptions::default()),
            "failed to load snapshot for loaded-query benchmarks",
        );
        bench_loaded_query_set(criterion, filters, &service, full);
    }
    if trusted_requested {
        let service = must(
            ZanzibarEngine::load_snapshot(&path, trusted_fast_load_options()),
            "failed to trusted-load snapshot for loaded-query benchmarks",
        );
        bench_loaded_query_set(criterion, filters, &service, trusted);
    }
    remove_file_if_owned(&path);
}

#[derive(Debug, Clone, Copy)]
struct LoadedQueryBenchNames {
    direct: &'static str,
    inherited: &'static str,
    lookup: &'static str,
}

fn loaded_query_requested(names: LoadedQueryBenchNames, filters: &[String]) -> bool {
    [names.direct, names.inherited, names.lookup]
        .iter()
        .any(|name| should_benchmark(name, filters))
}

fn bench_loaded_query_set(
    criterion: &mut Criterion,
    filters: &[String],
    service: &ZanzibarEngine,
    names: LoadedQueryBenchNames,
) {
    if !loaded_query_requested(names, filters) {
        return;
    }

    let target_user = User::UserId(TARGET_USER_ID.to_string());
    let can_view = relation("can_view");
    let direct_doc = object("doc", "direct_doc");
    let inherited_doc = object("doc", "inherited_doc");
    let lookup_request = LookupResourcesRequest {
        subject: target_user.clone(),
        permission: can_view.clone(),
        resource_type: "doc".to_string(),
    };

    if should_benchmark(names.direct, filters) {
        criterion.bench_function(names.direct, |bencher| {
            bencher.iter(|| {
                black_box(must(
                    service.check_relation(
                        black_box(&direct_doc),
                        black_box(&can_view),
                        black_box(&target_user),
                    ),
                    "loaded direct check failed",
                ))
            });
        });
    }

    if should_benchmark(names.inherited, filters) {
        criterion.bench_function(names.inherited, |bencher| {
            bencher.iter(|| {
                black_box(must(
                    service.check_relation(
                        black_box(&inherited_doc),
                        black_box(&can_view),
                        black_box(&target_user),
                    ),
                    "loaded inherited check failed",
                ))
            });
        });
    }

    if should_benchmark(names.lookup, filters) {
        criterion.bench_function(names.lookup, |bencher| {
            bencher.iter(|| {
                black_box(must(
                    service.lookup_resources(black_box(&lookup_request)),
                    "loaded lookup resources failed",
                ))
            });
        });
    }
}

fn bench_snapshot_file_size(criterion: &mut Criterion, filters: &[String]) {
    let name = "snapshot_file_size/1m";
    if !should_benchmark(name, filters) {
        return;
    }
    let path = prepared_snapshot_file(1_000_000, name);
    let len = must(fs::metadata(&path), "failed to stat snapshot file").len();
    eprintln!("{name}: {len} bytes");
    criterion.bench_function(name, |bencher| {
        bencher.iter(|| black_box(len));
    });
    remove_file_if_owned(&path);
}

fn prepared_snapshot_file(rules: usize, benchmark_name: &str) -> PathBuf {
    if benchmark_name == "snapshot_load_peak_rss/1m"
        && let Ok(path) = env::var(LOAD_PATH_ENV)
    {
        return PathBuf::from(path);
    }

    let path = unique_snapshot_path(rule_label(rules));
    let service = build_service_with_relationships(rules);
    must(
        service.save_snapshot(&path, SnapshotSaveOptions::default()),
        "failed to prepare snapshot file",
    );
    path
}

fn remove_file_if_owned(path: &Path) {
    if env::var(LOAD_PATH_ENV)
        .ok()
        .is_some_and(|external| Path::new(&external) == path)
    {
        return;
    }
    remove_file(path);
}

fn trusted_fast_load_options() -> SnapshotLoadOptions {
    SnapshotLoadOptions {
        validation: SnapshotValidationMode::TrustedFastLoad,
        integrity: SnapshotIntegrityMode::External,
        ..SnapshotLoadOptions::default()
    }
}

fn build_service_with_relationships(rules: usize) -> ZanzibarEngine {
    let service = configured_service();
    let relationships = generated_relationships(rules);
    apply_relationships(&service, &relationships);
    service
}

fn configured_service() -> ZanzibarEngine {
    let service = ZanzibarEngine::builder()
        .evaluation_limits(evaluation_limits())
        .build();
    must(service.add_dsl(org_schema()), "failed to apply org schema");
    service
}

fn evaluation_limits() -> EvaluationLimits {
    EvaluationLimits {
        max_depth: non_zero_u32(50),
        max_fanout_per_step: non_zero_u32(100_000),
        max_lookup_results: non_zero_u32(1_000),
    }
}

fn generated_relationships(rules: usize) -> Vec<Relationship> {
    let mut relationships = Vec::with_capacity(rules);
    relationships.extend(fixed_relationships());
    let generated = rules.saturating_sub(fixed_relationship_count());
    for index in 0..generated {
        relationships.push(generated_relationship(index));
    }
    relationships
}

fn apply_relationships(service: &ZanzibarEngine, relationships: &[Relationship]) {
    let mut batch = Vec::with_capacity(MUTATION_BATCH_LIMIT);
    for relationship in relationships {
        batch.push(RelationshipMutation::Touch(relationship.clone()));
        if batch.len() == MUTATION_BATCH_LIMIT {
            flush_relationships(service, &mut batch);
        }
    }
    flush_relationships(service, &mut batch);
}

fn flush_relationships(service: &ZanzibarEngine, batch: &mut Vec<RelationshipMutation>) {
    if batch.is_empty() {
        return;
    }
    let mutations = std::mem::take(batch);
    must(
        service.write_relationships_with_preconditions(mutations, []),
        "failed to apply relationship batch",
    );
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

fn relation(name: &str) -> Relation {
    Relation(name.to_string())
}

fn unique_snapshot_path(label: &str) -> PathBuf {
    env::temp_dir().join(format!(
        "simple_zanzibar_snapshot_bench_{}_{}_{}.szsnap",
        label,
        process::id(),
        unique_suffix(),
    ))
}

fn unique_suffix() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT_SUFFIX: AtomicU64 = AtomicU64::new(1);
    NEXT_SUFFIX.fetch_add(1, Ordering::Relaxed)
}

fn remove_file(path: &Path) {
    let _ = fs::remove_file(path);
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

fn rule_label(rules: usize) -> &'static str {
    match rules {
        1_000 => "1k",
        100_000 => "100k",
        1_000_000 => "1m",
        _ => "custom",
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
