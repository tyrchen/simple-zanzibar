//! Core evaluation logic for `check` and `expand` requests.

#[cfg(feature = "bench-internals")]
use std::cell::Cell;
#[cfg(feature = "bench-internals")]
use std::sync::atomic::{AtomicU64, Ordering};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    num::{NonZeroU32, NonZeroUsize},
};

use thiserror::Error;

use crate::{
    domain::{ObjectRef as DomainObjectRef, ObjectType, RelationName, SubjectId, SubjectType},
    error::ZanzibarError,
    model::{
        ExpandedUserset, LookupResources, LookupResourcesRequest, LookupSubjects,
        LookupSubjectsRequest, Object, Relation, User,
    },
    relationship::{QueryLimit, StoreCheckKey, SubjectFilter},
    revision::PublishedSnapshot,
    schema::{
        CompiledUsersetExpression, RelationDefinition as SchemaRelationDefinition, SchemaRelationId,
    },
};

const DEFAULT_MAX_DEPTH: u32 = 50;
const DEFAULT_MAX_FANOUT_PER_STEP: u32 = 1_000;
const DEFAULT_MAX_LOOKUP_RESULTS: u32 = 1_000;
const ACTIVE_INDEX_THRESHOLD: usize = 8;
const LOOKUP_PLANNER_SAMPLE_RELATIONSHIPS: u32 = 64;
const LOOKUP_PLANNER_MIN_PRUNE_BPS: u32 = 500;
#[cfg(feature = "bench-internals")]
thread_local! {
    static EVALUATION_READ_COUNTERS_ENABLED: Cell<bool> = const { Cell::new(false) };
}
#[cfg(feature = "bench-internals")]
static CHECK_EVALUATIONS: AtomicU64 = AtomicU64::new(0);
#[cfg(feature = "bench-internals")]
static CHECK_MEMO_HIT_OPPORTUNITIES: AtomicU64 = AtomicU64::new(0);
#[cfg(feature = "bench-internals")]
static CHECK_MEMO_HITS: AtomicU64 = AtomicU64::new(0);
#[cfg(feature = "bench-internals")]
static CHECK_MEMO_MISSES: AtomicU64 = AtomicU64::new(0);
#[cfg(feature = "bench-internals")]
static CHECK_MEMO_INSERTS: AtomicU64 = AtomicU64::new(0);
#[cfg(feature = "bench-internals")]
static CHECK_MEMO_CAPACITY_SKIPS: AtomicU64 = AtomicU64::new(0);
#[cfg(feature = "bench-internals")]
static CHECK_MEMO_ACTIVE_CYCLE_SKIPS: AtomicU64 = AtomicU64::new(0);
#[cfg(feature = "bench-internals")]
static CHECK_MEMO_DEPTH_INSUFFICIENT_SKIPS: AtomicU64 = AtomicU64::new(0);
#[cfg(feature = "bench-internals")]
static CHECK_COMPLETED_RESULTS: AtomicU64 = AtomicU64::new(0);
#[cfg(feature = "bench-internals")]
static CHECK_ACTIVE_CYCLE_DENIALS: AtomicU64 = AtomicU64::new(0);
#[cfg(feature = "bench-internals")]
static LOOKUP_RESOURCES_FRONTIER_SUBJECTS: AtomicU64 = AtomicU64::new(0);
#[cfg(feature = "bench-internals")]
static LOOKUP_RESOURCES_FRONTIER_RELATIONSHIPS: AtomicU64 = AtomicU64::new(0);
#[cfg(feature = "bench-internals")]
static LOOKUP_RESOURCES_CANDIDATE_RESOURCES: AtomicU64 = AtomicU64::new(0);
#[cfg(feature = "bench-internals")]
static LOOKUP_RESOURCES_SCHEMA_PRUNED: AtomicU64 = AtomicU64::new(0);
#[cfg(feature = "bench-internals")]
static LOOKUP_RESOURCES_PLANNER_FALLBACKS: AtomicU64 = AtomicU64::new(0);
#[cfg(feature = "bench-internals")]
static LOOKUP_RESOURCES_FULL_ROOT_CHECKS: AtomicU64 = AtomicU64::new(0);
#[cfg(feature = "bench-internals")]
static LOOKUP_RESOURCES_RESIDUAL_CHECKS: AtomicU64 = AtomicU64::new(0);
#[cfg(feature = "bench-internals")]
static LOOKUP_RESOURCES_PROVEN_WITHOUT_CHECK: AtomicU64 = AtomicU64::new(0);
#[cfg(feature = "bench-internals")]
static LOOKUP_RESOURCES_TUPLE_FALLBACKS: AtomicU64 = AtomicU64::new(0);
#[cfg(feature = "bench-internals")]
static LOOKUP_RESOURCES_RETURNED: AtomicU64 = AtomicU64::new(0);
#[cfg(feature = "bench-internals")]
static LOOKUP_RESOURCES_RESULT_LIMIT_EXITS: AtomicU64 = AtomicU64::new(0);
#[cfg(feature = "bench-internals")]
static LOOKUP_SUBJECTS_CANDIDATE_SUBJECTS: AtomicU64 = AtomicU64::new(0);
#[cfg(feature = "bench-internals")]
static LOOKUP_SUBJECTS_CANDIDATE_USERSETS: AtomicU64 = AtomicU64::new(0);
#[cfg(feature = "bench-internals")]
static LOOKUP_SUBJECTS_FULL_ROOT_CHECKS: AtomicU64 = AtomicU64::new(0);
#[cfg(feature = "bench-internals")]
static LOOKUP_SUBJECTS_RETURNED: AtomicU64 = AtomicU64::new(0);
#[cfg(feature = "bench-internals")]
static LOOKUP_SUBJECTS_RESULT_LIMIT_EXITS: AtomicU64 = AtomicU64::new(0);

/// Benchmark-only evaluator counters for read-optimization follow-up work.
#[cfg(feature = "bench-internals")]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct EvaluationReadCounters {
    /// Number of memo-eligible check evaluations that reached the non-active path.
    pub check_evaluations: u64,
    /// Number of completed check keys observed again inside the same request context.
    pub check_memo_hit_opportunities: u64,
    /// Number of request-local memo hits returned without re-evaluation.
    pub check_memo_hits: u64,
    /// Number of request-local memo misses that proceeded to evaluation.
    pub check_memo_misses: u64,
    /// Number of completed check results inserted into the request-local memo.
    pub check_memo_inserts: u64,
    /// Number of completed check results skipped because the request-local memo was full.
    pub check_memo_capacity_skips: u64,
    /// Number of active-cycle denials that intentionally bypassed request-local memo lookup.
    pub check_memo_active_cycle_skips: u64,
    /// Number of memo entries skipped because the caller had insufficient remaining depth.
    pub check_memo_depth_insufficient_skips: u64,
    /// Number of memo-eligible successful check results tracked for request-local memoization.
    pub check_completed_results: u64,
    /// Number of active-recursion cycle denials.
    pub check_active_cycle_denials: u64,
    /// Number of subject frontier entries popped by `lookup_resources`.
    pub lookup_resources_frontier_subjects: u64,
    /// Number of reverse relationships scanned by `lookup_resources`.
    pub lookup_resources_frontier_relationships: u64,
    /// Number of resource candidates sent to root verification.
    pub lookup_resources_candidate_resources: u64,
    /// Number of root-resource reverse relationships pruned by schema producer analysis.
    pub lookup_resources_schema_pruned: u64,
    /// Number of lookup-resource requests that fell back to the broad producer planner.
    pub lookup_resources_planner_fallbacks: u64,
    /// Number of full-root checks run by `lookup_resources`.
    pub lookup_resources_full_root_checks: u64,
    /// Number of residual guard checks run by `lookup_resources`.
    pub lookup_resources_residual_checks: u64,
    /// Number of exact-proof resources returned by `lookup_resources` without a root check.
    pub lookup_resources_proven_without_check: u64,
    /// Number of tuple-to-userset candidate paths intentionally verified by full-root fallback.
    pub lookup_resources_tuple_fallbacks: u64,
    /// Number of resources returned by `lookup_resources`.
    pub lookup_resources_returned: u64,
    /// Number of `lookup_resources` exits caused by `max_lookup_results`.
    pub lookup_resources_result_limit_exits: u64,
    /// Number of direct subject candidates seen by `lookup_subjects`.
    pub lookup_subjects_candidate_subjects: u64,
    /// Number of userset candidates seen by `lookup_subjects`.
    pub lookup_subjects_candidate_usersets: u64,
    /// Number of full-root checks run by `lookup_subjects`.
    pub lookup_subjects_full_root_checks: u64,
    /// Number of subjects returned by `lookup_subjects`.
    pub lookup_subjects_returned: u64,
    /// Number of `lookup_subjects` exits caused by `max_lookup_results`.
    pub lookup_subjects_result_limit_exits: u64,
}

/// Resets and enables benchmark-only evaluator counters.
#[cfg(feature = "bench-internals")]
pub fn reset_evaluation_read_counters() {
    set_evaluation_read_counters_enabled(true);
    CHECK_EVALUATIONS.store(0, Ordering::Relaxed);
    CHECK_MEMO_HIT_OPPORTUNITIES.store(0, Ordering::Relaxed);
    CHECK_MEMO_HITS.store(0, Ordering::Relaxed);
    CHECK_MEMO_MISSES.store(0, Ordering::Relaxed);
    CHECK_MEMO_INSERTS.store(0, Ordering::Relaxed);
    CHECK_MEMO_CAPACITY_SKIPS.store(0, Ordering::Relaxed);
    CHECK_MEMO_ACTIVE_CYCLE_SKIPS.store(0, Ordering::Relaxed);
    CHECK_MEMO_DEPTH_INSUFFICIENT_SKIPS.store(0, Ordering::Relaxed);
    CHECK_COMPLETED_RESULTS.store(0, Ordering::Relaxed);
    CHECK_ACTIVE_CYCLE_DENIALS.store(0, Ordering::Relaxed);
    LOOKUP_RESOURCES_FRONTIER_SUBJECTS.store(0, Ordering::Relaxed);
    LOOKUP_RESOURCES_FRONTIER_RELATIONSHIPS.store(0, Ordering::Relaxed);
    LOOKUP_RESOURCES_CANDIDATE_RESOURCES.store(0, Ordering::Relaxed);
    LOOKUP_RESOURCES_SCHEMA_PRUNED.store(0, Ordering::Relaxed);
    LOOKUP_RESOURCES_PLANNER_FALLBACKS.store(0, Ordering::Relaxed);
    LOOKUP_RESOURCES_FULL_ROOT_CHECKS.store(0, Ordering::Relaxed);
    LOOKUP_RESOURCES_RESIDUAL_CHECKS.store(0, Ordering::Relaxed);
    LOOKUP_RESOURCES_PROVEN_WITHOUT_CHECK.store(0, Ordering::Relaxed);
    LOOKUP_RESOURCES_TUPLE_FALLBACKS.store(0, Ordering::Relaxed);
    LOOKUP_RESOURCES_RETURNED.store(0, Ordering::Relaxed);
    LOOKUP_RESOURCES_RESULT_LIMIT_EXITS.store(0, Ordering::Relaxed);
    LOOKUP_SUBJECTS_CANDIDATE_SUBJECTS.store(0, Ordering::Relaxed);
    LOOKUP_SUBJECTS_CANDIDATE_USERSETS.store(0, Ordering::Relaxed);
    LOOKUP_SUBJECTS_FULL_ROOT_CHECKS.store(0, Ordering::Relaxed);
    LOOKUP_SUBJECTS_RETURNED.store(0, Ordering::Relaxed);
    LOOKUP_SUBJECTS_RESULT_LIMIT_EXITS.store(0, Ordering::Relaxed);
}

/// Enables or disables benchmark-only evaluator counters.
#[cfg(feature = "bench-internals")]
pub fn set_evaluation_read_counters_enabled(enabled: bool) {
    EVALUATION_READ_COUNTERS_ENABLED.with(|flag| flag.set(enabled));
}

#[cfg(feature = "bench-internals")]
fn evaluation_read_counters_enabled() -> bool {
    EVALUATION_READ_COUNTERS_ENABLED.with(Cell::get)
}

#[cfg(feature = "bench-internals")]
fn record_counter(counter: &AtomicU64) {
    if evaluation_read_counters_enabled() {
        counter.fetch_add(1, Ordering::Relaxed);
    }
}

