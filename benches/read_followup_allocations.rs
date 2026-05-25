//! Allocation-only baseline for Phase 15 read-optimization follow-up.

#[cfg(feature = "bench-internals")]
use std::{alloc::System, hint::black_box, process};

#[cfg(feature = "bench-internals")]
#[path = "common/phase15_fixtures.rs"]
mod phase15_fixtures;

#[cfg(feature = "bench-internals")]
use phase15_fixtures::{
    PHASE15_HIGH_FANOUT, PHASE15_LOOKUP_SUBJECTS, PHASE15_TARGET_USER_ID, PHASE15_TARGETED_RULES,
    build_phase15_high_fanout_engine, build_phase15_lookup_subjects_engine,
    build_phase15_shared_parent_engine,
};
#[cfg(feature = "bench-internals")]
use simple_zanzibar::{
    model::{LookupResourcesRequest, LookupSubjectsRequest, Object, Relation, User},
    revision::Consistency,
};
#[cfg(feature = "bench-internals")]
use stats_alloc::{INSTRUMENTED_SYSTEM, Region, Stats, StatsAlloc};

#[cfg(feature = "bench-internals")]
#[global_allocator]
static GLOBAL: &StatsAlloc<System> = &INSTRUMENTED_SYSTEM;

#[cfg(feature = "bench-internals")]
const PHASE15_ALLOCATION_ITERATIONS: usize = 16;

#[cfg(feature = "bench-internals")]
fn main() {
    if cfg!(debug_assertions) {
        return;
    }

    sample_phase15_memo_shared_parent_allocations();
    sample_phase15_lookup_subjects_allocations();
    sample_phase15_high_fanout_allocations();
}

#[cfg(not(feature = "bench-internals"))]
fn main() {}

#[cfg(feature = "bench-internals")]
fn sample_phase15_memo_shared_parent_allocations() {
    let name = "read_followup_allocations/phase15_memo_shared_parent";
    let engine = build_phase15_shared_parent_engine(PHASE15_TARGETED_RULES);
    let request = LookupResourcesRequest::new(
        User::user_id(PHASE15_TARGET_USER_ID),
        relation("can_view"),
        "doc",
    );
    let inputs = repeated_inputs(request);
    print_phase15_allocation_sample(name, inputs, |request| {
        must(
            engine.lookup_resources(request),
            "phase15 memo allocation sample failed",
        )
    });
}

#[cfg(feature = "bench-internals")]
fn sample_phase15_lookup_subjects_allocations() {
    let name = "read_followup_allocations/phase15_lookup_subjects_allocation";
    let engine = build_phase15_lookup_subjects_engine(PHASE15_LOOKUP_SUBJECTS);
    let request =
        LookupSubjectsRequest::new(object("doc", "allocation"), relation("can_view"), "user");
    let inputs = repeated_inputs(request);
    print_phase15_allocation_sample(name, inputs, |request| {
        must(
            engine.lookup_subjects(request),
            "phase15 lookup-subjects allocation sample failed",
        )
    });
}

#[cfg(feature = "bench-internals")]
fn sample_phase15_high_fanout_allocations() {
    let name = "read_followup_allocations/phase15_high_fanout_posting";
    let engine = build_phase15_high_fanout_engine(PHASE15_HIGH_FANOUT);
    let request =
        LookupSubjectsRequest::new(object("doc", "high_fanout"), relation("viewer"), "user");
    let inputs = repeated_inputs(request);
    print_phase15_allocation_sample(name, inputs, |request| {
        must(
            engine.lookup_subjects(request),
            "phase15 high-fanout allocation sample failed",
        )
    });
    let histograms = must(
        engine.bench_relationship_posting_histograms(Consistency::Latest),
        "phase15 high-fanout allocation histogram failed",
    );
    black_box(histograms);
}

#[cfg(feature = "bench-internals")]
fn repeated_inputs<T: Clone>(request: T) -> Vec<T> {
    vec![request; PHASE15_ALLOCATION_ITERATIONS]
}

#[cfg(feature = "bench-internals")]
fn print_phase15_allocation_sample<T, R>(
    name: &str,
    inputs: Vec<T>,
    mut operation: impl FnMut(T) -> R,
) {
    let mut region = Region::new(GLOBAL);
    region.reset();
    for input in inputs {
        black_box(operation(input));
    }
    print_phase15_allocation_stats(name, region.change());
}

#[cfg(feature = "bench-internals")]
fn print_phase15_allocation_stats(name: &str, stats: Stats) {
    eprintln!(
        "{name}: allocation_iterations={} harness_input_clones_measured=false allocations={} \
         deallocations={} reallocations={} bytes_allocated={} bytes_deallocated={} \
         bytes_reallocated={}",
        PHASE15_ALLOCATION_ITERATIONS,
        stats.allocations,
        stats.deallocations,
        stats.reallocations,
        stats.bytes_allocated,
        stats.bytes_deallocated,
        stats.bytes_reallocated,
    );
}

#[cfg(feature = "bench-internals")]
fn object(namespace: &str, id: &str) -> Object {
    Object {
        namespace: namespace.to_string(),
        id: id.to_string(),
    }
}

#[cfg(feature = "bench-internals")]
fn relation(name: &str) -> Relation {
    Relation(name.to_string())
}

#[cfg(feature = "bench-internals")]
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
