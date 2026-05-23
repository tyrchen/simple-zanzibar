//! Benchmarks for legacy scan and indexed direct-check paths.

use criterion::{BatchSize, Criterion};
use simple_zanzibar::model::{
    NamespaceConfig, Object, Relation, RelationConfig, RelationTuple, User,
};
use simple_zanzibar::store::{InMemoryTupleStore, TupleStore};
use simple_zanzibar::ZanzibarService;

use std::collections::HashMap;
use std::hint::black_box;
use std::time::Duration;

const DATASET_RELATIONSHIPS: usize = 100_000;

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
    bench_legacy_store_scan(&mut criterion);
    criterion.final_summary();
}