/// Returns benchmark-only evaluator counters.
#[cfg(feature = "bench-internals")]
#[must_use]
pub fn evaluation_read_counters() -> EvaluationReadCounters {
    EvaluationReadCounters {
        check_evaluations: CHECK_EVALUATIONS.load(Ordering::Relaxed),
        check_memo_hit_opportunities: CHECK_MEMO_HIT_OPPORTUNITIES.load(Ordering::Relaxed),
        check_memo_hits: CHECK_MEMO_HITS.load(Ordering::Relaxed),
        check_memo_misses: CHECK_MEMO_MISSES.load(Ordering::Relaxed),
        check_memo_inserts: CHECK_MEMO_INSERTS.load(Ordering::Relaxed),
        check_memo_capacity_skips: CHECK_MEMO_CAPACITY_SKIPS.load(Ordering::Relaxed),
        check_memo_active_cycle_skips: CHECK_MEMO_ACTIVE_CYCLE_SKIPS.load(Ordering::Relaxed),
        check_memo_depth_insufficient_skips: CHECK_MEMO_DEPTH_INSUFFICIENT_SKIPS
            .load(Ordering::Relaxed),
        check_completed_results: CHECK_COMPLETED_RESULTS.load(Ordering::Relaxed),
        check_active_cycle_denials: CHECK_ACTIVE_CYCLE_DENIALS.load(Ordering::Relaxed),
        lookup_resources_frontier_subjects: LOOKUP_RESOURCES_FRONTIER_SUBJECTS
            .load(Ordering::Relaxed),
        lookup_resources_frontier_relationships: LOOKUP_RESOURCES_FRONTIER_RELATIONSHIPS
            .load(Ordering::Relaxed),
        lookup_resources_candidate_resources: LOOKUP_RESOURCES_CANDIDATE_RESOURCES
            .load(Ordering::Relaxed),
        lookup_resources_schema_pruned: LOOKUP_RESOURCES_SCHEMA_PRUNED.load(Ordering::Relaxed),
        lookup_resources_planner_fallbacks: LOOKUP_RESOURCES_PLANNER_FALLBACKS
            .load(Ordering::Relaxed),
        lookup_resources_full_root_checks: LOOKUP_RESOURCES_FULL_ROOT_CHECKS
            .load(Ordering::Relaxed),
        lookup_resources_residual_checks: LOOKUP_RESOURCES_RESIDUAL_CHECKS.load(Ordering::Relaxed),
        lookup_resources_proven_without_check: LOOKUP_RESOURCES_PROVEN_WITHOUT_CHECK
            .load(Ordering::Relaxed),
        lookup_resources_tuple_fallbacks: LOOKUP_RESOURCES_TUPLE_FALLBACKS.load(Ordering::Relaxed),
        lookup_resources_returned: LOOKUP_RESOURCES_RETURNED.load(Ordering::Relaxed),
        lookup_resources_result_limit_exits: LOOKUP_RESOURCES_RESULT_LIMIT_EXITS
            .load(Ordering::Relaxed),
        lookup_subjects_candidate_subjects: LOOKUP_SUBJECTS_CANDIDATE_SUBJECTS
            .load(Ordering::Relaxed),
        lookup_subjects_candidate_usersets: LOOKUP_SUBJECTS_CANDIDATE_USERSETS
            .load(Ordering::Relaxed),
        lookup_subjects_full_root_checks: LOOKUP_SUBJECTS_FULL_ROOT_CHECKS.load(Ordering::Relaxed),
        lookup_subjects_returned: LOOKUP_SUBJECTS_RETURNED.load(Ordering::Relaxed),
        lookup_subjects_result_limit_exits: LOOKUP_SUBJECTS_RESULT_LIMIT_EXITS
            .load(Ordering::Relaxed),
    }
}

#[cfg(feature = "bench-internals")]
fn record_active_cycle_denial() {
    record_counter(&CHECK_ACTIVE_CYCLE_DENIALS);
}

#[cfg(not(feature = "bench-internals"))]
fn record_active_cycle_denial() {}

#[cfg(feature = "bench-internals")]
fn record_check_memo_hit() {
    record_counter(&CHECK_MEMO_HITS);
}

#[cfg(not(feature = "bench-internals"))]
fn record_check_memo_hit() {}

#[cfg(feature = "bench-internals")]
fn record_check_memo_miss() {
    record_counter(&CHECK_MEMO_MISSES);
}

#[cfg(not(feature = "bench-internals"))]
fn record_check_memo_miss() {}

#[cfg(feature = "bench-internals")]
fn record_check_memo_insert() {
    record_counter(&CHECK_MEMO_INSERTS);
}

#[cfg(not(feature = "bench-internals"))]
fn record_check_memo_insert() {}

#[cfg(feature = "bench-internals")]
fn record_check_memo_capacity_skip() {
    record_counter(&CHECK_MEMO_CAPACITY_SKIPS);
}

#[cfg(not(feature = "bench-internals"))]
fn record_check_memo_capacity_skip() {}

#[cfg(feature = "bench-internals")]
fn record_check_memo_active_cycle_skip() {
    record_counter(&CHECK_MEMO_ACTIVE_CYCLE_SKIPS);
}

#[cfg(not(feature = "bench-internals"))]
fn record_check_memo_active_cycle_skip() {}

#[cfg(feature = "bench-internals")]
fn record_check_memo_depth_insufficient_skip() {
    record_counter(&CHECK_MEMO_DEPTH_INSUFFICIENT_SKIPS);
}

#[cfg(not(feature = "bench-internals"))]
fn record_check_memo_depth_insufficient_skip() {}

#[cfg(feature = "bench-internals")]
fn record_lookup_resources_frontier_subject() {
    record_counter(&LOOKUP_RESOURCES_FRONTIER_SUBJECTS);
}

#[cfg(not(feature = "bench-internals"))]
fn record_lookup_resources_frontier_subject() {}

#[cfg(feature = "bench-internals")]
fn record_lookup_resources_frontier_relationship() {
    record_counter(&LOOKUP_RESOURCES_FRONTIER_RELATIONSHIPS);
}

#[cfg(not(feature = "bench-internals"))]
fn record_lookup_resources_frontier_relationship() {}

#[cfg(feature = "bench-internals")]
fn record_lookup_resources_candidate_resource() {
    record_counter(&LOOKUP_RESOURCES_CANDIDATE_RESOURCES);
}

#[cfg(not(feature = "bench-internals"))]
fn record_lookup_resources_candidate_resource() {}

#[cfg(feature = "bench-internals")]
fn record_lookup_resources_schema_pruned() {
    record_counter(&LOOKUP_RESOURCES_SCHEMA_PRUNED);
}

#[cfg(not(feature = "bench-internals"))]
fn record_lookup_resources_schema_pruned() {}

#[cfg(feature = "bench-internals")]
fn record_lookup_resources_planner_fallback() {
    record_counter(&LOOKUP_RESOURCES_PLANNER_FALLBACKS);
}

#[cfg(not(feature = "bench-internals"))]
fn record_lookup_resources_planner_fallback() {}

#[cfg(feature = "bench-internals")]
fn record_lookup_resources_full_root_check() {
    record_counter(&LOOKUP_RESOURCES_FULL_ROOT_CHECKS);
}

#[cfg(not(feature = "bench-internals"))]
fn record_lookup_resources_full_root_check() {}

#[cfg(feature = "bench-internals")]
fn record_lookup_resources_residual_check() {
    record_counter(&LOOKUP_RESOURCES_RESIDUAL_CHECKS);
}

#[cfg(not(feature = "bench-internals"))]
fn record_lookup_resources_residual_check() {}

#[cfg(feature = "bench-internals")]
fn record_lookup_resources_proven_without_check() {
    record_counter(&LOOKUP_RESOURCES_PROVEN_WITHOUT_CHECK);
}

#[cfg(not(feature = "bench-internals"))]
fn record_lookup_resources_proven_without_check() {}

#[cfg(feature = "bench-internals")]
fn record_lookup_resources_tuple_fallback() {
    record_counter(&LOOKUP_RESOURCES_TUPLE_FALLBACKS);
}

#[cfg(not(feature = "bench-internals"))]
fn record_lookup_resources_tuple_fallback() {}

#[cfg(feature = "bench-internals")]
fn record_lookup_resources_returned() {
    record_counter(&LOOKUP_RESOURCES_RETURNED);
}

#[cfg(not(feature = "bench-internals"))]
fn record_lookup_resources_returned() {}

#[cfg(feature = "bench-internals")]
fn record_lookup_resources_result_limit_exit() {
    record_counter(&LOOKUP_RESOURCES_RESULT_LIMIT_EXITS);
}

#[cfg(not(feature = "bench-internals"))]
fn record_lookup_resources_result_limit_exit() {}

#[cfg(feature = "bench-internals")]
fn record_lookup_subjects_candidate_subject() {
    record_counter(&LOOKUP_SUBJECTS_CANDIDATE_SUBJECTS);
}

#[cfg(not(feature = "bench-internals"))]
fn record_lookup_subjects_candidate_subject() {}

#[cfg(feature = "bench-internals")]
fn record_lookup_subjects_candidate_userset() {
    record_counter(&LOOKUP_SUBJECTS_CANDIDATE_USERSETS);
}

#[cfg(not(feature = "bench-internals"))]
fn record_lookup_subjects_candidate_userset() {}

#[cfg(feature = "bench-internals")]
fn record_lookup_subjects_full_root_check() {
    record_counter(&LOOKUP_SUBJECTS_FULL_ROOT_CHECKS);
}

#[cfg(not(feature = "bench-internals"))]
fn record_lookup_subjects_full_root_check() {}

#[cfg(feature = "bench-internals")]
fn record_lookup_subjects_returned() {
    record_counter(&LOOKUP_SUBJECTS_RETURNED);
}

#[cfg(not(feature = "bench-internals"))]
fn record_lookup_subjects_returned() {}

#[cfg(feature = "bench-internals")]
fn record_lookup_subjects_result_limit_exit() {
    record_counter(&LOOKUP_SUBJECTS_RESULT_LIMIT_EXITS);
}

#[cfg(not(feature = "bench-internals"))]
fn record_lookup_subjects_result_limit_exit() {}

/// Errors produced by the shared evaluation engine.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum EvaluationError {
    /// A recursive evaluation exceeded the configured depth limit.
    #[error("evaluation depth exceeded at {key:?}")]
    DepthExceeded {
        /// Evaluation key that exceeded the limit.
        key: Box<EvaluationKey>,
    },

    /// A single evaluator step exceeded the configured fanout limit.
    #[error("evaluation fanout exceeded limit {limit}")]
    FanoutExceeded {
        /// Configured fanout limit.
        limit: NonZeroU32,
    },
}

/// Immutable key for evaluator depth errors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EvaluationKey {
    /// A check key.
    Check(CheckKey),
    /// An expand key.
    Expand(ExpandKey),
}

/// Immutable key for recursion tracking.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum CheckKey {
    /// Store-native identifier key for relationships already interned in the compact snapshot.
    Store(StoreCheckKey),
    /// Validated public object/relation-name key used when a segment-native compact key is not
    /// available, such as checkpoint-plus-delta views.
    PublicName {
        /// Object being checked.
        object: Object,
        /// Validated relation or permission name being checked.
        relation: RelationName,
        /// Subject being checked.
        user: User,
    },
    /// Owned public-model fallback when at least one identifier is absent from the snapshot.
    Public {
        /// Object being checked.
        object: Object,
        /// Relation or permission being checked.
        relation: Relation,
        /// Subject being checked.
        user: User,
    },
}

impl CheckKey {
    fn new(
        snapshot: &PublishedSnapshot,
        object: &Object,
        relation: &Relation,
        user: &User,
    ) -> Self {
        snapshot
            .relationships()
            .store_check_key(object, relation, user)
            .map_or_else(
                || Self::Public {
                    object: object.clone(),
                    relation: relation.clone(),
                    user: user.clone(),
                },
                Self::Store,
            )
    }

    fn from_relation_name(
        snapshot: &PublishedSnapshot,
        object: &Object,
        relation_name: &RelationName,
        user: &User,
    ) -> Self {
        snapshot
            .relationships()
            .store_check_key_for_relation_name(object, relation_name, user)
            .map_or_else(
                || Self::PublicName {
                    object: object.clone(),
                    relation: relation_name.clone(),
                    user: user.clone(),
                },
                Self::Store,
            )
    }
}

/// Immutable key for expand recursion tracking.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ExpandKey {
    object: Object,
    relation: RelationName,
}

impl ExpandKey {
    fn new(object: &Object, relation: &RelationName) -> Self {
        Self {
            object: object.clone(),
            relation: relation.clone(),
        }
    }
}

/// Evaluator resource limits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EvaluationLimits {
    /// Maximum recursive check depth.
    pub max_depth: NonZeroU32,
    /// Maximum userset fanout per evaluator step.
    pub max_fanout_per_step: NonZeroU32,
    /// Maximum lookup results for lookup APIs.
    pub max_lookup_results: NonZeroU32,
}

impl Default for EvaluationLimits {
    fn default() -> Self {
        Self {
            max_depth: non_zero_u32(DEFAULT_MAX_DEPTH),
            max_fanout_per_step: non_zero_u32(DEFAULT_MAX_FANOUT_PER_STEP),
            max_lookup_results: non_zero_u32(DEFAULT_MAX_LOOKUP_RESULTS),
        }
    }
}

/// Membership algebra result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Membership {
    /// The subject is a member.
    Allowed,
    /// The subject is not a member.
    Denied,
    /// Reserved for future caveat/condition support.
    Conditional,
}

impl Membership {
    /// Returns true when membership is allowed.
    #[must_use]
    pub const fn is_allowed(self) -> bool {
        matches!(self, Self::Allowed)
    }

    /// Returns the union of two membership results.
    #[must_use]
    pub const fn union(self, other: Self) -> Self {
        match (self, other) {
            (Self::Allowed, _) | (_, Self::Allowed) => Self::Allowed,
            (Self::Conditional, _) | (_, Self::Conditional) => Self::Conditional,
            (Self::Denied, Self::Denied) => Self::Denied,
        }
    }

