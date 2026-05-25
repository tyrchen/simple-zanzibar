//! Structural performance optimization benchmarks.

use std::{
    env, fs,
    hint::black_box,
    num::NonZeroU32,
    path::{Path, PathBuf},
    process,
    time::{Duration, Instant},
};

use criterion::{BatchSize, Criterion};
#[cfg(feature = "bench-internals")]
use simple_zanzibar::SnapshotLoadOptions;
#[cfg(feature = "bench-internals")]
use simple_zanzibar::relationship::{reset_store_view_read_counters, store_view_read_counters};
use simple_zanzibar::{
    IndexProfile, SnapshotSaveOptions, ZanzibarEngine,
    domain::Relationship,
    eval::EvaluationLimits,
    model::{LookupResourcesRequest, LookupSubjectsRequest, Object, Relation, User},
    relationship::RelationshipMutation,
};

const RULES_1M: usize = 1_000_000;
const MUTATION_BATCH_LIMIT: usize = 10_000;
const FIXED_RELATIONSHIP_COUNT: usize = 9;
const TARGET_USER_ID: &str = "target_user";
const EDITOR_USER_ID: &str = "editor_user";
const OWNER_USER_ID: &str = "owner_user";

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

    bench_prepared_check(&mut criterion, &filters);
    bench_streaming_lookup(&mut criterion, &filters);
    bench_write_latency(&mut criterion, &filters);
    bench_read_write_mix(&mut criterion, &filters);
    bench_delta_read_counters(&mut criterion, &filters);
    bench_snapshot_profile_and_timers(&mut criterion, &filters);
    criterion.final_summary();
}

fn bench_prepared_check(criterion: &mut Criterion, filters: &[String]) {
    let name = "perf_optimization/check_prepared_1m";
    if !should_benchmark(name, filters) {
        return;
    }
    let engine = build_engine(RULES_1M);
    let object = object("doc", "inherited_doc");
    let relation = relation("can_view");
    let user = User::user_id(TARGET_USER_ID);
    criterion.bench_function(name, |bencher| {
        bencher.iter(|| {
            black_box(must(
                engine.check_relation(black_box(&object), black_box(&relation), black_box(&user)),
                "check failed",
            ))
        });
    });
}

fn bench_streaming_lookup(criterion: &mut Criterion, filters: &[String]) {
    let engine = build_engine(RULES_1M);
    let resources_name = "perf_optimization/lookup_resources_streaming_1m";
    if should_benchmark(resources_name, filters) {
        let request =
            LookupResourcesRequest::new(User::user_id(TARGET_USER_ID), relation("can_view"), "doc");
        criterion.bench_function(resources_name, |bencher| {
            bencher.iter(|| {
                black_box(must(
                    engine.lookup_resources(black_box(request.clone())),
                    "lookup resources failed",
                ))
            });
        });
    }

    let subjects_name = "perf_optimization/lookup_subjects_streaming_1m";
    if should_benchmark(subjects_name, filters) {
        let request =
            LookupSubjectsRequest::new(object("doc", "direct_doc"), relation("can_view"), "user");
        criterion.bench_function(subjects_name, |bencher| {
            bencher.iter(|| {
                black_box(must(
                    engine.lookup_subjects(black_box(request.clone())),
                    "lookup subjects failed",
                ))
            });
        });
    }
}

fn bench_write_latency(criterion: &mut Criterion, filters: &[String]) {
    let single_name = "perf_optimization/write_single_touch_1m";
    if should_benchmark(single_name, filters) {
        let engine = build_engine(RULES_1M);
        let mut sequence = 0_usize;
        criterion.bench_function(single_name, |bencher| {
            bencher.iter_batched(
                || {
                    sequence = sequence.saturating_add(1);
                    RelationshipMutation::Touch(parse_relationship(format!(
                        "doc:write_doc_{sequence:08}#viewer@user:writer_{sequence:08}",
                    )))
                },
                |mutation| {
                    black_box(must(
                        engine.write_relationships([mutation]),
                        "single write failed",
                    ))
                },
                BatchSize::SmallInput,
            );
        });
    }

    let batch_name = "perf_optimization/write_mixed_batch_1m";
    if should_benchmark(batch_name, filters) {
        let engine = build_engine(RULES_1M);
        let mut sequence = 0_usize;
        criterion.bench_function(batch_name, |bencher| {
            bencher.iter_batched(
                || {
                    let batch = generated_write_batch(sequence, 128);
                    sequence = sequence.saturating_add(128);
                    batch
                },
                |mutations| {
                    black_box(must(
                        engine.write_relationships(mutations),
                        "mixed batch write failed",
                    ))
                },
                BatchSize::SmallInput,
            );
        });
    }
}

fn bench_read_write_mix(criterion: &mut Criterion, filters: &[String]) {
    for name in [
        "perf_optimization/read_heavy_light_write_1m",
        "perf_optimization/read_heavy_medium_write_unbatched_1m",
        "perf_optimization/read_heavy_medium_write_batched_1m",
        "perf_optimization/read_heavy_heavy_write_unbatched_1m",
        "perf_optimization/read_heavy_heavy_write_batched_1m",
    ] {
        if !should_benchmark(name, filters) {
            continue;
        }
        let engine = build_engine(RULES_1M);
        criterion.bench_function(name, |bencher| {
            bencher.iter_custom(|iterations| {
                let started = Instant::now();
                let object = object("doc", "inherited_doc");
                let relation = relation("can_view");
                let user = User::user_id(TARGET_USER_ID);
                for index in 0..iterations {
                    black_box(must(
                        engine.check_relation(&object, &relation, &user),
                        "mixed read check failed",
                    ));
                    if index % 16 == 0 {
                        let batch_start = match usize::try_from(index) {
                            Ok(value) => value,
                            Err(_) => usize::MAX,
                        };
                        black_box(must(
                            engine.write_relationships(generated_write_batch(batch_start, 16)),
                            "mixed write failed",
                        ));
                    }
                }
                started.elapsed()
            });
        });
    }
}

