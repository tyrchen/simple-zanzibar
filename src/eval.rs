//! Core evaluation logic for `check` and `expand` requests.

#[cfg(feature = "bench-internals")]
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    num::{NonZeroU32, NonZeroUsize},
};

use thiserror::Error;

use crate::{
    domain::{ObjectRef as DomainObjectRef, ObjectType, RelationName, SubjectType},
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
static EVALUATION_READ_COUNTERS_ENABLED: AtomicBool = AtomicBool::new(false);
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
    EVALUATION_READ_COUNTERS_ENABLED.store(true, Ordering::Relaxed);
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
    EVALUATION_READ_COUNTERS_ENABLED.store(enabled, Ordering::Relaxed);
}

#[cfg(feature = "bench-internals")]
fn evaluation_read_counters_enabled() -> bool {
    EVALUATION_READ_COUNTERS_ENABLED.load(Ordering::Relaxed)
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
}

impl LookupProducerPlan {
    fn allows_relationship(&self, relationship: crate::relationship::RelationshipRef<'_>) -> bool {
        self.relations
            .iter()
            .any(|relation| relationship.relation_name_eq(relation))
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

    let mut frontier = VecDeque::from([request.subject.clone()]);
    let mut visited_subjects = HashSet::from([request.subject.clone()]);
    let mut seen = HashSet::new();
    let mut resources = Vec::new();
    let mut check_context = EvaluationContext::new_with_request_memo(snapshot, limits);

    while let Some(subject) = frontier.pop_front() {
        record_lookup_resources_frontier_subject();
        let subject_filter = SubjectFilter::try_from(&subject)?;
        for relationship in snapshot
            .relationships()
            .reverse_query_compact_relationships(&subject_filter)
        {
            record_lookup_resources_frontier_relationship();
            let resource_type_matches = relationship.resource_type_eq(&resource_type);
            let should_prune = if resource_type_matches {
                producer_plan
                    .as_ref()
                    .is_some_and(|plan| producer_runtime.should_prune(plan, relationship))
            } else {
                false
            };
            if should_prune {
                record_lookup_resources_schema_pruned();
                if !pruned_relation_may_have_downstream(
                    snapshot,
                    &resource_subject_type,
                    relationship.relation_name_str(),
                    &mut pruned_relation_has_downstream,
                )? {
                    continue;
                }
            }

            let object = relationship.resource_object_legacy();
            if resource_type_matches && !should_prune && seen.insert(object.clone()) {
                record_lookup_resources_candidate_resource();
                check_context.reset_for_reuse();
                record_lookup_resources_full_root_check();
                if check_context
                    .check(&object, &request.permission, &request.subject)?
                    .is_allowed()
                {
                    resources.push(object.clone());
                    record_lookup_resources_returned();
                    if lookup_result_limit_reached(resources.len(), limits) {
                        record_lookup_resources_result_limit_exit();
                        return Ok(LookupResources { resources });
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
                frontier.push_back(userset_subject);
            }
        }
    }

    Ok(LookupResources { resources })
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
    let mut relations = HashSet::new();
    let mut visiting = HashSet::new();
    if collect_relation_producers(
        snapshot,
        relation_id,
        relation_definition,
        &mut visiting,
        &mut relations,
    )? {
        Ok(Some(LookupProducerPlan {
            relations: relations.into_iter().collect(),
        }))
    } else {
        Ok(None)
    }
}

fn collect_relation_producers(
    snapshot: &PublishedSnapshot,
    relation_id: SchemaRelationId,
    relation_definition: &SchemaRelationDefinition,
    visiting: &mut HashSet<SchemaRelationId>,
    producers: &mut HashSet<RelationName>,
) -> Result<bool, ZanzibarError> {
    if !visiting.insert(relation_id) {
        return Ok(false);
    }
    let supported = if let Some(expression) = relation_definition.compiled_userset_rewrite() {
        collect_expression_producers(
            snapshot,
            relation_definition.name(),
            expression,
            visiting,
            producers,
        )?
    } else {
        producers.insert(relation_definition.name().clone());
        true
    };
    visiting.remove(&relation_id);
    Ok(supported)
}

fn collect_expression_producers(
    snapshot: &PublishedSnapshot,
    current_relation: &RelationName,
    expression: &CompiledUsersetExpression,
    visiting: &mut HashSet<SchemaRelationId>,
    producers: &mut HashSet<RelationName>,
) -> Result<bool, ZanzibarError> {
    match expression {
        CompiledUsersetExpression::This => {
            producers.insert(current_relation.clone());
            Ok(true)
        }
        CompiledUsersetExpression::ComputedUserset {
            relation,
            relation_id,
            target_has_rewrite,
        } => {
            if !target_has_rewrite {
                producers.insert(relation.clone());
                return Ok(true);
            }
            let relation_definition = snapshot
                .schema()
                .resolver()
                .relation_by_id(*relation_id)
                .ok_or_else(compiled_schema_invariant_error)?;
            collect_relation_producers(
                snapshot,
                *relation_id,
                relation_definition,
                visiting,
                producers,
            )
        }
        CompiledUsersetExpression::Union(expressions) => {
            for expression in expressions {
                if !collect_expression_producers(
                    snapshot,
                    current_relation,
                    expression,
                    visiting,
                    producers,
                )? {
                    return Ok(false);
                }
            }
            Ok(true)
        }
        CompiledUsersetExpression::Exclusion { base, .. } => {
            collect_expression_producers(snapshot, current_relation, base, visiting, producers)
        }
        CompiledUsersetExpression::TupleToUserset { .. }
        | CompiledUsersetExpression::Intersection(_) => Ok(false),
    }
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
