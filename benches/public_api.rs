//! Benchmarks for public API operations.

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
    PolicyText, SnapshotCompression, SnapshotLoadOptions, SnapshotSaveOptions, ZanzibarEngine,
    ZanzibarService,
    domain::Relationship,
    eval::EvaluationLimits,
    model::{
        CheckRequest, ExpandRequest, LookupObjectPermissionsRequest, LookupPermissionsRequest,
        LookupResourcesRequest, LookupSubjectsRequest, Object, Relation, User,
    },
    relationship::RelationshipMutation,
    revision::Consistency,
    schema::SchemaSource,
};

const DATASET_RULES: usize = 100_000;
const SMALL_DATASET_RULES: usize = 1_000;
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

    let service = build_service_with_relationships(DATASET_RULES);
    let policy = must(service.export_policy_text(), "failed to export policy text");
    let engine = must(
        ZanzibarEngine::from_policy_text(&policy),
        "failed to build engine from policy text",
    );

    bench_schema_apis(&mut criterion, &filters);
    bench_read_apis(&mut criterion, &filters, &engine);
    bench_write_apis(&mut criterion, &filters, &policy);
    bench_policy_apis(&mut criterion, &filters, &service);
    bench_snapshot_apis(&mut criterion, &filters, &service);
    criterion.final_summary();
}

fn bench_schema_apis(criterion: &mut Criterion, filters: &[String]) {
    let apply_name = "public_api/apply_schema/small";
    if should_benchmark(apply_name, filters) {
        criterion.bench_function(apply_name, |bencher| {
            bencher.iter_batched(
                ZanzibarEngine::default,
                |engine| {
                    black_box(must(
                        engine.apply_schema(SchemaSource {
                            name: Some("org"),
                            text: org_schema(),
                        }),
                        "apply_schema failed",
                    ))
                },
                BatchSize::SmallInput,
            );
        });
    }

    let replace_name = "public_api/replace_schema/small";
    if should_benchmark(replace_name, filters) {
        criterion.bench_function(replace_name, |bencher| {
            bencher.iter_batched(
                || {
                    let engine = ZanzibarEngine::default();
                    must(
                        engine.apply_schema(SchemaSource {
                            name: Some("initial"),
                            text: unused_schema(),
                        }),
                        "initial apply_schema failed",
                    );
                    engine
                },
                |engine| {
                    black_box(must(
                        engine.replace_schema(SchemaSource {
                            name: Some("org"),
                            text: org_schema(),
                        }),
                        "replace_schema failed",
                    ))
                },
                BatchSize::SmallInput,
            );
        });
    }

    let delete_relation_name = "public_api/delete_relation/small";
    if should_benchmark(delete_relation_name, filters) {
        criterion.bench_function(delete_relation_name, |bencher| {
            bencher.iter_batched(
                unused_policy_engine,
                |engine| {
                    black_box(must(
                        engine.delete_relation("doc", "unused"),
                        "delete failed",
                    ))
                },
                BatchSize::SmallInput,
            );
        });
    }

    let delete_namespace_name = "public_api/delete_namespace/small";
    if should_benchmark(delete_namespace_name, filters) {
        criterion.bench_function(delete_namespace_name, |bencher| {
            bencher.iter_batched(
                unused_policy_engine,
                |engine| black_box(must(engine.delete_namespace("unused"), "delete failed")),
                BatchSize::SmallInput,
            );
        });
    }
}

