//! Core evaluation logic for `check` and `expand` requests.

#[cfg(feature = "bench-internals")]
use std::sync::atomic::{AtomicU64, Ordering};
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
#[cfg(feature = "bench-internals")]
static CHECK_EVALUATIONS: AtomicU64 = AtomicU64::new(0);
#[cfg(feature = "bench-internals")]
static CHECK_MEMO_HIT_OPPORTUNITIES: AtomicU64 = AtomicU64::new(0);
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
    /// Number of check evaluations that reached the non-active path.
    pub check_evaluations: u64,
    /// Number of completed check keys observed again inside the same request context.
    pub check_memo_hit_opportunities: u64,
    /// Number of successful check results that would be cacheable by request-local memoization.
    pub check_completed_results: u64,
    /// Number of active-recursion cycle denials.
    pub check_active_cycle_denials: u64,
    /// Number of subject frontier entries popped by `lookup_resources`.
    pub lookup_resources_frontier_subjects: u64,
    /// Number of reverse relationships scanned by `lookup_resources`.
    pub lookup_resources_frontier_relationships: u64,
    /// Number of resource candidates sent to root verification.
    pub lookup_resources_candidate_resources: u64,
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

/// Resets benchmark-only evaluator counters.
#[cfg(feature = "bench-internals")]
pub fn reset_evaluation_read_counters() {
    CHECK_EVALUATIONS.store(0, Ordering::Relaxed);
    CHECK_MEMO_HIT_OPPORTUNITIES.store(0, Ordering::Relaxed);
    CHECK_COMPLETED_RESULTS.store(0, Ordering::Relaxed);
    CHECK_ACTIVE_CYCLE_DENIALS.store(0, Ordering::Relaxed);
    LOOKUP_RESOURCES_FRONTIER_SUBJECTS.store(0, Ordering::Relaxed);
    LOOKUP_RESOURCES_FRONTIER_RELATIONSHIPS.store(0, Ordering::Relaxed);
    LOOKUP_RESOURCES_CANDIDATE_RESOURCES.store(0, Ordering::Relaxed);
    LOOKUP_RESOURCES_FULL_ROOT_CHECKS.store(0, Ordering::Relaxed);
    LOOKUP_RESOURCES_RETURNED.store(0, Ordering::Relaxed);
    LOOKUP_RESOURCES_RESULT_LIMIT_EXITS.store(0, Ordering::Relaxed);
    LOOKUP_SUBJECTS_CANDIDATE_SUBJECTS.store(0, Ordering::Relaxed);
    LOOKUP_SUBJECTS_CANDIDATE_USERSETS.store(0, Ordering::Relaxed);
    LOOKUP_SUBJECTS_FULL_ROOT_CHECKS.store(0, Ordering::Relaxed);
    LOOKUP_SUBJECTS_RETURNED.store(0, Ordering::Relaxed);
    LOOKUP_SUBJECTS_RESULT_LIMIT_EXITS.store(0, Ordering::Relaxed);
}