    /// Returns the intersection of two membership results.
    #[must_use]
    pub const fn intersection(self, other: Self) -> Self {
        match (self, other) {
            (Self::Denied, _) | (_, Self::Denied) => Self::Denied,
            (Self::Conditional, _) | (_, Self::Conditional) => Self::Conditional,
            (Self::Allowed, Self::Allowed) => Self::Allowed,
        }
    }

    /// Returns membership for `self - other`.
    #[must_use]
    pub const fn exclusion(self, other: Self) -> Self {
        match (self, other) {
            (Self::Denied, _) | (_, Self::Allowed) => Self::Denied,
            (Self::Conditional, _) | (_, Self::Conditional) => Self::Conditional,
            (Self::Allowed, Self::Denied) => Self::Allowed,
        }
    }

    const fn is_memo_cacheable(self) -> bool {
        matches!(self, Self::Allowed | Self::Denied)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum CheckMemoKey {
    Store(StoreCheckKey),
    Public {
        object: Object,
        relation: RelationName,
        user: User,
    },
}

impl CheckMemoKey {
    fn from_check_key(key: &CheckKey) -> Result<Self, ZanzibarError> {
        match key {
            CheckKey::Store(key) => Ok(Self::Store(*key)),
            CheckKey::PublicName {
                object,
                relation,
                user,
            } => Ok(Self::Public {
                object: object.clone(),
                relation: relation.clone(),
                user: user.clone(),
            }),
            CheckKey::Public {
                object,
                relation,
                user,
            } => Ok(Self::Public {
                object: object.clone(),
                relation: RelationName::try_from(relation)?,
                user: user.clone(),
            }),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CheckMemoEntry {
    membership: Membership,
    depth_required: NonZeroU32,
}

#[derive(Debug)]
struct RequestCheckMemo {
    entries: HashMap<CheckMemoKey, CheckMemoEntry>,
    capacity: usize,
}

impl RequestCheckMemo {
    fn new(limits: EvaluationLimits) -> Self {
        Self {
            entries: HashMap::new(),
            capacity: memo_capacity(limits),
        }
    }

    fn get(&self, key: &CheckMemoKey, remaining_depth: u32) -> MemoLookup {
        match self.entries.get(key) {
            Some(entry) if entry.depth_required.get() <= remaining_depth => {
                MemoLookup::Hit(entry.membership)
            }
            Some(_) => MemoLookup::DepthInsufficient,
            None => MemoLookup::Miss,
        }
    }

    fn insert(&mut self, key: CheckMemoKey, membership: Membership, depth_required: NonZeroU32) {
        if let Some(entry) = self.entries.get_mut(&key) {
            if depth_required.get() <= entry.depth_required.get() {
                *entry = CheckMemoEntry {
                    membership,
                    depth_required,
                };
            }
            return;
        }
        if self.entries.len() >= self.capacity {
            record_check_memo_capacity_skip();
            return;
        }
        self.entries.insert(
            key,
            CheckMemoEntry {
                membership,
                depth_required,
            },
        );
        record_check_memo_insert();
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MemoLookup {
    Hit(Membership),
    Miss,
    DepthInsufficient,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CheckFrame {
    entry_remaining_depth: u32,
    minimum_remaining_depth: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct LookupProducerPlan {
    relations: Vec<RelationName>,
    verification_rules: Vec<LookupVerificationRule>,
}

impl LookupProducerPlan {
    fn allows_relationship(&self, relationship: crate::relationship::RelationshipRef<'_>) -> bool {
        self.relations
            .iter()
            .any(|relation| relationship.relation_name_eq(relation))
    }

    fn verification_for_relationship(
        &self,
        relationship: crate::relationship::RelationshipRef<'_>,
        proof: LookupSubjectProof,
        limits: EvaluationLimits,
    ) -> LookupCandidateVerification {
        if !proof.is_exact() || proof.direct_depth >= limits.max_depth.get() {
            return LookupCandidateVerification::FullRoot;
        }
        self.verification_rules
            .iter()
            .find(|rule| relationship.relation_name_eq(&rule.relation))
            .map_or(LookupCandidateVerification::FullRoot, |rule| {
                rule.verification.clone()
            })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct LookupVerificationRule {
    relation: RelationName,
    verification: LookupCandidateVerification,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum LookupCandidateVerification {
    FullRoot,
    ProvenWithoutCheck,
    Residual(Vec<LookupResidualCheck>),
}

impl LookupCandidateVerification {
    fn with_required_allowed(
        self,
        relation_context: RelationName,
        expression: CompiledUsersetExpression,
    ) -> Self {
        self.with_residual(LookupResidualCheck {
            relation_context,
            expression,
            expected: LookupResidualExpected::Allowed,
        })
    }

    fn with_required_denied(
        self,
        relation_context: RelationName,
        expression: CompiledUsersetExpression,
    ) -> Self {
        self.with_residual(LookupResidualCheck {
            relation_context,
            expression,
            expected: LookupResidualExpected::Denied,
        })
    }

    fn with_residual(self, residual: LookupResidualCheck) -> Self {
        match self {
            Self::FullRoot => Self::FullRoot,
            Self::ProvenWithoutCheck => Self::Residual(vec![residual]),
            Self::Residual(mut residuals) => {
                residuals.push(residual);
                Self::Residual(residuals)
            }
        }
    }

    fn union_merge(self, other: Self) -> Self {
        match (self, other) {
            (Self::ProvenWithoutCheck, _) | (_, Self::ProvenWithoutCheck) => {
                Self::ProvenWithoutCheck
            }
            (Self::Residual(left), Self::Residual(right)) if left == right => Self::Residual(left),
            _ => Self::FullRoot,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct LookupResidualCheck {
    relation_context: RelationName,
    expression: CompiledUsersetExpression,
    expected: LookupResidualExpected,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LookupResidualExpected {
    Allowed,
    Denied,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct LookupSubjectProof {
    direct_depth: u32,
    exact: bool,
}

impl LookupSubjectProof {
    const fn exact_root() -> Self {
        Self {
            direct_depth: 0,
            exact: true,
        }
    }

    const fn inexact() -> Self {
        Self {
            direct_depth: 0,
            exact: false,
        }
    }

    const fn is_exact(self) -> bool {
        self.exact
    }

    fn next_exact(self) -> Self {
        if self.exact {
            Self {
                direct_depth: self.direct_depth.saturating_add(1),
                exact: true,
            }
        } else {
            Self::inexact()
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct LookupFrontierEntry {
    subject: User,
    proof: LookupSubjectProof,
}

impl LookupFrontierEntry {
    fn new(subject: User, proof: LookupSubjectProof) -> Self {
        Self { subject, proof }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct LookupProducerRuntime {
    enabled: bool,
    sampled_relationships: u32,
    pruned_relationships: u32,
}

impl LookupProducerRuntime {
    const fn new(enabled: bool) -> Self {
        Self {
            enabled,
            sampled_relationships: 0,
            pruned_relationships: 0,
        }
    }

    fn should_prune(
        &mut self,
        plan: &LookupProducerPlan,
        relationship: crate::relationship::RelationshipRef<'_>,
    ) -> bool {
        if !self.enabled {
            return false;
        }
        let should_prune = !plan.allows_relationship(relationship);
        self.record_sample(should_prune);
        should_prune
    }

    fn record_sample(&mut self, should_prune: bool) {
        if self.sampled_relationships >= LOOKUP_PLANNER_SAMPLE_RELATIONSHIPS {
            return;
        }
        self.sampled_relationships = self.sampled_relationships.saturating_add(1);
        if should_prune {
            self.pruned_relationships = self.pruned_relationships.saturating_add(1);
        }
        if self.sampled_relationships == LOOKUP_PLANNER_SAMPLE_RELATIONSHIPS
            && self.pruned_relationships.saturating_mul(10_000)
                < LOOKUP_PLANNER_MIN_PRUNE_BPS.saturating_mul(self.sampled_relationships)
        {
            self.enabled = false;
        }
    }
}

#[derive(Debug)]
struct SameObjectRelationExpansion {
    object_type: ObjectType,
    source_relation: RelationName,
    target_relations: Vec<RelationName>,
}

#[derive(Debug)]
struct TupleToUsersetRelationExpansion {
    object_type: ObjectType,
    source_relation: RelationName,
    targets: Vec<TupleToUsersetExpansionTarget>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TupleToUsersetExpansionTarget {
    resource_type: ObjectType,
    tupleset_relation: RelationName,
    computed_relation: RelationName,
    target_relation: RelationName,
}

/// Evaluation context over one immutable snapshot.
#[derive(Debug)]
pub struct EvaluationContext<'a> {
    snapshot: &'a PublishedSnapshot,
    limits: EvaluationLimits,
    generation: NonZeroU32,
    use_active_indexes: bool,
    remaining_depth: u32,
    active_checks: HashMap<CheckKey, VisitMark>,
    active_expands: HashMap<ExpandKey, VisitMark>,
    check_stack: Vec<CheckKey>,
    expand_stack: Vec<ExpandKey>,
    check_frames: Vec<CheckFrame>,
    check_memo: Option<RequestCheckMemo>,
    #[cfg(feature = "bench-internals")]
    completed_check_keys: HashSet<CheckKey>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct VisitMark {
    generation: NonZeroU32,
    active: bool,
}

impl<'a> EvaluationContext<'a> {
    /// Creates an evaluation context for a snapshot.
    #[must_use]
    pub fn new(snapshot: &'a PublishedSnapshot, limits: EvaluationLimits) -> Self {
        Self {
            snapshot,
            limits,
            generation: NonZeroU32::MIN,
            use_active_indexes: false,
            remaining_depth: limits.max_depth.get(),
            active_checks: HashMap::new(),
            active_expands: HashMap::new(),
            check_stack: Vec::new(),
            expand_stack: Vec::new(),
            check_frames: Vec::new(),
            check_memo: None,
            #[cfg(feature = "bench-internals")]
            completed_check_keys: HashSet::new(),
        }
    }

    #[must_use]
    pub(crate) fn new_with_request_memo(
        snapshot: &'a PublishedSnapshot,
        limits: EvaluationLimits,
    ) -> Self {
        let mut context = Self::new(snapshot, limits);
        context.check_memo = Some(RequestCheckMemo::new(limits));
        context
    }

    pub(crate) fn reset_for_reuse(&mut self) {
        self.remaining_depth = self.limits.max_depth.get();
        self.use_active_indexes = true;
        self.check_stack.clear();
        self.expand_stack.clear();
        self.check_frames.clear();
        if let Some(generation) = self
            .generation
            .get()
            .checked_add(1)
            .and_then(NonZeroU32::new)
        {
            self.generation = generation;
        } else {
            self.generation = NonZeroU32::MIN;
            self.active_checks.clear();
            self.active_expands.clear();
        }
    }

    #[cfg(feature = "bench-internals")]
    fn record_check_started(&self, key: &CheckKey, memo_eligible: bool) {
        if !memo_eligible || !evaluation_read_counters_enabled() {
            return;
        }
        CHECK_EVALUATIONS.fetch_add(1, Ordering::Relaxed);
        if self.completed_check_keys.contains(key) {
            CHECK_MEMO_HIT_OPPORTUNITIES.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[cfg(feature = "bench-internals")]
    fn record_check_completed(
        &mut self,
        key: &CheckKey,
        result: &Result<Membership, ZanzibarError>,
        memo_eligible: bool,
    ) {
        if !memo_eligible || !evaluation_read_counters_enabled() {
            return;
        }
        if result.is_ok() && self.completed_check_keys.insert(key.clone()) {
            CHECK_COMPLETED_RESULTS.fetch_add(1, Ordering::Relaxed);
        }
    }

    fn evaluate_check_key(
        &mut self,
        key: &CheckKey,
        evaluate: impl FnOnce(&mut Self) -> Result<Membership, ZanzibarError>,
    ) -> Result<Membership, ZanzibarError> {
        if self.is_check_active(key) {
            record_active_cycle_denial();
            if self.check_memo.is_some() {
                record_check_memo_active_cycle_skip();
            }
            return Ok(Membership::Denied);
        }
        let memo_eligible = self.check_memo.is_some() && !self.check_stack.is_empty();
        #[cfg(feature = "bench-internals")]
        self.record_check_started(key, memo_eligible);
        if memo_eligible && let Some(membership) = self.memo_lookup(key)? {
            return Ok(membership);
        }

        let entry_remaining_depth = self.remaining_depth;
        self.enter(EvaluationKey::Check(key.clone()))?;
        if memo_eligible {
            self.push_check_frame(entry_remaining_depth);
        }
        self.push_check_key(key.clone());

        let result = evaluate(self);
        self.pop_check_key();
        let depth_required = if memo_eligible {
            self.pop_check_frame_depth_required()
        } else {
            NonZeroU32::MIN
        };
        self.leave();
        if memo_eligible
            && let Ok(membership) = &result
            && membership.is_memo_cacheable()
        {
            self.memo_insert(key, *membership, depth_required);
        }
        #[cfg(feature = "bench-internals")]
        self.record_check_completed(key, &result, memo_eligible);
        result
    }

    fn memo_lookup(&self, key: &CheckKey) -> Result<Option<Membership>, ZanzibarError> {
        let Some(memo) = &self.check_memo else {
            return Ok(None);
        };
        let memo_key = CheckMemoKey::from_check_key(key)?;
        match memo.get(&memo_key, self.remaining_depth) {
            MemoLookup::Hit(membership) => {
                record_check_memo_hit();
                Ok(Some(membership))
            }
            MemoLookup::Miss => {
                record_check_memo_miss();
                Ok(None)
            }
            MemoLookup::DepthInsufficient => {
                record_check_memo_depth_insufficient_skip();
                Ok(None)
            }
        }
    }

    fn memo_insert(&mut self, key: &CheckKey, membership: Membership, depth_required: NonZeroU32) {
        let Some(memo) = &mut self.check_memo else {
            return;
        };
        if let Ok(memo_key) = CheckMemoKey::from_check_key(key) {
            memo.insert(memo_key, membership, depth_required);
        }
    }

    /// Evaluates a check request and returns membership algebra.
    ///
    /// # Errors
    ///
    /// Returns [`ZanzibarError`] when validation, store access, or evaluator limits fail.
    pub fn check(
        &mut self,
        object: &Object,
        relation: &Relation,
        user: &User,
    ) -> Result<Membership, ZanzibarError> {
        let key = CheckKey::new(self.snapshot, object, relation, user);
        self.evaluate_check_key(&key, |context| {
            context.check_entered(object, relation, user)
        })
    }

    pub(crate) fn check_prepared(
        &mut self,
        object: &Object,
        relation: &Relation,
        user: &User,
        relation_definition: &SchemaRelationDefinition,
    ) -> Result<Membership, ZanzibarError> {
        let key = CheckKey::new(self.snapshot, object, relation, user);
        self.evaluate_check_key(&key, |context| {
            match relation_definition.compiled_userset_rewrite() {
                Some(expression) => context.eval_compiled_schema_expression(
                    object,
                    relation_definition.name(),
                    user,
                    expression,
                ),
                None => context.eval_this(object, relation_definition.name(), user),
            }
        })
    }

    fn verify_lookup_residuals(
        &mut self,
        object: &Object,
        root_relation: &RelationName,
        user: &User,
        residuals: &[LookupResidualCheck],
    ) -> Result<Membership, ZanzibarError> {
        let key = CheckKey::from_relation_name(self.snapshot, object, root_relation, user);
        self.evaluate_check_key(&key, |context| {
            context.verify_lookup_residuals_entered(object, user, residuals)
        })
    }

    fn verify_lookup_residuals_entered(
        &mut self,
        object: &Object,
        user: &User,
        residuals: &[LookupResidualCheck],
    ) -> Result<Membership, ZanzibarError> {
        for residual in residuals {
            record_lookup_resources_residual_check();
            let result = self.eval_compiled_schema_expression(
                object,
                &residual.relation_context,
                user,
                &residual.expression,
            )?;
            match residual.expected {
                LookupResidualExpected::Allowed if result != Membership::Allowed => {
                    return Ok(Membership::Denied);
                }
                LookupResidualExpected::Denied if result == Membership::Allowed => {
                    return Ok(Membership::Denied);
                }
                LookupResidualExpected::Allowed | LookupResidualExpected::Denied => {}
            }
        }
        Ok(Membership::Allowed)
    }

    fn check_relation_name(
        &mut self,
        object: &Object,
        relation_name: &RelationName,
        user: &User,
    ) -> Result<Membership, ZanzibarError> {
        let key = CheckKey::from_relation_name(self.snapshot, object, relation_name, user);
        self.evaluate_check_key(&key, |context| {
            context.check_relation_name_entered(object, relation_name, user)
        })
    }

    fn check_relation_id(
        &mut self,
        object: &Object,
        relation_name: &RelationName,
        relation_id: SchemaRelationId,
        user: &User,
    ) -> Result<Membership, ZanzibarError> {
        let key = CheckKey::from_relation_name(self.snapshot, object, relation_name, user);
        self.evaluate_check_key(&key, |context| {
            context.check_relation_id_entered(object, relation_name, relation_id, user)
        })
    }

    /// Expands a userset using the same snapshot and recursion limits.
    ///
    /// # Errors
    ///
    /// Returns [`ZanzibarError`] when validation, store access, or evaluator limits fail.
    pub fn expand(
        &mut self,
        object: &Object,
        relation: &Relation,
    ) -> Result<ExpandedUserset, ZanzibarError> {
        let relation_name = RelationName::try_from(relation)?;
        self.expand_relation_name(object, &relation_name)
    }

    fn expand_relation_name(
        &mut self,
        object: &Object,
        relation_name: &RelationName,
    ) -> Result<ExpandedUserset, ZanzibarError> {
        let key = ExpandKey::new(object, relation_name);
        if self.is_expand_active(&key) {
            return Ok(ExpandedUserset::Union(Vec::new()));
        }
        self.enter(EvaluationKey::Expand(key.clone()))?;
        self.push_expand_key(key);

        let result = self.expand_relation_name_entered(object, relation_name);
        self.pop_expand_key();
        self.leave();
        result
    }

    fn expand_relation_name_entered(
        &mut self,
        object: &Object,
        relation_name: &RelationName,
    ) -> Result<ExpandedUserset, ZanzibarError> {
        let object_type = ObjectType::try_from(object.namespace.as_str())?;
        let relation_definition = self
            .snapshot
            .schema()
            .resolver()
            .relation(&object_type, relation_name)?;
        match relation_definition.compiled_userset_rewrite() {
            Some(expression) => {
                self.expand_compiled_schema_expression(object, relation_name, expression)
            }
            None => self.expand_this(object, relation_name),
        }
    }

    fn check_entered(
        &mut self,
        object: &Object,
        relation: &Relation,
        user: &User,
    ) -> Result<Membership, ZanzibarError> {
        let object_type = ObjectType::try_from(object.namespace.as_str())?;
        let relation_name = RelationName::try_from(relation)?;
        self.check_relation_name_entered_with_type(object, &object_type, &relation_name, user)
    }

    fn check_relation_name_entered(
        &mut self,
        object: &Object,
        relation_name: &RelationName,
        user: &User,
    ) -> Result<Membership, ZanzibarError> {
        let object_type = ObjectType::try_from(object.namespace.as_str())?;
        self.check_relation_name_entered_with_type(object, &object_type, relation_name, user)
    }

    fn check_relation_name_entered_with_type(
        &mut self,
        object: &Object,
        object_type: &ObjectType,
        relation_name: &RelationName,
        user: &User,
    ) -> Result<Membership, ZanzibarError> {
        let relation_definition = self
            .snapshot
            .schema()
            .resolver()
            .relation(object_type, relation_name)?;
        match relation_definition.compiled_userset_rewrite() {
            Some(expression) => {
                self.eval_compiled_schema_expression(object, relation_name, user, expression)
            }
            None => self.eval_this(object, relation_name, user),
        }
    }

    fn check_relation_id_entered(
        &mut self,
        object: &Object,
        relation_name: &RelationName,
        relation_id: SchemaRelationId,
        user: &User,
    ) -> Result<Membership, ZanzibarError> {
        let relation_definition = self
            .snapshot
            .schema()
            .resolver()
            .relation_by_id(relation_id)
            .ok_or_else(compiled_schema_invariant_error)?;
        match relation_definition.compiled_userset_rewrite() {
            Some(expression) => {
                self.eval_compiled_schema_expression(object, relation_name, user, expression)
            }
            None => self.eval_this(object, relation_name, user),
        }
    }

    fn eval_compiled_schema_expression(
        &mut self,
        object: &Object,
        relation_name: &RelationName,
        user: &User,
        expression: &CompiledUsersetExpression,
    ) -> Result<Membership, ZanzibarError> {
        match expression {
            CompiledUsersetExpression::This => self.eval_this(object, relation_name, user),
            CompiledUsersetExpression::ComputedUserset {
                relation,
                relation_id,
                target_has_rewrite,
            } => {
                if !target_has_rewrite {
                    return self.eval_plain_computed_userset(object, relation, user);
                }
                self.check_relation_id(object, relation, *relation_id, user)
            }
            CompiledUsersetExpression::TupleToUserset {
                tupleset_relation: _,
                tupleset_relation_id,
                computed_userset_relation,
            } => {
                let tupleset_relation = self
                    .snapshot
                    .schema()
                    .resolver()
                    .relation_by_id(*tupleset_relation_id)
                    .ok_or_else(compiled_schema_invariant_error)?
                    .name()
                    .clone();
                self.eval_tuple_to_userset(
                    object,
                    user,
                    &tupleset_relation,
                    computed_userset_relation,
                )
            }
            CompiledUsersetExpression::Union(expressions) => {
                self.eval_compiled_schema_union(object, relation_name, user, expressions)
            }
            CompiledUsersetExpression::Intersection(expressions) => {
                self.eval_compiled_schema_intersection(object, relation_name, user, expressions)
            }
            CompiledUsersetExpression::Exclusion { base, exclude } => {
                self.eval_compiled_schema_exclusion(object, relation_name, user, base, exclude)
            }
        }
    }

    fn eval_plain_computed_userset(
        &mut self,
        object: &Object,
        relation_name: &RelationName,
        user: &User,
    ) -> Result<Membership, ZanzibarError> {
        let key = CheckKey::from_relation_name(self.snapshot, object, relation_name, user);
        self.evaluate_check_key(&key, |context| {
            context.eval_this(object, relation_name, user)
        })
    }

    fn eval_this(
        &mut self,
        object: &Object,
        relation_name: &RelationName,
        user: &User,
    ) -> Result<Membership, ZanzibarError> {
        let resource = DomainObjectRef::try_from(object)?;
        let subject = SubjectFilter::try_from(user)?;
        if self.snapshot.relationships().any_resource_relation_subject(
            &resource,
            relation_name,
            &subject,
        ) {
            return Ok(Membership::Allowed);
        }

        let mut fanout = 0_u32;
        for relationship in self.snapshot.relationships().resource_relation(
            &resource,
            relation_name,
            unbounded_query_limit(),
        ) {
            if let Some((nested_object, nested_relation)) =
                relationship.subject_userset_relation_name()?
            {
                self.increment_fanout(&mut fanout)?;
                if self
                    .check_relation_name(&nested_object, &nested_relation, user)?
                    .is_allowed()
                {
                    return Ok(Membership::Allowed);
                }
            }
        }

        Ok(Membership::Denied)
    }

    fn eval_tuple_to_userset(
        &mut self,
        object: &Object,
        user: &User,
        tupleset_relation: &RelationName,
        computed_userset_relation: &RelationName,
    ) -> Result<Membership, ZanzibarError> {
        let mut fanout = 0_u32;
        let resource = DomainObjectRef::try_from(object)?;
        for relationship in self.snapshot.relationships().resource_relation(
            &resource,
            tupleset_relation,
            unbounded_query_limit(),
        ) {
            if let Some((intermediate_object, _)) = relationship.subject_userset_relation_name()? {
                self.increment_fanout(&mut fanout)?;
                if self
                    .check_relation_name(&intermediate_object, computed_userset_relation, user)?
                    .is_allowed()
                {
                    return Ok(Membership::Allowed);
                }
            }
        }
        Ok(Membership::Denied)
    }

    fn eval_compiled_schema_union(
        &mut self,
        object: &Object,
        relation_name: &RelationName,
        user: &User,
        expressions: &[CompiledUsersetExpression],
    ) -> Result<Membership, ZanzibarError> {
        let mut result = Membership::Denied;
        for expression in expressions {
            result = result.union(self.eval_compiled_schema_expression(
                object,
                relation_name,
                user,
                expression,
            )?);
            if result == Membership::Allowed {
                return Ok(result);
            }
        }
        Ok(result)
    }

    fn eval_compiled_schema_intersection(
        &mut self,
        object: &Object,
        relation_name: &RelationName,
        user: &User,
        expressions: &[CompiledUsersetExpression],
    ) -> Result<Membership, ZanzibarError> {
        let mut result = Membership::Allowed;
        for expression in expressions {
            result = result.intersection(self.eval_compiled_schema_expression(
                object,
                relation_name,
                user,
                expression,
            )?);
            if result == Membership::Denied {
                return Ok(result);
            }
        }
        Ok(result)
    }

    fn eval_compiled_schema_exclusion(
        &mut self,
        object: &Object,
        relation_name: &RelationName,
        user: &User,
        base: &CompiledUsersetExpression,
        exclude: &CompiledUsersetExpression,
    ) -> Result<Membership, ZanzibarError> {
        if Self::should_eval_exclude_first(exclude) {
            let exclude_result =
                self.eval_compiled_schema_expression(object, relation_name, user, exclude)?;
            if exclude_result == Membership::Allowed {
                return Ok(Membership::Denied);
            }
            let base_result =
                self.eval_compiled_schema_expression(object, relation_name, user, base)?;
            return Ok(base_result.exclusion(exclude_result));
        }

        let base = self.eval_compiled_schema_expression(object, relation_name, user, base)?;
        if base == Membership::Denied {
            return Ok(Membership::Denied);
        }
        let exclude = self.eval_compiled_schema_expression(object, relation_name, user, exclude)?;
        Ok(base.exclusion(exclude))
    }

    fn should_eval_exclude_first(exclude: &CompiledUsersetExpression) -> bool {
        match exclude {
            CompiledUsersetExpression::This => true,
            CompiledUsersetExpression::ComputedUserset {
                target_has_rewrite, ..
            } => !target_has_rewrite,
            CompiledUsersetExpression::TupleToUserset { .. }
            | CompiledUsersetExpression::Union(_)
            | CompiledUsersetExpression::Intersection(_)
            | CompiledUsersetExpression::Exclusion { .. } => false,
        }
    }

    fn expand_compiled_schema_expression(
        &mut self,
        object: &Object,
        relation_name: &RelationName,
        expression: &CompiledUsersetExpression,
    ) -> Result<ExpandedUserset, ZanzibarError> {
        match expression {
            CompiledUsersetExpression::This => self.expand_this(object, relation_name),
            CompiledUsersetExpression::ComputedUserset { relation, .. } => {
                self.expand_relation_name(object, relation)
            }
            CompiledUsersetExpression::TupleToUserset {
                tupleset_relation: _,
                tupleset_relation_id,
                computed_userset_relation,
            } => {
                let mut users = Vec::new();
                let mut fanout = 0_u32;
                let resource = DomainObjectRef::try_from(object)?;
                let tupleset_relation = self
                    .snapshot
                    .schema()
                    .resolver()
                    .relation_by_id(*tupleset_relation_id)
                    .ok_or_else(compiled_schema_invariant_error)?
                    .name()
                    .clone();
                for relationship in self.snapshot.relationships().resource_relation(
                    &resource,
                    &tupleset_relation,
                    unbounded_query_limit(),
                ) {
                    if let Some((intermediate_object, _)) =
                        relationship.subject_userset_relation_name()?
                    {
                        self.increment_fanout(&mut fanout)?;
                        users.push(self.expand_relation_name(
                            &intermediate_object,
                            computed_userset_relation,
                        )?);
                    }
                }
                Ok(ExpandedUserset::Union(users))
            }
            CompiledUsersetExpression::Union(expressions) => {
                let mut users = Vec::with_capacity(expressions.len());
                for expression in expressions {
                    users.push(self.expand_compiled_schema_expression(
                        object,
                        relation_name,
                        expression,
                    )?);
                }
                Ok(ExpandedUserset::Union(users))
            }
            CompiledUsersetExpression::Intersection(expressions) => {
                let mut users = Vec::with_capacity(expressions.len());
                for expression in expressions {
                    users.push(self.expand_compiled_schema_expression(
                        object,
                        relation_name,
                        expression,
                    )?);
                }
                Ok(ExpandedUserset::Intersection(users))
            }
            CompiledUsersetExpression::Exclusion { base, exclude } => {
                let base = self.expand_compiled_schema_expression(object, relation_name, base)?;
                let exclude =
                    self.expand_compiled_schema_expression(object, relation_name, exclude)?;
                Ok(ExpandedUserset::Exclusion {
                    base: Box::new(base),
                    exclude: Box::new(exclude),
                })
            }
        }
    }

    fn expand_this(
        &mut self,
        object: &Object,
        relation_name: &RelationName,
    ) -> Result<ExpandedUserset, ZanzibarError> {
        let mut users = Vec::new();
        let mut fanout = 0_u32;
        let resource = DomainObjectRef::try_from(object)?;
        for relationship in self.snapshot.relationships().resource_relation(
            &resource,
            relation_name,
            self.fanout_query_limit(),
        ) {
            self.increment_fanout(&mut fanout)?;
            users.push(relationship.expanded_subject()?);
        }
        Ok(ExpandedUserset::Union(users))
    }

    fn stream_lookup_subjects_relation(
        &mut self,
        collector: &mut LookupSubjectCollector<'_, '_>,
        object: &Object,
        relation_name: &RelationName,
        inherited_verify_candidates: bool,
    ) -> Result<(), ZanzibarError> {
        if collector.result_limit_reached() {
            record_lookup_subjects_result_limit_exit();
            return Ok(());
        }
        let key = ExpandKey::new(object, relation_name);
        if self.is_expand_active(&key) {
            return Ok(());
        }
        self.enter(EvaluationKey::Expand(key.clone()))?;
        self.push_expand_key(key);

        let result = self.stream_lookup_subjects_relation_entered(
            collector,
            object,
            relation_name,
            inherited_verify_candidates,
        );
        self.pop_expand_key();
        self.leave();
        result
    }

    fn stream_lookup_subjects_relation_entered(
        &mut self,
        collector: &mut LookupSubjectCollector<'_, '_>,
        object: &Object,
        relation_name: &RelationName,
        inherited_verify_candidates: bool,
    ) -> Result<(), ZanzibarError> {
        let object_type = ObjectType::try_from(object.namespace.as_str())?;
        let relation_definition = self
            .snapshot
            .schema()
            .resolver()
            .relation(&object_type, relation_name)?;
        let verify_candidates = inherited_verify_candidates
            || relation_definition_requires_lookup_subject_verification(
                self.snapshot,
                relation_definition,
            )?;
        match relation_definition.compiled_userset_rewrite() {
            Some(expression) => self.stream_lookup_subjects_expression(
                collector,
                object,
                relation_name,
                expression,
                verify_candidates,
            ),
            None => self.stream_lookup_subjects_this(
                collector,
                object,
                relation_name,
                verify_candidates,
            ),
        }
    }

    fn stream_lookup_subjects_expression(
        &mut self,
        collector: &mut LookupSubjectCollector<'_, '_>,
        object: &Object,
        relation_name: &RelationName,
        expression: &CompiledUsersetExpression,
        verify_candidates: bool,
    ) -> Result<(), ZanzibarError> {
        if collector.result_limit_reached() {
            record_lookup_subjects_result_limit_exit();
            return Ok(());
        }
        match expression {
            CompiledUsersetExpression::This => self.stream_lookup_subjects_this(
                collector,
                object,
                relation_name,
                verify_candidates,
            ),
            CompiledUsersetExpression::ComputedUserset { relation, .. } => {
                self.stream_lookup_subjects_relation(collector, object, relation, verify_candidates)
            }
            CompiledUsersetExpression::TupleToUserset {
                tupleset_relation: _,
                tupleset_relation_id,
                computed_userset_relation,
            } => {
                let mut fanout = 0_u32;
                let resource = DomainObjectRef::try_from(object)?;
                let tupleset_relation = self
                    .snapshot
                    .schema()
                    .resolver()
                    .relation_by_id(*tupleset_relation_id)
                    .ok_or_else(compiled_schema_invariant_error)?
                    .name()
                    .clone();
                for relationship in self.snapshot.relationships().resource_relation(
                    &resource,
                    &tupleset_relation,
                    unbounded_query_limit(),
                ) {
                    if collector.result_limit_reached() {
                        record_lookup_subjects_result_limit_exit();
                        return Ok(());
                    }
                    if let Some((intermediate_object, _)) =
                        relationship.subject_userset_relation_name()?
                    {
                        self.increment_fanout(&mut fanout)?;
                        self.stream_lookup_subjects_relation(
                            collector,
                            &intermediate_object,
                            computed_userset_relation,
                            true,
                        )?;
                    }
                }
                Ok(())
            }
            CompiledUsersetExpression::Union(expressions)
            | CompiledUsersetExpression::Intersection(expressions) => {
                for expression in expressions {
                    self.stream_lookup_subjects_expression(
                        collector,
                        object,
                        relation_name,
                        expression,
                        verify_candidates,
                    )?;
                    if collector.result_limit_reached() {
                        record_lookup_subjects_result_limit_exit();
                        return Ok(());
                    }
                }
                Ok(())
            }
            CompiledUsersetExpression::Exclusion { base, .. } => {
                self.stream_lookup_subjects_expression(collector, object, relation_name, base, true)
            }
        }
    }

    fn stream_lookup_subjects_this(
        &mut self,
        collector: &mut LookupSubjectCollector<'_, '_>,
        object: &Object,
        relation_name: &RelationName,
        verify_candidates: bool,
    ) -> Result<(), ZanzibarError> {
        let mut fanout = 0_u32;
        let resource = DomainObjectRef::try_from(object)?;
        for relationship in self.snapshot.relationships().resource_relation(
            &resource,
            relation_name,
            self.fanout_query_limit(),
        ) {
            if collector.result_limit_reached() {
                record_lookup_subjects_result_limit_exit();
                return Ok(());
            }
            self.increment_fanout(&mut fanout)?;
            if let Some((nested_object, nested_relation)) =
                relationship.subject_userset_relation_name()?
            {
                if collector.collect_userset_candidate(
                    &nested_object,
                    &nested_relation,
                    verify_candidates,
                )? {
                    self.stream_lookup_subjects_relation(
                        collector,
                        &nested_object,
                        &nested_relation,
                        verify_candidates,
                    )?;
                }
            } else if let Some(user_id) = relationship.direct_user_subject_id() {
                collector.collect_user_candidate(user_id, verify_candidates)?;
            } else {
                let _ = relationship.expanded_subject()?;
            }
        }
        Ok(())
    }

    fn enter(&mut self, key: EvaluationKey) -> Result<(), ZanzibarError> {
        if self.remaining_depth == 0 {
            return Err(EvaluationError::DepthExceeded { key: Box::new(key) }.into());
        }
        self.remaining_depth = self.remaining_depth.saturating_sub(1);
        self.record_check_frame_depth(self.remaining_depth);
        Ok(())
    }

    fn leave(&mut self) {
        self.remaining_depth = self.remaining_depth.saturating_add(1);
    }

    fn increment_fanout(&self, current: &mut u32) -> Result<(), ZanzibarError> {
        *current = current.saturating_add(1);
        if *current > self.limits.max_fanout_per_step.get() {
            return Err(EvaluationError::FanoutExceeded {
                limit: self.limits.max_fanout_per_step,
            }
            .into());
        }
        Ok(())
    }

    fn fanout_query_limit(&self) -> QueryLimit {
        let requested = u64::from(self.limits.max_fanout_per_step.get()) + 1;
        let limit = match usize::try_from(requested).ok().and_then(NonZeroUsize::new) {
            Some(limit) => limit,
            None => NonZeroUsize::MAX,
        };
        QueryLimit::new(limit)
    }

    fn push_check_frame(&mut self, entry_remaining_depth: u32) {
        self.check_frames.push(CheckFrame {
            entry_remaining_depth,
            minimum_remaining_depth: self.remaining_depth,
        });
    }

    fn pop_check_frame_depth_required(&mut self) -> NonZeroU32 {
        let Some(frame) = self.check_frames.pop() else {
            return NonZeroU32::MIN;
        };
        self.record_check_frame_depth(frame.minimum_remaining_depth);
        non_zero_u32(
            frame
                .entry_remaining_depth
                .saturating_sub(frame.minimum_remaining_depth),
        )
    }

    fn record_check_frame_depth(&mut self, remaining_depth: u32) {
        if let Some(frame) = self.check_frames.last_mut() {
            frame.minimum_remaining_depth = frame.minimum_remaining_depth.min(remaining_depth);
        }
    }

    fn is_check_active(&self, key: &CheckKey) -> bool {
        if !self.use_active_indexes || self.check_stack.len() < ACTIVE_INDEX_THRESHOLD {
            return self.check_stack.contains(key);
        }
        self.active_checks
            .get(key)
            .is_some_and(|mark| mark.generation == self.generation && mark.active)
    }

    fn push_check_key(&mut self, key: CheckKey) {
        if self.use_active_indexes && self.check_stack.len() + 1 >= ACTIVE_INDEX_THRESHOLD {
            if self.check_stack.len() + 1 == ACTIVE_INDEX_THRESHOLD {
                self.rebuild_active_checks();
            }
            self.active_checks.insert(
                key.clone(),
                VisitMark {
                    generation: self.generation,
                    active: true,
                },
            );
        }
        self.check_stack.push(key);
    }

    fn pop_check_key(&mut self) {
        if let Some(key) = self.check_stack.pop() {
            self.deactivate_check_key(&key);
        }
    }

    fn deactivate_check_key(&mut self, key: &CheckKey) {
        if let Some(mark) = self.active_checks.get_mut(key)
            && mark.generation == self.generation
        {
            mark.active = false;
        }
    }

    fn rebuild_active_checks(&mut self) {
        for key in &self.check_stack {
            self.active_checks.insert(
                key.clone(),
                VisitMark {
                    generation: self.generation,
                    active: true,
                },
            );
        }
    }

    fn is_expand_active(&self, key: &ExpandKey) -> bool {
        if !self.use_active_indexes || self.expand_stack.len() < ACTIVE_INDEX_THRESHOLD {
            return self.expand_stack.contains(key);
        }
        self.active_expands
            .get(key)
            .is_some_and(|mark| mark.generation == self.generation && mark.active)
    }

    fn push_expand_key(&mut self, key: ExpandKey) {
        if self.use_active_indexes && self.expand_stack.len() + 1 >= ACTIVE_INDEX_THRESHOLD {
            if self.expand_stack.len() + 1 == ACTIVE_INDEX_THRESHOLD {
                self.rebuild_active_expands();
            }
            self.active_expands.insert(
                key.clone(),
                VisitMark {
                    generation: self.generation,
                    active: true,
                },
            );
        }
        self.expand_stack.push(key);
    }

    fn pop_expand_key(&mut self) {
        if let Some(key) = self.expand_stack.pop() {
            self.deactivate_expand_key(&key);
        }
    }

    fn deactivate_expand_key(&mut self, key: &ExpandKey) {
        if let Some(mark) = self.active_expands.get_mut(key)
            && mark.generation == self.generation
        {
            mark.active = false;
        }
    }

    fn rebuild_active_expands(&mut self) {
        for key in &self.expand_stack {
            self.active_expands.insert(
                key.clone(),
                VisitMark {
                    generation: self.generation,
                    active: true,
                },
            );
        }
    }
}

/// Evaluates a snapshot-backed check request.
///
/// # Errors
///
/// Returns [`ZanzibarError`] when validation, store access, or evaluator limits fail.
pub fn check_with_snapshot(
    snapshot: &PublishedSnapshot,
    object: &Object,
    relation: &Relation,
    user: &User,
    limits: EvaluationLimits,
) -> Result<Membership, ZanzibarError> {
    EvaluationContext::new(snapshot, limits).check(object, relation, user)
}

pub(crate) fn check_prepared_with_snapshot(
    snapshot: &PublishedSnapshot,
    object: &Object,
    relation: &Relation,
    user: &User,
    relation_definition: &SchemaRelationDefinition,
    limits: EvaluationLimits,
) -> Result<Membership, ZanzibarError> {
    EvaluationContext::new(snapshot, limits).check_prepared(
        object,
        relation,
        user,
        relation_definition,
    )
}

/// Evaluates a snapshot-backed expand request.
///
/// # Errors
///
/// Returns [`ZanzibarError`] when validation, store access, or evaluator limits fail.
pub fn expand_with_snapshot(
    snapshot: &PublishedSnapshot,
    object: &Object,
    relation: &Relation,
    limits: EvaluationLimits,
) -> Result<ExpandedUserset, ZanzibarError> {
    EvaluationContext::new(snapshot, limits).expand(object, relation)
}

/// Looks up resources of the requested type that pass the shared snapshot-backed check evaluator.
///
/// # Errors
///
/// Returns [`ZanzibarError`] when request validation, store access, or evaluation fails.
pub fn lookup_resources_with_snapshot(
    snapshot: &PublishedSnapshot,
    request: &LookupResourcesRequest,
    limits: EvaluationLimits,
) -> Result<LookupResources, ZanzibarError> {
    let resource_type = ObjectType::try_from(request.resource_type.as_str())?;
    let permission = RelationName::try_from(&request.permission)?;
    snapshot
        .schema()
        .resolver()
        .relation(&resource_type, &permission)?;
    let producer_plan = lookup_producer_plan(snapshot, &resource_type, &permission)?;
    if producer_plan.is_none() {
        record_lookup_resources_planner_fallback();
    }
    let mut producer_runtime = LookupProducerRuntime::new(producer_plan.is_some());
    let resource_subject_type = SubjectType::try_from(resource_type.as_str())?;
    let mut pruned_relation_has_downstream = Vec::new();
    let mut relation_expansions = Vec::new();
    let mut tuple_relation_expansions = Vec::new();

    let mut frontier = VecDeque::from([LookupFrontierEntry::new(
        request.subject.clone(),
        LookupSubjectProof::exact_root(),
    )]);
    let mut visited_subjects = HashSet::from([request.subject.clone()]);
    let mut seen = HashSet::new();
    let mut resources = Vec::new();
    let mut check_context = EvaluationContext::new_with_request_memo(snapshot, limits);

    while let Some(frontier_entry) = frontier.pop_front() {
        record_lookup_resources_frontier_subject();
        enqueue_same_object_relation_expansions(
            snapshot,
            &frontier_entry.subject,
            &mut frontier,
            &mut visited_subjects,
            &mut relation_expansions,
        )?;
        let subject_filter = SubjectFilter::try_from(&frontier_entry.subject)?;
        for relationship in snapshot
            .relationships()
            .reverse_query_compact_relationships(&subject_filter)
        {
            record_lookup_resources_frontier_relationship();
            if process_lookup_resources_relationship(
                snapshot,
                relationship,
                &frontier_entry,
                request,
                limits,
                &resource_type,
                &resource_subject_type,
                producer_plan.as_ref(),
                &mut producer_runtime,
                &mut pruned_relation_has_downstream,
                &mut frontier,
                &mut visited_subjects,
                &mut seen,
                &mut resources,
                &mut check_context,
            )? {
                return Ok(LookupResources { resources });
            }
        }
        if process_tuple_to_userset_ignored_relation_edges(
            snapshot,
            &frontier_entry.subject,
            request,
            limits,
            &resource_type,
            &mut frontier,
            &mut visited_subjects,
            &mut seen,
            &mut resources,
            &mut check_context,
            &mut tuple_relation_expansions,
        )? {
            return Ok(LookupResources { resources });
        }
    }

    Ok(LookupResources { resources })
}

#[allow(
    clippy::too_many_arguments,
    reason = "relationship traversal updates shared lookup_resources scratch without heap boxing"
)]
fn process_lookup_resources_relationship(
    snapshot: &PublishedSnapshot,
    relationship: crate::relationship::RelationshipRef<'_>,
    frontier_entry: &LookupFrontierEntry,
    request: &LookupResourcesRequest,
    limits: EvaluationLimits,
    resource_type: &ObjectType,
    resource_subject_type: &SubjectType,
    producer_plan: Option<&LookupProducerPlan>,
    producer_runtime: &mut LookupProducerRuntime,
    pruned_relation_has_downstream: &mut Vec<(RelationName, bool)>,
    frontier: &mut VecDeque<LookupFrontierEntry>,
    visited_subjects: &mut HashSet<User>,
    seen: &mut HashSet<Object>,
    resources: &mut Vec<Object>,
    check_context: &mut EvaluationContext<'_>,
) -> Result<bool, ZanzibarError> {
    let resource_type_matches = relationship.resource_type_eq(resource_type);
    let should_prune = if resource_type_matches {
        producer_plan.is_some_and(|plan| producer_runtime.should_prune(plan, relationship))
    } else {
        false
    };
    if should_prune {
        record_lookup_resources_schema_pruned();
        if !pruned_relation_may_have_downstream(
            snapshot,
            resource_subject_type,
            relationship.relation_name_str(),
            pruned_relation_has_downstream,
        )? {
            return Ok(false);
        }
    }

    let object = relationship.resource_object_legacy();
    let relationship_proof =
        lookup_relationship_proof(snapshot, relationship, frontier_entry.proof, &object)?;
    if resource_type_matches && !should_prune && seen.insert(object.clone()) {
        record_lookup_resources_candidate_resource();
        let verification = producer_plan.map_or(LookupCandidateVerification::FullRoot, |plan| {
            plan.verification_for_relationship(relationship, relationship_proof, limits)
        });
        if verify_lookup_resource_candidate(check_context, &object, request, &verification)? {
            resources.push(object.clone());
            record_lookup_resources_returned();
            if lookup_result_limit_reached(resources.len(), limits) {
                record_lookup_resources_result_limit_exit();
                return Ok(true);
            }
        }
    }

    let userset_subject = User::Userset(
        object,
        Relation(relationship.relation_name_str().to_string()),
    );
    let should_follow_userset = if should_prune {
        let userset_filter = SubjectFilter::try_from(&userset_subject)?;
        snapshot
            .relationships()
            .has_reverse_subject_candidates(&userset_filter)
    } else {
        true
    };
    if should_follow_userset && visited_subjects.insert(userset_subject.clone()) {
        frontier.push_back(LookupFrontierEntry::new(
            userset_subject,
            relationship_proof,
        ));
    }
    Ok(false)
}

fn lookup_relationship_proof(
    snapshot: &PublishedSnapshot,
    relationship: crate::relationship::RelationshipRef<'_>,
    frontier_proof: LookupSubjectProof,
    object: &Object,
) -> Result<LookupSubjectProof, ZanzibarError> {
    if !frontier_proof.is_exact() || frontier_proof.direct_depth > 0 {
        return Ok(LookupSubjectProof::inexact());
    }
    let object_type = ObjectType::try_from(object.namespace.as_str())?;
    let relation = RelationName::try_from(relationship.relation_name_str())?;
    if relation_direct_row_is_exact_proof(snapshot, &object_type, &relation)? {
        Ok(frontier_proof.next_exact())
    } else {
        Ok(LookupSubjectProof::inexact())
    }
}

fn relation_direct_row_is_exact_proof(
    snapshot: &PublishedSnapshot,
    object_type: &ObjectType,
    relation: &RelationName,
) -> Result<bool, ZanzibarError> {
    let relation_definition = snapshot
        .schema()
        .resolver()
        .relation(object_type, relation)?;
    Ok(matches!(
        relation_definition.compiled_userset_rewrite(),
        None | Some(CompiledUsersetExpression::This)
    ))
}

fn verify_lookup_resource_candidate(
    check_context: &mut EvaluationContext<'_>,
    object: &Object,
    request: &LookupResourcesRequest,
    verification: &LookupCandidateVerification,
) -> Result<bool, ZanzibarError> {
    match verification {
        LookupCandidateVerification::FullRoot => {
            check_context.reset_for_reuse();
            record_lookup_resources_full_root_check();
            Ok(check_context
                .check(object, &request.permission, &request.subject)?
                .is_allowed())
        }
        LookupCandidateVerification::ProvenWithoutCheck => {
            record_lookup_resources_proven_without_check();
            Ok(true)
        }
        LookupCandidateVerification::Residual(residuals) => {
            check_context.reset_for_reuse();
            let root_relation = RelationName::try_from(&request.permission)?;
            Ok(check_context
                .verify_lookup_residuals(object, &root_relation, &request.subject, residuals)?
                .is_allowed())
        }
    }
}

fn enqueue_same_object_relation_expansions(
    snapshot: &PublishedSnapshot,
    subject: &User,
    frontier: &mut VecDeque<LookupFrontierEntry>,
    visited_subjects: &mut HashSet<User>,
    relation_expansions: &mut Vec<SameObjectRelationExpansion>,
) -> Result<(), ZanzibarError> {
    let User::Userset(object, relation) = subject else {
        return Ok(());
    };
    let object_type = ObjectType::try_from(object.namespace.as_str())?;
    let source_relation = RelationName::try_from(relation.0.as_str())?;
    let target_relations = same_object_relation_expansions(
        snapshot,
        &object_type,
        &source_relation,
        relation_expansions,
    )?;
    for target_relation in target_relations {
        let expanded_subject = User::Userset(
            object.clone(),
            Relation(target_relation.as_str().to_string()),
        );
        let subject_filter = SubjectFilter::try_from(&expanded_subject)?;
        if snapshot
            .relationships()
            .has_reverse_subject_candidates(&subject_filter)
            && visited_subjects.insert(expanded_subject.clone())
        {
            frontier.push_back(LookupFrontierEntry::new(
                expanded_subject,
                LookupSubjectProof::inexact(),
            ));
        }
    }
    Ok(())
}

fn same_object_relation_expansions<'a>(
    snapshot: &PublishedSnapshot,
    object_type: &ObjectType,
    source_relation: &RelationName,
    relation_expansions: &'a mut Vec<SameObjectRelationExpansion>,
) -> Result<&'a [RelationName], ZanzibarError> {
    if let Some(index) = relation_expansions.iter().position(|expansion| {
        &expansion.object_type == object_type && &expansion.source_relation == source_relation
    }) {
        return Ok(&relation_expansions[index].target_relations);
    }

    let target_relations =
        compute_same_object_relation_expansions(snapshot, object_type, source_relation)?;
    relation_expansions.push(SameObjectRelationExpansion {
        object_type: object_type.clone(),
        source_relation: source_relation.clone(),
        target_relations,
    });
    let index = relation_expansions.len().saturating_sub(1);
    Ok(&relation_expansions[index].target_relations)
}

fn compute_same_object_relation_expansions(
    snapshot: &PublishedSnapshot,
    object_type: &ObjectType,
    source_relation: &RelationName,
) -> Result<Vec<RelationName>, ZanzibarError> {
    let resolver = snapshot.schema().resolver();
    let mut expansions = Vec::new();
    for relation_definition in resolver.sorted_relations(object_type)? {
        if relation_definition.name() == source_relation {
            continue;
        }
        let relation_id = resolver.relation_id(object_type, relation_definition.name())?;
        let mut visiting = HashSet::new();
        if relation_id_can_include_same_object_relation(
            snapshot,
            relation_id,
            source_relation,
            &mut visiting,
        )? {
            expansions.push(relation_definition.name().clone());
        }
    }
    Ok(expansions)
}

fn relation_id_can_include_same_object_relation(
    snapshot: &PublishedSnapshot,
    relation_id: SchemaRelationId,
    source_relation: &RelationName,
    visiting: &mut HashSet<SchemaRelationId>,
) -> Result<bool, ZanzibarError> {
    if !visiting.insert(relation_id) {
        return Ok(false);
    }
    let relation_definition = snapshot
        .schema()
        .resolver()
        .relation_by_id(relation_id)
        .ok_or_else(compiled_schema_invariant_error)?;
    let includes = relation_definition_can_include_same_object_relation(
        snapshot,
        relation_definition,
        source_relation,
        visiting,
    )?;
    visiting.remove(&relation_id);
    Ok(includes)
}

fn relation_definition_can_include_same_object_relation(
    snapshot: &PublishedSnapshot,
    relation_definition: &SchemaRelationDefinition,
    source_relation: &RelationName,
    visiting: &mut HashSet<SchemaRelationId>,
) -> Result<bool, ZanzibarError> {
    let Some(expression) = relation_definition.compiled_userset_rewrite() else {
        return Ok(false);
    };
    expression_can_include_same_object_relation(
        snapshot,
        relation_definition.name(),
        expression,
        source_relation,
        visiting,
    )
}

fn expression_can_include_same_object_relation(
    snapshot: &PublishedSnapshot,
    current_relation: &RelationName,
    expression: &CompiledUsersetExpression,
    source_relation: &RelationName,
    visiting: &mut HashSet<SchemaRelationId>,
) -> Result<bool, ZanzibarError> {
    match expression {
        CompiledUsersetExpression::This => Ok(current_relation == source_relation),
        CompiledUsersetExpression::ComputedUserset {
            relation,
            relation_id,
            target_has_rewrite,
        } => {
            if relation == source_relation {
                return Ok(true);
            }
            if !target_has_rewrite {
                return Ok(false);
            }
            relation_id_can_include_same_object_relation(
                snapshot,
                *relation_id,
                source_relation,
                visiting,
            )
        }
        CompiledUsersetExpression::Union(expressions)
        | CompiledUsersetExpression::Intersection(expressions) => {
            for expression in expressions {
                if expression_can_include_same_object_relation(
                    snapshot,
                    current_relation,
                    expression,
                    source_relation,
                    visiting,
                )? {
                    return Ok(true);
                }
            }
            Ok(false)
        }
        CompiledUsersetExpression::Exclusion { base, .. } => {
            expression_can_include_same_object_relation(
                snapshot,
                current_relation,
                base,
                source_relation,
                visiting,
            )
        }
        CompiledUsersetExpression::TupleToUserset { .. } => Ok(false),
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "lookup_resources keeps traversal-owned scratch in locals to avoid per-request heap \
              indirection"
)]
fn process_tuple_to_userset_ignored_relation_edges(
    snapshot: &PublishedSnapshot,
    subject: &User,
    request: &LookupResourcesRequest,
    limits: EvaluationLimits,
    resource_type: &ObjectType,
    frontier: &mut VecDeque<LookupFrontierEntry>,
    visited_subjects: &mut HashSet<User>,
    seen: &mut HashSet<Object>,
    resources: &mut Vec<Object>,
    check_context: &mut EvaluationContext<'_>,
    tuple_relation_expansions: &mut Vec<TupleToUsersetRelationExpansion>,
) -> Result<bool, ZanzibarError> {
    let User::Userset(object, relation) = subject else {
        return Ok(false);
    };
    let object_type = ObjectType::try_from(object.namespace.as_str())?;
    let source_relation = RelationName::try_from(relation.0.as_str())?;
    let targets = tuple_to_userset_relation_expansions(
        snapshot,
        &object_type,
        &source_relation,
        tuple_relation_expansions,
    )?;
    let subject_type = SubjectType::try_from(object.namespace.as_str())?;
    let subject_id = SubjectId::try_from(object.id.as_str())?;
    let broad_targets =
        tuple_to_userset_broad_targets(snapshot, &subject_type, &subject_id, targets);
    if broad_targets.is_empty() {
        return Ok(false);
    }

    let subject_filter = SubjectFilter::exact(subject_type, subject_id, None);
    for relationship in snapshot
        .relationships()
        .reverse_query_compact_relationships(&subject_filter)
    {
        record_lookup_resources_frontier_relationship();
        for target in broad_targets.iter().copied().filter(|target| {
            relationship.resource_type_eq(&target.resource_type)
                && relationship.relation_name_eq(&target.tupleset_relation)
        }) {
            let candidate = relationship.resource_object_legacy();
            if relationship.resource_type_eq(resource_type) && seen.insert(candidate.clone()) {
                record_lookup_resources_candidate_resource();
                record_lookup_resources_tuple_fallback();
                if verify_lookup_resource_candidate(
                    check_context,
                    &candidate,
                    request,
                    &LookupCandidateVerification::FullRoot,
                )? {
                    resources.push(candidate.clone());
                    record_lookup_resources_returned();
                    if lookup_result_limit_reached(resources.len(), limits) {
                        record_lookup_resources_result_limit_exit();
                        return Ok(true);
                    }
                }
            }

            let target_subject = User::Userset(
                candidate,
                Relation(target.target_relation.as_str().to_string()),
            );
            let target_filter = SubjectFilter::try_from(&target_subject)?;
            if snapshot
                .relationships()
                .has_reverse_subject_candidates(&target_filter)
                && visited_subjects.insert(target_subject.clone())
            {
                frontier.push_back(LookupFrontierEntry::new(
                    target_subject,
                    LookupSubjectProof::inexact(),
                ));
            }
        }
    }
    Ok(false)
}

fn tuple_to_userset_broad_targets<'a>(
    snapshot: &PublishedSnapshot,
    subject_type: &SubjectType,
    subject_id: &SubjectId,
    targets: &'a [TupleToUsersetExpansionTarget],
) -> Vec<&'a TupleToUsersetExpansionTarget> {
    let mut broad_targets = Vec::new();
    for target in targets {
        let exact_filter = SubjectFilter::exact(
            subject_type.clone(),
            subject_id.clone(),
            Some(target.computed_relation.clone()),
        );
        if !snapshot
            .relationships()
            .has_reverse_subject_candidates(&exact_filter)
        {
            broad_targets.push(target);
        }
    }
    broad_targets
}

fn tuple_to_userset_relation_expansions<'a>(
    snapshot: &PublishedSnapshot,
    object_type: &ObjectType,
    source_relation: &RelationName,
    tuple_relation_expansions: &'a mut Vec<TupleToUsersetRelationExpansion>,
) -> Result<&'a [TupleToUsersetExpansionTarget], ZanzibarError> {
    if let Some(index) = tuple_relation_expansions.iter().position(|expansion| {
        &expansion.object_type == object_type && &expansion.source_relation == source_relation
    }) {
        return Ok(&tuple_relation_expansions[index].targets);
    }

    let targets =
        compute_tuple_to_userset_relation_expansions(snapshot, object_type, source_relation)?;
    tuple_relation_expansions.push(TupleToUsersetRelationExpansion {
        object_type: object_type.clone(),
        source_relation: source_relation.clone(),
        targets,
    });
    let index = tuple_relation_expansions.len().saturating_sub(1);
    Ok(&tuple_relation_expansions[index].targets)
}

fn compute_tuple_to_userset_relation_expansions(
    snapshot: &PublishedSnapshot,
    object_type: &ObjectType,
    source_relation: &RelationName,
) -> Result<Vec<TupleToUsersetExpansionTarget>, ZanzibarError> {
    let mut targets = Vec::new();
    for namespace in snapshot.schema().definitions() {
        for relation_definition in namespace.relations() {
            let mut visiting = HashSet::new();
            collect_tuple_to_userset_relation_expansions(
                snapshot,
                namespace.name(),
                relation_definition.name(),
                relation_definition,
                object_type,
                source_relation,
                &mut visiting,
                &mut targets,
            )?;
        }
    }
    Ok(targets)
}

#[allow(
    clippy::too_many_arguments,
    reason = "schema tuple expansion needs both owner and intermediate relation context"
)]
fn collect_tuple_to_userset_relation_expansions(
    snapshot: &PublishedSnapshot,
    owner_object_type: &ObjectType,
    owner_relation: &RelationName,
    relation_definition: &SchemaRelationDefinition,
    intermediate_type: &ObjectType,
    source_relation: &RelationName,
    visiting: &mut HashSet<SchemaRelationId>,
    targets: &mut Vec<TupleToUsersetExpansionTarget>,
) -> Result<(), ZanzibarError> {
    let Some(expression) = relation_definition.compiled_userset_rewrite() else {
        return Ok(());
    };
    collect_tuple_to_userset_expression_expansions(
        snapshot,
        owner_object_type,
        owner_relation,
        expression,
        intermediate_type,
        source_relation,
        visiting,
        targets,
    )
}

#[allow(
    clippy::too_many_arguments,
    reason = "schema tuple expansion needs both owner and intermediate expression context"
)]
fn collect_tuple_to_userset_expression_expansions(
    snapshot: &PublishedSnapshot,
    owner_object_type: &ObjectType,
    owner_relation: &RelationName,
    expression: &CompiledUsersetExpression,
    intermediate_type: &ObjectType,
    source_relation: &RelationName,
    visiting: &mut HashSet<SchemaRelationId>,
    targets: &mut Vec<TupleToUsersetExpansionTarget>,
) -> Result<(), ZanzibarError> {
    match expression {
        CompiledUsersetExpression::This => Ok(()),
        CompiledUsersetExpression::ComputedUserset {
            relation_id,
            target_has_rewrite,
            ..
        } => {
            if !target_has_rewrite || !visiting.insert(*relation_id) {
                return Ok(());
            }
            let relation_definition = snapshot
                .schema()
                .resolver()
                .relation_by_id(*relation_id)
                .ok_or_else(compiled_schema_invariant_error)?;
            collect_tuple_to_userset_relation_expansions(
                snapshot,
                owner_object_type,
                owner_relation,
                relation_definition,
                intermediate_type,
                source_relation,
                visiting,
                targets,
            )?;
            visiting.remove(relation_id);
            Ok(())
        }
        CompiledUsersetExpression::TupleToUserset {
            tupleset_relation,
            computed_userset_relation,
            ..
        } => {
            if relation_name_can_include_same_object_relation(
                snapshot,
                intermediate_type,
                computed_userset_relation,
                source_relation,
            )? {
                targets.push(TupleToUsersetExpansionTarget {
                    resource_type: owner_object_type.clone(),
                    tupleset_relation: tupleset_relation.clone(),
                    computed_relation: computed_userset_relation.clone(),
                    target_relation: owner_relation.clone(),
                });
            }
            Ok(())
        }
        CompiledUsersetExpression::Union(expressions)
        | CompiledUsersetExpression::Intersection(expressions) => {
            for expression in expressions {
                collect_tuple_to_userset_expression_expansions(
                    snapshot,
                    owner_object_type,
                    owner_relation,
                    expression,
                    intermediate_type,
                    source_relation,
                    visiting,
                    targets,
                )?;
            }
            Ok(())
        }
        CompiledUsersetExpression::Exclusion { base, .. } => {
            collect_tuple_to_userset_expression_expansions(
                snapshot,
                owner_object_type,
                owner_relation,
                base,
                intermediate_type,
                source_relation,
                visiting,
                targets,
            )
        }
    }
}

fn relation_name_can_include_same_object_relation(
    snapshot: &PublishedSnapshot,
    object_type: &ObjectType,
    relation: &RelationName,
    source_relation: &RelationName,
) -> Result<bool, ZanzibarError> {
    if relation == source_relation {
        return Ok(true);
    }
    let Ok(relation_id) = snapshot
        .schema()
        .resolver()
        .relation_id(object_type, relation)
    else {
        return Ok(false);
    };
    let mut visiting = HashSet::new();
    relation_id_can_include_same_object_relation(
        snapshot,
        relation_id,
        source_relation,
        &mut visiting,
    )
}

fn pruned_relation_may_have_downstream(
    snapshot: &PublishedSnapshot,
    resource_subject_type: &SubjectType,
    relation_name: &str,
    pruned_relation_has_downstream: &mut Vec<(RelationName, bool)>,
) -> Result<bool, ZanzibarError> {
    if let Some((_, has_downstream)) = pruned_relation_has_downstream
        .iter()
        .find(|(known_relation, _)| known_relation.as_str() == relation_name)
    {
        return Ok(*has_downstream);
    }
    let relation_name = RelationName::try_from(relation_name)?;
    let relation_filter = SubjectFilter::new(
        resource_subject_type.clone(),
        None,
        Some(relation_name.clone()),
    );
    let has_downstream = snapshot
        .relationships()
        .has_reverse_subject_candidates(&relation_filter);
    pruned_relation_has_downstream.push((relation_name, has_downstream));
    Ok(has_downstream)
}

/// Looks up subjects of the requested type that pass the shared snapshot-backed check evaluator.
///
/// # Errors
///
/// Returns [`ZanzibarError`] when request validation, store access, or evaluation fails.
pub fn lookup_subjects_with_snapshot(
    snapshot: &PublishedSnapshot,
    request: &LookupSubjectsRequest,
    limits: EvaluationLimits,
) -> Result<LookupSubjects, ZanzibarError> {
    let resource = DomainObjectRef::try_from(&request.resource)?;
    let permission = RelationName::try_from(&request.permission)?;
    snapshot
        .schema()
        .resolver()
        .relation(resource.object_type(), &permission)?;

    let subject_type = SubjectType::try_from(request.subject_type.as_str())?;
    if subject_type.as_str() != "user" {
        let object_type = ObjectType::try_from(subject_type.as_str())?;
        snapshot.schema().resolver().namespace(&object_type)?;
    }

    let mut seen = HashSet::new();
    let mut seen_usersets = HashSet::new();
    let mut subjects = Vec::new();
    let mut expand_context = EvaluationContext::new(snapshot, limits);
    let mut check_context = EvaluationContext::new(snapshot, limits);
    let mut collector = LookupSubjectCollector {
        resource: &request.resource,
        permission: &request.permission,
        subject_type: &subject_type,
        limits,
        seen_subjects: &mut seen,
        seen_usersets: &mut seen_usersets,
        subjects: &mut subjects,
        check_context: &mut check_context,
    };
    expand_context.stream_lookup_subjects_relation(
        &mut collector,
        &request.resource,
        &permission,
        false,
    )?;

    Ok(LookupSubjects { subjects })
}

fn compiled_schema_invariant_error() -> ZanzibarError {
    ZanzibarError::StorageError("compiled schema relation id is out of bounds".to_string())
}

fn lookup_producer_plan(
    snapshot: &PublishedSnapshot,
    resource_type: &ObjectType,
    permission: &RelationName,
) -> Result<Option<LookupProducerPlan>, ZanzibarError> {
    let resolver = snapshot.schema().resolver();
    let relation_id = resolver.relation_id(resource_type, permission)?;
    let relation_definition = resolver
        .relation_by_id(relation_id)
        .ok_or_else(compiled_schema_invariant_error)?;
    let mut visiting = HashSet::new();
    let Some(verification_rules) = collect_relation_verification_rules(
        snapshot,
        relation_id,
        relation_definition,
        &mut visiting,
    )?
    else {
        return Ok(None);
    };
    if verification_rules.is_empty() {
        Ok(None)
    } else {
        let mut relations = Vec::new();
        for rule in &verification_rules {
            if !relations.contains(&rule.relation) {
                relations.push(rule.relation.clone());
            }
        }
        Ok(Some(LookupProducerPlan {
            relations,
            verification_rules,
        }))
    }
}

fn collect_relation_verification_rules(
    snapshot: &PublishedSnapshot,
    relation_id: SchemaRelationId,
    relation_definition: &SchemaRelationDefinition,
    visiting: &mut HashSet<SchemaRelationId>,
) -> Result<Option<Vec<LookupVerificationRule>>, ZanzibarError> {
    if !visiting.insert(relation_id) {
        return Ok(None);
    }
    let rules = if let Some(expression) = relation_definition.compiled_userset_rewrite() {
        collect_expression_verification_rules(
            snapshot,
            relation_definition.name(),
            expression,
            visiting,
        )?
    } else {
        Some(vec![LookupVerificationRule {
            relation: relation_definition.name().clone(),
            verification: LookupCandidateVerification::ProvenWithoutCheck,
        }])
    };
    visiting.remove(&relation_id);
    Ok(rules.map(merge_lookup_verification_rules))
}

fn collect_expression_verification_rules(
    snapshot: &PublishedSnapshot,
    current_relation: &RelationName,
    expression: &CompiledUsersetExpression,
    visiting: &mut HashSet<SchemaRelationId>,
) -> Result<Option<Vec<LookupVerificationRule>>, ZanzibarError> {
    match expression {
        CompiledUsersetExpression::This => Ok(Some(vec![LookupVerificationRule {
            relation: current_relation.clone(),
            verification: LookupCandidateVerification::ProvenWithoutCheck,
        }])),
        CompiledUsersetExpression::ComputedUserset {
            relation,
            relation_id,
            target_has_rewrite,
        } => {
            if !target_has_rewrite {
                return Ok(Some(vec![LookupVerificationRule {
                    relation: relation.clone(),
                    verification: LookupCandidateVerification::ProvenWithoutCheck,
                }]));
            }
            let relation_definition = snapshot
                .schema()
                .resolver()
                .relation_by_id(*relation_id)
                .ok_or_else(compiled_schema_invariant_error)?;
            collect_relation_verification_rules(
                snapshot,
                *relation_id,
                relation_definition,
                visiting,
            )
        }
        CompiledUsersetExpression::Union(expressions) => {
            let mut rules = Vec::new();
            for expression in expressions {
                let Some(child_rules) = collect_expression_verification_rules(
                    snapshot,
                    current_relation,
                    expression,
                    visiting,
                )?
                else {
                    return Ok(None);
                };
                rules.extend(child_rules);
            }
            Ok(Some(rules))
        }
        CompiledUsersetExpression::Exclusion { base, exclude } => {
            let Some(base_rules) =
                collect_expression_verification_rules(snapshot, current_relation, base, visiting)?
            else {
                return Ok(None);
            };
            Ok(Some(
                base_rules
                    .into_iter()
                    .map(|rule| LookupVerificationRule {
                        relation: rule.relation,
                        verification: rule.verification.with_required_denied(
                            current_relation.clone(),
                            exclude.as_ref().clone(),
                        ),
                    })
                    .collect(),
            ))
        }
        CompiledUsersetExpression::TupleToUserset {
            tupleset_relation, ..
        } => Ok(Some(vec![LookupVerificationRule {
            relation: tupleset_relation.clone(),
            verification: LookupCandidateVerification::FullRoot,
        }])),
        CompiledUsersetExpression::Intersection(expressions) => {
            collect_intersection_verification_rules(
                snapshot,
                current_relation,
                expressions,
                visiting,
            )
        }
    }
}

fn collect_intersection_verification_rules(
    snapshot: &PublishedSnapshot,
    current_relation: &RelationName,
    expressions: &[CompiledUsersetExpression],
    visiting: &mut HashSet<SchemaRelationId>,
) -> Result<Option<Vec<LookupVerificationRule>>, ZanzibarError> {
    for (seed_index, seed_expression) in expressions.iter().enumerate() {
        let Some(seed_rules) = collect_expression_verification_rules(
            snapshot,
            current_relation,
            seed_expression,
            visiting,
        )?
        else {
            continue;
        };
        if seed_rules
            .iter()
            .any(|rule| matches!(rule.verification, LookupCandidateVerification::FullRoot))
        {
            continue;
        }
        let mut guarded_rules = seed_rules;
        for (guard_index, guard_expression) in expressions.iter().enumerate() {
            if guard_index == seed_index {
                continue;
            }
            for rule in &mut guarded_rules {
                rule.verification = rule
                    .verification
                    .clone()
                    .with_required_allowed(current_relation.clone(), guard_expression.clone());
            }
        }
        return Ok(Some(guarded_rules));
    }
    Ok(None)
}

fn merge_lookup_verification_rules(
    rules: Vec<LookupVerificationRule>,
) -> Vec<LookupVerificationRule> {
    let mut merged: Vec<LookupVerificationRule> = Vec::new();
    for rule in rules {
        if let Some(existing) = merged
            .iter_mut()
            .find(|existing| existing.relation == rule.relation)
        {
            existing.verification = existing.verification.clone().union_merge(rule.verification);
        } else {
            merged.push(rule);
        }
    }
    merged
}

fn relation_definition_requires_lookup_subject_verification(
    snapshot: &PublishedSnapshot,
    relation_definition: &SchemaRelationDefinition,
) -> Result<bool, ZanzibarError> {
    let Some(expression) = relation_definition.compiled_userset_rewrite() else {
        return Ok(false);
    };
    let mut visiting = HashSet::new();
    expression_requires_lookup_subject_verification(snapshot, expression, &mut visiting)
}

fn expression_requires_lookup_subject_verification(
    snapshot: &PublishedSnapshot,
    expression: &CompiledUsersetExpression,
    visiting: &mut HashSet<SchemaRelationId>,
) -> Result<bool, ZanzibarError> {
    match expression {
        CompiledUsersetExpression::This => Ok(false),
        CompiledUsersetExpression::ComputedUserset {
            relation_id,
            target_has_rewrite,
            ..
        } => {
            if !target_has_rewrite {
                return Ok(false);
            }
            if !visiting.insert(*relation_id) {
                return Ok(true);
            }
            let relation_definition = snapshot
                .schema()
                .resolver()
                .relation_by_id(*relation_id)
                .ok_or_else(compiled_schema_invariant_error)?;
            let requires = match relation_definition.compiled_userset_rewrite() {
                Some(expression) => {
                    expression_requires_lookup_subject_verification(snapshot, expression, visiting)?
                }
                None => false,
            };
            visiting.remove(relation_id);
            Ok(requires)
        }
        CompiledUsersetExpression::Union(expressions) => {
            for expression in expressions {
                if expression_requires_lookup_subject_verification(snapshot, expression, visiting)?
                {
                    return Ok(true);
                }
            }
            Ok(false)
        }
        CompiledUsersetExpression::TupleToUserset { .. }
        | CompiledUsersetExpression::Intersection(_)
        | CompiledUsersetExpression::Exclusion { .. } => Ok(true),
    }
}

struct LookupSubjectCollector<'a, 'ctx> {
    resource: &'a Object,
    permission: &'a Relation,
    subject_type: &'a SubjectType,
    limits: EvaluationLimits,
    seen_subjects: &'ctx mut HashSet<User>,
    seen_usersets: &'ctx mut HashSet<(Object, RelationName)>,
    subjects: &'ctx mut Vec<User>,
    check_context: &'ctx mut EvaluationContext<'a>,
}

impl LookupSubjectCollector<'_, '_> {
    fn result_limit_reached(&self) -> bool {
        lookup_result_limit_reached(self.subjects.len(), self.limits)
    }

    fn collect_user_candidate(
        &mut self,
        id: &str,
        verify_candidate: bool,
    ) -> Result<(), ZanzibarError> {
        if self.subject_type.as_str() != "user" {
            return Ok(());
        }
        let subject = User::UserId(id.to_string());
        record_lookup_subjects_candidate_subject();
        if self.seen_subjects.insert(subject.clone()) {
            if !verify_candidate {
                self.push_verified_subject(subject);
                return Ok(());
            }
            self.check_context.reset_for_reuse();
            record_lookup_subjects_full_root_check();
            if self
                .check_context
                .check(self.resource, self.permission, &subject)?
                .is_allowed()
            {
                self.push_verified_subject(subject);
            }
        }
        Ok(())
    }

    fn collect_userset_candidate(
        &mut self,
        object: &Object,
        relation_name: &RelationName,
        verify_candidate: bool,
    ) -> Result<bool, ZanzibarError> {
        record_lookup_subjects_candidate_userset();
        if object.namespace == self.subject_type.as_str() {
            let relation = Relation(relation_name.as_str().to_string());
            let userset = User::Userset(object.clone(), relation);
            if self.seen_subjects.insert(userset.clone()) {
                if !verify_candidate {
                    return Ok(self.push_verified_subject(userset));
                }
                self.check_context.reset_for_reuse();
                record_lookup_subjects_full_root_check();
                if self
                    .check_context
                    .check(self.resource, self.permission, &userset)?
                    .is_allowed()
                    && !self.push_verified_subject(userset)
                {
                    return Ok(false);
                }
            }
        }
        Ok(self
            .seen_usersets
            .insert((object.clone(), relation_name.clone())))
    }

    fn push_verified_subject(&mut self, subject: User) -> bool {
        self.subjects.push(subject);
        record_lookup_subjects_returned();
        if lookup_result_limit_reached(self.subjects.len(), self.limits) {
            record_lookup_subjects_result_limit_exit();
            return false;
        }
        true
    }
}

fn non_zero_u32(value: u32) -> NonZeroU32 {
    match NonZeroU32::new(value) {
        Some(value) => value,
        None => NonZeroU32::MIN,
    }
}

fn memo_capacity(limits: EvaluationLimits) -> usize {
    let scaled = limits.max_lookup_results.get().saturating_mul(4);
    let clamped = scaled.clamp(128, 16_384);
    usize::try_from(clamped).map_or(16_384, |value| value)
}

fn unbounded_query_limit() -> QueryLimit {
    QueryLimit::new(NonZeroUsize::MAX)
}

fn lookup_result_limit_reached(current_len: usize, limits: EvaluationLimits) -> bool {
    let limit = match usize::try_from(limits.max_lookup_results.get()) {
        Ok(limit) => limit,
        Err(_) => usize::MAX,
    };
    current_len >= limit
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_should_skip_request_memo_hit_when_depth_is_insufficient()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut memo = RequestCheckMemo::new(EvaluationLimits::default());
        let key = CheckMemoKey::Public {
            object: Object::new("doc", "one"),
            relation: RelationName::try_from("viewer")?,
            user: User::user_id("alice"),
        };

        memo.insert(key.clone(), Membership::Allowed, non_zero_u32(3));

        assert_eq!(memo.get(&key, 2), MemoLookup::DepthInsufficient);
        assert_eq!(memo.get(&key, 3), MemoLookup::Hit(Membership::Allowed));
        Ok(())
    }

    #[test]
    fn test_should_disable_lookup_producer_runtime_when_pruning_ratio_is_low() {
        let mut runtime = LookupProducerRuntime::new(true);
        runtime.record_sample(true);
        for _ in 1..LOOKUP_PLANNER_SAMPLE_RELATIONSHIPS {
            runtime.record_sample(false);
        }

        assert!(!runtime.enabled);
    }

    #[test]
    fn test_should_keep_lookup_producer_runtime_when_pruning_ratio_is_high() {
        let mut runtime = LookupProducerRuntime::new(true);
        for _ in 0..LOOKUP_PLANNER_SAMPLE_RELATIONSHIPS {
            runtime.record_sample(true);
        }

        assert!(runtime.enabled);
    }
}