fn bench_read_apis(criterion: &mut Criterion, filters: &[String], engine: &ZanzibarEngine) {
    let target_user = User::UserId(TARGET_USER_ID.to_string());
    let can_view = relation("can_view");
    let direct_doc = object("doc", "direct_doc");
    let inherited_doc = object("doc", "inherited_doc");

    let check_name = "public_api/check/100k";
    if should_benchmark(check_name, filters) {
        criterion.bench_function(check_name, |bencher| {
            bencher.iter(|| {
                black_box(must(
                    engine.check(CheckRequest::new(
                        black_box(direct_doc.clone()),
                        black_box(can_view.clone()),
                        black_box(target_user.clone()),
                        Consistency::Latest,
                    )),
                    "check failed",
                ))
            });
        });
    }

    let expand_name = "public_api/expand/100k";
    if should_benchmark(expand_name, filters) {
        criterion.bench_function(expand_name, |bencher| {
            bencher.iter(|| {
                black_box(must(
                    engine.expand(ExpandRequest::new(
                        black_box(inherited_doc.clone()),
                        black_box(can_view.clone()),
                        Consistency::Latest,
                    )),
                    "expand failed",
                ))
            });
        });
    }

    let lookup_resources_name = "public_api/lookup_resources/100k";
    if should_benchmark(lookup_resources_name, filters) {
        let request = LookupResourcesRequest {
            subject: target_user.clone(),
            permission: can_view.clone(),
            resource_type: "doc".to_string(),
        };
        criterion.bench_function(lookup_resources_name, |bencher| {
            bencher.iter(|| {
                black_box(must(
                    engine.lookup_resources(black_box(request.clone())),
                    "lookup_resources failed",
                ))
            });
        });
    }

    let lookup_subjects_name = "public_api/lookup_subjects/100k";
    if should_benchmark(lookup_subjects_name, filters) {
        let request = LookupSubjectsRequest {
            resource: direct_doc.clone(),
            permission: can_view.clone(),
            subject_type: "user".to_string(),
        };
        criterion.bench_function(lookup_subjects_name, |bencher| {
            bencher.iter(|| {
                black_box(must(
                    engine.lookup_subjects(black_box(request.clone())),
                    "lookup_subjects failed",
                ))
            });
        });
    }

    let lookup_permissions_name = "public_api/lookup_permissions/100k";
    if should_benchmark(lookup_permissions_name, filters) {
        let request = LookupPermissionsRequest {
            subject: target_user.clone(),
            resource: inherited_doc.clone(),
            consistency: Consistency::Latest,
        };
        criterion.bench_function(lookup_permissions_name, |bencher| {
            bencher.iter(|| {
                black_box(must(
                    engine.lookup_permissions(black_box(request.clone())),
                    "lookup_permissions failed",
                ))
            });
        });
    }

    let object_permissions_name = "public_api/lookup_object_permissions/100k";
    if should_benchmark(object_permissions_name, filters) {
        let request = LookupObjectPermissionsRequest {
            resource: direct_doc,
            subject_type: "user".to_string(),
            consistency: Consistency::Latest,
        };
        criterion.bench_function(object_permissions_name, |bencher| {
            bencher.iter(|| {
                black_box(must(
                    engine.lookup_object_permissions(black_box(request.clone())),
                    "lookup_object_permissions failed",
                ))
            });
        });
    }
}

fn bench_write_apis(criterion: &mut Criterion, filters: &[String], policy: &PolicyText) {
    let write_name = "public_api/write_relationships/1k_batch";
    if should_benchmark(write_name, filters) {
        let mutations = generated_extra_relationships(1_000)
            .into_iter()
            .map(RelationshipMutation::Touch)
            .collect::<Vec<_>>();
        criterion.bench_function(write_name, |bencher| {
            bencher.iter_batched(
                || {
                    must(
                        ZanzibarEngine::from_policy_text(policy),
                        "engine build failed",
                    )
                },
                |engine| {
                    black_box(must(
                        engine.write_relationships(mutations.clone()),
                        "write_relationships failed",
                    ))
                },
                BatchSize::LargeInput,
            );
        });
    }

    let apply_policy_name = "public_api/apply_policy_text/1k";
    if should_benchmark(apply_policy_name, filters) {
        let small_policy = must(
            build_service_with_relationships(SMALL_DATASET_RULES).export_policy_text(),
            "small policy export failed",
        );
        criterion.bench_function(apply_policy_name, |bencher| {
            bencher.iter_batched(
                ZanzibarEngine::default,
                |engine| {
                    black_box(must(
                        engine.apply_policy_text(black_box(&small_policy)),
                        "apply_policy_text failed",
                    ))
                },
                BatchSize::LargeInput,
            );
        });
    }
}

fn bench_policy_apis(criterion: &mut Criterion, filters: &[String], service: &ZanzibarService) {
    let export_name = "public_api/export_policy_text/100k";
    if should_benchmark(export_name, filters) {
        criterion.bench_function(export_name, |bencher| {
            bencher.iter(|| {
                black_box(must(
                    service.export_policy_text(),
                    "export_policy_text failed",
                ))
            });
        });
    }

    let export_files_name = "public_api/export_policy_files/1k";
    if should_benchmark(export_files_name, filters) {
        let small_service = build_service_with_relationships(SMALL_DATASET_RULES);
        criterion.bench_function(export_files_name, |bencher| {
            bencher.iter_batched(
                || unique_path("policy_export"),
                |path| {
                    must(
                        small_service.export_policy_files(&path),
                        "export_policy_files failed",
                    );
                    remove_directory(&path);
                    black_box(path)
                },
                BatchSize::LargeInput,
            );
        });
    }
}