/// Returns benchmark-only evaluator counters.
#[cfg(feature = "bench-internals")]
#[must_use]
pub fn evaluation_read_counters() -> EvaluationReadCounters {
    EvaluationReadCounters {
        check_evaluations: CHECK_EVALUATIONS.load(Ordering::Relaxed),
        check_memo_hit_opportunities: CHECK_MEMO_HIT_OPPORTUNITIES.load(Ordering::Relaxed),
        check_completed_results: CHECK_COMPLETED_RESULTS.load(Ordering::Relaxed),
        check_active_cycle_denials: CHECK_ACTIVE_CYCLE_DENIALS.load(Ordering::Relaxed),
        lookup_resources_frontier_subjects: LOOKUP_RESOURCES_FRONTIER_SUBJECTS
            .load(Ordering::Relaxed),
        lookup_resources_frontier_relationships: LOOKUP_RESOURCES_FRONTIER_RELATIONSHIPS
            .load(Ordering::Relaxed),
        lookup_resources_candidate_resources: LOOKUP_RESOURCES_CANDIDATE_RESOURCES
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
    CHECK_ACTIVE_CYCLE_DENIALS.fetch_add(1, Ordering::Relaxed);
}

#[cfg(not(feature = "bench-internals"))]
fn record_active_cycle_denial() {}

#[cfg(feature = "bench-internals")]
fn record_lookup_resources_frontier_subject() {
    LOOKUP_RESOURCES_FRONTIER_SUBJECTS.fetch_add(1, Ordering::Relaxed);
}

#[cfg(not(feature = "bench-internals"))]
fn record_lookup_resources_frontier_subject() {}

#[cfg(feature = "bench-internals")]
fn record_lookup_resources_frontier_relationship() {
    LOOKUP_RESOURCES_FRONTIER_RELATIONSHIPS.fetch_add(1, Ordering::Relaxed);
}

#[cfg(not(feature = "bench-internals"))]
fn record_lookup_resources_frontier_relationship() {}

#[cfg(feature = "bench-internals")]
fn record_lookup_resources_candidate_resource() {
    LOOKUP_RESOURCES_CANDIDATE_RESOURCES.fetch_add(1, Ordering::Relaxed);
}

#[cfg(not(feature = "bench-internals"))]
fn record_lookup_resources_candidate_resource() {}

#[cfg(feature = "bench-internals")]
fn record_lookup_resources_full_root_check() {
    LOOKUP_RESOURCES_FULL_ROOT_CHECKS.fetch_add(1, Ordering::Relaxed);
}

#[cfg(not(feature = "bench-internals"))]
fn record_lookup_resources_full_root_check() {}

#[cfg(feature = "bench-internals")]
fn record_lookup_resources_returned() {
    LOOKUP_RESOURCES_RETURNED.fetch_add(1, Ordering::Relaxed);
}

#[cfg(not(feature = "bench-internals"))]
fn record_lookup_resources_returned() {}

#[cfg(feature = "bench-internals")]
fn record_lookup_resources_result_limit_exit() {
    LOOKUP_RESOURCES_RESULT_LIMIT_EXITS.fetch_add(1, Ordering::Relaxed);
}

#[cfg(not(feature = "bench-internals"))]
fn record_lookup_resources_result_limit_exit() {}

#[cfg(feature = "bench-internals")]
fn record_lookup_subjects_candidate_subject() {
    LOOKUP_SUBJECTS_CANDIDATE_SUBJECTS.fetch_add(1, Ordering::Relaxed);
}

#[cfg(not(feature = "bench-internals"))]
fn record_lookup_subjects_candidate_subject() {}

#[cfg(feature = "bench-internals")]
fn record_lookup_subjects_candidate_userset() {
    LOOKUP_SUBJECTS_CANDIDATE_USERSETS.fetch_add(1, Ordering::Relaxed);
}

#[cfg(not(feature = "bench-internals"))]
fn record_lookup_subjects_candidate_userset() {}

#[cfg(feature = "bench-internals")]
fn record_lookup_subjects_full_root_check() {
    LOOKUP_SUBJECTS_FULL_ROOT_CHECKS.fetch_add(1, Ordering::Relaxed);
}

#[cfg(not(feature = "bench-internals"))]
fn record_lookup_subjects_full_root_check() {}

#[cfg(feature = "bench-internals")]
fn record_lookup_subjects_returned() {
    LOOKUP_SUBJECTS_RETURNED.fetch_add(1, Ordering::Relaxed);
}

#[cfg(not(feature = "bench-internals"))]
fn record_lookup_subjects_returned() {}

#[cfg(feature = "bench-internals")]
fn record_lookup_subjects_result_limit_exit() {
    LOOKUP_SUBJECTS_RESULT_LIMIT_EXITS.fetch_add(1, Ordering::Relaxed);
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
            #[cfg(feature = "bench-internals")]
            completed_check_keys: HashSet::new(),
        }
    }

    fn reset_for_reuse(&mut self) {
        self.remaining_depth = self.limits.max_depth.get();
        self.use_active_indexes = true;
        self.check_stack.clear();
        self.expand_stack.clear();
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
    fn record_check_started(&self, key: &CheckKey) {
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
    ) {
        if result.is_ok() && self.completed_check_keys.insert(key.clone()) {
            CHECK_COMPLETED_RESULTS.fetch_add(1, Ordering::Relaxed);
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
        if self.is_check_active(&key) {
            record_active_cycle_denial();
            return Ok(Membership::Denied);
        }
        #[cfg(feature = "bench-internals")]
        self.record_check_started(&key);
        #[cfg(feature = "bench-internals")]
        let completed_key = key.clone();
        self.enter(EvaluationKey::Check(key.clone()))?;
        self.push_check_key(key);

        let result = self.check_entered(object, relation, user);
        self.pop_check_key();
        self.leave();
        #[cfg(feature = "bench-internals")]
        self.record_check_completed(&completed_key, &result);
        result
    }

    pub(crate) fn check_prepared(
        &mut self,
        object: &Object,
        relation: &Relation,
        user: &User,
        relation_definition: &SchemaRelationDefinition,
    ) -> Result<Membership, ZanzibarError> {
        let key = CheckKey::new(self.snapshot, object, relation, user);
        if self.is_check_active(&key) {
            record_active_cycle_denial();
            return Ok(Membership::Denied);
        }
        #[cfg(feature = "bench-internals")]
        self.record_check_started(&key);
        #[cfg(feature = "bench-internals")]
        let completed_key = key.clone();
        self.enter(EvaluationKey::Check(key.clone()))?;
        self.push_check_key(key);

        let result = match relation_definition.compiled_userset_rewrite() {
            Some(expression) => self.eval_compiled_schema_expression(
                object,
                relation_definition.name(),
                user,
                expression,
            ),
            None => self.eval_this(object, relation_definition.name(), user),
        };
        self.pop_check_key();
        self.leave();
        #[cfg(feature = "bench-internals")]
        self.record_check_completed(&completed_key, &result);
        result
    }

    fn check_relation_name(
        &mut self,
        object: &Object,
        relation_name: &RelationName,
        user: &User,
    ) -> Result<Membership, ZanzibarError> {
        let key = CheckKey::from_relation_name(self.snapshot, object, relation_name, user);
        if self.is_check_active(&key) {
            record_active_cycle_denial();
            return Ok(Membership::Denied);
        }
        #[cfg(feature = "bench-internals")]
        self.record_check_started(&key);
        #[cfg(feature = "bench-internals")]
        let completed_key = key.clone();
        self.enter(EvaluationKey::Check(key.clone()))?;
        self.push_check_key(key);

        let result = self.check_relation_name_entered(object, relation_name, user);
        self.pop_check_key();
        self.leave();
        #[cfg(feature = "bench-internals")]
        self.record_check_completed(&completed_key, &result);
        result
    }

    fn check_relation_id(
        &mut self,
        object: &Object,
        relation_name: &RelationName,
        relation_id: SchemaRelationId,
        user: &User,
    ) -> Result<Membership, ZanzibarError> {
        let key = CheckKey::from_relation_name(self.snapshot, object, relation_name, user);
        if self.is_check_active(&key) {
            record_active_cycle_denial();
            return Ok(Membership::Denied);
        }
        #[cfg(feature = "bench-internals")]
        self.record_check_started(&key);
        #[cfg(feature = "bench-internals")]
        let completed_key = key.clone();
        self.enter(EvaluationKey::Check(key.clone()))?;
        self.push_check_key(key);

        let result = self.check_relation_id_entered(object, relation_name, relation_id, user);
        self.pop_check_key();
        self.leave();
        #[cfg(feature = "bench-internals")]
        self.record_check_completed(&completed_key, &result);
        result
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
        if self.is_check_active(&key) {
            record_active_cycle_denial();
            return Ok(Membership::Denied);
        }
        #[cfg(feature = "bench-internals")]
        self.record_check_started(&key);
        #[cfg(feature = "bench-internals")]
        let completed_key = key.clone();
        self.enter(EvaluationKey::Check(key.clone()))?;
        self.push_check_key(key);

        let result = self.eval_this(object, relation_name, user);
        self.pop_check_key();
        self.leave();
        #[cfg(feature = "bench-internals")]
        self.record_check_completed(&completed_key, &result);
        result
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

    fn enter(&mut self, key: EvaluationKey) -> Result<(), ZanzibarError> {
        if self.remaining_depth == 0 {
            return Err(EvaluationError::DepthExceeded { key: Box::new(key) }.into());
        }
        self.remaining_depth = self.remaining_depth.saturating_sub(1);
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

    let mut frontier = VecDeque::from([request.subject.clone()]);
    let mut visited_subjects = HashSet::from([request.subject.clone()]);
    let mut seen = HashSet::new();
    let mut resources = Vec::new();
    let mut check_context = EvaluationContext::new(snapshot, limits);

    while let Some(subject) = frontier.pop_front() {
        record_lookup_resources_frontier_subject();
        let subject_filter = SubjectFilter::try_from(&subject)?;
        for relationship in snapshot
            .relationships()
            .reverse_query_compact_relationships(&subject_filter)
        {
            record_lookup_resources_frontier_relationship();
            let object = relationship.resource_object_legacy();
            if relationship.resource_type_eq(&resource_type) && seen.insert(object.clone()) {
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

            let userset_subject = User::Userset(object, relationship.relation_legacy());
            if visited_subjects.insert(userset_subject.clone()) {
                frontier.push_back(userset_subject);
            }
        }
    }

    Ok(LookupResources { resources })
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

    let expanded =
        EvaluationContext::new(snapshot, limits).expand(&request.resource, &request.permission)?;
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
        expand_context: &mut expand_context,
        check_context: &mut check_context,
    };
    collector.collect(&expanded)?;

    Ok(LookupSubjects { subjects })
}

fn compiled_schema_invariant_error() -> ZanzibarError {
    ZanzibarError::StorageError("compiled schema relation id is out of bounds".to_string())
}

struct LookupSubjectCollector<'a, 'ctx> {
    resource: &'a Object,
    permission: &'a Relation,
    subject_type: &'a SubjectType,
    limits: EvaluationLimits,
    seen_subjects: &'ctx mut HashSet<User>,
    seen_usersets: &'ctx mut HashSet<(Object, Relation)>,
    subjects: &'ctx mut Vec<User>,
    expand_context: &'ctx mut EvaluationContext<'a>,
    check_context: &'ctx mut EvaluationContext<'a>,
}

impl LookupSubjectCollector<'_, '_> {
    fn collect(&mut self, expanded: &ExpandedUserset) -> Result<(), ZanzibarError> {
        if lookup_result_limit_reached(self.subjects.len(), self.limits) {
            record_lookup_subjects_result_limit_exit();
            return Ok(());
        }
        match expanded {
            ExpandedUserset::User(id) if self.subject_type.as_str() == "user" => {
                let subject = User::UserId(id.clone());
                record_lookup_subjects_candidate_subject();
                if self.seen_subjects.insert(subject.clone()) {
                    self.check_context.reset_for_reuse();
                    record_lookup_subjects_full_root_check();
                    if self
                        .check_context
                        .check(self.resource, self.permission, &subject)?
                        .is_allowed()
                    {
                        self.subjects.push(subject);
                        record_lookup_subjects_returned();
                    }
                }
            }
            ExpandedUserset::User(_) => {}
            ExpandedUserset::Userset(object, relation) => {
                self.collect_userset(object, relation)?;
            }
            ExpandedUserset::Union(children) | ExpandedUserset::Intersection(children) => {
                for child in children {
                    self.collect(child)?;
                    if lookup_result_limit_reached(self.subjects.len(), self.limits) {
                        record_lookup_subjects_result_limit_exit();
                        return Ok(());
                    }
                }
            }
            ExpandedUserset::Exclusion { base, exclude } => {
                self.collect(base)?;
                if lookup_result_limit_reached(self.subjects.len(), self.limits) {
                    record_lookup_subjects_result_limit_exit();
                    return Ok(());
                }
                self.collect(exclude)?;
            }
        }
        Ok(())
    }

    fn collect_userset(
        &mut self,
        object: &Object,
        relation: &Relation,
    ) -> Result<(), ZanzibarError> {
        let userset = User::Userset(object.clone(), relation.clone());
        record_lookup_subjects_candidate_userset();
        if object.namespace == self.subject_type.as_str()
            && self.seen_subjects.insert(userset.clone())
        {
            self.check_context.reset_for_reuse();
            record_lookup_subjects_full_root_check();
            if self
                .check_context
                .check(self.resource, self.permission, &userset)?
                .is_allowed()
            {
                self.subjects.push(userset);
                record_lookup_subjects_returned();
                if lookup_result_limit_reached(self.subjects.len(), self.limits) {
                    record_lookup_subjects_result_limit_exit();
                    return Ok(());
                }
            }
        }
        if self
            .seen_usersets
            .insert((object.clone(), relation.clone()))
        {
            self.expand_context.reset_for_reuse();
            let nested = self.expand_context.expand(object, relation)?;
            self.collect(&nested)?;
        }
        Ok(())
    }
}

fn non_zero_u32(value: u32) -> NonZeroU32 {
    match NonZeroU32::new(value) {
        Some(value) => value,
        None => NonZeroU32::MIN,
    }
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
