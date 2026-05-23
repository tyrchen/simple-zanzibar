//! Benchmarks for legacy scan and indexed direct-check paths.

use std::{collections::HashMap, hint::black_box, num::NonZeroU32, time::Duration};

use criterion::{BatchSize, Criterion};
use simple_zanzibar::{
    ZanzibarService,
    eval::EvaluationLimits,
    model::{
        LookupResourcesRequest, NamespaceConfig, Object, Relation, RelationConfig, RelationTuple,
        User,
    },
    store::{InMemoryTupleStore, TupleStore},
};

const DATASET_RELATIONSHIPS: usize = 100_000;
const LOOKUP_CANDIDATES: usize = 10_000;

fn owner_relation() -> Relation {
    Relation("owner".to_string())
}

fn object_at(index: usize) -> Object {
    Object {
        namespace: "doc".to_string(),
        id: format!("doc-{index:06}"),
    }
}

fn user_at(index: usize) -> User {
    User::UserId(format!("user-{index:06}"))
}

fn owner_tuple(index: usize) -> RelationTuple {
    RelationTuple {
        object: object_at(index),
        relation: owner_relation(),
        user: user_at(index),
    }
}

fn owner_namespace() -> NamespaceConfig {
    let relation = owner_relation();
    NamespaceConfig {
        name: "doc".to_string(),
        relations: HashMap::from([(
            relation.clone(),
            RelationConfig {
                name: relation,
                userset_rewrite: None,
            },
        )]),
    }
}

fn graph_schema() -> &'static str {
    r#"
    namespace doc {
        relation owner {}
        relation viewer {}
        relation parent {}
        relation inherited_viewer {
            rewrite tuple_to_userset(tupleset: "parent", computed_userset: "viewer")
        }
    }

    namespace group {
        relation member {}
    }

    namespace folder {
        relation viewer {}
    }
    "#
}

fn write_or_abort(store: &mut impl TupleStore, tuple: RelationTuple) {
    if let Err(error) = store.write_tuple(tuple) {
        eprintln!("failed to build benchmark dataset: {error}");
        std::process::abort();
    }
}

fn service_with_relationships(count: usize) -> ZanzibarService {
    let mut service = ZanzibarService::new();

    for index in 0..count {
        if let Err(error) = service.write_tuple(owner_tuple(index)) {
            eprintln!("failed to build service benchmark dataset: {error}");
            std::process::abort();
        }
    }

    if let Err(error) = service.add_config(owner_namespace()) {
        eprintln!("failed to build service benchmark schema: {error}");
        std::process::abort();
    }

    service
}

fn service_with_one_hop_relationships(count: usize) -> ZanzibarService {
    let mut service = ZanzibarService::new();
    let pairs = count / 2;
    for index in 0..pairs {
        if let Err(error) = service.write_tuple(RelationTuple {
            object: object_at(index),
            relation: Relation("viewer".to_string()),
            user: User::Userset(
                Object {
                    namespace: "group".to_string(),
                    id: format!("group-{index:06}"),
                },
                Relation("member".to_string()),
            ),
        }) {
            eprintln!("failed to build one-hop benchmark dataset: {error}");
            std::process::abort();
        }
        if let Err(error) = service.write_tuple(RelationTuple {
            object: Object {
                namespace: "group".to_string(),
                id: format!("group-{index:06}"),
            },
            relation: Relation("member".to_string()),
            user: user_at(index),
        }) {
            eprintln!("failed to build one-hop benchmark dataset: {error}");
            std::process::abort();
        }
    }
    if let Err(error) = service.add_dsl(graph_schema()) {
        eprintln!("failed to build one-hop benchmark schema: {error}");
        std::process::abort();
    }
    service
}

fn service_with_tuple_to_userset_relationships(count: usize) -> ZanzibarService {
    let mut service = ZanzibarService::new();
    let pairs = count / 2;
    for index in 0..pairs {
        if let Err(error) = service.write_tuple(RelationTuple {
            object: object_at(index),
            relation: Relation("parent".to_string()),
            user: User::Userset(
                Object {
                    namespace: "folder".to_string(),
                    id: format!("folder-{index:06}"),
                },
                Relation("viewer".to_string()),
            ),
        }) {
            eprintln!("failed to build tuple-to-userset benchmark dataset: {error}");
            std::process::abort();
        }
        if let Err(error) = service.write_tuple(RelationTuple {
            object: Object {
                namespace: "folder".to_string(),
                id: format!("folder-{index:06}"),
            },
            relation: Relation("viewer".to_string()),
            user: user_at(index),
        }) {
            eprintln!("failed to build tuple-to-userset benchmark dataset: {error}");
            std::process::abort();
        }
    }
    if let Err(error) = service.add_dsl(graph_schema()) {
        eprintln!("failed to build tuple-to-userset benchmark schema: {error}");
        std::process::abort();
    }
    service
}

