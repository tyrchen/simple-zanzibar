//! Core evaluation logic for `check` and `expand` requests.

use std::{
    collections::{HashMap, HashSet, VecDeque},
    hash::BuildHasher,
    num::{NonZeroU32, NonZeroUsize},
};

use thiserror::Error;

use crate::{
    domain::{
        ObjectRef as DomainObjectRef, ObjectType, RelationName, Relationship, SubjectRef,
        SubjectType,
    },
    error::ZanzibarError,
    model::{
        ExpandedUserset, LookupResources, LookupResourcesRequest, LookupSubjects,
        LookupSubjectsRequest, NamespaceConfig, Object, Relation, User, UsersetExpression,
    },
    relationship::{
        IndexedRelationshipStore, QueryLimit, RelationshipFilter, RelationshipReader, SubjectFilter,
    },
    revision::PublishedSnapshot,
    schema::UsersetExpression as SchemaUsersetExpression,
    store::TupleStore,
};

const DEFAULT_MAX_DEPTH: u32 = 50;
const DEFAULT_MAX_FANOUT_PER_STEP: u32 = 1_000;
const DEFAULT_MAX_LOOKUP_RESULTS: u32 = 1_000;

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
pub struct CheckKey {
    object: Object,
    relation: Relation,
    user: User,
}

impl CheckKey {
    fn new(object: &Object, relation: &Relation, user: &User) -> Self {
        Self {
            object: object.clone(),
            relation: relation.clone(),
            user: user.clone(),
        }
    }
}

/// Immutable key for expand recursion tracking.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ExpandKey {
    object: Object,
    relation: Relation,
}

