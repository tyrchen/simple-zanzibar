//! Structural performance optimization benchmarks.

use std::{
    env, fs,
    hint::black_box,
    num::NonZeroU32,
    path::{Path, PathBuf},
    process,
    time::{Duration, Instant},
};

#[cfg(feature = "bench-internals")]
#[path = "common/phase15_fixtures.rs"]
mod phase15_fixtures;

use criterion::{BatchSize, Criterion};
#[cfg(feature = "bench-internals")]
use phase15_fixtures::{
    PHASE15_HIGH_FANOUT, PHASE15_LOOKUP_SUBJECTS, PHASE15_TARGET_USER_ID, PHASE15_TARGETED_RULES,
    build_phase15_high_fanout_engine, build_phase15_lookup_subjects_engine,
    build_phase15_shared_parent_engine,
};
#[cfg(feature = "bench-internals")]
use simple_zanzibar::SnapshotLoadOptions;
#[cfg(feature = "bench-internals")]
use simple_zanzibar::eval::{evaluation_read_counters, reset_evaluation_read_counters};
#[cfg(feature = "bench-internals")]
use simple_zanzibar::relationship::{
    StorePostingHistograms, reset_store_view_read_counters, store_view_read_counters,
};
#[cfg(feature = "bench-internals")]
use simple_zanzibar::revision::Consistency;
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
#[cfg(feature = "bench-internals")]
const PHASE15_DELETE_TOMBSTONES: usize = 4_096;
#[cfg(feature = "bench-internals")]
const PHASE15_PRUNING_CANDIDATES: usize = 50_000;
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
    bench_phase15_measurement_baseline(&mut criterion, &filters);
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

fn bench_phase15_measurement_baseline(criterion: &mut Criterion, filters: &[String]) {
    #[cfg(feature = "bench-internals")]
    {
        bench_phase15_memo_shared_parent(criterion, filters);
        bench_phase15_lookup_subjects_allocation(criterion, filters);
        bench_phase15_delete_heavy_delta(criterion, filters);
        bench_phase15_high_fanout_posting(criterion, filters);
        bench_phase15_lookup_planner_pruning(criterion, filters);
    }
    #[cfg(not(feature = "bench-internals"))]
    {
        let _ = criterion;
        let _ = filters;
    }
}

#[cfg(feature = "bench-internals")]
fn bench_phase15_memo_shared_parent(criterion: &mut Criterion, filters: &[String]) {
    let name = "perf_optimization/phase15_memo_shared_parent";
    if !should_benchmark(name, filters) {
        return;
    }
    let engine = build_phase15_shared_parent_engine(PHASE15_TARGETED_RULES);
    let request = LookupResourcesRequest::new(
        User::user_id(PHASE15_TARGET_USER_ID),
        relation("can_view"),
        "doc",
    );
    print_phase15_eval_counter_sample(name, || {
        black_box(must(
            engine.lookup_resources(request.clone()),
            "phase15 memo fixture lookup failed",
        ));
    });
    criterion.bench_function(name, |bencher| {
        bencher.iter(|| {
            reset_evaluation_read_counters();
            let resources = must(
                engine.lookup_resources(black_box(request.clone())),
                "phase15 memo fixture lookup failed",
            );
            black_box(evaluation_read_counters());
            black_box(resources)
        });
    });
}

#[cfg(feature = "bench-internals")]
fn bench_phase15_lookup_subjects_allocation(criterion: &mut Criterion, filters: &[String]) {
    let name = "perf_optimization/phase15_lookup_subjects_allocation";
    if !should_benchmark(name, filters) {
        return;
    }
    let engine = build_phase15_lookup_subjects_engine(PHASE15_LOOKUP_SUBJECTS);
    let request =
        LookupSubjectsRequest::new(object("doc", "allocation"), relation("can_view"), "user");
    print_phase15_eval_counter_sample(name, || {
        black_box(must(
            engine.lookup_subjects(request.clone()),
            "phase15 lookup-subjects fixture failed",
        ));
    });
    criterion.bench_function(name, |bencher| {
        bencher.iter(|| {
            reset_evaluation_read_counters();
            let subjects = must(
                engine.lookup_subjects(black_box(request.clone())),
                "phase15 lookup-subjects fixture failed",
            );
            black_box(evaluation_read_counters());
            black_box(subjects)
        });
    });
}

#[cfg(feature = "bench-internals")]
fn bench_phase15_delete_heavy_delta(criterion: &mut Criterion, filters: &[String]) {
    let name = "perf_optimization/phase15_delete_heavy_delta";
    if !should_benchmark(name, filters) {
        return;
    }
    let engine = build_phase15_delete_heavy_engine(PHASE15_DELETE_TOMBSTONES);
    let object = object("doc", "delete_heavy");
    let relation = relation("viewer");
    let user = User::user_id(PHASE15_TARGET_USER_ID);
    print_phase15_store_counter_sample(name, &engine, || {
        black_box(must(
            engine.check_relation(&object, &relation, &user),
            "phase15 delete-heavy check failed",
        ));
    });
    criterion.bench_function(name, |bencher| {
        bencher.iter(|| {
            reset_store_view_read_counters();
            let allowed = must(
                engine.check_relation(black_box(&object), black_box(&relation), black_box(&user)),
                "phase15 delete-heavy check failed",
            );
            black_box(store_view_read_counters());
            black_box(allowed)
        });
    });
}