fn service_with_lookup_relationships(count: usize) -> ZanzibarService {
    let mut service = ZanzibarService::new().with_evaluation_limits(EvaluationLimits {
        max_depth: non_zero_u32(50),
        max_fanout_per_step: non_zero_u32(1_000),
        max_lookup_results: non_zero_u32(u32::try_from(count).unwrap_or(u32::MAX)),
    });
    let viewer = Relation("viewer".to_string());
    let user = User::UserId("lookup-user".to_string());
    for index in 0..count {
        if let Err(error) = service.write_tuple(RelationTuple {
            object: object_at(index),
            relation: viewer.clone(),
            user: user.clone(),
        }) {
            eprintln!("failed to build lookup benchmark dataset: {error}");
            std::process::abort();
        }
    }
    if let Err(error) = service.add_dsl(graph_schema()) {
        eprintln!("failed to build lookup benchmark schema: {error}");
        std::process::abort();
    }
    service
}

fn store_with_relationships(count: usize) -> InMemoryTupleStore {
    let mut store = InMemoryTupleStore::default();
    for index in 0..count {
        write_or_abort(&mut store, owner_tuple(index));
    }
    store
}

fn bench_indexed_direct_check(c: &mut Criterion) {
    let service = service_with_relationships(DATASET_RELATIONSHIPS);
    let object = object_at(DATASET_RELATIONSHIPS - 1);
    let relation = owner_relation();
    let user = user_at(DATASET_RELATIONSHIPS - 1);

    c.bench_function("indexed_direct_check_100k", |b| {
        b.iter(|| {
            black_box(service.check(black_box(&object), black_box(&relation), black_box(&user)))
        });
    });
}

fn bench_legacy_store_scan(c: &mut Criterion) {
    let store = store_with_relationships(DATASET_RELATIONSHIPS);
    let object = object_at(DATASET_RELATIONSHIPS - 1);
    let relation = owner_relation();
    let user = user_at(DATASET_RELATIONSHIPS - 1);

    c.bench_function("legacy_store_read_tuples_scan_100k", |b| {
        b.iter_batched(
            || (&object, &relation, &user),
            |(object, relation, user)| {
                black_box(store.read_tuples(
                    black_box(object),
                    Some(black_box(relation)),
                    Some(black_box(user)),
                ))
            },
            BatchSize::SmallInput,
        );
    });
}

fn bench_one_hop_userset_check(c: &mut Criterion) {
    let service = service_with_one_hop_relationships(DATASET_RELATIONSHIPS);
    let target = (DATASET_RELATIONSHIPS / 2) - 1;
    let object = object_at(target);
    let relation = Relation("viewer".to_string());
    let user = user_at(target);

    c.bench_function("one_hop_userset_check_100k", |b| {
        b.iter(|| {
            black_box(service.check(black_box(&object), black_box(&relation), black_box(&user)))
        });
    });
}

fn bench_tuple_to_userset_check(c: &mut Criterion) {
    let service = service_with_tuple_to_userset_relationships(DATASET_RELATIONSHIPS);
    let target = (DATASET_RELATIONSHIPS / 2) - 1;
    let object = object_at(target);
    let relation = Relation("inherited_viewer".to_string());
    let user = user_at(target);

    c.bench_function("tuple_to_userset_check_100k", |b| {
        b.iter(|| {
            black_box(service.check(black_box(&object), black_box(&relation), black_box(&user)))
        });
    });
}

fn bench_lookup_resources(c: &mut Criterion) {
    for count in [100_usize, 1_000, LOOKUP_CANDIDATES] {
        let service = service_with_lookup_relationships(count);
        let request = LookupResourcesRequest {
            subject: User::UserId("lookup-user".to_string()),
            permission: Relation("viewer".to_string()),
            resource_type: "doc".to_string(),
        };
        let name = format!("lookup_resources_{count}_candidates");

        c.bench_function(&name, |b| {
            b.iter(|| black_box(service.lookup_resources(black_box(&request))));
        });
    }
}

fn main() {
    if cfg!(debug_assertions) {
        return;
    }

    let mut criterion = Criterion::default()
        .sample_size(10)
        .warm_up_time(Duration::from_millis(100))
        .measurement_time(Duration::from_millis(500))
        .configure_from_args();
    bench_indexed_direct_check(&mut criterion);
    bench_one_hop_userset_check(&mut criterion);
    bench_tuple_to_userset_check(&mut criterion);
    bench_lookup_resources(&mut criterion);
    bench_legacy_store_scan(&mut criterion);
    criterion.final_summary();
}

fn non_zero_u32(value: u32) -> NonZeroU32 {
    NonZeroU32::new(value).unwrap_or(NonZeroU32::MIN)
}