fn bench_snapshot_apis(criterion: &mut Criterion, filters: &[String], service: &ZanzibarService) {
    let save_raw_name = "public_api/snapshot_save_uncompressed/100k";
    if should_benchmark(save_raw_name, filters) {
        criterion.bench_function(save_raw_name, |bencher| {
            bencher.iter_batched(
                || unique_path("save_raw").with_extension("szsnap"),
                |path| {
                    must(
                        service.save_snapshot(&path, SnapshotSaveOptions::default()),
                        "raw snapshot save failed",
                    );
                    let len = must(fs::metadata(&path), "raw snapshot metadata failed").len();
                    remove_file(&path);
                    black_box(len)
                },
                BatchSize::LargeInput,
            );
        });
    }

    let load_raw_name = "public_api/snapshot_load_uncompressed/100k";
    if should_benchmark(load_raw_name, filters) {
        let path = prepared_snapshot(service, SnapshotSaveOptions::default(), "load_raw");
        criterion.bench_function(load_raw_name, |bencher| {
            bencher.iter(|| {
                black_box(must(
                    ZanzibarEngine::load_snapshot(&path, SnapshotLoadOptions::default()),
                    "raw snapshot load failed",
                ))
            });
        });
        remove_file(&path);
    }

    let save_zstd_name = "public_api/snapshot_save_zstd/100k";
    if should_benchmark(save_zstd_name, filters) {
        criterion.bench_function(save_zstd_name, |bencher| {
            bencher.iter_batched(
                || unique_path("save_zstd").with_extension("szsnap.zst"),
                |path| {
                    must(
                        service.save_snapshot(&path, zstd_save_options()),
                        "zstd snapshot save failed",
                    );
                    let len = must(fs::metadata(&path), "zstd snapshot metadata failed").len();
                    remove_file(&path);
                    black_box(len)
                },
                BatchSize::LargeInput,
            );
        });
    }

    let load_zstd_name = "public_api/snapshot_load_zstd/100k";
    if should_benchmark(load_zstd_name, filters) {
        let path = prepared_snapshot(service, zstd_save_options(), "load_zstd");
        criterion.bench_function(load_zstd_name, |bencher| {
            bencher.iter(|| {
                black_box(must(
                    ZanzibarEngine::load_snapshot(&path, zstd_load_options()),
                    "zstd snapshot load failed",
                ))
            });
        });
        remove_file(&path);
    }
}

fn build_service_with_relationships(rules: usize) -> ZanzibarService {
    let mut service = ZanzibarService::new().with_evaluation_limits(evaluation_limits());
    must(service.add_dsl(org_schema()), "failed to apply org schema");
    let relationships = generated_relationships(rules);
    apply_relationships(&mut service, &relationships);
    service
}

fn evaluation_limits() -> EvaluationLimits {
    EvaluationLimits {
        max_depth: non_zero_u32(50),
        max_fanout_per_step: non_zero_u32(100_000),
        max_lookup_results: non_zero_u32(1_000),
    }
}

fn apply_relationships(service: &mut ZanzibarService, relationships: &[Relationship]) {
    let mut batch = Vec::with_capacity(MUTATION_BATCH_LIMIT);
    for relationship in relationships {
        batch.push(RelationshipMutation::Touch(relationship.clone()));
        if batch.len() == MUTATION_BATCH_LIMIT {
            flush_relationships(service, &mut batch);
        }
    }
    flush_relationships(service, &mut batch);
}

fn flush_relationships(service: &mut ZanzibarService, batch: &mut Vec<RelationshipMutation>) {
    if batch.is_empty() {
        return;
    }
    let mutations = std::mem::take(batch);
    must(
        service.apply_relationship_mutations(mutations, []),
        "failed to apply relationship batch",
    );
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

fn generated_extra_relationships(count: usize) -> Vec<Relationship> {
    (0..count)
        .map(|index| {
            parse_relationship(format!(
                "doc:write_doc_{index:06}#viewer@user:write_user_{index:06}",
            ))
        })
        .collect()
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

fn unused_schema() -> &'static str {
    r"
    namespace unused {
        relation member {}
    }

    namespace doc {
        relation unused {}
    }
    "
}

fn unused_policy_engine() -> ZanzibarEngine {
    let engine = ZanzibarEngine::default();
    must(
        engine.apply_schema(SchemaSource {
            name: Some("unused"),
            text: unused_schema(),
        }),
        "unused schema failed",
    );
    engine
}

fn zstd_save_options() -> SnapshotSaveOptions {
    SnapshotSaveOptions {
        compression: SnapshotCompression::Zstd,
        ..SnapshotSaveOptions::default()
    }
}

fn zstd_load_options() -> SnapshotLoadOptions {
    SnapshotLoadOptions {
        compression: SnapshotCompression::Zstd,
        ..SnapshotLoadOptions::default()
    }
}

fn prepared_snapshot(
    service: &ZanzibarService,
    options: SnapshotSaveOptions,
    label: &str,
) -> PathBuf {
    let path = unique_path(label).with_extension(match options.compression {
        SnapshotCompression::None => "szsnap",
        SnapshotCompression::Zstd => "szsnap.zst",
    });
    must(
        service.save_snapshot(&path, options),
        "failed to prepare snapshot",
    );
    path
}

fn parse_relationship(value: impl AsRef<str>) -> Relationship {
    must(
        value.as_ref().parse(),
        "failed to parse benchmark relationship",
    )
}

fn object(namespace: &str, id: &str) -> Object {
    Object::new(namespace, id)
}

fn relation(name: &str) -> Relation {
    Relation::new(name)
}

fn unique_path(label: &str) -> PathBuf {
    env::temp_dir().join(format!(
        "simple_zanzibar_public_api_bench_{label}_{}_{}",
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

fn remove_directory(path: &Path) {
    let _ = fs::remove_dir_all(path);
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