#[cfg(feature = "bench-internals")]
fn bench_phase15_high_fanout_posting(criterion: &mut Criterion, filters: &[String]) {
    let name = "perf_optimization/phase15_high_fanout_posting";
    if !should_benchmark(name, filters) {
        return;
    }
    let engine = build_phase15_high_fanout_engine(PHASE15_HIGH_FANOUT);
    let request =
        LookupSubjectsRequest::new(object("doc", "high_fanout"), relation("viewer"), "user");
    let histograms = must(
        engine.bench_relationship_posting_histograms(Consistency::Latest),
        "phase15 high-fanout histogram failed",
    );
    print_phase15_posting_histograms(name, &histograms);
    criterion.bench_function(name, |bencher| {
        bencher.iter(|| {
            let subjects = must(
                engine.lookup_subjects(black_box(request.clone())),
                "phase15 high-fanout lookup failed",
            );
            black_box(subjects)
        });
    });
}

#[cfg(feature = "bench-internals")]
fn bench_phase15_lookup_planner_pruning(criterion: &mut Criterion, filters: &[String]) {
    let name = "perf_optimization/phase15_lookup_planner_pruning";
    if !should_benchmark(name, filters) {
        return;
    }
    let engine = build_phase15_lookup_pruning_engine(PHASE15_PRUNING_CANDIDATES);
    let request = LookupResourcesRequest::new(
        User::user_id(PHASE15_TARGET_USER_ID),
        relation("can_view"),
        "doc",
    );
    print_phase15_eval_counter_sample(name, || {
        black_box(must(
            engine.lookup_resources(request.clone()),
            "phase15 planner-pruning lookup failed",
        ));
    });
    criterion.bench_function(name, |bencher| {
        bencher.iter(|| {
            reset_evaluation_read_counters();
            let resources = must(
                engine.lookup_resources(black_box(request.clone())),
                "phase15 planner-pruning lookup failed",
            );
            black_box(evaluation_read_counters());
            black_box(resources)
        });
    });
}

#[cfg(feature = "bench-internals")]
fn print_phase15_eval_counter_sample(name: &str, mut operation: impl FnMut()) {
    reset_evaluation_read_counters();
    operation();
    let counters = evaluation_read_counters();
    eprintln!(
        "{name}: check_evaluations={} memo_hit_opportunities={} completed_results={} \
         lookup_resources_candidates={} lookup_resources_full_root_checks={} \
         lookup_subjects_candidates={} lookup_subjects_usersets={} \
         lookup_subjects_full_root_checks={}",
        counters.check_evaluations,
        counters.check_memo_hit_opportunities,
        counters.check_completed_results,
        counters.lookup_resources_candidate_resources,
        counters.lookup_resources_full_root_checks,
        counters.lookup_subjects_candidate_subjects,
        counters.lookup_subjects_candidate_usersets,
        counters.lookup_subjects_full_root_checks,
    );
}

#[cfg(feature = "bench-internals")]
fn print_phase15_store_counter_sample(
    name: &str,
    engine: &ZanzibarEngine,
    mut operation: impl FnMut(),
) {
    reset_store_view_read_counters();
    operation();
    let counters = store_view_read_counters();
    let stats = must(
        engine.bench_relationship_delta_stats(Consistency::Latest),
        "phase15 delta stats failed",
    );
    eprintln!(
        "{name}: query_calls={} delta_segments_inspected={} tombstone_checks={} \
         checkpoint_rows={} delta_segments={} delta_inserted_rows={} delta_deleted_rows={} \
         delta_mutations={} tombstone_ratio_bps={}",
        counters.query_calls,
        counters.delta_segments_inspected,
        counters.tombstone_checks,
        stats.checkpoint_rows,
        stats.delta_segments,
        stats.delta_inserted_rows,
        stats.delta_deleted_rows,
        stats.delta_mutations,
        stats.tombstone_ratio_bps,
    );
}

#[cfg(feature = "bench-internals")]
fn print_phase15_posting_histograms(name: &str, histograms: &StorePostingHistograms) {
    print_phase15_posting_histogram(name, "resource", histograms.resource);
    print_phase15_posting_histogram(name, "resource_object", histograms.resource_object);
    print_phase15_posting_histogram(
        name,
        "resource_type_relation",
        histograms.resource_type_relation,
    );
    print_phase15_posting_histogram(name, "resource_type", histograms.resource_type);
    print_phase15_posting_histogram(name, "subject", histograms.subject);
    print_phase15_posting_histogram(
        name,
        "subject_type_relation",
        histograms.subject_type_relation,
    );
    print_phase15_posting_histogram(name, "subject_type", histograms.subject_type);
}

