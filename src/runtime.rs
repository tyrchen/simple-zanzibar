//! Runtime snapshot state shared by public engine reads and writer publication.

use std::{collections::VecDeque, sync::Arc};

use arc_swap::ArcSwapOption;

use crate::{
    error::ZanzibarError,
    eval::EvaluationLimits,
    revision::{
        Consistency, ConsistencyError, ConsistencyToken, DatastoreId, PublishedSnapshot, Revision,
    },
};

pub(crate) type SharedEngineState = Arc<ArcSwapOption<EngineState>>;

#[derive(Debug)]
pub(crate) struct EngineState {
    latest_snapshot: Arc<PublishedSnapshot>,
    snapshot_history: VecDeque<Arc<PublishedSnapshot>>,
    datastore_id: DatastoreId,
    last_revision: Revision,
    evaluation_limits: EvaluationLimits,
}

impl EngineState {
    pub(crate) fn new(
        latest_snapshot: Arc<PublishedSnapshot>,
        snapshot_history: VecDeque<Arc<PublishedSnapshot>>,
        datastore_id: DatastoreId,
        last_revision: Revision,
        evaluation_limits: EvaluationLimits,
    ) -> Self {
        Self {
            latest_snapshot,
            snapshot_history,
            datastore_id,
            last_revision,
            evaluation_limits,
        }
    }

    pub(crate) fn latest_snapshot(&self) -> Arc<PublishedSnapshot> {
        Arc::clone(&self.latest_snapshot)
    }

    pub(crate) const fn evaluation_limits(&self) -> EvaluationLimits {
        self.evaluation_limits
    }

    pub(crate) fn snapshot_for_consistency(
        &self,
        consistency: Consistency,
    ) -> Result<Arc<PublishedSnapshot>, ZanzibarError> {
        match consistency {
            Consistency::Latest => Ok(Arc::clone(&self.latest_snapshot)),
            Consistency::Exact(token) => self.snapshot_for_token(&token),
        }
    }

    fn snapshot_for_token(
        &self,
        token: &ConsistencyToken,
    ) -> Result<Arc<PublishedSnapshot>, ZanzibarError> {
        if token.datastore_id() != self.datastore_id {
            return Err(ConsistencyError::WrongDatastore.into());
        }
        if token.revision() > self.last_revision {
            return Err(ConsistencyError::RevisionUnavailable {
                revision: token.revision(),
            }
            .into());
        }
        if let Some(oldest) = self.snapshot_history.front()
            && token.revision() < oldest.revision()
        {
            return Err(ConsistencyError::RevisionExpired {
                revision: token.revision(),
            }
            .into());
        }
        let snapshot = self
            .snapshot_history
            .iter()
            .find(|snapshot| snapshot.revision() == token.revision())
            .cloned()
            .ok_or(ConsistencyError::RevisionUnavailable {
                revision: token.revision(),
            })?;
        if snapshot.schema_hash() != token.schema_hash() {
            return Err(ConsistencyError::SchemaHashMismatch {
                revision: token.revision(),
            }
            .into());
        }
        Ok(snapshot)
    }
}