impl ExpandKey {
    fn new(object: &Object, relation: &Relation) -> Self {
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
    remaining_depth: u32,
    visited: HashSet<CheckKey>,
    expanded: HashSet<ExpandKey>,
}

impl<'a> EvaluationContext<'a> {
    /// Creates an evaluation context for a snapshot.
    #[must_use]
    pub fn new(snapshot: &'a PublishedSnapshot, limits: EvaluationLimits) -> Self {
        Self {
            snapshot,
            limits,
            remaining_depth: limits.max_depth.get(),
            visited: HashSet::new(),
            expanded: HashSet::new(),
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
        let key = CheckKey::new(object, relation, user);
        if !self.visited.insert(key.clone()) {
            return Ok(Membership::Denied);
        }
        if let Err(error) = self.enter(EvaluationKey::Check(key.clone())) {
            self.visited.remove(&key);
            return Err(error);
        }

        let result = self.check_entered(object, relation, user);
        self.visited.remove(&key);
        self.leave();
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
        let key = ExpandKey::new(object, relation);
        if !self.expanded.insert(key.clone()) {
            return Ok(ExpandedUserset::Union(Vec::new()));
        }
        if let Err(error) = self.enter(EvaluationKey::Expand(key.clone())) {
            self.expanded.remove(&key);
            return Err(error);
        }

        let result = self.expand_entered(object, relation);
        self.expanded.remove(&key);
        self.leave();
        result
    }

    fn expand_entered(
        &mut self,
        object: &Object,
        relation: &Relation,
    ) -> Result<ExpandedUserset, ZanzibarError> {
        let object_type = ObjectType::try_from(object.namespace.as_str())?;
        let relation_name = RelationName::try_from(relation)?;
        let relation_definition = self
            .snapshot
            .schema()
            .resolver()
            .relation(&object_type, &relation_name)?;
        match relation_definition.userset_rewrite() {
            Some(expression) => self.expand_schema_expression(object, relation, expression),
            None => self.expand_this(object, relation),
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
        let relation_definition = self
            .snapshot
            .schema()
            .resolver()
            .relation(&object_type, &relation_name)?;
        match relation_definition.userset_rewrite() {
            Some(expression) => self.eval_schema_expression(object, relation, user, expression),
            None => self.eval_this(object, relation, user),
        }
    }

    fn eval_schema_expression(
        &mut self,
        object: &Object,
        relation: &Relation,
        user: &User,
        expression: &SchemaUsersetExpression,
    ) -> Result<Membership, ZanzibarError> {
        match expression {
            SchemaUsersetExpression::This => self.eval_this(object, relation, user),
            SchemaUsersetExpression::ComputedUserset { relation } => {
                self.check(object, &legacy_relation(relation), user)
            }
            SchemaUsersetExpression::TupleToUserset {
                tupleset_relation,
                computed_userset_relation,
            } => self.eval_tuple_to_userset(
                object,
                user,
                &legacy_relation(tupleset_relation),
                &legacy_relation(computed_userset_relation),
            ),
            SchemaUsersetExpression::Union(expressions) => {
                self.eval_schema_union(object, relation, user, expressions)
            }
            SchemaUsersetExpression::Intersection(expressions) => {
                self.eval_schema_intersection(object, relation, user, expressions)
            }
            SchemaUsersetExpression::Exclusion { base, exclude } => {
                self.eval_schema_exclusion(object, relation, user, base, exclude)
            }
        }
    }

    fn eval_this(
        &mut self,
        object: &Object,
        relation: &Relation,
        user: &User,
    ) -> Result<Membership, ZanzibarError> {
        let resource = DomainObjectRef::try_from(object)?;
        let relation_name = RelationName::try_from(relation)?;
        let subject = SubjectFilter::try_from(user)?;
        let exact_filter =
            RelationshipFilter::for_exact_subject(&resource, relation_name.clone(), subject);
        if self
            .snapshot
            .relationships()
            .any_resource_match(&exact_filter)?
        {
            return Ok(Membership::Allowed);
        }

        let mut fanout = 0_u32;
        for relationship in indexed_resource_relation(
            self.snapshot.relationships(),
            object,
            relation,
            unbounded_query_limit(),
        )? {
            if let SubjectRef::Userset {
                object: userset_object,
                relation: userset_relation,
            } = relationship.subject()
            {
                self.increment_fanout(&mut fanout)?;
                let nested_object = legacy_object(userset_object);
                let nested_relation = legacy_relation(userset_relation);
                if self
                    .check(&nested_object, &nested_relation, user)?
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
        tupleset_relation: &Relation,
        computed_userset_relation: &Relation,
    ) -> Result<Membership, ZanzibarError> {
        let mut fanout = 0_u32;
        for relationship in indexed_resource_relation(
            self.snapshot.relationships(),
            object,
            tupleset_relation,
            unbounded_query_limit(),
        )? {
            if let SubjectRef::Userset {
                object: intermediate,
                ..
            } = relationship.subject()
            {
                self.increment_fanout(&mut fanout)?;
                let intermediate_object = legacy_object(intermediate);
                if self
                    .check(&intermediate_object, computed_userset_relation, user)?
                    .is_allowed()
                {
                    return Ok(Membership::Allowed);
                }
            }
        }
        Ok(Membership::Denied)
    }

    fn eval_schema_union(
        &mut self,
        object: &Object,
        relation: &Relation,
        user: &User,
        expressions: &[SchemaUsersetExpression],
    ) -> Result<Membership, ZanzibarError> {
        let mut result = Membership::Denied;
        for expression in expressions {
            result = result.union(self.eval_schema_expression(object, relation, user, expression)?);
            if result == Membership::Allowed {
                return Ok(result);
            }
        }
        Ok(result)
    }

    fn eval_schema_intersection(
        &mut self,
        object: &Object,
        relation: &Relation,
        user: &User,
        expressions: &[SchemaUsersetExpression],
    ) -> Result<Membership, ZanzibarError> {
        let mut result = Membership::Allowed;
        for expression in expressions {
            result = result
                .intersection(self.eval_schema_expression(object, relation, user, expression)?);
            if result == Membership::Denied {
                return Ok(result);
            }
        }
        Ok(result)
    }

    fn eval_schema_exclusion(
        &mut self,
        object: &Object,
        relation: &Relation,
        user: &User,
        base: &SchemaUsersetExpression,
        exclude: &SchemaUsersetExpression,
    ) -> Result<Membership, ZanzibarError> {
        let base = self.eval_schema_expression(object, relation, user, base)?;
        if base == Membership::Denied {
            return Ok(Membership::Denied);
        }
        let exclude = self.eval_schema_expression(object, relation, user, exclude)?;
        Ok(base.exclusion(exclude))
    }

    fn expand_schema_expression(
        &mut self,
        object: &Object,
        relation: &Relation,
        expression: &SchemaUsersetExpression,
    ) -> Result<ExpandedUserset, ZanzibarError> {
        match expression {
            SchemaUsersetExpression::This => self.expand_this(object, relation),
            SchemaUsersetExpression::ComputedUserset { relation } => {
                self.expand(object, &legacy_relation(relation))
            }
            SchemaUsersetExpression::TupleToUserset {
                tupleset_relation,
                computed_userset_relation,
            } => {
                let mut users = Vec::new();
                let mut fanout = 0_u32;
                for relationship in indexed_resource_relation(
                    self.snapshot.relationships(),
                    object,
                    &legacy_relation(tupleset_relation),
                    unbounded_query_limit(),
                )? {
                    if let SubjectRef::Userset {
                        object: intermediate,
                        ..
                    } = relationship.subject()
                    {
                        self.increment_fanout(&mut fanout)?;
                        users.push(self.expand(
                            &legacy_object(intermediate),
                            &legacy_relation(computed_userset_relation),
                        )?);
                    }
                }
                Ok(ExpandedUserset::Union(users))
            }
            SchemaUsersetExpression::Union(expressions) => {
                let mut users = Vec::with_capacity(expressions.len());
                for expression in expressions {
                    users.push(self.expand_schema_expression(object, relation, expression)?);
                }
                Ok(ExpandedUserset::Union(users))
            }
            SchemaUsersetExpression::Intersection(expressions) => {
                let mut users = Vec::with_capacity(expressions.len());
                for expression in expressions {
                    users.push(self.expand_schema_expression(object, relation, expression)?);
                }
                Ok(ExpandedUserset::Intersection(users))
            }
            SchemaUsersetExpression::Exclusion { base, exclude } => {
                let base = self.expand_schema_expression(object, relation, base)?;
                let exclude = self.expand_schema_expression(object, relation, exclude)?;
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
        relation: &Relation,
    ) -> Result<ExpandedUserset, ZanzibarError> {
        let mut users = Vec::new();
        let mut fanout = 0_u32;
        for relationship in indexed_resource_relation(
            self.snapshot.relationships(),
            object,
            relation,
            self.fanout_query_limit(),
        )? {
            self.increment_fanout(&mut fanout)?;
            users.push(expanded_subject(relationship.subject())?);
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

    while let Some(subject) = frontier.pop_front() {
        let subject_filter = SubjectFilter::try_from(&subject)?;
        for relationship in snapshot
            .relationships()
            .reverse_query_relationships(&subject_filter)?
        {
            let object = legacy_object(relationship.resource());
            if relationship.resource().object_type() == &resource_type
                && seen.insert(object.clone())
                && EvaluationContext::new(snapshot, limits)
                    .check(&object, &request.permission, &request.subject)?
                    .is_allowed()
            {
                resources.push(object.clone());
                if lookup_result_limit_reached(resources.len(), limits) {
                    return Ok(LookupResources { resources });
                }
            }

            let userset_subject = User::Userset(object, legacy_relation(relationship.relation()));
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
    let mut candidates = Vec::new();
    collect_lookup_subject_candidates(
        snapshot,
        &expanded,
        &subject_type,
        limits,
        &mut seen,
        &mut seen_usersets,
        &mut candidates,
    )?;

    let mut subjects = Vec::new();
    for subject in candidates {
        if EvaluationContext::new(snapshot, limits)
            .check(&request.resource, &request.permission, &subject)?
            .is_allowed()
        {
            subjects.push(subject);
            if lookup_result_limit_reached(subjects.len(), limits) {
                break;
            }
        }
    }

    Ok(LookupSubjects { subjects })
}

fn check_internal<S, C>(
    object: &Object,
    relation: &Relation,
    user: &User,
    configs: &HashMap<String, NamespaceConfig, C>,
    store: &(impl TupleStore + ?Sized),
    visited: &mut HashSet<(Object, Relation, User), S>,
) -> Result<bool, ZanzibarError>
where
    S: BuildHasher,
    C: BuildHasher,
{
    if !visited.insert((object.clone(), relation.clone(), user.clone())) {
        // We've already seen this exact check in this path, so we're in a cycle.
        return Ok(false);
    }

    let config = configs
        .get(&object.namespace)
        .ok_or_else(|| ZanzibarError::NamespaceNotFound(object.namespace.clone()))?;

    let relation_config = config
        .relations
        .get(relation)
        .ok_or_else(|| ZanzibarError::RelationNotFound(relation.0.clone(), config.name.clone()))?;

    // If there's a rewrite rule, evaluate it. Otherwise, default to `This`.
    let rewrite = relation_config
        .userset_rewrite
        .as_ref()
        .unwrap_or(&UsersetExpression::This);

    let result = eval_expression(object, relation, user, configs, store, visited, rewrite);

    // Important: remove the current check from `visited` on the way back up the recursion.
    visited.remove(&(object.clone(), relation.clone(), user.clone()));
    result
}

fn eval_expression<S, C>(
    object: &Object,
    relation: &Relation,
    user: &User,
    configs: &HashMap<String, NamespaceConfig, C>,
    store: &(impl TupleStore + ?Sized),
    visited: &mut HashSet<(Object, Relation, User), S>,
    expression: &UsersetExpression,
) -> Result<bool, ZanzibarError>
where
    S: BuildHasher,
    C: BuildHasher,
{
    match expression {
        UsersetExpression::This => {
            // Direct check: see if the exact tuple exists.
            let direct_tuples = store.read_tuples(object, Some(relation), Some(user));
            if !direct_tuples.is_empty() {
                return Ok(true);
            }

            // Indirect check: see if the user is in a userset that has the relation.
            let indirect_tuples = store.read_tuples(object, Some(relation), None);
            for t in indirect_tuples {
                if let User::Userset(obj, rel) = t.user {
                    // Note: a subtle but important detail. When checking a userset,
                    // we must start a fresh `check` call, not `eval_expression`,
                    // as the new object (`obj`) may have its own rewrite rules.
                    if check_internal(&obj, &rel, user, configs, store, visited)? {
                        return Ok(true);
                    }
                }
            }
            Ok(false)
        }
        UsersetExpression::ComputedUserset {
            relation: computed_relation,
        } => {
            // Similarly, start a fresh `check` to respect the rewrite rules of the new relation.
            check_internal(object, computed_relation, user, configs, store, visited)
        }
        UsersetExpression::TupleToUserset {
            tupleset_relation,
            computed_userset_relation,
        } => {
            let tupleset = store.read_tuples(object, Some(tupleset_relation), None);
            for t in tupleset {
                if let User::Userset(intermediate_obj, _) = t.user
                    && check_internal(
                        &intermediate_obj,
                        computed_userset_relation,
                        user,
                        configs,
                        store,
                        visited,
                    )?
                {
                    return Ok(true);
                }
            }
            Ok(false)
        }
        UsersetExpression::Union(expressions) => {
            for expr in expressions {
                if eval_expression(object, relation, user, configs, store, visited, expr)? {
                    return Ok(true);
                }
            }
            Ok(false)
        }
        UsersetExpression::Intersection(expressions) => {
            for expr in expressions {
                // If any sub-expression is false, the whole intersection is false.
                if !eval_expression(object, relation, user, configs, store, visited, expr)? {
                    return Ok(false);
                }
            }
            // If we get here, all sub-expressions were true.
            Ok(true)
        }
        UsersetExpression::Exclusion { base, exclude } => {
            // If the user is in the `exclude` set, the result is false.
            let is_excluded =
                eval_expression(object, relation, user, configs, store, visited, exclude)?;
            if is_excluded {
                return Ok(false);
            }
            // Otherwise, the result is determined by the `base` set.
            eval_expression(object, relation, user, configs, store, visited, base)
        }
    }
}

pub(crate) fn check_with_configs<S, C>(
    object: &Object,
    relation: &Relation,
    user: &User,
    configs: &HashMap<String, NamespaceConfig, C>,
    store: &(impl TupleStore + ?Sized),
    visited: &mut HashSet<(Object, Relation, User), S>,
) -> Result<bool, ZanzibarError>
where
    S: BuildHasher,
    C: BuildHasher,
{
    check_internal(object, relation, user, configs, store, visited)
}

fn indexed_resource_relation<'a>(
    relationships: &'a IndexedRelationshipStore,
    object: &Object,
    relation: &Relation,
    limit: QueryLimit,
) -> Result<impl Iterator<Item = &'a Relationship>, ZanzibarError> {
    let resource = DomainObjectRef::try_from(object)?;
    let relation_name = RelationName::try_from(relation)?;
    let filter = RelationshipFilter::new(
        ObjectType::try_from(object.namespace.as_str())?,
        Some(resource.object_id().clone()),
        Some(relation_name),
        None,
        limit,
    );
    Ok(relationships.query_relationships(&filter)?)
}

fn legacy_object(object: &DomainObjectRef) -> Object {
    Object {
        namespace: object.object_type().as_str().to_string(),
        id: object.object_id().as_str().to_string(),
    }
}

fn legacy_relation(relation: &RelationName) -> Relation {
    Relation(relation.as_str().to_string())
}

fn expanded_subject(subject: &SubjectRef) -> Result<ExpandedUserset, ZanzibarError> {
    match subject {
        SubjectRef::Object(object) if object.object_type().as_str() == "user" => Ok(
            ExpandedUserset::User(object.object_id().as_str().to_string()),
        ),
        SubjectRef::Object(object) => Err(ZanzibarError::StorageError(format!(
            "legacy expand cannot represent direct subject type '{}'",
            object.object_type()
        ))),
        SubjectRef::Userset { object, relation } => Ok(ExpandedUserset::Userset(
            legacy_object(object),
            legacy_relation(relation),
        )),
    }
}

fn collect_lookup_subject_candidates(
    snapshot: &PublishedSnapshot,
    expanded: &ExpandedUserset,
    subject_type: &SubjectType,
    limits: EvaluationLimits,
    seen_subjects: &mut HashSet<User>,
    seen_usersets: &mut HashSet<(Object, Relation)>,
    candidates: &mut Vec<User>,
) -> Result<(), ZanzibarError> {
    match expanded {
        ExpandedUserset::User(id) if subject_type.as_str() == "user" => {
            let subject = User::UserId(id.clone());
            if seen_subjects.insert(subject.clone()) {
                candidates.push(subject);
            }
        }
        ExpandedUserset::User(_) => {}
        ExpandedUserset::Userset(object, relation) => {
            let userset = User::Userset(object.clone(), relation.clone());
            if object.namespace == subject_type.as_str() && seen_subjects.insert(userset.clone()) {
                candidates.push(userset);
            }
            if seen_usersets.insert((object.clone(), relation.clone())) {
                let nested = EvaluationContext::new(snapshot, limits).expand(object, relation)?;
                collect_lookup_subject_candidates(
                    snapshot,
                    &nested,
                    subject_type,
                    limits,
                    seen_subjects,
                    seen_usersets,
                    candidates,
                )?;
            }
        }
        ExpandedUserset::Union(children) | ExpandedUserset::Intersection(children) => {
            for child in children {
                collect_lookup_subject_candidates(
                    snapshot,
                    child,
                    subject_type,
                    limits,
                    seen_subjects,
                    seen_usersets,
                    candidates,
                )?;
            }
        }
        ExpandedUserset::Exclusion { base, exclude } => {
            collect_lookup_subject_candidates(
                snapshot,
                base,
                subject_type,
                limits,
                seen_subjects,
                seen_usersets,
                candidates,
            )?;
            collect_lookup_subject_candidates(
                snapshot,
                exclude,
                subject_type,
                limits,
                seen_subjects,
                seen_usersets,
                candidates,
            )?;
        }
    }
    Ok(())
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

pub(crate) fn expand_with_configs<C>(
    object: &Object,
    relation: &Relation,
    configs: &HashMap<String, NamespaceConfig, C>,
    store: &(impl TupleStore + ?Sized),
) -> Result<ExpandedUserset, ZanzibarError>
where
    C: BuildHasher,
{
    let config = configs
        .get(&object.namespace)
        .ok_or_else(|| ZanzibarError::NamespaceNotFound(object.namespace.clone()))?;

    let relation_config = config
        .relations
        .get(relation)
        .ok_or_else(|| ZanzibarError::RelationNotFound(relation.0.clone(), config.name.clone()))?;

    let rewrite = relation_config
        .userset_rewrite
        .as_ref()
        .unwrap_or(&UsersetExpression::This);

    expand_expression(object, relation, configs, store, rewrite)
}

fn expand_expression<C>(
    object: &Object,
    relation: &Relation,
    configs: &HashMap<String, NamespaceConfig, C>,
    store: &(impl TupleStore + ?Sized),
    expression: &UsersetExpression,
) -> Result<ExpandedUserset, ZanzibarError>
where
    C: BuildHasher,
{
    match expression {
        UsersetExpression::This => {
            let direct_tuples = store.read_tuples(object, Some(relation), None);
            let mut users = Vec::new();
            for t in direct_tuples {
                match t.user {
                    User::UserId(id) => users.push(ExpandedUserset::User(id)),
                    User::Userset(obj, rel) => users.push(ExpandedUserset::Userset(obj, rel)),
                }
            }
            Ok(ExpandedUserset::Union(users))
        }
        UsersetExpression::ComputedUserset {
            relation: computed_relation,
        } => expand_with_configs(object, computed_relation, configs, store),
        UsersetExpression::TupleToUserset {
            tupleset_relation,
            computed_userset_relation,
        } => {
            let tupleset = store.read_tuples(object, Some(tupleset_relation), None);
            let mut users = Vec::new();
            for t in tupleset {
                if let User::Userset(intermediate_obj, _) = t.user {
                    users.push(expand_with_configs(
                        &intermediate_obj,
                        computed_userset_relation,
                        configs,
                        store,
                    )?);
                }
            }
            Ok(ExpandedUserset::Union(users))
        }
        UsersetExpression::Union(expressions) => {
            let mut sub_expressions = Vec::new();
            for expr in expressions {
                sub_expressions.push(expand_expression(object, relation, configs, store, expr)?);
            }
            Ok(ExpandedUserset::Union(sub_expressions))
        }
        UsersetExpression::Intersection(expressions) => {
            let mut sub_expressions = Vec::new();
            for expr in expressions {
                sub_expressions.push(expand_expression(object, relation, configs, store, expr)?);
            }
            Ok(ExpandedUserset::Intersection(sub_expressions))
        }
        UsersetExpression::Exclusion { base, exclude } => {
            let base_expr = expand_expression(object, relation, configs, store, base)?;
            let exclude_expr = expand_expression(object, relation, configs, store, exclude)?;
            Ok(ExpandedUserset::Exclusion {
                base: Box::new(base_expr),
                exclude: Box::new(exclude_expr),
            })
        }
    }
}
