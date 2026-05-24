//! Concurrent runtime benchmarks for lock-free reads and single-writer actor writes.

use std::{
    env,
    hint::black_box,
    num::{NonZeroU32, NonZeroUsize},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use criterion::Criterion;
use simple_zanzibar::{
    TenantId, ZanzibarEngine, ZanzibarTenantShards,
    eval::EvaluationLimits,
    model::{Object, Relation, User},
    relationship::RelationshipMutation,
};

const BASE_RELATIONSHIPS: usize = 20_000;
const SEED_BATCH_SIZE: usize = 5_000;
const WRITER_QUEUE_CAPACITY: usize = 16_384;
const TARGET_USER: &str = "user-000000";

fn main() {
    if cfg!(debug_assertions) {
        return;
    }

    let filters = benchmark_filters();
    let scenarios = scenario_configs();
    let selected = scenarios
        .iter()
        .copied()
        .filter(|scenario| should_benchmark(scenario.name, &filters))
        .collect::<Vec<_>>();

    let summary = selected
        .iter()
        .copied()
        .map(run_scenario)
        .collect::<Vec<_>>();
    print_markdown_summary(&summary);

    let mut criterion = Criterion::default()
        .sample_size(10)
        .warm_up_time(Duration::from_millis(100))
        .measurement_time(Duration::from_millis(800))
        .configure_from_args();

    for scenario in selected {
        criterion.bench_function(scenario.name, |bencher| {
            bencher.iter_custom(|iterations| {
                let started = Instant::now();
                for _ in 0..iterations {
                    black_box(run_scenario(scenario));
                }
                started.elapsed()
            });
        });
    }
    criterion.final_summary();
}

#[derive(Debug, Clone, Copy)]
struct Scenario {
    name: &'static str,
    reader_threads: usize,
    writer_threads: usize,
    batch_size: usize,
    writer_pause: Duration,
    duration: Duration,
    tenants: usize,
}

#[derive(Debug, Clone)]
struct ScenarioStats {
    name: &'static str,
    duration: Duration,
    tenants: usize,
    reader_threads: usize,
    writer_threads: usize,
    batch_size: usize,
    read_ops: u64,
    write_api_calls: u64,
    logical_mutations: u64,
    write_latency_p50: Duration,
    write_latency_p95: Duration,
    write_latency_max: Duration,
}

#[derive(Debug, Default)]
struct WriterStats {
    write_api_calls: u64,
    logical_mutations: u64,
    latencies: Vec<Duration>,
}

fn scenario_configs() -> [Scenario; 6] {
    [
        Scenario {
            name: "concurrent_runtime/read_heavy_light_write_batched",
            reader_threads: 8,
            writer_threads: 1,
            batch_size: 32,
            writer_pause: Duration::from_millis(25),
            duration: Duration::from_millis(500),
            tenants: 1,
        },
        Scenario {
            name: "concurrent_runtime/read_heavy_medium_write_unbatched",
            reader_threads: 8,
            writer_threads: 1,
            batch_size: 1,
            writer_pause: Duration::ZERO,
            duration: Duration::from_millis(500),
            tenants: 1,
        },
        Scenario {
            name: "concurrent_runtime/read_heavy_medium_write_batched",
            reader_threads: 8,
            writer_threads: 1,
            batch_size: 128,
            writer_pause: Duration::ZERO,
            duration: Duration::from_millis(500),
            tenants: 1,
        },
        Scenario {
            name: "concurrent_runtime/read_heavy_heavy_write_unbatched",
            reader_threads: 8,
            writer_threads: 4,
            batch_size: 1,
            writer_pause: Duration::ZERO,
            duration: Duration::from_millis(500),
            tenants: 1,
        },
        Scenario {
            name: "concurrent_runtime/read_heavy_heavy_write_batched",
            reader_threads: 8,
            writer_threads: 4,
            batch_size: 128,
            writer_pause: Duration::ZERO,
            duration: Duration::from_millis(500),
            tenants: 1,
        },
        Scenario {
            name: "concurrent_runtime/tenant_sharded_heavy_write_batched",
            reader_threads: 8,
            writer_threads: 4,
            batch_size: 128,
            writer_pause: Duration::ZERO,
            duration: Duration::from_millis(500),
            tenants: 4,
        },
    ]
}

fn run_scenario(scenario: Scenario) -> ScenarioStats {
    let engines = build_engines(scenario);
    let stop = Arc::new(AtomicBool::new(false));
    let read_ops = Arc::new(AtomicU64::new(0));
    let sequence = Arc::new(AtomicU64::new(0));

    let readers = spawn_readers(scenario, &engines, &stop, &read_ops);
    let writers = spawn_writers(scenario, &engines, &stop, &sequence);

    thread::sleep(scenario.duration);
    stop.store(true, Ordering::Release);

    for reader in readers {
        join_reader(reader);
    }

    let mut writer_stats = WriterStats::default();
    for writer in writers {
        let local = join_writer(writer);
        writer_stats.write_api_calls = writer_stats
            .write_api_calls
            .saturating_add(local.write_api_calls);
        writer_stats.logical_mutations = writer_stats
            .logical_mutations
            .saturating_add(local.logical_mutations);
        writer_stats.latencies.extend(local.latencies);
    }

    writer_stats.latencies.sort_unstable();
    ScenarioStats {
        name: scenario.name,
        duration: scenario.duration,
        tenants: scenario.tenants,
        reader_threads: scenario.reader_threads,
        writer_threads: scenario.writer_threads,
        batch_size: scenario.batch_size,
        read_ops: read_ops.load(Ordering::Acquire),
        write_api_calls: writer_stats.write_api_calls,
        logical_mutations: writer_stats.logical_mutations,
        write_latency_p50: percentile(&writer_stats.latencies, 50),
        write_latency_p95: percentile(&writer_stats.latencies, 95),
        write_latency_max: percentile(&writer_stats.latencies, 100),
    }
}

fn spawn_readers(
    scenario: Scenario,
    engines: &[Arc<ZanzibarEngine>],
    stop: &Arc<AtomicBool>,
    read_ops: &Arc<AtomicU64>,
) -> Vec<thread::JoinHandle<()>> {
    (0..scenario.reader_threads)
        .map(|index| {
            let engine = engine_for(engines, index);
            let stop = Arc::clone(stop);
            let read_ops = Arc::clone(read_ops);
            thread::spawn(move || {
                let object = Object::new("doc", "doc-000000");
                let relation = Relation::new("viewer");
                let user = User::user_id(TARGET_USER);
                let mut local_reads = 0_u64;
                while !stop.load(Ordering::Acquire) {
                    let allowed = must(
                        engine.check_relation(&object, &relation, &user),
                        "reader check failed",
                    );
                    if allowed {
                        local_reads = local_reads.saturating_add(1);
                    }
                }
                read_ops.fetch_add(local_reads, Ordering::AcqRel);
            })
        })
        .collect()
}

fn spawn_writers(
    scenario: Scenario,
    engines: &[Arc<ZanzibarEngine>],
    stop: &Arc<AtomicBool>,
    sequence: &Arc<AtomicU64>,
) -> Vec<thread::JoinHandle<WriterStats>> {
    (0..scenario.writer_threads)
        .map(|index| {
            let engine = engine_for(engines, index);
            let stop = Arc::clone(stop);
            let sequence = Arc::clone(sequence);
            thread::spawn(move || {
                let mut stats = WriterStats::default();
                while !stop.load(Ordering::Acquire) {
                    let mutations = build_write_batch(scenario.batch_size, &sequence);
                    let mutation_count = mutations.len();
                    let started = Instant::now();
                    must(engine.write_relationships(mutations), "writer batch failed");
                    stats.latencies.push(started.elapsed());
                    stats.write_api_calls = stats.write_api_calls.saturating_add(1);
                    stats.logical_mutations = stats
                        .logical_mutations
                        .saturating_add(u64::try_from(mutation_count).unwrap_or(u64::MAX));
                    if !scenario.writer_pause.is_zero() {
                        thread::sleep(scenario.writer_pause);
                    }
                }
                stats
            })
        })
        .collect()
}

fn build_engines(scenario: Scenario) -> Vec<Arc<ZanzibarEngine>> {
    if scenario.tenants == 1 {
        return vec![Arc::new(build_engine(BASE_RELATIONSHIPS))];
    }

    let shards = ZanzibarTenantShards::new(
        ZanzibarEngine::builder().writer_queue_capacity(non_zero_usize(WRITER_QUEUE_CAPACITY)),
    );
    (0..scenario.tenants)
        .map(|tenant_index| {
            let tenant = must(
                TenantId::new(format!("tenant-{tenant_index:02}")),
                "invalid benchmark tenant id",
            );
            let engine = shards.get_or_create(tenant);
            seed_engine(&engine, BASE_RELATIONSHIPS / scenario.tenants);
            engine
        })
        .collect()
}

fn build_engine(relationships: usize) -> ZanzibarEngine {
    let engine = ZanzibarEngine::builder()
        .writer_queue_capacity(non_zero_usize(WRITER_QUEUE_CAPACITY))
        .evaluation_limits(EvaluationLimits {
            max_depth: non_zero_u32(50),
            max_fanout_per_step: non_zero_u32(100_000),
            max_lookup_results: non_zero_u32(10_000),
        })
        .build();
    seed_engine(&engine, relationships);
    engine
}

fn seed_engine(engine: &ZanzibarEngine, relationships: usize) {
    must(
        engine.add_dsl(doc_schema()),
        "failed to apply benchmark schema",
    );
    let mut batch = Vec::with_capacity(SEED_BATCH_SIZE);
    for index in 0..relationships {
        batch.push(must(
            RelationshipMutation::touch(format!("doc:doc-{index:06}#viewer@user:user-{index:06}")),
            "failed to build seed relationship",
        ));
        if batch.len() == SEED_BATCH_SIZE {
            flush_seed_batch(engine, &mut batch);
        }
    }
    flush_seed_batch(engine, &mut batch);
}

fn flush_seed_batch(engine: &ZanzibarEngine, batch: &mut Vec<RelationshipMutation>) {
    if batch.is_empty() {
        return;
    }
    let mutations = std::mem::take(batch);
    must(
        engine.write_relationships(mutations),
        "failed to seed benchmark relationships",
    );
}

fn build_write_batch(batch_size: usize, sequence: &AtomicU64) -> Vec<RelationshipMutation> {
    let mut mutations = Vec::with_capacity(batch_size);
    for _ in 0..batch_size {
        let next = sequence.fetch_add(1, Ordering::AcqRel);
        mutations.push(must(
            RelationshipMutation::touch(format!(
                "doc:write-{next:012}#viewer@user:writer-{next:012}",
            )),
            "failed to build writer relationship",
        ));
    }
    mutations
}

fn engine_for(engines: &[Arc<ZanzibarEngine>], index: usize) -> Arc<ZanzibarEngine> {
    let engine_count = engines.len();
    if engine_count == 0 {
        abort("benchmark requires at least one engine");
    }
    let engine_index = index % engine_count;
    match engines.get(engine_index) {
        Some(engine) => Arc::clone(engine),
        None => abort("benchmark engine index out of range"),
    }
}

fn join_reader(handle: thread::JoinHandle<()>) {
    if handle.join().is_err() {
        abort("reader thread panicked");
    }
}

fn join_writer(handle: thread::JoinHandle<WriterStats>) -> WriterStats {
    match handle.join() {
        Ok(stats) => stats,
        Err(_) => abort("writer thread panicked"),
    }
}

fn percentile(values: &[Duration], percentile: usize) -> Duration {
    if values.is_empty() {
        return Duration::ZERO;
    }
    let last_index = values.len().saturating_sub(1);
    let rank = last_index.saturating_mul(percentile) / 100;
    match values.get(rank) {
        Some(value) => *value,
        None => Duration::ZERO,
    }
}

fn print_markdown_summary(stats: &[ScenarioStats]) {
    if stats.is_empty() {
        return;
    }
    println!();
    println!("concurrent runtime benchmark summary");
    println!(
        "| scenario | tenants | readers | writers | batch | read ops/s | write calls/s | logical \
         writes/s | write p50 | write p95 | write max |",
    );
    println!("| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |");
    for stat in stats {
        println!(
            "| {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} |",
            stat.name,
            stat.tenants,
            stat.reader_threads,
            stat.writer_threads,
            stat.batch_size,
            per_second(stat.read_ops, stat.duration),
            per_second(stat.write_api_calls, stat.duration),
            per_second(stat.logical_mutations, stat.duration),
            micros(stat.write_latency_p50),
            micros(stat.write_latency_p95),
            micros(stat.write_latency_max),
        );
    }
    println!();
}

fn per_second(count: u64, duration: Duration) -> u64 {
    let nanos = duration.as_nanos();
    if nanos == 0 {
        return 0;
    }
    let scaled = u128::from(count).saturating_mul(1_000_000_000) / nanos;
    u64::try_from(scaled).unwrap_or(u64::MAX)
}

fn micros(duration: Duration) -> u64 {
    u64::try_from(duration.as_micros()).unwrap_or(u64::MAX)
}

fn benchmark_filters() -> Vec<String> {
    env::args()
        .skip(1)
        .filter(|argument| !argument.starts_with("--"))
        .collect()
}

fn should_benchmark(name: &str, filters: &[String]) -> bool {
    filters.is_empty() || filters.iter().any(|filter| name.contains(filter))
}

fn doc_schema() -> &'static str {
    r"
    namespace doc {
        relation viewer {}
    }
    "
}

fn non_zero_u32(value: u32) -> NonZeroU32 {
    match NonZeroU32::new(value) {
        Some(value) => value,
        None => NonZeroU32::MIN,
    }
}

fn non_zero_usize(value: usize) -> NonZeroUsize {
    match NonZeroUsize::new(value) {
        Some(value) => value,
        None => NonZeroUsize::MIN,
    }
}

fn must<T, E: std::fmt::Display>(result: Result<T, E>, message: &str) -> T {
    match result {
        Ok(value) => value,
        Err(error) => abort(&format!("{message}: {error}")),
    }
}

fn abort(message: &str) -> ! {
    eprintln!("{message}");
    std::process::abort();
}