#[cfg(feature = "bench-internals")]
fn print_phase15_posting_histogram(
    name: &str,
    group: &str,
    histogram: simple_zanzibar::relationship::StorePostingHistogram,
) {
    eprintln!(
        "{name}: posting_group={group} keys={} total_postings={} max_posting_len={} \
         singleton_keys={} keys_2_to_4={} keys_5_to_16={} keys_17_to_64={} keys_65_to_256={} \
         keys_257_to_1024={} keys_1025_to_4096={} keys_over_4096={} estimated_row_id_bytes={}",
        histogram.keys,
        histogram.total_postings,
        histogram.max_posting_len,
        histogram.singleton_keys,
        histogram.keys_2_to_4,
        histogram.keys_5_to_16,
        histogram.keys_17_to_64,
        histogram.keys_65_to_256,
        histogram.keys_257_to_1024,
        histogram.keys_1025_to_4096,
        histogram.keys_over_4096,
        histogram.estimated_row_id_bytes,
    );
}

fn build_engine(rules: usize) -> ZanzibarEngine {
    let service = ZanzibarEngine::builder()
        .evaluation_limits(evaluation_limits())
        .build();
    must(service.add_dsl(org_schema()), "failed to apply org schema");
    apply_relationships(&service, &generated_relationships(rules));
    service
}

#[cfg(feature = "bench-internals")]
fn build_phase15_delete_heavy_engine(tombstones: usize) -> ZanzibarEngine {
    let service = phase15_engine(PHASE15_DIRECT_VIEWER_SCHEMA);
    let mut batch = Vec::with_capacity(MUTATION_BATCH_LIMIT);
    for index in 0..tombstones {
        push_phase15_relationship_line(
            &service,
            &mut batch,
            format!("doc:delete_heavy#viewer@user:deleted_{index:05}"),
        );
    }
    push_phase15_relationship_line(
        &service,
        &mut batch,
        format!("doc:delete_heavy#viewer@user:{PHASE15_TARGET_USER_ID}"),
    );
    flush_relationships(&service, &mut batch);
    let path = unique_snapshot_path("phase15_delete_heavy");
    must(
        service.save_snapshot(&path, SnapshotSaveOptions::default()),
        "failed to checkpoint phase15 delete-heavy fixture",
    );
    let loaded = must(
        ZanzibarEngine::load_snapshot(&path, SnapshotLoadOptions::default()),
        "failed to load phase15 delete-heavy fixture",
    );
    remove_file(&path);

    let deletes = (0..tombstones)
        .map(|index| {
            must(
                RelationshipMutation::delete(format!(
                    "doc:delete_heavy#viewer@user:deleted_{index:05}",
                )),
                "failed to build phase15 delete mutation",
            )
        })
        .collect::<Vec<_>>();
    must(
        loaded.write_relationships(deletes),
        "failed to apply phase15 delete-heavy delta",
    );
    loaded
}

#[cfg(feature = "bench-internals")]
fn build_phase15_lookup_pruning_engine(candidates: usize) -> ZanzibarEngine {
    let service = phase15_engine(PHASE15_LOOKUP_PRUNING_SCHEMA);
    let mut batch = Vec::with_capacity(MUTATION_BATCH_LIMIT);
    for index in 0..candidates {
        push_phase15_relationship_line(
            &service,
            &mut batch,
            format!("doc:blocked_{index:06}#banned@user:{PHASE15_TARGET_USER_ID}"),
        );
    }
    flush_relationships(&service, &mut batch);
    service
}

#[cfg(feature = "bench-internals")]
fn phase15_engine(schema: &str) -> ZanzibarEngine {
    let service = ZanzibarEngine::builder()
        .evaluation_limits(evaluation_limits())
        .build();
    must(service.add_dsl(schema), "failed to apply phase15 schema");
    service
}

#[cfg(feature = "bench-internals")]
fn push_phase15_relationship_line(
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

fn evaluation_limits() -> EvaluationLimits {
    EvaluationLimits {
        max_depth: non_zero_u32(50),
        max_fanout_per_step: non_zero_u32(100_000),
        max_lookup_results: non_zero_u32(1_000),
    }
}

#[cfg(feature = "bench-internals")]
const PHASE15_DIRECT_VIEWER_SCHEMA: &str = r"
    namespace doc {
        relation viewer {}
    }
    ";

#[cfg(feature = "bench-internals")]
const PHASE15_LOOKUP_PRUNING_SCHEMA: &str = r#"
    namespace doc {
        relation viewer {}
        relation banned {}

        relation can_view {
            rewrite exclusion(
                computed_userset(relation: "viewer"),
                computed_userset(relation: "banned")
            )
        }
    }
    "#;

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