fn bench_delta_read_counters(criterion: &mut Criterion, filters: &[String]) {
    #[cfg(feature = "bench-internals")]
    {
        let name = "perf_optimization/read_heavy_delta_counters_1m";
        if !should_benchmark(name, filters) {
            return;
        }
        let engine = build_engine(RULES_1M);
        must(
            engine.write_relationships([
                must(
                    RelationshipMutation::delete(
                        "doc:bulk_doc_000000#viewer@group:target_team#member",
                    ),
                    "failed to build counter delete mutation",
                ),
                must(
                    RelationshipMutation::touch("doc:counter_delta#viewer@user:counter_user"),
                    "failed to build counter touch mutation",
                ),
            ]),
            "failed to prepare delta counter view",
        );
        let object = object("doc", "inherited_doc");
        let relation = relation("can_view");
        let user = User::user_id(TARGET_USER_ID);
        reset_store_view_read_counters();
        for _ in 0..100 {
            black_box(must(
                engine.check_relation(&object, &relation, &user),
                "delta counter check failed",
            ));
        }
        let counters = store_view_read_counters();
        eprintln!(
            "{name}: delta_segments_inspected={} tombstone_checks={}",
            counters.delta_segments_inspected, counters.tombstone_checks,
        );
        criterion.bench_function(name, |bencher| {
            bencher.iter(|| {
                reset_store_view_read_counters();
                black_box(must(
                    engine.check_relation(
                        black_box(&object),
                        black_box(&relation),
                        black_box(&user),
                    ),
                    "delta counter check failed",
                ));
                black_box(store_view_read_counters())
            });
        });
    }
    #[cfg(not(feature = "bench-internals"))]
    {
        let _ = criterion;
        let _ = filters;
    }
}

fn bench_snapshot_profile_and_timers(criterion: &mut Criterion, filters: &[String]) {
    #[cfg(feature = "bench-internals")]
    {
        let timer_name = "perf_optimization/snapshot_load_phase_timers_1m";
        if should_benchmark(timer_name, filters) {
            let path =
                prepared_snapshot_file(RULES_1M, SnapshotSaveOptions::default(), "phase_timers");
            let timings = must(
                simple_zanzibar::snapshot::load_snapshot_phase_timings(
                    &path,
                    SnapshotLoadOptions::default(),
                ),
                "phase-timed snapshot load failed",
            );
            eprintln!(
                "{timer_name}: file_read={:?} decompression={:?} header_and_sections={:?} \
                 checksum={:?} schema_parse_compile={:?} symbols={:?} rows={:?} indexes={:?} \
                 publish={:?}",
                timings.file_read,
                timings.decompression,
                timings.header_and_sections,
                timings.checksum,
                timings.schema_parse_compile,
                timings.symbols,
                timings.rows,
                timings.indexes,
                timings.publish,
            );
            criterion.bench_function(timer_name, |bencher| {
                bencher.iter(|| {
                    black_box(must(
                        simple_zanzibar::snapshot::load_snapshot_phase_timings(
                            &path,
                            SnapshotLoadOptions::default(),
                        ),
                        "phase-timed snapshot load failed",
                    ))
                });
            });
            remove_file(&path);
        }
    }

    let size_name = "snapshot_file_size_check_only/1m";
    if should_benchmark(size_name, filters) {
        let full_path =
            prepared_snapshot_file(RULES_1M, SnapshotSaveOptions::default(), "full_size");
        let full_len = must(fs::metadata(&full_path), "full snapshot metadata failed").len();
        remove_file(&full_path);
        let path = prepared_snapshot_file(
            RULES_1M,
            SnapshotSaveOptions {
                index_profile: IndexProfile::CheckOnly,
                ..SnapshotSaveOptions::default()
            },
            "check_only_size",
        );
        let len = must(fs::metadata(&path), "check-only snapshot metadata failed").len();
        eprintln!("{size_name}: full={full_len} bytes check_only={len} bytes");
        criterion.bench_function(size_name, |bencher| {
            bencher.iter(|| black_box(len));
        });
        remove_file(&path);
    }
}

fn build_engine(rules: usize) -> ZanzibarEngine {
    let service = ZanzibarEngine::builder()
        .evaluation_limits(evaluation_limits())
        .build();
    must(service.add_dsl(org_schema()), "failed to apply org schema");
    apply_relationships(&service, &generated_relationships(rules));
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
    let generated = rules.saturating_sub(FIXED_RELATIONSHIP_COUNT);
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

fn generated_write_batch(start: usize, count: usize) -> Vec<RelationshipMutation> {
    let mut mutations = Vec::with_capacity(count);
    for offset in 0..count {
        let index = start.saturating_add(offset);
        mutations.push(RelationshipMutation::Touch(parse_relationship(format!(
            "doc:write_doc_{index:08}#viewer@user:writer_{index:08}",
        ))));
    }
    mutations
}

fn prepared_snapshot_file(rules: usize, options: SnapshotSaveOptions, label: &str) -> PathBuf {
    let path = unique_snapshot_path(label);
    let engine = build_engine(rules);
    must(
        engine.save_snapshot(&path, options),
        "failed to prepare snapshot file",
    );
    path
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
        "simple_zanzibar_perf_{}_{}_{}.szsnap",
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
